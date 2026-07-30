use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::config::{Config, ScopeConfig};
use crate::error::{Error, Result};

/// The exact slice of [`Config`] that decides scope membership: the
/// `[scope]` block, the output directory (whose self-exclusion glob is
/// derived below), and — only when a `conditional_exclude` rule can
/// consult it — the terminal status vocabulary. Public scan functions
/// project into this view immediately and every private helper takes
/// `&ScanConfig`, so a new membership-affecting option cannot be read
/// without surfacing in the hashed projection
/// (`builder::graph_config_hash`) — the same compiler-enforcement
/// story as `parser::ParseConfig`. `terminal` is `None` when
/// `scope.conditional_exclude` is empty, so retuning terminal statuses
/// can never flag a graph outdated when no exclusion rule reads them.
#[derive(Serialize)]
pub struct ScanConfig<'a> {
    scope: &'a ScopeConfig,
    output_dir: &'a str,
    terminal: Option<&'a [String]>,
}

impl<'a> ScanConfig<'a> {
    /// Project the membership-affecting surface out of the full config.
    pub fn new(config: &'a Config) -> Self {
        Self {
            scope: &config.scope,
            output_dir: &config.output.dir,
            terminal: (!config.scope.conditional_exclude.is_empty())
                .then_some(config.statuses.terminal.as_slice()),
        }
    }

    /// True when `status` is terminal under the projected vocabulary.
    /// Always false when no `conditional_exclude` rule exists to ask.
    fn is_terminal(&self, status: &str) -> bool {
        self.terminal
            .is_some_and(|terminal| terminal.iter().any(|s| s == status))
    }

    /// The exclude patterns a scan actually enforces: the user's
    /// `scope.exclude` plus nodex's own output directory. Users would
    /// otherwise have to copy-paste `"_index/**"` into every project,
    /// and forgetting it silently causes `migrate`, `rename`, and
    /// `build` to treat GRAPH.md as a user document. The self-exclusion
    /// is unconditional — `Config::validate` refuses an empty
    /// `output.dir`. `Config::validate_scope` compiles exactly this
    /// list at load, so load-accept implies scan-success.
    pub(crate) fn effective_exclude_patterns(&self) -> Vec<String> {
        let mut patterns = self.scope.exclude.clone();
        patterns.push(format!("{}/**", self.output_dir.trim_end_matches('/')));
        patterns
    }
}

/// In-scope document paths, plus every way this walk declined to yield one.
/// Each decline is reported on the build result so it is auditable rather
/// than silent — a document the build never saw is a document no rule
/// judged, and the loss has to be visible from the outside.
pub struct ScopeScan {
    pub paths: Vec<PathBuf>,
    /// Paths a `conditional_exclude` rule dropped.
    pub conditionally_excluded: Vec<PathBuf>,
    /// In-scope paths that resolve to neither a file nor a directory — a
    /// symlink whose target is absent is the reachable case. The walk
    /// classifies by `is_dir` / `is_file`, both of which answer false here,
    /// so without this the entry would fall out of the classification with
    /// no record anywhere.
    pub dangling: Vec<PathBuf>,
}

/// Scan the filesystem for in-scope document paths.
/// Applies include/exclude globs, then conditional_exclude rules.
pub fn scan_scope(root: &Path, config: &Config) -> Result<ScopeScan> {
    scan_scope_with_overlay(root, config, &[])
}

