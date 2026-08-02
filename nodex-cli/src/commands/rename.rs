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
    let old_norm = nodex_core::path_guard::normalize_doc_path(root, &args.old)?;
    let new_norm = nodex_core::path_guard::normalize_doc_path(root, &args.new)?;
    let old_path = old_norm.as_str();
    let new_path = new_norm.as_str();

    let old_rel = Path::new(old_path);
    let new_rel = Path::new(new_path);
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
    // `symlink_metadata`, like the source above: the question is whether the
    // destination name is taken, and `exists` answers whether it *resolves*.
    // A symlink pointing at nothing is an entry someone made, and the move
    // below is a raw `fs::rename` — it would replace the link without a word.
    if std::fs::symlink_metadata(&new_abs).is_ok() {
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
    let pre_move_scan = nodex_core::builder::scanner::scan_scope(root, &config)
        .context("pre-move scope scan failed")?;
    let pre_move_scope: BTreeSet<String> = pre_move_scan
        .paths
        .iter()
        .map(|p| nodex_core::path_guard::forward_string(p))
        .collect();

    // A followed link gives one document several names, and the graph carries
    // one. Moving it by a name the graph does not carry reads as an untracked
    // source — a plain move, no reference rewriting — so the real file leaves
    // and every reference to the name in use dangles.
    if let Some((_, named)) = pre_move_scan
        .aliases
        .iter()
        .find(|(unused, _)| unused == Path::new(old_path))
    {
        return Err(CoreError::Config(format!(
            "source {old_path:?} names the document the graph carries as {:?}; use that path so \
             its references can be rewritten",
            nodex_core::path_guard::forward_string(named)
        ))
        .into());
    }

    // An untracked source — outside scope or conditionally excluded —
    // has no node and no edges: nothing can dangle, so the rename is a
    // plain guarded move with no destination gate, no id anchoring, and
    // no reference rewriting. The gate below therefore fires exactly
    // when it protects something: moving a *tracked* document to a spot
    // the scan would not admit would silently drop it from the graph
    // and orphan its edges.
    let old_forward = nodex_core::path_guard::forward_str(old_path);
    let source_tracked = pre_move_scope.contains(&old_forward);

    // The document as the move will leave it, decided before anything is
    // written. Every pre-move question is asked of *these* bytes: they are
    // what the destination will hold, and an anchored id is a difference the
    // scope probe and the lock gate below both have to see. An untracked
    // source has no graph id to keep stable — its move is a plain one.
    let moved = source_tracked
        .then(|| plan_moved_document(root, &old_abs, old_rel, new_rel, &config))
        .transpose()?;

    let mut skipped: Vec<String> = Vec::new();

    // What the destination will hold, for a source the graph carries and one
    // it does not alike: every rename proposes the same project — this file's
    // content at the new path, and nothing at the old one. A path outside the
    // graph today can land inside it (a draft moved into `docs/`), so the gate
    // below has to be asked either way.
    let destination = match &moved {
        Some(moved) => match &moved.destination {
            Proposed::Content(content) => Some(content.clone()),
            // Only a moved symlink resolves to nothing, and for a document the
            // graph carries that ends the rename: the move would put it out of
            // the project's reach entirely.
            Proposed::Absent => {
                return Err(CoreError::Config(format!(
                    "rename destination {new_path:?} would not be graphed — it is a symlink, and \
                     moving the link leaves its target unreachable from there (a relative target \
                     resolves against the new parent) — move the document the link points at, or \
                     repoint the link first"
                ))
                .into());
            }
        },
        None => untracked_destination(root, &config, &old_abs, new_rel, new_path, &mut skipped)?,
    };

    // The scope as the move leaves it, modelled through the same scope
    // authority the build uses: the moved document's bytes are overlaid at
    // the destination (its status is what a conditional-exclude evaluation
    // reads there), and the source path is overlaid empty — equivalent to
    // absent for every other path's admission, so the still-on-disk source
    // can't act as its own terminal parent and veto its own move. Nothing has
    // been written, so this is also the world every gate below is asked about.
    let move_overlay: Vec<(std::path::PathBuf, Proposed)> = vec![
        (
            new_rel.to_path_buf(),
            match &destination {
                Some(content) => Proposed::Content(content.clone()),
                None => Proposed::Absent,
            },
        ),
        (old_rel.to_path_buf(), Proposed::Absent),
    ];
    let post_move_scan =
        nodex_core::builder::scanner::scan_scope_with_overlay(root, &config, &move_overlay)
            .context("destination scope probe failed")?;

    if moved.is_some() && !post_move_scan.paths.iter().any(|p| p == new_rel) {
        return Err(CoreError::Config(format!(
            "rename destination {new_path:?} would not be graphed — {}",
            unadmitted_cause(&post_move_scan, new_rel)
        ))
        .into());
    }

    // The project as it stands — what every gate below compares against, and
    // the graph the baseline is asked to pair its records with by id.
    let before = nodex_core::builder::build_with_overlay(root, &config, &[])
        .context("graph build failed")?;

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
    // The project the move produces. Asked for by the lock gates below —
    // of every move, not only of a document the graph carries, because a
    // source outside the graph still lands somewhere and the path it lands
    // on may hold a record the baseline froze — and asked again by the
    // rebase, which reads every reference of the moved document against
    // where it will stand. That it is graphable at all is a question of
    // its own, not a side effect of asking about locks, which a project
    // with no baseline never asks.
    let proposed = nodex_core::builder::build_with_overlay(root, &config, &move_overlay)
        .context("the project this move would produce does not build")?;

    {
        let refusals = probe.refusals(root, &config, &move_overlay, today)?;
        // Two refusals with different causes and different remedies, so they
        // are reported apart. A rule the moved document would carry is about
        // the state the move leaves it in; a destroyed record is about the
        // record ceasing to exist, which no field change describes.
        if let Some((path, lock)) = refusals.destroyed() {
            let record = nodex_core::path_guard::forward_string(path);
            return Err(CoreError::Config(format!(
                "rename cannot complete: moving {old_path:?} to {new_path:?} would leave the \
                 baseline record at {record:?} with nothing carrying its id, and it is frozen \
                 ({lock}). A record travels under its id: give the document an explicit `id:` if \
                 this one is derived from its path, or move it where it stays readable"
            ))
            .into());
        }
        // A frozen record the project no longer holds anywhere is invisible to
        // every rule — `check` reads its replacement as a removal plus an
        // addition and consumes neither — so the baseline is asked directly,
        // exactly as `scaffold` asks before writing to such a path. Landing a
        // document on it replaces frozen history whatever the bytes came
        // from, and the remedy is the same: supersede the record.
        //
        // Asked by id, of the project the move produces — the same graph
        // `scaffold` asks. A record travels under its id, so one that merely
        // *moved* has left its path free; and a move that carries the record
        // back onto its own path loses nothing, which the project as it
        // stands cannot say because the record is missing from it either way.
        if let Some(lock) = probe.frozen_record_lost(new_rel, &proposed.graph, &config) {
            return Err(CoreError::Config(format!(
                "rename cannot complete: moving {old_path:?} to {new_path:?} would write over a \
                 record the baseline froze there ({lock}); supersede the record instead of \
                 replacing it"
            ))
            .into());
        }
        if let Some(lock) = refusals
            .refusing(new_rel)
            .or_else(|| refusals.refusing(old_rel))
        {
            return Err(CoreError::Config(format!(
                "rename cannot complete: moving {old_path:?} to {new_path:?} would leave this \
                 document in a state its baseline locks — {lock}. Plain `nodex check` names the \
                 same violation on the document as it stands; clear that, or supersede the \
                 record instead of moving it"
            ))
            .into());
        }
    }

    // Every reference the move invalidates, planned while the tree is still
    // untouched. Which of them can be repointed is knowable here — a lock, a
    // symlink, an unsplittable fence each refuse a rewrite for reasons the
    // move does not change — so the project this rename really produces is
    // knowable too, and the gate below can still refuse it.
    let plans = match (source_tracked, destination.as_deref()) {
        (true, Some(destination)) => {
            let post_move_scope: BTreeSet<String> = post_move_scan
                .paths
                .iter()
                .map(|p| nodex_core::path_guard::forward_string(p))
                .collect();
            plan_all_references(
                root,
                &config,
                old_rel,
                new_rel,
                destination,
                &pre_move_scope,
                &post_move_scope,
                nodex_core::builder::resolver::Worlds {
                    before: &nodex_core::builder::resolver::Bindings::of_graph(&before.graph),
                    after: &nodex_core::builder::resolver::Bindings::of_graph(&proposed.graph),
                },
                &mut skipped,
            )?
        }
        _ => Vec::new(),
    };

    // One lock gate for the whole rename: a rename that repoints N referrers
    // is one atomic edit, and asking per file would judge each against a
    // project the other rewrites had not landed in yet. The move is part of
    // the proposal, so each rewrite is judged in the project it lands in.
    let mut proposal = move_overlay.clone();
    for (plan, _) in &plans {
        overlay_with(&mut proposal, plan);
    }
    let refusals = probe
        .refusals(root, &config, &proposal, today)
        .context("the immutability locks could not be evaluated")?;
    let mut writable: Vec<&nodex_core::Planned> = Vec::new();
    for (plan, kind) in &plans {
        let shown = nodex_core::path_guard::forward_string(&plan.rel_path);
        match refusals.refusing(&plan.rel_path) {
            Some(lock) => skipped.push(match kind {
                PlanKind::Inbound => format!(
                    "{shown} references the renamed file but is locked ({lock}); it was not \
                     rewritten — the stale reference will surface as an unresolved edge"
                ),
                PlanKind::Rewritten => format!(
                    "{shown} carries references that need rebasing but is locked ({lock}); it \
                     was not rewritten — its stale self-references will surface as unresolved \
                     edges"
                ),
            }),
            None => writable.push(plan),
        }
    }

    // The project this rename really produces — the move, plus exactly the
    // rewrites that will land. A reference the seam could not repoint is in
    // it as the stale reference it will be, so the gate answers for the
    // rename as performed rather than as intended.
    let mut final_proposal = move_overlay.clone();
    for plan in &writable {
        overlay_with(&mut final_proposal, plan);
    }
    let introduced = nodex_core::introduced(
        root,
        &config,
        &before.graph,
        &final_proposal,
        nodex_core::ProposalDiff::Inert,
        today,
    )
    .context("the project this rename would produce could not be checked")?;
    if let Some(refusal) = introduced.refusal(format!("moving {old_path:?} to {new_path:?}")) {
        return Err(refusal.into());
    }

    // Past here nothing can be refused, and everything that could be has been.
    //
    // A rename is one edit across several files, and the gate judged it whole,
    // so it has to land whole. Every write is staged first — the content on
    // disk beside its target, waiting for a rename — because that is where the
    // failures live: an unwritable directory, a full disk. A staging failure
    // leaves the tree exactly as it was, every staged write dropped, and the
    // command refuses. What remains after that is same-directory renames, the
    // atomic primitive itself.
    let mut staged: Vec<(&nodex_core::Planned, nodex_core::path_guard::Staged)> = Vec::new();
    for plan in &writable {
        staged.push((
            plan,
            nodex_core::mutate::stage_plan(root, plan).with_context(|| {
                format!(
                    "the reference in {} could not be staged, so nothing was written",
                    nodex_core::path_guard::forward_string(&plan.rel_path)
                )
            })?,
        ));
    }
    // The anchor is staged against the *source*, and `fs::rename` below carries
    // it to the destination — so the bytes the gates judged are the bytes that
    // land. It has to be committed before the move: afterwards the id it
    // preserves is already gone.
    let anchor = moved
        .as_ref()
        .and_then(|moved| moved.anchor.as_deref())
        .map(|anchor| nodex_core::path_guard::stage_in_root(root, &old_abs, anchor))
        .transpose()?;

    if let Some(parent) = new_abs.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    if let Some(anchor) = anchor {
        anchor.commit()?;
    }

    // The file move itself stays a `rename` — that *is* the atomic primitive.
    std::fs::rename(&old_abs, &new_abs).map_err(|source| CoreError::Io {
        path: old_abs.clone(),
        source,
    })?;

    let mut updated_files = Vec::new();
    for (plan, staged) in staged {
        let shown = nodex_core::path_guard::forward_string(&plan.rel_path);
        match staged.commit() {
            Ok(()) => updated_files.push(shown),
            // The move has landed, so an abort here would strand it and
            // discard the record of what the surviving rewrites did. A commit
            // is a rename within one directory of a file that already exists,
            // so what is left here is the filesystem failing at the primitive
            // — reported per file, like every other skip.
            Err(e) => skipped.push(format!(
                "{shown} could not be rewritten ({}); its reference to the renamed file is \
                 stale — repoint it manually",
                nodex_core::error::chain(&e)
            )),
        }
    }

    // The scan this move was planned against carries what the walk could not
    // read: a reference behind that boundary is one this rename did not
    // repoint.
    let mut warnings: Vec<nodex_core::Warning> =
        nodex_core::builder::scanner::boundary_warning(&pre_move_scan.unfollowed_in_scope, "graph")
            .into_iter()
            .collect();
    let stability = moved.map_or(IdStability::Unchanged, |moved| moved.stability);
    if let IdStability::BareNoFrontmatter { warning } = &stability {
        warnings.push(nodex_core::Warning::new(
            nodex_core::WarningCode::BuildRecommended,
            warning.clone(),
        ));
    }
    warnings.extend(introduced.advisories());
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

/// Fold one planned rewrite into a proposal, replacing rather than joining
/// what the proposal already says about that path.
///
/// The moved document is in every proposal at its destination, and its own
/// rebased bytes are a *later* statement about the same path — a proposal
/// carrying both says two things about one file, and whichever the overlay
/// reads first silently wins.
fn overlay_with(proposal: &mut Vec<(std::path::PathBuf, Proposed)>, plan: &nodex_core::Planned) {
    match proposal.iter_mut().find(|(path, _)| *path == plan.rel_path) {
        Some(entry) => entry.1 = Proposed::Content(plan.content.clone()),
        None => proposal.push(plan.proposed()),
    }
}

/// Why the post-move scan does not admit `new_rel` — the cause the operator
/// can act on, picked from what the probe itself recorded.
fn unadmitted_cause(
    scan: &nodex_core::builder::scanner::ScopeScan,
    new_rel: &Path,
) -> std::borrow::Cow<'static, str> {
    if let Some((_, named)) = scan.aliases.iter().find(|(unused, _)| unused == new_rel) {
        return format!(
            "the same document is named {:?} there, and the graph carries one name per document; \
             move to that path",
            nodex_core::path_guard::forward_string(named)
        )
        .into();
    }
    if let Some(link) = scan
        .unfollowed
        .iter()
        .find(|link| new_rel.starts_with(link))
    {
        return format!(
            "it is below {:?}, a directory symlink the scan does not descend; set \
             scope.follow_symlinks or move to the directory the link points at",
            nodex_core::path_guard::forward_string(link)
        )
        .into();
    }
    if scan.conditionally_excluded.iter().any(|p| p == new_rel) {
        return "a [[scope.conditional_exclude]] rule drops it there (a terminal parent's \
                sub-artifact); change the parent's status or the rule"
            .into();
    }
    "it is outside scope.include / inside scope.exclude; adjust the path or the scope config in \
     nodex.toml"
        .into()
}

