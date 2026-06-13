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

use crate::config::{
    BUILTIN_FRONTMATTER_FIELDS, Config, FieldType, IdRule, KindRule, OutputConfig, ParserConfig,
    ScopeConfig,
};
use crate::error::{Error, Result};
use crate::rules::Severity;

/// JSON Schema (draft 2020-12) for the project's frontmatter, composed
/// as a `oneOf` of per-kind branches (global `[schema]` merged with each
/// `[[schema.overrides]]`).
///
/// The schema describes *authorable* frontmatter — the shape a user
/// writes in the `---` block — and is the *structural* half of the
/// contract: it encodes the per-field constraints JSON Schema expresses
/// cleanly — field presence (`required`), JSON type (`types`), closed
/// vocabularies (`enums`, plus the `kind`/`status` allowed sets), and,
/// in strict mode, rejection of undeclared fields
/// (`additionalProperties: false`).
///
/// `required` lists only the fields a document must author — by
/// loader guarantee: `Config::validate` rejects a `required` entry
/// naming a parser-inferred built-in
/// (`id`/`kind`/`status`/`title`/`orphan_ok`, see
/// `INFERRED_FRONTMATTER_FIELDS`), so the schema emits the configured
/// lists verbatim while the inferred fields' `type`/`enum` constraints
/// stay in `properties` (applied when the field is present).
///
/// Two facets of `check` deliberately live elsewhere, because forcing
/// them into the schema would either mislead a code generator or couple
/// the schema to `check`'s internal field typing:
/// - *emptiness-as-absence*: `check` treats an explicitly empty value
///   (`field: ""` / `field: []`) as an absent field, with a field-by-
///   field nuance — a `String` field counts empty as missing, an
///   `Option<String>` field counts it as present — that tracks `check`'s
///   Rust types, not anything JSON Schema can mirror. So the schema adds
///   no non-emptiness floor: `required` asserts key presence only, and a
///   present value (empty or not) is validated against its declared
///   `type` / `enum`. The schema therefore never rejects a value for
///   being empty *per se*, and the lone divergence is a malformed
///   explicit-empty value on a typed/enum'd field, which the schema
///   flags structurally while `check` leniently ignores.
/// - *relations*: `cross_field` predicates (`when X require Y`), which
///   JSON Schema cannot express without a conditional explosion, are
///   carried by the rules manifest (`export_rules`, the `cross_field`
///   params) and enforced by `check`.
///
/// So the boundary is: structure is the schema's, emptiness-as-absence
/// and relations are `check`'s. For well-formed frontmatter (no field
/// carries an explicit empty value) the schema and `check` agree exactly
/// on structure; a consumer runs `check` (or reads the rules manifest)
/// for emptiness leniency and relational predicates.
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

    // `status` enum overwrites the built-in `{"type": "string"}`
    // placeholder directly: its vocabulary comes from `statuses.allowed`
    // — config state the `enums_for` pass below never sees (a per-kind
    // `status` enum in `schema.enums` then *narrows* this seed via
    // `constrain_enum`'s intersect path).
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
    // and `scaffold` consume, so the schema cannot drift from them. A
    // field declared in BOTH carries its type AND its enum — emitted in
    // the field's JSON type (load validation guarantees the values
    // parse), since `{"type":"integer","enum":["1"]}` would match
    // nothing and silently dropping the enum would let the schema
    // accept values `check` rejects.
    let types = config.types_for(representative);
    for (field, ft) in &types {
        properties.insert(field.clone(), field_type_schema(*ft));
    }

    for (field, values) in &config.enums_for(representative) {
        let ft = types.get(field).copied();
        let vs: Vec<Value> = values.iter().map(|v| typed_enum_value(v, ft)).collect();
        properties
            .entry(field.clone())
            .and_modify(|v| constrain_enum(v, &vs))
            .or_insert_with(|| json!({"type": "string", "enum": vs}));
    }

    // Every remaining *declared* field — a `required` entry or a
    // `cross_field` participant with no type/enum of its own — gets a
    // permissive property. Strict mode's `additionalProperties: false`
    // must forbid exactly what `UnknownFieldRule` forbids: undeclared
    // fields. A declared-but-unconstrained field left out of
    // `properties` would make the exported schema reject documents the
    // same config's `check` accepts.
    for field in config.declared_fields_for(representative) {
        properties.entry(field).or_insert_with(|| json!({}));
    }

    let mut node = Map::new();
    node.insert("type".into(), Value::String("object".into()));
    // `required` = the authored project fields (`schema.required`, merged
    // per-kind) PLUS `schema.require_explicit`, the global opt-in that
    // forces specific inferrable built-ins (`id` / `title` / `kind` /
    // `status`) to be authored. The loader keeps the two sets disjoint
    // (`required` rejects inferred built-ins; `require_explicit` accepts
    // only them), so the chain is dedup-free — and the schema marks
    // exactly what `check` enforces (`required_field` + `explicit_field`),
    // so export and check can never disagree about what a document must
    // author.
    let required_fields: Vec<Value> = required
        .iter()
        .cloned()
        .chain(config.schema.require_explicit.iter().cloned())
        .map(Value::String)
        .collect();
    node.insert("required".into(), Value::Array(required_fields));
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

/// Apply an enum constraint to an already-seeded property schema.
///
/// Two cases, both required for export ↔ check agreement:
/// - the property has NO enum yet (seeded by the built-in or `types`
///   loop as a bare `{"type": ...}`): the constraint is **added**,
///   preserving the type — silently dropping it here was the gap that
///   let the exported schema accept values `check`'s field_enum
///   rejects for any field declared in both `types` and `enums`.
/// - the property already has an enum (the `kind` / `status` seeds):
///   the sets are **intersected**, so a per-kind enum tightening the
///   seeded vocabulary survives without one silently erasing the other.
///   The intersection is applied unconditionally: load validation makes
///   an empty result unreachable (a `status` enum must be a non-empty
///   subset of `statuses.allowed`; `validate_kind_satisfiability`
///   guarantees a `kind` enum admits every kind its branch covers), and
///   if a future regression ever produced one, an `enum: []` that
///   matches nothing is the honest fail-closed rendering — silently
///   keeping the wider seed would hide the contradiction.
fn constrain_enum(existing: &mut Value, candidates: &[Value]) {
    let Some(obj) = existing.as_object_mut() else {
        return;
    };
    match obj.get_mut("enum").and_then(Value::as_array_mut) {
        None => {
            obj.insert("enum".into(), Value::Array(candidates.to_vec()));
        }
        Some(arr) => {
            let candidate_strings: std::collections::BTreeSet<&str> =
                candidates.iter().filter_map(Value::as_str).collect();
            arr.retain(|v| {
                v.as_str()
                    .map(|s| candidate_strings.contains(s))
                    .unwrap_or(false)
            });
        }
    }
}

