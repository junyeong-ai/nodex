use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nodex_core::command_result::{MigrateResult, MigrationChange};
use nodex_core::error::Error as CoreError;
use nodex_core::parser::editor::{FrontmatterEditor, Scalar};
use nodex_core::parser::frontmatter;
use nodex_core::parser::identity;

use crate::format::emit_write;

/// Args for `nodex migrate`.
#[derive(Args)]
pub struct MigrateArgs {
    /// Actually write files (default: dry-run).
    #[arg(long)]
    pub apply: bool,
}

/// `FileSkipped` note for a bare markdown file that is (or resolves
/// through) a symlink. One source for the plan-phase pre-check and the
/// apply-phase `SkipReason::Symlink` arm, so the two phases can never
/// word the same skip differently.
fn symlink_skip_note(rel_path: &Path) -> String {
    format!(
        "{} is bare markdown but is or resolves through a symlink; it was not migrated \
         (writing through a symlink could escape the project root) — add frontmatter to \
         the link target manually",
        nodex_core::path_guard::forward_string(rel_path)
    )
}

/// `FileSkipped` note for an opened-but-unclosed frontmatter fence. One
/// source for the two plan-phase sites that detect it.
fn unclosed_fence_skip_note(rel_path: &Path) -> String {
    format!(
        "{} has an opened but unclosed frontmatter fence; not migrated — close the fence first",
        nodex_core::path_guard::forward_string(rel_path)
    )
}

/// One planned migration: a bare file plus the frontmatter that
/// would be injected. Built up-front so we can detect id collisions
/// across the entire batch before any write occurs — atomic refuse,
/// never partial success.
struct PlannedMigration {
    rel_path: PathBuf,
    id: String,
    kind: String,
    rendered: String,
}

