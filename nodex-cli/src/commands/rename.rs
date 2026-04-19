use anyhow::{Context, Result};
use std::path::{Component, Path, PathBuf};

use nodex_core::config::Config;
use nodex_core::error::Error as CoreError;

use crate::format::{Envelope, print_json};

pub fn run(root: &Path, old_path: &str, new_path: &str, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;

    // Refuse `..` / absolute forms in either argument so an AI agent
    // or a typoed invocation cannot move a project file outside root.
    nodex_core::path_guard::reject_traversal(Path::new(old_path))?;
    nodex_core::path_guard::reject_traversal(Path::new(new_path))?;

    let old_abs = root.join(old_path);
    let new_abs = root.join(new_path);

    if !old_abs.exists() {
        return Err(CoreError::Io {
            path: old_abs,
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        }
        .into());
    }
    if new_abs.exists() {
        return Err(CoreError::AlreadyExists { path: new_abs }.into());
    }

    if let Some(parent) = new_abs.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    std::fs::rename(&old_abs, &new_abs).map_err(|source| CoreError::Io {
        path: old_abs.clone(),
        source,
    })?;

    // Update references by walking every in-scope document, parsing
    // its markdown links, and rewriting each whose target resolves to
    // the renamed file. Previously this was a literal `content.replace`
    // against the CLI-passed path, which missed every link written in
    // relative form — the common case for cross-references within a
    // directory. We resolve each link against the linking file's own
    // directory and compare to the normalized renamed path, so both
    // `[x](docs/decisions/first.md)` and `[x](first.md)` (written from
    // `docs/decisions/second.md`) update correctly.
    let paths =
        nodex_core::builder::scanner::scan_scope(root, &config).context("scope scan failed")?;

    let old_norm = normalize(&PathBuf::from(old_path));
    let new_norm = normalize(&PathBuf::from(new_path));

    let link_re = regex::Regex::new(r"\]\(([^)#\s]+)(#[^)]*)?\)").expect("static regex compiles");

    let mut updated_files = Vec::new();

    for rel_path in &paths {
        let abs_path = root.join(rel_path);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let parent_dir = rel_path.parent().unwrap_or_else(|| Path::new(""));
        let mut changed = false;

        let rewritten = link_re.replace_all(&content, |caps: &regex::Captures<'_>| {
            let url = &caps[1];
            let anchor = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let url_as_is = normalize(&PathBuf::from(url));
            let url_relative = normalize(&parent_dir.join(url));

            // Match both authoring styles: a link written root-relative
            // (`docs/b.md`) and one written file-relative (`b.md` from
            // inside `docs/`). Preserve the author's style in the
            // rewritten URL so their intent survives the rename.
            if url_as_is == old_norm {
                changed = true;
                format!("]({}{anchor})", new_path)
            } else if url_relative == old_norm {
                changed = true;
                let new_rel = relative_from(parent_dir, &new_norm);
                format!(
                    "]({}{anchor})",
                    new_rel.to_string_lossy().replace('\\', "/")
                )
            } else {
                caps[0].to_string()
            }
        });

        if changed {
            std::fs::write(&abs_path, rewritten.as_ref()).map_err(|source| CoreError::Io {
                path: abs_path.clone(),
                source,
            })?;
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

/// Resolve `.` and `..` segments without filesystem access.
/// `docs/./a/../b.md` → `docs/b.md`.
fn normalize(p: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in p.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

/// Compute `target` as a path relative to `from_dir` (both
/// project-root-relative). Emits `..` segments where needed and
/// returns just the filename when both paths share a parent.
fn relative_from(from_dir: &Path, target: &Path) -> PathBuf {
    let from_components: Vec<_> = from_dir.components().collect();
    let target_components: Vec<_> = target.components().collect();
    let common = from_components
        .iter()
        .zip(target_components.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_components.len() - common;
    let mut result = PathBuf::new();
    for _ in 0..ups {
        result.push("..");
    }
    for c in &target_components[common..] {
        result.push(c);
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_resolves_dot_dot() {
        assert_eq!(
            normalize(&PathBuf::from("docs/./a/../b.md")),
            PathBuf::from("docs/b.md")
        );
    }

    #[test]
    fn relative_same_dir() {
        assert_eq!(
            relative_from(
                Path::new("docs/decisions"),
                Path::new("docs/decisions/x.md")
            ),
            PathBuf::from("x.md")
        );
    }

    #[test]
    fn relative_walks_up() {
        assert_eq!(
            relative_from(Path::new("docs/a"), Path::new("docs/b/x.md")),
            PathBuf::from("../b/x.md")
        );
    }
}