/// An enum value in the JSON type of its field. Config enum values are
/// TOML strings; a typed field's exported enum must carry them in the
/// field's JSON type or the constraint matches nothing. Load validation
/// (`value_matches_field_type`) guarantees the parse succeeds; the
/// string fallback is unreachable belt-and-suspenders.
fn typed_enum_value(value: &str, ft: Option<FieldType>) -> Value {
    match ft {
        Some(FieldType::Integer) => value
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(value.to_string())),
        Some(FieldType::Bool) => value
            .parse::<bool>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(value.to_string())),
        Some(FieldType::String | FieldType::Date) | None => Value::String(value.to_string()),
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
    ///
    /// `default` (not just `skip_serializing_if`) so the derived JSON
    /// Schema marks it OPTIONAL, matching its serde-omitted reality:
    /// schemars cannot see `skip_serializing_if`, so without `default`
    /// it would publish `per_kind` as required and a typed client
    /// generated from the schema would reject the (valid) override-free
    /// payload where the field is absent.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
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

// ─── config manifest ────────────────────────────────────────────────────

/// The resolved document-locating surface: post-default, post-validation
/// values of the config blocks that answer "which files does nodex
/// graph, where do artifacts live, how do ids / kinds / statuses
/// derive" — including the code-level fallbacks (`fallback_kind`,
/// `fallback_id_template`, the resolved `initial_status`) that exist in
/// no TOML key. Consumers read this instead of re-parsing `nodex.toml`,
/// which cannot show them an omitted key's real default.
///
/// The boundary is principled: vocabularies stay in `export enums` and
/// validation rules in `export rules` — this manifest cross-references
/// them, never duplicates them.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConfigManifest {
    /// Nodex binary version that produced this manifest.
    pub version: String,
    pub scope: ScopeConfig,
    pub output: OutputConfig,
    pub parser: ParserConfig,
    pub identity: IdentityManifest,
    /// The status newly created / frontmatter-less documents take:
    /// `statuses.initial` when declared, else the first allowed status
    /// — the same source `scaffold` consumes.
    pub initial_status: String,
}

/// Identity classification rules in evaluation order (first match
/// wins), plus the code-level fallbacks that apply when nothing
/// matches.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IdentityManifest {
    pub kind_rules: Vec<KindRule>,
    pub id_rules: Vec<IdRule>,
    /// Kind assigned when no `kind_rules` glob matches.
    pub fallback_kind: String,
    /// Id template applied when no `id_rules` entry matches.
    pub fallback_id_template: String,
}

/// Build the resolved-config manifest. Pure transformation of
/// [`Config`] — no I/O, no graph access.
pub fn export_config(config: &Config) -> ConfigManifest {
    ConfigManifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        scope: config.scope.clone(),
        output: config.output.clone(),
        parser: config.parser.clone(),
        identity: IdentityManifest {
            kind_rules: config.identity.kind_rules.clone(),
            id_rules: config.identity.id_rules.clone(),
            fallback_kind: crate::parser::identity::FALLBACK_KIND.to_string(),
            fallback_id_template: crate::parser::identity::FALLBACK_ID_TEMPLATE.to_string(),
        },
        initial_status: config.initial_status_for().to_string(),
    }
}

// ─── commands manifest ──────────────────────────────────────────────────

/// The authoritative CLI invocation surface: one entry per CLI leaf,
/// derived in `nodex-cli` from the same clap tree the binary parses
/// (`commands_manifest()`), so the published grammar can never drift
/// from the real surface. The types live here so
/// `per_command_schemas` derives the `export.commands` schema from the
/// same struct the CLI serialises.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CommandsManifest {
    /// Nodex binary version that produced this manifest.
    pub version: String,
    pub commands: Vec<CommandManifestEntry>,
}

/// One CLI leaf: its invocation tokens, its `per_command` envelope-
/// schema key (the dotted join of `path`), any flag-selected alternate
/// payload shapes, and its positional arity.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CommandManifestEntry {
    /// Invocation tokens, e.g. `["query", "trust"]`.
    pub path: Vec<String>,
    /// The leaf's `per_command` envelope-schema key (dotted `path`).
    pub schema: String,
    /// Flag-selected alternate payload shapes (e.g. `query.trust-list`
    /// behind `--bottom` / `--top`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<CommandMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positionals: Vec<PositionalEntry>,
}

/// A second payload shape one leaf emits when any of `flags` is set.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CommandMode {
    /// The mode's `per_command` envelope-schema key.
    pub schema: String,
    /// Long flag names (without `--`) that select this mode.
    pub flags: Vec<String>,
}

/// One positional argument of a CLI leaf.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PositionalEntry {
    pub name: String,
    pub required: bool,
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
///
/// Two emission forms of one canonical model: with `inline_refs`
/// `false`, each per-command entry bundles its nested types under a
/// per-entry `$defs` (the names drive named-model codegen in
/// spec-following toolchains); with `true`, every entry is fully
/// self-contained — [`inline_schema_refs`] resolves each `#/$defs/...`
/// reference in place, fail-closed, for `$ref`-naive generators.
pub fn export_envelope_schema(inline_refs: bool) -> Result<EnvelopeSchemaManifest> {
    let mut per_command = per_command_schemas();
    if inline_refs {
        for (_, schema) in per_command.iter_mut() {
            *schema = inline_schema_refs(schema)?;
        }
    }
    Ok(EnvelopeSchemaManifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        envelope: envelope_shape(),
        per_command,
    })
}

/// Resolve every `#/$defs/...` reference of one per-command entry in
/// place, producing a self-contained schema with no `$ref` / `$defs`
/// anywhere. Fail-closed and exact:
///
/// - an unresolvable reference (external URI, pointer outside the
///   entry's `$defs`, missing definition) is [`Error::Config`] — never
///   truncation or silent retention;
/// - a definition ring is detected by the active reference path (no
///   depth cap) and is [`Error::Cycle`] naming the ring.
///
/// Keywords beside a `$ref` apply conjunctively in draft 2020-12, so
/// they are preserved around the inlined target via `allOf`. The walk
/// treats every `$ref` / `$defs` key as the JSON Schema keyword: the
/// per-command schemas are derived from the shipped Rust types via
/// schemars, whose serialised field names never collide with
/// `$`-prefixed keywords.
fn inline_schema_refs(entry: &Value) -> Result<Value> {
    let defs: Map<String, Value> = entry
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut active: Vec<String> = Vec::new();
    inline_value(entry, &defs, &mut active)
}

