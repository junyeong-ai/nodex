use anyhow::{Context, Result};
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::error::Error as CoreError;

use crate::format::{Envelope, print_json};

pub fn run(root: &Path, old_path: &str, new_path: &str, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;

    let old_abs = root.join(old_path);
    let new_abs = root.join(new_path);

    if !old_abs.exists() {
        return Err(CoreError::Io {
            path: old_abs,
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "source file not found",
            ),
        }
        .into());
    }
    if new_abs.exists() {
        return Err(CoreError::AlreadyExists { path: new_abs }.into());
    }

    // Create target directory if needed
    if let Some(parent) = new_abs.parent() {
        std::fs::create_dir_all(parent)
            .context(format!("failed to create directory {}", parent.display()))?;
    }

    // Move file
    std::fs::rename(&old_abs, &new_abs)
        .context(format!("failed to move {old_path} → {new_path}"))?;

    // Update references in all in-scope documents
    let paths =
        nodex_core::builder::scanner::scan_scope(root, &config).context("scope scan failed")?;

    let mut updated_files = Vec::new();

    for rel_path in &paths {
        let abs_path = root.join(rel_path);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if content.contains(old_path) {
            let new_content = content.replace(old_path, new_path);
            std::fs::write(&abs_path, &new_content).context(format!(
                "failed to update references in {}",
                rel_path.display()
            ))?;
            updated_files.push(rel_path.to_string_lossy().to_string());
        }
    }

    print_json(
        &Envelope::success(serde_json::json!({
            "old_path": old_path,
            "new_path": new_path,
            "references_updated": updated_files,
            "total_updated": updated_files.len(),
        })),
        pretty,
    );

    Ok(())
}
