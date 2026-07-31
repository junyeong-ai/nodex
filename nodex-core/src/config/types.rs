//! Config data: the serde structs and enums, their defaults, the
//! vocabulary constants, and the id-template scanners. Pure data and
//! parsing-shape helpers — no `Config::validate` logic, no runtime views.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every frontmatter field that the parser recognises by name (and
/// therefore strips from the `attrs` catch-all). Single source of
/// truth — `Config::declared_fields_for` and the strict-mode rule both
/// read from here so a new built-in is acknowledged in exactly one
/// place. Mirrors the named fields of
/// `parser::frontmatter::RawFrontmatter`.
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
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Binary-compatibility pin (`meta.nodex_version`). The project
    /// declares which `nodex` binaries may *write* its documents:
    /// mutating commands refuse to run on a binary outside the pin
    /// (`load_project_for_mutation`), while read-only commands always run
    /// and merely attach a `binary_compat_warning` to their output. The
    /// pin string is validated as a SemVer requirement at load time.
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
    #[serde(default)]
    pub search: SearchConfig,
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
#[serde(deny_unknown_fields)]
pub struct MetaConfig {
    /// SemVer requirement (e.g. `">=0.8, <0.9"`) the running `nodex`
    /// binary must satisfy. `None` (the key omitted entirely) accepts
    /// any version — the recommended default during early development
    /// while the binary's API surface is still settling.
    #[serde(default)]
    pub nodex_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScopeConfig {
    // Field-level default, NOT a bare `#[serde(default)]`: a present
    // `[scope]` table that sets only `exclude` / `conditional_exclude`
    // must still scan markdown. A bare default resolves an absent
    // `include` to `Vec::default()` (`[]`, matching nothing) — the
    // container `Default` only fires when the whole `[scope]` table is
    // absent — which silently empties the graph and lets `check` pass on
    // an unscanned corpus.
    #[serde(default = "default_scope_include")]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub conditional_exclude: Vec<ConditionalExclude>,
    /// Directory basenames pruned during the walk at any depth, before
    /// include matching — dependency / build / VCS trees that descending
    /// only costs traversal time. Project-varying (a Go repo has no
    /// `.venv`; a docs vault may legitimately live under a directory
    /// named like one of these), so it is config, not a hardcoded list.
    /// The default preserves the historical set; pruning is a plain
    /// path-segment match at any depth (no globs). `.git` and other
    /// dot-prefixed trees are *also* skipped by the hidden-path guard,
    /// so removing one from this list does not expose it unless an
    /// `include` literally names the segment.
    #[serde(default = "default_prune_dirs")]
    pub prune_dirs: Vec<String>,
    /// Whether the walk descends a directory reached through a symlink.
    ///
    /// Off by default, as it is for `git`, `ripgrep`, `fd` and `find`. A
    /// followed directory link makes the project's path space a graph rather
    /// than a tree: one document becomes reachable under several names, and
    /// every rule that keys on a path — `include`, `exclude`, a
    /// `conditional_exclude` `parent_glob`, an `identity.kind_rules` glob — has
    /// to be read against a name chosen from among them. Traversal cost stops
    /// being bounded by the tree, too: nested links multiply the paths that
    /// reach one directory, so a link DAG costs a factor per level rather than
    /// per link — two links per level measure 0.05s at six levels and 1.4s at
    /// twelve, for one document. That is the price of naming every path, and
    /// `find -L` pays it the same way.
    ///
    /// Turn it on for a project whose documents genuinely live behind a link —
    /// a vendored tree linked into `docs/`, say. The scan then admits every
    /// name a document is reachable under, keeps one document per directory
    /// entry, and reports it under the smallest admitted name.
    ///
    /// A symlink to a *file* is unaffected: those are read wherever they point,
    /// which is the reader-follows half of the write discipline.
    #[serde(default)]
    pub follow_symlinks: bool,
}

fn default_scope_include() -> Vec<String> {
    vec!["**/*.md".to_string()]
}

fn default_prune_dirs() -> Vec<String> {
    ["node_modules", "__pycache__", "target", ".git", ".venv"]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            include: default_scope_include(),
            exclude: vec![],
            conditional_exclude: vec![],
            prune_dirs: default_prune_dirs(),
            follow_symlinks: false,
        }
    }
}