fn inline_value(
    value: &Value,
    defs: &Map<String, Value>,
    active: &mut Vec<String>,
) -> Result<Value> {
    match value {
        Value::Object(obj) => {
            if let Some(reference) = obj.get("$ref") {
                let Some(name) = reference.as_str().and_then(|r| r.strip_prefix("#/$defs/")) else {
                    return Err(Error::Config(format!(
                        "envelope-schema: unresolvable $ref {reference} — only \
                         `#/$defs/<name>` references are inlineable"
                    )));
                };
                let Some(target) = defs.get(name) else {
                    return Err(Error::Config(format!(
                        "envelope-schema: unresolvable $ref \"#/$defs/{name}\" — \
                         no such definition in the entry's $defs"
                    )));
                };
                if active.iter().any(|n| n == name) {
                    let mut chain = active.clone();
                    chain.push(name.to_string());
                    return Err(Error::Cycle { chain });
                }
                active.push(name.to_string());
                let resolved = inline_value(target, defs, active)?;
                active.pop();

                let siblings: Vec<(&String, &Value)> =
                    obj.iter().filter(|(k, _)| k.as_str() != "$ref").collect();
                if siblings.is_empty() {
                    return Ok(resolved);
                }
                let mut merged = Map::new();
                for (key, sibling) in siblings {
                    merged.insert(key.clone(), inline_value(sibling, defs, active)?);
                }
                // The resolved target joins the conjunction. When the
                // node already carries an `allOf` sibling, the target
                // joins *that* array — overwriting or dropping either
                // side would silently discard validation constraints,
                // and a non-array `allOf` is malformed input the
                // inliner refuses rather than papers over.
                match merged.get_mut("allOf") {
                    None => {
                        merged.insert("allOf".into(), Value::Array(vec![resolved]));
                    }
                    Some(Value::Array(branches)) => branches.insert(0, resolved),
                    Some(other) => {
                        return Err(Error::Config(format!(
                            "envelope-schema: `allOf` beside $ref \"#/$defs/{name}\" is not \
                             an array (got {other}) — the schema is malformed"
                        )));
                    }
                }
                return Ok(Value::Object(merged));
            }
            let mut out = Map::new();
            for (key, nested) in obj {
                // Definitions are inlined at their use sites; the
                // hoisted map itself does not survive.
                if key == "$defs" {
                    continue;
                }
                out.insert(key.clone(), inline_value(nested, defs, active)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|item| inline_value(item, defs, active))
                .collect::<Result<Vec<_>>>()?,
        )),
        scalar => Ok(scalar.clone()),
    }
}

// ─── envelope-schema diff (the release contract gate) ──────────────────

/// Classified delta between two envelope-schema payloads (the `.data`
/// of two `nodex export envelope-schema` runs).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EnvelopeSchemaDiff {
    pub breaking: Vec<ContractChange>,
    pub additive: Vec<ContractChange>,
}

/// One classified contract change. `command` is the `per_command` key
/// (or `envelope` for the generic wrapper); `location` is the
/// slash-joined keyword path inside that schema.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ContractChange {
    pub command: String,
    pub location: String,
    pub message: String,
}

/// Schema keywords that carry prose or tooling metadata, never
/// validation semantics — changes to them are not contract changes.
const METADATA_KEYWORDS: &[&str] = &[
    "description",
    "title",
    "$comment",
    "examples",
    "default",
    "$schema",
    "$id",
    "deprecated",
];

/// Classify every difference between two envelope-schema payloads as
/// breaking or additive. Pure transformation: `version` and metadata
/// keywords are ignored; any construct the classifier cannot
/// positively classify is breaking (fail closed).
///
/// The classification is **output-schema polarity** throughout: the
/// envelope schemas describe what nodex *emits*, and the canonical
/// consumer is a generated typed client (docs/CODEGEN.md), so a change
/// breaks exactly when it withdraws something the output guaranteed —
/// a guarantee gained is additive. A request-schema manifest needs the
/// inverse polarity for `required` and `enum`; this classifier is
/// output-only and must be split before being pointed at inputs.
///
/// Breaking: a `per_command` key removed, a property removed, a type
/// change, `required` losing a member, an enum value added,
/// `additionalProperties` tightening to `false`, a `oneOf` branch
/// count change, or any unclassifiable keyword change. Additive: a key
/// added, a property added, an enum value removed, `required` gaining
/// a member, `additionalProperties` loosening from `false`.
pub fn compute_envelope_schema_diff(baseline: &Value, head: &Value) -> EnvelopeSchemaDiff {
    let mut diff = EnvelopeSchemaDiff {
        breaking: vec![],
        additive: vec![],
    };
    let empty = Map::new();
    let baseline_commands = baseline
        .get("per_command")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let head_commands = head
        .get("per_command")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    for (key, before) in baseline_commands {
        match head_commands.get(key) {
            None => diff.breaking.push(ContractChange {
                command: key.clone(),
                location: "per_command".into(),
                message: "command schema removed".into(),
            }),
            Some(after) => diff_schema(key, "data", before, after, &mut diff),
        }
    }
    for key in head_commands.keys() {
        if !baseline_commands.contains_key(key) {
            diff.additive.push(ContractChange {
                command: key.clone(),
                location: "per_command".into(),
                message: "command schema added".into(),
            });
        }
    }
    // The envelope wrapper is half the contract: a payload without it is
    // malformed, and a gate must never wave malformed input through —
    // every absence classifies as breaking.
    match (baseline.get("envelope"), head.get("envelope")) {
        (Some(before), Some(after)) => {
            diff_schema("envelope", "envelope", before, after, &mut diff)
        }
        (Some(_), None) => {
            diff.breaking
                .push(change("envelope", "envelope", "envelope schema removed"))
        }
        (None, Some(_)) => {
            diff.breaking
                .push(change("envelope", "envelope", "envelope schema added"))
        }
        (None, None) => diff.breaking.push(change(
            "envelope",
            "envelope",
            "envelope schema absent from both payloads",
        )),
    }
    diff
}

fn change(command: &str, location: &str, message: impl Into<String>) -> ContractChange {
    ContractChange {
        command: command.to_string(),
        location: location.to_string(),
        message: message.into(),
    }
}

