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

/// Closed set of `condition` values honoured by
/// `builder::scanner::apply_conditional_excludes`. Single source of
/// truth: `Config::validate` rejects unknown values, and the scanner
/// only branches on members of this set. Adding a new condition
/// requires extending this constant **and** the matching arm in the
/// scanner — keeping load-time validation in lockstep with runtime
/// behaviour (no silent runtime skips).
pub const CONDITIONAL_EXCLUDE_CONDITIONS: &[&str] = &["status_terminal"];

/// Closed set of placeholder names `identity.id_rules[].template`
/// understands. Single source of truth: `Config::validate` rejects
/// `{anything-else}`, and `parser::identity::expand_template` only
/// substitutes members of this set. A typo like `{stme}` would
/// otherwise load cleanly and produce a literal `{stme}` in every
/// generated id. Adding a new placeholder requires extending this
/// constant **and** the matching substitution arm — keeping load-time
/// validation in lockstep with runtime behaviour (no silent runtime
/// skips).
pub const ID_TEMPLATE_PLACEHOLDERS: &[&str] = &["kind", "stem", "parent", "path_slug"];

/// The well-formed-placeholder regex shared by [`scan_template_placeholders`]
/// and [`scan_template_malformed_braces`]. A "well-formed" placeholder is
/// `{` + ASCII identifier + `}` with no whitespace, no nesting, and no
/// unmatched brace. Lazy-initialised, single-allocation; the literal is a
/// compile-time constant so `Regex::new` cannot fail.
fn id_template_placeholder_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static ID_TEMPLATE_RE: OnceLock<regex::Regex> = OnceLock::new();
    ID_TEMPLATE_RE.get_or_init(|| {
        regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}")
            .expect("ID_TEMPLATE_RE literal is always a valid regex")
    })
}

/// Extract every well-formed `{ident}` placeholder name from a template.
/// Returns `Vec<String>` (not `BTreeSet`) so the validator reports the
/// *first* occurrence of a typo in source order. The regex matches an
/// ASCII-identifier alphabet so legitimate body content like
/// `{0..5}` or `{ a }` never gets misread as a placeholder.
fn scan_template_placeholders(template: &str) -> Vec<String> {
    id_template_placeholder_re()
        .captures_iter(template)
        .map(|c| c[1].to_string())
        .collect()
}

/// True if `template` contains any `{` or `}` outside a well-formed
/// `{ident}` placeholder. Catches `{ kind }` (whitespace), `{kind`
/// (unclosed), `kind}` (unopened), and `{{kind}}` (double-brace) —
/// every form `expand_template` would otherwise emit literal into the
/// generated id without warning. We deliberately don't support `{{`/`}}`
/// as a literal-brace escape: ID templates rarely need a literal brace,
/// and supporting an escape would complicate both the validator and the
/// substitution.
fn scan_template_malformed_braces(template: &str) -> bool {
    let stripped = id_template_placeholder_re().replace_all(template, "");
    stripped.contains('{') || stripped.contains('}')
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

/// Map file path to document kind using glob patterns.
///
/// Rules are evaluated in order — first matching glob wins.
/// Order matters: reordering rules changes which kind is inferred.
/// If no rule matches, `FALLBACK_KIND` ("generic") is assigned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindRule {
    /// Glob pattern (e.g., "docs/decisions/**" matches ADRs)
    pub glob: String,
    /// Kind to assign when glob matches (e.g., "adr")
    pub kind: String,
}

/// Map document (kind, path) to an ID using templates.
///
/// Rules are evaluated in order — first matching rule wins.
/// Order matters: reordering rules changes ID inference.
/// If no rule matches, default ID "{kind}-{stem}" is generated.
///
/// Template substitution:
/// - {kind}: the document kind
/// - {stem}: filename without extension (slugified)
/// - {parent}: parent directory name (slugified)
/// - {path_slug}: full relative path minus extension (slugified)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdRule {
    /// Kind filter: "*" = all kinds, or specific kind name (e.g., "adr")
    #[serde(default)]
    pub kind: String,
    /// Optional path glob filter (applied after kind match)
    #[serde(default)]
    pub glob: Option<String>,
    /// Template for ID generation
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
/// The `when` string is parsed into a `WhenPredicate` at load time.
/// Supported forms: `field=value`, `field in {v1,v2}`, `field exists`,
/// `field not_exists`.
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
}

/// One body-immutability policy. Multiple blocks let a project apply
/// different locking semantics to different kinds — ADRs `frozen`
/// (decisions are immutable in spirit), narratives `append_only`
/// (history grows but does not rewrite). The rule activates only for
/// nodes whose *current* status is terminal — pre-terminal documents
/// are still authoring drafts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyImmutableRuleConfig {
    /// Stable identifier used in the violation `rule_id`
    /// (`body_immutable/<name>`) and in the rule manifest. Must be
    /// unique across all `[[rules.body_immutable]]` blocks.
    pub name: String,
    /// Lock semantic for matching documents.
    pub mode: BodyImmutableMode,
    /// Which kinds this block locks. Empty = every kind. Every entry
    /// must be in `kinds.allowed`; `Config::load` enforces.
    #[serde(default)]
    pub kinds: Vec<String>,
}

