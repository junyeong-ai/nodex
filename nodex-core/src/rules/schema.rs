use chrono::NaiveDate;
use serde_json::Value;

use crate::config::{Config, FieldType, WhenPredicate, parse_when};
use crate::model::{Graph, Node};

use super::{Rule, Severity, Violation};

/// Check that nodes have all required frontmatter fields.
pub struct RequiredField;

impl Rule for RequiredField {
    fn id(&self) -> &str {
        "required_field"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let mut violations = Vec::new();

        for node in graph.nodes().values() {
            let required = config.required_for(node.kind.as_str());

            for field in required {
                if is_field_missing(node, field) {
                    violations.push(Violation {
                        rule_id: self.id().to_string(),
                        severity: self.severity(),
                        node_id: Some(node.id.clone()),
                        path: Some(node.path.to_string_lossy().to_string()),
                        message: format!("missing required field: {field}"),
                    });
                }
            }
        }

        violations
    }
}

/// Check that `attrs` field values conform to configured types.
///
/// Built-in fields (`status`, `created`, etc.) are strongly typed in `Node`
/// so the parser catches their type errors. This rule targets
/// project-specific frontmatter keys that land in `Node::attrs` as
/// `serde_json::Value`.
pub struct FieldTypeRule;

impl Rule for FieldTypeRule {
    fn id(&self) -> &str {
        "field_type"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let mut violations = Vec::new();

        for node in graph.nodes().values() {
            let types = config.types_for(node.kind.as_str());
            if types.is_empty() {
                continue;
            }

            for (field, expected) in &types {
                let Some(value) = node.attrs.get(field) else {
                    continue; // missing fields belong to `required_field`
                };
                if let Some(msg) = validate_type(value, *expected) {
                    violations.push(Violation {
                        rule_id: self.id().to_string(),
                        severity: self.severity(),
                        node_id: Some(node.id.clone()),
                        path: Some(node.path.to_string_lossy().to_string()),
                        message: format!("field {field:?}: {msg}"),
                    });
                }
            }
        }

        violations
    }
}

/// Check that field values are members of the configured enumeration.
///
/// Handles project-specific fields declared under
/// `schema.enums` / `schema.overrides.enums` AND the two built-in
/// scalar fields (`kind`, `status`) which are implicitly constrained
/// by the global `kinds.allowed` / `statuses.allowed`. An override
/// enum on `kind` or `status` supersedes the implicit backstop — the
/// override is always a subset of the global (`Config::validate`
/// enforces that), so the stricter rule wins without drift.
pub struct FieldEnumRule;

impl Rule for FieldEnumRule {
    fn id(&self) -> &str {
        "field_enum"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let mut violations = Vec::new();

        for node in graph.nodes().values() {
            let mut enums = config.enums_for(node.kind.as_str());

            // Back-fill kind/status with the global allowed lists when no
            // explicit enum was declared for them. Declaring `kinds.allowed`
            // in `nodex.toml` must mean "these and only these kinds are
            // valid"; silently accepting out-of-vocabulary kinds/statuses
            // because the user didn't also write `schema.enums.kind = [...]`
            // would defeat the purpose of the allowed list.
            enums
                .entry("kind".to_string())
                .or_insert_with(|| config.kinds.allowed.clone());
            enums
                .entry("status".to_string())
                .or_insert_with(|| config.statuses.allowed.clone());

            for (field, allowed) in &enums {
                let actual = read_field_as_string(node, field);
                let Some(actual) = actual else {
                    continue; // missing fields belong to `required_field`
                };
                if !allowed.iter().any(|v| v == &actual) {
                    violations.push(Violation {
                        rule_id: self.id().to_string(),
                        severity: self.severity(),
                        node_id: Some(node.id.clone()),
                        path: Some(node.path.to_string_lossy().to_string()),
                        message: format!(
                            "field {field:?} has value {actual:?}; expected one of {allowed:?}"
                        ),
                    });
                }
            }
        }

        violations
    }
}

/// Check cross-field conditional requirements.
///
/// "When predicate holds, `require` field must be present."
pub struct CrossFieldRule;

impl Rule for CrossFieldRule {
    fn id(&self) -> &str {
        "cross_field"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let mut violations = Vec::new();

        for node in graph.nodes().values() {
            let cross_fields = config.cross_field_for(node.kind.as_str());
            if cross_fields.is_empty() {
                continue;
            }

            for cf in &cross_fields {
                let Ok(predicate) = parse_when(&cf.when) else {
                    continue; // already rejected by Config::validate
                };
                if !predicate_matches_node(&predicate, node) {
                    continue;
                }
                if is_field_missing(node, &cf.require) {
                    violations.push(Violation {
                        rule_id: self.id().to_string(),
                        severity: self.severity(),
                        node_id: Some(node.id.clone()),
                        path: Some(node.path.to_string_lossy().to_string()),
                        message: format!("when {}, field {:?} is required", cf.when, cf.require),
                    });
                }
            }
        }

        violations
    }
}

