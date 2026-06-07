//! Authoritative manifests of the project's schema and vocabularies.
//!
//! External tools (TypeScript linters, IDE plugins, CI scripts) consume
//! these manifests instead of re-parsing `nodex.toml` themselves —
//! enforcing a one-way dependency where nodex owns the canonical
//! values and every other tool reads them.
//!
//! Pure transformation of [`crate::config::Config`] into
//! `serde_json::Value`. No file I/O, no validation side effects.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::config::{BUILTIN_FRONTMATTER_FIELDS, Config, FieldType};
use crate::rules::Severity;

/// JSON Schema (draft 2020-12) describing the frontmatter shape every
/// document in the project must satisfy. Encodes global `required` /
/// `types` / `enums` and per-kind overrides as a `oneOf` so a single
/// document instance can be validated against the union.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SchemaManifest {
    /// `https://json-schema.org/draft/2020-12/schema`.
    #[serde(rename = "$schema")]
    pub draft: &'static str,
    pub title: &'static str,
    /// The composed schema — `oneOf` per-kind branches when overrides
    /// exist, otherwise a flat object.
    #[serde(flatten)]
    pub body: Value,
}

const JSON_SCHEMA_DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

pub fn export_schema(config: &Config) -> SchemaManifest {
    // Collect every kind covered by an override. The global branch
    // must exclude these or oneOf disambiguation breaks — an instance
    // whose `kind` is in an override would otherwise match both
    // branches and violate "exactly one of".
    let override_kinds: std::collections::BTreeSet<&str> = config
        .schema
        .overrides
        .iter()
        .flat_map(|ov| ov.kinds.iter().map(String::as_str))
        .collect();
    let global_residual_kinds: Vec<String> = config
        .kinds
        .allowed
        .iter()
        .filter(|k| !override_kinds.contains(k.as_str()))
        .cloned()
        .collect();

    let body = if config.schema.overrides.is_empty() {
        render_branch(config, &config.kinds.allowed)
    } else {
        let mut branches: Vec<Value> = Vec::with_capacity(config.schema.overrides.len() + 1);
        for ov in &config.schema.overrides {
            branches.push(render_branch(config, &ov.kinds));
        }
        // Only emit the global branch when residual kinds exist; an
        // empty `enum: []` would match nothing yet still inflate the
        // schema. When residual is empty *and* there is exactly one
        // override, flatten further to avoid a one-element oneOf.
        if !global_residual_kinds.is_empty() {
            branches.push(render_branch(config, &global_residual_kinds));
        }
        if branches.len() == 1 {
            branches.remove(0)
        } else {
            json!({
                "type": "object",
                "oneOf": branches,
            })
        }
    };

    SchemaManifest {
        draft: JSON_SCHEMA_DRAFT,
        title: "nodex frontmatter",
        body,
    }
}

/// Render one `oneOf` branch for `branch_kinds`. Everything the branch
/// asserts — `required`, `types`, `enums` — is derived from the merged
/// per-kind views (`required_for` / `types_for` / `enums_for`) via a
/// representative kind, never from raw `schema.overrides`: the exported
/// contract must accept exactly the documents the same config's `check`
/// accepts (an override's `required` *adds to* the global set, so a
/// branch built from the raw override list would under-require). All
/// kinds of a branch share one override by construction (a kind appears
/// in at most one override; residual kinds in none), so any
/// representative yields the branch's views.
fn render_branch(config: &Config, branch_kinds: &[String]) -> Value {
    let representative = branch_kinds
        .first()
        .expect("every schema branch covers at least one kind by construction");
    let required = config.required_for(representative);
    let override_cfg = config.schema_override_for(representative);

    // Properties = built-ins + every key declared in types/enums.
    let mut properties: Map<String, Value> = Map::new();

    // Built-ins with their canonical JSON Schema types.
    for &field in BUILTIN_FRONTMATTER_FIELDS {
        properties.insert(field.to_string(), builtin_field_schema(field));
    }

    // `kind` enum is the *branch's* kind list — never the global
    // `kinds.allowed` — so oneOf branches are mutually exclusive on
    // the kind discriminator.
    let kind_values: Vec<Value> = branch_kinds
        .iter()
        .map(|s| Value::String(s.clone()))
        .collect();
    properties.insert(
        "kind".to_string(),
        json!({"type": "string", "enum": kind_values}),
    );

    // `status` enum overrides the built-in `{"type": "string"}` placeholder
    // directly — the merge-on-existing path can't add an enum when the
    // existing value has none, so an `entry().and_modify(merge_enum)`
    // would silently leave `status` un-constrained.
    let status_values: Vec<Value> = config
        .statuses
        .allowed
        .iter()
        .map(|s| Value::String(s.clone()))
        .collect();
    properties.insert(
        "status".to_string(),
        json!({"type": "string", "enum": status_values}),
    );

    // Project-specific types and enums: the same merged views `check`
    // and `scaffold` consume, so the schema cannot drift from them.
    for (field, ft) in &config.types_for(representative) {
        properties.insert(field.clone(), field_type_schema(*ft));
    }

    for (field, values) in &config.enums_for(representative) {
        let vs: Vec<Value> = values.iter().map(|s| Value::String(s.clone())).collect();
        properties
            .entry(field.clone())
            .and_modify(|v| merge_enum(v, &vs))
            .or_insert_with(|| json!({"type": "string", "enum": vs}));
    }

    let mut node = Map::new();
    node.insert("type".into(), Value::String("object".into()));
    node.insert(
        "required".into(),
        Value::Array(required.iter().map(|s| Value::String(s.clone())).collect()),
    );
    node.insert("properties".into(), Value::Object(properties));

    // Strict mode: forbid undeclared keys. Lenient mode is the default;
    // we still encode `additionalProperties: true` explicitly for clarity.
    let strict = matches!(config.schema.mode, crate::config::SchemaMode::Strict);
    node.insert("additionalProperties".into(), Value::Bool(!strict));

    if let Some(ov) = override_cfg {
        node.insert(
            "title".into(),
            Value::String(format!("kinds: {}", ov.kinds.join(", "))),
        );
    }

    Value::Object(node)
}

