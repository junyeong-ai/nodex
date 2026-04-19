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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalExclude {
    pub parent_glob: String,
    pub child_glob: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaConfig {
    #[serde(default = "default_required")]
    pub required: Vec<String>,
    #[serde(default)]
    pub overrides: Vec<SchemaOverride>,
}

impl Default for SchemaConfig {
    fn default() -> Self {
        Self {
            required: default_required(),
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
    /// Checks:
    /// - `enums.status` values are all present in `statuses.allowed`
    /// - `enums.kind` values are all present in `kinds.allowed`
    /// - `cross_field.when` parses successfully
    pub fn validate(&self) -> Result<()> {
        for ov in &self.schema.overrides {
            for (field, allowed) in &ov.enums {
                let global = match field.as_str() {
                    "status" => Some(&self.statuses.allowed),
                    "kind" => Some(&self.kinds.allowed),
                    _ => None,
                };
                if let Some(global) = global {
                    for value in allowed {
                        if !global.contains(value) {
                            return Err(Error::Config(format!(
                                "schema.overrides.enums.{field} contains {value:?} \
                                 which is not in {field}s.allowed"
                            )));
                        }
                    }
                }
            }
            for cf in &ov.cross_field {
                parse_when(&cf.when).map_err(|e| {
                    Error::Config(format!(
                        "schema.overrides.cross_field.when {:?}: {e}",
                        cf.when
                    ))
                })?;
            }
        }
        Ok(())
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

/// Parse a `cross_field.when` expression. v1 accepts only `field=value`.
pub fn parse_when(raw: &str) -> std::result::Result<WhenPredicate, String> {
    let trimmed = raw.trim();
    if let Some((lhs, rhs)) = trimmed.split_once('=') {
        let field = lhs.trim();
        let value = rhs.trim();
        if field.is_empty() || value.is_empty() {
            return Err("expected non-empty <field>=<value>".to_string());
        }
        return Ok(WhenPredicate::Equals {
            field: field.to_string(),
            value: value.to_string(),
        });
    }
    Err("expected <field>=<value>".to_string())
}
