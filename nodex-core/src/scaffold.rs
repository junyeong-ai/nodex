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

use crate::config::{Config, CrossFieldSpec, FieldType, WhenPredicate, parse_when};
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
#[derive(Debug, Clone, Serialize)]
pub struct ScaffoldResult {
    pub id: String,
    #[serde(serialize_with = "serialize_path_forward")]
    pub path: PathBuf,
    pub content: String,
    pub written: bool,
}

fn serialize_path_forward<S: serde::Serializer>(p: &Path, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&p.to_string_lossy().replace('\\', "/"))
}

/// Scaffold a new node.
///
/// When `write` is `true`, the file is written atomically (temp file +
/// rename) and `ScaffoldResult::written` is set. Existing files are
/// rejected unless `force` is set.
pub fn scaffold(
    root: &Path,
    spec: ScaffoldSpec,
    graph: &Graph,
    config: &Config,
    write: bool,
    force: bool,
) -> Result<ScaffoldResult> {
    // 1. Validate kind against config.
    if !config.kinds.allowed.contains(&spec.kind.as_str().to_string()) {
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
    let abs_path = root.join(&rel_path);

    // 3. Resolve id (explicit override or infer via existing identity rules).
    let id = spec.id.clone().unwrap_or_else(|| infer_id(&rel_path, &spec.kind, config));
    if graph.nodes().contains_key(&id) {
        return Err(Error::DuplicateId {
            id: id.clone(),
            first: graph.nodes()[&id].path.clone(),
            second: rel_path.clone(),
        });
    }

    // 4. Reject existing file unless --force.
    if abs_path.exists() && !force {
        return Err(Error::Config(format!(
            "file already exists at {}; pass --force to overwrite",
            abs_path.display()
        )));
    }

    // 5. Build frontmatter YAML and body.
    let content = render_document(&id, &spec, &rel_path, config);

    // 6. Write atomically (or skip in dry-run).
    let written = if write {
        write_atomic(&abs_path, &content)?;
        true
    } else {
        false
    };

    Ok(ScaffoldResult {
        id,
        path: rel_path,
        content,
        written,
    })
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
/// for the target directory, use `NNNN-<slug>` with the next available
/// number; otherwise plain `<slug>`.
fn next_filename_stem(dir: &Path, title: &str, graph: &Graph, config: &Config) -> String {
    let slug = slugify(title);
    let dir_str = dir.to_string_lossy().replace('\\', "/");

    for rule in &config.rules.naming {
        if !rule.sequential {
            continue;
        }
        let Ok(glob) = Glob::new(&rule.glob) else { continue };
        let matcher = glob.compile_matcher();
        // Only consider rules whose glob points into our directory.
        let probe = format!("{dir_str}/0000-test.md");
        if !matcher.is_match(&probe) {
            continue;
        }
        let (next, width) = next_sequence(graph, &matcher, &rule.pattern);
        let padded = format!("{:0>width$}", next, width = width);
        return format!("{padded}-{slug}");
    }

    slug
}

/// Find the next sequence number for files matching `matcher`, preserving
/// the digit width of existing filenames.
fn next_sequence(graph: &Graph, matcher: &globset::GlobMatcher, pattern: &str) -> (u64, usize) {
    let digit_re = Regex::new(r"^(\d+)").expect("static regex compiles");
    let pattern_re = Regex::new(pattern).ok();
    let mut max_seen: u64 = 0;
    let mut width: usize = 4; // sensible default for ADR-style numbering

    for node in graph.nodes().values() {
        let path_str = node.path.to_string_lossy().replace('\\', "/");
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

fn render_document(id: &str, spec: &ScaffoldSpec, path: &Path, config: &Config) -> String {
    let kind = spec.kind.as_str();
    let ov = config.schema_override_for(kind);
    let required: Vec<String> = config.required_for(kind).to_vec();
    let today = Local::now().date_naive().to_string();

    // Start with the canonical ordering: id, title, kind, status, then
    // other required fields in declaration order.
    let mut lines: Vec<String> = Vec::new();
    let mut emit = |key: &str, value: String| {
        lines.push(format!("{key}: {value}"));
    };

    emit("id", id.to_string());
    emit("title", format!("{:?}", spec.title));
    emit("kind", kind.to_string());

    let default_status = default_status_value(kind, config);
    emit("status", default_status.clone());

    // Non-core required fields in declaration order.
    let mut seen: std::collections::BTreeSet<&str> =
        ["id", "title", "kind", "status"].into_iter().collect();
    for field in &required {
        if seen.contains(field.as_str()) {
            continue;
        }
        let value = default_for_field(field, kind, config, &today);
        emit(field, value);
        seen.insert(field.as_str());
    }

    // Honour cross_field: when predicate matches the default status, emit
    // `require` field even if not in the required list, so scaffolded
    // content is immediately valid.
    if let Some(ov) = ov {
        let predicates = current_predicates(ov, &default_status);
        for cf in &ov.cross_field {
            let Ok(predicate) = parse_when(&cf.when) else { continue };
            if predicates.iter().any(|p| p == &predicate) && !seen.contains(cf.require.as_str()) {
                let value = default_for_field(&cf.require, kind, config, &today);
                emit(&cf.require, value);
                seen.insert(&cf.require);
            }
        }
    }

    let frontmatter = lines.join("\n");
    let stem_title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Document");
    let body_heading = if spec.title.is_empty() {
        stem_title.to_string()
    } else {
        spec.title.clone()
    };

    format!("---\n{frontmatter}\n---\n\n# {body_heading}\n")
}

fn default_status_value(kind: &str, config: &Config) -> String {
    if let Some(ov) = config.schema_override_for(kind)
        && let Some(allowed) = ov.enums.get("status")
        && let Some(first) = allowed.first()
    {
        return first.clone();
    }
    if let Some(first) = config.statuses.allowed.first() {
        return first.clone();
    }
    "draft".to_string()
}

fn default_for_field(field: &str, kind: &str, config: &Config, today: &str) -> String {
    let ov = config.schema_override_for(kind);

    // Enum default: first allowed value
    if let Some(ov) = ov
        && let Some(allowed) = ov.enums.get(field)
        && let Some(first) = allowed.first()
    {
        return first.clone();
    }

    // Type-based default
    if let Some(ov) = ov
        && let Some(ty) = ov.types.get(field)
    {
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

/// Collect every predicate that currently holds for a scaffolded node
/// (based on its default status). Used to decide which cross_field
/// `require` entries to emit up-front.
fn current_predicates(
    ov: &crate::config::SchemaOverride,
    default_status: &str,
) -> Vec<WhenPredicate> {
    let mut out = Vec::new();
    for cf in &ov.cross_field {
        if let Ok(predicate) = parse_when(&cf.when) {
            match &predicate {
                WhenPredicate::Equals { field, value }
                    if field == "status" && value == default_status =>
                {
                    out.push(predicate);
                }
                _ => {}
            }
            let _ = CrossFieldSpec {
                when: String::new(),
                require: String::new(),
            }; // keep import
        }
    }
    out
}

// ─── atomic write ───────────────────────────────────────────────────

fn write_atomic(target: &Path, content: &str) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = target.with_extension("md.tmp");
    std::fs::write(&tmp, content).map_err(|e| Error::Io {
        path: tmp.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp, target).map_err(|e| Error::Io {
        path: target.to_path_buf(),
        source: e,
    })?;
    Ok(())
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
            },
            ..Config::default()
        }
    }

    fn empty_graph() -> Graph {
        Graph::new(IndexMap::new(), vec![])
    }

    #[test]
    fn infers_sequential_filename_from_empty_graph() {
        let result = scaffold(
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
            result.path.to_string_lossy(),
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
                orphan_ok: true,
                attrs: Default::default(),
            },
        );
        let graph = Graph::new(map, vec![]);
        let result = scaffold(
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
            result.path.to_string_lossy(),
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
        let result = scaffold(
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
}