/// [`scan_scope`] with proposed content overlaid: each overlay
/// `(rel_path, content)` participates in scope exactly as if it were
/// that file's on-disk bytes. A not-yet-written path joins the
/// candidate set under the same static policy the walk applies, and
/// `conditional_exclude` reads a parent's status from the overlay
/// before the disk. The scan is the single scope authority, so an
/// overlay graph and the real post-write build can never disagree
/// about a path's membership — including when the proposal itself
/// flips a conditional-exclude parent into or out of a terminal
/// status.
pub fn scan_scope_with_overlay(
    root: &Path,
    config: &Config,
    overlay: &[(PathBuf, String)],
) -> Result<ScopeScan> {
    let scan = ScanConfig::new(config);
    let include = build_globset(&scan.scope.include, "scope.include")?;
    let exclude = build_globset(&scan.effective_exclude_patterns(), "scope.exclude")?;

    // Hidden paths (`.draft.md`, `.archive/`, `.claude/`, …) are skipped
    // by default — the ripgrep / fd / git convention, since dot-prefixed
    // entries are overwhelmingly editor state or tooling config. An
    // include pattern overrides that default for exactly the segments it
    // names literally: `.claude/routines/*.md` opts `.claude` in, while a
    // greedy `**/*.md` does not. The include pattern is the opt-in; there
    // is no separate flag to keep in sync.
    let prefixes = literal_prefixes(&scan.scope.include);

    let policy = WalkPolicy {
        include: &include,
        exclude: &exclude,
        prefixes: &prefixes,
        prune_dirs: &scan.scope.prune_dirs,
    };
    let mut found = WalkFindings {
        paths: Vec::new(),
        dangling: Vec::new(),
    };
    walk_dir(root, root, &policy, &mut found)?;
    let WalkFindings {
        mut paths,
        mut dangling,
    } = found;

    // Overlay paths not on disk join the candidate set under the same
    // static policy the walk just applied entry by entry.
    for (rel_path, _) in overlay {
        if !paths.iter().any(|p| p == rel_path) && policy.admits(rel_path) {
            paths.push(rel_path.clone());
        }
    }

    // Apply conditional_exclude rules (e.g., terminal spec sub-artifact filtering)
    let mut conditionally_excluded = if scan.scope.conditional_exclude.is_empty() {
        Vec::new()
    } else {
        apply_conditional_excludes(root, &mut paths, &scan, overlay)
    };

    // Sort for deterministic processing order
    paths.sort();
    conditionally_excluded.sort();
    dangling.sort();
    Ok(ScopeScan {
        paths,
        conditionally_excluded,
        dangling,
    })
}

/// The compiled static scope policy: non-content pruning, hidden-segment
/// opt-in, include / exclude globs. The walk applies it entry by entry and
/// [`WalkPolicy::admits`] applies it to a single candidate path, so an
/// overlay path not yet on disk is judged by exactly what the walk judges.
struct WalkPolicy<'a> {
    include: &'a GlobSet,
    exclude: &'a GlobSet,
    prefixes: &'a [Vec<String>],
    prune_dirs: &'a [String],
}

impl WalkPolicy<'_> {
    /// Whether this policy admits `rel_path` as an in-scope document.
    fn admits(&self, rel_path: &Path) -> bool {
        let rel_str = crate::path_guard::forward_string(rel_path);
        let segments: Vec<&str> = rel_str.split('/').collect();
        if segments
            .iter()
            .any(|s| self.prune_dirs.iter().any(|d| d == s))
        {
            return false;
        }
        if hidden_path_skipped(&segments, self.prefixes) {
            return false;
        }
        self.include.is_match(&rel_str) && !self.exclude.is_match(&rel_str)
    }
}

/// What one walk yielded: the documents, and every path it declined.
struct WalkFindings {
    paths: Vec<PathBuf>,
    dangling: Vec<PathBuf>,
}

/// The overlay bytes for `rel_path`, when the path is overlaid.
pub(crate) fn overlay_content<'a>(
    overlay: &'a [(PathBuf, String)],
    rel_path: &Path,
) -> Option<&'a str> {
    overlay
        .iter()
        .find(|(p, _)| p == rel_path)
        .map(|(_, content)| content.as_str())
}

