use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};

/// Every frontmatter field that the parser recognises by name (and
/// therefore strips from the `attrs` catch-all). Single source of
/// truth — `Config::declared_fields_for` and the strict-mode rule both
/// read from here so a new built-in is acknowledged in exactly one
/// place. Mirrors the named fields of
/// [`crate::parser::frontmatter::RawFrontmatter`].
pub const BUILTIN_FRONTMATTER_FIELDS: &[&str] = &[
    "id",
    "title",
    "kind",
    "status",
    "created",
    "updated",
    "reviewed",
    "owner",
    "supersedes",
    "superseded_by",
    "implements",
    "related",
    "tags",
    "covers",
    "orphan_ok",
];

/// Root configuration deserialized from `nodex.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Binary-compatibility pin. When `meta.nodex_version` is set,
    /// [`Config::load`] refuses to return a value unless the running
    /// `nodex` binary satisfies the SemVer requirement — the project
    /// declares which binary versions can read it, instead of every
    /// CI / contributor re-implementing the version check.
    #[serde(default)]
    pub meta: MetaConfig,
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
    #[serde(default)]
    pub trust: TrustConfig,
    #[serde(default)]
    pub similarity: SimilarityConfig,
    /// Body-text markers extracted at parse time and surfaced by
    /// `nodex query annotations`. Each block declares a regex with a
    /// named-capture grouping key; matches outside code blocks are
    /// recorded against the source node and made queryable by the
    /// capture's value. See [`AnnotationConfig`] for field meaning.
    #[serde(default, rename = "annotations")]
    pub annotations: Vec<AnnotationConfig>,
}

/// Project-level metadata. Today the only entry is the binary-version
/// pin; future entries (e.g. project name, doc-graph schema version,
/// canonical doc root) extend this block without reshaping the rest of
/// the config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetaConfig {
    /// SemVer requirement (e.g. `">=0.8, <0.9"`) the running `nodex`
    /// binary must satisfy. `None` (the key omitted entirely) accepts
    /// any version — the recommended default during early development
    /// while the binary's API surface is still settling.
    #[serde(default)]
    pub nodex_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeConfig {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub conditional_exclude: Vec<ConditionalExclude>,
    /// Include files / directories whose name starts with `.`
    /// (`.draft.md`, `.archive/`, `.claude/`, …). Defaults to `false`
    /// to match the convention established by `ripgrep` / `ag` —
    /// most projects keep dot-prefixed entries as editor state or
    /// tooling config, not documentation. Set to `true` to scan
    /// hidden paths (the curated tooling exclusion list —
    /// `node_modules`, `__pycache__`, `target` — still applies).
    #[serde(default)]
    pub include_hidden: bool,
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            include: vec!["**/*.md".to_string()],
            exclude: vec![],
            conditional_exclude: vec![],
            include_hidden: false,
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
    /// Frontmatter strictness. `Lenient` (default) lets undeclared
    /// project-specific keys land in `attrs` untouched. `Strict` rejects
    /// any frontmatter key that is neither built-in nor declared in
    /// `types` / `enums` / `required` / `cross_field` (global + per-kind
    /// override) — this is the typo-catcher mode.
    #[serde(default)]
    pub mode: SchemaMode,
}

impl Default for SchemaConfig {
    fn default() -> Self {
        Self {
            required: default_required(),
            types: BTreeMap::new(),
            enums: BTreeMap::new(),
            cross_field: vec![],
            overrides: vec![],
            mode: SchemaMode::default(),
        }
    }
}

/// Frontmatter strictness mode. Drives [`UnknownFieldRule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaMode {
    /// Undeclared keys are preserved silently in `Node::attrs`.
    #[default]
    Lenient,
    /// Undeclared keys produce a `unknown_field` violation per node.
    Strict,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub naming: Vec<NamingRuleConfig>,
    /// Lock declared frontmatter fields against further edits once a
    /// node has reached a terminal status, with one block per locking
    /// policy. Every doc terminal at `superseded` keeps its identity
    /// stable; ADR-kind docs additionally lock `decision_date` once
    /// they hit `archived`. Inert without `--since`. Drives
    /// [`crate::rules::frontmatter_immutable::FrontmatterImmutableRule`].
    #[serde(default)]
    pub frontmatter_immutable: Vec<FrontmatterImmutableRuleConfig>,
    /// Lock document bodies against post-terminal edits, with one
    /// block per locking policy (a project may freeze some kinds
    /// outright while permitting append-only growth on others).
    /// Inert without a `--since` ref. Drives
    /// [`crate::rules::body_immutable::BodyImmutableRule`].
    #[serde(default)]
    pub body_immutable: Vec<BodyImmutableRuleConfig>,
    /// Per-line body-text conformance rules. Each block declares a
    /// regex with named captures; for every match outside a code
    /// block, each capture listed in `enums` must contain a value
    /// from its declared allowed set. Drives
    /// [`crate::rules::body_line::BodyLineRule`].
    #[serde(default)]
    pub body_line: Vec<BodyLineRuleConfig>,
    /// Multi-line body-block conformance rules — one block per
    /// `[[rules.body_block]]` config entry. Each block names a
    /// `start_pattern` whose match opens a span and an `end_pattern`
    /// whose match closes it (the start of a new block also closes
    /// the previous one; end-of-body closes any still-open block).
    /// Captures from the start-pattern match are validated against
    /// `enums` at check time. Drives
    /// [`crate::rules::body_block::BodyBlockRule`].
    #[serde(default)]
    pub body_block: Vec<BodyBlockRuleConfig>,
}

/// The `(applies_to_kind, applies_to_status, applies_to_tag)` triple
/// every body-derived rule and the annotation vocabulary accept.
/// Centralised so the five places that need it — five config structs,
/// the validator, the runtime predicate — all read from one shape.
/// Adding a new axis (e.g. `applies_to_owner`) is a single-file change
/// here and in [`crate::scope_predicate::ScopePredicate`].
///
/// The TOML surface keeps the flat `applies_to_*` keys via
/// `#[serde(flatten)]` + `#[serde(rename)]`; nesting under
/// `applies_to = { kinds = … }` would be less idiomatic TOML and
/// inflate authoring overhead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyTo {
    /// When non-empty, only docs whose `kind` appears here are
    /// scanned / locked. Empty = no kind restriction.
    #[serde(default, rename = "applies_to_kind")]
    pub kinds: Vec<String>,
    /// When non-empty, only docs whose `status` appears here are
    /// scanned / locked. Empty = no status restriction. For
    /// vocabulary rules every value must be in `statuses.allowed`;
    /// for immutability rules every value must be in
    /// `statuses.terminal`. `Config::load` rejects out-of-universe
    /// values.
    #[serde(default, rename = "applies_to_status")]
    pub statuses: Vec<String>,
    /// When non-empty, only docs whose `tags` overlap this list are
    /// scanned / locked (at least one tag must match). Empty = no
    /// tag restriction. Tags are free-form project vocabulary; the
    /// validator only rejects empty entries — an allowlist would
    /// re-introduce the central registry the tag axis is designed
    /// to avoid.
    #[serde(default, rename = "applies_to_tag")]
    pub tags: Vec<String>,
}

impl ApplyTo {
    /// Borrowed view consumed by the runtime predicate. The slices'
    /// lifetimes track the config block's lifetime, which always
    /// outlives any single rule evaluation — passing slices keeps
    /// the per-block hot loop allocation-free.
    pub fn predicate(&self) -> crate::scope_predicate::ScopePredicate<'_> {
        crate::scope_predicate::ScopePredicate {
            kinds: &self.kinds,
            statuses: &self.statuses,
            tags: &self.tags,
        }
    }
}

/// One body-block conformance rule.
///
/// Use to enforce a structured vocabulary on captured fields in
/// multi-line spans — ADR decision sections (`## Decision (status: …)`
/// bounded by the next `##` heading), runbook step blocks, contract
/// clauses. Captures come from the *start* line's match: body_block
/// is a framing primitive, not a per-line scanner. A project that
/// needs both framing and per-line conformance composes
/// `[[rules.body_block]]` with `[[rules.body_line]]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyBlockRuleConfig {
    /// Stable identifier used in the violation `rule_id`
    /// (`body_block/<name>`) and in the rule manifest. Must be unique
    /// across all `[[rules.body_block]]` blocks.
    pub name: String,
    /// Regex matched against each non-code body line; the first match
    /// opens a span. Named captures from this match become the
    /// block's `captures` map. Must compile, and any capture key
    /// listed in `enums` must appear in this pattern's named-capture
    /// list — `Config::load` rejects otherwise.
    pub start_pattern: String,
    /// Regex matched against every subsequent non-code body line; the
    /// first match closes the span. A line that matches `start_pattern`
    /// while a span is open *also* closes it and opens a new span (so
    /// `end_pattern = "^## "` correctly partitions a doc full of
    /// sibling sections). End-of-body closes any still-open span.
    pub end_pattern: String,
    /// Scope triple — which docs are scanned. See [`ApplyTo`].
    #[serde(default, flatten)]
    pub applies: ApplyTo,
    /// `capture_name -> allowed values`. Every key must be a named
    /// capture in `start_pattern`; every captured value must appear
    /// in the corresponding list or a violation fires. At least one
    /// entry is required — a body_block rule with no enum check has
    /// no failure mode and would silently never fire (same
    /// discipline `body_line` enforces).
    pub enums: BTreeMap<String, Vec<String>>,
}

