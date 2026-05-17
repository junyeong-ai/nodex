//! Scaffold new document nodes.
//!
//! Creates a valid frontmatter + body skeleton obeying the project's
//! config (kind inference, id rules, required fields, enum defaults,
//! cross-field constraints). AI agents use this to avoid frontmatter
//! typos and missing-field errors when creating new documents.
//!
//! Every decision prefers config over heuristic; heuristics only kick
//! in when config is silent. Callers can override any inferred value
//! by supplying it explicitly on [`ScaffoldSpec`].

use chrono::Local;
use globset::Glob;
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::config::{Config, FieldType, parse_when};
use crate::error::{Error, Result};
use crate::model::{Graph, Kind};
use crate::parser::identity::infer_id;

/// User-supplied scaffold parameters. All override fields are optional;
/// [`scaffold`] fills in the rest from config.
#[derive(Debug, Clone)]
pub struct ScaffoldSpec {
    pub kind: Kind,
    pub title: String,
    /// Overrides automatic id inference when `Some`.
    pub id: Option<String>,
    /// Overrides automatic path inference when `Some`. Relative to root.
    pub path: Option<PathBuf>,
}

/// Outcome of a scaffold request. When `write = false` (dry-run),
/// `written` is `false` and the file is untouched.
///
/// Advisory notes the caller might want to surface live in the
/// separate `Vec<String>` returned alongside this struct, never on the
/// struct itself — the JSON envelope contract puts `warnings` at the
/// envelope level, not inside `data`.
#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldResult {
    pub id: String,
    #[serde(serialize_with = "crate::model::node::serialize_path_forward")]
    pub path: PathBuf,
    pub content: String,
    pub written: bool,
}

/// Scaffold a new node.
///
/// When `write` is `true`, the file is written atomically (temp file +
/// rename) and `ScaffoldResult::written` is set. Existing files are
/// rejected unless `force` is set.
///
/// Returns `(result, warnings)` so the caller can surface warnings at
/// the JSON-envelope level (`json-output.md`'s `{ ok, data, warnings }`
/// contract) without fishing them out of `data`.
pub fn scaffold(
    root: &Path,
    spec: ScaffoldSpec,
    graph: &Graph,
    config: &Config,
    write: bool,
    force: bool,
) -> Result<(ScaffoldResult, Vec<String>)> {
    // 1. Validate kind against config.
    if !config
        .kinds
        .allowed
        .contains(&spec.kind.as_str().to_string())
    {
        return Err(Error::Config(format!(
            "unknown kind {:?}; allowed: {:?}",
            spec.kind.as_str(),
            config.kinds.allowed
        )));
    }

    // 2. Resolve path (explicit override or infer from kind_rules).
    let rel_path = match spec.path.clone() {
        Some(p) => p,
        None => infer_path(&spec.kind, &spec.title, graph, config)?,
    };

    // Scaffold targets must wear one of the project's document
    // extensions; otherwise nothing downstream (parser, link
    // extraction, scanner globs) would treat the file as in-graph.
    let target_ext = rel_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"));
    if !target_ext.as_deref().is_some_and(|ext| {
        config
            .parser
            .extensions
            .iter()
            .any(|allowed| allowed == ext)
    }) {
        return Err(Error::Config(format!(
            "scaffold target must end with one of {:?}; got {}",
            config.parser.extensions,
            rel_path.display()
        )));
    }

    // Refuse any path that would escape the project root — `..` or
    // absolute forms are never legitimate scaffold targets.
    crate::path_guard::reject_traversal(&rel_path)?;

    let abs_path = root.join(&rel_path);

    // 3. Resolve id (explicit override or infer via existing identity rules).
    let id = spec
        .id
        .clone()
        .unwrap_or_else(|| infer_id(&rel_path, &spec.kind, config));
    detect_id_collision(&id, &rel_path, root, graph, config)?;

    // 4. Reject existing file unless --force.
    if abs_path.exists() && !force {
        return Err(Error::Exists(abs_path));
    }

    // 5. Build frontmatter YAML and body.
    let content = render_document(&id, &spec, &rel_path, config);

    // 6. Pre-validate: run rules against a synthetic single-node graph
    //    so the caller learns which defaults they still need to fill in;
    //    additionally warn when an existing doc looks similar enough to
    //    be a duplicate so the caller can supersede instead of forking.
    let warnings =
        collect_scaffold_warnings(&id, &rel_path, &content, &spec, graph, config, root, write);

    // 7. Write atomically (or skip in dry-run).
    let written = if write {
        crate::path_guard::write_atomic(&abs_path, &content)?;
        true
    } else {
        false
    };

    Ok((
        ScaffoldResult {
            id,
            path: rel_path,
            content,
            written,
        },
        warnings,
    ))
}

