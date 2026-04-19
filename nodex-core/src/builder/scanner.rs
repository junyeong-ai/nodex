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
/// 1. Find "parent" files matching `parent_glob`
/// 2. Check if parent's frontmatter status is terminal
/// 3. If yes, exclude every other file in the parent's directory
///    (children / sub-artifacts); keep the parent file itself
fn apply_conditional_excludes(
    root: &Path,
    paths: Vec<PathBuf>,
    rules: &[ConditionalExclude],
    config: &Config,
) -> Result<Vec<PathBuf>> {
    // Track the exact parent files that triggered exclusion (not just
    // their directories). The previous implementation kept only files
    // literally named `spec.md`, which silently dropped the parent
    // when a project used any other naming convention — e.g. a
    // `parent_glob = "specs/**/*.md"` matching `specs/auth/SPEC.md`.
    let mut parents_to_keep: BTreeSet<PathBuf> = BTreeSet::new();
    let mut excluded_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for rule in rules {
        if rule.condition != "status_terminal" {
            continue;
        }

        let parent_glob = Glob::new(&rule.parent_glob)
            .map_err(|e| Error::Config(format!("invalid parent_glob {:?}: {e}", rule.parent_glob)))?
            .compile_matcher();

        for rel_path in &paths {
            let rel_str = rel_path.to_string_lossy().replace('\\', "/");
            if !parent_glob.is_match(&rel_str) {
                continue;
            }

            let abs_path = root.join(rel_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if is_terminal_status(&content, config) {
                parents_to_keep.insert(rel_path.clone());
                if let Some(parent_dir) = rel_path.parent() {
                    excluded_dirs.insert(parent_dir.to_path_buf());
                }
            }
        }
    }

    if excluded_dirs.is_empty() {
        return Ok(paths);
    }

    let mut filtered = Vec::new();
    for rel_path in paths {
        let in_excluded = excluded_dirs.iter().any(|dir| rel_path.starts_with(dir));
        if in_excluded {
            if parents_to_keep.contains(&rel_path) {
                filtered.push(rel_path);
            }
            // else: sub-artifact of a terminal parent — drop
        } else {
            filtered.push(rel_path);
        }
    }

    Ok(filtered)
}

/// Quick check if a file's frontmatter declares a terminal status.
/// Uses a lightweight YAML parse (not the full frontmatter parser) on
/// the hot scan path. A missing status field, unparseable YAML, or an
/// absent frontmatter block is treated as "not terminal" — those
/// documents surface as schema violations in `check`, not as silent
/// excludes from `build`.
fn is_terminal_status(content: &str, config: &Config) -> bool {
    let (Some(yaml), _) = crate::parser::frontmatter::split_frontmatter(content) else {
        return false;
    };
    let Ok(value) = yaml_serde::from_str::<yaml_serde::Value>(yaml) else {
        return false;
    };
    value
        .as_mapping()
        .and_then(|m| m.get(yaml_serde::Value::String("status".to_string())))
        .and_then(|v| v.as_str())
        .map(|s| config.is_terminal(s))
        .unwrap_or(false)
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
    fn conditional_exclude_keeps_non_spec_named_parent() {
        // Regression: the filter used to hard-code `filename == "spec.md"`
        // as the sole file to keep from an excluded directory. Any
        // project whose `parent_glob` matched a different name (here
        // `SPEC.md`) lost its parent too. The fix tracks each
        // matched parent path explicitly.
        let dir = TempDir::new().unwrap();
        let auth = dir.path().join("specs/auth");
        fs::create_dir_all(&auth).unwrap();
        fs::write(
            auth.join("SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("tasks.md"),
            "---\nid: spec-auth-tasks\ntitle: Tasks\nkind: spec\nstatus: draft\n---\n",
        )
        .unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["specs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/**/SPEC.md".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let paths = scan_scope(dir.path(), &config).unwrap();
        assert_eq!(
            paths.len(),
            1,
            "SPEC.md parent should be kept, sub-artifacts excluded"
        );
        assert!(paths[0].ends_with("SPEC.md"));
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
