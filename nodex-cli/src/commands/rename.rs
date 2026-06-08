use anyhow::{Context, Result};
use clap::Args;
use std::collections::BTreeSet;
use std::path::Path;

use nodex_core::Config;
use nodex_core::command_result::{IdStability, RenameResult};
use nodex_core::error::Error as CoreError;
use nodex_core::parser::editor::{FrontmatterEditor, Scalar};
use nodex_core::parser::frontmatter::{canonicalize, split_frontmatter};
use nodex_core::parser::identity::{infer_id, infer_kind};

use crate::format::{Envelope, print_json};

/// Args for `nodex rename`.
#[derive(Args)]
pub struct RenameArgs {
    /// Source path (relative to root).
    pub old: String,
    /// Target path (relative to root).
    pub new: String,
}

pub fn run(root: &Path, args: RenameArgs, pretty: bool) -> Result<()> {
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

    if source_tracked {
        // Refuse a destination the scan would not admit *post-move*.
        // The probe models the post-move world through the same scope
        // authority the build uses: the source's actual bytes are
        // overlaid at the destination (its status is what a
        // conditional-exclude evaluation reads there), and the source
        // path is overlaid empty — equivalent to absent for every other
        // path's admission, so the still-on-disk source can't act as
        // its own terminal parent and veto its own move. Runs before
        // any mutation (the id anchor below already writes). Probing
        // pre-anchor bytes is decision-equivalent to post-anchor bytes:
        // the anchor only ever adds an `id:` line, and admission reads
        // nothing but `status`.
        let moved_content = std::fs::read_to_string(&old_abs).map_err(|source| CoreError::Io {
            path: old_abs.clone(),
            source,
        })?;
        let post_move_scan = nodex_core::builder::scanner::scan_scope_with_overlay(
            root,
            &config,
            &[
                (Path::new(new_path).to_path_buf(), moved_content),
                (Path::new(old_path).to_path_buf(), String::new()),
            ],
        )
        .context("destination scope probe failed")?;
        if !post_move_scan
            .paths
            .iter()
            .any(|p| p == Path::new(new_path))
        {
            let cause = if post_move_scan
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

    // ─── id-stability anchoring ────────────────────────────────────
    //
    // Before the move, check whether the *inferred* id would change
    // (path-derived ids depend on the file's stem / parent / glob).
    // If yes and the doc doesn't already pin an explicit `id:`, write
    // the current effective id into the doc's frontmatter so the move
    // doesn't silently break every cross-document `related:` /
    // `supersedes:` / `implements:` reference in the rest of the
    // graph. This is the single seam where rename guarantees that the
    // file-system primitive (`fs::rename`) doesn't produce a broken
    // semantic graph. An untracked source has no graph id to keep
    // stable — its move needs no anchor.
    let stability = if source_tracked {
        anchor_id_before_move(
            root,
            &old_abs,
            Path::new(old_path),
            Path::new(new_path),
            &config,
        )?
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
        rewrite_all_references(root, &config, old_path, new_path, &pre_move_scope)?
    } else {
        (Vec::new(), Vec::new())
    };

    let mut warnings: Vec<String> = match &stability {
        IdStability::BareNoFrontmatter { warning } => vec![warning.clone()],
        _ => Vec::new(),
    };
    warnings.extend(skipped);

    let data = RenameResult {
        old_path: nodex_core::path_guard::forward_str(old_path),
        new_path: nodex_core::path_guard::forward_str(new_path),
        total_updated: updated_files.len(),
        references_updated: updated_files,
        id_stability: stability,
    };

    if warnings.is_empty() {
        print_json(&Envelope::success(data), pretty);
    } else {
        print_json(&Envelope::with_warnings(data, warnings), pretty);
    }

    Ok(())
}

/// Repoint every reference to the moved document — inbound links from
/// every other in-scope file, and the moved file's own self- and
/// directory-sensitive references. Detection and rewriting are
/// delegated to `reference_rewrite`, which reuses the build-time
/// resolver's candidate ladder (so it rewrites exactly the links the
/// graph treats as edges) and is code-fence aware (a link inside a code
/// sample is never mutated). Returns `(updated_files, skip_warnings)`.
fn rewrite_all_references(
    root: &Path,
    config: &Config,
    old_path: &str,
    new_path: &str,
    pre_move_scope: &BTreeSet<String>,
) -> Result<(Vec<String>, Vec<String>)> {
    let paths = nodex_core::builder::scanner::scan_scope(root, config)
        .context("scope scan failed")?
        .paths;

    // Immutability lock probe: the baseline snapshot a `check` against
    // `immutable_baseline` would diff against. Outside a git work tree
    // (or with no baseline) those rules are inert for `check`, so the
    // probe is inert too (returns `None` for every path).
    let baseline_in_git =
        config.rules.immutable_baseline.is_some() && super::git_worktree::is_work_tree(root);
    let baseline_content = |p: &Path| -> Option<String> {
        if !baseline_in_git {
            return None;
        }
        super::git_worktree::ref_file_content(
            root,
            config
                .rules
                .immutable_baseline
                .as_deref()
                .expect("guarded by baseline_in_git"),
            p,
        )
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
        let rewrite = |content: &str| {
            Ok(nodex_core::reference_rewrite::rewrite_references(
                content,
                source_dir,
                old_rel,
                new_rel,
                pre_move_scope,
                &config.parser,
            ))
        };
        // Writer-skips for immutability locks, mirroring the symlink
        // discipline: a rewrite nodex's own `check` would flag as a
        // body_immutable violation is not performed — frozen history
        // keeps its original spelling, and the stale reference surfaces
        // here as a warning and on the next build as an unresolved edge.
        if let Ok(current) = std::fs::read_to_string(root.join(rel_path))
            && let Some(after) = rewrite(&current)?
            && let Some(lock) = nodex_core::rules::body_immutable::rewrite_lock_reason(
                &after,
                rel_path,
                config,
                &baseline_content,
                false,
            )
        {
            skipped.push(format!(
                "{rel} references the renamed file but its body is locked ({lock}); it was \
                 not rewritten — the stale reference will surface as an unresolved edge"
            ));
            continue;
        }
        // The reader-follows / writer-skips symlink discipline and the
        // atomic write live in the one core mutation seam.
        match nodex_core::mutate::apply_to_file(root, rel_path, rewrite, || {
            format!(
                "{} references the renamed file but is or resolves through a symlink; it was \
                 not rewritten (writing through a symlink could escape the project root) — \
                 update it manually",
                nodex_core::path_guard::forward_string(rel_path)
            )
        })? {
            nodex_core::mutate::FileOutcome::Rewritten => {
                updated_files.push(nodex_core::path_guard::forward_string(rel_path));
            }
            nodex_core::mutate::FileOutcome::Skipped(warning) => skipped.push(warning),
            nodex_core::mutate::FileOutcome::Unchanged => {}
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
    let rewrite_moved = |content: &str| -> nodex_core::Result<Option<String>> {
        // Pass 1 repoints the moved file's self-references (links that
        // bound `old_path`) — resolved against the pre-move scope, same
        // as the inbound loop. Pass 2 rebases its outbound links to
        // other files against the post-move world.
        let pass1 = nodex_core::reference_rewrite::rewrite_references(
            content,
            old_dir,
            old_rel,
            new_rel,
            pre_move_scope,
            &config.parser,
        );
        let base = pass1.as_deref().unwrap_or(content);
        Ok(nodex_core::reference_rewrite::rewrite_moved_references(
            base,
            old_dir,
            new_dir,
            &post_move_scope,
            &config.parser,
        )
        .or(pass1))
    };
    // The moved document's own body is under the same lock discipline —
    // a frozen record keeps its original link spellings.
    let moved_locked = if let Ok(current) = std::fs::read_to_string(root.join(new_rel)) {
        if let Some(after) = rewrite_moved(&current)? {
            // The lock question is about the *before* node — the diff
            // tracks it by id across the move, so its baseline snapshot
            // and its before-kind both live at the old path. Reading the
            // baseline from `old_rel` keeps a cross-kind move from
            // slipping past a kind-scoped lock via the new path.
            nodex_core::rules::body_immutable::rewrite_lock_reason(
                &after,
                old_rel,
                config,
                &|_| baseline_content(old_rel),
                false,
            )
        } else {
            None
        }
    } else {
        None
    };
    if let Some(lock) = moved_locked {
        skipped.push(format!(
            "{new_rel_forward} carries references that need rebasing but its body is locked \
             ({lock}); it was not rewritten — its stale self-references will surface as \
             unresolved edges"
        ));
        return Ok((updated_files, skipped));
    }

    // `fs::rename` moved the file (or the symlink itself); the same one
    // core seam guards the write.
    match nodex_core::mutate::apply_to_file(root, new_rel, rewrite_moved, || {
        format!(
            "{new_rel_forward} carries references that need rebasing but is or resolves \
             through a symlink; it was not rewritten (writing through a symlink could \
             escape the project root) — update it manually",
        )
    })? {
        nodex_core::mutate::FileOutcome::Rewritten => updated_files.push(new_rel_forward.clone()),
        nodex_core::mutate::FileOutcome::Skipped(warning) => skipped.push(warning),
        nodex_core::mutate::FileOutcome::Unchanged => {}
    }

    Ok((updated_files, skipped))
}

/// Read the doc at `old_abs`, compare its effective id against the id
/// it *would* infer at `new_rel`, and — if a path-derived id would
/// change — anchor the previous id into the doc's frontmatter before
/// the move. Returns the [`IdStability`] outcome for the envelope.
///
/// This is the only mutation point for stability anchoring; it runs
/// before `fs::rename` so a write failure aborts cleanly without
/// leaving the move half-done.
fn anchor_id_before_move(
    root: &Path,
    old_abs: &Path,
    old_rel: &Path,
    new_rel: &Path,
    config: &Config,
) -> Result<IdStability> {
    let raw = std::fs::read_to_string(old_abs).map_err(|source| CoreError::Io {
        path: old_abs.to_path_buf(),
        source,
    })?;
    // Route through the same canonicalisation (BOM strip, CRLF/CR → LF)
    // every parser entry uses, so frontmatter delimited with Windows
    // line endings splits identically here and in the build — otherwise
    // a CRLF document with an explicit `id:` would be mis-read as bare
    // and its id silently left un-anchored.
    let content = canonicalize(&raw);
    let (yaml_opt, body) = split_frontmatter(&content);

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
            return Ok(IdStability::BareNoFrontmatter {
                warning: format!(
                    "renamed file has no frontmatter; its inferred id changed from \
                     {inferred_old_id:?} to {inferred_new_id:?}. Other documents \
                     referencing {inferred_old_id:?} via `related` / `supersedes` / \
                     `implements` / `superseded_by` will become stale. Add an explicit \
                     `id:` frontmatter to the file (or run `nodex migrate --apply` to \
                     generate one) and re-run rename to re-anchor."
                ),
            });
        }
        return Ok(IdStability::Unchanged);
    };

    let mut editor = FrontmatterEditor::parse(yaml, old_abs)?;
    // Explicit id first: an already-pinned id makes the move path-only
    // by construction — no anchoring, so a broken `kind:` is irrelevant.
    match editor.scalar("id") {
        Scalar::Value(v) if !v.is_empty() => {
            return Ok(IdStability::AlreadyAnchored);
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

    let effective_old_id = inferred_old_id.clone();

    if inferred_old_id == inferred_new_id {
        return Ok(IdStability::Unchanged);
    }

    editor.set("id", &effective_old_id);
    let new_frontmatter = editor.render();
    // Rewrite the source file in place so the post-move file already
    // carries the anchored id. Using the project-wide atomic-write
    // primitive keeps the failure mode binary (old content intact or
    // new content written — never half-written).
    let rewritten = format!("---\n{new_frontmatter}---\n{body}");
    nodex_core::path_guard::write_atomic_in_root(root, old_abs, &rewritten)?;

    Ok(IdStability::Anchored {
        id: effective_old_id,
    })
}