/// One frontmatter-immutability policy. Multiple blocks let a project
/// lock different field sets in different parts of the corpus.
/// Inert without `--since`. Symmetric with
/// [`BodyImmutableRuleConfig`]: each block carries a unique `name`,
/// a kind filter, and the per-block payload (`fields`).
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
    /// Which kinds this block locks. Empty = every kind. Every entry
    /// must be in `kinds.allowed`; `Config::load` enforces.
    #[serde(default)]
    pub kinds: Vec<String>,
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
    /// Which kinds this rule scans. Empty = every kind. Every entry
    /// must be in `kinds.allowed`; `Config::load` enforces.
    #[serde(default)]
    pub kinds: Vec<String>,
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
    /// Which kinds this annotation is extracted from. Empty = every
    /// kind. Every entry must be in `kinds.allowed`; `Config::load`
    /// enforces.
    #[serde(default)]
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionConfig {
    /// `Some(n)` where n > 0: documents with updated date older than n days are flagged as stale.
    /// `None`: stale detection disabled.
    ///
    /// Zero is not permitted: use `None` to disable.
    #[serde(default = "default_stale_days")]
    pub stale_days: Option<u32>,
    /// Number of days after document creation during which orphan detection is suppressed.
    ///
    /// New documents are often in a grace period where they haven't yet been linked from
    /// other documents. This setting allows them to exist without triggering orphan warnings.
    ///
    /// Default: 14 days. Set to 0 to disable grace period and require immediate linking.
    ///
    /// Design note: Unlike `stale_days` and `git_drift_threshold` (which use `Option<u32>`
    /// with `Some(0)` rejected), `orphan_grace_days` is a plain `u32`. This is intentional:
    /// zero is a VALID and useful value (no grace period = immediate orphan detection).
    /// The semantic is: "suppress orphan detection for N days" where N=0 means "suppress nothing".
    ///
    /// A document is considered "orphan-ok" if ANY of these is true:
    /// 1. Its kind is in `orphan_ok_kinds` (leaf-by-design kinds), OR
    /// 2. Its frontmatter has `orphan_ok: true` (per-node opt-out), OR
    /// 3. Its created date is within the grace period (< orphan_grace_days old)
    ///
    /// These three mechanisms are independent by design, allowing flexible orphan policies:
    /// - Grace period: "all new documents get a warmup window"
    /// - orphan_ok_kinds: "this kind is always orphan-ok"
    /// - orphan_ok field: "this specific doc is intentionally orphaned"
    ///
    /// FUTURE: The grace period mechanism may become Option<u32> in a future version
    /// for consistency with threshold-based settings. For now, set to 0 if you prefer
    /// explicit per-kind or per-node control without time-based grace.
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
    /// `Some(n)` where n > 0 enables [`crate::rules::git_drift::GitDriftRule`]: a
    /// document is flagged when the referenced docs it points to have
    /// accumulated more than `n` git commits since this document's
    /// `reviewed` date. `None` (default) disables the rule.
    ///
    /// Zero is not permitted: use `None` to disable.
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

fn default_stale_days() -> Option<u32> {
    Some(180)
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
    /// Per-kind weight overrides. Each entry replaces the global
    /// `weights` entirely for nodes whose kind is listed in the
    /// entry's `kinds` vec — no field-level merge. Mirrors the
    /// `[[schema.overrides]]` design: first match wins, overlap
    /// rejected at load.
    #[serde(default)]
    pub overrides: Vec<TrustWeightOverride>,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            weights: default_trust_weights(),
            overrides: Vec::new(),
        }
    }
}

/// Per-kind trust weight override. Replaces the global `TrustWeights`
/// entirely for nodes whose kind appears in `kinds`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustWeightOverride {
    pub kinds: Vec<String>,
    pub weights: TrustWeights,
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
    /// Default `limit` applied when callers don't supply one — this
    /// is the operator-capacity contract. Score cutoffs are not part
    /// of the ranking primitive; opt-in filters live at the CLI
    /// layer (`--min-score`).
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
    kinds: &'a [String],
}