fn builtin_field_schema(field: &str) -> Value {
    match field {
        "id" | "title" | "owner" | "superseded_by" => json!({"type": "string"}),
        "created" | "updated" | "reviewed" => {
            json!({"type": "string", "format": "date"})
        }
        "supersedes" | "implements" | "related" | "tags" | "covers" => {
            json!({
                "oneOf": [
                    {"type": "string"},
                    {"type": "array", "items": {"type": "string"}},
                ]
            })
        }
        "orphan_ok" => json!({"type": "boolean"}),
        // `kind` / `status` are overlaid with their enum below.
        _ => json!({"type": "string"}),
    }
}

fn field_type_schema(ft: FieldType) -> Value {
    match ft {
        FieldType::String => json!({"type": "string"}),
        FieldType::Integer => json!({"type": "integer"}),
        FieldType::Bool => json!({"type": "boolean"}),
        FieldType::Date => json!({"type": "string", "format": "date"}),
    }
}

/// Intersect an already-present enum with a new set, so an override
/// tightening the global vocabulary survives without one silently
/// erasing the other.
fn merge_enum(existing: &mut Value, candidates: &[Value]) {
    if let Some(arr) = existing
        .as_object_mut()
        .and_then(|o| o.get_mut("enum"))
        .and_then(Value::as_array_mut)
    {
        let candidate_strings: std::collections::BTreeSet<&str> =
            candidates.iter().filter_map(Value::as_str).collect();
        let intersect: Vec<Value> = arr
            .iter()
            .filter(|v| {
                v.as_str()
                    .map(|s| candidate_strings.contains(s))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if !intersect.is_empty() {
            *arr = intersect;
        }
    }
}

// ─── enums manifest ─────────────────────────────────────────────────────

/// Closed vocabularies the project enforces. Consumed by external lints
/// to verify their own enums stay in sync with nodex's.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EnumsManifest {
    pub kinds: Vec<String>,
    pub statuses: StatusesManifest,
    /// Field-enum constraints for kinds no `[[schema.overrides]]` block
    /// covers — the global `[schema]` enums verbatim. A flat
    /// global+override fold would be wrong in both directions: it would
    /// impose an override's narrowing on kinds it never covered and
    /// hide the global vocabulary the un-overridden kinds keep.
    pub fields: std::collections::BTreeMap<String, Vec<String>>,
    /// The full merged enum view (`enums_for`) for every kind a
    /// `[[schema.overrides]]` block covers — the same view `check`
    /// enforces, so a consumer validates per kind without
    /// re-implementing the replace semantics: look the kind up here
    /// first, fall back to `fields`. Omitted when no overrides exist.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub per_kind:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StatusesManifest {
    pub allowed: Vec<String>,
    pub terminal: Vec<String>,
}

// ─── rules manifest ─────────────────────────────────────────────────────

/// Active rules the project's `check` would run, paired with the
/// scope each operates over. Consumed by external tooling (IDE
/// plugins, language-agnostic pre-commit hooks, generators) that
/// needs to introspect the rule set without parsing `nodex.toml`.
///
/// Only *active* rules are emitted: a rule whose opt-in toggle is
/// not set (e.g. `git_drift` without `detection.git_drift_threshold`)
/// is omitted entirely, so consumers see "what would fire" rather
/// than "what could fire under different config".
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RulesManifest {
    /// Nodex binary version that produced this manifest, so a
    /// consumer can detect when the rule set drifted under their
    /// feet without comparing the full payload.
    pub version: String,
    pub rules: Vec<RuleManifestEntry>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RuleManifestEntry {
    /// Stable rule identifier — the value used in `Violation.rule_id`.
    pub id: String,
    pub source: RuleSource,
    pub severity: Severity,
    pub description: String,
    /// True when this rule semantically requires a diff context
    /// (`check --since <ref>`) to fire. Sourced from
    /// [`crate::rules::Rule::diff_aware`]. Consumers (CI gates,
    /// PR-only validators) dispatch on this instead of hardcoding
    /// the diff-aware rule list.
    pub diff_aware: bool,
    /// Rule-specific parameters — the configured values that
    /// distinguish this rule instance from another in the same family
    /// (regex pattern, kinds, mode, enums, …). Schema is
    /// per-rule (described in the `description`) and kept as a
    /// free-form object so adding a new built-in rule doesn't reshape
    /// the manifest.
    pub params: Map<String, Value>,
}

// `RuleSource` lives in `crate::rules` (the source of truth) and is
// re-exported here as the schema-emitting surface.
pub use crate::rules::RuleSource;

/// Build the active-rules manifest. Pure transformation of [`Config`]
/// — no I/O, no graph access. Every entry is derived from the same
/// [`crate::rules::registered_rules`] registry that `check` runs, so
/// there is no parallel hand-written description / params / source /
/// diff-aware list to drift.
pub fn export_rules(config: &Config) -> RulesManifest {
    let rules = crate::rules::registered_rules(config)
        .iter()
        .map(|rule| RuleManifestEntry {
            id: rule.id().to_string(),
            source: rule.source(),
            severity: rule.severity(),
            description: rule.description().to_string(),
            diff_aware: rule.diff_aware(),
            params: rule.params(config),
        })
        .collect();

    RulesManifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        rules,
    }
}

pub fn export_enums(config: &Config) -> EnumsManifest {
    // Per-kind merged views for every override-covered kind — the same
    // `enums_for` the check rules consume, so the manifest can never
    // disagree with `check` about which values a kind accepts.
    let per_kind: std::collections::BTreeMap<_, _> = config
        .schema
        .overrides
        .iter()
        .flat_map(|ov| ov.kinds.iter())
        .map(|kind| (kind.clone(), config.enums_for(kind)))
        .collect();

    EnumsManifest {
        kinds: config.kinds.allowed.clone(),
        statuses: StatusesManifest {
            allowed: config.statuses.allowed.clone(),
            terminal: config.statuses.terminal.clone(),
        },
        fields: config.schema.enums.clone(),
        per_kind,
    }
}

// ─── envelope-schema manifest ──────────────────────────────────────────

/// JSON-Schema (draft 2020-12) manifest of every CLI response shape.
///
/// External consumers (TypeScript lints, IDE plugins, Python hooks)
/// codegen their own types from this manifest instead of hand-mirroring
/// the wire contract — the same one-way-dependency story as
/// `export_schema` / `export_enums` / `export_rules`, generalised to
/// the JSON envelope itself.
///
/// `envelope` is the generic `{ ok, data, warnings?, error? }` shape
/// independent of any specific command (with `data` as the open
/// `Schema` placeholder). `per_command` keys each canonical command
/// identifier (dotted form, e.g. `query.annotations`) to the schema of
/// its `data` payload. Multiple commands sharing one shape (e.g. every
/// `lifecycle.*` action returning `LifecycleResult`) map to the same
/// schema value — the canonical-name map carries the surface mapping
/// without re-emitting identical schemas.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EnvelopeSchemaManifest {
    /// Nodex binary version that produced this manifest so consumers
    /// can detect when a regenerate is needed.
    pub version: String,
    /// JSON Schema of the generic envelope wrapping every command's
    /// response. `data` is left open (`{}`) — the per-command schemas
    /// below specify the concrete shape.
    pub envelope: Value,
    /// `canonical_command_name -> JSON Schema of the `data` payload`.
    /// Canonical names use dot-separated form
    /// (`query.annotations`, `lifecycle.supersede`, `export.rules`, …).
    pub per_command: Map<String, Value>,
}

