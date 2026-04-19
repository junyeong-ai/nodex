use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};

/// Root configuration deserialized from `nodex.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub scope: ScopeConfig,
    #[serde(default)]
    pub kinds: KindsConfig,
    #[serde(default)]
    pub statuses: StatusesConfig,
    #[serde(default)]
    pub identity: IdentityConfig,
    #[serde(default)]
    pub schema: SchemaConfig,
    #[serde(default)]
    pub rules: RulesConfig,
    #[serde(default)]
    pub parser: ParserConfig,
    #[serde(default)]
    pub detection: DetectionConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub report: ReportConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeConfig {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub conditional_exclude: Vec<ConditionalExclude>,
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            include: vec!["**/*.md".to_string()],
            exclude: vec![],
            conditional_exclude: vec![],
        }
    }
}

/// When a file matching `parent_glob` satisfies `condition` (today the
/// only supported condition is `status_terminal`), every other file in
/// the parent's directory is dropped from scan scope. The parent itself
/// stays in scope so it still parses into the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalExclude {
    pub parent_glob: String,
    #[serde(default = "default_condition")]
    pub condition: String,
}

fn default_condition() -> String {
    "status_terminal".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindsConfig {
    #[serde(default = "default_kinds")]
    pub allowed: Vec<String>,
}

impl Default for KindsConfig {
    fn default() -> Self {
        Self {
            allowed: default_kinds(),
        }
    }
}

fn default_kinds() -> Vec<String> {
    ["generic", "guide", "readme"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusesConfig {
    #[serde(default = "default_statuses")]
    pub allowed: Vec<String>,
    #[serde(default = "default_terminal")]
    pub terminal: Vec<String>,
}

impl Default for StatusesConfig {
    fn default() -> Self {
        Self {
            allowed: default_statuses(),
            terminal: default_terminal(),
        }
    }
}

fn default_statuses() -> Vec<String> {
    [
        "active",
        "superseded",
        "archived",
        "deprecated",
        "abandoned",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_terminal() -> Vec<String> {
    ["superseded", "archived", "deprecated", "abandoned"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdentityConfig {
    #[serde(default)]
    pub kind_rules: Vec<KindRule>,
    #[serde(default)]
    pub id_rules: Vec<IdRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindRule {
    pub glob: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdRule {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub glob: Option<String>,
    pub template: String,
}

/// Document-schema constraints.
///
/// Top-level entries (`required`, `types`, `enums`, `cross_field`)
/// apply to **every** document. Per-kind tightening is expressed in
/// `overrides`; rules combine the global set with the first matching
/// override so kinds inherit a project-wide baseline without ceremony.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaConfig {
    #[serde(default = "default_required")]
    pub required: Vec<String>,
    #[serde(default)]
    pub types: BTreeMap<String, FieldType>,
    #[serde(default)]
    pub enums: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub cross_field: Vec<CrossFieldSpec>,
    #[serde(default)]
    pub overrides: Vec<SchemaOverride>,
}

impl Default for SchemaConfig {
    fn default() -> Self {
        Self {
            required: default_required(),
            types: BTreeMap::new(),
            enums: BTreeMap::new(),
            cross_field: vec![],
            overrides: vec![],
        }
    }
}

fn default_required() -> Vec<String> {
    ["id", "title", "kind", "status"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Per-kind schema constraints.
///
/// Every field except `kinds` and `required` defaults to an empty
/// collection, and each corresponding rule short-circuits when empty.
/// Projects that never configure these keep today's behaviour verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaOverride {
    pub kinds: Vec<String>,
    pub required: Vec<String>,
    #[serde(default)]
    pub types: BTreeMap<String, FieldType>,
    #[serde(default)]
    pub enums: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub cross_field: Vec<CrossFieldSpec>,
}

/// Accepted frontmatter field types. Covers the scalars that actually
/// appear in document frontmatter. Add a variant when a real need arises —
/// the `match` statement in the validator will force every consumer to
/// acknowledge the new type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Integer,
    Bool,
    Date,
}

/// Conditional field requirement: "when LHS predicate holds, `require` must be present".
///
/// v1 parser accepts only `"<field>=<value>"` equality. Extending to new
/// predicates (e.g. `in`, `matches`) happens by versioning the `when`
/// string into a richer type, without invalidating existing configs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossFieldSpec {
    pub when: String,
    pub require: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RulesConfig {
    #[serde(default)]
    pub naming: Vec<NamingRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamingRule {
    pub glob: String,
    pub pattern: String,
    #[serde(default)]
    pub sequential: bool,
    #[serde(default)]
    pub unique: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParserConfig {
    #[serde(default)]
    pub link_patterns: Vec<LinkPattern>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkPattern {
    pub pattern: String,
    pub relation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionConfig {
    #[serde(default = "default_stale_days")]
    pub stale_days: u32,
    #[serde(default = "default_orphan_grace_days")]
    pub orphan_grace_days: u32,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            stale_days: default_stale_days(),
            orphan_grace_days: default_orphan_grace_days(),
        }
    }
}

fn default_stale_days() -> u32 {
    180
}

fn default_orphan_grace_days() -> u32 {
    14
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    #[serde(default = "default_output_dir")]
    pub dir: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            dir: default_output_dir(),
        }
    }
}

fn default_output_dir() -> String {
    "_index".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportConfig {
    #[serde(default = "default_report_title")]
    pub title: String,
    #[serde(default = "default_god_node_display_limit")]
    pub god_node_display_limit: usize,
    #[serde(default = "default_display_limit")]
    pub orphan_display_limit: usize,
    #[serde(default = "default_display_limit")]
    pub stale_display_limit: usize,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            title: default_report_title(),
            god_node_display_limit: default_god_node_display_limit(),
            orphan_display_limit: default_display_limit(),
            stale_display_limit: default_display_limit(),
        }
    }
}

fn default_report_title() -> String {
    "Document Graph".to_string()
}

fn default_god_node_display_limit() -> usize {
    10
}

fn default_display_limit() -> usize {
    20
}

impl Config {
    /// Load config from a `nodex.toml` file. Returns default config if not found.
    ///
    /// Config is validated for internal consistency before it is returned,
    /// so downstream code can assume that `enums` / `cross_field` references
    /// are well-formed.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("nodex.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path).map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })?;
        let config: Self =
            toml::from_str(&content).map_err(|e| Error::Config(format!("{path:?}: {e}")))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate internal consistency. Called automatically by `load()`.
    ///
    /// Rejects definitions that would otherwise only surface as
    /// confusing runtime behaviour:
    /// - `enums` on collection-valued built-in fields (`tags`,
    ///   `supersedes`, `implements`, `related`) — these cannot be
    ///   validated against a scalar set, so silent ignore would trap
    ///   users who typed the obvious syntax and saw no effect.
    /// - `enums.status` / `enums.kind` values that are not in the
    ///   corresponding global `allowed` list.
    /// - `cross_field.when` expressions that don't parse.
    /// - `cross_field.when`'s LHS and `cross_field.require` referring
    ///   to a field name that is not a built-in scalar and is not
    ///   declared in the override's `types` / `enums` / `required`.
    pub fn validate(&self) -> Result<()> {
        // Refuse structurally-broken configs: empty `kinds.allowed`
        // means every document would be kind-less (inference falls
        // back to "generic") yet no kind would ever be valid — either
        // the user is mis-configured or they meant "accept all kinds"
        // (which is the default when the key is omitted entirely).
        if self.kinds.allowed.is_empty() {
            return Err(Error::Config(
                "kinds.allowed must not be empty; omit the key to accept the defaults, \
                 or list every kind your project uses"
                    .to_string(),
            ));
        }

        // Same rationale as `kinds.allowed`: an empty `statuses.allowed`
        // would make every status value invalid and break scaffolding,
        // which picks the first allowed status for the initial value.
        if self.statuses.allowed.is_empty() {
            return Err(Error::Config(
                "statuses.allowed must not be empty; omit the key to accept the defaults, \
                 or list every status your project uses"
                    .to_string(),
            ));
        }

        // `nodex lifecycle <action>` writes a fixed target status per
        // action (supersede → "superseded", archive → "archived", …).
        // If the project's `statuses.allowed` omits any of those, a
        // lifecycle transition would silently produce a document that
        // then fails enum validation. Surface the mismatch at load time
        // instead, with a message pointing at the exact missing values.
        let missing: Vec<&str> = crate::lifecycle::LIFECYCLE_TARGET_STATUSES
            .iter()
            .copied()
            .filter(|s| !self.statuses.allowed.iter().any(|a| a == s))
            .collect();
        if !missing.is_empty() {
            return Err(Error::Config(format!(
                "statuses.allowed is missing lifecycle target status(es): {missing:?}; \
                 add them to `statuses.allowed` or omit the key to accept the defaults"
            )));
        }

        self.validate_block(
            "schema",
            &self.schema.required,
            &self.schema.types,
            &self.schema.enums,
            &self.schema.cross_field,
        )?;

        // Validate naming rules at load time rather than silently
        // skipping invalid patterns at check time — a typo in a glob
        // or regex would otherwise validate zero files forever.
        for (idx, nr) in self.rules.naming.iter().enumerate() {
            if globset::Glob::new(&nr.glob).is_err() {
                return Err(Error::Config(format!(
                    "rules.naming[{idx}].glob {:?} is not a valid glob",
                    nr.glob
                )));
            }
            if regex::Regex::new(&nr.pattern).is_err() {
                return Err(Error::Config(format!(
                    "rules.naming[{idx}].pattern {:?} is not a valid regex",
                    nr.pattern
                )));
            }
        }

        for (idx, ov) in self.schema.overrides.iter().enumerate() {
            let ctx = format!("schema.overrides[{idx}] (kinds={:?})", ov.kinds);
            self.validate_block(&ctx, &ov.required, &ov.types, &ov.enums, &ov.cross_field)?;
            // Reject cross_field entries that duplicate a global entry.
            // `cross_field_for` accumulates global + override — if a
            // user copy-pastes the same rule into both slots, every
            // matching node would get two violations. Fail loud at
            // load time rather than debug silently.
            for cf in &ov.cross_field {
                if self
                    .schema
                    .cross_field
                    .iter()
                    .any(|g| g.when == cf.when && g.require == cf.require)
                {
                    return Err(Error::Config(format!(
                        "{ctx}: cross_field {{ when={:?}, require={:?} }} \
                         is already declared in [schema].cross_field — \
                         remove the override copy or change its predicate",
                        cf.when, cf.require
                    )));
                }
            }
        }
        Ok(())
    }

    /// Validate one schema block (the global [schema] or one override).
    /// Extracted so both share the same rules.
    fn validate_block(
        &self,
        ctx: &str,
        required: &[String],
        types: &BTreeMap<String, FieldType>,
        enums: &BTreeMap<String, Vec<String>>,
        cross_field: &[CrossFieldSpec],
    ) -> Result<()> {
        for (field, allowed) in enums {
            if is_collection_builtin(field) {
                return Err(Error::Config(format!(
                    "{ctx}: enums.{field} — collection-valued built-in \
                     fields cannot have a scalar enum constraint"
                )));
            }
            let global = match field.as_str() {
                "status" => Some((&self.statuses.allowed, "statuses.allowed")),
                "kind" => Some((&self.kinds.allowed, "kinds.allowed")),
                _ => None,
            };
            if let Some((global, key)) = global {
                for value in allowed {
                    if !global.contains(value) {
                        return Err(Error::Config(format!(
                            "{ctx}: enums.{field} contains {value:?} \
                             which is not in {key}"
                        )));
                    }
                }
            }

            // A narrowing enum on `status` — whether at the global
            // `[schema]` level or inside a `[[schema.overrides]]` block —
            // must still cover the four lifecycle target statuses.
            // Otherwise `nodex lifecycle <action>` on a matching document
            // would write a status value that immediately fails its own
            // enum validation, producing a config the tool can mutate
            // only by violating itself.
            if field == "status" {
                let missing: Vec<&str> = crate::lifecycle::LIFECYCLE_TARGET_STATUSES
                    .iter()
                    .copied()
                    .filter(|s| !allowed.iter().any(|a| a == s))
                    .collect();
                if !missing.is_empty() {
                    return Err(Error::Config(format!(
                        "{ctx}: enums.status narrows below the lifecycle target set; \
                         missing {missing:?}. Either include all four \
                         (superseded, archived, deprecated, abandoned) or drop \
                         the enum constraint on status"
                    )));
                }
            }
        }

        for cf in cross_field {
            let predicate = parse_when(&cf.when).map_err(|e| {
                Error::Config(format!("{ctx}: cross_field.when {:?}: {e}", cf.when))
            })?;
            let WhenPredicate::Equals { field, .. } = &predicate;
            ensure_field_known(field, required, types, enums, ctx, "cross_field.when")?;
            ensure_field_known(
                &cf.require,
                required,
                types,
                enums,
                ctx,
                "cross_field.require",
            )?;
        }
        Ok(())
    }

    /// Merged view: return every field-type constraint that applies to
    /// a given kind (global + first matching override). Scaffold and
    /// rules use this so every declared constraint is honoured once.
    pub fn types_for(&self, kind: &str) -> BTreeMap<String, FieldType> {
        let mut out = self.schema.types.clone();
        if let Some(ov) = self.schema_override_for(kind) {
            for (k, v) in &ov.types {
                out.insert(k.clone(), *v);
            }
        }
        out
    }

    /// Merged view: every enum constraint that applies to a given kind.
    pub fn enums_for(&self, kind: &str) -> BTreeMap<String, Vec<String>> {
        let mut out = self.schema.enums.clone();
        if let Some(ov) = self.schema_override_for(kind) {
            for (k, v) in &ov.enums {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    /// Merged view: every cross-field constraint that applies to a
    /// given kind. Global and override entries accumulate; an override
    /// never silently drops a global rule.
    pub fn cross_field_for(&self, kind: &str) -> Vec<CrossFieldSpec> {
        let mut out = self.schema.cross_field.clone();
        if let Some(ov) = self.schema_override_for(kind) {
            out.extend_from_slice(&ov.cross_field);
        }
        out
    }

    /// Check whether a status string is terminal.
    pub fn is_terminal(&self, status: &str) -> bool {
        self.statuses.terminal.iter().any(|t| t == status)
    }

    /// Get required fields for a given kind. Falls back to the global
    /// `schema.required` list when no override matches.
    pub fn required_for(&self, kind: &str) -> &[String] {
        for ov in &self.schema.overrides {
            if ov.kinds.iter().any(|k| k == kind) {
                return &ov.required;
            }
        }
        &self.schema.required
    }

    /// Find the schema override that applies to a given kind, if any.
    pub fn schema_override_for(&self, kind: &str) -> Option<&SchemaOverride> {
        self.schema
            .overrides
            .iter()
            .find(|ov| ov.kinds.iter().any(|k| k == kind))
    }
}

/// Parsed `cross_field.when` predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhenPredicate {
    /// `<field>=<value>` — match when the given field equals the value exactly.
    Equals { field: String, value: String },
}

/// Every built-in scalar field on `Node`. Kept here (not on `Node`) so
/// config validation sees the canonical list without pulling in the
/// whole model module. Collections (`tags`, `supersedes`, etc.) are
/// intentionally excluded — they cannot be members of a scalar enum.
pub const BUILTIN_SCALAR_FIELDS: &[&str] = &[
    "id",
    "title",
    "kind",
    "status",
    "created",
    "updated",
    "reviewed",
    "owner",
    "superseded_by",
];

/// Collection-valued built-in fields. Enum/type constraints on these
/// must be rejected — there is no single scalar value to check.
pub const BUILTIN_COLLECTION_FIELDS: &[&str] = &["tags", "supersedes", "implements", "related"];

/// True when `field` is one of the built-in `Node` fields of any kind.
pub fn is_builtin_node_field(field: &str) -> bool {
    BUILTIN_SCALAR_FIELDS.contains(&field) || BUILTIN_COLLECTION_FIELDS.contains(&field)
}

/// True when `field` is a built-in collection-valued field.
pub fn is_collection_builtin(field: &str) -> bool {
    BUILTIN_COLLECTION_FIELDS.contains(&field)
}

/// Reject field names in `cross_field.when` / `cross_field.require`
/// that are not built-in and not explicitly declared in the current
/// schema block. Keeps typos from turning into silently-skipped checks.
fn ensure_field_known(
    field: &str,
    required: &[String],
    types: &BTreeMap<String, FieldType>,
    enums: &BTreeMap<String, Vec<String>>,
    ctx: &str,
    slot: &str,
) -> Result<()> {
    if is_builtin_node_field(field)
        || required.iter().any(|r| r == field)
        || types.contains_key(field)
        || enums.contains_key(field)
    {
        return Ok(());
    }
    Err(Error::Config(format!(
        "{ctx}: {slot} references unknown field {field:?}; declare it \
         in required / types / enums or use a built-in name"
    )))
}

/// Parse a `cross_field.when` expression. v1 accepts only `field=value`.
///
/// Rejects `==` and any form where the value starts with `=`, so a typo
/// can never silently turn into a predicate that matches nothing. Also
/// rejects empty LHS / RHS and expressions with multiple top-level `=`.
pub fn parse_when(raw: &str) -> std::result::Result<WhenPredicate, String> {
    let trimmed = raw.trim();
    let parts: Vec<&str> = trimmed.splitn(3, '=').collect();
    if parts.len() != 2 {
        return Err(format!(
            "expected exactly one '=' in <field>=<value>; values with \
             embedded '=' are not supported in v1 (got {raw:?})"
        ));
    }
    let field = parts[0].trim();
    let value = parts[1].trim();
    if field.is_empty() || value.is_empty() {
        return Err("expected non-empty <field>=<value>".to_string());
    }
    if value.starts_with('=') {
        return Err("value must not start with '=' (use a single '=' separator)".to_string());
    }
    Ok(WhenPredicate::Equals {
        field: field.to_string(),
        value: value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_when_accepts_simple_equality() {
        let p = parse_when("status=superseded").unwrap();
        assert_eq!(
            p,
            WhenPredicate::Equals {
                field: "status".into(),
                value: "superseded".into()
            }
        );
    }

    #[test]
    fn parse_when_trims_whitespace() {
        let p = parse_when("  status  =  superseded  ").unwrap();
        let WhenPredicate::Equals { field, value } = p;
        assert_eq!(field, "status");
        assert_eq!(value, "superseded");
    }

    #[test]
    fn parse_when_rejects_double_equals() {
        assert!(parse_when("status==foo").is_err());
    }

    #[test]
    fn parse_when_rejects_empty_sides() {
        assert!(parse_when("=foo").is_err());
        assert!(parse_when("field=").is_err());
        assert!(parse_when("").is_err());
    }

    #[test]
    fn parse_when_rejects_triple_equals() {
        assert!(parse_when("a=b=c").is_err());
    }

    fn override_with(kind: &str, mut ov: SchemaOverride) -> Config {
        ov.kinds = vec![kind.into()];
        Config {
            schema: SchemaConfig {
                overrides: vec![ov],
                ..Default::default()
            },
            ..Config::default()
        }
    }

    #[test]
    fn validate_rejects_enum_on_collection_field() {
        let config = override_with(
            "adr",
            SchemaOverride {
                kinds: vec![],
                required: vec![],
                types: BTreeMap::new(),
                enums: [("tags".to_string(), vec!["foo".into()])]
                    .into_iter()
                    .collect(),
                cross_field: vec![],
            },
        );
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("collection-valued"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_enum_value_outside_global_allowed() {
        // `statuses.allowed` must cover the four lifecycle target
        // statuses (superseded / archived / deprecated / abandoned);
        // include them so this test isolates the "enum value outside
        // allowed" check rather than tripping the lifecycle-coverage
        // check first.
        let config = Config {
            statuses: StatusesConfig {
                allowed: vec![
                    "active".into(),
                    "superseded".into(),
                    "archived".into(),
                    "deprecated".into(),
                    "abandoned".into(),
                ],
                terminal: vec![],
            },
            schema: SchemaConfig {
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into()],
                    required: vec![],
                    types: BTreeMap::new(),
                    enums: [("status".to_string(), vec!["active".into(), "bogus".into()])]
                        .into_iter()
                        .collect(),
                    cross_field: vec![],
                }],
                ..Default::default()
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("bogus"));
                assert!(msg.contains("statuses.allowed"));
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_cross_field_unknown_field() {
        let config = override_with(
            "adr",
            SchemaOverride {
                kinds: vec![],
                required: vec![],
                types: BTreeMap::new(),
                enums: BTreeMap::new(),
                cross_field: vec![CrossFieldSpec {
                    when: "statuz=superseded".into(),
                    require: "superseded_by".into(),
                }],
            },
        );
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("unknown field"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_error_includes_override_context() {
        let config = Config {
            schema: SchemaConfig {
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into(), "guide".into()],
                    required: vec![],
                    types: BTreeMap::new(),
                    enums: [("tags".to_string(), vec!["x".into()])]
                        .into_iter()
                        .collect(),
                    cross_field: vec![],
                }],
                ..Default::default()
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("overrides[0]"));
                assert!(msg.contains("\"adr\""));
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_empty_schema() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn validate_rejects_statuses_allowed_missing_lifecycle_target() {
        // Omitting "archived" would let `nodex lifecycle archive` write
        // a status value the rest of the project's config treats as
        // invalid. The config must fail fast at load time.
        let config = Config {
            statuses: StatusesConfig {
                allowed: vec![
                    "active".into(),
                    "superseded".into(),
                    "deprecated".into(),
                    "abandoned".into(),
                ],
                terminal: vec!["superseded".into()],
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("archived"), "message was: {msg}");
                assert!(msg.contains("lifecycle"), "message was: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_override_status_enum_missing_lifecycle_target() {
        // An override enum that narrows `status` below the four
        // lifecycle targets would let `nodex lifecycle archive` on a
        // matching kind write a status the config's own enum then
        // rejects — the tool mutating itself into invalidity. Refuse
        // at load.
        let config = Config {
            schema: SchemaConfig {
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into()],
                    required: vec![],
                    types: BTreeMap::new(),
                    enums: [(
                        "status".to_string(),
                        vec!["active".into(), "superseded".into()],
                    )]
                    .into_iter()
                    .collect(),
                    cross_field: vec![],
                }],
                ..Default::default()
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("archived"), "message was: {msg}");
                assert!(msg.contains("lifecycle"), "message was: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn global_cross_field_applies_without_override() {
        let config = Config {
            schema: SchemaConfig {
                cross_field: vec![CrossFieldSpec {
                    when: "status=superseded".into(),
                    require: "superseded_by".into(),
                }],
                ..Default::default()
            },
            ..Config::default()
        };
        config.validate().unwrap();
        let collected = config.cross_field_for("adr");
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].require, "superseded_by");
    }

    #[test]
    fn validate_rejects_cross_field_duplicate_across_global_and_override() {
        let config = Config {
            schema: SchemaConfig {
                cross_field: vec![CrossFieldSpec {
                    when: "status=superseded".into(),
                    require: "superseded_by".into(),
                }],
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into()],
                    required: vec![],
                    types: BTreeMap::new(),
                    enums: BTreeMap::new(),
                    cross_field: vec![CrossFieldSpec {
                        when: "status=superseded".into(),
                        require: "superseded_by".into(),
                    }],
                }],
                ..Default::default()
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("already declared in [schema].cross_field"));
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn parse_when_error_mentions_quoting_unsupported() {
        let err = parse_when("status==foo").unwrap_err();
        assert!(err.contains("embedded '='") || err.contains("exactly one"));
    }
}
