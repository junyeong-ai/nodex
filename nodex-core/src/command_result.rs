//! Typed shapes of every CLI command's `data` payload.
//!
//! Living in the core (rather than each `nodex-cli/src/commands/*.rs`)
//! gives [`export::export_envelope_schema`] a single source of truth to
//! `schemars::schema_for!` against — no hand-written JSON Schemas to
//! drift from reality. Every CLI handler that emits one of these
//! shapes constructs the canonical type and serialises it; the
//! envelope-schema manifest re-derives the schema from the same type.
//!
//! Naming follows the project-wide convention (see
//! `nodex-core/CLAUDE.md`): mutation outcomes end with `*Result`.
//! Each command exposes its own concrete `*Result` (`LifecycleResult`,
//! `MigrateResult`, `RenameResult`, `InitResult`, `ReportResult`,
//! `BuildResult`, `CheckResult`); the
//! generic CLI envelope (`{ok, data, warnings | error}`) is the
//! separate `format::Envelope<T>` wrapper that holds whichever
//! `*Result` the command produced.
//!
//! Adding a new mutation command:
//!
//!   1. Define its `*Result` type here with `Serialize + Deserialize +
//!      JsonSchema`.
//!   2. Construct + emit the typed value from the command handler.
//!   3. Register `schema_of::<TheResult>()` in
//!      [`crate::export::per_command_schemas`] so consumers can
//!      codegen against it.
//!
//! Per the project's self-consistency invariant: anything nodex itself
//! writes (including the response shapes downstream codegen depends
//! on) must be derivable from the same canonical structure that
//! produces it.
//!
//! [`export::export_envelope_schema`]: crate::export::export_envelope_schema

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `lifecycle <action> <id>` result. Carries the node identifier the
/// action targeted, the action name (so consumers don't have to
/// re-parse the original command line), and the relative path that
/// was rewritten.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LifecycleResult {
    pub node_id: String,
    pub action: String,
    pub path: String,
}

/// One planned (or applied) frontmatter injection inside `migrate`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MigrationChange {
    pub path: String,
    pub id: String,
    pub kind: String,
}

/// `migrate [--apply]` result. `applied = false` means the planned
/// changes were not written (default dry-run mode).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MigrateResult {
    pub changes: Vec<MigrationChange>,
    pub total: usize,
    pub applied: bool,
}

/// What `rename` did to the renamed file's frontmatter, if anything,
/// to keep the node's id stable across the move. Always surfaced in
/// the envelope so a caller can verify the id-stability contract held
/// — never silent.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IdStability {
    /// The doc already declared `id:` explicitly. Move is path-only.
    AlreadyAnchored,
    /// Path-derived id would not have changed. Nothing to anchor.
    Unchanged,
    /// Path-derived id *would* have changed; the previous effective id
    /// was anchored into the doc's frontmatter so cross-references
    /// from other docs (`related`, `supersedes`, `implements`,
    /// `superseded_by`) remain valid.
    Anchored { id: String },
    /// The doc has no frontmatter at all (a bare markdown file). The
    /// runtime infers an id from the path, which the rename has
    /// changed; the caller must either add frontmatter to the moved
    /// file or audit cross-references manually. Surfaced as a warning,
    /// never silently skipped.
    BareNoFrontmatter { warning: String },
}

/// `rename <old> <new>` result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RenameResult {
    pub old_path: String,
    pub new_path: String,
    pub references_updated: Vec<String>,
    pub total_updated: usize,
    pub id_stability: IdStability,
}

/// `retarget <old-id> <new-id>` result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RetargetResult {
    pub old_id: String,
    pub new_id: String,
    pub references_updated: Vec<String>,
    pub total_updated: usize,
}

/// `init` result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InitResult {
    pub path: String,
}

/// `report` result. Lists the artefacts the run produced (e.g.
/// `["graph.json", "GRAPH.md"]`) and the directory they were written
/// into.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportResult {
    pub generated: Vec<String>,
    pub output_dir: String,
}

/// `build [--full]` envelope. Full superset of
/// [`crate::builder::BuildStats`] plus the CLI-measured `duration_ms`
/// — the typed struct the envelope-schema entry for `build` derives
/// from, so the schema can't drift from what the command emits.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BuildResult {
    pub nodes: usize,
    pub edges: usize,
    pub annotations: usize,
    pub body_line_matches: usize,
    /// Number of files served from the per-content-hash cache
    /// instead of re-parsed. `cached + parsed == nodes` for any
    /// in-scope file set with no read errors.
    pub cached: usize,
    pub parsed: usize,
    pub duration_ms: u64,
    /// Project-root-relative paths a `conditional_exclude` rule dropped
    /// from scope. Empty (and omitted from JSON) when no rule fired, so
    /// the exclusion is auditable rather than a silent disappearance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditionally_excluded: Vec<String>,
}

/// `check [--severity --since]` result. The CLI envelope is richer
/// than the core [`crate::rules::CheckReport`] because it also exposes
/// the post-filter violation count and the typed pass/fail flag the
/// runner uses to decide the exit code (1 vs 0). Keeping it a
/// dedicated `*Result` type means the envelope-schema entry for
/// `check` is derived from this struct — no hand-rolled JSON shape
/// can drift from what the command actually emits.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckResult {
    pub violations: Vec<crate::rules::Violation>,
    /// Rules the runner declined to evaluate, with their reasons.
    /// Surfaced alongside violations so a consumer never confuses
    /// "rule passed" with "rule never ran" — the same "no silent
    /// skips" discipline `query issues` follows.
    pub skipped_rules: Vec<crate::rules::SkippedRule>,
    /// Number of violations after `--severity` filtering. Cheap
    /// pre-computed counter for consumers building UI summaries
    /// without iterating the `violations` array.
    pub total: usize,
    /// `true` when any surviving violation has severity `Error` —
    /// drives the CLI exit-code-1 contract.
    pub has_errors: bool,
}