/// Build the envelope-schema manifest. Pure transformation — no
/// I/O, no `Config` dependency (envelope shape is fixed by the
/// project, not the project's content).
pub fn export_envelope_schema() -> EnvelopeSchemaManifest {
    EnvelopeSchemaManifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        envelope: envelope_shape(),
        per_command: per_command_schemas(),
    }
}

/// JSON Schema (draft 2020-12) of the generic envelope. Hand-written
/// because the envelope shape is documented as the project's stable
/// contract — encoding it from a Rust type would couple this artefact
/// to whatever generic substitution rustc + schemars produce for
/// `Envelope<T>`, defeating the "envelope is the same every release"
/// promise.
fn envelope_shape() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "nodex CLI envelope",
        "oneOf": [
            {
                "type": "object",
                "required": ["ok", "data"],
                "properties": {
                    "ok": { "const": true },
                    "data": true,
                    "warnings": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "additionalProperties": false
            },
            {
                "type": "object",
                "required": ["ok", "error"],
                "properties": {
                    "ok": { "const": false },
                    "error": {
                        "type": "object",
                        "required": ["code", "message"],
                        "properties": {
                            "code": { "type": "string" },
                            "message": { "type": "string" }
                        },
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false
            }
        ]
    })
}

/// Canonical-name → JSON Schema of that command's `data` payload.
/// Adding a new command extends this map in one place; `schemars`
/// derive on the response type makes the schema emission mechanical.
/// Every entry routes through `schema_of::<T>()` so the canonical
/// source of truth is the actual Rust type — there are no
/// hand-written schemas that could drift from the structs the CLI
/// serialises.
fn per_command_schemas() -> Map<String, Value> {
    use crate::command_result::{
        BuildResult, CheckResult, InitResult, LifecycleResult, MigrateResult, RenameResult,
        ReportResult, RetargetResult,
    };
    use crate::diff::GraphDiff;
    use crate::impact::ImpactReport;
    use crate::query::NodeRefProjection;
    use crate::query::annotations::AnnotationGroup;
    use crate::query::dependents::DependentsReport;
    use crate::query::detect::{OrphanEntry, StaleEntry};
    use crate::query::issues::IssueReport;
    use crate::query::recent::RecentEntry;
    use crate::query::search::SearchEntry;
    use crate::query::similar::SimilarityEntry;
    use crate::query::structure::{Component, Neighborhood};
    use crate::query::traverse::{BacklinkEntry, ChainEntry, CoveredByEntry, NodeEntry};
    use crate::query::trust::TrustEntry;
    use crate::scaffold::ScaffoldResult;

    let mut out: Map<String, Value> = Map::new();

    // Read-only queries — list shape variants. `query.nodes` is the
    // one projected list (`--fields`): its item fields are optional by
    // contract, while `NodeRef` flattened into every other entry stays
    // non-null.
    out.insert("query.nodes".into(), items_envelope::<NodeRefProjection>());
    out.insert("query.search".into(), items_envelope::<SearchEntry>());
    out.insert("query.backlinks".into(), items_envelope::<BacklinkEntry>());
    out.insert("query.chain".into(), items_envelope::<ChainEntry>());
    out.insert("query.orphans".into(), items_envelope::<OrphanEntry>());
    out.insert("query.stale".into(), items_envelope::<StaleEntry>());
    out.insert("query.node".into(), schema_of::<NodeEntry>());
    out.insert(
        "query.covered-by".into(),
        items_envelope::<CoveredByEntry>(),
    );
    out.insert("query.issues".into(), schema_of::<IssueReport>());
    // The CLI emits two distinct shapes for `query trust`:
    //   * `<id>` form               → single `TrustEntry`
    //   * `--bottom/--top` listing  → `ItemsEnvelope<TrustEntry>`
    // Both schemas are registered so typed codegen consumers can
    // validate either response without re-deriving the list shape.
    out.insert("query.trust".into(), schema_of::<TrustEntry>());
    out.insert("query.trust-list".into(), items_envelope::<TrustEntry>());
    out.insert("query.similar".into(), items_envelope::<SimilarityEntry>());
    out.insert("query.recent".into(), items_envelope::<RecentEntry>());
    out.insert("query.components".into(), items_envelope::<Component>());
    out.insert("query.neighborhood".into(), schema_of::<Neighborhood>());
    out.insert("query.dependents".into(), schema_of::<DependentsReport>());
    out.insert(
        "query.annotations".into(),
        items_envelope::<AnnotationGroup>(),
    );

    // Build / check / diff / report — single-object shapes
    out.insert("build".into(), schema_of::<BuildResult>());
    out.insert("check".into(), schema_of::<CheckResult>());
    out.insert("diff".into(), schema_of::<GraphDiff>());
    out.insert("impact".into(), schema_of::<ImpactReport>());
    out.insert("report".into(), schema_of::<ReportResult>());

    // Mutations
    out.insert("scaffold".into(), schema_of::<ScaffoldResult>());
    let lifecycle = schema_of::<LifecycleResult>();
    out.insert("lifecycle.review".into(), lifecycle.clone());
    out.insert("lifecycle.set".into(), lifecycle.clone());
    out.insert("lifecycle.supersede".into(), lifecycle);
    out.insert("migrate".into(), schema_of::<MigrateResult>());
    out.insert("rename".into(), schema_of::<RenameResult>());
    out.insert("retarget".into(), schema_of::<RetargetResult>());
    out.insert("init".into(), schema_of::<InitResult>());

    // Exports
    out.insert("export.schema".into(), schema_of::<SchemaManifest>());
    out.insert("export.enums".into(), schema_of::<EnumsManifest>());
    out.insert("export.rules".into(), schema_of::<RulesManifest>());
    out.insert(
        "export.envelope-schema".into(),
        schema_of::<EnvelopeSchemaManifest>(),
    );

    out
}

