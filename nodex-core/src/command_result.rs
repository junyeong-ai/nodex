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
//! `MigrateResult`, `RenameResult`, `InitResult`, `ReportResult`); the
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
