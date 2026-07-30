use anyhow::{Context, Result};
use clap::Args;
use std::collections::BTreeSet;
use std::path::Path;

use chrono::NaiveDate;
use nodex_core::Config;
use nodex_core::builder::scanner::Proposed;
use nodex_core::command_result::{IdStability, RenameResult};
use nodex_core::error::Error as CoreError;
use nodex_core::parser::editor::{FrontmatterEditor, Scalar};
use nodex_core::parser::frontmatter::{canonicalize, split_frontmatter};
use nodex_core::parser::identity::{infer_id, infer_kind};

use crate::format::emit_write;

/// Args for `nodex rename`.
#[derive(Args)]
pub struct RenameArgs {
    /// Source path (relative to root).
    pub old: String,
    /// Target path (relative to root).
    pub new: String,
}

pub fn run(root: &Path, args: RenameArgs, pretty: bool, today: NaiveDate) -> Result<()> {
    let config = nodex_core::load_project_for_mutation(root)?;

    // The one canonical normalization every user-supplied document path
    // gets (symmetric with `check --content` and `scaffold`): fold `\`
    // to `/`, refuse traversal / absolute forms, collapse `.` segments.
    // Everything downstream — id inference, the destination probe,
    // reference rewriting, the move itself — keys on these strings, so
    // the probe verdict, the moved artifact, and the next scan can
    // never disagree about which document was named.
    let old_norm = nodex_core::path_guard::normalize_doc_path(&args.old)?;
    let new_norm = nodex_core::path_guard::normalize_doc_path(&args.new)?;
    let old_path = old_norm.as_str();
    let new_path = new_norm.as_str();

    let old_abs = root.join(old_path);
    let new_abs = root.join(new_path);

    if !old_abs.exists() {
        return Err(CoreError::Io {
            path: old_abs,
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        }
        .into());
    }
    // rename moves a single document. A directory source would slide a
    // whole tree past every per-document guarantee — the destination
    // gate, id anchoring, and reference rewriting all reason about one
    // file — silently dangling every reference into the tree. Refuse
    // loudly; `symlink_metadata` so a symlinked directory can't dodge
    // the guard while a file symlink (moved as the link itself, the
    // documented behavior) still passes.
    let old_meta = std::fs::symlink_metadata(&old_abs).map_err(|source| CoreError::Io {
        path: old_abs.clone(),
        source,
    })?;
    if old_meta.is_dir() {
        return Err(CoreError::Config(format!(
            "rename moves a single document; {old_path:?} is a directory — move its documents \
             individually so their references can be rewritten"
        ))
        .into());
    }
    if new_abs.exists() {
        return Err(CoreError::Exists(new_abs).into());
    }

    // `reject_traversal` above is the lexical half of the guard; this
    // is the filesystem half, applied to BOTH ends of the move
    // (symmetric guards): a lexically clean path can still escape root
    // through a symlinked ancestor directory. An escaping *source*
    // would pull an out-of-root file into the project (exfiltration
    // via `fs::rename`); an escaping *destination* would push a
    // project file out. Checked before any directory creation or the
    // move itself, so a refused rename leaves no trace — not even an
    // empty directory chain.
    nodex_core::path_guard::reject_outside_root(root, &old_abs)?;
    nodex_core::path_guard::reject_outside_root(root, &new_abs)?;

    // The scope as it really is *before* the move, scanned while the
    // file still lives at `old_path`. It decides whether the source is
    // a graph document at all, and later lets `rewrite_references`
    // resolve each link's pre-rewrite binding so it rewrites exactly
    // the links the build bound to `old_path` — leaving a link bound to
    // a different file (e.g. a bare sibling shadowing the renamed
    // `.md`) untouched. A *real* scan rather than one fabricated from
    // the post-move scan is essential: `scope` is status- and
    // location-dependent (`conditional_exclude`).
    let pre_move_scope: BTreeSet<String> = nodex_core::builder::scanner::scan_scope(root, &config)
        .context("pre-move scope scan failed")?
        .paths
        .iter()
        .map(|p| nodex_core::path_guard::forward_string(p))
        .collect();

    // An untracked source — outside scope or conditionally excluded —
    // has no node and no edges: nothing can dangle, so the rename is a
    // plain guarded move with no destination gate, no id anchoring, and
    // no reference rewriting. The gate below therefore fires exactly
    // when it protects something: moving a *tracked* document to a spot
    // the scan would not admit would silently drop it from the graph
    // and orphan its edges.
    let old_forward = nodex_core::path_guard::forward_str(old_path);
    let source_tracked = pre_move_scope.contains(&old_forward);

    // A source that resolves to a tracked document under another
    // spelling (a case- or normalization-insensitive filesystem alias)
    // would, if treated as untracked, move the real file while every
    // exact-string comparison misses it — dangling all of its
    // references. The one filesystem-alias test lives in `path_guard`;
    // it only runs on the already-rare untracked-source path.
    if !source_tracked
        && let Some(canonical) = nodex_core::path_guard::find_scope_alias(
            root,
            Path::new(old_path),
            pre_move_scope.iter().map(|p| Path::new(p.as_str())),
        )
    {
        return Err(CoreError::Config(format!(
            "source {old_path:?} resolves to the tracked document {:?} (a filesystem \
             spelling alias); use the exact spelling so its references can be rewritten",
            nodex_core::path_guard::forward_string(&canonical)
        ))
        .into());
    }

    // The document as the move will leave it, decided before anything is
    // written. Every pre-move question is asked of *these* bytes: they are
    // what the destination will hold, and an anchored id is a difference the
    // scope probe and the lock gate below both have to see. An untracked
    // source has no graph id to keep stable — its move is a plain one.
    let moved = source_tracked
        .then(|| {
            plan_moved_document(
                root,
                &old_abs,
                Path::new(old_path),
                Path::new(new_path),
                &config,
            )
        })
        .transpose()?;

    if let Some(moved) = &moved {
        // Refuse a destination the scan would not admit *post-move*.
        // The probe models the post-move world through the same scope
        // authority the build uses: the moved document's bytes are
        // overlaid at the destination (its status is what a
        // conditional-exclude evaluation reads there), and the source
        // path is overlaid empty — equivalent to absent for every other
        // path's admission, so the still-on-disk source can't act as
        // its own terminal parent and veto its own move.
        let post_move_scan = nodex_core::builder::scanner::scan_scope_with_overlay(
            root,
            &config,
            &[
                (Path::new(new_path).to_path_buf(), moved.destination.clone()),
                (
                    Path::new(old_path).to_path_buf(),
                    nodex_core::builder::scanner::Proposed::Absent,
                ),
            ],
        )
        .context("destination scope probe failed")?;
        if !post_move_scan
            .paths
            .iter()
            .any(|p| p == Path::new(new_path))
        {
            let cause = if matches!(moved.destination, Proposed::Absent) {
                "it is a symlink, and moving the link leaves its target unreachable from there \
                 (a relative target resolves against the new parent) — move the document the \
                 link points at, or repoint the link first"
            } else if post_move_scan
                .conditionally_excluded
                .iter()
                .any(|p| p == Path::new(new_path))
            {
                "a [[scope.conditional_exclude]] rule drops it there (a terminal parent's \
                 sub-artifact); change the parent's status or the rule"
            } else {
                "it is outside scope.include / inside scope.exclude; adjust the path or the \
                 scope config in nodex.toml"
            };
            return Err(CoreError::Config(format!(
                "rename destination {new_path:?} would not be graphed — {cause}"
            ))
            .into());
        }
    }

    // Refuse a destination filename the project's `rules.naming` reject:
    // the moved document would land and then be flagged by its own
    // `filename_pattern` check (the self-consistency invariant — a tool
    // never writes a doc that fails the project's own rules). The same
    // predicate the rule uses decides here, so they cannot disagree.
    if let Some(rule) =
        nodex_core::rules::naming::first_filename_violation(&config, Path::new(new_path))
    {
        return Err(CoreError::Config(format!(
            "rename destination {new_path:?} violates rules.naming pattern {:?} (glob {:?}); \
             choose a conforming filename or adjust the naming rule",
            rule.pattern, rule.glob
        ))
        .into());
    }

    // Immutability lock probe: the baseline snapshot a `check` against
    // `immutable_baseline` would diff against, resolved once for the
    // command. Outside a git work tree (or with no baseline) those rules
    // are inert for `check`, so the probe is inert too — the mutation
    // seam consults it per file, and the advisory rides the envelope
    // whether or not this rename had references to rewrite.
    //
    // Resolved before anything is written. A rename is a move plus a
    // reference rewrite, and the move is the half that cannot be undone:
    // a baseline that refuses the run must refuse it while the tree is
    // still untouched, not between the two halves.
    let probe = super::git_worktree::write_baseline(root, &config)?;

    // The move itself is a mutation the rules judge, and this is the only side
    // of it where a refusal can be honoured.
    //
    // A move writes no bytes, so nothing about it is expressible as a rewrite —
    // and a gate that only sees rewrites cannot see it at all. What it does
    // change is the document's *path*, and every field config derives from a
    // path moves with it: `kind` through `identity.kind_rules`, `title` through
    // the stem. A `frontmatter_immutable` lock on either of those fires at
    // check time on a terminal document that crossed a rule boundary, so the
    // seam has to refuse the move for the same reason it refuses a rewrite.
    //
    // The proposal is the post-move project: the document as it will exist at
    // the destination — id anchoring included, since that is part of what the
    // move writes — and the source gone. Both are knowable here, before
    // `fs::rename` — which matters, because afterwards a refusal cannot be
    // honoured.
    let stability = if let Some(moved) = moved {
        let proposal = [
            (Path::new(new_path).to_path_buf(), moved.destination.clone()),
            (
                Path::new(old_path).to_path_buf(),
                nodex_core::builder::scanner::Proposed::Absent,
            ),
        ];

        // The project the move produces has to be graphable, and that is a
        // question of its own — not a side effect of asking about locks, which
        // a project with no baseline never asks. Downstream, `fs::rename` has
        // landed and the reference rewrite can only degrade to a warning, so a
        // graph the move breaks has to be refused here, while refusing still
        // undoes nothing. `retarget` and `scaffold` establish the same
        // precondition by building before they write.
        nodex_core::builder::build_with_overlay(root, &config, &proposal)
            .context("the project this move would produce does not build")?;

        let refusals = probe.refusals(root, &config, &proposal, today)?;
        // Two refusals with different causes and different remedies, so they
        // are reported apart. A rule the moved document would carry is about
        // the state the move leaves it in; a destroyed record is about the
        // record ceasing to exist, which no field change describes.
        if let Some((path, lock)) = refusals.destroyed() {
            let record = nodex_core::path_guard::forward_string(path);
            return Err(CoreError::Config(format!(
                "rename cannot complete: moving {old_path:?} to {new_path:?} would leave the \
                 baseline record at {record:?} with no counterpart in the project, and it is \
                 frozen ({lock}). A record travels under its id: pin one with an explicit `id:` \
                 if this document's is derived from its path, and move a document that other \
                 records depend on to a location that still graphs them"
            ))
            .into());
        }
        if let Some(lock) = refusals
            .refusing(Path::new(new_path))
            .or_else(|| refusals.refusing(Path::new(old_path)))
        {
            return Err(CoreError::Config(format!(
                "rename cannot complete: moving {old_path:?} to {new_path:?} would leave this \
                 document in a state its baseline locks — {lock}. Plain `nodex check` names the \
                 same violation on the document as it stands; clear that, or supersede the \
                 record instead of moving it"
            ))
            .into());
        }

        // The anchor is written to the source, and `fs::rename` below carries
        // it to the destination — so the bytes the gate judged are the bytes
        // that land. Writing before the move keeps a write failure clean: the
        // document is intact and nothing has moved.
        if let Some(anchor) = &moved.anchor {
            nodex_core::path_guard::write_atomic_in_root(root, &old_abs, anchor)?;
        }
        moved.stability
    } else {
        IdStability::Unchanged
    };

    if let Some(parent) = new_abs.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // The file move itself stays a `rename` — that *is* the atomic
    // primitive. The link rewriter below has to be guarded separately.
    std::fs::rename(&old_abs, &new_abs).map_err(|source| CoreError::Io {
        path: old_abs.clone(),
        source,
    })?;

    // Repoint references only when the moved file was a graph document:
    // an untracked source has no edges anywhere, so there is nothing to
    // rewrite (and a plain move is exactly what was asked for).
    let (updated_files, skipped) = if source_tracked {
        rewrite_all_references(
            root,
            &config,
            &probe,
            old_path,
            new_path,
            &pre_move_scope,
            today,
        )?
    } else {
        (Vec::new(), Vec::new())
    };

    let mut warnings: Vec<nodex_core::Warning> = match &stability {
        IdStability::BareNoFrontmatter { warning } => vec![nodex_core::Warning::new(
            nodex_core::WarningCode::BuildRecommended,
            warning.clone(),
        )],
        _ => Vec::new(),
    };
    warnings.extend(
        skipped
            .into_iter()
            .map(|w| nodex_core::Warning::new(nodex_core::WarningCode::FileSkipped, w)),
    );

    let data = RenameResult {
        old_path: nodex_core::path_guard::forward_str(old_path),
        new_path: nodex_core::path_guard::forward_str(new_path),
        total_updated: updated_files.len(),
        references_updated: updated_files,
        id_stability: stability,
    };

    emit_write(data, warnings, &probe, pretty);

    Ok(())
}

