use chrono::NaiveDate;
use serde_json::{Map, Value, json};

use crate::config::{FieldType, SchemaMode, WhenPredicate, parse_when};
use crate::model::Node;

use super::detail::ValueKind;
use super::{
    Rule, RuleContext, RuleRun, Severity, SubjectUnit, Violation, ViolationDetails,
    detail::Evidence,
};

/// Check that nodes have all required frontmatter fields.
pub struct RequiredFieldRule;

impl Rule for RequiredFieldRule {
    fn id(&self) -> &str {
        "required_field"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Every required frontmatter field (global plus per-kind override) must be set"
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();
        let mut subjects = 0;

        for node in graph.nodes().values() {
            let required = config.required_for(node.kind.as_str());
            if required.is_empty() {
                continue;
            }
            subjects += 1;

            for field in &required {
                if is_field_missing(node, field) {
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        Some(node.id.clone()),
                        Some(crate::path_guard::forward_string(&node.path)),
                        ViolationDetails::RequiredField {
                            field: field.to_string(),
                        },
                    ));
                }
            }
        }

        RuleRun::new(subjects, violations)
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

    fn description(&self) -> &str {
        "Typed fields must parse as their declared type (date / integer / bool)"
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();
        let mut subjects = 0;

        for node in graph.nodes().values() {
            let types = config.types_for(node.kind.as_str());
            if types.is_empty() {
                continue;
            }
            subjects += 1;

            for (field, expected) in &types {
                let Some(value) = node.attrs.get(field) else {
                    continue; // missing fields belong to `required_field`
                };
                if let Some(mismatch) = validate_type(value, *expected) {
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        Some(node.id.clone()),
                        Some(crate::path_guard::forward_string(&node.path)),
                        ViolationDetails::FieldType {
                            field: field.clone(),
                            expected: *expected,
                            found: Evidence(mismatch.found),
                            invalid_date: Evidence(mismatch.invalid_date),
                        },
                    ));
                }
            }
        }

        RuleRun::new(subjects, violations)
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

    fn description(&self) -> &str {
        "Enum-constrained fields must hold a value from their declared allowed set"
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();
        let mut subjects = 0;

        for node in graph.nodes().values() {
            // The effective enum view — declared enums plus the implicit
            // kind/status vocabularies — is one seam (`config/views.rs`),
            // shared with the load-time predicate-value check so the value
            // a field may hold is identical at load and check time.
            let enums = config.effective_enums_for(node.kind.as_str());
            if enums.is_empty() {
                continue;
            }
            subjects += 1;

            for (field, allowed) in &enums {
                let actual = read_field_as_string(node, field);
                let Some(actual) = actual else {
                    continue; // missing fields belong to `required_field`
                };
                if !allowed.iter().any(|v| v == &actual) {
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        Some(node.id.clone()),
                        Some(crate::path_guard::forward_string(&node.path)),
                        ViolationDetails::FieldEnum {
                            field: field.clone(),
                            found: Evidence(actual),
                            allowed: allowed.clone(),
                        },
                    ));
                }
            }
        }

        RuleRun::new(subjects, violations)
    }
}

/// Reject frontmatter keys that are neither built-in nor declared.
///
/// Inert under [`SchemaMode::Lenient`] (the default): undeclared
/// keys are preserved on `Node::attrs` untouched, matching the
/// project's longstanding "passthrough is data" stance. Under
/// [`SchemaMode::Strict`] every entry in `attrs` is checked against
/// [`crate::config::Config::declared_fields_for`]; an unrecognised key
/// fires one `unknown_field` violation, surfacing typos like
/// `relatd:` or `Implementss:` that would otherwise vanish silently.
pub struct UnknownFieldRule;