/// Refuse any immutability block whose `name`, kind filter, or
/// field-list (frontmatter only) would silently mis-fire at check
/// time.
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
        config.validate_kinds(&ctx, block.kinds)?;
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
    ///   `supersedes`, `implements`, `related`, `covers`) — these
    ///   cannot be validated against a scalar set, so silent ignore
    ///   would trap users who typed the obvious syntax and saw no
    ///   effect.
    /// - `enums.status` / `enums.kind` values that are not in the
    ///   corresponding global `allowed` list.
    /// - `cross_field.when` expressions that don't parse.
    /// - `cross_field.when`'s LHS and `cross_field.require` referring
    ///   to a field name that is not a built-in and is not declared in
    ///   the override's `types` / `enums` / `required`.
    /// - `equals` / `in` predicates on collection-valued fields — these
    ///   always evaluate false; `exists` / `not_exists` should be used
    ///   instead.
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

        // `statuses.terminal` drives `is_terminal`, which gates
        // body / frontmatter immutability rules and decides which
        // statuses block further lifecycle transitions. A terminal
        // entry that is not in `statuses.allowed` is the "tool writes
        // a value the same config rejects" failure mode in two
        // different ways at once: a `lifecycle archive` on a kind
        // whose terminal status is mis-spelled would silently never
        // terminate, and any node that *did* land on the typo'd
        // status would later fail FieldEnumRule. Refuse at load.
        for status in &self.statuses.terminal {
            if !self.statuses.allowed.iter().any(|s| s == status) {
                return Err(Error::Config(format!(
                    "statuses.terminal contains {status:?} which is not in statuses.allowed; \
                     every terminal status must also be in allowed"
                )));
            }
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
                "kinds.allowed is missing the required fallback kind {fallback:?}. \
                 \n\nWhy? Every document must have a kind. When no identity.kind_rules glob matches \
                 a file's path, {fallback:?} is assigned as the catch-all kind. Without it, \
                 the parser would fail on unclassified documents. \
                 \n\nHow to fix: \
                 \n  Option 1 (recommended): Add {fallback:?} to kinds.allowed: \
                 \n    kinds.allowed = [\"adr\", \"guide\", {fallback:?}, ...] \
                 \n  Option 2: Remove kinds.allowed entirely to use defaults (includes {fallback:?}): \
                 \n    # kinds.allowed is omitted, using built-in defaults \
                 \n\nAlternatively, declare exhaustive identity.kind_rules to classify all documents, \
                 in which case {fallback:?} becomes a safety net that never fires.",
                fallback = crate::parser::identity::FALLBACK_KIND
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

        // Detection thresholds must be strictly positive (or None to disable).
        // Zero is not a valid threshold: it would have ambiguous semantics
        // ("disabled" vs "immediate"), so reject it at load time.
        if let Some(0) = self.detection.stale_days {
            return Err(Error::Config(
                "detection.stale_days must be > 0 or None (omitted to disable); got 0".to_string(),
            ));
        }

        if let Some(0) = self.detection.git_drift_threshold {
            return Err(Error::Config(
                "detection.git_drift_threshold must be > 0 or None (omitted to disable); got 0"
                    .to_string(),
            ));
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
            // `parser::identity::infer_kind` skips id_rules whose `kind`
            // is neither `*` nor the inferred kind. A value outside
            // `kinds.allowed` would silently never match — the rule
            // loads cleanly and the runtime applies it to nothing.
            // Refuse at load instead, matching the same subset
            // discipline as `identity.kind_rules[].kind` above.
            if ir.kind != "*" && !self.kinds.allowed.iter().any(|a| a == &ir.kind) {
                return Err(Error::Config(format!(
                    "identity.id_rules[{idx}].kind {:?} is not in kinds.allowed; \
                     use \"*\" for any-kind or one of the allowed kinds",
                    ir.kind
                )));
            }
            // `parser::identity::expand_template` only substitutes the
            // names listed in `ID_TEMPLATE_PLACEHOLDERS`; an unknown
            // placeholder (typo like `{stme}`) silently survives the
            // substitution and ends up literal in every generated id.
            // Reject any `{ident}` that isn't a recognised placeholder
            // at load — keeping validation in lockstep with the
            // substitution arms (no silent runtime skips).
            for placeholder in scan_template_placeholders(&ir.template) {
                if !ID_TEMPLATE_PLACEHOLDERS.contains(&placeholder.as_str()) {
                    return Err(Error::Config(format!(
                        "identity.id_rules[{idx}].template references unknown placeholder {placeholder:?}; \
                         valid placeholders: {{kind}}, {{stem}}, {{parent}}, {{path_slug}}"
                    )));
                }
            }
            // After accepting every well-formed `{ident}`, any leftover
            // `{` or `}` is a malformed brace: whitespace inside
            // (`{ kind }`), an unmatched brace (`{kind`, `kind}`), or a
            // double-brace (`{{kind}}`). `expand_template` would emit
            // every such fragment literal into the generated id — the
            // exact "no silent runtime skips" failure mode this
            // validator exists to refuse.
            if scan_template_malformed_braces(&ir.template) {
                return Err(Error::Config(format!(
                    "identity.id_rules[{idx}].template {template:?} contains malformed brace syntax; \
                     placeholders must be exactly {{kind}} / {{stem}} / {{parent}} / {{path_slug}} \
                     with no whitespace, no unmatched braces, and no double-brace escape",
                    template = ir.template,
                )));
            }
        }
        for (idx, ce) in self.scope.conditional_exclude.iter().enumerate() {
            globset::Glob::new(&ce.parent_glob).map_err(|e| {
                Error::Config(format!(
                    "scope.conditional_exclude[{idx}].parent_glob {:?} is not a valid glob: {e}",
                    ce.parent_glob
                ))
            })?;
            // `builder::scanner::apply_conditional_excludes` only
            // honours `condition = "status_terminal"`; any other value
            // is silently skipped, which would make the rule load
            // cleanly and exclude nothing. Reject unknown conditions
            // at load so a typo surfaces with the valid set in the
            // error message.
            if !CONDITIONAL_EXCLUDE_CONDITIONS
                .iter()
                .any(|c| *c == ce.condition)
            {
                return Err(Error::Config(format!(
                    "scope.conditional_exclude[{idx}].condition {value:?} is unknown; \
                     valid values: {valid:?}",
                    value = ce.condition,
                    valid = CONDITIONAL_EXCLUDE_CONDITIONS,
                )));
            }
        }
        for (idx, lp) in self.parser.link_patterns.iter().enumerate() {
            let re = regex::Regex::new(&lp.pattern).map_err(|e| {
                Error::Config(format!(
                    "parser.link_patterns[{idx}].pattern {:?} is not a valid regex: {e}",
                    lp.pattern
                ))
            })?;
            // `parser::body` reads edge targets from `caps.get(1)` — the
            // first (and only) capture group. Each pattern must have
            // exactly one capture group to avoid silent misbehavior.
            // `captures_len()` counts group 0 (the full match) plus
            // every explicit `(...)` group, so a value of 2 means one
            // capture group was declared.
            match re.captures_len() {
                0 | 1 => {
                    return Err(Error::Config(format!(
                        "parser.link_patterns[{idx}].pattern {pattern:?} has no capture group; \
                         add exactly one (...) so link targets can be extracted",
                        pattern = lp.pattern,
                    )));
                }
                2 => {
                    // Expected: exactly one capture group
                }
                _ => {
                    return Err(Error::Config(format!(
                        "parser.link_patterns[{idx}].pattern {pattern:?} has multiple capture groups; \
                         only the first capture group is used, so having more is confusing. \
                         Use a single (...) group for the link target.",
                        pattern = lp.pattern,
                    )));
                }
            }
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
            self.validate_kinds(
                &format!("rules.body_line[{idx}] ({name:?})", name = bl.name),
                &bl.kinds,
            )?;
        }

        // Annotation patterns: compile, key ∈ named captures, kinds
        // valid, names unique. Same "no silent runtime skips" discipline
        // as everywhere else — a typo in `key` or `kinds`
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
            self.validate_kinds(
                &format!("annotations[{idx}] ({name:?})", name = ann.name),
                &ann.kinds,
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

        // Immutability rules — symmetric validation for the two
        // diff-aware lock families. Both surface as
        // `<family>/<name>` rule_ids, both gate on terminal status,
        // both accept the kind filter. Centralising the validation
        // means a future third immutability family lands once.
        validate_immutable_blocks(
            self,
            "rules.body_immutable",
            self.rules.body_immutable.iter().map(|b| ImmutableBlock {
                name: &b.name,
                fields: None,
                kinds: &b.kinds,
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
                    kinds: &b.kinds,
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
        let w_sum = w.status + w.freshness + w.drift + w.backlinks;
        if !w_sum.is_finite() || w_sum <= 0.0 {
            return Err(Error::Config(
                "trust.weights must have at least one positive component \
                 and a finite sum"
                    .into(),
            ));
        }
        // Trust weight overrides: reject duplicate kinds, validate
        // weight values. Mirrors the schema.overrides overlap
        // detection — first-match lookup means a kind in two
        // overrides would silently ignore the second block.
        let mut trust_kind_origin: BTreeMap<&str, usize> = BTreeMap::new();
        for (idx, ov) in self.trust.overrides.iter().enumerate() {
            let ctx = format!("trust.overrides[{idx}]");
            if ov.kinds.is_empty() {
                return Err(Error::Config(format!("{ctx}.kinds must not be empty")));
            }
            self.validate_kinds(&ctx, &ov.kinds)?;

            for kind in &ov.kinds {
                if let Some(prev) = trust_kind_origin.insert(kind.as_str(), idx) {
                    return Err(Error::Config(format!(
                        "trust.overrides[{idx}] declares kind {kind:?} which is \
                         already covered by trust.overrides[{prev}]"
                    )));
                }
            }

            let tw = &ov.weights;
            for (name, value) in [
                ("status", tw.status),
                ("freshness", tw.freshness),
                ("drift", tw.drift),
                ("backlinks", tw.backlinks),
            ] {
                if value < 0.0 || !value.is_finite() {
                    return Err(Error::Config(format!(
                        "{ctx}.weights.{name} must be a finite non-negative number; got {value}"
                    )));
                }
            }
            let tw_sum = tw.status + tw.freshness + tw.drift + tw.backlinks;
            if !tw_sum.is_finite() || tw_sum <= 0.0 {
                return Err(Error::Config(format!(
                    "{ctx}.weights must have at least one positive component \
                     and a finite sum"
                )));
            }
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
        let sw_sum = sw.title + sw.tags + sw.kind + sw.directory + sw.linked;
        if !sw_sum.is_finite() || sw_sum <= 0.0 {
            return Err(Error::Config(
                "similarity.weights must have at least one positive component \
                 and a finite sum"
                    .into(),
            ));
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
            // Symmetric with every other `kinds` filter (rules.body_line,
            // rules.body_immutable, rules.frontmatter_immutable,
            // annotations, trust.overrides): a typo in `kinds` would
            // otherwise silently match no document and the override
            // would never fire — the exact "no silent runtime skips"
            // failure mode this validator exists to refuse.
            self.validate_kinds(&ctx, &ov.kinds)?;
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

    /// Validate one rule block's `kinds` filter against
    /// [`KindsConfig::allowed`]. Centralised so every rule family
    /// rejects an out-of-vocabulary kind with the same message shape.
    fn validate_kinds(&self, ctx: &str, kinds: &[String]) -> Result<()> {
        for kind in kinds {
            if !self.kinds.allowed.iter().any(|k| k == kind) {
                return Err(Error::Config(format!(
                    "{ctx}.kinds contains {kind:?} which is not in \
                     kinds.allowed; add the kind or drop the filter"
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
            let when_field = match &predicate {
                WhenPredicate::Equals { field, .. }
                | WhenPredicate::In { field, .. }
                | WhenPredicate::Exists { field }
                | WhenPredicate::NotExists { field } => field,
            };
            ensure_field_known(when_field, required, types, enums, ctx, "cross_field.when")?;
            if is_collection_builtin(when_field)
                && matches!(
                    predicate,
                    WhenPredicate::Equals { .. } | WhenPredicate::In { .. }
                )
            {
                return Err(Error::Config(format!(
                    "{ctx}: cross_field.when references collection field {when_field:?}; \
                     equals/in predicates operate on scalar values — \
                     use exists/not_exists for collection presence"
                )));
            }
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
            if let Ok(pred) = parse_when(&cf.when) {
                let field = match pred {
                    WhenPredicate::Equals { field, .. }
                    | WhenPredicate::In { field, .. }
                    | WhenPredicate::Exists { field }
                    | WhenPredicate::NotExists { field } => field,
                };
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
            if let Ok(pred) = parse_when(&cf.when) {
                let field = match pred {
                    WhenPredicate::Equals { field, .. }
                    | WhenPredicate::In { field, .. }
                    | WhenPredicate::Exists { field }
                    | WhenPredicate::NotExists { field } => field,
                };
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

    /// Find the trust weight override that applies to a given kind.
    pub fn trust_weight_override_for(&self, kind: &str) -> Option<&TrustWeightOverride> {
        self.trust
            .overrides
            .iter()
            .find(|ov| ov.kinds.iter().any(|k| k == kind))
    }

    /// Merged trust weights for a kind — override replaces global
    /// entirely when matched. Parallels `required_for` / `types_for`
    /// / `enums_for` in taking a kind and returning the effective view.
    pub fn trust_weights_for(&self, kind: &str) -> TrustWeights {
        match self.trust_weight_override_for(kind) {
            Some(ov) => ov.weights,
            None => self.trust.weights,
        }
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
    /// Get the initial status for newly-created documents of the given kind.
    ///
    /// Used by `migrate` and `scaffold` commands when creating documents
    /// without an explicit --status. The value is derived from schema.enums.status
    /// (or per-kind override), taking the first allowed status.
    ///
    /// Self-consistency guarantee: The returned value is always in
    /// `statuses.allowed` and `enums.status` (if declared), ensuring that
    /// scaffold/migrate output passes the same config's `check`.
    ///
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
    /// `<field> in {v1,v2,...}` — match when the field's value is one of the listed values.
    In { field: String, values: Vec<String> },
    /// `<field> exists` — match when the field is present (non-empty).
    Exists { field: String },
    /// `<field> not_exists` — match when the field is absent (or empty).
    NotExists { field: String },
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
    "orphan_ok",
];

/// Collection-valued built-in fields. Enum/type constraints on these
/// must be rejected — there is no single scalar value to check.
pub const BUILTIN_COLLECTION_FIELDS: &[&str] =
    &["tags", "supersedes", "implements", "related", "covers"];

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

/// Parse a `cross_field.when` expression.
///
/// Accepted forms:
/// - `<field>=<value>` — equality predicate.
/// - `<field> in {v1,v2,...}` — membership predicate (comma-separated inside braces).
/// - `<field> exists` — presence predicate (field is non-empty).
/// - `<field> not_exists` — absence predicate (field is absent or empty).
///
/// Rejects `==` and any form where the value starts with `=`, so a typo
/// can never silently turn into a predicate that matches nothing. Also
/// rejects empty field names and empty value lists.
pub fn parse_when(raw: &str) -> std::result::Result<WhenPredicate, String> {
    let trimmed = raw.trim();

    // Try keyword-based forms first (whitespace-separated tokens).
    if let Some((field, rest)) = trimmed.split_once(char::is_whitespace) {
        let field = field.trim();
        let rest = rest.trim();
        if rest == "exists" {
            if field.is_empty() {
                return Err("expected non-empty field name before `exists`".to_string());
            }
            return Ok(WhenPredicate::Exists {
                field: field.to_string(),
            });
        }
        if rest == "not_exists" {
            if field.is_empty() {
                return Err("expected non-empty field name before `not_exists`".to_string());
            }
            return Ok(WhenPredicate::NotExists {
                field: field.to_string(),
            });
        }

        // `<field> in {v1,v2,...}` — strip the `in` keyword and parse braced values.
        if rest.starts_with("in ") || rest.starts_with("in\t") || rest == "in" {
            let braced = rest.strip_prefix("in").unwrap().trim();
            return parse_in_predicate(field, braced, raw);
        }
        if let Some(after) = rest.strip_prefix("in{") {
            let braced = format!("{{{after}");
            return parse_in_predicate(field, &braced, raw);
        }
    }

    // Fall through to `<field>=<value>` equality syntax.
    let parts: Vec<&str> = trimmed.splitn(3, '=').collect();
    if parts.len() != 2 {
        return Err(format!(
            "expected `<field>=<value>`, `<field> in {{...}}`, \
             `<field> exists`, or `<field> not_exists` (got {raw:?})"
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

/// Helper: parse the `{v1,v2,...}` portion of an `in` predicate.
fn parse_in_predicate(
    field: &str,
    rest: &str,
    raw: &str,
) -> std::result::Result<WhenPredicate, String> {
    if field.is_empty() {
        return Err("expected non-empty field name before `in`".to_string());
    }
    if !rest.starts_with('{') || !rest.ends_with('}') {
        return Err(format!(
            "expected `<field> in {{val1,val2,...}}` with curly braces (got {raw:?})"
        ));
    }
    let inner = &rest[1..rest.len() - 1];
    let values: Vec<String> = inner.split(',').map(|v| v.trim().to_string()).collect();
    if values.is_empty() || values.iter().all(|v| v.is_empty()) {
        return Err(format!(
            "expected at least one non-empty value inside braces (got {raw:?})"
        ));
    }
    for (i, v) in values.iter().enumerate() {
        if v.is_empty() {
            return Err(format!(
                "empty value at position {i} inside braces (got {raw:?})"
            ));
        }
    }
    Ok(WhenPredicate::In {
        field: field.to_string(),
        values,
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
        assert_eq!(
            p,
            WhenPredicate::Equals {
                field: "status".into(),
                value: "superseded".into()
            }
        );
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

    #[test]
    fn parse_when_accepts_in_syntax() {
        let p = parse_when("status in {active,archived}").unwrap();
        assert_eq!(
            p,
            WhenPredicate::In {
                field: "status".into(),
                values: vec!["active".into(), "archived".into()],
            }
        );
    }

    #[test]
    fn parse_when_accepts_in_with_whitespace() {
        let p = parse_when("status in { active , archived }").unwrap();
        assert_eq!(
            p,
            WhenPredicate::In {
                field: "status".into(),
                values: vec!["active".into(), "archived".into()],
            }
        );
    }

    #[test]
    fn parse_when_rejects_in_empty_values() {
        assert!(parse_when("status in {}").is_err());
    }

    #[test]
    fn parse_when_rejects_in_with_empty_element() {
        assert!(parse_when("status in {active,,archived}").is_err());
    }

    #[test]
    fn parse_when_accepts_exists() {
        let p = parse_when("owner exists").unwrap();
        assert_eq!(
            p,
            WhenPredicate::Exists {
                field: "owner".into(),
            }
        );
    }

    #[test]
    fn parse_when_accepts_not_exists() {
        let p = parse_when("reviewed not_exists").unwrap();
        assert_eq!(
            p,
            WhenPredicate::NotExists {
                field: "reviewed".into(),
            }
        );
    }

    #[test]
    fn parse_when_rejects_exists_empty_field() {
        assert!(parse_when(" exists").is_err());
    }

    fn override_with(kind: &str, mut ov: SchemaOverride) -> Config {
        ov.kinds = vec![kind.into()];
        let mut kinds = KindsConfig::default();
        if !kinds.allowed.iter().any(|k| k == kind) {
            kinds.allowed.push(kind.into());
        }
        Config {
            kinds,
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
        // check first. The override targets `adr`, which must also
        // be in `kinds.allowed` or `validate_kinds` would intercept
        // ahead of the enum check.
        let config = Config {
            kinds: KindsConfig {
                allowed: vec![
                    "generic".into(),
                    "guide".into(),
                    "readme".into(),
                    "adr".into(),
                ],
            },
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
            kinds: KindsConfig {
                allowed: vec![
                    "generic".into(),
                    "guide".into(),
                    "readme".into(),
                    "adr".into(),
                ],
            },
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
            kinds: KindsConfig {
                allowed: vec![
                    "generic".into(),
                    "guide".into(),
                    "readme".into(),
                    "adr".into(),
                ],
            },
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
            kinds: KindsConfig {
                allowed: vec![
                    "generic".into(),
                    "guide".into(),
                    "readme".into(),
                    "adr".into(),
                ],
            },
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
            kinds: KindsConfig {
                allowed: vec![
                    "generic".into(),
                    "guide".into(),
                    "readme".into(),
                    "adr".into(),
                ],
            },
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
            kinds: KindsConfig {
                allowed: vec![
                    "generic".into(),
                    "guide".into(),
                    "readme".into(),
                    "adr".into(),
                ],
            },
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
    fn validate_rejects_schema_override_kinds_not_in_allowed() {
        // A typo in `schema.overrides[].kinds` would silently match no
        // document and the override would never fire — the "no silent
        // runtime skips" failure mode. Mirror the kinds-validation
        // every other rule family already runs.
        let config = Config {
            schema: SchemaConfig {
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into()], // not in default kinds.allowed
                    required: vec!["owner".into()],
                    types: BTreeMap::new(),
                    enums: BTreeMap::new(),
                    cross_field: vec![],
                }],
                ..Default::default()
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("schema.overrides[0]"), "{msg}");
                assert!(msg.contains("\"adr\""), "{msg}");
                assert!(msg.contains("kinds.allowed"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_schema_override_kinds_in_allowed() {
        // Positive complement of the rejection test above. Confirms a
        // well-formed override with `kinds` entirely in `kinds.allowed`
        // loads cleanly — guards against an overzealous validator that
        // rejects valid inputs.
        let config = Config {
            kinds: KindsConfig {
                allowed: vec![
                    "generic".into(),
                    "guide".into(),
                    "readme".into(),
                    "adr".into(),
                ],
            },
            schema: SchemaConfig {
                overrides: vec![SchemaOverride {
                    kinds: vec!["adr".into()],
                    required: vec!["owner".into()],
                    types: BTreeMap::new(),
                    enums: BTreeMap::new(),
                    cross_field: vec![],
                }],
                ..Default::default()
            },
            ..Config::default()
        };
        config
            .validate()
            .expect("override with kinds in allowed must load");
    }

    #[test]
    fn validate_rejects_terminal_status_not_in_allowed() {
        // A `statuses.terminal` entry that isn't in `statuses.allowed`
        // is two self-consistency violations at once: any node landing
        // on that status would fail FieldEnumRule, and `lifecycle`
        // transitions targeting it would never terminate. Refuse at
        // load.
        let config = Config {
            statuses: StatusesConfig {
                allowed: vec![
                    "active".into(),
                    "superseded".into(),
                    "archived".into(),
                    "deprecated".into(),
                    "abandoned".into(),
                ],
                terminal: vec!["frozen".into()], // not in allowed
            },
            ..Config::default()
        };
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("statuses.terminal"), "{msg}");
                assert!(msg.contains("\"frozen\""), "{msg}");
                assert!(msg.contains("statuses.allowed"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_terminal_subset_of_allowed() {
        // Positive complement. Default config — which carries the
        // canonical lifecycle terminal statuses (`superseded`,
        // `archived`, `deprecated`, `abandoned`) all of which are in
        // `statuses.allowed` — must continue to load. Without this
        // test a regression that swung the subset check to a strict
        // equality could pass silently.
        Config::default()
            .validate()
            .expect("default config's terminal must be a subset of allowed");
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

                    kinds: vec![],
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

                kinds: vec![],
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
    fn parse_when_error_mentions_quoting_unsupported() {
        let err = parse_when("status==foo").unwrap_err();
        assert!(
            err.contains("expected") && err.contains("got"),
            "error should mention the unexpected input: {err}"
        );
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

            kinds: vec![],
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

                kinds: vec![],
            },
            AnnotationConfig {
                name: "x".into(),
                pattern: r"(?P<j>\w+)".into(),
                key: "j".into(),

                kinds: vec![],
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

            kinds: vec![],
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

            kinds: vec![],
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
            enums,

            kinds: vec![],
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
    fn validate_rejects_body_line_unknown_kind() {
        let mut block = well_formed_body_line();
        block.kinds = vec!["spec".into()];
        // Default kinds.allowed has no "spec" — Config::default has only generic/guide/readme.
        let err = body_line_config(vec![block]).validate().unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("not in kinds.allowed"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_body_line_kinds_in_allowed() {
        // Positive complement of `validate_rejects_body_line_unknown_kind`:
        // a block whose `kinds` list is fully covered by `kinds.allowed`
        // must load cleanly. Without this, a regression that
        // accidentally tightened the validator (e.g. requiring kinds
        // be non-empty) could pass with only the negative test green.
        let mut block = well_formed_body_line();
        block.kinds = vec!["guide".into()]; // "guide" is in default kinds.allowed
        body_line_config(vec![block])
            .validate()
            .expect("body_line block with kinds in allowed must load");
    }

    // ─── Annotations validation ────────────────────────────────────────

    #[test]
    fn validate_rejects_annotation_unknown_kind() {
        let err = annotations_config(vec![AnnotationConfig {
            name: "promotes".into(),
            pattern: r"(?P<id>\w+)".into(),
            key: "id".into(),
            kinds: vec!["learnng".into()],
        }])
        .validate()
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("not in kinds.allowed"), "{msg}"),
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_annotations_kinds_in_allowed() {
        // Positive complement of `validate_rejects_annotation_unknown_kind`.
        // The existing `validate_accepts_well_formed_annotation_pattern`
        // uses `kinds: vec![]` (no restriction), so the *populated*
        // positive path was previously untested.
        annotations_config(vec![AnnotationConfig {
            name: "promotes".into(),
            pattern: r"(?P<id>[\w-]+)".into(),
            key: "id".into(),
            kinds: vec!["guide".into()],
        }])
        .validate()
        .expect("annotation with kinds in allowed must load");
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

    // ─── [[rules.body_immutable]] validation ───────────────────────────

    fn body_immutable_block(name: &str) -> crate::config::BodyImmutableRuleConfig {
        crate::config::BodyImmutableRuleConfig {
            name: name.into(),
            mode: crate::config::BodyImmutableMode::Frozen,

            kinds: vec![],
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
        // A typo in `kinds` would silently match zero
        // documents forever. Same "no silent runtime skips"
        // discipline body_line / annotations apply.
        let mut c = Config::default();
        let mut block = body_immutable_block("policy");
        block.kinds = vec!["adrr".into()]; // typo
        c.rules.body_immutable = vec![block];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("not in kinds.allowed"), "{msg}");
            }
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

            kinds: vec![],
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
    fn validate_accepts_frontmatter_immutable_kind_scoped() {
        let mut c = Config::default();
        c.kinds.allowed.push("adr".into());
        let mut block = frontmatter_immutable_block("lock", vec!["id"]);
        block.kinds = vec!["adr".into()];
        c.rules.frontmatter_immutable = vec![block];
        c.validate().expect("well-formed kind filter must load");
    }

    #[test]
    fn validate_rejects_frontmatter_immutable_kinds_not_in_allowed() {
        // Mirror of `validate_rejects_body_line_unknown_kind` and
        // `validate_rejects_annotation_unknown_kind` — the same
        // typo-silently-matches-nothing failure mode also lives on the
        // frontmatter_immutable surface, but was previously only
        // exercised via the positive path. Negative test anchors the
        // symmetric-guards discipline (`.claude/rules/config-driven.md`).
        let mut c = Config::default();
        // Do *not* add "adr" to kinds.allowed — that's the bug.
        let mut block = frontmatter_immutable_block("lock", vec!["id"]);
        block.kinds = vec!["adr".into()];
        c.rules.frontmatter_immutable = vec![block];
        let err = c.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("not in kinds.allowed"), "{msg}");
                assert!(msg.contains("frontmatter_immutable"), "{msg}");
                assert!(msg.contains("\"adr\""), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_body_immutable_block_with_allowed_kind() {
        let mut c = Config::default();
        c.kinds.allowed.push("adr".into());
        let mut block = body_immutable_block("policy");
        block.kinds = vec!["adr".into()];
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

    #[test]
    fn validate_accepts_trust_overrides_with_valid_kinds() {
        let mut config = Config::default();
        config.kinds.allowed.push("adr".into());
        config.trust.overrides = vec![TrustWeightOverride {
            kinds: vec!["adr".into()],
            weights: TrustWeights {
                status: 0.2,
                freshness: 0.2,
                drift: 0.2,
                backlinks: 0.4,
            },
        }];
        config.validate().expect("valid trust override must load");
    }

    #[test]
    fn validate_rejects_trust_override_with_unknown_kind() {
        let mut config = Config::default();
        config.trust.overrides = vec![TrustWeightOverride {
            kinds: vec!["bogus".into()],
            weights: TrustWeights::default(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("trust.overrides[0]"), "{msg}");
                assert!(msg.contains("\"bogus\""), "{msg}");
                assert!(msg.contains("kinds.allowed"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_trust_override_duplicate_kind() {
        let mut config = Config::default();
        config.kinds.allowed.push("adr".into());
        config.trust.overrides = vec![
            TrustWeightOverride {
                kinds: vec!["adr".into()],
                weights: TrustWeights::default(),
            },
            TrustWeightOverride {
                kinds: vec!["adr".into()],
                weights: TrustWeights {
                    status: 0.1,
                    freshness: 0.1,
                    drift: 0.1,
                    backlinks: 0.7,
                },
            },
        ];
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
    fn validate_rejects_trust_override_negative_weight() {
        let mut config = Config::default();
        config.kinds.allowed.push("adr".into());
        config.trust.overrides = vec![TrustWeightOverride {
            kinds: vec!["adr".into()],
            weights: TrustWeights {
                status: -0.1,
                freshness: 0.3,
                drift: 0.2,
                backlinks: 0.1,
            },
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("trust.overrides[0]"), "{msg}");
                assert!(msg.contains("status"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_trust_override_all_zero_weights() {
        let mut config = Config::default();
        config.kinds.allowed.push("adr".into());
        config.trust.overrides = vec![TrustWeightOverride {
            kinds: vec!["adr".into()],
            weights: TrustWeights {
                status: 0.0,
                freshness: 0.0,
                drift: 0.0,
                backlinks: 0.0,
            },
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("trust.overrides[0]"), "{msg}");
                assert!(msg.contains("at least one positive"), "{msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn trust_weights_for_returns_override_when_matched() {
        let mut config = Config::default();
        config.kinds.allowed.push("adr".into());
        let override_weights = TrustWeights {
            status: 0.1,
            freshness: 0.1,
            drift: 0.1,
            backlinks: 0.7,
        };
        config.trust.overrides = vec![TrustWeightOverride {
            kinds: vec!["adr".into()],
            weights: override_weights,
        }];
        let resolved = config.trust_weights_for("adr");
        assert_eq!(resolved.backlinks, 0.7);
        assert_eq!(resolved.status, 0.1);
        // Unmatched kind falls back to global.
        let fallback = config.trust_weights_for("generic");
        assert_eq!(fallback.status, config.trust.weights.status);
        assert_eq!(fallback.backlinks, config.trust.weights.backlinks);
    }

    // ─── Phase 3: silent-no-op invariants ──────────────────────────────
    //
    // Each pair (reject + accept) guards one runtime contract that would
    // otherwise let a config load cleanly and produce zero observable
    // effect. The validator's job is to refuse precisely the inputs the
    // runtime would silently drop.

    #[test]
    fn validate_rejects_link_pattern_without_capture_group() {
        // `parser::body` extracts edge targets from `caps.get(1)`. A
        // pattern without a `(...)` group silently emits nothing — the
        // user thinks they declared a custom link, the graph has zero
        // edges for it.
        let mut config = Config::default();
        config.parser.link_patterns = vec![LinkPattern {
            pattern: r"@import\s+\S+".into(),
            relation: "imports".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("parser.link_patterns[0]"), "msg: {msg}");
                assert!(msg.contains("no capture group"), "msg: {msg}");
                assert!(msg.contains("(...)"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_link_pattern_with_capture_group() {
        // The same shape `parser::body` already consumes — at least
        // one explicit capture group so `caps.get(1)` resolves.
        let mut config = Config::default();
        config.parser.link_patterns = vec![LinkPattern {
            pattern: r"@import\s+(\S+)".into(),
            relation: "imports".into(),
        }];
        config
            .validate()
            .expect("link_pattern with one capture group must validate");
    }

    #[test]
    fn validate_rejects_link_pattern_with_multiple_capture_groups() {
        // Multiple capture groups would cause confusion: only the first
        // is used, so having more is a silent misbehavior. Reject explicitly.
        let mut config = Config::default();
        config.parser.link_patterns = vec![LinkPattern {
            pattern: r"@import\s+(\S+)\s+from\s+(\S+)".into(),
            relation: "imports".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(
                    msg.contains("parser.link_patterns[0].pattern"),
                    "msg: {msg}"
                );
                assert!(msg.contains("multiple capture groups"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_unknown_conditional_exclude_condition() {
        // `builder::scanner::apply_conditional_excludes` only honours
        // `status_terminal`. A misspelling like `status_terminated`
        // would load cleanly and exclude nothing — a silent no-op
        // rule. Refuse at load with the valid set in the message.
        let mut config = Config::default();
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/*/spec.md".into(),
            condition: "status_terminated".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("scope.conditional_exclude[0]"), "msg: {msg}");
                assert!(msg.contains("\"status_terminated\""), "msg: {msg}");
                assert!(msg.contains("status_terminal"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_status_terminal_conditional_exclude() {
        let mut config = Config::default();
        config.scope.conditional_exclude = vec![ConditionalExclude {
            parent_glob: "specs/*/spec.md".into(),
            condition: "status_terminal".into(),
        }];
        config
            .validate()
            .expect("status_terminal condition must validate");
    }

    #[test]
    fn validate_rejects_id_rule_kind_not_in_allowed() {
        // `parser::identity::infer_kind` skips id_rules whose `kind`
        // is neither `*` nor the inferred kind. A typo like `"guidde"`
        // would load cleanly and silently never apply. Refuse at load.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "guidde".into(),
            glob: None,
            template: "guide-{stem}".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("identity.id_rules[0].kind"), "msg: {msg}");
                assert!(msg.contains("\"guidde\""), "msg: {msg}");
                assert!(msg.contains("kinds.allowed"), "msg: {msg}");
                assert!(msg.contains("\"*\""), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_wildcard_kind_in_id_rule() {
        // The any-kind escape hatch documented in `parser::identity`.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "{kind}-{stem}".into(),
        }];
        config
            .validate()
            .expect("wildcard kind must validate without being in kinds.allowed");
    }

    #[test]
    fn validate_accepts_id_rule_kind_in_kinds_allowed() {
        // The companion to the wildcard case: an explicit kind that
        // *is* in `kinds.allowed` must load. `"guide"` is one of the
        // default kinds shipped by `default_kinds()`.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "guide".into(),
            glob: None,
            template: "guide-{stem}".into(),
        }];
        config
            .validate()
            .expect("id_rule with kind in kinds.allowed must validate");
    }

    #[test]
    fn validate_rejects_unknown_id_template_placeholder() {
        // `parser::identity::expand_template` only knows about
        // `{kind}`, `{stem}`, `{parent}`, `{path_slug}`. A typo like
        // `{stme}` would otherwise load cleanly and produce a literal
        // `{stme}` substring in every generated id — surfacing the
        // typo at load instead is the symmetric guard.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "{kind}-{stme}".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
                assert!(msg.contains("\"stme\""), "msg: {msg}");
                assert!(msg.contains("{kind}"), "msg: {msg}");
                assert!(msg.contains("{path_slug}"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_every_known_id_template_placeholder() {
        // Positive companion to the typo case: every name listed in
        // `ID_TEMPLATE_PLACEHOLDERS` must validate. If a future patch
        // adds a new placeholder to the substitution arms without
        // extending the constant, this test still passes — but its
        // *companion* must be added here too, locking the closed set
        // in sync with the substitution.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "{kind}-{stem}-{parent}-{path_slug}".into(),
        }];
        config
            .validate()
            .expect("template referencing every known placeholder must validate");
    }

    #[test]
    fn validate_accepts_id_template_without_any_placeholder() {
        // A literal-only template ("readme-root") is a legitimate
        // use case for path-pinned rules. The placeholder scan must
        // not require at least one `{ident}`.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "readme-root".into(),
        }];
        config
            .validate()
            .expect("literal-only template must validate");
    }

    #[test]
    fn validate_accepts_id_template_with_repeated_placeholder() {
        // `{stem}-{stem}` is well-formed: the placeholder regex matches
        // it twice, both names are in `ID_TEMPLATE_PLACEHOLDERS`, and
        // no brace is left over after stripping. The malformed-brace
        // scan must not false-positive on legitimate repetition.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "{stem}-{stem}".into(),
        }];
        config
            .validate()
            .expect("repeated well-formed placeholder must validate");
    }

    #[test]
    fn validate_rejects_id_template_with_whitespace_in_braces() {
        // `{ kind }` is not a well-formed placeholder — the regex skips
        // it, the substitution arm in `expand_template` skips it, and
        // the runtime would emit the literal `{ kind }` substring in
        // every generated id. Reject at load with a clear error.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "{ kind }-{stem}".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
                assert!(msg.contains("malformed brace"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_id_template_with_unclosed_brace() {
        // `{kind-{stem}` leaves a stray `{` after stripping the
        // well-formed `{stem}` — the runtime would emit `{kind-` into
        // every generated id. Reject at load.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "{kind-{stem}".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
                assert!(msg.contains("malformed brace"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_id_template_with_unopened_brace() {
        // `kind}-{stem}` leaves a stray `}` after stripping the
        // well-formed `{stem}` — the runtime would emit `kind}-` into
        // every generated id. Reject at load.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "kind}-{stem}".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
                assert!(msg.contains("malformed brace"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_id_template_with_double_braces() {
        // We don't support `{{kind}}` as a literal-brace escape. The
        // inner `{kind}` is well-formed and gets stripped; the outer
        // `{` and `}` are left over and the runtime would emit them
        // literal. Reject at load — keep the substitution model
        // simple, and surface the ambiguity at config load time.
        let mut config = Config::default();
        config.identity.id_rules = vec![IdRule {
            kind: "*".into(),
            glob: None,
            template: "{{kind}}-{stem}".into(),
        }];
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("identity.id_rules[0].template"), "msg: {msg}");
                assert!(msg.contains("malformed brace"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_zero_stale_days() {
        let mut config = Config::default();
        config.detection.stale_days = Some(0);
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("stale_days"), "msg: {msg}");
                assert!(msg.contains("must be > 0 or None"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_rejects_zero_git_drift_threshold() {
        let mut config = Config::default();
        config.detection.git_drift_threshold = Some(0);
        let err = config.validate().unwrap_err();
        match err {
            Error::Config(msg) => {
                assert!(msg.contains("git_drift_threshold"), "msg: {msg}");
                assert!(msg.contains("must be > 0 or None"), "msg: {msg}");
            }
            _ => panic!("expected Config error"),
        }
    }

    #[test]
    fn validate_accepts_none_stale_days() {
        let mut config = Config::default();
        config.detection.stale_days = None;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_accepts_positive_stale_days() {
        let mut config = Config::default();
        config.detection.stale_days = Some(180);
        assert!(config.validate().is_ok());
    }
}