/// One body-immutability policy. Multiple blocks let a project apply
/// different locking semantics to different kinds — ADRs `frozen`
/// (decisions are immutable in spirit), narratives `append_only`
/// (history grows but does not rewrite). The rule activates only for
/// nodes whose *current* status is terminal — pre-terminal documents
/// are still authoring drafts. The scope triple narrows further: a
/// block with `applies_to_status = ["superseded"]` locks only when
/// the doc supersedes (never when it archives), and every status
/// listed must itself be terminal (`Config::load` enforces).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyImmutableRuleConfig {
    /// Stable identifier used in the violation `rule_id`
    /// (`body_immutable/<name>`) and in the rule manifest. Must be
    /// unique across all `[[rules.body_immutable]]` blocks.
    pub name: String,
    /// Lock semantic for matching documents.
    pub mode: BodyImmutableMode,
    /// Scope triple — which docs are locked. See [`ApplyTo`]. For
    /// this family `applies.statuses` must be a subset of
    /// `statuses.terminal` (`Config::load` enforces).
    #[serde(default, flatten)]
    pub applies: ApplyTo,
}

/// One frontmatter-immutability policy. Multiple blocks let a project
/// lock different field sets in different parts of the corpus — every
/// doc terminal at `superseded` keeps its identity stable, while
/// ADR-kind docs additionally lock `decision_date` once they hit
/// `archived`. Inert without `--since`. Symmetric with
/// [`BodyImmutableRuleConfig`]: each block carries a unique `name`,
/// the scope triple, and the per-block payload (`fields`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontmatterImmutableRuleConfig {
    /// Stable identifier used in the violation `rule_id`
    /// (`frontmatter_immutable/<name>`) and in the rule manifest.
    /// Must be unique across all `[[rules.frontmatter_immutable]]`
    /// blocks.
    pub name: String,
    /// Frontmatter fields locked by this block. Each entry must be
    /// either a built-in field or declared in `[schema]` — locking
    /// an unknown field would silently never fire, the same failure
    /// mode `Config::load` already refuses for body-derived rules.
    pub fields: Vec<String>,
    /// Scope triple — which docs are locked. See [`ApplyTo`]. For
    /// this family `applies.statuses` must be a subset of
    /// `statuses.terminal` (`Config::load` enforces).
    #[serde(default, flatten)]
    pub applies: ApplyTo,
}

/// How a terminal document's body is locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyImmutableMode {
    /// Any body change is a violation — the document is fixed once
    /// it reaches terminal status. The natural mode for ADRs,
    /// contracts, signed-off specs.
    Frozen,
    /// The pre-terminal body must remain an exact prefix of the new
    /// body — appends are allowed, edits or deletions are not. Suits
    /// log-shaped documents (changelogs, post-mortems, decision
    /// journals) where new entries land at the bottom.
    AppendOnly,
}

/// One body-line conformance rule. Use to enforce a structured
/// vocabulary on lines that match a known pattern — ADR status
/// lines, decision-log entries, conventional-commit body lines —
/// without coding the vocabulary into the rule itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyLineRuleConfig {
    /// Stable identifier used in the violation `rule_id`
    /// (`body_line/<name>`) and in the rule manifest.
    pub name: String,
    /// Regex with at least one named capture. Lines outside code
    /// blocks are scanned; non-matching lines are ignored — this is
    /// a *conformance* rule, not a *presence* rule.
    pub pattern: String,
    /// Scope triple — which docs are scanned. See [`ApplyTo`].
    #[serde(default, flatten)]
    pub applies: ApplyTo,
    /// `capture_name -> allowed values`. Every key must be a named
    /// capture in `pattern`; every captured value must appear in
    /// the corresponding list or a violation fires. At least one
    /// entry is required — a body_line rule with no enum check has
    /// no semantic.
    pub enums: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamingRuleConfig {
    pub glob: String,
    pub pattern: String,
    #[serde(default)]
    pub sequential: bool,
    #[serde(default)]
    pub unique: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParserConfig {
    /// File extensions — including the leading `.` — that nodex
    /// treats as in-graph documents. Body-link extraction ignores
    /// references whose path does not end with one of these, and
    /// scaffold refuses to target a path with any other extension.
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,

    /// Enable Obsidian-style `[[wikilink]]` parsing in body text.
    /// When on, `[[X]]` (or `[[X|display]]`) outside code blocks emits
    /// a reference edge to X. Resolution tries the literal path, then
    /// the path with the first configured extension appended, then a
    /// node id — so both `[[guides/intro]]` and `[[adr-001]]` work.
    #[serde(default)]
    pub wikilink_enabled: bool,

    #[serde(default)]
    pub link_patterns: Vec<LinkPattern>,
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            extensions: default_extensions(),
            wikilink_enabled: false,
            link_patterns: vec![],
        }
    }
}

fn default_extensions() -> Vec<String> {
    vec![".md".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkPattern {
    pub pattern: String,
    pub relation: String,
}

/// One body-annotation pattern. Lines outside code blocks are matched
/// against `pattern`; for every match, the named capture identified by
/// `key` becomes the grouping value, and the match is recorded against
/// the source node.
///
/// Decoupled from `[parser.link_patterns]` on purpose: link patterns
/// produce graph edges (their target resolves to another node or fails
/// loudly as `unresolved`); annotation keys are pre-graph identifiers
/// that may never become nodes (promotion candidates, open research
/// questions, TODO topics). Mixing the two would either produce
/// permanently-unresolved edges or silently swallow real broken links.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationConfig {
    /// Stable identifier used in JSON output and as the CLI filter
    /// argument. Must be unique across all `[[annotations]]` blocks.
    pub name: String,
    /// Regex with at least one named capture group; one of those
    /// captures (named below in `key`) holds the grouping value.
    pub pattern: String,
    /// The named capture inside `pattern` whose matched text becomes
    /// the marker's grouping key.
    pub key: String,
    /// Scope triple — which docs are scanned. See [`ApplyTo`].
    #[serde(default, flatten)]
    pub applies: ApplyTo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionConfig {
    #[serde(default = "default_stale_days")]
    pub stale_days: u32,
    #[serde(default = "default_orphan_grace_days")]
    pub orphan_grace_days: u32,
    /// Kinds whose nodes are skipped by orphan detection regardless of incoming-edge count.
    ///
    /// Use for kinds that are leaf-by-design (entry-point skills, package READMEs, runbook
    /// procedures, architecture overviews) where a missing inbound edge is the expected
    /// shape rather than a defect. Per-node `orphan_ok: true` remains available for the
    /// per-instance opt-out within tracked kinds.
    #[serde(default)]
    pub orphan_ok_kinds: Vec<String>,
    /// `Some(n)` enables [`crate::rules::git_drift::GitDriftRule`]: a
    /// document is flagged when the referenced docs it points to have
    /// accumulated more than `n` git commits since this document's
    /// `reviewed` date. `None` (default) disables the rule.
    ///
    /// Opting in requires git on PATH and a git work tree at the
    /// project root. The check is performed by
    /// [`crate::rules::preflight`] (called by [`crate::load_project`]),
    /// not by [`Config::load`].
    #[serde(default)]
    pub git_drift_threshold: Option<u32>,
    /// Outgoing relations that participate in git_drift. Default
    /// `["references", "implements", "covers"]` — supersedes/related
    /// are intentionally excluded since their drift signal is captured
    /// by supersession itself.
    #[serde(default = "default_git_drift_relations")]
    pub git_drift_relations: Vec<String>,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            stale_days: default_stale_days(),
            orphan_grace_days: default_orphan_grace_days(),
            orphan_ok_kinds: Vec::new(),
            git_drift_threshold: None,
            git_drift_relations: default_git_drift_relations(),
        }
    }
}

fn default_git_drift_relations() -> Vec<String> {
    vec![
        "references".to_string(),
        "implements".to_string(),
        "covers".to_string(),
    ]
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

/// Composite trust score weights. Each component score is in `[0, 1]`;
/// the final composite is the weighted average normalised by the sum
/// of *active* weights — when a component is unavailable (e.g.
/// `drift` without `git_drift_threshold`) its weight is excluded so
/// the result stays in `[0, 1]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustConfig {
    #[serde(default = "default_trust_weights")]
    pub weights: TrustWeights,
    /// Default cut-off used by `nodex query low-trust` when the caller
    /// does not supply one. Mirrors `similarity.threshold` so both
    /// scoring surfaces are equally tunable from config rather than
    /// burying their defaults in CLI wiring.
    #[serde(default = "default_low_trust_threshold")]
    pub low_trust_threshold: f64,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            weights: default_trust_weights(),
            low_trust_threshold: default_low_trust_threshold(),
        }
    }
}

fn default_low_trust_threshold() -> f64 {
    0.5
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TrustWeights {
    pub status: f64,
    pub freshness: f64,
    pub drift: f64,
    pub backlinks: f64,
}

impl Default for TrustWeights {
    fn default() -> Self {
        Self {
            status: 0.4,
            freshness: 0.3,
            drift: 0.2,
            backlinks: 0.1,
        }
    }
}

fn default_trust_weights() -> TrustWeights {
    TrustWeights::default()
}

/// Vector-free similarity scoring. Each component is in `[0, 1]`;
/// the composite is a weighted average normalised by the sum of all
/// declared weights so users can tune relative importance without
/// worrying about renormalisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarityConfig {
    #[serde(default = "default_similarity_threshold")]
    pub threshold: f64,
    /// Default `limit` applied when callers don't supply one — mirrors
    /// `trust.low_trust_threshold` so both scoring surfaces are
    /// equally tunable from config rather than burying a magic number
    /// in CLI wiring.
    #[serde(default = "default_similarity_limit")]
    pub default_limit: usize,
    #[serde(default = "default_similarity_weights")]
    pub weights: SimilarityWeights,
    #[serde(default = "default_title_stop_words")]
    pub title_stop_words: Vec<String>,
}