// ─── helpers ────────────────────────────────────────────────────────────

/// Return true when `field` has no value on the node. Handles both
/// built-in scalar/vector fields and `attrs`.
fn is_field_missing(node: &Node, field: &str) -> bool {
    match field {
        "id" => node.id.is_empty(),
        "title" => node.title.is_empty(),
        "kind" => node.kind.as_str().is_empty(),
        "status" => node.status.as_str().is_empty(),
        "created" => node.created.is_none(),
        "updated" => node.updated.is_none(),
        "reviewed" => node.reviewed.is_none(),
        "owner" => node.owner.is_none(),
        "superseded_by" => node.superseded_by.is_none(),
        "supersedes" => node.supersedes.is_empty(),
        "implements" => node.implements.is_empty(),
        "related" => node.related.is_empty(),
        "tags" => node.tags.is_empty(),
        other => match node.attrs.get(other) {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            _ => false,
        },
    }
}

/// Read a field's value as a `String` for enum comparison. Returns
/// `None` when the field is absent or cannot be represented as a scalar
/// string (arrays, objects, etc. are not enum candidates).
fn read_field_as_string(node: &Node, field: &str) -> Option<String> {
    match field {
        "id" => none_if_empty(&node.id),
        "title" => none_if_empty(&node.title),
        "kind" => none_if_empty(node.kind.as_str()),
        "status" => none_if_empty(node.status.as_str()),
        "owner" => node.owner.clone(),
        "superseded_by" => node.superseded_by.clone(),
        other => match node.attrs.get(other)? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        },
    }
}