/// The bytes a move leaves at the destination for a source the graph does not
/// carry.
///
/// Such a source has no node and no edges, so nothing can dangle behind it —
/// but it can still land somewhere the graph reaches, and the document that
/// arrives there is one the rules govern. So the same proposal is formed as
/// for a tracked source.
///
/// `None` where the move leaves no document to propose: a symlink whose target
/// the destination cannot reach, or bytes that are not text. Neither is a
/// document any rule can be asked about, and the overlay has no way to say so
/// — which the caller reports rather than passes over, since the move still
/// happens.
fn untracked_destination(
    root: &Path,
    config: &Config,
    old_abs: &Path,
    new_rel: &Path,
    new_path: &str,
    skipped: &mut Vec<String>,
) -> Result<Option<String>> {
    let proposed = if nodex_core::path_guard::is_symlink(old_abs) {
        destination_through_link(root, old_abs, new_rel)
    } else if std::fs::metadata(old_abs).is_ok_and(|meta| meta.is_file()) {
        // `metadata` before the read, and never the read alone: an entry that
        // is not a regular file has no document in it, and opening a FIFO
        // blocks until a writer appears — which would hang the command with
        // no envelope at all, on a move the guards above have already
        // cleared. The scanner admits documents by the same predicate.
        match std::fs::read_to_string(old_abs) {
            Ok(content) => Proposed::Content(content),
            Err(_) => Proposed::Absent,
        }
    } else {
        Proposed::Absent
    };
    match proposed {
        Proposed::Content(content) => Ok(Some(content)),
        // Nothing the rules can be asked about will stand at the destination.
        // Where the graph reaches it, that is itself the answer: the scan
        // admits the path, the build finds bytes it cannot read, and the next
        // `check` reds a `parse_failure` the move introduced. Refuse it here,
        // where refusing still costs nothing. Everywhere else the move is the
        // plain one the guards already cleared, and the gate has no document
        // to judge — said out loud rather than passed over.
        Proposed::Absent => {
            if lands_in_scope(root, config, new_rel)? {
                return Err(CoreError::Config(format!(
                    "rename cannot complete: {new_path:?} is inside the graph's scope, and {} \
                     holds no document the rules can read (it is not text, or it is a symlink \
                     whose target the destination cannot reach) — the move would land bytes the \
                     next build reports as a parse failure",
                    nodex_core::path_guard::forward_string(old_abs)
                ))
                .into());
            }
            skipped.push(format!(
                "{} is not a document the graph can be asked about (it is not text, or it is a \
                 symlink whose target the destination cannot reach), and it moves where the graph \
                 does not reach, so there was nothing for the gate to judge",
                nodex_core::path_guard::forward_string(old_abs)
            ));
            Ok(None)
        }
    }
}