// ─── path inference ─────────────────────────────────────────────────

fn infer_path(kind: &Kind, title: &str, graph: &Graph, config: &Config) -> Result<PathBuf> {
    // Find the first kind_rule that produces this kind.
    let Some(rule) = config
        .identity
        .kind_rules
        .iter()
        .find(|r| r.kind == kind.as_str())
    else {
        return Err(Error::Config(format!(
            "cannot infer path for kind {:?}: no identity.kind_rules match; \
             supply `--path` explicitly",
            kind.as_str()
        )));
    };

    let dir = directory_from_glob(&rule.glob).ok_or_else(|| {
        Error::Config(format!(
            "kind_rule glob {:?} does not yield a concrete directory; \
             supply `--path` explicitly",
            rule.glob
        ))
    })?;

    let stem = next_filename_stem(&dir, title, graph, config);
    Ok(dir.join(format!("{stem}.md")))
}

/// Reduce a glob to its leading literal directory. `docs/decisions/**`
/// → `docs/decisions`. Returns `None` when the glob lacks a literal prefix.
fn directory_from_glob(glob: &str) -> Option<PathBuf> {
    let mut parts = Vec::new();
    for segment in glob.split('/') {
        if segment.contains('*') || segment.contains('?') || segment.contains('[') {
            break;
        }
        parts.push(segment);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.iter().collect())
}

/// Build the filename stem. When a naming rule has `sequential = true`
/// and matches the target directory via directory-prefix containment,
/// use `NNNN-<slug>` with the next available number; otherwise plain
/// `<slug>`.
fn next_filename_stem(dir: &Path, title: &str, graph: &Graph, config: &Config) -> String {
    let slug = slugify(title);
    let dir_str = crate::path_guard::forward_string(dir);

    for rule in &config.rules.naming {
        if !rule.sequential {
            continue;
        }
        let matcher = Glob::new(&rule.glob)
            .expect("validated by Config::load")
            .compile_matcher();

        // The rule's literal prefix (segments before the first
        // wildcard) must equal the scaffolded directory.
        if !rule_targets_directory(&rule.glob, &dir_str) {
            continue;
        }
        let (next, width) = next_sequence(graph, &matcher, &rule.pattern);
        let padded = format!("{:0>width$}", next, width = width);
        return format!("{padded}-{slug}");
    }

    slug
}

/// Does `glob` apply to files under `dir`? The glob's literal prefix
/// (every segment before the first wildcard) must equal `dir`.
///
/// `directory_from_glob` already computes that prefix — delegating to
/// it keeps the "literal prefix equality" contract documented at one
/// place and dodges a class of broken glob-synthesis edge cases
/// (`*.md`, `?*`, `[0-9]*.md`, middle-path wildcards) that the earlier
/// synthesis approach silently mis-matched.
///
/// Examples (all verified in tests):
///   glob = "docs/decisions/**",        dir = "docs/decisions"       → true
///   glob = "docs/decisions/*.md",      dir = "docs/decisions"       → true
///   glob = "docs/decisions/[0-9]*.md", dir = "docs/decisions"       → true
///   glob = "docs/*/decisions/**",      dir = "docs"                 → true
///   glob = "docs/guides/**",           dir = "docs/decisions"       → false
fn rule_targets_directory(glob: &str, dir: &str) -> bool {
    let Some(prefix) = directory_from_glob(glob) else {
        return false;
    };
    crate::path_guard::forward_string(&prefix) == dir
}

