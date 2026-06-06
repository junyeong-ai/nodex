use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Directories that are never project content: version-control,
/// dependency, and build-artifact trees. Pruned unconditionally during
/// the walk — descending them costs traversal time and can never yield
/// a document, regardless of any include pattern. This is not scope
/// policy (which lives entirely in include/exclude globs); it is a
/// physical fact about these directories.
const NON_CONTENT_DIRS: &[&str] = &["node_modules", "__pycache__", "target", ".git", ".venv"];

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

    // Hidden paths (`.draft.md`, `.archive/`, `.claude/`, …) are skipped
    // by default — the ripgrep / fd / git convention, since dot-prefixed
    // entries are overwhelmingly editor state or tooling config. An
    // include pattern overrides that default for exactly the segments it
    // names literally: `.claude/routines/*.md` opts `.claude` in, while a
    // greedy `**/*.md` does not. The include pattern is the opt-in; there
    // is no separate flag to keep in sync.
    let prefixes = literal_prefixes(&config.scope.include);

    let mut paths = Vec::new();
    walk_dir(root, root, &include, &exclude, &prefixes, &mut paths)?;

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
                // A vanished file is benign — it simply can't be a
                // terminal parent. Any other I/O error (permissions,
                // bad UTF-8, …) must not be silently treated as
                // "non-terminal": that would quietly pull a terminal
                // parent's sub-artifacts back into scope. Propagate it.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(Error::Io {
                        path: abs_path,
                        source: e,
                    });
                }
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

/// The literal directory prefix of each include pattern — the segments
/// before the first one carrying a glob metacharacter. `.claude/**/*.md`
/// → `[.claude]`; `docs/.drafts/**` → `[docs, .drafts]`; a pattern that
/// opens with a wildcard (`**/*.md`) has an empty prefix and is dropped.
/// These anchor where an include pattern *literally* reaches, which is
/// what decides whether a hidden path was deliberately opted in.
fn literal_prefixes(include: &[String]) -> Vec<Vec<String>> {
    include
        .iter()
        .map(|pattern| {
            pattern
                .split('/')
                .take_while(|seg| !seg.contains(['*', '?', '[', '{']))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|prefix| !prefix.is_empty())
        .collect()
}

/// True if a path with a hidden (`.`-prefixed) segment is *not* opted in
/// by any include pattern, and so should be skipped — the ripgrep / fd /
/// git convention. A hidden segment counts as opted in only when an
/// include pattern names it literally *at its position*: a prefix `P`
/// admits path `R` when one is a path-prefix of the other and every
/// hidden segment of `R` falls within `P`'s literal coverage. So
/// `.claude/**/*.md` (`P = [.claude]`) admits root `.claude/...` but not
/// a nested `foo/.claude/...`, and a greedy `**/*.md` (empty prefix)
/// admits no hidden path at all.
fn hidden_path_skipped(rel: &[&str], prefixes: &[Vec<String>]) -> bool {
    let last_hidden = rel.iter().rposition(|seg| seg.starts_with('.'));
    let Some(last_hidden) = last_hidden else {
        return false; // no hidden segment — never skipped here
    };
    let admitted = prefixes.iter().any(|p| {
        let l = rel.len().min(p.len());
        last_hidden < l && rel[..l] == p[..l]
    });
    !admitted
}

fn walk_dir(
    base: &Path,
    dir: &Path,
    include: &GlobSet,
    exclude: &GlobSet,
    prefixes: &[Vec<String>],
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

        let rel = path.strip_prefix(base).unwrap_or(&path);
        let rel_str = crate::path_guard::forward_string(rel);
        let segments: Vec<&str> = rel_str.split('/').collect();

        if path.is_dir() {
            // `node_modules` / `.git` / … are non-content at any depth,
            // pruned by basename regardless of include patterns.
            if NON_CONTENT_DIRS.contains(&name_str.as_ref())
                || hidden_path_skipped(&segments, prefixes)
            {
                continue;
            }
            walk_dir(base, &path, include, exclude, prefixes, out)?;
        } else if path.is_file() {
            if hidden_path_skipped(&segments, prefixes) {
                continue;
            }
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
    fn greedy_include_skips_hidden_paths() {
        // A wildcard include that does not name a dotted segment leaves
        // hidden entries skipped — the ripgrep / fd / git convention.
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::create_dir_all(dir.path().join(".archive")).unwrap();
        fs::write(docs.join("guide.md"), "# Guide").unwrap();
        fs::write(docs.join(".draft.md"), "# Draft").unwrap();
        fs::write(dir.path().join(".archive/old.md"), "# Old").unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];

        let paths = scan_scope(dir.path(), &config).unwrap();
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(
            names.iter().any(|n| n.ends_with("guide.md")),
            "regular doc must be scanned: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(".draft")),
            "hidden file must be skipped under a greedy include: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains(".archive")),
            "hidden directory must be skipped under a greedy include: {names:?}"
        );
    }

    #[test]
    fn include_naming_dotted_segment_admits_hidden_path() {
        // An include pattern that literally names a dotted segment opts
        // exactly that hidden path in — no separate flag. Non-content
        // directories (`.git`, `.venv`, `node_modules`) stay pruned even
        // if a sibling pattern would otherwise reach them.
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
        config.scope.include = vec![".claude/**/*.md".to_string(), "**/*.md".to_string()];
        let paths = scan_scope(dir.path(), &config).unwrap();
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(names.iter().any(|n| n.contains(".claude")), "{names:?}");
        assert!(names.iter().any(|n| n.ends_with("doc.md")), "{names:?}");
        for noise in ["node_modules", ".git", ".venv"] {
            assert!(
                !names.iter().any(|n| n.contains(noise)),
                "non-content dir {noise:?} stays excluded: {names:?}"
            );
        }
    }

    #[test]
    fn dotted_include_is_position_anchored_not_basename() {
        // `.claude/**/*.md` opts in the ROOT `.claude`, not a `.claude`
        // nested anywhere. A vendored `sub/.claude/` must stay hidden
        // even though a sibling greedy `**/*.md` would otherwise match
        // its files once the directory is descended.
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".claude")).unwrap();
        fs::create_dir_all(dir.path().join("sub/.claude")).unwrap();
        fs::write(dir.path().join(".claude/root.md"), "# root").unwrap();
        fs::write(dir.path().join("sub/.claude/nested.md"), "# nested").unwrap();
        fs::write(dir.path().join("sub/plain.md"), "# plain").unwrap();

        let mut config = Config::default();
        config.scope.include = vec![".claude/**/*.md".to_string(), "**/*.md".to_string()];
        let paths = scan_scope(dir.path(), &config).unwrap();
        let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(
            names
                .iter()
                .any(|n| n.contains(".claude") && n.contains("root")),
            "root .claude must be admitted: {names:?}"
        );
        assert!(
            names.iter().any(|n| n.ends_with("plain.md")),
            "non-hidden sibling must be admitted: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("nested")),
            "nested sub/.claude must NOT be admitted by the root-anchored pattern: {names:?}"
        );
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
