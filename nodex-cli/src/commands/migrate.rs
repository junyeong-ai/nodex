use anyhow::{Context, Result};
use clap::Args;
use std::path::Path;

use nodex_core::error::Error as CoreError;
use nodex_core::parser::frontmatter;

use crate::format::{Envelope, print_json};

/// Args for `nodex migrate`.
#[derive(Args)]
pub struct MigrateArgs {
    /// Actually write files (default: dry-run).
    #[arg(long)]
    pub apply: bool,
}

pub fn run(root: &Path, args: MigrateArgs, pretty: bool) -> Result<()> {
    let apply = args.apply;
    let config = nodex_core::load_project(root)?;

    let paths =
        nodex_core::builder::scanner::scan_scope(root, &config).context("scope scan failed")?;

    let mut changes = Vec::new();

    for rel_path in &paths {
        let abs_path = root.join(rel_path);
        // Refuse to operate on symlinks. A symlink whose target sits
        // outside the project root would otherwise let `migrate
        // --apply` write arbitrary frontmatter into external files.
        if nodex_core::path_guard::is_symlink(&abs_path) {
            continue;
        }
        let content = std::fs::read_to_string(&abs_path).map_err(|source| CoreError::Io {
            path: abs_path.clone(),
            source,
        })?;

        let (yaml_opt, body) = frontmatter::split_frontmatter(&content);

        if yaml_opt.is_some() {
            continue; // Already has frontmatter
        }

        // Infer fields. Title shares `parser::frontmatter::extract_h1`
        // so setext / inline-code / code-block heuristics never drift
        // between migrate and the regular document parser.
        let kind = nodex_core::parser::identity::infer_kind(rel_path, &config);
        let id = nodex_core::parser::identity::infer_id(rel_path, &kind, &config);
        let title = nodex_core::parser::frontmatter::extract_h1(body, rel_path);

        // Shared with scaffold: emits id/title/kind/status + every
        // required and cross_field-implied field with typed defaults,
        // so the migrated doc passes the project's own `check`
        // immediately rather than surfacing new violations.
        let frontmatter_body =
            nodex_core::scaffold::render_default_frontmatter(&id, &title, kind.as_str(), &config);
        let new_content = format!("---\n{frontmatter_body}\n---\n{body}");

        changes.push(MigrationChange {
            path: nodex_core::path_guard::forward_string(rel_path),
            id,
            kind: kind.to_string(),
        });

        if apply {
            nodex_core::path_guard::write_atomic(&abs_path, &new_content)?;
        }
    }

    #[derive(serde::Serialize)]
    struct MigrateOutput {
        changes: Vec<MigrationChange>,
        total: usize,
        applied: bool,
    }

    let total = changes.len();
    print_json(
        &Envelope::success(MigrateOutput {
            changes,
            total,
            applied: apply,
        }),
        pretty,
    );

    Ok(())
}

#[derive(serde::Serialize)]
struct MigrationChange {
    path: String,
    id: String,
    kind: String,
}