/// Find the next sequence number for files matching `matcher`, preserving
/// the digit width of existing filenames.
fn next_sequence(graph: &Graph, matcher: &globset::GlobMatcher, pattern: &str) -> (u64, usize) {
    let digit_re = Regex::new(r"^(\d+)").expect("static regex compiles");
    let pattern_re = Regex::new(pattern).ok();
    let mut max_seen: u64 = 0;
    let mut width: usize = 4; // sensible default for ADR-style numbering

    for node in graph.nodes().values() {
        let path_str = crate::path_guard::forward_string(&node.path);
        if !matcher.is_match(&path_str) {
            continue;
        }
        if let Some(ref re) = pattern_re
            && let Some(stem) = node.path.file_name().and_then(|s| s.to_str())
            && !re.is_match(stem)
        {
            continue;
        }
        let Some(stem) = node.path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(cap) = digit_re.captures(stem) else {
            continue;
        };
        let digits = cap.get(1).unwrap().as_str();
        width = width.max(digits.len());
        if let Ok(n) = digits.parse::<u64>() {
            max_seen = max_seen.max(n);
        }
    }

    (max_seen + 1, width)
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_hyphen = true; // leading underscore → no leading hyphen
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_hyphen = false;
        } else if !last_hyphen {
            out.push('-');
            last_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

// ─── frontmatter rendering ──────────────────────────────────────────

/// Render a YAML frontmatter body (without `---` delimiters) that
/// satisfies every `required` + `cross_field` rule the project has
/// declared for `kind`. Shared between `scaffold` (creating a new
/// file) and `migrate` (injecting frontmatter into a bare file) so
/// both paths produce documents that pass `check` immediately — the
/// self-consistency invariant codified in
/// `.claude/rules/config-driven.md`.
pub fn render_default_frontmatter(id: &str, title: &str, kind: &str, config: &Config) -> String {
    let required: Vec<String> = config.required_for(kind).to_vec();
    let today = Local::now().date_naive().to_string();

    let mut lines: Vec<String> = Vec::new();
    let mut emit = |key: &str, value: String| {
        lines.push(format!("{key}: {value}"));
    };

    emit("id", crate::yaml_text::quote(id));
    emit("title", crate::yaml_text::quote(title));
    emit("kind", crate::yaml_text::quote(kind));

    let default_status = config.initial_status_for(kind);
    emit("status", crate::yaml_text::quote(default_status));

    let mut seen: std::collections::BTreeSet<String> = ["id", "title", "kind", "status"]
        .into_iter()
        .map(String::from)
        .collect();
    for field in &required {
        if seen.contains(field) {
            continue;
        }
        let value = default_for_field(field, kind, config, &today);
        emit(field, value);
        seen.insert(field.clone());
    }

    // Honour cross_field (global + per-kind): when a predicate matches
    // the default node, emit the `require` field so the document is
    // immediately valid against its own schema.
    let default_node = scaffold_default_node(kind, default_status);
    for cf in config.cross_field_for(kind) {
        let Ok(predicate) = parse_when(&cf.when) else {
            continue;
        };
        if !crate::rules::schema::predicate_matches_node(&predicate, &default_node) {
            continue;
        }
        if seen.contains(&cf.require) {
            continue;
        }
        let value = default_for_field(&cf.require, kind, config, &today);
        emit(&cf.require, value);
        seen.insert(cf.require.clone());
    }

    lines.join("\n")
}

fn render_document(id: &str, spec: &ScaffoldSpec, path: &Path, config: &Config) -> String {
    let frontmatter = render_default_frontmatter(id, &spec.title, spec.kind.as_str(), config);

    let stem_title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Document");
    // The body heading is plain markdown — control characters would
    // break the H1 line (a newline splits the heading into unrelated
    // prose). Collapse every control character to a space so the
    // visible title in the rendered markdown matches the frontmatter.
    let body_heading = if spec.title.is_empty() {
        stem_title.to_string()
    } else {
        spec.title
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
    };

    format!("---\n{frontmatter}\n---\n\n# {body_heading}\n")
}

/// Build a synthetic `Node` reflecting the scaffold's defaults. Used to
/// evaluate `cross_field.when` predicates against the not-yet-written
/// document without duplicating predicate-evaluation logic.
fn scaffold_default_node(kind: &str, default_status: &str) -> crate::model::Node {
    crate::model::Node {
        id: String::new(),
        path: PathBuf::new(),
        title: String::new(),
        kind: crate::model::Kind::new(kind),
        status: crate::model::Status::new(default_status),
        created: None,
        updated: None,
        reviewed: None,
        owner: None,
        supersedes: vec![],
        superseded_by: None,
        implements: vec![],
        related: vec![],
        tags: vec![],
        covers: vec![],
        orphan_ok: false,
        attrs: Default::default(),
    }
}

fn default_for_field(field: &str, kind: &str, config: &Config, today: &str) -> String {
    // Use the merged (global + override) views so a project declaring
    // `types` / `enums` at the top-level `[schema]` — with no per-kind
    // override — still gets a type-/enum-valid default here. Reading
    // only from `schema_override_for(kind)` missed that case and let
    // scaffold write `priority: ""` against a global
    // `types = { priority = "integer" }`, which immediately failed
    // `FieldTypeRule`. `enums_for` and `types_for` are the same views
    // the rules themselves consume, so scaffold's defaults and
    // check's expectations cannot drift.
    let enums = config.enums_for(kind);
    if let Some(allowed) = enums.get(field)
        && let Some(first) = allowed.first()
    {
        return first.clone();
    }

    let types = config.types_for(kind);
    if let Some(ty) = types.get(field) {
        return match ty {
            FieldType::Date => today.to_string(),
            FieldType::Integer => "0".to_string(),
            FieldType::Bool => "false".to_string(),
            FieldType::String => "\"\"".to_string(),
        };
    }

    // Built-in field conventions
    match field {
        "created" | "updated" | "reviewed" => today.to_string(),
        "owner" | "superseded_by" => "\"\"".to_string(),
        "supersedes" | "implements" | "related" | "tags" => "[]".to_string(),
        _ => "\"\"".to_string(),
    }
}

// ─── collision detection ────────────────────────────────────────────

/// Reject the scaffold if `id` already exists in the graph **or** in
/// any scanned markdown file under `root`. The disk-level check closes
/// a race where the graph was built before a recent scaffold, so the
/// stale graph.json doesn't know about the new id yet.
fn detect_id_collision(
    id: &str,
    rel_path: &Path,
    root: &Path,
    graph: &Graph,
    config: &Config,
) -> Result<()> {
    if let Some(existing) = graph.nodes().get(id) {
        // If the graph already indexes this id at the scaffold target
        // itself, it is not a collision — the caller's `--force` flag
        // decides whether to overwrite. The later `abs_path.exists()`
        // check gates that.
        if existing.path != rel_path {
            return Err(Error::DuplicateId {
                id: id.to_string(),
                first: existing.path.clone(),
                second: rel_path.to_path_buf(),
            });
        }
    }
    if let Some(existing) = scan_disk_for_id(id, rel_path, root, config) {
        return Err(Error::DuplicateId {
            id: id.to_string(),
            first: existing,
            second: rel_path.to_path_buf(),
        });
    }
    Ok(())
}

fn scan_disk_for_id(id: &str, rel_path: &Path, root: &Path, config: &Config) -> Option<PathBuf> {
    // Only inspect the target directory: scanning the whole project
    // every scaffold would be O(N) and defeats the point of the graph
    // index. Same-id conflicts between different directories are
    // caught by the normal build step.
    let parent = rel_path.parent()?;
    let dir = root.join(parent);
    let target_abs = root.join(rel_path);
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext_dot = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|e| format!(".{e}"));
        let is_doc = ext_dot
            .as_deref()
            .is_some_and(|e| config.parser.extensions.iter().any(|allowed| allowed == e));
        if !is_doc {
            continue;
        }
        // Skip the scaffold target itself so `--force` can legitimately
        // overwrite an existing file holding the same id.
        if std::fs::canonicalize(&path).ok() == std::fs::canonicalize(&target_abs).ok()
            && target_abs.exists()
        {
            continue;
        }
        // Any per-file read error must *skip* that file, not abort
        // the whole scan. A single permission glitch would otherwise
        // claim "no collision" and let scaffold clobber a legitimate
        // duplicate.
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (yaml, _) = crate::parser::frontmatter::split_frontmatter(&content);
        let Some(yaml) = yaml else { continue };
        // Keep the id lookup line-based rather than pulling a full YAML
        // parser into scaffold — we only care about a single scalar.
        for line in yaml.lines() {
            if let Some(rest) = line.strip_prefix("id:") {
                let value = rest.trim().trim_matches(['"', '\'']);
                if value == id {
                    let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                    return Some(rel);
                }
                break;
            }
        }
    }
    None
}