/// When a file matching `parent_glob` satisfies `condition` (today the
/// only supported condition is `status_terminal`), the sub-artifacts it
/// governs — files in the parent's directory subtree that match
/// `child_glob` — are dropped from scan scope. The parent itself always
/// stays in scope so it still parses into the graph.
///
/// `child_glob` is what makes the exclusion precise: only the paths the
/// project declares as derivative are dropped, so an independently-owned
/// document that merely happens to share the directory (a live decision
/// log beside a superseded spec) is never silently erased. To drop the
/// whole directory the project writes `child_glob = "**/*"` and owns
/// that choice explicitly. Every conditionally-excluded path is reported
/// on the build result, so the exclusion is auditable rather than silent.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionalExclude {
    pub parent_glob: String,
    pub child_glob: String,
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
pub(crate) fn id_template_placeholder_re() -> &'static regex::Regex {
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
pub(crate) fn scan_template_placeholders(template: &str) -> Vec<String> {
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
pub(crate) fn scan_template_malformed_braces(template: &str) -> bool {
    let stripped = id_template_placeholder_re().replace_all(template, "");
    stripped.contains('{') || stripped.contains('}')
}

fn default_condition() -> String {
    "status_terminal".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct StatusesConfig {
    #[serde(default = "default_statuses")]
    pub allowed: Vec<String>,
    #[serde(default = "default_terminal")]
    pub terminal: Vec<String>,
    /// Initial status for newly scaffolded documents. Must be in `allowed` list.
    /// If not specified, defaults to the first value in `allowed`.
    #[serde(default)]
    pub initial: Option<String>,
}

impl Default for StatusesConfig {
    fn default() -> Self {
        Self {
            allowed: default_statuses(),
            terminal: default_terminal(),
            initial: None,
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

pub(crate) fn default_terminal() -> Vec<String> {
    ["superseded", "archived", "deprecated", "abandoned"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaConfig {
    /// Fields every document must author. Empty by default: the parser
    /// already resolves id/title/kind/status/orphan_ok for every
    /// document (and the loader rejects listing them here), so the
    /// only meaningful entries are project-declared authored fields.
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub types: BTreeMap<String, FieldType>,
    #[serde(default)]
    pub enums: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub cross_field: Vec<CrossFieldSpec>,
    #[serde(default)]
    pub overrides: Vec<SchemaOverride>,
    /// Inferrable built-in fields (`id` / `title` / `kind` / `status`)
    /// a document must author *explicitly* rather than inherit from a
    /// fallback. These can never appear in `required` (the parser always
    /// resolves them, so a `required` entry could never fire) — this is
    /// the opt-in escape from that ergonomic default: a named field that
    /// falls back to inference reds `check` via the `explicit_field`
    /// rule, while the graph still gets its valid inferred value so the
    /// build never breaks. Empty by default. `orphan_ok` is rejected:
    /// a bool is structurally always present, so "authored vs omitted"
    /// is not a meaningful distinction for it.
    #[serde(default)]
    pub require_explicit: Vec<String>,
    /// Frontmatter strictness. `Lenient` (default) lets undeclared
    /// project-specific keys land in `attrs` untouched. `Strict` rejects
    /// any frontmatter key that is neither built-in nor declared in
    /// `types` / `enums` / `required` / `cross_field` (global + per-kind
    /// override) — this is the typo-catcher mode.
    #[serde(default)]
    pub mode: SchemaMode,
}

/// Frontmatter strictness mode. Drives `UnknownFieldRule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaMode {
    /// Undeclared keys are preserved silently in `Node::attrs`.
    #[default]
    Lenient,
    /// Undeclared keys produce a `unknown_field` violation per node.
    Strict,
}

/// Per-kind schema constraints.
///
/// Every field except `kinds` and `required` defaults to an empty
/// collection, and each corresponding rule short-circuits when empty.
/// Projects that never configure these keep today's behaviour verbatim.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaOverride {
    pub kinds: Vec<String>,
    /// Per-kind required fields, unioned with the global list. Optional
    /// like every other sub-block — an override that only narrows
    /// `enums` or adds a `cross_field` needs no `required` at all
    /// (omitted = adds nothing).
    #[serde(default)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
#[serde(deny_unknown_fields)]
pub struct CrossFieldSpec {
    pub when: String,
    pub require: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Default git ref that `nodex check` diffs against when `--since`
    /// is omitted, so the diff-aware immutability rules
    /// (`frontmatter_immutable`, `body_immutable`) are enforced by
    /// default instead of only when a ref is passed explicitly.
    /// `None` leaves them inert until `--since` is given. Unlike
    /// `--since`, the baseline never narrows the reported violations to
    /// changed nodes — it only supplies the before-state the
    /// immutability rules need.
    #[serde(default)]
    pub immutable_baseline: Option<String>,
    /// Relations whose resolved edge graph must stay acyclic — a cycle
    /// is a design defect (circular dependency) reported at Error
    /// severity. Validated against [`Config::known_relations`] at load;
    /// an empty list is rejected (it would silently fire nothing).
    /// `supersedes` is validated separately — and harder, as a
    /// build-time error — by `builder::validate_supersedes_dag`.
    /// Drives [`crate::rules::graph_invariants::CycleDetectionRule`].
    #[serde(default = "default_acyclic_relations")]
    pub acyclic_relations: Vec<String>,
}

/// Explicit `Default` (not derived): a derived impl would produce an
/// empty `acyclic_relations`, which `validate` rejects — the in-code
/// default must be the same one serde supplies for an omitted field.
impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            naming: Vec::new(),
            frontmatter_immutable: Vec::new(),
            body_immutable: Vec::new(),
            body_line: Vec::new(),
            immutable_baseline: None,
            acyclic_relations: default_acyclic_relations(),
        }
    }
}

pub(crate) fn default_acyclic_relations() -> Vec<String> {
    vec!["implements".to_string()]
}

/// One body-immutability policy. Multiple blocks let a project apply
/// different locking semantics to different kinds — ADRs `frozen`
/// (decisions are immutable in spirit), narratives `append_only`
/// (history grows but does not rewrite). When the lock engages is the
/// block's [`ImmutableTrigger`]: at terminal status (the default —
/// pre-terminal documents are still authoring drafts, and the single
/// write that first drives a doc terminal may finalise its body in the
/// same edit, mirroring `frontmatter_immutable`), or from creation
/// (the body freezes as soon as a prior committed snapshot exists,
/// regardless of status — the immutable-from-day-one contract for
/// ADR-style records).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyImmutableRuleConfig {
    /// Stable identifier used in the violation `rule_id`
    /// (`body_immutable/<name>`) and in the rule manifest. Must be
    /// unique across all `[[rules.body_immutable]]` blocks.
    pub name: String,
    /// Lock semantic for matching documents.
    pub mode: BodyImmutableMode,
    /// When the lock engages for matching documents.
    #[serde(default)]
    pub trigger: ImmutableTrigger,
    /// Which kinds this block locks. Empty = every kind. Every entry
    /// must be in `kinds.allowed`; `Config::load` enforces.
    #[serde(default)]
    pub kinds: Vec<String>,
}

/// When an immutability lock engages for a document.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImmutableTrigger {
    /// The lock engages once the document's *before-snapshot* status
    /// is terminal — the boundary `frontmatter_immutable` also reports
    /// on, so the write that first drives a doc terminal can finalise
    /// it in the same edit.
    #[default]
    Terminal,
    /// The lock engages as soon as a prior committed snapshot exists,
    /// regardless of status. The creating commit is structurally
    /// exempt: the diff layer only emits a body change for nodes
    /// present in *both* snapshots, so a document's first appearance
    /// can never fire the lock.
    Creation,
}

/// One frontmatter-immutability policy. Multiple blocks let a project
/// lock different field sets in different parts of the corpus.
/// Inert without `--since`. Symmetric with
/// [`BodyImmutableRuleConfig`]: each block carries a unique `name`,
/// a kind filter, and the per-block payload (`fields`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// `id` is refused too: it is structurally immutable (a changed id
    /// is a different node) and has no reliable diff signal, so a lock on
    /// it could only ever be a false positive. `status` is fine — it is
    /// enforced from the transition stream.
    pub fields: Vec<String>,
    /// Which kinds this block locks. Empty = every kind. Every entry
    /// must be in `kinds.allowed`; `Config::load` enforces.
    #[serde(default)]
    pub kinds: Vec<String>,
}