/// Whether the scan would admit a document at `new_rel`.
///
/// Asked with an empty document overlaid, because admission is a question
/// about the *path*: the scope globs, the prune list and the hidden opt-in all
/// read it, and the one rule that reads content — a `conditional_exclude`
/// parent's status — reads a different file's. An empty document is what a
/// bare one would be, so the probe answers what a document arriving there
/// would get.
fn lands_in_scope(root: &Path, config: &Config, new_rel: &Path) -> Result<bool> {
    let scan = nodex_core::builder::scanner::scan_scope_with_overlay(
        root,
        config,
        &[(new_rel.to_path_buf(), Proposed::Content(String::new()))],
    )
    .context("destination scope probe failed")?;
    Ok(scan.paths.iter().any(|p| p == new_rel))
}

/// Which of a rename's two rewrite shapes a plan is, so a refusal reads in
/// the caller's own words rather than a generic one.
enum PlanKind {
    /// A file that references the renamed document.
    Inbound,
    /// The renamed document itself.
    Rewritten,
}

/// Plan the repoint of every reference the move invalidates — inbound links
/// from every other in-scope file, and the moved file's own self- and
/// directory-sensitive references. Detection and rewriting are delegated to
/// `reference_rewrite`, which reuses the build-time resolver's candidate
/// ladder (so it rewrites exactly the links the graph treats as edges) and is
/// code-fence aware (a link inside a code sample is never mutated).
///
/// Nothing is written and nothing has moved: every input is knowable while the
/// tree is still untouched — the referrers from the two scans, the moved
/// document's post-move bytes from the proposal, and each refusal to rewrite
/// (a symlink, an unsplittable fence) from the file itself. So the caller can
/// still refuse the whole rename over what these plans do and do not cover.
/// The one failure that cannot be planned is a write that the filesystem
/// rejects, which it answers only when written to.
///
/// A file that would be read through but never written through, or whose fence
/// does not parse, appends its warning to `skipped` — never an abort, since the
/// rest of the batch is unaffected and the caller judges the whole.
#[allow(clippy::too_many_arguments)]
fn plan_all_references(
    root: &Path,
    config: &Config,
    old_rel: &Path,
    new_rel: &Path,
    destination: &str,
    pre_move_scope: &BTreeSet<String>,
    post_move_scope: &BTreeSet<String>,
    worlds: nodex_core::builder::resolver::Worlds<'_>,
    skipped: &mut Vec<String>,
) -> Result<Vec<(nodex_core::Planned, PlanKind)>> {
    let new_rel_forward = nodex_core::path_guard::forward_string(new_rel);
    let old_rel_forward = nodex_core::path_guard::forward_string(old_rel);
    let mut plans: Vec<(nodex_core::Planned, PlanKind)> = Vec::new();

    // Visit every file that is in scope before OR after the move: a
    // referencing file can be evicted from scope *by* the move (a
    // `conditional_exclude` parent landing in its directory), yet it
    // still holds a real pre-move edge to the renamed file that must be
    // repointed. Iterating only the post-move scan would silently leave
    // that edge dangling. The old path (going away) and the new path (the
    // moved file, handled separately) are excluded.
    let inbound: BTreeSet<&String> = pre_move_scope
        .union(post_move_scope)
        .filter(|p| **p != old_rel_forward && **p != new_rel_forward)
        .collect();

    for rel in inbound {
        let rel_path = Path::new(rel);
        let source_dir = rel_path.parent().unwrap_or_else(|| Path::new(""));
        // An unsplittable fence in a referencing file is a per-file skip: the
        // transform reports "no change" and the warning names the file (which
        // already reds `check` as a `parse_failure`; its stale reference
        // surfaces as an unresolved edge, which the gate then answers for).
        let mut unsplittable: Option<String> = None;
        let mut rebound: Vec<nodex_core::reference_rewrite::Rebound> = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        let rewrite = |content: &str| match nodex_core::reference_rewrite::rewrite_for_move(
            content,
            nodex_core::reference_rewrite::Rewriting::Referrer(source_dir),
            old_rel,
            new_rel,
            worlds,
            &config.parser,
        ) {
            Ok(change) => {
                rebound = change.rebound;
                refused = change.refused;
                Ok(change.content)
            }
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
        // live in the one core seam. The referrers are untouched by the move,
        // so what it reads now is what the rewrite would have read after.
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
        // A reference the rewrite could not repoint stays spelled as it
        // was, and the rename takes the rung it stood on out from under
        // it: the next candidate can be a different document. The graph
        // that leaves is valid, so this is the only place it is said.
        for one in rebound {
            let named = match &one.was {
                Some(was) => format!("named the document `{was}`"),
                None => "named no document".to_string(),
            };
            skipped.push(format!(
                "{rel} reference \"{}\" {named} before the rename and names `{}` after it; it \
                 could not be repointed, so the rename moved what it binds — spell it so it \
                 names the document you mean",
                one.reference, one.now
            ));
        }
        // And one the rename declined to repoint at all. It named the
        // moved document and names nothing now, so the next build reports
        // an unresolved edge — a fact about the project, arriving later.
        // That the rename gave up on it is knowable only here.
        for reference in refused {
            skipped.push(format!(
                "{rel} reference \"{reference}\" was not repointed: no rewrite of it the rename \
                 could accept was available, so it was left as it is — it names nothing now, \
                 and will surface as an unresolved edge"
            ));
        }
    }

    // ─── moved file's own references ───────────────────────────────
    //
    // The moved document is rewritten by the same rule as every other file
    // and in one pass: its references were written from the old directory
    // and must go on naming what they named, spelled from the new one.
    // Asked as two passes over one buffer — repoint the self-references,
    // then rebase the rest — the second read the first's output as the text
    // the author had written, and every claim it made about a self-reference
    // was about a spelling that had never been in the document.
    //
    // The bytes are the proposal's, not the disk's: the destination does not
    // exist yet, and what will be there is what the move carries — an anchored
    // id included, and for a moved symlink whatever it resolves to from the
    // new parent.
    let rebased = match nodex_core::reference_rewrite::rewrite_for_move(
        destination,
        nodex_core::reference_rewrite::Rewriting::Moved,
        old_rel,
        new_rel,
        worlds,
        &config.parser,
    ) {
        Ok(rewritten) => {
            // A reference the rebase could not re-render is left spelled as
            // it was, and a relative one means whatever it means from where
            // it sits — so the move can leave it naming a different
            // document. The graph that results is valid and `check` has
            // nothing to say about it, which is why the move says it here.
            for one in rewritten.rebound {
                let named = match &one.was {
                    Some(was) => format!("named the document `{was}`"),
                    None => "named no document".to_string(),
                };
                skipped.push(format!(
                    "{new_rel_forward} reference \"{}\" {named} where the file stood and names \
                     `{}` where it now stands; it could not be re-rendered, so the move \
                     repointed it — spell it relative to the new directory, or root-relative",
                    one.reference, one.now
                ));
            }
            for reference in rewritten.refused {
                skipped.push(format!(
                    "{new_rel_forward} reference \"{reference}\" was not rebased: no rewrite of \
                     it the move could accept was available, so it was left as it is — it names \
                     nothing from the new directory, and will surface as an unresolved edge"
                ));
            }
            rewritten.content
        }
        Err(_) => {
            skipped.push(format!(
                "{new_rel_forward} carries references that need rebasing but its frontmatter \
                 fence does not parse, so it was not rewritten (it already fails `check` as a \
                 parse_failure) — fix the fence, then rebase its references manually"
            ));
            None
        }
    };
    if let Some(content) = rebased {
        // The write discipline the core seam applies to a path on disk, asked
        // of the entry the move carries: `fs::rename` moves a symlink as the
        // link itself, so writing at the destination would write through it.
        // The move still happens — only the rebasing is refused.
        if nodex_core::path_guard::is_symlink(&root.join(old_rel)) {
            skipped.push(format!(
                "{new_rel_forward} carries references that need rebasing but is or resolves \
                 through a symlink; it was not rewritten (writing through a symlink could escape \
                 the project root) — update it manually"
            ));
        } else {
            plans.push((
                nodex_core::Planned {
                    rel_path: new_rel.to_path_buf(),
                    content,
                },
                PlanKind::Rewritten,
            ));
        }
    }

    Ok(plans)
}

/// The document as it will exist once the move lands, and what the move did
/// to its id.
struct RewrittenDocument {
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
///
/// Only ever asked about a source the scan admitted, and `walk_dir` admits by
/// `is_file()` — so the link resolves to a regular file *today*, and what is
/// being decided is only whether it still will from the destination. Loosening
/// that admission rule would put this function on paths it has never been
/// measured against.
fn destination_through_link(root: &Path, old_abs: &Path, new_rel: &Path) -> Proposed {
    let Ok(target) = std::fs::read_link(old_abs) else {
        return Proposed::Absent;
    };
    let resolved = if target.is_absolute() {
        target
    } else {
        let Some(parent) = root.join(new_rel).parent().map(Path::to_path_buf) else {
            return Proposed::Absent;
        };
        let Some((parent, existing)) = destination_parent(&parent) else {
            return Proposed::Absent;
        };
        match walk_target(&parent, &existing, &target) {
            Some(resolved) => resolved,
            // Nothing the kernel could traverse either, so the destination
            // holds nothing.
            None => return Proposed::Absent,
        }
    };
    // The move takes the source away, so a read that goes through it reports
    // bytes the link is about to stop reaching. It reads fine right now, which
    // is exactly the trap.
    if reads_through_source(&resolved, old_abs) {
        return Proposed::Absent;
    }
    // The scanner admits a document by `is_file()`, so anything else holds
    // none — and `metadata` answers that without opening, which matters
    // because opening a FIFO blocks until a writer appears and would hang the
    // command with no envelope at all.
    if !std::fs::metadata(&resolved).is_ok_and(|meta| meta.is_file()) {
        return Proposed::Absent;
    }
    std::fs::read_to_string(resolved).map_or(Proposed::Absent, Proposed::Content)
}

/// Whether reading `resolved` goes through `source`, the entry the move is
/// about to remove.
///
/// Both are followed all the way down. Landing *on* the source is only the
/// simplest way to depend on it: the target may reach a link that reaches
/// another that reaches the source, and every hop in that chain stops working
/// when the source does. Following to the end catches the whole chain in one
/// comparison, and it settles spelling too — a case- or normalisation-
/// insensitive filesystem hands back its own spelling for both sides, which
/// comparing the written path cannot do.
///
/// Ending at the same file is not the test — two links may reach one document
/// independently, and neither stops working when the other goes. What matters
/// is whether the source is one of the hops.
fn reads_through_source(resolved: &Path, source: &Path) -> bool {
    let Some(source_entry) = entry_id(source) else {
        return false;
    };
    let mut hop = resolved.to_path_buf();
    // Longer than any chain the kernel will follow before it answers ELOOP.
    for _ in 0..40 {
        match entry_id(&hop) {
            Some(id) if id == source_entry => return true,
            Some(_) => {}
            None => return false,
        }
        let Ok(next) = std::fs::read_link(&hop) else {
            // Not a link: the chain ends here, and it never met the source.
            return false;
        };
        hop = if next.is_absolute() {
            next
        } else {
            let Some(parent) = hop.parent().and_then(|p| std::fs::canonicalize(p).ok()) else {
                return false;
            };
            parent.join(next)
        };
    }
    false
}

/// Identity of the directory entry at `path`, or `None` when nothing is there.
///
/// `symlink_metadata` so a link is identified as itself rather than as what it
/// points at — the question is which entry the move removes. On Unix that is
/// the (device, inode) pair, which settles spelling for free: a case- or
/// normalisation-insensitive filesystem gives one identity to the spellings
/// that name one entry, where comparing the written path cannot. Elsewhere the
/// canonical parent plus the written name is the closest stable answer, so on
/// such a filesystem the guard is spelling-exact.
///
/// An inode is a directory entry only while one name refers to it. Where a
/// platform allows a hard link to a symlink, two names share an inode and
/// outlive each other, so the name has to tell them apart — and the written
/// path is all there is to do it with, since stable Rust does not expose the
/// name as the filesystem stores it. That reintroduces the spelling blindness
/// on the aliasing filesystems this pair exists to handle, so it is asked for
/// only where the inode alone cannot answer. Resolving it cannot fail where it
/// is asked: `symlink_metadata` has already answered for `path`, so its parent
/// exists and canonicalizes, and every caller passes an absolute path, so
/// `parent` and `file_name` are both present.
#[cfg(unix)]
fn entry_id(path: &Path) -> Option<(u64, u64, Option<std::path::PathBuf>)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let named = (meta.nlink() > 1)
        .then(|| {
            let parent = std::fs::canonicalize(path.parent()?).ok()?;
            Some(parent.join(path.file_name()?))
        })
        .flatten();
    Some((meta.dev(), meta.ino(), named))
}

#[cfg(not(unix))]
fn entry_id(path: &Path) -> Option<std::path::PathBuf> {
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    Some(parent.join(path.file_name()?))
}

/// The destination's parent directory as the kernel will see it before
/// `fs::create_dir_all` has made it, paired with the canonical ancestor that
/// already exists.
///
/// The existing part is canonicalised, so a symlinked ancestor resolves exactly
/// as the post-move scanner will. The rest is appended verbatim: those are the
/// segments `create_dir_all` is about to make, and it makes real directories.
/// (It will instead fail outright if one of them is occupied by a file or a
/// dangling link, and `rename` dies there without moving anything.)
fn destination_parent(parent: &Path) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = parent.to_path_buf();
    loop {
        if let Ok(real) = std::fs::canonicalize(&probe) {
            let composed = missing
                .iter()
                .rev()
                .fold(real.clone(), |acc, seg| acc.join(seg));
            return Some((composed, real));
        }
        missing.push(probe.file_name()?.to_os_string());
        if !probe.pop() {
            return None;
        }
    }
}