/// What the apply phase decided for the bytes it just read — the file
/// is re-classified from those exact bytes, so frontmatter that
/// appeared between plan and apply is detected and skipped rather than
/// buried under a second injected block.
enum ApplyDecision {
    /// Still bare: the document composed from the planned frontmatter
    /// and the body just read.
    Inject(String),
    /// No longer a migration target; the reason rides the warnings array.
    Skip(&'static str),
}

/// Re-classify bareness from `raw` (the bytes the mutation seam just
/// read) and compose the injected document. Canonicalizes (BOM strip +
/// CRLF→LF) before splitting — the same pre-pass the build parser runs
/// — so a BOM / CRLF file is never misread as bare.
fn classify_for_injection(raw: &str, rendered: &str) -> ApplyDecision {
    let content = frontmatter::canonicalize(raw);
    match frontmatter::split_frontmatter(&content) {
        Ok((None, body)) => ApplyDecision::Inject(format!("---\n{rendered}\n---\n{body}")),
        Ok((Some(_), _)) => ApplyDecision::Skip(
            "already has frontmatter (it appeared between plan and apply); not migrated — \
             the existing block wins",
        ),
        Err(_) => ApplyDecision::Skip(
            "has an opened but unclosed frontmatter fence; not migrated — close the fence first",
        ),
    }
}

/// Effective id of an existing (non-bare) file in scope. Held as a
/// separate index so a bare file's inferred id can be checked against
/// every already-pinned id without rebuilding the graph (which would
/// itself fail with `DUPLICATE_ID` if the collision already existed).
struct ExistingId {
    rel_path: PathBuf,
    id: String,
}

pub fn run(root: &Path, args: MigrateArgs, pretty: bool, today: NaiveDate) -> Result<()> {
    let apply = args.apply;
    let config = nodex_core::load_project(root)?;
    // The version pin gates the actual write. A dry-run (the default)
    // only plans, so it carries the advisory like any read; `--apply`
    // is refused on an incompatible binary.
    let mut warnings = Vec::new();
    if apply {
        nodex_core::ensure_binary_compatible(&config)?;
    } else {
        warnings.extend(nodex_core::binary_compat_warning(&config));
    }

    let paths = nodex_core::builder::scanner::scan_scope(root, &config)
        .context("scope scan failed")?
        .paths;

    // ─── Phase 1 — plan ────────────────────────────────────────────
    //
    // Walk every in-scope file, classify bare vs. has-frontmatter,
    // and record the effective id (explicit if pinned, inferred from
    // path otherwise). No file is written in this phase.
    let mut planned: Vec<PlannedMigration> = Vec::new();
    let mut existing: Vec<ExistingId> = Vec::new();

    for rel_path in &paths {
        let abs_path = root.join(rel_path);
        // Writer-skips / reader-follows: never write through a symlink —
        // the file itself, or a symlinked ancestor directory the scan
        // legitimately followed — since the target may escape the
        // project root. Surface the skip only when the file is bare,
        // i.e. it would have been a migration target, so the operator
        // can add frontmatter manually. Paths that already carry
        // frontmatter, or that can't be read (dangling), are not
        // migration targets and stay silent; a read failure must never
        // abort the batch.
        if nodex_core::path_guard::is_symlink(&abs_path)
            || nodex_core::path_guard::reject_outside_root(root, &abs_path).is_err()
        {
            if let Ok(raw) = std::fs::read_to_string(&abs_path) {
                let content = frontmatter::canonicalize(&raw);
                match frontmatter::split_frontmatter(&content) {
                    Ok((None, _)) => {
                        warnings.push(nodex_core::Warning::new(
                            nodex_core::WarningCode::FileSkipped,
                            symlink_skip_note(rel_path),
                        ));
                    }
                    Ok((Some(_), _)) => {}
                    Err(_) => {
                        warnings.push(nodex_core::Warning::new(
                            nodex_core::WarningCode::FileSkipped,
                            unclosed_fence_skip_note(rel_path),
                        ));
                    }
                }
            }
            continue;
        }
        // A read error on a regular in-scope file skips that file with a
        // warning instead of aborting the batch — the same degradation
        // the build's read stage applies, so one unreadable file cannot
        // strand an otherwise-valid migration.
        let raw = match std::fs::read_to_string(&abs_path) {
            Ok(raw) => raw,
            Err(e) => {
                warnings.push(nodex_core::Warning::new(
                    nodex_core::WarningCode::FileSkipped,
                    format!(
                        "could not read in-scope file {}: {e}; skipped",
                        nodex_core::path_guard::forward_string(rel_path)
                    ),
                ));
                continue;
            }
        };
        // Canonicalize (BOM strip + CRLF→LF) before splitting — the same
        // pre-pass the build parser runs — so a CRLF / BOM file authored
        // outside nodex is detected as already having frontmatter rather
        // than misread as bare and given a duplicate injected block. An
        // opened-but-unclosed fence is neither bare nor parseable —
        // injecting frontmatter would bury the malformed block in the
        // body, so the file is skipped with a per-file warning (it also
        // surfaces as a `parse_failure` violation in `check`).
        let content = frontmatter::canonicalize(&raw);
        let (yaml_opt, body) = match frontmatter::split_frontmatter(&content) {
            Ok(split) => split,
            Err(_) => {
                warnings.push(nodex_core::Warning::new(
                    nodex_core::WarningCode::FileSkipped,
                    unclosed_fence_skip_note(rel_path),
                ));
                continue;
            }
        };
        let kind = identity::infer_kind(rel_path, &config.identity);
        let inferred_id = identity::infer_id(rel_path, &kind, &config.identity);

        match yaml_opt {
            Some(yaml) => {
                // Has frontmatter — never written by migrate. Record
                // its effective id so a bare file in the same batch
                // can be checked against it.
                let id = match FrontmatterEditor::parse(yaml, &abs_path).ok().map(|e| {
                    match e.scalar("id") {
                        Scalar::Value(v) if !v.is_empty() => Some(v.to_string()),
                        _ => None,
                    }
                }) {
                    Some(Some(explicit)) => explicit,
                    _ => inferred_id,
                };
                existing.push(ExistingId {
                    rel_path: rel_path.clone(),
                    id,
                });
            }
            None => {
                // Bare — candidate for injection. Render now so the
                // planning output matches what would be written.
                let title = frontmatter::extract_h1(body, rel_path);
                let rendered = nodex_core::scaffold::render_default_frontmatter(
                    &inferred_id,
                    &title,
                    kind.as_str(),
                    &[],
                    &config,
                    today,
                );
                planned.push(PlannedMigration {
                    rel_path: rel_path.clone(),
                    id: inferred_id,
                    kind: kind.to_string(),
                    rendered,
                });
            }
        }
    }

    // ─── Phase 2 — collision validation ────────────────────────────
    //
    // Two failure modes to reject *before* writing:
    //   (a) two planned bare files would receive the same id;
    //   (b) a planned id matches the effective id of a file that is
    //       already in scope (would surface as `DUPLICATE_ID` on the
    //       next build).
    //
    // Either case is reported as a `DuplicateId` error with the
    // colliding paths in the message, and no file is touched — the
    // self-consistency invariant says nodex must never write a
    // graph state its own `check` would reject.
    let mut by_id: BTreeMap<&str, Vec<&Path>> = BTreeMap::new();
    for p in &planned {
        by_id.entry(p.id.as_str()).or_default().push(&p.rel_path);
    }
    for (id, paths) in &by_id {
        if paths.len() > 1 {
            return Err(CoreError::DuplicateId {
                id: (*id).to_string(),
                first: paths[0].to_path_buf(),
                second: paths[1].to_path_buf(),
            }
            .into());
        }
    }

    let existing_by_id: BTreeMap<&str, &Path> = existing
        .iter()
        .map(|e| (e.id.as_str(), e.rel_path.as_path()))
        .collect();
    for p in &planned {
        if let Some(existing_path) = existing_by_id.get(p.id.as_str()) {
            return Err(CoreError::DuplicateId {
                id: p.id.clone(),
                first: existing_path.to_path_buf(),
                second: p.rel_path.clone(),
            }
            .into());
        }
    }

    // ─── Phase 3 — apply ───────────────────────────────────────────
    //
    // Every write routes through the one core mutation seam: the transform
    // re-classifies bareness from the bytes the seam just read (closing the
    // plan/apply window — frontmatter that appeared in between is skipped,
    // never buried under a second injected block), and the seam owns the
    // symlink/containment backstop and the read. A read error on one planned
    // file becomes a skip warning, never a batch abort. A dry-run reports the
    // plan without touching the seam.
    //
    // The immutability lock is asked once for the whole batch, after every
    // injection is planned: an injected block changes every field in it, so
    // whether that is refused depends on what the project looks like with all
    // of them landed. Under `--apply`, `changes` lists only files actually
    // written; every skip rides the warnings array.
    let probe = super::git_worktree::write_baseline(root, &config)?;
    let mut changes = Vec::with_capacity(planned.len());
    let mut pending: Vec<(nodex_core::Planned, String, String)> = Vec::new();
    for p in planned {
        if !apply {
            changes.push(MigrationChange {
                path: nodex_core::path_guard::forward_string(&p.rel_path),
                id: p.id,
                kind: p.kind,
            });
            continue;
        }
        let mut skip_note: Option<&'static str> = None;
        let outcome = nodex_core::mutate::plan_file(
            root,
            &p.rel_path,
            |raw| match classify_for_injection(raw, &p.rendered) {
                ApplyDecision::Inject(content) => Ok(Some(content)),
                ApplyDecision::Skip(reason) => {
                    skip_note = Some(reason);
                    Ok(None)
                }
            },
            || symlink_skip_note(&p.rel_path),
        )?;
        match outcome {
            nodex_core::PlanOutcome::Planned(plan) => pending.push((plan, p.id, p.kind)),
            nodex_core::PlanOutcome::Skipped(warning) => warnings.push(nodex_core::Warning::new(
                nodex_core::WarningCode::FileSkipped,
                warning,
            )),
            nodex_core::PlanOutcome::Unchanged => {
                if let Some(reason) = skip_note {
                    warnings.push(nodex_core::Warning::new(
                        nodex_core::WarningCode::FileSkipped,
                        format!(
                            "{} {reason}",
                            nodex_core::path_guard::forward_string(&p.rel_path)
                        ),
                    ));
                }
            }
        }
    }

    let plans: Vec<nodex_core::Planned> = pending.iter().map(|(plan, ..)| plan.clone()).collect();
    let refusals = probe.refusals(root, &config, &plans, today)?;
    for (plan, id, kind) in &pending {
        let shown = nodex_core::path_guard::forward_string(&plan.rel_path);
        match refusals.refusing(&plan.rel_path) {
            Some(lock) => warnings.push(nodex_core::Warning::new(
                nodex_core::WarningCode::FileSkipped,
                format!("{shown} is locked ({lock}); it was not migrated"),
            )),
            None => {
                nodex_core::mutate::write_plan(root, plan)?;
                changes.push(MigrationChange {
                    path: shown,
                    id: id.clone(),
                    kind: kind.clone(),
                });
            }
        }
    }

    let total = changes.len();
    emit_write(
        MigrateResult {
            changes,
            total,
            applied: apply,
        },
        warnings,
        &probe,
        pretty,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_injects_into_a_still_bare_file() {
        let rendered = "id: \"doc\"\ntitle: \"Doc\"";
        match classify_for_injection("# Doc\nBody.\n", rendered) {
            ApplyDecision::Inject(content) => {
                assert_eq!(
                    content,
                    "---\nid: \"doc\"\ntitle: \"Doc\"\n---\n# Doc\nBody.\n"
                );
            }
            ApplyDecision::Skip(reason) => panic!("bare file must inject, skipped: {reason}"),
        }
    }

    #[test]
    fn classify_skips_when_frontmatter_appeared_between_plan_and_apply() {
        // The plan/apply window: a file that was bare at plan time but
        // carries frontmatter by apply time must be skipped, never
        // given a second injected block on top of the existing one.
        let rendered = "id: \"doc\"\ntitle: \"Doc\"";
        let raced = "---\nid: someone-else\n---\n# Doc\n";
        match classify_for_injection(raced, rendered) {
            ApplyDecision::Skip(reason) => assert!(reason.contains("already has frontmatter")),
            ApplyDecision::Inject(content) => {
                panic!("must not double-inject; would have written:\n{content}")
            }
        }
    }

    #[test]
    fn classify_skips_a_fence_that_appeared_between_plan_and_apply() {
        let rendered = "id: \"doc\"";
        match classify_for_injection("---\nid: broken\n# never closed\n", rendered) {
            ApplyDecision::Skip(reason) => assert!(reason.contains("unclosed")),
            ApplyDecision::Inject(_) => panic!("an unclosed fence is not a bare file"),
        }
    }

    #[test]
    fn classify_treats_bom_crlf_frontmatter_as_present() {
        // Canonicalization runs before the split, so a BOM/CRLF file
        // authored outside nodex is never misread as bare.
        let raced = "\u{FEFF}---\r\nid: x\r\n---\r\nBody.\r\n";
        match classify_for_injection(raced, "id: \"doc\"") {
            ApplyDecision::Skip(reason) => assert!(reason.contains("already has frontmatter")),
            ApplyDecision::Inject(_) => panic!("BOM/CRLF frontmatter must be detected"),
        }
    }
}