/// JSON Schema of `T` via schemars. Centralised so the schemars
/// invocation lives in exactly one place — any change to schemars
/// API or to the project's schema-derivation policy is a single
/// edit.
fn schema_of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("schemars schema is always JSON-serialisable")
}

/// Wrap a per-item schema in the canonical list envelope every
/// items-returning query uses: `{ items: [...], total: N, returned? }`
/// — `total` counts every match, `returned` appears only when a
/// `--limit` cap dropped entries (mirrors the CLI's `ItemsEnvelope`;
/// the two shapes must stay in lockstep or a capped response fails
/// schema validation). Built via `schema_for!` over a generic helper
/// so `$defs` for nested types (e.g. `AnnotationEntry` inside
/// `AnnotationGroup`) propagate to the outer schema root and stay
/// resolvable by external validators.
fn items_envelope<T: schemars::JsonSchema>() -> Value {
    #[derive(serde::Serialize, schemars::JsonSchema)]
    #[allow(dead_code)] // serde never runs here — schema_for is type-driven
    struct ItemsEnvelopeShape<T> {
        items: Vec<T>,
        total: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        returned: Option<usize>,
    }
    schema_of::<ItemsEnvelopeShape<T>>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config, KindsConfig, SchemaConfig, SchemaMode, SchemaOverride, StatusesConfig,
    };

    fn cfg() -> Config {
        Config {
            kinds: KindsConfig {
                allowed: vec!["adr".into(), "guide".into()],
            },
            statuses: StatusesConfig {
                allowed: vec!["active".into(), "superseded".into()],
                terminal: vec!["superseded".into()],
                initial: None,
            },
            schema: SchemaConfig {
                required: vec!["id".into(), "title".into(), "kind".into(), "status".into()],
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into()],
                    required: vec![
                        "id".into(),
                        "title".into(),
                        "kind".into(),
                        "status".into(),
                        "decision_date".into(),
                    ],
                    types: [("decision_date".to_string(), FieldType::Date)]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                }],
                mode: SchemaMode::Lenient,
                ..Default::default()
            },
            ..Config::default()
        }
    }

    #[test]
    fn schema_emits_draft_and_oneof_for_overrides() {
        let m = export_schema(&cfg());
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["$schema"].as_str(), Some(JSON_SCHEMA_DRAFT));
        assert!(v["oneOf"].as_array().is_some(), "overrides → oneOf");
    }

    #[test]
    fn schema_branch_required_is_the_union_check_enforces() {
        // The exported contract must accept exactly the documents the
        // same config's `check` accepts: an override's `required` ADDS
        // to the global set (`required_for`), so every override branch's
        // `required` array must be that union — a branch built from the
        // raw override list would let a downstream validator approve a
        // document `check` rejects.
        let mut c = cfg();
        c.schema.required = vec!["id".into(), "title".into(), "owner".into()];
        c.schema.overrides[0].required = vec!["decision_date".into()];
        let v = serde_json::to_value(export_schema(&c)).unwrap();
        let branches = v["oneOf"].as_array().expect("oneOf");
        let adr = branches
            .iter()
            .find(|b| b["properties"]["kind"]["enum"][0] == "adr")
            .expect("adr branch");
        let mut required: Vec<&str> = adr["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        required.sort_unstable();
        assert_eq!(
            required,
            vec!["decision_date", "id", "owner", "title"],
            "override branch requires the global ∪ override union"
        );
    }

    #[test]
    fn schema_strict_mode_forbids_additional() {
        let mut c = cfg();
        c.schema.mode = SchemaMode::Strict;
        let v = serde_json::to_value(export_schema(&c)).unwrap();
        let branch = &v["oneOf"][0];
        assert_eq!(branch["additionalProperties"].as_bool(), Some(false));
    }

    #[test]
    fn enums_includes_kinds_statuses_and_field_enums() {
        let m = export_enums(&cfg());
        assert_eq!(m.kinds, vec!["adr".to_string(), "guide".to_string()]);
        assert_eq!(m.statuses.terminal, vec!["superseded".to_string()]);
    }

    #[test]
    fn enums_fields_stay_global_and_per_kind_carries_the_merged_view() {
        // The flat `fields` map must never fold an override in — that
        // would impose its narrowing on kinds it doesn't cover and hide
        // the vocabulary un-overridden kinds keep. Override-covered
        // kinds get their full merged view under `per_kind`, the same
        // `enums_for` check enforces.
        use std::collections::BTreeMap;
        let mut c = cfg();
        c.schema.enums =
            BTreeMap::from([("priority".to_string(), vec!["low".into(), "high".into()])]);
        c.schema.overrides[0].enums =
            BTreeMap::from([("priority".to_string(), vec!["critical".into()])]);
        let m = export_enums(&c);
        assert_eq!(
            m.fields["priority"],
            vec!["low".to_string(), "high".to_string()],
            "fields = global verbatim"
        );
        assert_eq!(
            m.per_kind["adr"]["priority"],
            vec!["critical".to_string()],
            "the override kind's merged view"
        );
        assert!(
            !m.per_kind.contains_key("guide"),
            "un-overridden kinds use `fields`"
        );

        // No overrides → per_kind empty (omitted from the JSON).
        let mut plain = cfg();
        plain.schema.overrides.clear();
        assert!(export_enums(&plain).per_kind.is_empty());
    }

    // ─── External validator parity ──────────────────────────────────
    //
    // The export contract is "external tools consume this verbatim".
    // The tests below run the emitted JSON Schema through the
    // `jsonschema` crate's draft 2020-12 validator — the same library
    // most downstream Rust / TypeScript / Python consumers eventually
    // call. If the schema we emit isn't accepted as valid by a real
    // validator, the entire "external lints read our manifest"
    // promise breaks; this is the regression gate that catches a
    // mis-spelled keyword or misnested oneOf before it ships.

    fn compile_emitted_schema(config: &Config) -> jsonschema::Validator {
        let schema_value = serde_json::to_value(export_schema(config)).expect("serialise schema");
        jsonschema::draft202012::new(&schema_value)
            .expect("emitted schema must be a valid JSON Schema draft 2020-12 document")
    }

    #[test]
    fn emitted_schema_is_valid_draft_2020_12() {
        // Two regression gates folded into one: (1) compilation must
        // succeed under draft 2020-12, and (2) it must succeed under
        // both schema modes — strict toggles `additionalProperties`,
        // which is the spot most likely to drift into an invalid
        // shape if the renderer regresses.
        let _ = compile_emitted_schema(&cfg());
        let mut strict = cfg();
        strict.schema.mode = SchemaMode::Strict;
        let _ = compile_emitted_schema(&strict);
    }

    #[test]
    fn emitted_schema_accepts_valid_adr_instance() {
        let validator = compile_emitted_schema(&cfg());
        let instance = serde_json::json!({
            "id": "adr-0001",
            "title": "Choose auth strategy",
            "kind": "adr",
            "status": "active",
            "decision_date": "2026-05-15",
        });
        assert!(
            validator.is_valid(&instance),
            "valid ADR instance was rejected: errors = {:?}",
            validator
                .iter_errors(&instance)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn emitted_schema_rejects_unknown_status() {
        let validator = compile_emitted_schema(&cfg());
        // `archived` is not in this fixture's statuses.allowed.
        let bad = serde_json::json!({
            "id": "adr-0001",
            "title": "Choose auth strategy",
            "kind": "adr",
            "status": "archived",
            "decision_date": "2026-05-15",
        });
        assert!(
            !validator.is_valid(&bad),
            "validator should reject an out-of-vocabulary status"
        );
    }

    #[test]
    fn emitted_schema_rejects_unknown_field_under_strict_mode() {
        let mut strict = cfg();
        strict.schema.mode = SchemaMode::Strict;
        let validator = compile_emitted_schema(&strict);
        let typo = serde_json::json!({
            "id": "adr-0001",
            "title": "Choose auth strategy",
            "kind": "adr",
            "status": "active",
            "decision_date": "2026-05-15",
            "relatd": "adr-0002",   // typo (should be `related`)
        });
        assert!(
            !validator.is_valid(&typo),
            "strict-mode schema must mirror UnknownFieldRule and reject undeclared keys"
        );
    }

    // ─── export_rules ──────────────────────────────────────────────────

    fn rule_ids(m: &RulesManifest) -> Vec<&str> {
        m.rules.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn rules_manifest_includes_always_active_schema_and_freshness() {
        let m = export_rules(&Config::default());
        let ids = rule_ids(&m);
        for expected in [
            "required_field",
            "field_type",
            "field_enum",
            "cross_field",
            "stale_review",
        ] {
            assert!(
                ids.contains(&expected),
                "default config must list {expected}; got {ids:?}"
            );
        }
    }

    #[test]
    fn rules_manifest_omits_strict_only_rule_in_lenient_mode() {
        let m = export_rules(&Config::default());
        assert!(
            !rule_ids(&m).contains(&"unknown_field"),
            "lenient mode must not advertise the strict-only rule"
        );
    }

    #[test]
    fn rules_manifest_includes_strict_only_rule_in_strict_mode() {
        let mut c = Config::default();
        c.schema.mode = crate::config::SchemaMode::Strict;
        assert!(rule_ids(&export_rules(&c)).contains(&"unknown_field"));
    }

    #[test]
    fn rules_manifest_omits_git_drift_when_disabled() {
        let m = export_rules(&Config::default());
        assert!(!rule_ids(&m).contains(&"git_drift"));
    }

    #[test]
    fn rules_manifest_includes_git_drift_with_threshold_scope_when_enabled() {
        let mut c = Config::default();
        c.detection.git_drift_threshold = Some(7);
        let m = export_rules(&c);
        let entry = m
            .rules
            .iter()
            .find(|r| r.id == "git_drift")
            .expect("git_drift should be listed when threshold is set");
        assert_eq!(entry.params["threshold"].as_u64(), Some(7));
        assert!(entry.params["relations"].is_array());
    }

    #[test]
    fn rules_manifest_emits_one_entry_per_body_line_block() {
        let mut c = Config::default();
        let mut enums = std::collections::BTreeMap::new();
        enums.insert("g".into(), vec!["a".into()]);
        c.rules.body_line = vec![crate::config::BodyLineRuleConfig {
            name: "one".into(),
            pattern: r"(?P<g>\w+)".into(),
            enums: enums.clone(),

            kinds: vec![],
        }];
        let m = export_rules(&c);
        let entry = m
            .rules
            .iter()
            .find(|r| r.id == "body_line/one")
            .expect("expected body_line/one entry");
        assert!(matches!(entry.source, RuleSource::Config));
        assert_eq!(entry.params["pattern"].as_str(), Some(r"(?P<g>\w+)"));
    }

    #[test]
    fn rules_manifest_omits_frontmatter_immutable_when_unset() {
        let m = export_rules(&Config::default());
        assert!(!rule_ids(&m).contains(&"frontmatter_immutable"));
    }

    #[test]
    fn rules_manifest_emits_one_entry_per_frontmatter_immutable_block() {
        // Mirrors `rules_manifest_emits_one_entry_per_body_immutable_block`
        // — each `[[rules.frontmatter_immutable]]` becomes its own
        // entry under `frontmatter_immutable/<name>`. Consumers see the
        // locked `fields`, the kind filter, and the diff-aware flag
        // from the manifest params payload.
        let mut c = Config::default();
        c.rules.frontmatter_immutable = vec![crate::config::FrontmatterImmutableRuleConfig {
            name: "identity".into(),
            fields: vec!["id".into(), "kind".into()],

            kinds: vec![],
        }];
        let m = export_rules(&c);
        let entry = m
            .rules
            .iter()
            .find(|r| r.id == "frontmatter_immutable/identity")
            .expect("entry expected when config block present");
        assert_eq!(entry.params["fields"][0].as_str(), Some("id"));
        assert_eq!(entry.source, RuleSource::Config);
        assert!(entry.diff_aware);
    }

    #[test]
    fn rules_manifest_omits_naming_rules_when_unconfigured() {
        let m = export_rules(&Config::default());
        for forbidden in [
            "filename_pattern",
            "sequential_numbering",
            "unique_numbering",
        ] {
            assert!(
                !rule_ids(&m).contains(&forbidden),
                "{forbidden} must be absent without rules.naming"
            );
        }
    }

    #[test]
    fn rules_manifest_carries_binary_version() {
        let m = export_rules(&Config::default());
        assert_eq!(m.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn rules_manifest_marks_frontmatter_immutable_diff_aware() {
        // PR-gate consumers dispatch on `diff_aware: true` instead of
        // hardcoding the rule id — verify each per-block instance
        // self-reports correctly.
        let mut c = Config::default();
        c.rules.frontmatter_immutable = vec![crate::config::FrontmatterImmutableRuleConfig {
            name: "identity".into(),
            fields: vec!["id".into()],

            kinds: vec![],
        }];
        let m = export_rules(&c);
        let entry = m
            .rules
            .iter()
            .find(|r| r.id == "frontmatter_immutable/identity")
            .expect("frontmatter_immutable/identity entry");
        assert!(
            entry.diff_aware,
            "frontmatter_immutable must self-report as diff-aware"
        );
    }

    #[test]
    fn rules_manifest_emits_one_entry_per_body_immutable_block() {
        // Each `[[rules.body_immutable]]` block becomes its own
        // manifest entry, mirroring the body_line / annotations
        // multi-block pattern. Consumers see `body_immutable/<name>`
        // and can dispatch on `mode` / `kinds` without re-parsing
        // nodex.toml.
        let mut c = Config::default();
        c.statuses.terminal = vec!["superseded".into()];
        c.rules.body_immutable = vec![
            crate::config::BodyImmutableRuleConfig {
                name: "adr-frozen".into(),
                mode: crate::config::BodyImmutableMode::Frozen,
                trigger: crate::config::ImmutableTrigger::Terminal,
                kinds: vec![],
            },
            crate::config::BodyImmutableRuleConfig {
                name: "log-append".into(),
                mode: crate::config::BodyImmutableMode::AppendOnly,
                trigger: crate::config::ImmutableTrigger::Terminal,
                kinds: vec![],
            },
        ];
        let m = export_rules(&c);
        let ids: Vec<&str> = m.rules.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"body_immutable/adr-frozen"),
            "missing body_immutable/adr-frozen in {ids:?}"
        );
        assert!(
            ids.contains(&"body_immutable/log-append"),
            "missing body_immutable/log-append in {ids:?}"
        );
        let frozen = m
            .rules
            .iter()
            .find(|r| r.id == "body_immutable/adr-frozen")
            .unwrap();
        assert!(
            frozen.diff_aware,
            "body_immutable rules must self-report as diff-aware"
        );
        assert_eq!(frozen.source, RuleSource::Config);
        assert_eq!(
            frozen.params.get("mode").and_then(|v| v.as_str()),
            Some("frozen"),
            "manifest params must echo the mode verbatim"
        );
    }

    #[test]
    fn rules_manifest_mirrors_registered_rules_exactly() {
        // The manifest is built from `registered_rules`; every entry's
        // id / severity / description / diff_aware / source / scope
        // must come from the Rule trait directly. This test locks the
        // SoT alignment: a hand-edit in `export.rs` that drifts from
        // the trait would fire this.
        let mut c = Config::default();
        c.rules.frontmatter_immutable = vec![crate::config::FrontmatterImmutableRuleConfig {
            name: "identity".into(),
            fields: vec!["id".into()],

            kinds: vec![],
        }];
        c.schema.mode = crate::config::SchemaMode::Strict;
        let registry = crate::rules::registered_rules(&c);
        let manifest = export_rules(&c);
        assert_eq!(
            registry.len(),
            manifest.rules.len(),
            "manifest must have exactly one entry per registered rule"
        );
        for (rule, entry) in registry.iter().zip(manifest.rules.iter()) {
            assert_eq!(entry.id, rule.id());
            assert_eq!(entry.severity, rule.severity());
            assert_eq!(entry.description, rule.description());
            assert_eq!(entry.diff_aware, rule.diff_aware());
            assert_eq!(entry.source, rule.source());
            assert_eq!(entry.params, rule.params(&c));
        }
    }

    #[test]
    fn rules_manifest_default_rules_are_not_diff_aware() {
        // Every other shipped rule operates on a single graph snapshot.
        // If a future rule needs `--since`, its entry must opt in to
        // `diff_aware: true` *and* its `Rule::diff_aware` override.
        let m = export_rules(&Config::default());
        for entry in &m.rules {
            assert!(
                !entry.diff_aware,
                "{} must not claim diff_aware by default",
                entry.id
            );
        }
    }

    // ─── export_envelope_schema ────────────────────────────────────────

    #[test]
    fn envelope_schema_carries_binary_version() {
        let m = export_envelope_schema();
        assert_eq!(m.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn envelope_schema_includes_success_and_error_branches() {
        let m = export_envelope_schema();
        // Generic envelope is a oneOf of {ok:true,data,...} and
        // {ok:false,error,...}. Two branches must exist regardless of
        // which command's data shape sits inside.
        let one_of = m.envelope.get("oneOf").and_then(Value::as_array).unwrap();
        assert_eq!(one_of.len(), 2);
    }

    #[test]
    fn envelope_schema_per_command_covers_every_query_subcommand() {
        let m = export_envelope_schema();
        for expected in [
            "query.nodes",
            "query.search",
            "query.backlinks",
            "query.chain",
            "query.orphans",
            "query.stale",
            "query.node",
            "query.covered-by",
            "query.issues",
            "query.trust",
            "query.trust-list",
            "query.similar",
            "query.recent",
            "query.components",
            "query.neighborhood",
            "query.dependents",
            "query.annotations",
        ] {
            assert!(
                m.per_command.contains_key(expected),
                "missing per_command entry for {expected}"
            );
        }
    }

    #[test]
    fn envelope_schema_trust_registers_both_single_and_list_shapes() {
        // `query trust <id>` returns a single `TrustEntry`; `query
        // trust --bottom/--top` returns an `ItemsEnvelope<TrustEntry>`.
        // Typed consumers can't validate both responses unless both
        // shapes are explicitly registered.
        let m = export_envelope_schema();
        let single = m
            .per_command
            .get("query.trust")
            .expect("query.trust must be registered");
        let listing = m
            .per_command
            .get("query.trust-list")
            .expect("query.trust-list must be registered for --bottom/--top mode");
        // Single shape is the bare TrustEntry object — must not
        // expose the items-envelope keys.
        let single_props = single
            .get("properties")
            .and_then(Value::as_object)
            .expect("TrustEntry must serialise as object");
        assert!(
            !single_props.contains_key("items"),
            "query.trust (single) must not carry `items`; got {single_props:?}"
        );
        // List shape is the canonical items envelope: `items` +
        // `total`.
        let listing_props = listing
            .get("properties")
            .and_then(Value::as_object)
            .expect("trust-list envelope must serialise as object");
        assert!(
            listing_props.contains_key("items"),
            "query.trust-list must carry `items`; got {listing_props:?}"
        );
        assert!(
            listing_props.contains_key("total"),
            "query.trust-list must carry `total`; got {listing_props:?}"
        );
    }

    #[test]
    fn envelope_schema_per_command_excludes_retired_low_trust() {
        // `query low-trust` was retired in v0.13 in favour of the
        // generalised `query trust --bottom`. If anyone ever
        // re-introduces `query.low-trust` here without re-introducing
        // the CLI verb, this test fires.
        let m = export_envelope_schema();
        assert!(
            !m.per_command.contains_key("query.low-trust"),
            "query.low-trust must not be registered — retired in v0.13"
        );
    }

    #[test]
    fn envelope_schema_per_command_covers_lifecycle_actions() {
        let m = export_envelope_schema();
        for action in ["review", "set", "supersede"] {
            let key = format!("lifecycle.{action}");
            assert!(
                m.per_command.contains_key(&key),
                "missing per_command entry for {key}"
            );
        }
    }

    #[test]
    fn envelope_schema_per_command_covers_export_subcommands() {
        let m = export_envelope_schema();
        for name in [
            "export.schema",
            "export.enums",
            "export.rules",
            "export.envelope-schema",
        ] {
            assert!(
                m.per_command.contains_key(name),
                "missing per_command entry for {name}"
            );
        }
    }

    #[test]
    fn envelope_schema_per_entry_is_valid_draft_2020_12() {
        // The whole point of this manifest is consumer codegen — every
        // entry must compile cleanly under a real validator. Regression
        // gate: if a future response type emits an invalid schema
        // shape (mis-spelt keyword, malformed oneOf), this test fires.
        let m = export_envelope_schema();
        for (name, schema) in &m.per_command {
            let _ = jsonschema::draft202012::new(schema)
                .unwrap_or_else(|e| panic!("schema for {name:?} is not valid draft 2020-12: {e}"));
        }
        let _ = jsonschema::draft202012::new(&m.envelope)
            .expect("envelope schema must validate as draft 2020-12");
    }

    #[test]
    fn envelope_schema_validates_real_mutation_envelopes() {
        // Lock the mutation-command schemas against their real
        // serde shapes. If `LifecycleResult` / `MigrateResult`
        // / `RenameResult` / `InitResult` / `ReportResult`
        // diverges from what the CLI actually emits, this test
        // fires — regression gate against hand-written schemas
        // silently drifting from reality.
        use crate::command_result::{
            IdStability, InitResult, LifecycleResult, MigrateResult, MigrationChange, RenameResult,
            ReportResult,
        };
        let m = export_envelope_schema();

        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "lifecycle.supersede",
                serde_json::to_value(LifecycleResult {
                    node_id: "doc-a".into(),
                    action: "supersede".into(),
                    path: "docs/a.md".into(),
                })
                .unwrap(),
            ),
            (
                "migrate",
                serde_json::to_value(MigrateResult {
                    changes: vec![MigrationChange {
                        path: "docs/a.md".into(),
                        id: "doc-a".into(),
                        kind: "generic".into(),
                    }],
                    total: 1,
                    applied: false,
                })
                .unwrap(),
            ),
            (
                "rename",
                serde_json::to_value(RenameResult {
                    old_path: "docs/a.md".into(),
                    new_path: "docs/b.md".into(),
                    references_updated: vec!["docs/c.md".into()],
                    total_updated: 1,
                    id_stability: IdStability::Anchored { id: "doc-a".into() },
                })
                .unwrap(),
            ),
            (
                "init",
                serde_json::to_value(InitResult {
                    path: "nodex.toml".into(),
                })
                .unwrap(),
            ),
            (
                "report",
                serde_json::to_value(ReportResult {
                    generated: vec!["graph.json".into(), "GRAPH.md".into()],
                    output_dir: "_index".into(),
                })
                .unwrap(),
            ),
        ];

        for (key, instance) in cases {
            let schema = m
                .per_command
                .get(key)
                .unwrap_or_else(|| panic!("missing per_command entry for {key}"));
            let validator = jsonschema::draft202012::new(schema)
                .unwrap_or_else(|e| panic!("schema for {key} must compile: {e}"));
            assert!(
                validator.is_valid(&instance),
                "schema for {key} rejected a real envelope instance: {:?}",
                validator
                    .iter_errors(&instance)
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn envelope_schema_validates_capped_and_projected_nodes_envelope() {
        // The exported `query.nodes` schema must accept everything the
        // CLI can actually emit: an uncapped full-field response, a
        // capped one carrying `returned`, and a `--fields` projection
        // whose items omit the dropped fields. A desync here means a
        // typed-codegen client rejects a real response.
        use crate::query::{NODE_REF_FIELDS, NodeRef, NodeRefProjection};
        let m = export_envelope_schema();
        let schema = m.per_command.get("query.nodes").expect("query.nodes entry");
        let validator =
            jsonschema::draft202012::new(schema).expect("query.nodes schema must compile");

        let full = NodeRefProjection::from_node_ref(
            NodeRef {
                id: "doc-a".into(),
                title: "A".into(),
                kind: "generic".into(),
                status: "active".into(),
                path: "docs/a.md".into(),
            },
            &[],
        );
        let projected = NodeRefProjection::from_node_ref(
            NodeRef {
                id: "doc-b".into(),
                title: "B".into(),
                kind: "generic".into(),
                status: "active".into(),
                path: "docs/b.md".into(),
            },
            &["id".to_string(), "kind".to_string()],
        );
        for instance in [
            serde_json::json!({
                "items": [serde_json::to_value(&full).unwrap()],
                "total": 1,
            }),
            serde_json::json!({
                "items": [serde_json::to_value(&projected).unwrap()],
                "total": 5,
                "returned": 1,
            }),
        ] {
            assert!(
                validator.is_valid(&instance),
                "query.nodes schema rejected a real envelope: {:?}",
                validator
                    .iter_errors(&instance)
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
            );
        }

        // The projection constructor's empty-fields contract: never an
        // empty object, and the vocabulary constant stays five-wide.
        assert_eq!(NODE_REF_FIELDS.len(), 5);
        let v = serde_json::to_value(&full).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 5, "empty fields = all five");
        let v = serde_json::to_value(&projected).unwrap();
        assert_eq!(
            v.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["id", "kind"],
            "projection keeps exactly the named fields"
        );
    }

    #[test]
    fn envelope_schema_validates_real_annotations_items_envelope() {
        // The items-shape wrapper (`ItemsEnvelopeShape<T>`) plus a real
        // serialised `AnnotationGroup` must round-trip cleanly against
        // the schema. Specifically locks `AnnotationSourceRef.frontmatter`
        // as optional: a real envelope produced without `--with-frontmatter`
        // omits the key entirely, and that absence must still validate.
        use crate::query::annotations::{AnnotationEntry, AnnotationGroup, AnnotationSourceRef};
        use std::collections::BTreeMap;
        let m = export_envelope_schema();
        let schema = m
            .per_command
            .get("query.annotations")
            .expect("query.annotations entry");
        let validator =
            jsonschema::draft202012::new(schema).expect("query.annotations schema must compile");

        // Case 1: no frontmatter requested — `frontmatter` key absent.
        let bare_group = AnnotationGroup {
            name: "promotes".into(),
            entries: vec![AnnotationEntry {
                key: "x".into(),
                count: 1,
                sources: vec![AnnotationSourceRef {
                    source: "doc-a".into(),
                    path: "docs/a.md".into(),
                    line: 4,
                    frontmatter: BTreeMap::new(),
                }],
            }],
        };
        let bare_envelope = serde_json::json!({
            "items": [serde_json::to_value(&bare_group).unwrap()],
            "total": 1,
        });
        assert!(
            validator.is_valid(&bare_envelope),
            "bare items envelope must validate (frontmatter absent): {:?}",
            validator
                .iter_errors(&bare_envelope)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );

        // Case 2: frontmatter populated — both built-in and project-declared.
        let mut fm = BTreeMap::new();
        fm.insert(
            "created".into(),
            serde_json::Value::String("2026-04-01".into()),
        );
        fm.insert("priority".into(), serde_json::Value::String("high".into()));
        let enriched_group = AnnotationGroup {
            name: "promotes".into(),
            entries: vec![AnnotationEntry {
                key: "x".into(),
                count: 1,
                sources: vec![AnnotationSourceRef {
                    source: "doc-a".into(),
                    path: "docs/a.md".into(),
                    line: 4,
                    frontmatter: fm,
                }],
            }],
        };
        let enriched_envelope = serde_json::json!({
            "items": [serde_json::to_value(&enriched_group).unwrap()],
            "total": 1,
        });
        assert!(
            validator.is_valid(&enriched_envelope),
            "enriched items envelope must validate: {:?}",
            validator
                .iter_errors(&enriched_envelope)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn envelope_schema_validates_real_build_summary_instance() {
        // The `build` command emits a richer envelope than the core
        // `BuildStats` (adds `duration_ms`). Verify the schema entry
        // for `build` matches what the CLI actually serialises so a
        // future hand-rolled regression in `commands/build.rs` is
        // caught here.
        use crate::command_result::BuildResult;
        let m = export_envelope_schema();
        let schema = m.per_command.get("build").expect("build entry");
        let validator = jsonschema::draft202012::new(schema).expect("build schema must compile");
        let instance = serde_json::to_value(BuildResult {
            nodes: 3,
            edges: 1,
            annotations: 0,
            body_line_matches: 0,
            cached: 0,
            parsed: 3,
            duration_ms: 5,
            conditionally_excluded: vec![],
        })
        .unwrap();
        assert!(
            validator.is_valid(&instance),
            "build schema rejected a real BuildResult instance: {:?}",
            validator
                .iter_errors(&instance)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn envelope_schema_validates_real_check_result_instance() {
        // The `check` command emits a richer envelope than the core
        // `CheckReport` (adds `total` + `has_errors`). The
        // `per_command["check"]` schema is derived from `CheckResult`
        // — verify a real CLI-shaped payload validates so a future
        // hand-rolled regression in `commands/check.rs` is caught
        // here before reaching consumers.
        use crate::command_result::CheckResult;
        let m = export_envelope_schema();
        let schema = m.per_command.get("check").expect("check entry");
        let validator = jsonschema::draft202012::new(schema).expect("check schema must compile");
        let instance = serde_json::to_value(CheckResult {
            violations: vec![],
            skipped_rules: vec![],
            total: 0,
            has_errors: false,
        })
        .unwrap();
        assert!(
            validator.is_valid(&instance),
            "check schema rejected a real CheckResult instance: {:?}",
            validator
                .iter_errors(&instance)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn envelope_schema_validates_real_issue_report_instance() {
        // End-to-end: a real `IssueReport` envelope, serialised
        // through serde, must validate against the schema the
        // envelope-schema manifest emits for `query.issues`. This is
        // the load-bearing promise for downstream codegen.
        let m = export_envelope_schema();
        let schema = m
            .per_command
            .get("query.issues")
            .expect("query.issues entry");
        let validator =
            jsonschema::draft202012::new(schema).expect("query.issues schema must compile");
        // Hand-shaped to match `query::issues::IssueReport` exactly —
        // every required field present, no spurious extras. A drift
        // in either direction (schema gains a field, struct drops one)
        // surfaces here.
        let instance = serde_json::json!({
            "orphans": [],
            "stale": [],
            "unresolved_edges": [],
            "violations": [],
            "skipped_rules": [],
            "summary": {
                "total": 0,
                "by_category": {}
            }
        });
        assert!(
            validator.is_valid(&instance),
            "schema rejected a real IssueReport instance: {:?}",
            validator
                .iter_errors(&instance)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
    }
}