/// Repoint every reference to the moved document — inbound links from
/// every other in-scope file, and the moved file's own self- and
/// directory-sensitive references. Detection and rewriting are
/// delegated to `reference_rewrite`, which reuses the build-time
/// resolver's candidate ladder (so it rewrites exactly the links the
/// graph treats as edges) and is code-fence aware (a link inside a code
/// sample is never mutated). Returns `(updated_files, skip_warnings)`.
/// Which of a rename's two rewrite shapes a plan is, so a refusal reads in
/// the caller's own words rather than a generic one.
enum PlanKind {
    /// A file that references the renamed document.
    Inbound,
    /// The renamed document itself.
    Moved,
}

fn rewrite_all_references(
    root: &Path,
    config: &Config,
    probe: &nodex_core::BaselineProbe,
    old_path: &str,
    new_path: &str,
    pre_move_scope: &BTreeSet<String>,
    today: NaiveDate,
) -> Result<(Vec<String>, Vec<String>)> {
    // `fs::rename` has already landed, so this is past the point where an
    // error can be honoured: aborting would strand the move and say only that
    // a scan failed. The scan is what finds the referrers, so its failure
    // means none can be rewritten — one warning that says exactly that, in the
    // same skip discipline every other failure here follows.
    let paths = match nodex_core::builder::scanner::scan_scope(root, config) {
        Ok(scan) => scan.paths,
        Err(e) => {
            return Ok((
                Vec::new(),
                vec![format!(
                    "the move landed, but the project could not be scanned for referrers ({}), \
                     so no reference was rewritten — fix that, then re-run the rewrites",
                    nodex_core::error::chain(&e)
                )],
            ));
        }
    };

    let old_rel = Path::new(old_path);
    let new_rel = Path::new(new_path);
    let new_rel_forward = nodex_core::path_guard::forward_string(new_rel);
    // The scope as the scanner sees it now (post-move): `new_path`
    // present, `old_path` gone. `rewrite_moved_references` rebases the
    // moved file's outbound links against this current world.
    let post_move_scope: BTreeSet<String> = paths
        .iter()
        .map(|p| nodex_core::path_guard::forward_string(p))
        .collect();
    let mut updated_files = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut plans: Vec<(nodex_core::Planned, PlanKind)> = Vec::new();

    // Visit every file that is in scope before OR after the move: a
    // referencing file can be evicted from scope *by* the move (a
    // `conditional_exclude` parent landing in its directory), yet it
    // still holds a real pre-move edge to the renamed file that must be
    // repointed. Iterating only the post-move scan would silently leave
    // that edge dangling. The old path (now gone) and the new path (the
    // moved file, handled separately) are excluded.
    let old_rel_forward = nodex_core::path_guard::forward_string(old_rel);
    let inbound: BTreeSet<&String> = pre_move_scope
        .union(&post_move_scope)
        .filter(|p| **p != old_rel_forward && **p != new_rel_forward)
        .collect();

    for rel in inbound {
        let rel_path = Path::new(rel);
        let source_dir = rel_path.parent().unwrap_or_else(|| Path::new(""));
        // An unsplittable fence in a referencing file is a per-file
        // skip, never a batch abort: `fs::rename` has already moved the
        // document, so aborting here would strand a half-applied batch.
        // The transform reports "no change" and the warning names the
        // file (which already reds `check` as a `parse_failure`; its
        // stale reference surfaces as an unresolved edge) — the
        // classify-and-skip discipline migrate's apply phase follows.
        let mut unsplittable: Option<String> = None;
        let rewrite = |content: &str| match nodex_core::reference_rewrite::rewrite_references(
            content,
            source_dir,
            old_rel,
            new_rel,
            pre_move_scope,
            &config.parser,
        ) {
            Ok(change) => Ok(change),
            Err(_) => {
                unsplittable = Some(format!(
                    "{rel} may reference the renamed file but its frontmatter fence does \
                         not parse, so it was not rewritten (it already fails `check` as a \
                         parse_failure) — fix the fence, then repoint the reference manually"
                ));
                Ok(None)
            }
        };
        // The reader-follows / writer-skips symlink discipline and the read
        // live in the one core seam. The lock is asked once for the whole
        // rename, after every rewrite is planned.
        match nodex_core::mutate::plan_file(root, rel_path, rewrite, || {
            format!(
                "{} references the renamed file but is or resolves through a symlink; it was \
                 not rewritten (writing through a symlink could escape the project root) — \
                 update it manually",
                nodex_core::path_guard::forward_string(rel_path)
            )
        })? {
            nodex_core::PlanOutcome::Planned(plan) => plans.push((plan, PlanKind::Inbound)),
            nodex_core::PlanOutcome::Skipped(warning) => skipped.push(warning),
            nodex_core::PlanOutcome::Unchanged => {
                if let Some(warning) = unsplittable {
                    skipped.push(warning);
                }
            }
        }
    }

    // ─── moved file's own references ───────────────────────────────
    //
    // Two passes composed on one buffer, so the file is read and
    // written at most once. Pass 1 repoints self-references (links to
    // the old path, still spelled from the old directory's vantage
    // point). Pass 2 rebases every directory-sensitive reference from
    // the old directory to the new one — a no-op for same-directory
    // renames. Both passes share the resolver's candidate ladder, so
    // they bind references exactly as the graph does.
    let old_dir = old_rel.parent().unwrap_or_else(|| Path::new(""));
    let new_dir = new_rel.parent().unwrap_or_else(|| Path::new(""));
    // Same per-file skip as the inbound loop: with the move already on
    // disk, an unsplittable fence in the moved file leaves its own
    // references unrewritten with a warning — never a batch abort.
    let mut moved_unsplittable: Option<String> = None;
    let rewrite_moved = |content: &str| -> nodex_core::Result<Option<String>> {
        // Pass 1 repoints the moved file's self-references (links that
        // bound `old_path`) — resolved against the pre-move scope, same
        // as the inbound loop. Pass 2 rebases its outbound links to
        // other files against the post-move world.
        let inner = || {
            let pass1 = nodex_core::reference_rewrite::rewrite_references(
                content,
                old_dir,
                old_rel,
                new_rel,
                pre_move_scope,
                &config.parser,
            )?;
            let base = pass1.as_deref().unwrap_or(content);
            Ok::<_, nodex_core::error::ParseError>(
                nodex_core::reference_rewrite::rewrite_moved_references(
                    base,
                    old_dir,
                    new_dir,
                    &post_move_scope,
                    &config.parser,
                )?
                .or(pass1),
            )
        };
        match inner() {
            Ok(change) => Ok(change),
            Err(_) => {
                moved_unsplittable = Some(format!(
                    "{new_rel_forward} carries references that need rebasing but its \
                     frontmatter fence does not parse, so it was not rewritten (it already \
                     fails `check` as a parse_failure) — fix the fence, then rebase its \
                     references manually"
                ));
                Ok(None)
            }
        }
    };
    // `fs::rename` moved the file (or the symlink itself); the same one core
    // seam guards the write. The moved document is planned at its *new* path,
    // which is where the next build will read it — so the fields config
    // derives from a path (`title` from the stem, `kind` from
    // `identity.kind_rules`) are the ones the rules will judge, and the
    // pairing is the id the overlay assigns rather than one reconstructed
    // from where the document used to live.
    match nodex_core::mutate::plan_file(root, new_rel, rewrite_moved, || {
        format!(
            "{new_rel_forward} carries references that need rebasing but is or resolves \
             through a symlink; it was not rewritten (writing through a symlink could escape \
             the project root) — update it manually"
        )
    })? {
        nodex_core::PlanOutcome::Planned(plan) => plans.push((plan, PlanKind::Moved)),
        nodex_core::PlanOutcome::Skipped(warning) => skipped.push(warning),
        nodex_core::PlanOutcome::Unchanged => {
            if let Some(warning) = moved_unsplittable {
                skipped.push(warning);
            }
        }
    }

    // One gate for the whole rename: a rename that repoints N referrers is one
    // atomic edit, and asking per file would judge each against a project the
    // other rewrites had not landed in yet.
    //
    // The gate builds the whole project, so it can fail for reasons that have
    // nothing to do with this rename — a duplicate id or a supersedes cycle the
    // project already carried. `fs::rename` has already landed by here, so
    // propagating that error would abort mid-batch and strand the move with its
    // references unrewritten and nothing said about why. The same per-file-skip
    // discipline the loops above follow applies: an unevaluable lock refuses
    // the writes it guards, and the cause is reported.
    let proposal: Vec<_> = plans.iter().map(|(plan, _)| plan.proposed()).collect();
    let refusals = match probe.refusals(root, config, &proposal, today) {
        Ok(refusals) => refusals,
        Err(e) => {
            skipped.push(format!(
                "the move landed, but the immutability locks could not be evaluated \
                 ({}), so no reference was rewritten — the project must build before a \
                 rename can rebase references; fix that, then re-run the rewrites",
                nodex_core::error::chain(&e)
            ));
            return Ok((updated_files, skipped));
        }
    };
    for (plan, kind) in &plans {
        let shown = nodex_core::path_guard::forward_string(&plan.rel_path);
        match refusals.refusing(&plan.rel_path) {
            Some(lock) => skipped.push(match kind {
                PlanKind::Inbound => format!(
                    "{shown} references the renamed file but is locked ({lock}); it was not \
                     rewritten — the stale reference will surface as an unresolved edge"
                ),
                PlanKind::Moved => format!(
                    "{shown} carries references that need rebasing but is locked ({lock}); it \
                     was not rewritten — its stale self-references will surface as unresolved \
                     edges"
                ),
            }),
            None => match nodex_core::mutate::write_plan(root, plan) {
                Ok(()) => updated_files.push(shown),
                // The move has landed, so an abort here would strand it and
                // discard the record of what the surviving rewrites did. One
                // unwritable file is one skipped reference, named like every
                // other skip — and named by which edge it is, since the moved
                // document's own outbound links go stale differently from a
                // referrer's inbound one.
                Err(e) => skipped.push(match kind {
                    PlanKind::Inbound => format!(
                        "{shown} references the renamed file but could not be rewritten ({}); \
                         the stale reference will surface as an unresolved edge",
                        nodex_core::error::chain(&e)
                    ),
                    PlanKind::Moved => format!(
                        "{shown} carries references that need rebasing but could not be \
                         rewritten ({}); its links are still spelled relative to the old \
                         location and now resolve from the new one",
                        nodex_core::error::chain(&e)
                    ),
                }),
            },
        }
    }

    Ok((updated_files, skipped))
}