/// How a terminal document's body is locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct NamingRuleConfig {
    pub glob: String,
    pub pattern: String,
    #[serde(default)]
    pub sequential: bool,
    #[serde(default)]
    pub unique: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParserConfig {
    /// File extensions — including the leading `.` — that nodex
    /// treats as in-graph documents. Body-link extraction ignores
    /// references whose path does not end with one of these, and
    /// scaffold refuses to target a path with any other extension.
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,

    /// Enable Obsidian-style `[[wikilink]]` parsing in body text.
    /// When on, `[[X]]` (or `[[X|display]]`) outside code blocks emits a
    /// reference edge to X, resolved by the body-link ladder
    /// ([`crate::builder::resolver`]): the literal path and the same path
    /// relative to the source file, each also tried with a configured
    /// `parser.extensions` suffix appended, then a bare node id — so
    /// `[[guides/intro]]`, `[[intro]]`, and `[[adr-001]]` all resolve.
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    /// A plain `u32`, not `Option<u32>`: a grace period is a duration, so
    /// `0` is meaningful (no warmup window — orphan-check immediately).
    /// This differs deliberately from threshold settings like `stale_days`,
    /// where `0` would be ambiguous between "off" and "flag everything".
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
    /// Ordered classification of unresolved references — whether a
    /// given dangling link is a defect, a triage item, or
    /// expected-by-design varies by project, so the judgment lives
    /// here. Rows are evaluated in declared order and the first row
    /// whose `cause` equals the edge's cause and whose `glob` (when
    /// present) matches wins; an edge no row matches takes the
    /// built-in fallthrough, [`UnresolvedSeverity::Warning`].
    /// Declaring the table replaces this default entirely (the
    /// `rules.acyclic_relations` replacement discipline) — re-declare
    /// the `excluded_target` row to keep it. Evaluated at query/check
    /// time against the live filesystem; never part of the parse
    /// surface or the build cache key.
    #[serde(default = "default_unresolved_policy")]
    pub unresolved_policy: Vec<UnresolvedPolicyRuleConfig>,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            stale_days: default_stale_days(),
            orphan_grace_days: default_orphan_grace_days(),
            orphan_ok_kinds: Vec::new(),
            git_drift_threshold: None,
            git_drift_relations: default_git_drift_relations(),
            unresolved_policy: default_unresolved_policy(),
        }
    }
}