fn diff_schema(
    command: &str,
    location: &str,
    before: &Value,
    after: &Value,
    diff: &mut EnvelopeSchemaDiff,
) {
    if before == after {
        return;
    }
    let (Some(b), Some(a)) = (before.as_object(), after.as_object()) else {
        // Boolean schemas or a representation change — not positively
        // classifiable, fail closed.
        diff.breaking
            .push(change(command, location, "schema shape changed"));
        return;
    };
    let keys: std::collections::BTreeSet<&str> =
        b.keys().chain(a.keys()).map(String::as_str).collect();
    for key in keys {
        if METADATA_KEYWORDS.contains(&key) {
            continue;
        }
        let bv = b.get(key);
        let av = a.get(key);
        if bv == av {
            continue;
        }
        let loc = format!("{location}/{key}");
        match key {
            "properties" => diff_properties(command, &loc, bv, av, a.get("required"), diff),
            "$defs" => diff_definitions(command, &loc, bv, av, diff),
            "required" => diff_required(command, &loc, bv, av, diff),
            "enum" => diff_enum(command, &loc, bv, av, diff),
            "type" => diff.breaking.push(change(
                command,
                &loc,
                format!("type changed from {} to {}", render(bv), render(av)),
            )),
            "additionalProperties" => diff_additional_properties(command, &loc, bv, av, diff),
            "items" => match (bv, av) {
                (Some(before), Some(after)) => diff_schema(command, &loc, before, after, diff),
                _ => diff
                    .breaking
                    .push(change(command, &loc, "items schema added or removed")),
            },
            "oneOf" | "anyOf" | "allOf" => diff_branches(command, &loc, key, bv, av, diff),
            other => diff.breaking.push(change(
                command,
                &loc,
                format!("`{other}` changed — not positively classifiable, treated as breaking"),
            )),
        }
    }
}

fn render(value: Option<&Value>) -> String {
    value.map_or_else(|| "(absent)".to_string(), Value::to_string)
}

fn diff_properties(
    command: &str,
    location: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    required_after: Option<&Value>,
    diff: &mut EnvelopeSchemaDiff,
) {
    let empty = Map::new();
    let b = before.and_then(Value::as_object).unwrap_or(&empty);
    let a = after.and_then(Value::as_object).unwrap_or(&empty);
    let required: Vec<&str> = required_after
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for (name, before_schema) in b {
        match a.get(name) {
            None => diff.breaking.push(change(
                command,
                location,
                format!("property `{name}` removed"),
            )),
            Some(after_schema) => diff_schema(
                command,
                &format!("{location}/{name}"),
                before_schema,
                after_schema,
                diff,
            ),
        }
    }
    for name in a.keys() {
        if b.contains_key(name) {
            continue;
        }
        // Output-schema polarity: a new emitted property is additive —
        // typed clients ignore fields they do not model, and a
        // *required* new property additionally gains a guarantee (its
        // `required` join is also reported by `diff_required`; this
        // entry attributes it to the property itself).
        let message = if required.contains(&name.as_str()) {
            format!("required property `{name}` added")
        } else {
            format!("optional property `{name}` added")
        };
        diff.additive.push(change(command, location, message));
    }
}

fn diff_definitions(
    command: &str,
    location: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    diff: &mut EnvelopeSchemaDiff,
) {
    let empty = Map::new();
    let b = before.and_then(Value::as_object).unwrap_or(&empty);
    let a = after.and_then(Value::as_object).unwrap_or(&empty);
    for (name, before_schema) in b {
        match a.get(name) {
            None => diff.breaking.push(change(
                command,
                location,
                format!("definition `{name}` removed"),
            )),
            Some(after_schema) => diff_schema(
                command,
                &format!("{location}/{name}"),
                before_schema,
                after_schema,
                diff,
            ),
        }
    }
    for name in a.keys() {
        if !b.contains_key(name) {
            diff.additive.push(change(
                command,
                location,
                format!("definition `{name}` added"),
            ));
        }
    }
}

/// Output-schema polarity for `required` membership: the manifest
/// describes emitted payloads, so a member leaving `required`
/// withdraws a presence guarantee consumers rely on (breaking —
/// oasdiff's response-required-property-became-optional), while a
/// member joining `required` strengthens the output (additive). A
/// request-schema diff needs the exact inverse.
fn diff_required(
    command: &str,
    location: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    diff: &mut EnvelopeSchemaDiff,
) {
    let (Some(b), Some(a)) = (
        before.map_or(Some(vec![]), string_set),
        after.map_or(Some(vec![]), string_set),
    ) else {
        diff.breaking.push(change(
            command,
            location,
            "`required` is not a string array",
        ));
        return;
    };
    for member in &b {
        if !a.contains(member) {
            diff.breaking.push(change(
                command,
                location,
                format!("required no longer lists `{member}`"),
            ));
        }
    }
    for member in &a {
        if !b.contains(member) {
            diff.additive.push(change(
                command,
                location,
                format!("required gained `{member}`"),
            ));
        }
    }
}

/// Output-schema polarity for `enum` membership: a value *added* to an
/// emitted enum is a value consumers' exhaustive matches do not cover
/// (breaking), while a value removed narrows the output to a set every
/// existing consumer already handles (additive). A request-schema diff
/// needs the exact inverse.
fn diff_enum(
    command: &str,
    location: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    diff: &mut EnvelopeSchemaDiff,
) {
    let (Some(b), Some(a)) = (
        before.and_then(Value::as_array),
        after.and_then(Value::as_array),
    ) else {
        diff.breaking
            .push(change(command, location, "enum added or removed"));
        return;
    };
    for value in a {
        if !b.contains(value) {
            diff.breaking.push(change(
                command,
                location,
                format!("enum value {value} added"),
            ));
        }
    }
    for value in b {
        if !a.contains(value) {
            diff.additive.push(change(
                command,
                location,
                format!("enum value {value} removed"),
            ));
        }
    }
}

fn diff_additional_properties(
    command: &str,
    location: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    diff: &mut EnvelopeSchemaDiff,
) {
    // An absent `additionalProperties` means `true` in JSON Schema.
    let permissive = Value::Bool(true);
    let b = before.unwrap_or(&permissive);
    let a = after.unwrap_or(&permissive);
    if b == a {
        return;
    }
    match (b, a) {
        (Value::Bool(false), _) => diff.additive.push(change(
            command,
            location,
            "additionalProperties loosened from false",
        )),
        (_, Value::Bool(false)) => diff.breaking.push(change(
            command,
            location,
            "additionalProperties tightened to false",
        )),
        (Value::Object(_), Value::Object(_)) => diff_schema(command, location, b, a, diff),
        _ => diff.breaking.push(change(
            command,
            location,
            "additionalProperties changed — not positively classifiable",
        )),
    }
}

