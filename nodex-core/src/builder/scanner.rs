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
    walk_dir(
        root,
        root,
        &include,
        &exclude,
        config.scope.include_hidden,
        &mut paths,
    )?;

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
    // Track every parent file that triggered exclusion explicitly so
    // any naming convention (`SPEC.md`, `index.md`, …) matched by a
    // `parent_glob` survives while its sub-artefacts drop out.
    let mut parents_to_keep: BTreeSet<PathBuf> = BTreeSet::new();
    let mut excluded_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for rule in rules {
        if rule.condition != "status_terminal" {
            continue;
        }

        let parent_glob = Glob::new(&rule.parent_glob)
            .expect("validated by Config::load")
            .compile_matcher();

        for rel_path in &paths {
            let rel_str = crate::path_guard::forward_string(rel_path);
            if !parent_glob.is_match(&rel_str) {
                continue;
            }

            let abs_path = root.join(rel_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let content = crate::parser::frontmatter::canonicalize(&content);

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
    include_hidden: bool,
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
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Hidden segments (`.draft.md`, `.archive/`, `.claude/`, …)
        // are skipped by default; opt in via `[scope].include_hidden`.
        // Matches ripgrep / ag convention — these are editor state or
        // tooling config in the overwhelming majority of projects.
        if name_str.starts_with('.') && !include_hidden {
            continue;
        }

        if path.is_dir() {
            // Curated tooling exclusions — these directories are
            // unconditionally non-content regardless of the
            // `include_hidden` toggle. `.git` / `.venv` are dot-
            // prefixed AND tooling, so they need both layers: the
            // hidden-skip catches them when `include_hidden` is false,
            // and this list catches them when an operator opts in to
            // `include_hidden = true` for some other dotted path
            // (e.g. `.claude/`).
            if matches!(
                name_str.as_ref(),
                "node_modules" | "__pycache__" | "target" | ".git" | ".venv"
            ) {
                continue;
            }
            walk_dir(base, &path, include, exclude, include_hidden, out)?;
        } else if path.is_file() {
            let rel = path.strip_prefix(base).unwrap_or(&path);
            let rel_str = crate::path_guard::forward_string(rel);

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
    fn conditional_exclude_keeps_arbitrarily_named_parent() {
        // A `parent_glob` that matches `SPEC.md` (or any other naming
        // convention) keeps the parent file while excluding its
        // sub-artefacts.
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
    fn scan_skips_hidden_files_by_default() {
        // Dotted files / directories should NOT surface in the scan
        // unless the user opts in. Mirrors ripgrep / ag — most
        // dot-prefixed entries are editor state or tooling config
        // (`.draft.md`, `.git/`, `.claude/`), never project docs.
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::create_dir_all(dir.path().join(".archive")).unwrap();
        fs::write(docs.join("guide.md"), "# Guide").unwrap();
        fs::write(docs.join(".draft.md"), "# Draft").unwrap();
        fs::write(dir.path().join(".archive/old.md"), "# Old").unwrap();

        let config = Config::default(); // include_hidden defaults to false
        let paths = scan_scope(dir.path(), &config).unwrap();
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(
            names.iter().any(|n| n.ends_with("guide.md")),
            "regular doc must be scanned: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(".draft")),
            "dot-prefixed file must be skipped: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(".archive")),
            "dot-prefixed directory must be skipped: {names:?}"
        );
    }

    #[test]
    fn include_hidden_true_admits_dotted_segments() {
        // Opt-in unblocks dotted-segment scanning for the rare project
        // that genuinely keeps documentation under `.claude/` or
        // similar. Tooling-noise directories — including `.git` and
        // `.venv` — stay excluded; an operator who wants `.claude/`
        // doesn't simultaneously sign up to scan their git internals
        // or python virtual-env metadata.
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".claude/skills")).unwrap();
        fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        fs::create_dir_all(dir.path().join(".git/refs")).unwrap();
        fs::create_dir_all(dir.path().join(".venv/lib")).unwrap();
        fs::write(dir.path().join(".claude/skills/x.md"), "# x").unwrap();
        fs::write(dir.path().join("node_modules/pkg/readme.md"), "# r").unwrap();
        fs::write(dir.path().join(".git/readme.md"), "# git").unwrap();
        fs::write(dir.path().join(".venv/lib/notes.md"), "# venv").unwrap();
        fs::write(dir.path().join("doc.md"), "# doc").unwrap();

        let mut config = Config::default();
        config.scope.include_hidden = true;
        let paths = scan_scope(dir.path(), &config).unwrap();
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(names.iter().any(|n| n.contains(".claude")), "{names:?}");
        assert!(names.iter().any(|n| n.ends_with("doc.md")), "{names:?}");
        for noise in ["node_modules", ".git", ".venv"] {
            assert!(
                !names.iter().any(|n| n.contains(noise)),
                "tooling noise {noise:?} stays excluded even under include_hidden=true: {names:?}"
            );
        }
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