impl Rule for UnknownFieldRule {
    fn id(&self) -> &str {
        "unknown_field"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Strict mode: any frontmatter key not declared in built-ins or schema is rejected"
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (graph, config) = (ctx.graph, ctx.config);
        if config.schema.mode != SchemaMode::Strict {
            return RuleRun::clean(0);
        }
        let mut violations = Vec::new();
        let mut subjects = 0;

        for node in graph.nodes().values() {
            if node.attrs.is_empty() {
                continue;
            }
            subjects += 1;
            let declared = config.declared_fields_for(node.kind.as_str());
            for key in node.attrs.keys() {
                if !declared.contains(key) {
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        Some(node.id.clone()),
                        Some(crate::path_guard::forward_string(&node.path)),
                        ViolationDetails::UnknownField { field: key.clone() },
                    ));
                }
            }
        }

        RuleRun::new(subjects, violations)
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

    fn description(&self) -> &str {
        "Cross-field predicates (`when X require Y`) must be honoured"
    }

    /// Surface the relational contract JSON Schema cannot express, so the
    /// rules manifest is the machine-readable home for `cross_field` the
    /// way the schema manifest is for structural constraints. Mirrors the
    /// `export_enums` shape: `global` is the `[schema]` predicate list;
    /// `per_kind` carries the merged `cross_field_for` view (global ∪
    /// override) for each override-covered kind. A consumer reads the
    /// per-kind list when present, else `global`.
    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        let mut params = Map::new();
        params.insert("global".into(), json!(config.schema.cross_field));

        let per_kind: Map<String, Value> = config
            .schema
            .overrides
            .iter()
            .flat_map(|ov| ov.kinds.iter())
            .map(|kind| (kind.clone(), json!(config.cross_field_for(kind))))
            .collect();
        if !per_kind.is_empty() {
            params.insert("per_kind".into(), Value::Object(per_kind));
        }
        params
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();
        let mut subjects = 0;

        for node in graph.nodes().values() {
            let cross_fields = config.cross_field_for(node.kind.as_str());
            if cross_fields.is_empty() {
                continue;
            }
            subjects += 1;

            for cf in &cross_fields {
                // `Config::load` parses every `cross_field.when`
                // (`validate_cross_field_syntax`), so the predicate
                // always parses here.
                let predicate = parse_when(&cf.when).expect("validated by Config::load");
                if !predicate_matches_node(&predicate, node) {
                    continue;
                }
                if is_field_missing(node, &cf.require) {
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        Some(node.id.clone()),
                        Some(crate::path_guard::forward_string(&node.path)),
                        ViolationDetails::CrossField {
                            when: cf.when.clone(),
                            require: cf.require.clone(),
                        },
                    ));
                }
            }
        }

        RuleRun::new(subjects, violations)
    }
}

/// Require inferrable built-ins to be authored, not inferred.
///
/// Opt-in via `schema.require_explicit`. The parser resolves
/// id/title/kind/status for every document (so they can never be
/// `required`), but a project may want to *forbid* relying on that
/// fallback — e.g. an unstated lifecycle `status`. This rule reds a
/// named field that fell back to inference, while the graph still holds
/// the valid inferred value (construction never breaks). Registered only
/// when `require_explicit` is non-empty.
pub struct ExplicitFieldRule;

impl Rule for ExplicitFieldRule {
    fn id(&self) -> &str {
        "explicit_field"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Fields in schema.require_explicit must be authored in frontmatter, not inferred"
    }

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        let mut params = Map::new();
        params.insert(
            "require_explicit".into(),
            json!(config.schema.require_explicit),
        );
        params
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let required = &ctx.config.schema.require_explicit;
        let mut violations = Vec::new();
        let mut subjects = 0;
        for node in ctx.graph.nodes().values() {
            subjects += 1;
            // `node.inferred_fields` already excludes a field that was
            // authored-but-malformed (it carries a `parse_issue` that
            // `field_parse` reds), so a hit here is genuinely "not
            // authored" — the message never lies about authorship.
            for field in required {
                if node.inferred_fields.iter().any(|f| f == field) {
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        Some(node.id.clone()),
                        Some(crate::path_guard::forward_string(&node.path)),
                        ViolationDetails::ExplicitField {
                            field: field.clone(),
                        },
                    ));
                }
            }
        }
        RuleRun::new(subjects, violations)
    }
}