/// The document as it will exist once the move lands, and what the move did
/// to its id.
struct MovedDocument {
    /// What the destination will hold. For a plain file that is the source's
    /// own bytes, anchored when the id had to be pinned. `rename` moves a file
    /// symlink as the link itself, so for one of those it is whatever the link
    /// resolves to *from the destination* — a relative target that changes
    /// directory depth lands elsewhere, and often nowhere.
    destination: nodex_core::builder::scanner::Proposed,
    /// The source's rewritten bytes, when the id had to be anchored into them.
    anchor: Option<String>,
    stability: IdStability,
}

/// What the moved symlink will resolve to from its destination.
///
/// The bytes the graph reads at the source come from following the link, and
/// following it from somewhere else is a different question with a different
/// answer. Judging the source's bytes lets every pre-move gate approve a
/// document the move does not produce — including the destruction guard, which
/// then sees a record that will not exist.
fn destination_through_link(root: &Path, old_abs: &Path, new_rel: &Path) -> Proposed {
    let Ok(target) = std::fs::read_link(old_abs) else {
        return Proposed::Absent;
    };
    let resolved = if target.is_absolute() {
        target
    } else {
        match root.join(new_rel).parent() {
            Some(parent) => parent.join(&target),
            None => target,
        }
    };
    std::fs::read_to_string(resolved).map_or(Proposed::Absent, Proposed::Content)
}

