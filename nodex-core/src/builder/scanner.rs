use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::{ConditionalExclude, Config};
use crate::error::{Error, Result};

/// Scan the filesystem for in-scope document paths.
/// Applies include/exclude globs, then conditional_exclude rules.
pub fn scan_scope(root: &Path, config: &Config) -> Result<Vec<PathBuf>> {
    let include = build_globset(&config.scope.include)?;

    // Always exclude nodex's own output directory. Users would
    // otherwise have to copy-paste `"_index/**"` into every project,
    // and forgetting it silently causes `migrate`, `rename`, and
    // `build` to treat GRAPH.md as a user document.
    let mut exclude_patterns = config.scope.exclude.clone();
    if !config.output.dir.is_empty() {
        exclude_patterns.push(format!("{}/**", config.output.dir.trim_end_matches('/')));
    }
    let exclude = build_globset(&exclude_patterns)?;

    let mut paths = Vec::new();
    walk_dir(root, root, &include, &exclude, &mut paths)?;

    // Apply conditional_exclude rules (e.g., terminal spec sub-artifact filtering)
    if !config.scope.conditional_exclude.is_empty() {
        paths = apply_conditional_excludes(root, paths, &config.scope.conditional_exclude, config)?;
    }

    // Sort for deterministic processing order
    paths.sort();
    Ok(paths)
}

/// For each conditional_exclude rule:
/// 1. Find "parent" files matching parent_glob
/// 2. Check if parent's frontmatter status is terminal
/// 3. If yes, exclude all "child" files matching child_glob EXCEPT the parent itself
fn apply_conditional_excludes(
    root: &Path,
    paths: Vec<PathBuf>,
    rules: &[ConditionalExclude],
    config: &Config,
) -> Result<Vec<PathBuf>> {
    let mut excluded_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for rule in rules {
        if rule.condition != "status_terminal" {
            continue;
        }

        let parent_glob = Glob::new(&rule.parent_glob)
            .map_err(|e| Error::Config(format!("invalid parent_glob {:?}: {e}", rule.parent_glob)))?
            .compile_matcher();

        // Find parent files and check their status
        for rel_path in &paths {
            let rel_str = rel_path.to_string_lossy().replace('\\', "/");
            if !parent_glob.is_match(&rel_str) {
                continue;
            }

            // Read parent file and check frontmatter status
            let abs_path = root.join(rel_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if is_terminal_status(&content, config) {
                // Mark the parent's directory for exclusion
                if let Some(parent_dir) = rel_path.parent() {
                    excluded_dirs.insert(parent_dir.to_path_buf());
                }
            }
        }
    }

    if excluded_dirs.is_empty() {
        return Ok(paths);
    }

    // Filter: keep files NOT in excluded dirs, OR files that are the parent spec.md itself
    let mut filtered = Vec::new();
    for rel_path in paths {
        let in_excluded = excluded_dirs.iter().any(|dir| rel_path.starts_with(dir));
        if in_excluded {
            // Only keep the parent file (spec.md) itself, not sub-files
            let filename = rel_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if filename == "spec.md" {
                filtered.push(rel_path);
            }
            // else: skip this sub-file
        } else {
            filtered.push(rel_path);
        }
    }

    Ok(filtered)
}

/// Quick check if a file's frontmatter has a terminal status.
/// Uses simple YAML parsing (not full frontmatter parser) for performance.
fn is_terminal_status(content: &str, config: &Config) -> bool {
    let (yaml_opt, _) = crate::parser::frontmatter::split_frontmatter(content);
    let Some(yaml) = yaml_opt else {
        return false;
    };

    // Parse just the status field
    let value: std::result::Result<yaml_serde::Value, _> = yaml_serde::from_str(yaml);
    let Ok(value) = value else {
        return false;
    };

    let status = value
        .as_mapping()
        .and_then(|m| m.get(yaml_serde::Value::String("status".to_string())))
        .and_then(|v| v.as_str())
        .unwrap_or("active");

    config.is_terminal(status)
}

fn walk_dir(
    base: &Path,
    dir: &Path,
    include: &GlobSet,
    exclude: &GlobSet,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| Error::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();

        if path.is_dir() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Skip common non-content directories but NOT .claude (which contains rules/skills)
            if name_str == "node_modules"
                || name_str == "__pycache__"
                || name_str == ".venv"
                || name_str == ".git"
                || name_str == "target"
            {
                continue;
            }
            walk_dir(base, &path, include, exclude, out)?;
        } else if path.is_file() {
            let rel = path.strip_prefix(base).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");

            if include.is_match(&rel_str) && !exclude.is_match(&rel_str) {
                out.push(rel.to_path_buf());
            }
        }
    }

    Ok(())
}

fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern)
            .map_err(|e| Error::Config(format!("invalid glob {pattern:?}: {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| Error::Config(format!("globset build error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn scan_includes_matching_files() {
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("guide.md"), "# Guide").unwrap();
        fs::write(docs.join("notes.txt"), "notes").unwrap();
        fs::write(dir.path().join("README.md"), "# Root").unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];

        let paths = scan_scope(dir.path(), &config).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("guide.md")));
        assert!(paths.iter().any(|p| p.ends_with("README.md")));
    }

    #[test]
    fn scan_excludes_patterns() {
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        let index = docs.join("_index");
        fs::create_dir_all(&index).unwrap();
        fs::write(docs.join("guide.md"), "# Guide").unwrap();
        fs::write(index.join("generated.md"), "gen").unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];
        config.scope.exclude = vec!["docs/_index/**".to_string()];

        let paths = scan_scope(dir.path(), &config).unwrap();
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("guide.md"));
    }
}