/// One row of the ordered `[[detection.unresolved_policy]]` table.
/// Rows are evaluated in order — first matching row wins (the
/// `identity.kind_rules` discipline).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnresolvedPolicyRuleConfig {
    /// Stable identifier. Keys the `summary.by_category` bucket (info
    /// rows), the violation `rule_id` (`unresolved_reference/<name>`,
    /// error rows), and the per-edge `policy_name` attribution. Must
    /// be unique across rows and must not collide with a reserved
    /// summary category.
    pub name: String,
    /// Typed cause this row matches
    /// ([`crate::model::UnresolvedCause`]). An unknown cause string is
    /// refused at deserialization.
    pub cause: crate::model::UnresolvedCause,
    /// Optional glob over the edge's *normalized root-relative
    /// resolution candidates* — the exact set the build resolver
    /// probes (extension ladder, root-relative and source-relative
    /// interpretations, escaping candidates dropped) — never the raw
    /// authored target, so `../docs/x.md` written from `designs/a.md`
    /// matches `docs/**`. Only legal on causes that carry such
    /// candidates ([`crate::model::UnresolvedCause::has_path_candidates`]);
    /// `Config::validate` rejects it elsewhere.
    #[serde(default)]
    pub glob: Option<String>,
    /// Which reporting plane matching edges land on.
    pub severity: UnresolvedSeverity,
}