fn diff_branches(
    command: &str,
    location: &str,
    keyword: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    diff: &mut EnvelopeSchemaDiff,
) {
    let (Some(b), Some(a)) = (
        before.and_then(Value::as_array),
        after.and_then(Value::as_array),
    ) else {
        diff.breaking.push(change(
            command,
            location,
            format!("`{keyword}` added or removed"),
        ));
        return;
    };
    if b.len() != a.len() {
        // Removal drops a shape consumers handle; addition emits a
        // shape they do not — neither is positively additive.
        diff.breaking.push(change(
            command,
            location,
            format!(
                "`{keyword}` branch count changed ({} → {})",
                b.len(),
                a.len()
            ),
        ));
        return;
    }
    for (idx, (before_branch, after_branch)) in b.iter().zip(a.iter()).enumerate() {
        diff_schema(
            command,
            &format!("{location}/{idx}"),
            before_branch,
            after_branch,
            diff,
        );
    }
}

/// Every member of a `required` array as a string — `None` when the
/// value is not an array or any member is not a string, so a malformed
/// schema trips the caller's "not a string array" breaking change
/// instead of having members silently ignored.
fn string_set(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|member| member.as_str().map(str::to_string))
        .collect()
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
    out.insert("export.config".into(), schema_of::<ConfigManifest>());
    out.insert("export.commands".into(), schema_of::<CommandsManifest>());

    // Introspection
    out.insert("status".into(), schema_of::<crate::status::StatusReport>());

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
/// `AnnotationGroup`) propagate to the outer schema root — the default
/// emission form, whose `$defs` names drive named-model codegen;
/// `export_envelope_schema(true)` re-emits the same model with every
/// reference inlined for `$ref`-naive generators.
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
                required: vec!["created".into()],
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into()],
                    required: vec!["decision_date".into()],
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
    fn required_is_emitted_verbatim_and_carries_no_floor() {
        // The loader guarantees `required` holds only authorable fields
        // (inferred built-ins are rejected at load), so the export emits
        // the configured list verbatim. And no emptiness floor is
        // smuggled in — check's per-field emptiness is idiosyncratic,
        // so flooring would reject documents check accepts.
        let mut c = cfg();
        c.schema.overrides.clear();
        c.schema.required = vec!["owner".into(), "created".into()];
        let v = serde_json::to_value(export_schema(&c)).unwrap();
        let p = &v["properties"];
        assert!(p["owner"].get("minLength").is_none());
        assert!(p["created"].get("minLength").is_none());

        let req: Vec<&str> = v["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        assert_eq!(req, vec!["owner", "created"], "configured list verbatim");
    }

    #[test]
    fn strict_schema_admits_every_declared_field() {
        // Strict mode's `additionalProperties: false` must forbid exactly
        // what UnknownFieldRule forbids. A field declared only in
        // `required` (no type/enum) still gets a permissive property —
        // leaving it out would make the exported schema reject documents
        // the same config's `check` accepts.
        let mut c = cfg();
        c.schema.mode = SchemaMode::Strict;
        c.schema.overrides.clear();
        c.schema.required = vec!["audit_ref".into()];
        let v = serde_json::to_value(export_schema(&c)).unwrap();
        assert_eq!(
            v["properties"]["audit_ref"],
            serde_json::json!({}),
            "required-only field is admitted permissively"
        );
        assert_eq!(v["additionalProperties"], serde_json::json!(false));
    }

    #[test]
    fn schema_keeps_the_enum_on_typed_and_builtin_fields() {
        // A field declared in BOTH `types` and `enums` (and a built-in
        // scalar with an enum) must export `type` AND `enum` — the old
        // intersect-only merge silently dropped the enum when the
        // property was seeded without one, letting the schema accept
        // values check's field_enum rejects. Typed fields carry enum
        // values in the field's JSON type.
        use std::collections::BTreeMap;
        let mut c = cfg();
        c.schema.overrides.clear();
        c.schema.types = BTreeMap::from([("priority".to_string(), FieldType::Integer)]);
        c.schema.enums = BTreeMap::from([
            ("priority".to_string(), vec!["1".into(), "2".into()]),
            ("owner".to_string(), vec!["alice".into(), "bob".into()]),
        ]);
        let v = serde_json::to_value(export_schema(&c)).unwrap();
        let props = &v["properties"];
        assert_eq!(props["priority"]["type"], "integer");
        assert_eq!(
            props["priority"]["enum"],
            serde_json::json!([1, 2]),
            "typed enum values are emitted in the field's JSON type"
        );
        assert_eq!(
            props["owner"]["enum"],
            serde_json::json!(["alice", "bob"]),
            "a built-in scalar's enum constraint survives the seed"
        );
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
        c.schema.required = vec!["owner".into()];
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
            vec!["decision_date", "owner"],
            "union of the global and override required sets"
        );
    }

    #[test]
    fn schema_required_includes_require_explicit_fields() {
        // `schema.require_explicit` forces inferrable built-ins to be
        // authored; the exported schema must mark them required so it
        // agrees with what the `explicit_field` rule enforces — else a
        // codegen consumer would generate `status` as optional when
        // `check` rejects a document that omits it.
        let mut c = cfg();
        c.schema.require_explicit = vec!["status".into()];
        let v = serde_json::to_value(export_schema(&c)).unwrap();
        let branch = &v["oneOf"][0];
        let required: Vec<&str> = branch["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        assert!(
            required.contains(&"status"),
            "require_explicit fields must appear in the exported `required`: {required:?}"
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
            "created": "2026-05-01",
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
            "created": "2026-05-01",
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
    fn export_rules_lists_unresolved_reference_row_with_params() {
        // An error-severity `[[detection.unresolved_policy]]` row is a
        // registered rule like any other per-block instance: listed
        // with `source: config`, `severity: error`, and its {cause,
        // glob} params. The default config (info row only) lists none —
        // the default manifest rule-id set is unchanged.
        let default_ids: Vec<String> = rule_ids(&export_rules(&Config::default()))
            .into_iter()
            .map(str::to_string)
            .collect();
        assert!(
            !default_ids
                .iter()
                .any(|id| id.starts_with("unresolved_reference/")),
            "default config registers no unresolved rule: {default_ids:?}"
        );

        let mut c = Config::default();
        c.detection.unresolved_policy = vec![crate::config::UnresolvedPolicyRuleConfig {
            name: "broken-docs-link".into(),
            cause: crate::model::UnresolvedCause::Missing,
            glob: Some("docs/**".into()),
            severity: crate::config::UnresolvedSeverity::Error,
        }];
        let m = export_rules(&c);
        let entry = m
            .rules
            .iter()
            .find(|r| r.id == "unresolved_reference/broken-docs-link")
            .expect("error row must appear in the manifest");
        assert!(matches!(entry.source, RuleSource::Config));
        assert_eq!(entry.severity, crate::rules::Severity::Error);
        assert_eq!(entry.params["cause"].as_str(), Some("missing"));
        assert_eq!(entry.params["glob"].as_str(), Some("docs/**"));
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

    /// The default ($defs-bundled) emission form — infallible by
    /// construction, since no reference inlining runs.
    fn envelope_manifest() -> EnvelopeSchemaManifest {
        export_envelope_schema(false).expect("the default emission form performs no inlining")
    }

    #[test]
    fn envelope_schema_carries_binary_version() {
        let m = envelope_manifest();
        assert_eq!(m.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn envelope_schema_includes_success_and_error_branches() {
        let m = envelope_manifest();
        // Generic envelope is a oneOf of {ok:true,data,...} and
        // {ok:false,error,...}. Two branches must exist regardless of
        // which command's data shape sits inside.
        let one_of = m.envelope.get("oneOf").and_then(Value::as_array).unwrap();
        assert_eq!(one_of.len(), 2);
    }

    #[test]
    fn envelope_schema_per_command_covers_every_query_subcommand() {
        let m = envelope_manifest();
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
        let m = envelope_manifest();
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
        let m = envelope_manifest();
        assert!(
            !m.per_command.contains_key("query.low-trust"),
            "query.low-trust must not be registered — retired in v0.13"
        );
    }

    #[test]
    fn envelope_schema_per_command_covers_lifecycle_actions() {
        let m = envelope_manifest();
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
        let m = envelope_manifest();
        for name in [
            "export.schema",
            "export.enums",
            "export.rules",
            "export.envelope-schema",
            "export.config",
            "export.commands",
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
        let m = envelope_manifest();
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
        let m = envelope_manifest();

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
    fn envelope_schema_validates_real_export_manifests() {
        // The blind spot that let `export.enums` publish `per_kind` as
        // required while serde omitted it: the `export.*` manifests were
        // never fed through their OWN per_command schema. Lock all four —
        // crucially `export.enums` under an OVERRIDE-FREE config, where
        // `per_kind` is empty and therefore absent from the JSON, so the
        // schema must mark it optional. A typed client codegen'd from the
        // schema must accept exactly this real payload.
        let mut plain = cfg();
        plain.schema.overrides.clear(); // override-free → per_kind omitted
        let m = envelope_manifest();
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "export.enums",
                serde_json::to_value(export_enums(&plain)).unwrap(),
            ),
            (
                "export.schema",
                serde_json::to_value(export_schema(&plain)).unwrap(),
            ),
            (
                "export.rules",
                serde_json::to_value(export_rules(&plain)).unwrap(),
            ),
            (
                "export.config",
                serde_json::to_value(export_config(&plain)).unwrap(),
            ),
        ];
        for (key, instance) in cases {
            assert!(
                instance.get("per_kind").is_none() || key != "export.enums",
                "override-free export.enums must omit per_kind to exercise the regression"
            );
            let schema = m
                .per_command
                .get(key)
                .unwrap_or_else(|| panic!("missing per_command entry for {key}"));
            let validator = jsonschema::draft202012::new(schema)
                .unwrap_or_else(|e| panic!("schema for {key} must compile: {e}"));
            assert!(
                validator.is_valid(&instance),
                "schema for {key} rejected its own real manifest: {:?}",
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
        let m = envelope_manifest();
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
        let m = envelope_manifest();
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
        let m = envelope_manifest();
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
            parse_failures: vec![crate::model::ParseFailure {
                path: "docs/bad.md".into(),
                message: "parse error at docs/bad.md: yaml".into(),
                content_hash: "abc".into(),
            }],
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
        let m = envelope_manifest();
        let schema = m.per_command.get("check").expect("check entry");
        let validator = jsonschema::draft202012::new(schema).expect("check schema must compile");
        let instance = serde_json::to_value(CheckResult {
            violations: vec![],
            skipped_rules: vec![],
            total: 0,
            has_errors: false,
            proposals: None,
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
        let m = envelope_manifest();
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

    // ─── export_config ─────────────────────────────────────────────────

    #[test]
    fn export_config_resolves_defaults() {
        // The manifest's whole value over raw TOML parsing: an omitted
        // key shows its real resolved default, and the code-level
        // fallbacks surface as data.
        let m = export_config(&Config::default());
        assert_eq!(m.scope.include, vec!["**/*.md".to_string()]);
        assert_eq!(m.output.dir, "_index");
        assert_eq!(
            m.initial_status,
            Config::default().statuses.allowed[0],
            "no declared initial → the first allowed status"
        );
        assert_eq!(m.identity.fallback_kind, "generic");
        assert_eq!(m.identity.fallback_id_template, "{kind}-{stem}");
        assert_eq!(m.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn export_config_initial_status_honors_declared_initial() {
        let mut c = Config::default();
        c.statuses.initial = Some("superseded".into());
        assert_eq!(export_config(&c).initial_status, "superseded");
    }

    // ─── inline_schema_refs ────────────────────────────────────────────

    fn assert_no_refs(value: &Value, context: &str) {
        match value {
            Value::Object(map) => {
                for (key, nested) in map {
                    assert!(
                        key != "$ref" && key != "$defs",
                        "{context} still carries `{key}`: {nested}"
                    );
                    assert_no_refs(nested, context);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| assert_no_refs(item, context)),
            _ => {}
        }
    }

    #[test]
    fn inlined_envelope_schema_entries_are_self_contained() {
        // The `--inline-refs` promise: every per_command entry and the
        // envelope carry no `$ref` / `$defs` at any depth AND still
        // compile under a real draft 2020-12 validator.
        let m = export_envelope_schema(true).expect("shipped types inline cleanly");
        for (name, schema) in &m.per_command {
            assert_no_refs(schema, name);
            let _ = jsonschema::draft202012::new(schema).unwrap_or_else(|e| {
                panic!("inlined schema for {name:?} is not valid draft 2020-12: {e}")
            });
        }
        assert_no_refs(&m.envelope, "envelope");
        let _ = jsonschema::draft202012::new(&m.envelope)
            .expect("inlined envelope schema must validate as draft 2020-12");
    }

    #[test]
    fn inlined_and_default_forms_validate_identical_instances() {
        // Two emission forms of ONE canonical model: a real instance a
        // command emits must validate identically under both.
        let issue_instance = serde_json::json!({
            "orphans": [],
            "stale": [],
            "unresolved_edges": [],
            "violations": [],
            "skipped_rules": [],
            "summary": { "total": 0, "by_category": {} }
        });
        for inline in [false, true] {
            let m = export_envelope_schema(inline).expect("emission succeeds");
            let schema = m
                .per_command
                .get("query.issues")
                .expect("query.issues entry");
            let validator = jsonschema::draft202012::new(schema)
                .unwrap_or_else(|e| panic!("query.issues (inline={inline}) must compile: {e}"));
            assert!(
                validator.is_valid(&issue_instance),
                "query.issues (inline={inline}) rejected a real instance: {:?}",
                validator
                    .iter_errors(&issue_instance)
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn inline_schema_refs_fails_closed_on_cycle() {
        // A definition ring is refused with a typed error naming the
        // ring — never truncated, never silently retained.
        let entry = serde_json::json!({
            "type": "object",
            "properties": { "a": { "$ref": "#/$defs/A" } },
            "$defs": {
                "A": { "properties": { "b": { "$ref": "#/$defs/B" } } },
                "B": { "$ref": "#/$defs/A" }
            }
        });
        let err = inline_schema_refs(&entry).unwrap_err();
        let Error::Cycle { chain } = err else {
            panic!("expected Error::Cycle, got {err:?}");
        };
        assert!(
            chain.contains(&"A".to_string()) && chain.contains(&"B".to_string()),
            "the chain names the ring: {chain:?}"
        );
    }

    #[test]
    fn inline_schema_refs_fails_closed_on_unresolvable_ref() {
        let missing = serde_json::json!({
            "properties": { "a": { "$ref": "#/$defs/Ghost" } },
            "$defs": {}
        });
        let err = inline_schema_refs(&missing).unwrap_err();
        assert!(
            matches!(&err, Error::Config(msg) if msg.contains("unresolvable")),
            "missing definition must refuse: {err:?}"
        );

        let external = serde_json::json!({
            "properties": { "a": { "$ref": "https://example.com/schema.json" } }
        });
        let err = inline_schema_refs(&external).unwrap_err();
        assert!(
            matches!(&err, Error::Config(msg) if msg.contains("unresolvable")),
            "non-$defs reference must refuse: {err:?}"
        );
    }

    // ─── compute_envelope_schema_diff ──────────────────────────────────

    fn schema_payload(per_command: Value) -> Value {
        serde_json::json!({
            "version": "0.15.0",
            "envelope": {},
            "per_command": per_command
        })
    }

    fn build_entry() -> Value {
        serde_json::json!({
            "build": {
                "type": "object",
                "properties": {
                    "nodes": { "type": "integer" },
                    "mode": { "type": "string", "enum": ["fast", "full"] }
                },
                "required": ["nodes"]
            }
        })
    }

    #[test]
    fn schema_diff_classifies_removals_and_tightenings_as_breaking() {
        let baseline = schema_payload(build_entry());

        // per_command key removed.
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(serde_json::json!({})));
        assert_eq!(d.breaking.len(), 1, "{:?}", d.breaking);
        assert!(d.breaking[0].message.contains("removed"));

        // Property removed.
        let mut head = build_entry();
        head["build"]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("mode");
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(
            d.breaking
                .iter()
                .any(|c| c.message.contains("property `mode` removed")),
            "{:?}",
            d.breaking
        );

        // Type changed.
        let mut head = build_entry();
        head["build"]["properties"]["nodes"]["type"] = "string".into();
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(
            d.breaking
                .iter()
                .any(|c| c.message.contains("type changed")),
            "{:?}",
            d.breaking
        );

        // Required loosened — output polarity: a member leaving
        // `required` withdraws a presence guarantee consumers rely on.
        let mut head = build_entry();
        head["build"]["required"] = serde_json::json!([]);
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(d.additive.is_empty(), "{:?}", d.additive);
        assert!(
            d.breaking
                .iter()
                .any(|c| c.message.contains("required no longer lists `nodes`")),
            "{:?}",
            d.breaking
        );

        // Enum grown — output polarity: a new emitted value escapes
        // every consumer's exhaustive match.
        let mut head = build_entry();
        head["build"]["properties"]["mode"]["enum"] = serde_json::json!(["fast", "full", "verify"]);
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(d.additive.is_empty(), "{:?}", d.additive);
        assert!(
            d.breaking
                .iter()
                .any(|c| c.message.contains(r#"enum value "verify" added"#)),
            "{:?}",
            d.breaking
        );

        // additionalProperties tightened (absent = true → false).
        let mut head = build_entry();
        head["build"]["additionalProperties"] = serde_json::json!(false);
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(
            d.breaking.iter().any(|c| c.message.contains("tightened")),
            "{:?}",
            d.breaking
        );
    }

    #[test]
    fn schema_diff_classifies_widenings_as_additive() {
        let baseline = schema_payload(build_entry());

        // per_command key added.
        let mut head = build_entry();
        head["status"] = serde_json::json!({ "type": "object" });
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(d.breaking.is_empty(), "{:?}", d.breaking);
        assert!(d.additive.iter().any(|c| c.message.contains("added")));

        // Optional property added.
        let mut head = build_entry();
        head["build"]["properties"]["edges"] = serde_json::json!({ "type": "integer" });
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(d.breaking.is_empty(), "{:?}", d.breaking);
        assert!(
            d.additive
                .iter()
                .any(|c| c.message.contains("optional property `edges` added")),
            "{:?}",
            d.additive
        );

        // New required property — output polarity: a typed client
        // ignores fields it does not model, and the output gains a
        // presence guarantee. Both attribution entries (the property
        // itself and its `required` join) are additive.
        let mut head = build_entry();
        head["build"]["properties"]["edges"] = serde_json::json!({ "type": "integer" });
        head["build"]["required"] = serde_json::json!(["nodes", "edges"]);
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(d.breaking.is_empty(), "{:?}", d.breaking);
        assert!(
            d.additive
                .iter()
                .any(|c| c.message.contains("required property `edges` added")),
            "{:?}",
            d.additive
        );
        assert!(
            d.additive
                .iter()
                .any(|c| c.message.contains("required gained `edges`")),
            "{:?}",
            d.additive
        );

        // Existing property joins `required` — output polarity: the
        // output now guarantees a field it previously emitted
        // optionally; consumers lose nothing.
        let mut head = build_entry();
        head["build"]["required"] = serde_json::json!(["nodes", "mode"]);
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(d.breaking.is_empty(), "{:?}", d.breaking);
        assert!(
            d.additive
                .iter()
                .any(|c| c.message.contains("required gained `mode`")),
            "{:?}",
            d.additive
        );

        // Enum shrunk — output polarity: the emitted set narrows to
        // values every existing consumer already covers.
        let mut head = build_entry();
        head["build"]["properties"]["mode"]["enum"] = serde_json::json!(["fast"]);
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(d.breaking.is_empty(), "{:?}", d.breaking);
        assert!(
            d.additive
                .iter()
                .any(|c| c.message.contains(r#"enum value "full" removed"#)),
            "{:?}",
            d.additive
        );
    }

    #[test]
    fn schema_diff_ignores_metadata_only_changes() {
        let baseline = schema_payload(build_entry());
        let mut head_payload = schema_payload(build_entry());
        head_payload["version"] = "0.99.0".into();
        head_payload["per_command"]["build"]["description"] = "reworded prose".into();
        head_payload["per_command"]["build"]["properties"]["nodes"]["title"] = "Nodes".into();
        let d = compute_envelope_schema_diff(&baseline, &head_payload);
        assert!(d.breaking.is_empty(), "{:?}", d.breaking);
        assert!(d.additive.is_empty(), "{:?}", d.additive);
    }

    #[test]
    fn schema_diff_treats_unclassifiable_constructs_as_breaking() {
        // A keyword the classifier has no positive rule for — here a
        // `format` change — must land in breaking, never be dropped.
        let baseline = schema_payload(build_entry());
        let mut head = build_entry();
        head["build"]["properties"]["nodes"]["format"] = "int64".into();
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(
            d.breaking
                .iter()
                .any(|c| c.message.contains("not positively classifiable")),
            "{:?}",
            d.breaking
        );
        assert!(d.additive.is_empty(), "{:?}", d.additive);
    }

    #[test]
    fn schema_diff_flags_oneof_branch_count_changes_as_breaking() {
        let baseline = schema_payload(serde_json::json!({
            "report": { "oneOf": [ { "type": "string" }, { "type": "integer" } ] }
        }));
        let head = schema_payload(serde_json::json!({
            "report": { "oneOf": [ { "type": "string" } ] }
        }));
        let d = compute_envelope_schema_diff(&baseline, &head);
        assert!(
            d.breaking
                .iter()
                .any(|c| c.message.contains("branch count changed")),
            "{:?}",
            d.breaking
        );
    }

    #[test]
    fn schema_diff_treats_missing_envelope_as_breaking() {
        // The envelope wrapper is half the contract: its absence on
        // either side — or both — is a malformed payload, and the gate
        // fails closed instead of silently skipping the comparison.
        let with_envelope = schema_payload(build_entry());
        let mut without_envelope = schema_payload(build_entry());
        without_envelope.as_object_mut().unwrap().remove("envelope");

        let d = compute_envelope_schema_diff(&with_envelope, &without_envelope);
        assert!(
            d.breaking
                .iter()
                .any(|c| c.command == "envelope" && c.message.contains("removed")),
            "{:?}",
            d.breaking
        );

        let d = compute_envelope_schema_diff(&without_envelope, &with_envelope);
        assert!(
            d.breaking
                .iter()
                .any(|c| c.command == "envelope" && c.message.contains("added")),
            "{:?}",
            d.breaking
        );

        let d = compute_envelope_schema_diff(&without_envelope, &without_envelope);
        assert!(
            d.breaking
                .iter()
                .any(|c| c.command == "envelope" && c.message.contains("absent from both")),
            "{:?}",
            d.breaking
        );
    }

    #[test]
    fn schema_diff_refuses_required_with_non_string_member() {
        // A `required` array carrying a non-string member is a malformed
        // schema: every member counts, so the diff reports the array as
        // not-a-string-array (breaking) instead of comparing only the
        // string members and waving the rest through.
        let baseline = schema_payload(build_entry());
        let mut head = build_entry();
        head["build"]["required"] = serde_json::json!(["nodes", 3]);
        let d = compute_envelope_schema_diff(&baseline, &schema_payload(head));
        assert!(
            d.breaking
                .iter()
                .any(|c| c.message.contains("`required` is not a string array")),
            "{:?}",
            d.breaking
        );
    }

    #[test]
    fn inline_value_merges_ref_target_into_sibling_all_of() {
        // A `$ref` node that also carries an `allOf` sibling keeps every
        // constraint: the resolved target joins the existing conjunction
        // alongside the authored branches and the other siblings.
        let entry = serde_json::json!({
            "properties": {
                "a": {
                    "$ref": "#/$defs/A",
                    "allOf": [ { "minLength": 1 } ],
                    "description": "doc"
                }
            },
            "$defs": { "A": { "type": "string" } }
        });
        let inlined = inline_schema_refs(&entry).expect("inlines");
        let merged = inlined
            .pointer("/properties/a/allOf")
            .and_then(Value::as_array)
            .expect("allOf survives as an array");
        assert_eq!(merged.len(), 2, "target + authored branch: {merged:?}");
        assert!(
            merged.contains(&serde_json::json!({ "type": "string" })),
            "the resolved target is in the conjunction: {merged:?}"
        );
        assert!(
            merged.contains(&serde_json::json!({ "minLength": 1 })),
            "the authored branch is preserved: {merged:?}"
        );
        assert_eq!(
            inlined.pointer("/properties/a/description"),
            Some(&serde_json::json!("doc")),
            "other siblings survive: {inlined}"
        );

        // A non-array `allOf` sibling is malformed input — refused, never
        // silently replaced.
        let malformed = serde_json::json!({
            "properties": {
                "a": { "$ref": "#/$defs/A", "allOf": { "minLength": 1 } }
            },
            "$defs": { "A": { "type": "string" } }
        });
        let err = inline_schema_refs(&malformed).unwrap_err();
        assert!(
            matches!(&err, Error::Config(msg) if msg.contains("allOf")),
            "non-array allOf must refuse: {err:?}"
        );
    }
}