// ─── helpers ────────────────────────────────────────────────────────────

/// Return true when `field` has no value on the node. Every built-in
/// frontmatter field has an explicit arm — the `other` arm only
/// reaches project-specific `attrs` keys. A missing built-in arm
/// would silently fall into the `attrs` lookup, which returns `None`
/// for struct-backed fields and therefore *always* reports them as
/// missing regardless of the actual value. `pub(crate)` because the
/// lifecycle `set` write-seam guard evaluates the same semantics over
/// the same parsed node, so the guard and this rule can never disagree
/// about what "missing" means.
pub(crate) fn is_field_missing(node: &Node, field: &str) -> bool {
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
        "covers" => node.covers.is_empty(),
        // `orphan_ok` is a `bool` — `false` is a meaningful value, not
        // an absence, so the field is structurally always present.
        // Returning `false` lets `cross_field.require = "orphan_ok"`
        // pass once the doc declares the flag either way.
        "orphan_ok" => false,
        other => match node.attrs.get(other) {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            _ => false,
        },
    }
}

/// Read a field's value as a `String` for predicate evaluation.
/// Returns `None` when the field is absent or empty. Collection
/// fields return a comma-joined string when non-empty so that
/// `exists` / `not_exists` predicates work correctly.
fn read_field_as_string(node: &Node, field: &str) -> Option<String> {
    match field {
        "id" => none_if_empty(&node.id),
        "title" => none_if_empty(&node.title),
        "kind" => none_if_empty(node.kind.as_str()),
        "status" => none_if_empty(node.status.as_str()),
        // The node's filesystem path in canonical forward-slash form —
        // the exact string `--fields path` projects (query/mod.rs), so a
        // `--where path=<value>` predicate filters on the same value the
        // listing emits. `path` is a reserved structural field
        // (`config::is_reserved_structural_field`): config can never
        // declare it in types / enums / required / cross_field, so neither
        // `field_enum` nor a `cross_field` predicate ever reaches this arm
        // — only `--where` does, and it builds equality only (hence no
        // `is_field_missing` arm).
        "path" => Some(crate::path_guard::forward_string(&node.path)),
        "owner" => node.owner.clone(),
        "superseded_by" => node.superseded_by.clone(),
        // Date built-ins format as the canonical YAML date string so
        // equality predicates like `when = "reviewed=2026-01-01"`
        // round-trip against user-authored values.
        "created" => node.created.map(|d| d.format("%Y-%m-%d").to_string()),
        "updated" => node.updated.map(|d| d.format("%Y-%m-%d").to_string()),
        "reviewed" => node.reviewed.map(|d| d.format("%Y-%m-%d").to_string()),
        "orphan_ok" => Some(node.orphan_ok.to_string()),
        "tags" => {
            if node.tags.is_empty() {
                None
            } else {
                Some(node.tags.join(","))
            }
        }
        "supersedes" => {
            if node.supersedes.is_empty() {
                None
            } else {
                Some(node.supersedes.join(","))
            }
        }
        "implements" => {
            if node.implements.is_empty() {
                None
            } else {
                Some(node.implements.join(","))
            }
        }
        "related" => {
            if node.related.is_empty() {
                None
            } else {
                Some(node.related.join(","))
            }
        }
        "covers" => {
            if node.covers.is_empty() {
                None
            } else {
                Some(node.covers.join(","))
            }
        }
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

/// The typed payload of a `field_type` mismatch: the value's actual
/// runtime [`ValueKind`], plus the offending string when the value is a
/// string that does not parse as `YYYY-MM-DD`. The caller pairs it with
/// the field name and declared [`FieldType`] to build
/// [`ViolationDetails::FieldType`]; the human message is rendered from
/// that single source.
pub(crate) struct TypeMismatch {
    pub found: ValueKind,
    pub invalid_date: Option<String>,
}

/// Validate a JSON value against an expected field type. Returns the
/// structured mismatch on failure, `None` on success.
///
/// Written as `match expected { Variant => match value { ... } }` so
/// that adding a new `FieldType` variant is a compile error here —
/// silent acceptance of unknown types would defeat the validation.
fn validate_type(value: &Value, expected: FieldType) -> Option<TypeMismatch> {
    let mismatch = |invalid_date| TypeMismatch {
        found: ValueKind::of(value),
        invalid_date,
    };
    match expected {
        FieldType::String => match value {
            Value::String(_) => None,
            _ => Some(mismatch(None)),
        },
        FieldType::Integer => match value {
            Value::Number(n) if n.is_i64() || n.is_u64() => None,
            _ => Some(mismatch(None)),
        },
        FieldType::Bool => match value {
            Value::Bool(_) => None,
            _ => Some(mismatch(None)),
        },
        FieldType::Date => match value {
            Value::String(s) => match NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                Ok(_) => None,
                Err(_) => Some(mismatch(Some(s.clone()))),
            },
            _ => Some(mismatch(None)),
        },
    }
}

/// Evaluate whether a `when` predicate holds for a given node.
///
/// Public so scaffold can evaluate cross_field predicates against the
/// node it reparses from the frontmatter it has written so far —
/// reusing this exact predicate logic, so scaffold and `check` agree by
/// construction about which predicates fire.
pub fn predicate_matches_node(predicate: &WhenPredicate, node: &Node) -> bool {
    match predicate {
        WhenPredicate::Equals { field, value } => read_field_as_string(node, field)
            .as_deref()
            .map(|actual| actual == value.as_str())
            .unwrap_or(false),
        WhenPredicate::In { field, values } => read_field_as_string(node, field)
            .as_deref()
            .map(|actual| values.iter().any(|v| v == actual))
            .unwrap_or(false),
        WhenPredicate::Exists { field } => read_field_as_string(node, field).is_some(),
        WhenPredicate::NotExists { field } => read_field_as_string(node, field).is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config, CrossFieldSpec, FieldType, KindsConfig, SchemaConfig, SchemaOverride,
        StatusesConfig,
    };
    use crate::model::{Graph, Kind, Status};
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
                initial: None,
            },
            schema: SchemaConfig {
                required: vec!["created".to_string()],
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".to_string()],
                    required: vec!["decision_date".to_string()],
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

    #[test]
    fn exchanging_one_failing_date_value_for_another_introduces_nothing() {
        // `invalid_date` is present exactly when the failing value is a
        // non-date string, so its presence is derived from the value the
        // finding is not about. Left around the evidence rather than inside
        // it, swapping a malformed date string for a number read as a
        // finding introduced, while swapping a number for a list did not.
        let cause = |value: serde_json::Value| {
            let mut types = BTreeMap::new();
            types.insert("due".to_string(), FieldType::Date);
            let config = Config {
                schema: SchemaConfig {
                    types,
                    ..Default::default()
                },
                ..test_config()
            };
            let mut node = make_node("a", "generic", "active");
            node.attrs.insert("due".to_string(), value);
            let graph = make_graph(vec![node]);
            FieldTypeRule
                .check(&super::super::test_ctx(&graph, &config))
                .violations
                .into_iter()
                .map(|v| v.details)
                .collect::<Vec<_>>()
        };
        let malformed = cause(serde_json::json!("2024-99-99"));
        let number = cause(serde_json::json!(42));
        let list = cause(serde_json::json!([1, 2]));
        assert_eq!(malformed.len(), 1);
        assert_eq!(malformed, number, "a failing value is not the finding");
        assert_eq!(number, list);
    }

    #[test]
    fn cross_field_params_expose_global_and_merged_per_kind() {
        // The relational contract lives in the rules manifest: `global`
        // is the [schema] list, `per_kind` the merged (global ∪ override)
        // view for each override-covered kind. test_config has an empty
        // global and one override predicate on `adr`.
        let mut config = test_config();
        config.schema.cross_field = vec![CrossFieldSpec {
            when: "owner exists".to_string(),
            require: "reviewed".to_string(),
        }];
        let params = CrossFieldRule.params(&config);

        let global = params["global"].as_array().unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0]["when"], "owner exists");

        // adr's merged view = global predicate + the override's own.
        let adr = params["per_kind"]["adr"].as_array().unwrap();
        assert_eq!(adr.len(), 2);
        assert!(adr.iter().any(|p| p["require"] == "superseded_by"));
        assert!(adr.iter().any(|p| p["require"] == "reviewed"));

        // A kind with no override is absent from per_kind (use `global`).
        assert!(params["per_kind"].get("guide").is_none());

        // No overrides → no per_kind key at all.
        let mut plain = test_config();
        plain.schema.overrides.clear();
        assert!(CrossFieldRule.params(&plain).get("per_kind").is_none());
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
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn make_graph(nodes: Vec<Node>) -> Graph {
        use indexmap::IndexMap;
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(
            map,
            vec![],
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    #[test]
    fn field_types_accepts_valid_date() {
        let mut node = make_node("adr-1", "adr", "active");
        node.attrs.insert(
            "decision_date".to_string(),
            Value::String("2026-04-19".to_string()),
        );
        let graph = make_graph(vec![node]);
        let v = FieldTypeRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
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
        let v = FieldTypeRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "field_type");
    }

    #[test]
    fn field_types_skip_missing_field() {
        let node = make_node("adr-1", "adr", "active");
        let graph = make_graph(vec![node]);
        let v = FieldTypeRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
        assert!(v.is_empty()); // required_field handles missing
    }

    #[test]
    fn field_enums_rejects_typo() {
        let node = make_node("adr-1", "adr", "actives");
        let graph = make_graph(vec![node]);
        let v = FieldEnumRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "field_enum");
    }

    #[test]
    fn field_enums_accepts_valid() {
        let node = make_node("adr-1", "adr", "active");
        let graph = make_graph(vec![node]);
        let v = FieldEnumRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
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
        let v = FieldEnumRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
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
        let v = FieldEnumRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
        assert!(v.iter().any(|v| v.message.contains("\"unlisted-kind\"")));
    }

    #[test]
    fn cross_field_fires_when_predicate_matches() {
        let node = make_node("adr-1", "adr", "superseded");
        // missing superseded_by
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
        assert_eq!(v.len(), 1);
        assert!(v[0].message.contains("superseded_by"));
    }

    #[test]
    fn cross_field_silent_when_predicate_false() {
        let node = make_node("adr-1", "adr", "draft");
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
        assert!(v.is_empty());
    }

    #[test]
    fn cross_field_fires_on_date_valued_builtin_predicate() {
        // `when = "reviewed=YYYY-MM-DD"` against a date-valued built-in
        // matches by canonical-string comparison.
        use chrono::NaiveDate;
        let mut config = test_config();
        config.schema.overrides[0].cross_field = vec![CrossFieldSpec {
            when: "reviewed=2026-01-01".to_string(),
            require: "owner".to_string(),
        }];
        let mut node = make_node("adr-1", "adr", "active");
        node.reviewed = Some(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        // missing owner
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert_eq!(v.len(), 1, "expected one violation, got: {v:?}");
        assert!(v[0].message.contains("owner"));
    }

    #[test]
    fn cross_field_silent_when_required_field_present() {
        let mut node = make_node("adr-1", "adr", "superseded");
        node.superseded_by = Some("adr-2".to_string());
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &test_config()))
            .violations;
        assert!(v.is_empty());
    }

    #[test]
    fn is_field_missing_handles_orphan_ok_as_always_present() {
        // `orphan_ok` is a built-in `bool` — `false` is a meaningful
        // value, not absence. The dispatch must treat both `true` and
        // `false` as present; falling through to the `attrs` lookup
        // would always return `None` for struct-backed fields and
        // silently report orphan_ok missing on every doc.
        let mut node = make_node("adr-1", "adr", "active");
        node.orphan_ok = false;
        assert!(!is_field_missing(&node, "orphan_ok"));
        node.orphan_ok = true;
        assert!(!is_field_missing(&node, "orphan_ok"));
    }

    #[test]
    fn is_field_missing_handles_covers_as_collection() {
        // `covers` (Vec<String>) joined the built-in roster late; its
        // arm must check `is_empty()` like the other vectors, not
        // fall through to the attrs catch-all.
        let mut node = make_node("adr-1", "adr", "active");
        node.covers.clear();
        assert!(is_field_missing(&node, "covers"));
        node.covers.push("src/lib.rs".into());
        assert!(!is_field_missing(&node, "covers"));
    }

    #[test]
    fn cross_field_require_orphan_ok_fires_against_node_value() {
        // End-to-end: `cross_field.require = "orphan_ok"` must inspect
        // the actual bool, not silently see it as absent. With
        // `orphan_ok = true` the rule passes; without an explicit
        // dispatch arm a struct-backed boolean field would fall
        // through to the `attrs` lookup and the rule would flag
        // every doc as missing it.
        use crate::config::{CrossFieldSpec, SchemaConfig};
        let mut config = test_config();
        config.schema = SchemaConfig {
            cross_field: vec![CrossFieldSpec {
                when: "kind=adr".into(),
                require: "orphan_ok".into(),
            }],
            ..Default::default()
        };
        let mut node = make_node("adr-1", "adr", "active");
        node.orphan_ok = true;
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert!(v.is_empty(), "orphan_ok = true must satisfy require: {v:?}");
    }

    #[test]
    fn predicate_matches_orphan_ok_scalar_comparison() {
        // `read_field_as_string("orphan_ok")` must emit `"true"` /
        // `"false"` so `when = "orphan_ok=true"` round-trips against
        // the literal YAML scalar.
        use crate::config::WhenPredicate;
        let mut node = make_node("adr-1", "adr", "active");
        node.orphan_ok = true;
        assert!(predicate_matches_node(
            &WhenPredicate::Equals {
                field: "orphan_ok".into(),
                value: "true".into(),
            },
            &node,
        ));
        node.orphan_ok = false;
        assert!(predicate_matches_node(
            &WhenPredicate::Equals {
                field: "orphan_ok".into(),
                value: "false".into(),
            },
            &node,
        ));
    }

    #[test]
    fn cross_field_fires_when_in_predicate_matches() {
        use crate::config::CrossFieldSpec;
        let mut config = test_config();
        config.schema.overrides[0].cross_field = vec![CrossFieldSpec {
            when: "status in {superseded,archived}".to_string(),
            require: "superseded_by".to_string(),
        }];
        let node = make_node("adr-1", "adr", "superseded");
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert_eq!(v.len(), 1, "expected one violation, got: {v:?}");
        assert!(v[0].message.contains("superseded_by"));
    }

    #[test]
    fn cross_field_silent_when_in_predicate_no_match() {
        use crate::config::CrossFieldSpec;
        let mut config = test_config();
        config.schema.overrides[0].cross_field = vec![CrossFieldSpec {
            when: "status in {superseded,archived}".to_string(),
            require: "superseded_by".to_string(),
        }];
        let node = make_node("adr-1", "adr", "draft");
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert!(v.is_empty(), "expected no violations, got: {v:?}");
    }

    #[test]
    fn cross_field_fires_when_exists_predicate_holds() {
        use crate::config::CrossFieldSpec;
        let mut config = test_config();
        config.schema.overrides[0].cross_field = vec![CrossFieldSpec {
            when: "owner exists".to_string(),
            require: "reviewed".to_string(),
        }];
        config.schema.overrides[0].required.push("reviewed".into());
        let mut node = make_node("adr-1", "adr", "active");
        node.owner = Some("alice".into());
        // `reviewed` absent -> violation
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert_eq!(v.len(), 1, "expected one violation, got: {v:?}");
        assert!(v[0].message.contains("reviewed"));
    }

    #[test]
    fn cross_field_silent_when_exists_predicate_false() {
        use crate::config::CrossFieldSpec;
        let mut config = test_config();
        config.schema.overrides[0].cross_field = vec![CrossFieldSpec {
            when: "owner exists".to_string(),
            require: "reviewed".to_string(),
        }];
        config.schema.overrides[0].required.push("reviewed".into());
        let node = make_node("adr-1", "adr", "active");
        // `owner` absent -> predicate false -> no violation
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert!(v.is_empty(), "expected no violations, got: {v:?}");
    }

    #[test]
    fn cross_field_fires_when_not_exists_matches() {
        use crate::config::CrossFieldSpec;
        let mut config = test_config();
        config.schema.overrides[0].cross_field = vec![CrossFieldSpec {
            when: "reviewed not_exists".to_string(),
            require: "owner".to_string(),
        }];
        let node = make_node("adr-1", "adr", "active");
        // `reviewed` absent -> predicate true, `owner` absent -> violation
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert_eq!(v.len(), 1, "expected one violation, got: {v:?}");
        assert!(v[0].message.contains("owner"));
    }

    #[test]
    fn cross_field_exists_detects_non_empty_collection() {
        let mut config = test_config();
        config.schema.cross_field = vec![CrossFieldSpec {
            when: "tags exists".to_string(),
            require: "owner".to_string(),
        }];
        let mut node = make_node("adr-1", "adr", "active");
        node.tags = vec!["important".to_string()];
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert_eq!(
            v.len(),
            1,
            "tags non-empty -> exists true -> owner required: {v:?}"
        );
    }

    #[test]
    fn cross_field_exists_false_for_empty_collection() {
        let mut config = test_config();
        config.schema.cross_field = vec![CrossFieldSpec {
            when: "tags exists".to_string(),
            require: "owner".to_string(),
        }];
        let node = make_node("adr-1", "adr", "active");
        // tags is empty by default
        let graph = make_graph(vec![node]);
        let v = CrossFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert!(
            v.is_empty(),
            "tags empty -> exists false -> no violation: {v:?}"
        );
    }

    #[test]
    fn unknown_field_rule_inert_under_lenient_mode() {
        let mut config = test_config();
        config.schema.mode = crate::config::SchemaMode::Lenient;
        let mut node = make_node("adr-1", "adr", "active");
        node.attrs
            .insert("relatd".to_string(), Value::String("typo".to_string()));
        let graph = make_graph(vec![node]);
        let v = UnknownFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert!(v.is_empty(), "lenient mode must stay silent: {v:?}");
    }

    #[test]
    fn unknown_field_rule_flags_typo_under_strict_mode() {
        let mut config = test_config();
        config.schema.mode = crate::config::SchemaMode::Strict;
        let mut node = make_node("adr-1", "adr", "active");
        node.attrs
            .insert("relatd".to_string(), Value::String("typo".to_string()));
        let graph = make_graph(vec![node]);
        let v = UnknownFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "unknown_field");
        assert!(v[0].message.contains("\"relatd\""));
    }

    #[test]
    fn unknown_field_rule_accepts_when_clause_field_under_strict() {
        // `cross_field.when = "priority=high"` implicitly declares
        // `priority`. A document with `priority: high` must pass strict
        // mode — otherwise the very predicate the rule fires on would
        // also fire `unknown_field`.
        use crate::config::{CrossFieldSpec, SchemaConfig};
        let mut config = test_config();
        config.schema = SchemaConfig {
            cross_field: vec![CrossFieldSpec {
                when: "priority=high".into(),
                require: "owner".into(),
            }],
            mode: crate::config::SchemaMode::Strict,
            ..Default::default()
        };
        let mut node = make_node("adr-1", "adr", "active");
        node.attrs
            .insert("priority".to_string(), Value::String("high".to_string()));
        // owner is also required by the cross_field — supply it so we
        // isolate the UnknownFieldRule check from CrossFieldRule.
        node.owner = Some("alice".into());
        let graph = make_graph(vec![node]);
        let v = UnknownFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert!(v.is_empty(), "when-clause field must be declared: {v:?}");
    }

    #[test]
    fn unknown_field_rule_accepts_declared_attr_under_strict() {
        // `decision_date` is declared in the override's `types`, so
        // strict mode must accept it without complaint.
        let mut config = test_config();
        config.schema.mode = crate::config::SchemaMode::Strict;
        let mut node = make_node("adr-1", "adr", "active");
        node.attrs.insert(
            "decision_date".to_string(),
            Value::String("2026-04-19".to_string()),
        );
        let graph = make_graph(vec![node]);
        let v = UnknownFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert!(v.is_empty(), "declared field must pass: {v:?}");
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
        assert!(
            FieldTypeRule
                .check(&super::super::test_ctx(&graph, &config))
                .violations
                .is_empty()
        );
        assert!(
            CrossFieldRule
                .check(&super::super::test_ctx(&graph, &config))
                .violations
                .is_empty()
        );
    }

    #[test]
    fn explicit_field_fires_only_for_inferred_named_fields() {
        let mut config = test_config();
        config.schema.require_explicit = vec!["status".to_string()];

        // status fell back to inference → red.
        let mut inferred = make_node("adr-1", "adr", "active");
        inferred.inferred_fields = vec!["status".to_string()];
        // status was authored → silent.
        let mut authored = make_node("adr-2", "adr", "active");
        authored.inferred_fields = vec![];
        // title fell back, but it is not in require_explicit → silent.
        let mut other_inferred = make_node("adr-3", "adr", "active");
        other_inferred.inferred_fields = vec!["title".to_string()];

        let graph = make_graph(vec![inferred, authored, other_inferred]);
        let v = ExplicitFieldRule
            .check(&super::super::test_ctx(&graph, &config))
            .violations;
        assert_eq!(v.len(), 1, "exactly the inferred `status` node reds: {v:?}");
        assert_eq!(v[0].node_id.as_deref(), Some("adr-1"));
        assert_eq!(v[0].rule_id, "explicit_field");
        assert!(matches!(
            &v[0].details,
            ViolationDetails::ExplicitField { field } if field == "status"
        ));
    }

    #[test]
    fn explicit_field_does_not_double_report_a_malformed_field() {
        // A field authored-but-malformed carries a `parse_issue` and is
        // therefore NOT in `inferred_fields` — `field_parse` owns it, and
        // `explicit_field` must stay silent rather than falsely claim the
        // value was "not authored". (The parser guarantees this exclusion;
        // here we assert the rule honours an empty `inferred_fields`.)
        let mut config = test_config();
        config.schema.require_explicit = vec!["status".to_string()];
        let mut malformed = make_node("adr-1", "adr", "active");
        malformed.inferred_fields = vec![]; // parser excluded the malformed field
        malformed.parse_issues = vec![crate::model::FieldParseIssue {
            field: "status".to_string(),
            expected: "string".to_string(),
            found: "array".to_string(),
        }];
        let graph = make_graph(vec![malformed]);
        assert!(
            ExplicitFieldRule
                .check(&super::super::test_ctx(&graph, &config))
                .violations
                .is_empty(),
            "a malformed (parse_issue) field must not also red explicit_field"
        );
    }
}
