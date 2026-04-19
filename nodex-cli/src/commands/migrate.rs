use anyhow::{Context, Result};
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::error::Error as CoreError;
use nodex_core::parser::frontmatter;

use crate::format::{Envelope, print_json};

pub fn run(root: &Path, apply: bool, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;

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

        // Infer fields
        let kind = nodex_core::parser::identity::infer_kind(rel_path, &config);
        let id = nodex_core::parser::identity::infer_id(rel_path, &kind, &config);

        // Extract title from H1
        let title = body
            .lines()
            .find(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches("# ").trim().to_string())
            .unwrap_or_else(|| {
                rel_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled")
                    .to_string()
            });

        // Build YAML with proper quoting for safety
        let new_content = format!(
            "---\nid: {}\ntitle: {}\nkind: {}\nstatus: active\n---\n{body}",
            yaml_quote(&id),
            yaml_quote(&title),
            yaml_quote(&kind.to_string()),
        );

        changes.push(MigrationChange {
            path: rel_path.to_string_lossy().to_string(),
            id,
            kind: kind.to_string(),
        });

        if apply {
            std::fs::write(&abs_path, &new_content).map_err(|source| CoreError::Io {
                path: abs_path.clone(),
                source,
            })?;
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

/// Quote a YAML scalar value if it contains special characters.
fn yaml_quote(s: &str) -> String {
    let needs_quoting = s.is_empty()
        || s.contains(':')
        || s.contains('#')
        || s.contains('"')
        || s.contains('\'')
        || s.contains('\n')
        || s.starts_with(' ')
        || s.ends_with(' ')
        || s.starts_with('{')
        || s.starts_with('[')
        || s.starts_with('*')
        || s.starts_with('&')
        || s == "true"
        || s == "false"
        || s == "null"
        || s.parse::<f64>().is_ok();

    if needs_quoting {
        // Use single quotes, escape embedded single quotes by doubling
        format!("'{}'", s.replace('\'', "''"))
    } else {
        s.to_string()
    }
}