/// For each conditional_exclude rule:
/// 1. Find "parent" files matching `parent_glob` whose frontmatter
///    status is terminal — read from the overlay when the parent is
///    overlaid, so a proposal's own bytes decide its terminality.
/// 2. Within that parent's directory subtree, drop every file that
///    matches the rule's `child_glob` — except the parent itself. A
///    sibling the `child_glob` does not name is left in scope, so an
///    independently-owned document is never erased just for sharing a
///    directory with a terminal parent.
///
/// Mutates `paths` in place to the surviving set and returns the
/// excluded paths (for reporting on the build result — the exclusion is
/// auditable, never silent).
fn apply_conditional_excludes(
    root: &Path,
    paths: &mut Vec<PathBuf>,
    scan: &ScanConfig<'_>,
    overlay: &[(PathBuf, String)],
) -> Vec<PathBuf> {
    // A sub-artifact is dropped iff it sits under a terminal parent's
    // directory AND matches that rule's `child_glob`. The parent file
    // itself is always kept so it still parses into the graph.
    let mut drop: BTreeSet<PathBuf> = BTreeSet::new();
    let mut parents_to_keep: BTreeSet<PathBuf> = BTreeSet::new();

    for rule in &scan.scope.conditional_exclude {
        if rule.condition != "status_terminal" {
            continue;
        }

        let parent_glob = Glob::new(&rule.parent_glob)
            .expect("validated by Config::load")
            .compile_matcher();
        let child_glob = Glob::new(&rule.child_glob)
            .expect("validated by Config::load")
            .compile_matcher();

        // Directories whose terminal parent this rule governs.
        let mut terminal_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        for rel_path in paths.iter() {
            let rel_str = crate::path_guard::forward_string(rel_path);
            if !parent_glob.is_match(&rel_str) {
                continue;
            }

            // The overlay is the authoritative source for an overlaid
            // parent — its proposed bytes, not the stale on-disk ones,
            // decide whether the parent is terminal.
            let content = if let Some(proposed) = overlay_content(overlay, rel_path) {
                proposed.to_string()
            } else {
                // A parent the probe cannot read (vanished, permissions,
                // bad UTF-8, …) cannot be *confirmed* terminal, and
                // exclusion is a positive decision — so the probe
                // degrades conservatively: the sub-artifacts stay in
                // scope, and the unreadable parent — itself a scanned
                // path — is recorded as a typed `ParseFailure` by the
                // build's read phase, so `check` reds the same file
                // (reader-degrades / loud-elsewhere, exactly like an
                // unparseable or fence-broken parent below). Never a
                // silent exclusion, never a halted scan.
                match std::fs::read_to_string(root.join(rel_path)) {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            };
            let content = crate::parser::frontmatter::canonicalize(&content);

            if is_terminal_status(&content, scan) {
                parents_to_keep.insert(rel_path.clone());
                terminal_dirs.insert(rel_path.parent().map(Path::to_path_buf).unwrap_or_default());
            }
        }

        if terminal_dirs.is_empty() {
            continue;
        }
        for rel_path in paths.iter() {
            if parents_to_keep.contains(rel_path) {
                continue;
            }
            let under_terminal = terminal_dirs.iter().any(|dir| rel_path.starts_with(dir));
            let rel_str = crate::path_guard::forward_string(rel_path);
            if under_terminal && child_glob.is_match(&rel_str) {
                drop.insert(rel_path.clone());
            }
        }
    }

    if drop.is_empty() {
        return Vec::new();
    }
    paths.retain(|p| !drop.contains(p));
    drop.into_iter().collect()
}

/// Quick check if a file's frontmatter declares a terminal status.
/// Uses a lightweight YAML parse (not the full frontmatter parser) on
/// the hot scan path. A missing status field, unparseable YAML, an
/// unclosed fence, or an absent frontmatter block is treated as "not
/// terminal" — those documents surface as violations in `check`, not
/// as silent excludes from `build`.
fn is_terminal_status(content: &str, scan: &ScanConfig<'_>) -> bool {
    let Ok((Some(yaml), _)) = crate::parser::frontmatter::split_frontmatter(content) else {
        return false;
    };
    let Ok(value) = yaml_serde::from_str::<yaml_serde::Value>(yaml) else {
        return false;
    };
    value
        .as_mapping()
        .and_then(|m| m.get(yaml_serde::Value::String("status".to_string())))
        .and_then(|v| v.as_str())
        .map(|s| scan.is_terminal(s))
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
    root: &Path,
    policy: &WalkPolicy<'_>,
    found: &mut WalkFindings,
) -> Result<()> {
    // Iterative DFS over an explicit stack, with a visited-set of
    // canonicalised directory paths. The scanner follows symlinks on read
    // (`is_dir`/`is_file` resolve them), so a symlinked directory that
    // points back into the tree (a cycle) — or a pathologically deep tree —
    // must NOT recurse the call stack into a stack overflow: that aborted
    // `nodex build` with SIGABRT, escaping the JSON envelope. Canonicalising
    // each directory before descending collapses a symlink and its target
    // to one identity, so a re-visit is a cycle and is skipped; `paths.sort()`
    // downstream makes the pop-order irrelevant to the output.
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();

    while let Some(dir) = stack.pop() {
        let identity = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if !visited.insert(identity) {
            // Already walked this real directory via another path — a
            // symlink cycle. Skip it rather than loop forever.
            continue;
        }

        let entries = std::fs::read_dir(&dir).map_err(|e| Error::Io {
            path: dir.clone(),
            source: e,
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| Error::Io {
                path: dir.clone(),
                source: e,
            })?;
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            let rel = path.strip_prefix(base).unwrap_or(&path);
            let rel_str = crate::path_guard::forward_string(rel);
            let segments: Vec<&str> = rel_str.split('/').collect();

            if path.is_dir() {
                // `scope.prune_dirs` basenames (node_modules / target / …)
                // are pruned at any depth regardless of include patterns;
                // dot-prefixed trees are also caught by the hidden guard.
                if policy.prune_dirs.iter().any(|d| d == name_str.as_ref())
                    || hidden_path_skipped(&segments, policy.prefixes)
                {
                    continue;
                }
                stack.push(path);
            } else if path.is_file() {
                if policy.admits(rel) {
                    found.paths.push(rel.to_path_buf());
                }
            } else if policy.admits(rel) {
                // Neither a directory nor a file, yet the globs say this
                // path is meant to hold a document: a symlink with no
                // target. There is nothing to read, so the walk cannot
                // yield it — but a build that omits an in-scope document
                // without saying so is how a baseline loses one whose lock
                // then never fires, with an empty warnings array. Record
                // the decline; the consumer decides what it means.
                found.dangling.push(rel.to_path_buf());
            }
        }
    }

    Ok(())
}

/// Compile a glob list into one matcher set, labelling errors with the
/// config surface (`key`) the patterns came from. The single compile
/// path shared by the scanner and `Config::validate_scope` — load-time
/// acceptance and scan-time compilation can never drift, and the
/// load-time error names exactly the key the operator must fix.
pub(crate) fn build_globset(patterns: &[String], key: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|e| {
            Error::Config(format!(
                "{key} pattern {pattern:?} is not a valid glob: {e}"
            ))
        })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| Error::Config(format!("{key} globset build error: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConditionalExclude;
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

        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("guide.md")));
        assert!(paths.iter().any(|p| p.ends_with("README.md")));
    }

    #[test]
    fn default_prune_dirs_skips_dependency_trees() {
        let dir = TempDir::new().unwrap();
        let nm = dir.path().join("node_modules/pkg");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("readme.md"), "# dep").unwrap();
        fs::write(dir.path().join("real.md"), "# real").unwrap();

        // Default config prunes node_modules.
        let paths = scan_scope(dir.path(), &Config::default()).unwrap().paths;
        assert!(paths.iter().any(|p| p.ends_with("real.md")));
        assert!(
            !paths
                .iter()
                .any(|p| p.to_string_lossy().contains("node_modules")),
            "node_modules pruned by default: {paths:?}"
        );
    }

    #[test]
    fn prune_dirs_override_admits_a_formerly_pruned_dir() {
        // A project that vaults docs under a directory named like a build
        // tree opts it back in by dropping it from scope.prune_dirs — the
        // prune list is config, not a hardcoded fact.
        let dir = TempDir::new().unwrap();
        let t = dir.path().join("target/docs");
        fs::create_dir_all(&t).unwrap();
        fs::write(t.join("spec.md"), "# spec").unwrap();

        let mut config = Config::default();
        config.scope.prune_dirs = vec!["node_modules".to_string(), ".git".to_string()];
        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert!(
            paths.iter().any(|p| p.ends_with("spec.md")),
            "target/ is scannable when not in prune_dirs: {paths:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_overflow_on_a_symlink_cycle() {
        // A symlinked directory pointing back into the tree must not loop
        // the scanner into a stack overflow (the recursive walk aborted
        // `nodex build` with SIGABRT). The iterative walk with a
        // canonical-path visited-set skips the re-entry: the real file is
        // found and the cycle adds no unbounded phantom paths.
        let dir = TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("a.md"), "# A").unwrap();
        // docs/loop -> project root, i.e. docs/loop/docs/loop/… forever.
        std::os::unix::fs::symlink(dir.path(), docs.join("loop")).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["**/*.md".to_string()];

        // Completes (no SIGABRT / hang); the real doc is found and the
        // cycle yields no unbounded multiplication of paths.
        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert!(
            paths.iter().any(|p| p.ends_with("a.md")),
            "real file must be found: {paths:?}"
        );
        assert!(paths.len() < 5, "cycle must not multiply paths: {paths:?}");
    }

    #[test]
    fn conditional_exclude_drops_only_child_glob_matches() {
        // A terminal `SPEC.md` drops its `child_glob`-matching
        // sub-artefacts (here `tasks/*`) while keeping the parent — and
        // crucially leaves an independently-owned sibling (an active
        // decision log the rule never names) in scope, instead of
        // erasing the whole directory.
        let dir = TempDir::new().unwrap();
        let auth = dir.path().join("specs/auth");
        fs::create_dir_all(auth.join("tasks")).unwrap();
        fs::write(
            auth.join("SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("tasks/t1.md"),
            "---\nid: spec-auth-t1\ntitle: T1\nkind: spec\nstatus: draft\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("decisions.md"),
            "---\nid: dec-auth\ntitle: Decisions\nkind: generic\nstatus: active\n---\n",
        )
        .unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["specs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/**/SPEC.md".to_string(),
            child_glob: "specs/**/tasks/**".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let scan = scan_scope(dir.path(), &config).unwrap();
        let kept: Vec<String> = scan
            .paths
            .iter()
            .map(|p| crate::path_guard::forward_string(p))
            .collect();
        assert!(
            kept.iter().any(|p| p.ends_with("SPEC.md")),
            "parent kept: {kept:?}"
        );
        assert!(
            kept.iter().any(|p| p.ends_with("decisions.md")),
            "independent active sibling must survive: {kept:?}"
        );
        assert!(
            !kept.iter().any(|p| p.contains("tasks/")),
            "child_glob sub-artefact dropped: {kept:?}"
        );
        // The drop is reported, not silent.
        assert_eq!(
            scan.conditionally_excluded
                .iter()
                .map(|p| crate::path_guard::forward_string(p))
                .collect::<Vec<_>>(),
            vec!["specs/auth/tasks/t1.md".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn conditional_exclude_degrades_on_unreadable_parent_and_build_reds_it() {
        // A parent the terminality probe cannot read is never *confirmed*
        // terminal, so the scan completes with its sub-artifacts in scope
        // (exclusion is a positive decision) — and the unreadable parent,
        // itself a scanned path, becomes a typed ParseFailure in the
        // build, so `check` reds the same file. Loud through the typed
        // channel, never a halted scan, never a silent exclusion.
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let auth = dir.path().join("specs/auth");
        fs::create_dir_all(auth.join("tasks")).unwrap();
        let spec = auth.join("SPEC.md");
        fs::write(
            &spec,
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("tasks/t1.md"),
            "---\nid: spec-auth-t1\ntitle: T1\nkind: spec\nstatus: draft\n---\n",
        )
        .unwrap();
        fs::set_permissions(&spec, fs::Permissions::from_mode(0o000)).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["specs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/**/SPEC.md".to_string(),
            child_glob: "specs/**/tasks/**".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let scan = scan_scope(dir.path(), &config).expect("scan completes");
        let kept: Vec<String> = scan
            .paths
            .iter()
            .map(|p| crate::path_guard::forward_string(p))
            .collect();
        assert!(
            kept.contains(&"specs/auth/tasks/t1.md".to_string()),
            "unconfirmed terminality keeps the children in scope: {kept:?}"
        );
        assert!(
            scan.conditionally_excluded.is_empty(),
            "nothing was excluded: {:?}",
            scan.conditionally_excluded
        );

        let outcome = crate::builder::build(dir.path(), &config, true).expect("build never halts");
        assert!(
            outcome
                .graph
                .parse_failures()
                .iter()
                .any(|f| f.path == "specs/auth/SPEC.md"),
            "the unreadable parent is a typed parse failure: {:?}",
            outcome.graph.parse_failures()
        );

        fs::set_permissions(&spec, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn conditional_exclude_can_drop_whole_directory_explicitly() {
        // `child_glob = "**/*"` is the explicit opt-in to clearing the
        // whole subtree under a terminal parent.
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
            child_glob: "**/*".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert_eq!(paths.len(), 1, "only the parent survives: {paths:?}");
        assert!(paths[0].ends_with("SPEC.md"));
    }

    #[test]
    fn overlay_parent_status_drives_conditional_exclude_both_ways() {
        // The proposal's bytes — not the stale on-disk ones — decide a
        // conditional-exclude parent's terminality, so an overlay scan
        // matches the post-write world in both directions: a proposal
        // that terminalizes the parent drops its children, and one that
        // re-activates a terminal parent re-admits them.
        let dir = TempDir::new().unwrap();
        let auth = dir.path().join("specs/auth");
        fs::create_dir_all(auth.join("tasks")).unwrap();
        fs::write(
            auth.join("SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: active\n---\n",
        )
        .unwrap();
        fs::write(
            auth.join("tasks/t1.md"),
            "---\nid: spec-auth-t1\ntitle: T1\nkind: spec\nstatus: draft\n---\n",
        )
        .unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["specs/**/*.md".to_string()];
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/**/SPEC.md".to_string(),
            child_glob: "specs/**/tasks/**".to_string(),
            condition: "status_terminal".to_string(),
        }];

        let terminal_parent = (
            PathBuf::from("specs/auth/SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n".to_string(),
        );
        let scan =
            scan_scope_with_overlay(dir.path(), &config, std::slice::from_ref(&terminal_parent))
                .unwrap();
        assert!(
            !scan
                .paths
                .iter()
                .any(|p| p.to_string_lossy().contains("tasks")),
            "a proposal terminalizing the parent must drop its children: {:?}",
            scan.paths
        );

        // Reverse: terminal on disk, proposal re-activates → children return.
        fs::write(
            auth.join("SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: superseded\n---\n",
        )
        .unwrap();
        let active_parent = (
            PathBuf::from("specs/auth/SPEC.md"),
            "---\nid: spec-auth\ntitle: Auth\nkind: spec\nstatus: active\n---\n".to_string(),
        );
        let scan =
            scan_scope_with_overlay(dir.path(), &config, std::slice::from_ref(&active_parent))
                .unwrap();
        assert!(
            scan.paths
                .iter()
                .any(|p| p.to_string_lossy().contains("tasks")),
            "a proposal re-activating a terminal parent must re-admit its children: {:?}",
            scan.paths
        );
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

        let paths = scan_scope(dir.path(), &config).unwrap().paths;
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
        let paths = scan_scope(dir.path(), &config).unwrap().paths;
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
        let paths = scan_scope(dir.path(), &config).unwrap().paths;
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

        let paths = scan_scope(dir.path(), &config).unwrap().paths;
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("guide.md"));
    }
}