/// Follow `target`'s components from the destination the way the kernel will,
/// or `None` where the kernel would find nothing to follow.
///
/// `..` means the parent of whatever precedes it *resolves to*, not of how it
/// is spelled, so it is taken after canonicalising — a target may traverse a
/// symlinked directory of its own, and folding the spelling would climb out of
/// the wrong place. It also has to be a directory: `x/..` is `ENOTDIR` for the
/// kernel when `x` is a file, while `canonicalize` answers happily and `pop`
/// would step into that file's parent.
///
/// The one place spelling *is* the kernel's answer is the chain
/// `create_dir_all` is about to create — from the canonical `existing` prefix
/// down to `dest_parent`. Those segments cannot exist yet, so `canonicalize`
/// could only fail on them, and they will be real directories by the time
/// anything reads through. A target may leave that chain and re-enter it by
/// name, so membership is a question about where `at` currently is, not a count
/// of how many `..` have been seen.
///
/// Everywhere else a path that does not resolve is the answer, not a reason to
/// fall back to spelling: continuing lexically names whatever unrelated file
/// happens to sit where the spelling points.
fn walk_target(dest_parent: &Path, existing: &Path, target: &Path) -> Option<std::path::PathBuf> {
    let mut at = dest_parent.to_path_buf();
    for part in target.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let to_be_created =
                    at != existing && at.starts_with(existing) && dest_parent.starts_with(&at);
                if !to_be_created {
                    let real = std::fs::canonicalize(&at).ok()?;
                    if !real.is_dir() {
                        return None;
                    }
                    at = real;
                }
                at.pop();
            }
            other => at.push(other),
        }
    }
    Some(at)
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
) -> Result<RewrittenDocument> {
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
            return Ok(RewrittenDocument {
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
        return Ok(RewrittenDocument {
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
            return Ok(RewrittenDocument {
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
        return Ok(RewrittenDocument {
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
    Ok(RewrittenDocument {
        destination: Proposed::Content(anchored.clone()),
        anchor: Some(anchored),
        stability: IdStability::Anchored {
            id: inferred_old_id,
        },
    })
}

// Every test here drives the symlink resolver against the kernel, which is a
// unix question; on other platforms the module has no tests and its imports
// would read as unused.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use nodex_core::builder::scanner::Proposed;
    use std::fs;
    use std::path::PathBuf;

    /// The contract of [`destination_through_link`] is agreement with the
    /// kernel, so that is what is asserted — against the kernel itself rather
    /// than against a model of it.
    ///
    /// Each layout is built twice. One copy is asked; in the other the move is
    /// really performed the way [`run`] performs it, and the result is read
    /// exactly as the pipeline downstream reads it: `walk_dir` admits a
    /// document by `is_file()`, and the build's read phase takes the bytes. A
    /// `Content` answer must match those bytes and an `Absent` answer must
    /// match their absence.
    ///
    /// Generated rather than enumerated because the failures this function has
    /// had were all shapes nobody thought to write down: a `..` crossing a
    /// regular file, a target re-entering a directory the move is about to
    /// create, a chain running back through the source.
    #[test]
    fn the_resolver_answers_what_the_kernel_answers() {
        const SOURCES: &[&str] = &["docs/x", "docs", "other/x", "deep/a/b"];
        const DESTINATIONS: &[&str] = &[
            "docs/dest.md",
            "docs/y/dest.md",
            "docs/y/z/dest.md",
            "docs/sym/dest.md",
            "deep/a/b/dest.md",
            "dest.md",
        ];
        const TARGETS: &[&str] = &[
            "../store/f.md",
            "../../store/f.md",
            "store/f.md",
            "./../store/f.md",
            "../store/",
            "../sym/../store/f.md",
            "../q/../store/f.md",
            "../afile/../store/f.md",
            "../y/../store/f.md",
            "../dangly/../store/f.md",
            "../chain",
            "../adir",
            "../afile",
            "..",
            "q/../store/f.md",
            "sym/../../store/f.md",
        ];

        let mut checked = 0usize;
        for source in SOURCES {
            for destination in DESTINATIONS {
                for target in TARGETS {
                    let asked = TempDir::new().unwrap();
                    let moved = TempDir::new().unwrap();
                    let (Some(a), Some(m)) = (
                        layout(asked.path(), source, target),
                        layout(moved.path(), source, target),
                    ) else {
                        continue;
                    };

                    let verdict =
                        destination_through_link(asked.path(), &a, Path::new(destination));

                    let landed = moved.path().join(destination);
                    if let Some(parent) = landed.parent()
                        && fs::create_dir_all(parent).is_err()
                    {
                        continue;
                    }
                    if fs::rename(&m, &landed).is_err() {
                        continue;
                    }
                    let truth = fs::metadata(&landed)
                        .ok()
                        .filter(std::fs::Metadata::is_file)
                        .and_then(|_| fs::read_to_string(&landed).ok());

                    match (&verdict, &truth) {
                        (Proposed::Content(got), Some(want)) => assert_eq!(
                            got, want,
                            "source {source}, destination {destination}, target {target}"
                        ),
                        (Proposed::Absent, None) => {}
                        _ => panic!(
                            "source {source}, destination {destination}, target {target}: \
                             resolver said {} but the move produced {}",
                            match &verdict {
                                Proposed::Content(_) => "content",
                                Proposed::Absent => "nothing",
                            },
                            if truth.is_some() {
                                "content"
                            } else {
                                "nothing"
                            }
                        ),
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "the sweep must actually run: {checked}");
    }

    /// The furniture every case is resolved against: directories to climb
    /// through, a regular file where a `..` might try to, symlinks that
    /// resolve and one that does not, and a chain. Returns the source link, or
    /// `None` when this combination cannot be built.
    #[cfg(unix)]
    fn layout(root: &Path, source: &str, target: &str) -> Option<PathBuf> {
        use std::os::unix::fs::symlink;
        for dir in [
            "store",
            "q/store",
            "adir",
            "docs/store",
            "docs/q",
            "docs/x",
            "other/store",
            "other/x",
            "deep/a/b/store",
            "deep/a/b",
            "real",
        ] {
            fs::create_dir_all(root.join(dir)).ok()?;
        }
        for (path, body) in [
            ("store/f.md", "F-root"),
            ("docs/store/f.md", "F-docs"),
            ("other/store/f.md", "F-other"),
            ("deep/a/b/store/f.md", "F-deep"),
            ("q/store/f.md", "F-q"),
            ("docs/afile", "not-a-directory"),
            ("real/f.md", "F-real"),
        ] {
            fs::write(root.join(path), body).ok()?;
        }
        symlink("../real", root.join("docs/sym")).ok()?;
        symlink("nowhere-at-all", root.join("docs/dangly")).ok()?;
        symlink("../store/f.md", root.join("docs/link2")).ok()?;
        symlink("link2", root.join("docs/chain")).ok()?;

        let link = root.join(source).join("a.md");
        fs::create_dir_all(link.parent()?).ok()?;
        symlink(target, &link).ok()?;
        Some(link)
    }

    #[cfg(unix)]
    use tempfile::TempDir;
}