fn none_if_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Validate a JSON value against an expected field type. Returns a
/// human-readable error message on mismatch, `None` on success.
///
/// Written as `match expected { Variant => match value { ... } }` so
/// that adding a new `FieldType` variant is a compile error here —
/// silent acceptance of unknown types would defeat the validation.
fn validate_type(value: &Value, expected: FieldType) -> Option<String> {
    match expected {
        FieldType::String => match value {
            Value::String(_) => None,
            other => Some(format!("expected string, got {}", describe_value(other))),
        },
        FieldType::Integer => match value {
            Value::Number(n) if n.is_i64() || n.is_u64() => None,
            other => Some(format!("expected integer, got {}", describe_value(other))),
        },
        FieldType::Bool => match value {
            Value::Bool(_) => None,
            other => Some(format!("expected bool, got {}", describe_value(other))),
        },
        FieldType::Date => match value {
            Value::String(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .map(|_| None)
                .unwrap_or_else(|| Some(format!("invalid date {s:?}, expected YYYY-MM-DD"))),
            other => Some(format!(
                "expected date (YYYY-MM-DD), got {}",
                describe_value(other)
            )),
        },
    }
}

fn describe_value(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "float",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Evaluate whether a `when` predicate holds for a given node.
///
/// Public so scaffold can evaluate cross_field predicates against a
/// synthetic default node without reimplementing the predicate logic.
pub fn predicate_matches_node(predicate: &WhenPredicate, node: &Node) -> bool {
    match predicate {
        WhenPredicate::Equals { field, value } => read_field_as_string(node, field)
            .as_deref()
            .map(|actual| actual == value)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        CrossFieldSpec, FieldType, KindsConfig, SchemaConfig, SchemaOverride, StatusesConfig,
    };
    use crate::model::{Kind, Status};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            kinds: KindsConfig {
                allowed: vec!["adr".to_string(), "guide".to_string()],
            },
            statuses: StatusesConfig {
                allowed: vec![
                    "draft".to_string(),
                    "active".to_string(),
                    "superseded".to_string(),
                ],
                terminal: vec!["superseded".to_string()],
            },
            schema: SchemaConfig {
                required: vec!["id".to_string(), "title".to_string()],
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".to_string()],
                    required: vec!["id".to_string(), "title".to_string(), "status".to_string()],
                    types: [("decision_date".to_string(), FieldType::Date)]
                        .into_iter()
                        .collect(),
                    enums: [(
                        "status".to_string(),
                        vec![
                            "draft".to_string(),
                            "active".to_string(),
                            "superseded".to_string(),
                        ],
                    )]
                    .into_iter()
                    .collect(),
                    cross_field: vec![CrossFieldSpec {
                        when: "status=superseded".to_string(),
                        require: "superseded_by".to_string(),
                    }],
                }],
                ..Default::default()
            },
            ..Config::default()
        }
    }

    fn make_node(id: &str, kind: &str, status: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.to_string(),
            kind: Kind::new(kind),
            status: Status::new(status),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
        }
    }

    fn make_graph(nodes: Vec<Node>) -> Graph {
        use indexmap::IndexMap;
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![])
    }

    #[test]
    fn field_types_accepts_valid_date() {
        let mut node = make_node("adr-1", "adr", "active");
        node.attrs.insert(
            "decision_date".to_string(),
            Value::String("2026-04-19".to_string()),
        );
        let graph = make_graph(vec![node]);
        let v = FieldTypeRule.check(&graph, &test_config());
        assert!(v.is_empty());
    }

    #[test]
    fn field_types_rejects_invalid_date() {
        let mut node = make_node("adr-1", "adr", "active");
        node.attrs.insert(
            "decision_date".to_string(),
            Value::String("yesterday".to_string()),
        );
        let graph = make_graph(vec![node]);
        let v = FieldTypeRule.check(&graph, &test_config());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "field_type");
    }

    #[test]
    fn field_types_skip_missing_field() {
        let node = make_node("adr-1", "adr", "active");
        let graph = make_graph(vec![node]);
        let v = FieldTypeRule.check(&graph, &test_config());
        assert!(v.is_empty()); // required_field handles missing
    }

    #[test]
    fn field_enums_rejects_typo() {
        let node = make_node("adr-1", "adr", "actives");
        let graph = make_graph(vec![node]);
        let v = FieldEnumRule.check(&graph, &test_config());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "field_enum");
    }

    #[test]
    fn field_enums_accepts_valid() {
        let node = make_node("adr-1", "adr", "active");
        let graph = make_graph(vec![node]);
        let v = FieldEnumRule.check(&graph, &test_config());
        assert!(v.is_empty());
    }

    #[test]
    fn field_enums_fall_back_to_global_allowed() {
        // A "guide" doc has no per-kind enum override, but the global
        // `statuses.allowed` still constrains its `status` field —
        // declaring an allowed list has to mean "these and only these,
        // everywhere," otherwise the list is a lie.
        let node = make_node("guide-1", "guide", "actives");
        let graph = make_graph(vec![node]);
        let v = FieldEnumRule.check(&graph, &test_config());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "field_enum");
        assert!(v[0].message.contains("\"actives\""));
    }

    #[test]
    fn field_enums_rejects_unknown_kind() {
        // Symmetric to the status check: a kind value outside
        // `kinds.allowed` is flagged even when no explicit enum
        // override on `kind` was declared.
        let node = make_node("x-1", "unlisted-kind", "active");
        let graph = make_graph(vec![node]);
        let v = FieldEnumRule.check(&graph, &test_config());
        assert!(v.iter().any(|v| v.message.contains("\"unlisted-kind\"")));
    }

    #[test]
    fn cross_field_fires_when_predicate_matches() {
        let node = make_node("adr-1", "adr", "superseded");
        // missing superseded_by
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule.check(&graph, &test_config());
        assert_eq!(v.len(), 1);
        assert!(v[0].message.contains("superseded_by"));
    }

    #[test]
    fn cross_field_silent_when_predicate_false() {
        let node = make_node("adr-1", "adr", "draft");
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule.check(&graph, &test_config());
        assert!(v.is_empty());
    }

    #[test]
    fn cross_field_silent_when_required_field_present() {
        let mut node = make_node("adr-1", "adr", "superseded");
        node.superseded_by = Some("adr-2".to_string());
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule.check(&graph, &test_config());
        assert!(v.is_empty());
    }

    #[test]
    fn type_and_cross_field_rules_early_return_on_empty_override() {
        // `FieldTypeRule` and `CrossFieldRule` are purely config-driven
        // — no declared constraints, no violations. `FieldEnumRule` is
        // now stricter: even with no override, `kind` and `status` are
        // validated against the global allowed lists, so it is no
        // longer part of this "no constraints configured" test.
        let mut config = test_config();
        config.schema.overrides[0].types.clear();
        config.schema.overrides[0].enums.clear();
        config.schema.overrides[0].cross_field.clear();
        // Use a valid status so the global-backstop enum check stays silent.
        let node = make_node("adr-1", "adr", "active");
        let graph = make_graph(vec![node]);
        assert!(FieldTypeRule.check(&graph, &config).is_empty());
        assert!(CrossFieldRule.check(&graph, &config).is_empty());
    }
}
