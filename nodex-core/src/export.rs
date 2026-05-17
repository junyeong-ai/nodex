//! Authoritative manifests of the project's schema and vocabularies.
//!
//! External tools (TypeScript linters, IDE plugins, CI scripts) consume
//! these manifests instead of re-parsing `nodex.toml` themselves —
//! enforcing a one-way dependency where nodex owns the canonical
//! values and every other tool reads them.
//!
//! Pure transformation of [`crate::config::Config`] into
//! `serde_json::Value`. No file I/O, no validation side effects.

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::config::{BUILTIN_FRONTMATTER_FIELDS, Config, FieldType, SchemaOverride};

/// JSON Schema (draft 2020-12) describing the frontmatter shape every
/// document in the project must satisfy. Encodes global `required` /
/// `types` / `enums` and per-kind overrides as a `oneOf` so a single
/// document instance can be validated against the union.
#[derive(Debug, Clone, Serialize)]
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
        render_branch(config, &config.schema.required, None, &config.kinds.allowed)
    } else {
        let mut branches: Vec<Value> = Vec::with_capacity(config.schema.overrides.len() + 1);
        for ov in &config.schema.overrides {
            branches.push(render_branch(config, &ov.required, Some(ov), &ov.kinds));
        }
        // Only emit the global branch when residual kinds exist; an
        // empty `enum: []` would match nothing yet still inflate the
        // schema. When residual is empty *and* there is exactly one
        // override, flatten further to avoid a one-element oneOf.
        if !global_residual_kinds.is_empty() {
            branches.push(render_branch(
                config,
                &config.schema.required,
                None,
                &global_residual_kinds,
            ));
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

fn render_branch(
    config: &Config,
    required: &[String],
    override_cfg: Option<&SchemaOverride>,
    branch_kinds: &[String],
) -> Value {
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

    // Project-specific types.
    let types = match override_cfg {
        Some(ov) => {
            let mut merged = config.schema.types.clone();
            for (k, v) in &ov.types {
                merged.insert(k.clone(), *v);
            }
            merged
        }
        None => config.schema.types.clone(),
    };
    for (field, ft) in &types {
        properties.insert(field.clone(), field_type_schema(*ft));
    }

    // Project-specific enums (override globals when both declared).
    let enums = match override_cfg {
        Some(ov) => {
            let mut merged = config.schema.enums.clone();
            for (k, v) in &ov.enums {
                merged.insert(k.clone(), v.clone());
            }
            merged
        }
        None => config.schema.enums.clone(),
    };
    for (field, values) in &enums {
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
#[derive(Debug, Clone, Serialize)]
pub struct EnumsManifest {
    pub kinds: Vec<String>,
    pub statuses: StatusesManifest,
    pub fields: std::collections::BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusesManifest {
    pub allowed: Vec<String>,
    pub terminal: Vec<String>,
}

pub fn export_enums(config: &Config) -> EnumsManifest {
    // Global enums + every override's enums, keyed by field. When an
    // override and the global both declare the same field, the override
    // *replaces* (per the merged-view contract used by rules).
    let mut fields: std::collections::BTreeMap<String, Vec<String>> = config.schema.enums.clone();
    for ov in &config.schema.overrides {
        for (k, v) in &ov.enums {
            fields.insert(k.clone(), v.clone());
        }
    }

    EnumsManifest {
        kinds: config.kinds.allowed.clone(),
        statuses: StatusesManifest {
            allowed: config.statuses.allowed.clone(),
            terminal: config.statuses.terminal.clone(),
        },
        fields,
    }
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
}