/// Read the doc at `old_abs`, compare its effective id against the id
/// it *would* infer at `new_rel`, and — if a path-derived id would
/// change — pin the previous id into the frontmatter the moved document
/// carries, so the move doesn't silently break every cross-document
/// `related:` / `supersedes:` / `implements:` reference in the rest of
/// the graph. This is the single seam where rename guarantees that the
/// file-system primitive (`fs::rename`) doesn't produce a broken semantic
/// graph.
///
/// Deciding is separate from writing because the lock gate judges the
/// document the move produces, not the one it started from: an anchored id
/// is the difference between a record that survives the move and one the
/// baseline sees destroyed.
fn plan_moved_document(
    root: &Path,
    old_abs: &Path,
    old_rel: &Path,
    new_rel: &Path,
    config: &Config,
) -> Result<MovedDocument> {
    let raw = std::fs::read_to_string(old_abs).map_err(|source| CoreError::Io {
        path: old_abs.to_path_buf(),
        source,
    })?;
    // The move carries the link, not the bytes it currently reaches, so the
    // destination is resolved from the destination. The source's bytes still
    // decide the id, because that is the document the graph holds today.
    let via_link = nodex_core::path_guard::is_symlink(old_abs);
    let destination = |content: String| {
        if via_link {
            destination_through_link(root, old_abs, new_rel)
        } else {
            Proposed::Content(content)
        }
    };
    // Route through the same canonicalisation (BOM strip, CRLF/CR → LF)
    // every parser entry uses, so frontmatter delimited with Windows
    // line endings splits identically here and in the build — otherwise
    // a CRLF document with an explicit `id:` would be mis-read as bare
    // and its id silently left un-anchored.
    // An unsplittable fence refuses the rename outright: the anchoring
    // decision depends on frontmatter this file does not parseably
    // declare, and a path move on top of a malformed document would
    // compound the breakage. Fix the fence (it is a `parse_failure`
    // violation in `check`), then rename.
    let canonical = canonicalize(&raw).into_owned();
    let (yaml_opt, body) = split_frontmatter(&canonical).map_err(|source| CoreError::Parse {
        path: old_abs.to_path_buf(),
        source,
    })?;

    let Some(yaml) = yaml_opt else {
        // Bare markdown: nodex still infers an id from the path and
        // other docs can reference it. Path change → id change. We
        // refuse to silently invent a frontmatter block (too invasive
        // for a path operation), but surface a warning so the caller
        // can fix up references manually instead of discovering broken
        // edges on the next `build`. Kind inference is purely
        // path-driven here — a bare doc has no frontmatter `kind:`.
        let old_kind = infer_kind(old_rel, &config.identity);
        let new_kind = infer_kind(new_rel, &config.identity);
        let inferred_old_id = infer_id(old_rel, &old_kind, &config.identity);
        let inferred_new_id = infer_id(new_rel, &new_kind, &config.identity);
        if inferred_old_id != inferred_new_id {
            return Ok(MovedDocument {
                destination: destination(raw),
                anchor: None,
                stability: IdStability::BareNoFrontmatter {
                    warning: format!(
                        "renamed file has no frontmatter; its inferred id changed from \
                         {inferred_old_id:?} to {inferred_new_id:?}. Other documents \
                         referencing {inferred_old_id:?} via `related` / `supersedes` / \
                         `implements` / `superseded_by` will become stale, and any \
                         immutability lock held against {inferred_old_id:?} no longer \
                         governs this document — the locks pair with the baseline by id. \
                         Add an explicit `id:` frontmatter to the file (or run \
                         `nodex migrate --apply` to generate one) and re-run rename to \
                         re-anchor."
                    ),
                },
            });
        }
        return Ok(MovedDocument {
            destination: destination(raw),
            anchor: None,
            stability: IdStability::Unchanged,
        });
    };

    let mut editor = FrontmatterEditor::parse(yaml, old_abs)?;
    // Explicit id first: an already-pinned id makes the move path-only
    // by construction — no anchoring, so a broken `kind:` is irrelevant.
    match editor.scalar("id") {
        Scalar::Value(v) if !v.is_empty() => {
            return Ok(MovedDocument {
                destination: destination(raw),
                anchor: None,
                stability: IdStability::AlreadyAnchored,
            });
        }
        Scalar::NonScalar => {
            // An `id:` field that isn't a scalar (e.g., a list) is a
            // pre-existing authoring error; surface it instead of
            // attempting to silently overwrite the structure.
            return Err(CoreError::Config(format!(
                "{} has an `id:` field that is not a scalar; cannot anchor it during rename",
                old_abs.display()
            ))
            .into());
        }
        Scalar::Value(_) | Scalar::Absent => {}
    }

    // Effective kind exactly as the build derives it: the frontmatter
    // `kind:` wins and path inference is only the fallback — the anchor
    // must pin the id the build actually assigns, or it would write a
    // wrong id and break the very references it exists to protect. A
    // declared kind travels with the file, so it drives both sides;
    // without one, each path infers independently.
    let (old_kind, new_kind) = match editor.scalar("kind") {
        Scalar::Value(k) if !k.is_empty() => {
            let declared = nodex_core::model::Kind::new(k.as_ref());
            (declared.clone(), declared)
        }
        Scalar::NonScalar => {
            // A non-scalar `kind:` means the build cannot parse this
            // document at all — there is no node, so there is no id to
            // anchor. Writing a path-inferred id into an unparseable
            // file would be phantom work; refuse loudly instead.
            return Err(CoreError::Config(format!(
                "{} has a `kind:` field that is not a scalar; the build cannot parse this \
                 document, so rename refuses to anchor an id into it — fix the frontmatter \
                 first",
                old_abs.display()
            ))
            .into());
        }
        Scalar::Value(_) | Scalar::Absent => (
            infer_kind(old_rel, &config.identity),
            infer_kind(new_rel, &config.identity),
        ),
    };
    let inferred_old_id = infer_id(old_rel, &old_kind, &config.identity);
    let inferred_new_id = infer_id(new_rel, &new_kind, &config.identity);

    if inferred_old_id == inferred_new_id {
        return Ok(MovedDocument {
            destination: destination(raw),
            anchor: None,
            stability: IdStability::Unchanged,
        });
    }

    // Anchoring writes into the source, and the write seam refuses a symlink
    // target — replacing the link is never what a document mutation means. So
    // the id cannot be pinned here, and moving the link would silently change
    // the document's id. Refuse, naming the document's real home.
    if via_link {
        return Err(CoreError::Config(format!(
            "rename cannot anchor an id into {}: it is a symlink, and the document lives at its \
             target. Moving the link would change the document's inferred id from \
             {inferred_old_id:?} to {inferred_new_id:?} with no way to pin it — rename the \
             target instead, or give the document an explicit `id:`",
            old_abs.display()
        ))
        .into());
    }

    editor.set("id", &inferred_old_id);
    let new_frontmatter = editor.render();
    let anchored = format!("---\n{new_frontmatter}---\n{body}");
    Ok(MovedDocument {
        destination: Proposed::Content(anchored.clone()),
        anchor: Some(anchored),
        stability: IdStability::Anchored {
            id: inferred_old_id,
        },
    })
}