impl Default for SimilarityConfig {
    fn default() -> Self {
        Self {
            threshold: default_similarity_threshold(),
            default_limit: default_similarity_limit(),
            weights: default_similarity_weights(),
            title_stop_words: default_title_stop_words(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SimilarityWeights {
    pub title: f64,
    pub tags: f64,
    pub kind: f64,
    pub directory: f64,
    pub linked: f64,
}

impl Default for SimilarityWeights {
    fn default() -> Self {
        Self {
            title: 0.4,
            tags: 0.2,
            kind: 0.1,
            directory: 0.1,
            linked: 0.2,
        }
    }
}

fn default_similarity_threshold() -> f64 {
    0.3
}

fn default_similarity_limit() -> usize {
    10
}

fn default_similarity_weights() -> SimilarityWeights {
    SimilarityWeights::default()
}

fn default_title_stop_words() -> Vec<String> {
    [
        "the", "a", "an", "and", "or", "of", "to", "for", "in", "on", "with", "is", "are", "be",
        "by", "as", "at", "from",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Which status set a rule's `applies_to_status` filter must subset.
/// Drives [`Config::validate_apply_to`]; selects the no-silent-skip
/// gate the rule family enforces. Private to the config module —
/// the choice between universes is a structural detail of the
/// validator's internal contract, not part of the public API.
#[derive(Debug, Clone, Copy)]
enum StatusUniverse {
    /// Any status in `statuses.allowed` is acceptable. The vocabulary
    /// rule families (`body_line`, `body_block`, `annotations`) use
    /// this universe — they have no built-in status filter, so any
    /// declared status is in scope.
    Allowed,
    /// Only statuses in `statuses.terminal` are acceptable. The
    /// immutability rule families (`frontmatter_immutable`,
    /// `body_immutable`) gate on terminal status at check time;
    /// declaring a non-terminal status would silently scope the rule
    /// to zero documents.
    Terminal,
}

/// Common view of an immutability-rule config block — owned by the
/// validator so the two families (`body_immutable`,
/// `frontmatter_immutable`) reject the same typos with the same
/// message shape. `fields` is `Some` only for `frontmatter_immutable`,
/// whose per-block payload is a frontmatter field list. Body
/// immutability has no field-list payload; its `mode` is enforced at
/// check time, not at config load.
struct ImmutableBlock<'a> {
    name: &'a str,
    fields: Option<&'a [String]>,
    applies: &'a ApplyTo,
}

/// Refuse any immutability block whose `name`, scope triple, or
/// field-list (frontmatter only) would silently mis-fire at check
/// time. Mirrors the discipline `validate_scope_triple` already
/// enforces for vocabulary rules and adds the terminal-subset and
/// field-universe gates that are specific to immutability semantics.
fn validate_immutable_blocks<'a, I>(config: &Config, family: &str, blocks: I) -> Result<()>
where
    I: IntoIterator<Item = ImmutableBlock<'a>>,
{
    let mut seen_names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let field_universe = config.declared_fields_universe();
    for (idx, block) in blocks.into_iter().enumerate() {
        if block.name.trim().is_empty() {
            return Err(Error::Config(format!(
                "{family}[{idx}].name must be a non-empty string"
            )));
        }
        if !seen_names.insert(block.name) {
            return Err(Error::Config(format!(
                "{family}[{idx}].name {:?} is declared more than once; \
                 names must be unique so violation rule_ids stay distinguishable",
                block.name
            )));
        }
        let ctx = format!("{family}[{idx}] ({name:?})", name = block.name);
        config.validate_apply_to(&ctx, block.applies, StatusUniverse::Terminal)?;
        if let Some(fields) = block.fields {
            for field in fields {
                if !field_universe.contains(field) {
                    return Err(Error::Config(format!(
                        "{ctx}.fields contains {field:?} which is neither a \
                         built-in frontmatter field nor declared in [schema] \
                         (required / types / enums / cross_field). Locking an \
                         unknown field would never fire — declare it or remove \
                         it from the lock list"
                    )));
                }
            }
            if fields.is_empty() {
                return Err(Error::Config(format!(
                    "{ctx}.fields must list at least one field — an empty list \
                     locks nothing and would silently never fire"
                )));
            }
        }
    }
    Ok(())
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
        // Binary-compatibility pin. Runs *after* `validate()` so a
        // structurally-broken config surfaces as `CONFIG_ERROR` rather
        // than the (less informative) `VERSION_MISMATCH` it would
        // produce if the same file were authored against a future
        // binary. The version check is therefore the last gate — once
        // a config is internally consistent, the only remaining
        // question is "can this binary honour it?".
        if let Some(req) = config.meta.nodex_version.as_deref() {
            crate::verify_version(req)?;
        }
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

        // `FALLBACK_KIND` is what `parser::identity::infer_kind`
        // assigns when no `identity.kind_rules` glob matches a
        // document's path, and what `migrate` injects when scaffolding
        // frontmatter onto a bare file. Leaving this out of
        // `kinds.allowed` was the exact defect that let `migrate` /
        // `parse_document` write documents their own config then
        // rejected. Require its presence at load; projects that want
        // every document strongly classified can still write
        // exhaustive `kind_rules`, in which case the fallback simply
        // never fires.
        if !self
            .kinds
            .allowed
            .iter()
            .any(|k| k == crate::parser::identity::FALLBACK_KIND)
        {
            return Err(Error::Config(format!(
                "kinds.allowed is missing the fallback kind {:?}; \
                 either include it, or omit `kinds.allowed` to accept the defaults",
                crate::parser::identity::FALLBACK_KIND
            )));
        }

        // Every `detection.orphan_ok_kinds` entry must reference a kind
        // the project actually accepts; a typo would otherwise load
        // cleanly and the runtime would exempt nothing. Same subset
        // discipline as the `enums.status` / `enums.kind` checks below.
        for k in &self.detection.orphan_ok_kinds {
            if !self.kinds.allowed.iter().any(|a| a == k) {
                return Err(Error::Config(format!(
                    "detection.orphan_ok_kinds contains {k:?} which is not in \
                     kinds.allowed; add it to kinds.allowed or remove the exemption"
                )));
            }
        }

        // `output.dir` is joined to the project root whenever build /
        // report / cache writes their artefacts, so a value like
        // `"../escape"` or `"/etc/out"` would silently write files
        // outside the project. `path_guard::reject_traversal` already
        // enforces this invariant for user-supplied paths on rename /
        // scaffold / migrate; extend it to the config surface.
        if !self.output.dir.is_empty() {
            crate::path_guard::reject_traversal(std::path::Path::new(&self.output.dir)).map_err(
                |_| {
                    Error::Config(format!(
                        "output.dir {:?} escapes the project root; \
                         use a relative path without `..` or a leading `/`",
                        self.output.dir
                    ))
                },
            )?;
        }

        self.validate_block(
            "schema",
            &self.schema.required,
            &self.schema.types,
            &self.schema.enums,
            &self.schema.cross_field,
        )?;

        // Pre-validate every glob and regex the runtime depends on.
        // The contract is symmetric: the load-time validator's only
        // purpose is to reject what the runtime cannot honour, and the
        // runtime never silently skips a rule the validator accepted.
        // Both halves break if a pattern that loads cleanly fails to
        // compile downstream — projects then see "no violations" when
        // the truth is "no rule ever ran".
        for (idx, nr) in self.rules.naming.iter().enumerate() {
            globset::Glob::new(&nr.glob).map_err(|e| {
                Error::Config(format!(
                    "rules.naming[{idx}].glob {:?} is not a valid glob: {e}",
                    nr.glob
                ))
            })?;
            regex::Regex::new(&nr.pattern).map_err(|e| {
                Error::Config(format!(
                    "rules.naming[{idx}].pattern {:?} is not a valid regex: {e}",
                    nr.pattern
                ))
            })?;
        }
        for (idx, kr) in self.identity.kind_rules.iter().enumerate() {
            globset::Glob::new(&kr.glob).map_err(|e| {
                Error::Config(format!(
                    "identity.kind_rules[{idx}].glob {:?} is not a valid glob: {e}",
                    kr.glob
                ))
            })?;
            if !self.kinds.allowed.iter().any(|a| a == &kr.kind) {
                return Err(Error::Config(format!(
                    "identity.kind_rules[{idx}].kind {:?} is not in kinds.allowed",
                    kr.kind
                )));
            }
        }
        for (idx, ir) in self.identity.id_rules.iter().enumerate() {
            if let Some(glob) = &ir.glob {
                globset::Glob::new(glob).map_err(|e| {
                    Error::Config(format!(
                        "identity.id_rules[{idx}].glob {glob:?} is not a valid glob: {e}"
                    ))
                })?;
            }
        }
        for (idx, ce) in self.scope.conditional_exclude.iter().enumerate() {
            globset::Glob::new(&ce.parent_glob).map_err(|e| {
                Error::Config(format!(
                    "scope.conditional_exclude[{idx}].parent_glob {:?} is not a valid glob: {e}",
                    ce.parent_glob
                ))
            })?;
        }
        for (idx, lp) in self.parser.link_patterns.iter().enumerate() {
            regex::Regex::new(&lp.pattern).map_err(|e| {
                Error::Config(format!(
                    "parser.link_patterns[{idx}].pattern {:?} is not a valid regex: {e}",
                    lp.pattern
                ))
            })?;
        }

        // Body-line rules: compile, enum keys ∈ named captures, kinds
        // valid, names unique, `enums` non-empty. Same "no silent
        // runtime skips" discipline.
        let mut body_line_names: std::collections::BTreeSet<&str> =
            std::collections::BTreeSet::new();
        for (idx, bl) in self.rules.body_line.iter().enumerate() {
            if bl.name.trim().is_empty() {
                return Err(Error::Config(format!(
                    "rules.body_line[{idx}].name must be a non-empty string"
                )));
            }
            if !body_line_names.insert(bl.name.as_str()) {
                return Err(Error::Config(format!(
                    "rules.body_line[{idx}].name {:?} is declared more than once; \
                     names must be unique so violation rule_ids stay distinguishable",
                    bl.name
                )));
            }
            let re = regex::Regex::new(&bl.pattern).map_err(|e| {
                Error::Config(format!(
                    "rules.body_line[{idx}] ({name:?}).pattern {pat:?} is not a valid regex: {e}",
                    name = bl.name,
                    pat = bl.pattern
                ))
            })?;
            if bl.enums.is_empty() {
                return Err(Error::Config(format!(
                    "rules.body_line[{idx}] ({name:?}).enums must have at least one entry — \
                     a body_line rule without an enum check has no failure mode and would \
                     silently never fire",
                    name = bl.name
                )));
            }
            let capture_names: Vec<&str> = re.capture_names().flatten().collect();
            for capture in bl.enums.keys() {
                if !capture_names.contains(&capture.as_str()) {
                    return Err(Error::Config(format!(
                        "rules.body_line[{idx}] ({name:?}).enums.{capture} is not a named \
                         capture in pattern {pat:?}; declared captures: {caps:?}",
                        name = bl.name,
                        pat = bl.pattern,
                        caps = capture_names
                    )));
                }
            }
            for (capture, allowed) in &bl.enums {
                if allowed.is_empty() {
                    return Err(Error::Config(format!(
                        "rules.body_line[{idx}] ({name:?}).enums.{capture} is empty; \
                         an empty allowed set rejects every captured value",
                        name = bl.name
                    )));
                }
            }
            self.validate_apply_to(
                &format!("rules.body_line[{idx}] ({name:?})", name = bl.name),
                &bl.applies,
                StatusUniverse::Allowed,
            )?;
        }

        // Annotation patterns: compile, key ∈ named captures, kinds
        // valid, names unique. Same "no silent runtime skips" discipline
        // as everywhere else — a typo in `key` or `applies_to_kind`
        // would otherwise silently extract zero markers forever.
        let mut annotation_names: std::collections::BTreeSet<&str> =
            std::collections::BTreeSet::new();
        for (idx, ann) in self.annotations.iter().enumerate() {
            if ann.name.trim().is_empty() {
                return Err(Error::Config(format!(
                    "annotations[{idx}].name must be a non-empty string"
                )));
            }
            if !annotation_names.insert(ann.name.as_str()) {
                return Err(Error::Config(format!(
                    "annotations[{idx}].name {:?} is declared more than once; \
                     names must be unique so CLI filters and JSON output stay deterministic",
                    ann.name
                )));
            }
            let re = regex::Regex::new(&ann.pattern).map_err(|e| {
                Error::Config(format!(
                    "annotations[{idx}] ({name:?}).pattern {pat:?} is not a valid regex: {e}",
                    name = ann.name,
                    pat = ann.pattern
                ))
            })?;
            let capture_names: Vec<&str> = re.capture_names().flatten().collect();
            if !capture_names.iter().any(|n| *n == ann.key) {
                return Err(Error::Config(format!(
                    "annotations[{idx}] ({name:?}).key {key:?} is not a named capture in \
                     pattern {pat:?}; declared captures: {caps:?}",
                    name = ann.name,
                    key = ann.key,
                    pat = ann.pattern,
                    caps = capture_names
                )));
            }
            self.validate_apply_to(
                &format!("annotations[{idx}] ({name:?})", name = ann.name),
                &ann.applies,
                StatusUniverse::Allowed,
            )?;
        }

        // The graph has no notion of "no extensions"; an empty list
        // would silently turn off body-link extraction altogether.
        if self.parser.extensions.is_empty() {
            return Err(Error::Config(
                "parser.extensions must list at least one extension; \
                 omit the key to accept the default [\".md\"]"
                    .to_string(),
            ));
        }
        for (idx, ext) in self.parser.extensions.iter().enumerate() {
            if !ext.starts_with('.') || ext.len() < 2 {
                return Err(Error::Config(format!(
                    "parser.extensions[{idx}] {ext:?} must start with '.' and have at least one character after it"
                )));
            }
        }

        // git_drift_relations only matters when the rule is enabled.
        // Two failure modes: an empty list silently fires nothing, and
        // any entry that isn't a known relation (built-in or declared
        // via [[parser.link_patterns]]) silently matches zero edges.
        // Both refused at load — same "no silent runtime skips"
        // discipline the rest of the validator enforces.
        if self.detection.git_drift_threshold.is_some() {
            if self.detection.git_drift_relations.is_empty() {
                return Err(Error::Config(
                    "detection.git_drift_relations must list at least one relation when \
                     detection.git_drift_threshold is set"
                        .to_string(),
                ));
            }
            let known = self.known_relations();
            for (idx, rel) in self.detection.git_drift_relations.iter().enumerate() {
                if !known.contains(rel) {
                    let known_sorted: Vec<&str> = known.iter().map(String::as_str).collect();
                    return Err(Error::Config(format!(
                        "detection.git_drift_relations[{idx}] {rel:?} is not a known relation; \
                         declare it via [[parser.link_patterns]] or pick one of {known_sorted:?}"
                    )));
                }
            }
        }

        // Body-block rules: same discipline as body_line — compile,
        // enum keys ∈ start_pattern named captures, applies_to_kind ⊆
        // kinds.allowed, names unique, enums non-empty. Both regexes
        // must compile; an end_pattern that fails to match anything
        // still produces a well-defined behaviour (end-of-body closes
        // the span), so the load-time check is structural, not
        // semantic.
        let mut body_block_names: std::collections::BTreeSet<&str> =
            std::collections::BTreeSet::new();
        for (idx, bb) in self.rules.body_block.iter().enumerate() {
            if bb.name.trim().is_empty() {
                return Err(Error::Config(format!(
                    "rules.body_block[{idx}].name must be a non-empty string"
                )));
            }
            if !body_block_names.insert(bb.name.as_str()) {
                return Err(Error::Config(format!(
                    "rules.body_block[{idx}].name {:?} is declared more than once; \
                     names must be unique so violation rule_ids stay distinguishable",
                    bb.name
                )));
            }
            let start_re = regex::Regex::new(&bb.start_pattern).map_err(|e| {
                Error::Config(format!(
                    "rules.body_block[{idx}] ({name:?}).start_pattern {pat:?} is not a valid regex: {e}",
                    name = bb.name,
                    pat = bb.start_pattern
                ))
            })?;
            regex::Regex::new(&bb.end_pattern).map_err(|e| {
                Error::Config(format!(
                    "rules.body_block[{idx}] ({name:?}).end_pattern {pat:?} is not a valid regex: {e}",
                    name = bb.name,
                    pat = bb.end_pattern
                ))
            })?;
            if bb.enums.is_empty() {
                return Err(Error::Config(format!(
                    "rules.body_block[{idx}] ({name:?}).enums must have at least one entry — \
                     a body_block rule without an enum check has no failure mode and would \
                     silently never fire",
                    name = bb.name
                )));
            }
            let capture_names: Vec<&str> = start_re.capture_names().flatten().collect();
            for capture in bb.enums.keys() {
                if !capture_names.contains(&capture.as_str()) {
                    return Err(Error::Config(format!(
                        "rules.body_block[{idx}] ({name:?}).enums.{capture} is not a named \
                         capture in start_pattern {pat:?}; declared captures: {caps:?}",
                        name = bb.name,
                        pat = bb.start_pattern,
                        caps = capture_names
                    )));
                }
            }
            for (capture, allowed) in &bb.enums {
                if allowed.is_empty() {
                    return Err(Error::Config(format!(
                        "rules.body_block[{idx}] ({name:?}).enums.{capture} is empty; \
                         an empty allowed set rejects every captured value",
                        name = bb.name
                    )));
                }
            }
            self.validate_apply_to(
                &format!("rules.body_block[{idx}] ({name:?})", name = bb.name),
                &bb.applies,
                StatusUniverse::Allowed,
            )?;
        }

        // Immutability rules — symmetric validation for the two
        // diff-aware lock families. Both surface as
        // `<family>/<name>` rule_ids, both gate on terminal status,
        // both accept the scope triple. Centralising the validation
        // means a future third immutability family lands once.
        validate_immutable_blocks(
            self,
            "rules.body_immutable",
            self.rules.body_immutable.iter().map(|b| ImmutableBlock {
                name: &b.name,
                fields: None,
                applies: &b.applies,
            }),
        )?;
        validate_immutable_blocks(
            self,
            "rules.frontmatter_immutable",
            self.rules
                .frontmatter_immutable
                .iter()
                .map(|b| ImmutableBlock {
                    name: &b.name,
                    fields: Some(&b.fields),
                    applies: &b.applies,
                }),
        )?;

        // Trust weights: each non-negative, at least one > 0 so the
        // composite has a defined denominator.
        let w = &self.trust.weights;
        for (name, value) in [
            ("status", w.status),
            ("freshness", w.freshness),
            ("drift", w.drift),
            ("backlinks", w.backlinks),
        ] {
            if value < 0.0 || !value.is_finite() {
                return Err(Error::Config(format!(
                    "trust.weights.{name} must be a finite non-negative number; got {value}"
                )));
            }
        }
        if w.status + w.freshness + w.drift + w.backlinks <= 0.0 {
            return Err(Error::Config(
                "trust.weights must have at least one positive component".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.trust.low_trust_threshold)
            || !self.trust.low_trust_threshold.is_finite()
        {
            return Err(Error::Config(format!(
                "trust.low_trust_threshold must be a finite number in [0, 1]; got {}",
                self.trust.low_trust_threshold
            )));
        }

        // Similarity: same shape as trust.
        let sw = &self.similarity.weights;
        for (name, value) in [
            ("title", sw.title),
            ("tags", sw.tags),
            ("kind", sw.kind),
            ("directory", sw.directory),
            ("linked", sw.linked),
        ] {
            if value < 0.0 || !value.is_finite() {
                return Err(Error::Config(format!(
                    "similarity.weights.{name} must be a finite non-negative number; got {value}"
                )));
            }
        }
        if sw.title + sw.tags + sw.kind + sw.directory + sw.linked <= 0.0 {
            return Err(Error::Config(
                "similarity.weights must have at least one positive component".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.similarity.threshold) {
            return Err(Error::Config(format!(
                "similarity.threshold must be in [0, 1]; got {}",
                self.similarity.threshold
            )));
        }
        if self.similarity.default_limit == 0 {
            return Err(Error::Config(
                "similarity.default_limit must be ≥ 1 — `0` would never return any candidate"
                    .into(),
            ));
        }

        // Reject overlapping `kinds` across overrides. The lookup
        // helpers (`schema_override_for`, `required_for`, …) all stop
        // at the *first* matching block, so a kind that appears in two
        // overrides would have everything declared in the second block
        // silently ignored. The earlier failure mode we already guard
        // against — a tool writing a value the same config rejects —
        // has a mirror here: a config rule the same config silently
        // never applies. Refuse at load instead of debugging in prod.
        let mut kind_origin: BTreeMap<&str, usize> = BTreeMap::new();
        for (idx, ov) in self.schema.overrides.iter().enumerate() {
            for kind in &ov.kinds {
                if let Some(prev) = kind_origin.insert(kind.as_str(), idx) {
                    return Err(Error::Config(format!(
                        "schema.overrides[{idx}] declares kind {kind:?} which is \
                         already covered by schema.overrides[{prev}]; only the \
                         earlier block would take effect — merge them or \
                         re-partition the kind sets"
                    )));
                }
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

    /// Validate one [`ApplyTo`] block against the supplied status
    /// universe. Centralised so every rule family that accepts the
    /// scope triple rejects the same typos with the same message
    /// shape — a future fourth axis (`applies_to_owner`, …) lands
    /// here once instead of in five call sites.
    ///
    /// `status_universe` selects which set the rule's status filter
    /// must subset: [`StatusUniverse::Allowed`] for vocabulary rules
    /// (any allowed status is in scope); [`StatusUniverse::Terminal`]
    /// for immutability rules (only terminal statuses can be locked,
    /// and a non-terminal entry would silently scope to zero docs).
    fn validate_apply_to(
        &self,
        ctx: &str,
        applies: &ApplyTo,
        status_universe: StatusUniverse,
    ) -> Result<()> {
        for kind in &applies.kinds {
            if !self.kinds.allowed.iter().any(|k| k == kind) {
                return Err(Error::Config(format!(
                    "{ctx}.applies_to_kind contains {kind:?} which is not in \
                     kinds.allowed; add the kind or drop the filter"
                )));
            }
        }
        let (universe, label, hint) = match status_universe {
            StatusUniverse::Allowed => (
                &self.statuses.allowed,
                "statuses.allowed",
                "add the status or drop the filter",
            ),
            StatusUniverse::Terminal => (
                &self.statuses.terminal,
                "statuses.terminal",
                "immutability rules gate on terminal status, so a non-terminal \
                 entry would silently scope to zero documents",
            ),
        };
        for status in &applies.statuses {
            if !universe.iter().any(|s| s == status) {
                return Err(Error::Config(format!(
                    "{ctx}.applies_to_status contains {status:?} which is not in \
                     {label}; {hint}"
                )));
            }
        }
        for tag in &applies.tags {
            if tag.trim().is_empty() {
                return Err(Error::Config(format!(
                    "{ctx}.applies_to_tag contains an empty entry; \
                     remove it or replace with a real tag value"
                )));
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

            // If the same field also declares a non-string `types`
            // constraint, every enum value has to parse as that type.
            // Otherwise `scaffold`'s default ("first allowed enum
            // value") writes a document that immediately fails
            // `field_type` on the next `check` — observed with
            // `types = { priority = "integer" }` combined with
            // `enums = { priority = ["low", "medium", "high"] }`.
            if let Some(ty) = types.get(field)
                && let Some(bad) = allowed.iter().find(|v| !value_matches_field_type(v, *ty))
            {
                return Err(Error::Config(format!(
                    "{ctx}: enums.{field} value {bad:?} is not a valid \
                     {ty:?}; either drop the enum or widen types.{field}"
                )));
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
            // Self-consistency invariant: a `cross_field.require` field
            // must be capable of receiving a tool-generated default
            // value that the SAME rule then accepts. Without this
            // check, `scaffold` / `migrate` happily emit an empty
            // string for a `type = "string"` (or undeclared) field and
            // the next `check` immediately fires a `cross_field`
            // violation, breaking the "anything nodex writes passes
            // nodex's own check" invariant called out in
            // `.claude/rules/config-driven.md`.
            ensure_cross_field_default_satisfiable(&cf.require, types, enums, ctx)?;
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

    /// Whether nodes of the given kind are exempt from orphan detection.
    ///
    /// Driven by `detection.orphan_ok_kinds`. Pairs with the per-instance
    /// `node.orphan_ok` opt-out so callers can express both "this entire
    /// kind is leaf-by-design" and "this specific document is exceptional".
    /// Named to mirror the field and the per-node flag, paralleling
    /// `is_terminal` ↔ `statuses.terminal`.
    pub fn is_orphan_ok_kind(&self, kind: &str) -> bool {
        self.detection.orphan_ok_kinds.iter().any(|k| k == kind)
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

    /// Every frontmatter field name that is *declared* for a given
    /// kind — built-in fields, plus every key referenced by `required`,
    /// `types`, `enums`, or `cross_field` (global + first matching
    /// override). For `cross_field` the set includes both the
    /// `require` target *and* the field named on the LHS of the
    /// `when` predicate, so a rule like
    /// `when = "priority=high" require = "owner"` implicitly declares
    /// `priority` — otherwise strict mode would reject the very
    /// documents the predicate is meant to fire on. Used by
    /// [`crate::rules::schema::UnknownFieldRule`].
    pub fn declared_fields_for(&self, kind: &str) -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> = BUILTIN_FRONTMATTER_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        for f in self.required_for(kind) {
            out.insert(f.clone());
        }
        for f in self.types_for(kind).keys() {
            out.insert(f.clone());
        }
        for f in self.enums_for(kind).keys() {
            out.insert(f.clone());
        }
        for cf in self.cross_field_for(kind) {
            if let Ok(WhenPredicate::Equals { field, .. }) = parse_when(&cf.when) {
                out.insert(field);
            }
            out.insert(cf.require);
        }
        out
    }

    /// Union of [`Self::declared_fields_for`] across every kind in
    /// `kinds.allowed` plus the global schema (independent of kind).
    /// Used by validators that need a project-wide "is this field name
    /// known to *any* part of the schema?" question — for example,
    /// [`crate::config::RulesConfig::frontmatter_immutable`] rejects
    /// lock entries whose name is nowhere declared.
    pub fn declared_fields_universe(&self) -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> = BUILTIN_FRONTMATTER_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        // Global schema, independent of kind.
        for f in &self.schema.required {
            out.insert(f.clone());
        }
        for f in self.schema.types.keys() {
            out.insert(f.clone());
        }
        for f in self.schema.enums.keys() {
            out.insert(f.clone());
        }
        for cf in &self.schema.cross_field {
            if let Ok(WhenPredicate::Equals { field, .. }) = parse_when(&cf.when) {
                out.insert(field);
            }
            out.insert(cf.require.clone());
        }
        // Plus every per-kind override (in case an override declares
        // fields that no global ever references).
        for kind in &self.kinds.allowed {
            out.extend(self.declared_fields_for(kind));
        }
        out
    }

    /// Find the schema override that applies to a given kind, if any.
    pub fn schema_override_for(&self, kind: &str) -> Option<&SchemaOverride> {
        self.schema
            .overrides
            .iter()
            .find(|ov| ov.kinds.iter().any(|k| k == kind))
    }

    /// Every edge relation the project may emit — built-in relations
    /// (`references`, `supersedes`, `implements`, `related`, `covers`)
    /// plus every `[[parser.link_patterns]].relation` the operator
    /// declared. Consumed by surfaces that take user-supplied relation
    /// filters (`query dependents --relations …`, `git_drift_relations`)
    /// so a typo surfaces as a typed error instead of silently matching
    /// zero edges.
    pub fn known_relations(&self) -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> = crate::model::BUILTIN_EDGE_RELATIONS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        for lp in &self.parser.link_patterns {
            out.insert(lp.relation.clone());
        }
        out
    }

    /// The status value that tool-level actions (`scaffold`, `migrate`)
    /// should write when they create a new document of a given kind.
    ///
    /// Walks from the narrowest declaration to the broadest: per-kind
    /// override's `enums.status`, then the global `schema.enums.status`,
    /// then `statuses.allowed`. The first hit's `first()` wins.
    /// `Config::validate` guarantees each of these is either absent or
    /// non-empty, and that any `enums.status` covers the four lifecycle
    /// targets — so the result is always in-vocabulary and the invariant
    /// holding migrate / scaffold together with `check` never breaks.
    pub fn initial_status_for(&self, kind: &str) -> &str {
        if let Some(ov) = self.schema_override_for(kind)
            && let Some(allowed) = ov.enums.get("status")
            && let Some(first) = allowed.first()
        {
            return first.as_str();
        }
        if let Some(allowed) = self.schema.enums.get("status")
            && let Some(first) = allowed.first()
        {
            return first.as_str();
        }
        self.statuses
            .allowed
            .first()
            .map(String::as_str)
            .expect("statuses.allowed non-empty — enforced by Config::validate")
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

/// True when the raw frontmatter-style string `value` is a valid
/// member of the declared `FieldType`. Used by `Config::validate` to
/// reject configs that pair a typed field with an enum containing
/// values that can never satisfy the type.
fn value_matches_field_type(value: &str, ty: FieldType) -> bool {
    match ty {
        FieldType::String => true,
        FieldType::Integer => value.parse::<i64>().is_ok(),
        FieldType::Bool => matches!(value, "true" | "false"),
        FieldType::Date => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok(),
    }
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

/// Reject a `cross_field.require` whose field cannot receive a
/// tool-generated default value the same rule then accepts.
///
/// The `is_field_missing` predicate that powers `RequiredFieldRule`
/// and `CrossFieldRule` treats empty strings and empty arrays as
/// missing. So a `cross_field.require` pointing at a `type = "string"`
/// field would fire the moment `scaffold` / `migrate` writes the
/// empty-string default. The valid combinations are enumerated below;
/// anything else would let nodex write a document that fails its own
/// check, violating the "tool-written must pass" invariant.
fn ensure_cross_field_default_satisfiable(
    field: &str,
    types: &BTreeMap<String, FieldType>,
    enums: &BTreeMap<String, Vec<String>>,
    ctx: &str,
) -> Result<()> {
    // Enum-constrained fields default to the first allowed value,
    // which `Config::validate` guarantees is non-empty.
    if enums.contains_key(field) {
        return Ok(());
    }
    // Non-string typed fields default to `today` / `0` / `false` —
    // all non-empty when serialised back.
    if let Some(ty) = types.get(field) {
        return match ty {
            FieldType::Date | FieldType::Integer | FieldType::Bool => Ok(()),
            FieldType::String => Err(Error::Config(format!(
                "{ctx}: cross_field.require {field:?} is declared as `type = \"string\"`; \
                 a scaffolded / migrated document would receive an empty-string default \
                 that this very rule treats as missing. Constrain it with `enums = {{ \
                 {field} = [...] }}` so the default is meaningful, or pick a non-string \
                 type."
            ))),
        };
    }
    // Built-in fields fall into three groups for default-emptiness:
    //   safe   — date fields default to today; Option<String> scalars
    //            (`owner` / `superseded_by`) keep `Some("")` which the
    //            checker does not consider missing.
    //   unsafe — collection-valued built-ins (`supersedes`, `implements`,
    //            `related`, `tags`, `covers`) default to an empty Vec
    //            which `is_field_missing` flags.
    //   N/A    — `id` / `title` / `kind` / `status` are written from
    //            scaffold's positional args, never via `default_for_field`.
    //            We allow them so `cross_field.require = "status"` keeps
    //            working (an unusual but defensible use case).
    match field {
        "created" | "updated" | "reviewed" | "owner" | "superseded_by" | "id" | "title"
        | "kind" | "status" | "orphan_ok" => Ok(()),
        "supersedes" | "implements" | "related" | "tags" | "covers" => Err(Error::Config(format!(
            "{ctx}: cross_field.require {field:?} is a collection-valued built-in; \
             scaffold / migrate default it to `[]` which this very rule treats as \
             missing. Either pick a scalar field, or drop the cross_field constraint."
        ))),
        _ => unreachable!("ensure_field_known should have rejected unknown `{field}` already"),
    }
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
    use super::ApplyTo;
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
    fn validate_rejects_output_dir_escaping_root() {
        // `output.dir` is joined to the project root for every
        // build / report / cache write. A traversal value would
        // silently write artefacts outside the project root. Refuse at load.
        for bad in ["../escape", "/etc/nodex", "docs/../../out"] {
            let config = Config {
                output: OutputConfig {
                    dir: bad.to_string(),
                },
                ..Config::default()
            };
            match config.validate() {
                Err(Error::Config(msg)) => assert!(
                    msg.contains("output.dir") && msg.contains("escapes"),
                    "for {bad:?} got unexpected message: {msg}"
                ),
                other => panic!("value {bad:?} should have been rejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn validate_rejects_kinds_allowed_missing_fallback_kind() {
        // `migrate` / `parse_document` assign the fallback kind
        // ("generic") to any document whose path isn't covered by an
        // `identity.kind_rules` glob. If the user's `kinds.allowed`
        // omits it, that assignment immediately fails FieldEnumRule —
        // the tool writing a document its own config rejects. Refuse
        // at load.
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["adr".into()],
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("generic"), "message was: {msg}");
                assert!(msg.contains("fallback"), "message was: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_enum_value_failing_its_declared_type() {
        // `types = { priority = "integer" }` paired with
        // `enums = { priority = ["low", "medium", "high"] }` was an
        // accepted config that made `scaffold` emit an immediately-
        // invalid document (first enum value written, then FieldTypeRule
        // flagged it). Both constraints can legally coexist, but each
        // enum value must parse as the declared type.
        let config = Config {
            schema: SchemaConfig {
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into()],
                    required: vec![],
                    types: [("priority".to_string(), FieldType::Integer)]
                        .into_iter()
                        .collect(),
                    enums: [(
                        "priority".to_string(),
                        vec!["low".into(), "medium".into(), "high".into()],
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
                assert!(msg.contains("priority"), "message was: {msg}");
                assert!(msg.contains("\"low\""), "message was: {msg}");
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
    fn validate_rejects_orphan_ok_kind_outside_kinds_allowed() {
        // Listing a kind in `detection.orphan_ok_kinds` that isn't in
        // `kinds.allowed` would let the user think they had exempted
        // a kind from orphan detection while the runtime silently
        // exempts nothing. Refuse at load.
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["generic".into(), "guide".into(), "readme".into()],
            },
            detection: DetectionConfig {
                orphan_ok_kinds: vec!["skll".into()],
                ..DetectionConfig::default()
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("orphan_ok_kinds"), "message was: {msg}");
                assert!(msg.contains("\"skll\""), "message was: {msg}");
                assert!(msg.contains("kinds.allowed"), "message was: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn is_orphan_ok_kind_matches_configured_entries() {
        let config = Config {
            kinds: KindsConfig {
                allowed: vec!["generic".into(), "skill".into()],
            },
            detection: DetectionConfig {
                orphan_ok_kinds: vec!["skill".into()],
                ..DetectionConfig::default()
            },
            ..Config::default()
        };
        config.validate().unwrap();
        assert!(config.is_orphan_ok_kind("skill"));
        assert!(!config.is_orphan_ok_kind("generic"));
    }

    #[test]
    fn validate_rejects_overlapping_kinds_across_overrides() {
        // Two overrides both targeting `adr` would silently drop the
        // second block's declarations because every lookup helper
        // stops at the first match.
        let config = Config {
            schema: SchemaConfig {
                overrides: vec![
                    SchemaOverride {
                        kinds: vec!["adr".into()],
                        required: vec!["owner".into()],
                        types: BTreeMap::new(),
                        enums: BTreeMap::new(),
                        cross_field: vec![],
                    },
                    SchemaOverride {
                        kinds: vec!["adr".into(), "guide".into()],
                        required: vec!["reviewed".into()],
                        types: BTreeMap::new(),
                        enums: BTreeMap::new(),
                        cross_field: vec![],
                    },
                ],
                ..Default::default()
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("\"adr\""), "{msg}");
                assert!(msg.contains("overrides[1]"), "{msg}");
                assert!(msg.contains("overrides[0]"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_similarity_default_limit_zero() {
        let mut config = Config::default();
        config.similarity.default_limit = 0;
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("similarity.default_limit"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_frontmatter_immutable_unknown_field() {
        // Locking a field name nowhere declared in schema or built-ins
        // is a silent-skip trap — the rule never finds the misspelt
        // field in `field_changes`. `Config::validate` must reject it.
        use crate::config::{FrontmatterImmutableRuleConfig, RulesConfig};
        let config = Config {
            rules: RulesConfig {
                frontmatter_immutable: vec![FrontmatterImmutableRuleConfig {
                    name: "lock".into(),
                    fields: vec!["superceded_by".into()], // typo
                    applies: ApplyTo::default(),
                }],
                ..Default::default()
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("superceded_by"), "{msg}");
                assert!(msg.contains("frontmatter_immutable"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_frontmatter_immutable_builtin_and_declared_fields() {
        use crate::config::{FrontmatterImmutableRuleConfig, RulesConfig};
        let mut config = Config::default();
        // `superseded_by` is built-in; `decision_date` is declared via types.
        config
            .schema
            .types
            .insert("decision_date".into(), crate::config::FieldType::Date);
        config.rules = RulesConfig {
            frontmatter_immutable: vec![FrontmatterImmutableRuleConfig {
                name: "lock".into(),
                fields: vec!["superseded_by".into(), "decision_date".into()],
                applies: ApplyTo::default(),
            }],
            ..Default::default()
        };
        config.validate().expect("must accept valid lock list");
    }

    #[test]
    fn validate_rejects_cross_field_require_string_type() {
        // `type = "string"` defaults to `""` which `is_field_missing`
        // treats as missing. A scaffolded / migrated document would
        // immediately fire `cross_field` — exactly the self-consistency
        // gap the validator must close at load time.
        let mut config = Config::default();
        config
            .schema
            .types
            .insert("owner_team".into(), FieldType::String);
        config.schema.cross_field.push(CrossFieldSpec {
            when: "status=active".into(),
            require: "owner_team".into(),
        });
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("owner_team"), "{msg}");
                assert!(msg.contains("string"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_cross_field_require_collection_builtin() {
        // `tags`, `supersedes`, etc. default to `[]` which the checker
        // treats as missing — same self-consistency gap as the
        // string-type case, on the built-in side.
        let mut config = Config::default();
        config.schema.cross_field.push(CrossFieldSpec {
            when: "status=active".into(),
            require: "tags".into(),
        });
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("tags"), "{msg}");
                assert!(msg.contains("collection"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_cross_field_require_with_enum_or_date_type() {
        // Enum-constrained fields default to a non-empty first value;
        // date-typed fields default to today. Both survive
        // `is_field_missing` so the validator must accept them.
        let mut enum_config = Config::default();
        enum_config.schema.enums.insert(
            "priority".into(),
            vec!["low".into(), "medium".into(), "high".into()],
        );
        enum_config.schema.cross_field.push(CrossFieldSpec {
            when: "status=active".into(),
            require: "priority".into(),
        });
        enum_config
            .validate()
            .expect("enum-constrained require is safe");

        let mut date_config = Config::default();
        date_config
            .schema
            .types
            .insert("decision_date".into(), FieldType::Date);
        date_config.schema.cross_field.push(CrossFieldSpec {
            when: "status=active".into(),
            require: "decision_date".into(),
        });
        date_config.validate().expect("date-typed require is safe");
    }

    #[test]
    fn validate_accepts_cross_field_require_builtin_optional_scalar() {
        // The init template ships exactly this pattern:
        //   when = "status=superseded" require = "superseded_by"
        // It must keep validating.
        let config = Config::default();
        // Default required = ["id", "title", "kind", "status"] and
        // default statuses include `superseded`, so this is the
        // canonical superseded → superseded_by linkage.
        let mut c = config;
        c.schema.cross_field.push(CrossFieldSpec {
            when: "status=superseded".into(),
            require: "superseded_by".into(),
        });
        c.validate()
            .expect("canonical superseded → superseded_by must validate");
    }

    #[test]
    fn validate_rejects_low_trust_threshold_outside_unit_interval() {
        let mut config = Config::default();
        config.trust.low_trust_threshold = 1.5;
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("low_trust_threshold"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn parse_when_error_mentions_quoting_unsupported() {
        let err = parse_when("status==foo").unwrap_err();
        assert!(err.contains("embedded '='") || err.contains("exactly one"));
    }

    // ─── Annotations validation ────────────────────────────────────────

    fn annotations_config(blocks: Vec<AnnotationConfig>) -> Config {
        Config {
            annotations: blocks,
            ..Config::default()
        }
    }

    #[test]
    fn validate_accepts_well_formed_annotation_pattern() {
        annotations_config(vec![AnnotationConfig {
            name: "promotes".into(),
            pattern: r"\[PROMOTES:\s*(?P<id>[\w-]+)\]".into(),
            key: "id".into(),
            applies: ApplyTo::default(),
        }])
        .validate()
        .unwrap();
    }

    #[test]
    fn validate_rejects_duplicate_annotation_name() {
        let err = annotations_config(vec![
            AnnotationConfig {
                name: "x".into(),
                pattern: r"(?P<k>\w+)".into(),
                key: "k".into(),
                applies: ApplyTo::default(),
            },
            AnnotationConfig {
                name: "x".into(),
                pattern: r"(?P<j>\w+)".into(),
                key: "j".into(),
                applies: ApplyTo::default(),
            },
        ])
        .validate()
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("declared more than once"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_annotation_pattern_invalid_regex() {
        let err = annotations_config(vec![AnnotationConfig {
            name: "broken".into(),
            pattern: r"(unclosed".into(),
            key: "k".into(),
            applies: ApplyTo::default(),
        }])
        .validate()
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("is not a valid regex"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_annotation_key_missing_from_pattern() {
        let err = annotations_config(vec![AnnotationConfig {
            name: "typo".into(),
            pattern: r"(?P<id>\w+)".into(),
            // `key` references a capture name that doesn't exist in the
            // pattern — at runtime this would silently extract zero
            // markers, the textbook "no silent runtime skip" violation.
            key: "topic".into(),
            applies: ApplyTo::default(),
        }])
        .validate()
        .unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("not a named capture"), "{msg}");
                assert!(msg.contains("declared captures"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    // ─── body_line validation ──────────────────────────────────────────

    fn body_line_config(blocks: Vec<BodyLineRuleConfig>) -> Config {
        Config {
            rules: RulesConfig {
                body_line: blocks,
                ..Default::default()
            },
            ..Config::default()
        }
    }

    fn well_formed_body_line() -> BodyLineRuleConfig {
        let mut enums = BTreeMap::new();
        enums.insert("gate".into(), vec!["scope".into(), "design".into()]);
        BodyLineRuleConfig {
            name: "spec-log".into(),
            pattern: r"^- \*\*(?P<gate>[a-z-]+)\*\*".into(),
            applies: ApplyTo::default(),
            enums,
        }
    }

    #[test]
    fn validate_accepts_well_formed_body_line_block() {
        body_line_config(vec![well_formed_body_line()])
            .validate()
            .unwrap();
    }

    #[test]
    fn validate_rejects_body_line_duplicate_name() {
        let err = body_line_config(vec![well_formed_body_line(), well_formed_body_line()])
            .validate()
            .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("declared more than once"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_line_empty_enums() {
        let mut block = well_formed_body_line();
        block.enums.clear();
        let err = body_line_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("must have at least one entry"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_line_enum_capture_missing_from_pattern() {
        let mut block = well_formed_body_line();
        block.enums.clear();
        // `decision` is not a named capture in the pattern.
        block.enums.insert("decision".into(), vec!["accept".into()]);
        let err = body_line_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("is not a named capture"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_line_empty_allowed_list() {
        let mut block = well_formed_body_line();
        block.enums.insert("gate".into(), vec![]);
        let err = body_line_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("is empty"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_line_applies_to_unknown_kind() {
        let mut block = well_formed_body_line();
        block.applies.kinds = vec!["spec".into()];
        // Default kinds.allowed has no "spec" — Config::default has only generic/guide/readme.
        let err = body_line_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("not in kinds.allowed"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    // ─── Annotations validation ────────────────────────────────────────

    #[test]
    fn validate_rejects_annotation_applies_to_unknown_kind() {
        let err = annotations_config(vec![AnnotationConfig {
            name: "promotes".into(),
            pattern: r"(?P<id>\w+)".into(),
            key: "id".into(),
            applies: ApplyTo {
                kinds: vec!["learnng".into()], // typo for "learning"
                statuses: vec![],
                tags: vec![],
            },
        }])
        .validate()
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("not in kinds.allowed"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    // ─── git_drift_relations validation ────────────────────────────────

    #[test]
    fn validate_accepts_known_git_drift_relations() {
        let mut config = Config::default();
        config.detection.git_drift_threshold = Some(5);
        config.detection.git_drift_relations =
            vec!["references".into(), "implements".into(), "covers".into()];
        config.validate().expect("built-in relations must validate");
    }

    #[test]
    fn validate_accepts_user_declared_git_drift_relation() {
        // A relation produced by [[parser.link_patterns]] is part of
        // `known_relations()` — git_drift may filter on it.
        let mut config = Config::default();
        config.parser.link_patterns = vec![LinkPattern {
            pattern: r"@import\s+(.+)".into(),
            relation: "imports".into(),
        }];
        config.detection.git_drift_threshold = Some(3);
        config.detection.git_drift_relations = vec!["imports".into()];
        config
            .validate()
            .expect("user-declared relation must validate");
    }

    #[test]
    fn validate_rejects_unknown_git_drift_relation() {
        // A typo would silently match zero edges — `git_drift` would
        // self-report "fine" forever. Refused at load instead.
        let mut config = Config::default();
        config.detection.git_drift_threshold = Some(5);
        config.detection.git_drift_relations = vec!["referenced".into()];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("referenced"), "msg: {msg}");
                assert!(msg.contains("not a known relation"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    // ─── [meta] binary-version pin ─────────────────────────────────────
    //
    // `Config::load` runs the version check as the last gate. These
    // tests exercise the file path (not just `verify_version` in
    // isolation) so the wiring — parse → validate → version check —
    // stays connected.

    fn write_config(root: &std::path::Path, body: &str) {
        std::fs::write(root.join("nodex.toml"), body).expect("write nodex.toml");
    }

    #[test]
    fn load_accepts_meta_version_satisfying_current_binary() {
        // The wildcard requirement always matches; the load path must
        // not reject a project that pins to *any* nodex binary.
        let dir = tempfile::tempdir().expect("tempdir");
        write_config(dir.path(), "[meta]\nnodex_version = \"*\"\n");
        Config::load(dir.path()).expect("wildcard pin must load");
    }

    #[test]
    fn load_rejects_meta_version_unsatisfiable_by_current_binary() {
        // An upper bound below the current binary surfaces as
        // VERSION_MISMATCH, not CONFIG_ERROR — the config is internally
        // consistent, the running binary just can't honour it.
        let dir = tempfile::tempdir().expect("tempdir");
        write_config(dir.path(), "[meta]\nnodex_version = \"<0.0.1\"\n");
        let err = Config::load(dir.path()).unwrap_err();
        assert_eq!(
            err.code(),
            "VERSION_MISMATCH",
            "unsatisfiable pin must surface as VERSION_MISMATCH, got {err}"
        );
    }

    #[test]
    fn load_rejects_meta_version_with_malformed_requirement() {
        // A garbage SemVer requirement is a config defect, not a
        // version mismatch — `verify_version` routes it to
        // `Error::Config` so the operator sees CONFIG_ERROR with the
        // parser's diagnostic.
        let dir = tempfile::tempdir().expect("tempdir");
        write_config(dir.path(), "[meta]\nnodex_version = \"not-a-req\"\n");
        let err = Config::load(dir.path()).unwrap_err();
        assert_eq!(
            err.code(),
            "CONFIG_ERROR",
            "malformed requirement must surface as CONFIG_ERROR, got {err}"
        );
    }

    // ─── [[rules.body_block]] validation ───────────────────────────────

    fn well_formed_body_block() -> crate::config::BodyBlockRuleConfig {
        let mut enums = BTreeMap::new();
        enums.insert("status".into(), vec!["accepted".into(), "rejected".into()]);
        crate::config::BodyBlockRuleConfig {
            name: "adr-decision".into(),
            start_pattern: r"^## Decision \((?P<status>[a-z]+)\)".into(),
            end_pattern: r"^## ".into(),
            applies: ApplyTo::default(),
            enums,
        }
    }

    fn body_block_config(blocks: Vec<crate::config::BodyBlockRuleConfig>) -> Config {
        let mut c = Config::default();
        c.rules.body_block = blocks;
        c
    }

    #[test]
    fn validate_accepts_well_formed_body_block() {
        body_block_config(vec![well_formed_body_block()])
            .validate()
            .expect("well-formed block must validate");
    }

    #[test]
    fn validate_rejects_body_block_invalid_start_pattern() {
        let mut block = well_formed_body_block();
        block.start_pattern = r"(?P<unterminated".into();
        let err = body_block_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("start_pattern"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_block_invalid_end_pattern() {
        let mut block = well_formed_body_block();
        block.end_pattern = r"(?P<also-unterminated".into();
        let err = body_block_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("end_pattern"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_block_enum_key_not_in_start_pattern() {
        // The user enumerated `priority` but the start_pattern only
        // captures `status`. A silent skip here would let a typo
        // never fire.
        let mut block = well_formed_body_block();
        block.enums.insert("priority".into(), vec!["high".into()]);
        let err = body_block_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("not a named capture"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_block_empty_enums() {
        // A body_block with no enum constraints has no failure mode;
        // refused at load so it can't loiter unused.
        let mut block = well_formed_body_block();
        block.enums = BTreeMap::new();
        let err = body_block_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("at least one entry"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_block_duplicate_names() {
        let err = body_block_config(vec![well_formed_body_block(), well_formed_body_block()])
            .validate()
            .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("declared more than once"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_block_applies_to_unknown_kind() {
        let mut block = well_formed_body_block();
        block.applies.kinds = vec!["adr".into()];
        // Default kinds.allowed has no "adr"
        let err = body_block_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("not in kinds.allowed"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    // ─── [[rules.body_immutable]] validation ───────────────────────────

    fn body_immutable_block(name: &str) -> crate::config::BodyImmutableRuleConfig {
        crate::config::BodyImmutableRuleConfig {
            name: name.into(),
            mode: crate::config::BodyImmutableMode::Frozen,
            applies: ApplyTo::default(),
        }
    }

    #[test]
    fn validate_rejects_body_immutable_empty_name() {
        let mut c = Config::default();
        c.rules.body_immutable = vec![body_immutable_block("")];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("non-empty"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_immutable_duplicate_name() {
        // Two blocks with the same name would emit identical
        // violation rule_ids, making CI dashboards confuse one
        // policy for another. Refused at load.
        let mut c = Config::default();
        c.rules.body_immutable = vec![body_immutable_block("dup"), body_immutable_block("dup")];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("declared more than once"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_immutable_unknown_kind() {
        // A typo in `applies_to_kind` would silently match zero
        // documents forever. Same "no silent runtime skips"
        // discipline body_line / annotations apply.
        let mut c = Config::default();
        let mut block = body_immutable_block("policy");
        block.applies.kinds = vec!["adrr".into()]; // typo
        c.rules.body_immutable = vec![block];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("not in kinds.allowed"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    // ─── shared scope-triple validation ─────────────────────────────────

    #[test]
    fn validate_rejects_body_line_unknown_status() {
        // `applies_to_status` is a closed enum bound to
        // statuses.allowed. A typo would silently scope to zero
        // documents forever — refused at load.
        let mut block = well_formed_body_line();
        block.applies.statuses = vec!["activ".into()]; // typo for "active"
        let err = body_line_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("applies_to_status"), "{msg}");
                assert!(msg.contains("not in statuses.allowed"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_annotation_unknown_status() {
        let err = annotations_config(vec![AnnotationConfig {
            name: "promotes".into(),
            pattern: r"(?P<id>\w+)".into(),
            key: "id".into(),
            applies: ApplyTo {
                kinds: vec![],
                statuses: vec!["arxived".into()],
                tags: vec![],
            },
        }])
        .validate()
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("not in statuses.allowed"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_block_empty_tag_entry() {
        // The tag axis has no allowlist (tags are free vocabulary),
        // but empty strings are still rejected — they'd silently
        // match no node and look like a successful filter.
        let mut block = well_formed_body_block();
        block.applies.tags = vec!["".into()];
        let err = body_block_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("empty entry"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_body_line_with_status_and_tag_scope() {
        let mut c = Config::default();
        c.kinds.allowed.push("spec".into());
        let mut block = well_formed_body_line();
        block.applies.kinds = vec!["spec".into()];
        block.applies.statuses = vec!["active".into()];
        block.applies.tags = vec!["billing".into()];
        c.rules.body_line = vec![block];
        c.validate()
            .expect("well-formed scope triple must validate");
    }

    #[test]
    fn validate_rejects_body_immutable_status_outside_terminal() {
        // Immutability rules gate on terminal status. An entry that
        // isn't terminal would silently scope to zero documents,
        // which violates the no-silent-skip discipline.
        let mut c = Config::default();
        let mut block = body_immutable_block("policy");
        block.applies.statuses = vec!["active".into()]; // not terminal
        c.rules.body_immutable = vec![block];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("statuses.terminal"), "{msg}");
                assert!(msg.contains("\"active\""), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_body_immutable_empty_tag_entry() {
        let mut c = Config::default();
        let mut block = body_immutable_block("policy");
        block.applies.tags = vec!["".into()];
        c.rules.body_immutable = vec![block];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("empty entry"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    // ─── [[rules.frontmatter_immutable]] validation ─────────────────────

    fn frontmatter_immutable_block(
        name: &str,
        fields: Vec<&str>,
    ) -> crate::config::FrontmatterImmutableRuleConfig {
        crate::config::FrontmatterImmutableRuleConfig {
            name: name.into(),
            fields: fields.into_iter().map(String::from).collect(),
            applies: ApplyTo::default(),
        }
    }

    #[test]
    fn validate_rejects_frontmatter_immutable_empty_name() {
        let mut c = Config::default();
        c.rules.frontmatter_immutable = vec![frontmatter_immutable_block("", vec!["id"])];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("non-empty"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_frontmatter_immutable_duplicate_name() {
        let mut c = Config::default();
        c.rules.frontmatter_immutable = vec![
            frontmatter_immutable_block("dup", vec!["id"]),
            frontmatter_immutable_block("dup", vec!["kind"]),
        ];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("declared more than once"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_frontmatter_immutable_empty_fields() {
        // An empty `fields` list locks nothing — refused at load,
        // same reason an empty enums list is refused for body_line.
        let mut c = Config::default();
        c.rules.frontmatter_immutable = vec![frontmatter_immutable_block("empty", vec![])];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("at least one field"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_frontmatter_immutable_status_outside_terminal() {
        let mut c = Config::default();
        let mut block = frontmatter_immutable_block("lock", vec!["id"]);
        block.applies.statuses = vec!["active".into()];
        c.rules.frontmatter_immutable = vec![block];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("statuses.terminal"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_frontmatter_immutable_scope_triple() {
        let mut c = Config::default();
        c.kinds.allowed.push("adr".into());
        let mut block = frontmatter_immutable_block("lock", vec!["id"]);
        block.applies.kinds = vec!["adr".into()];
        block.applies.statuses = vec!["superseded".into()];
        block.applies.tags = vec!["signed-off".into()];
        c.rules.frontmatter_immutable = vec![block];
        c.validate().expect("well-formed scope triple must load");
    }

    #[test]
    fn validate_accepts_body_immutable_block_with_allowed_kind() {
        let mut c = Config::default();
        c.kinds.allowed.push("adr".into());
        let mut block = body_immutable_block("policy");
        block.applies.kinds = vec!["adr".into()];
        c.rules.body_immutable = vec![block];
        c.validate().expect("well-formed block must load");
    }

    #[test]
    fn load_accepts_meta_omitted_entirely() {
        // The pin is opt-in. A config with no `[meta]` block must load
        // exactly the same as a config with `nodex_version` unset —
        // this is the recommended default during early development.
        let dir = tempfile::tempdir().expect("tempdir");
        write_config(dir.path(), "[scope]\ninclude = [\"**/*.md\"]\n");
        Config::load(dir.path()).expect("absent [meta] must load");
    }
}