/// Severity a `[[detection.unresolved_policy]]` row assigns to the
/// unresolved edges it classifies. Distinct from
/// [`crate::rules::Severity`]: only `Error` has a check-plane mapping;
/// `Warning` and `Info` are `query issues` report-plane levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedSeverity {
    /// Gate plane: the row registers one check rule
    /// `unresolved_reference/<name>` (Error severity), so matching
    /// edges fail `nodex check` at exit 1 and are counted by
    /// `query issues` as `violation_unresolved_reference/<name>`.
    Error,
    /// Triage plane: counted in `query issues` `summary.total` under
    /// `unresolved_edge`, never a check violation. Also the built-in
    /// fallthrough for edges no row matches.
    Warning,
    /// Informational plane: the edge is reported but kept out of
    /// `summary.total`, counted under `by_category[<row name>]`.
    Info,
}

/// The single default policy row: a link to a real on-disk file that
/// scope keeps out of the graph is informational, not broken. Every
/// other cause falls through to the counted `warning` plane.
pub(crate) fn default_unresolved_policy() -> Vec<UnresolvedPolicyRuleConfig> {
    vec![UnresolvedPolicyRuleConfig {
        name: crate::model::edge::categories::EXCLUDED_TARGET.to_string(),
        cause: crate::model::UnresolvedCause::ExcludedFromScope,
        glob: None,
        severity: UnresolvedSeverity::Info,
    }]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    /// Where nodex writes its own artefacts, project-relative.
    ///
    /// Authored config, so it arrives in nodex's path language and is folded
    /// into it once, here, as it is read. Every consumer — the traversal
    /// guard, the scan's self-exclusion glob, the join each writer performs,
    /// the path `status` reports — then reads one value, and the same
    /// `nodex.toml` is accepted or refused the same way on every platform.
    #[serde(default = "default_output_dir", deserialize_with = "forward_slashed")]
    pub dir: String,
}

fn forward_slashed<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(deserializer).map(|dir| crate::path_guard::forward_str(&dir))
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    #[serde(default = "default_trust_weights")]
    pub weights: TrustWeights,
    /// Per-kind weight overrides. Each entry *replaces* the global
    /// `weights` entirely for nodes whose kind is listed in the entry's
    /// `kinds` vec — a weight vector is only meaningful whole, so unlike
    /// `[[schema.overrides]]` (which merges field-by-field) there is no
    /// partial merge here. Lookup matches `[[schema.overrides]]`: first
    /// match wins, overlap rejected at load.
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
#[serde(deny_unknown_fields)]
pub struct TrustWeightOverride {
    pub kinds: Vec<String>,
    pub weights: TrustWeights,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

/// `[search]` — keyword ranking for `nodex query search`. The third
/// ranking surface alongside `[trust]` and `[similarity]`; like them it
/// exposes its weights so a project tunes relevance without a recompile.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    #[serde(default)]
    pub weights: SearchWeights,
}

/// Per-field keyword-match weights. Unlike `[trust]` / `[similarity]`,
/// which renormalise a composite over the whole corpus, search is an
/// *additive* ranking: a node's score is the sum of the weights of the
/// fields its keyword matched, and a node that matches nothing is
/// excluded. Each field has an exact and a partial (substring) tier so
/// the exact-vs-partial preference is itself config, not a hidden
/// constant. Tags match only by substring (a tag set has no single
/// "exact" notion), so there is one tag weight.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchWeights {
    pub id_exact: f64,
    pub id_partial: f64,
    pub title_exact: f64,
    pub title_partial: f64,
    pub tag: f64,
}

impl Default for SearchWeights {
    fn default() -> Self {
        Self {
            id_exact: 3.0,
            id_partial: 1.5,
            title_exact: 2.5,
            title_partial: 1.0,
            tag: 0.5,
        }
    }
}