// ─── pre-validation ─────────────────────────────────────────────────

/// Compose advisory warnings for the scaffold caller:
/// 1. Existing doc(s) the new one closely resembles — supersede candidate.
/// 2. Rule violations the new doc would trigger if built now.
/// 3. "Run `nodex build`" reminder once the file was actually written.
#[allow(clippy::too_many_arguments)]
fn collect_scaffold_warnings(
    id: &str,
    rel_path: &Path,
    content: &str,
    spec: &ScaffoldSpec,
    graph: &Graph,
    config: &Config,
    root: &Path,
    written: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();

    // Duplicate detection — vector-free similarity against the live
    // graph. Surface the top match with its score so the agent can
    // decide whether `lifecycle supersede` is the right move.
    let target = crate::query::similar::SimilarityTarget::Spec {
        title: &spec.title,
        kind: Some(spec.kind.as_str()),
        tags: &[],
        parent_dir: rel_path.parent(),
    };
    let opts = crate::query::similar::SimilarityOptions::from_config(config);
    if let Ok(candidates) = crate::query::similar::compute_similarity(graph, config, &target, &opts)
        && let Some(top) = candidates.first()
    {
        warnings.push(format!(
            "similar doc exists: {:?} (similarity {:.2}); consider `lifecycle supersede` instead of creating a duplicate",
            top.node.id, top.similarity
        ));
    }

    if let Ok((node, _)) = crate::parser::frontmatter::parse_frontmatter(rel_path, content) {
        let mut map = indexmap::IndexMap::new();
        map.insert(id.to_string(), node);
        let synthetic = Graph::new(map, vec![]);
        let report = crate::rules::check_all(&synthetic, config, root);
        for v in report.violations {
            warnings.push(format!("{}: {}", v.rule_id, v.message));
        }
    }

    if written {
        warnings.push("run `nodex build` to include this document in the graph".to_string());
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{IdRule, IdentityConfig, KindRule, KindsConfig, NamingRule, RulesConfig};
    use crate::model::{Kind, Node, Status};
    use indexmap::IndexMap;

    fn adr_config() -> Config {
        Config {
            kinds: KindsConfig {
                allowed: vec!["adr".into(), "guide".into()],
            },
            identity: IdentityConfig {
                kind_rules: vec![KindRule {
                    glob: "docs/decisions/**".into(),
                    kind: "adr".into(),
                }],
                id_rules: vec![IdRule {
                    kind: "adr".into(),
                    glob: None,
                    template: "adr-{stem}".into(),
                }],
            },
            rules: RulesConfig {
                naming: vec![NamingRule {
                    glob: "docs/decisions/**".into(),
                    pattern: r"^\d{4}-[a-z0-9-]+\.md$".into(),
                    sequential: true,
                    unique: true,
                }],
                ..Default::default()
            },
            ..Config::default()
        }
    }

    fn empty_graph() -> Graph {
        Graph::new(IndexMap::new(), vec![])
    }

    #[test]
    fn infers_sequential_filename_from_empty_graph() {
        let (result, _) = scaffold(
            Path::new("/tmp"),
            ScaffoldSpec {
                kind: Kind::new("adr"),
                title: "Retry policy".into(),
                id: None,
                path: None,
            },
            &empty_graph(),
            &adr_config(),
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            crate::path_guard::forward_string(&result.path),
            "docs/decisions/0001-retry-policy.md"
        );
        assert_eq!(result.id, "adr-0001-retry-policy");
        assert!(!result.written);
    }

    #[test]
    fn increments_sequence_from_existing_nodes() {
        let mut map = IndexMap::new();
        map.insert(
            "adr-0003-auth".into(),
            Node {
                id: "adr-0003-auth".into(),
                path: PathBuf::from("docs/decisions/0003-auth.md"),
                title: "Auth".into(),
                kind: Kind::new("adr"),
                status: Status::new("active"),
                created: None,
                updated: None,
                reviewed: None,
                owner: None,
                supersedes: vec![],
                superseded_by: None,
                implements: vec![],
                related: vec![],
                tags: vec![],
                covers: vec![],
                orphan_ok: true,
                attrs: Default::default(),
            },
        );
        let graph = Graph::new(map, vec![]);
        let (result, _) = scaffold(
            Path::new("/tmp"),
            ScaffoldSpec {
                kind: Kind::new("adr"),
                title: "Cache eviction".into(),
                id: None,
                path: None,
            },
            &graph,
            &adr_config(),
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            crate::path_guard::forward_string(&result.path),
            "docs/decisions/0004-cache-eviction.md"
        );
    }

    #[test]
    fn rejects_unknown_kind() {
        let err = scaffold(
            Path::new("/tmp"),
            ScaffoldSpec {
                kind: Kind::new("wat"),
                title: "x".into(),
                id: None,
                path: None,
            },
            &empty_graph(),
            &adr_config(),
            false,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn explicit_path_bypasses_kind_rule() {
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["note".into()],
            },
            ..Config::default()
        };
        let (result, _) = scaffold(
            Path::new("/tmp"),
            ScaffoldSpec {
                kind: Kind::new("note"),
                title: "Hello".into(),
                id: Some("note-hello".into()),
                path: Some(PathBuf::from("misc/hello.md")),
            },
            &empty_graph(),
            &config,
            false,
            false,
        )
        .unwrap();
        assert_eq!(result.path.to_string_lossy(), "misc/hello.md");
        assert_eq!(result.id, "note-hello");
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  multiple   spaces  "), "multiple-spaces");
        assert_eq!(slugify("cache_eviction-v2"), "cache-eviction-v2");
    }

    #[test]
    fn directory_from_glob_handles_literals() {
        assert_eq!(
            directory_from_glob("docs/decisions/**"),
            Some(PathBuf::from("docs/decisions"))
        );
        assert_eq!(directory_from_glob("**/SKILL.md"), None);
    }

    #[test]
    fn rule_targets_directory_common_shapes() {
        // Trailing ** is the canonical form.
        assert!(rule_targets_directory(
            "docs/decisions/**",
            "docs/decisions"
        ));
        // Wildcard leaf globs must still target the parent directory.
        assert!(rule_targets_directory(
            "docs/decisions/*.md",
            "docs/decisions"
        ));
        assert!(rule_targets_directory(
            "docs/decisions/[0-9]*.md",
            "docs/decisions"
        ));
        assert!(rule_targets_directory(
            "docs/decisions/?*",
            "docs/decisions"
        ));
        // Middle-path wildcard resolves its literal prefix only.
        assert!(rule_targets_directory("docs/*/decisions/**", "docs"));
        // Disjoint directories must not match.
        assert!(!rule_targets_directory("docs/guides/**", "docs/decisions"));
        // Leading wildcard has no literal prefix at all.
        assert!(!rule_targets_directory("**/SKILL.md", "docs/decisions"));
    }

    #[test]
    fn scaffold_rejects_non_md_extension() {
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["note".into()],
            },
            ..Config::default()
        };
        let err = scaffold(
            Path::new("/tmp"),
            ScaffoldSpec {
                kind: Kind::new("note"),
                title: "x".into(),
                id: Some("note-x".into()),
                path: Some(PathBuf::from("misc/hello.txt")),
            },
            &empty_graph(),
            &config,
            false,
            false,
        )
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains(".md"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }
}
