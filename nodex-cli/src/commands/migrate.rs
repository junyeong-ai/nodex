use anyhow::{Context, Result};
use clap::Args;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nodex_core::command_result::{MigrateResult, MigrationChange};
use nodex_core::error::Error as CoreError;
use nodex_core::parser::editor::{FrontmatterEditor, Scalar};
use nodex_core::parser::frontmatter;
use nodex_core::parser::identity;

use crate::format::{Envelope, print_json};

/// Args for `nodex migrate`.
#[derive(Args)]
pub struct MigrateArgs {
    /// Actually write files (default: dry-run).
    #[arg(long)]
    pub apply: bool,
}

/// One planned migration: a bare file plus the frontmatter that
/// would be injected. Built up-front so we can detect id collisions
/// across the entire batch before any write occurs — atomic refuse,
/// never partial success.
struct PlannedMigration {
    rel_path: PathBuf,
    abs_path: PathBuf,
    id: String,
    kind: String,
    rendered: String,
}

/// Effective id of an existing (non-bare) file in scope. Held as a
/// separate index so a bare file's inferred id can be checked against
/// every already-pinned id without rebuilding the graph (which would
/// itself fail with `DUPLICATE_ID` if the collision already existed).
struct ExistingId {
    rel_path: PathBuf,
    id: String,
}

pub fn run(root: &Path, args: MigrateArgs, pretty: bool) -> Result<()> {
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
        // Writer-skips / reader-follows: never write through a symlink
        // (the target may escape the project root). Surface the skip
        // only when the symlinked file is bare — i.e. it would have
        // been a migration target — so the operator can add
        // frontmatter manually. Symlinks that already carry
        // frontmatter, or that can't be read (dangling), are not
        // migration targets and stay silent; a read failure must never
        // abort the batch.
        if nodex_core::path_guard::is_symlink(&abs_path) {
            if let Ok(content) = std::fs::read_to_string(&abs_path) {
                let (yaml_opt, _) = frontmatter::split_frontmatter(&content);
                if yaml_opt.is_none() {
                    warnings.push(format!(
                        "{} is bare markdown but also a symlink; it was not migrated \
                         (writing through a symlink could escape the project root) — \
                         add frontmatter to the link target manually",
                        nodex_core::path_guard::forward_string(rel_path)
                    ));
                }
            }
            continue;
        }
        let content = std::fs::read_to_string(&abs_path).map_err(|source| CoreError::Io {
            path: abs_path.clone(),
            source,
        })?;

        let (yaml_opt, body) = frontmatter::split_frontmatter(&content);
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
                    &config,
                );
                planned.push(PlannedMigration {
                    rel_path: rel_path.clone(),
                    abs_path,
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
    let mut changes = Vec::with_capacity(planned.len());
    for p in planned {
        let new_content = format!("---\n{}\n---\n{}", p.rendered, read_body(&p.abs_path)?);
        if apply {
            nodex_core::path_guard::write_atomic_in_root(root, &p.abs_path, &new_content)?;
        }
        changes.push(MigrationChange {
            path: nodex_core::path_guard::forward_string(&p.rel_path),
            id: p.id,
            kind: p.kind,
        });
    }

    let total = changes.len();
    print_json(
        &Envelope::with_warnings(
            MigrateResult {
                changes,
                total,
                applied: apply,
            },
            warnings,
        ),
        pretty,
    );

    Ok(())
}

/// Re-read the body half of a file that we already split during the
/// planning phase. Reading twice keeps the planning struct light and
/// avoids holding the full body in memory across the collision check
/// for projects with thousands of bare files.
fn read_body(abs_path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(abs_path).map_err(|source| CoreError::Io {
        path: abs_path.to_path_buf(),
        source,
    })?;
    let (_, body) = frontmatter::split_frontmatter(&content);
    Ok(body.to_string())
}
