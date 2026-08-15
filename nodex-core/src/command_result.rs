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
//!      `export::per_command_schemas` so consumers can
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
#[serde(rename_all = "snake_case", tag = "type")]
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
    /// Project-root-relative paths a `conditional_exclude` rule matched as a
    /// derivative and spared, because the same rule also read them as one of
    /// its terminal parents. `conditionally_excluded` says what a rule
    /// removed; without this nothing says what it kept back, and the kept one
    /// is the surprising half — a directory where one record's sub-artifact
    /// stays while its live neighbours' leave is a `parent_glob` that reaches
    /// the derivatives. Empty (and omitted) when no rule spared anything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditionally_kept: Vec<String>,
    /// Paths the walk reached and could not read as a file or descend as a
    /// directory — a symlink whose target is absent, or a socket / FIFO /
    /// device node. What such an entry would have held is unknowable, so this
    /// is every one the walk did not keep away from rather than only those
    /// matching the document globs. The build yielded nothing there, so no
    /// rule judged anything; reported for the same reason
    /// `conditionally_excluded` is, and empty (omitted) when every entry the
    /// walk reached could be read or descended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dangling_paths: Vec<String>,
    /// In-scope paths a ref build dropped because they resolve outside the
    /// checkout. Always empty for `nodex build`, which graphs the working
    /// tree and follows a symlink wherever it leads; present so a consumer
    /// reading a ref build sees the same accounting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escaping_paths: Vec<String>,
    /// Directory symlinks the walk did not descend, because
    /// `scope.follow_symlinks` is off. Nothing below them is graphed, so the
    /// boundary is stated instead of left to be discovered — reported for the
    /// same reason `conditionally_excluded` is. Like `dangling_paths`, this is
    /// every link the walk reached rather than only those that could have held
    /// an in-scope document: what lies below one is unknowable without
    /// descending it, which is the cost the policy exists to avoid, so a link
    /// under an `exclude`d tree is listed too. What the walk *does* keep away
    /// from — a `prune_dirs` basename, a hidden segment — is absent here,
    /// because that boundary is the project's own and holds for a real
    /// directory just the same.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unfollowed_paths: Vec<String>,
    /// Names the scan holds a document under but does not use, each paired
    /// with the one it does. Only `scope.follow_symlinks` produces these.
    /// Nothing is lost — the document is graphed under the name in use — so
    /// this is not a decline; it is reported because a path the operator can
    /// read that the graph does not carry needs an explanation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliased_paths: Vec<crate::builder::AliasedPath>,
    /// In-scope documents that failed to parse and have no node —
    /// mirrored from [`crate::model::Graph::parse_failures`] so the
    /// build reports every drop structurally; `check` turns the same
    /// records into Error-severity `parse_failure` violations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parse_failures: Vec<crate::model::ParseFailure>,
}

/// One proposed document's verdict in a `check --content` run. Each
/// `--content PATH=SOURCE` pair yields one entry, in invocation order,
/// so a clean or out-of-scope proposal is reported as checked rather
/// than vanishing into an empty violation list (no silent green). The
/// proposal's introduced violations are not re-nested here — they live
/// once in [`CheckResult::violations`], each carrying its `path`, so a
/// consumer groups by `path` without the data being duplicated. An
/// items-list element nested in [`CheckResult::proposals`], hence the
/// `*Entry` name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProposalEntry {
    /// Normalized, forward-slash document path the proposed bytes target.
    pub path: String,
    /// Whether the path is in scan scope. A `false` here explains a
    /// clean verdict that validated nothing (nodex governs no document
    /// there) — the same fact the out-of-scope warning surfaces.
    pub in_scope: bool,
    /// `true` when an introduced Error-severity violation is *attributed to
    /// this proposal's own path* (a violation whose `path` equals this
    /// one). The name says the scope: a finding keyed to somebody else's
    /// path flips no entry's flag here — a `unique_numbering` collision is
    /// keyed to the first colliding path, which may be a pre-existing doc
    /// rather than this proposal. A `cycle` finding names the document it
    /// caught, so it flips that document's entry and only that one. Read from
    /// the judged set for the reason [`CheckResult::has_errors`] is, so under
    /// `--severity` this can be `true` while [`CheckResult::violations`]
    /// shows nothing for the path: the list is what is displayed, this is
    /// what the gate decided. The run-wide verdict is `has_errors`.
    pub has_path_errors: bool,
}

/// `check [--severity --since | --content]` result. The CLI envelope is
/// richer than the core [`crate::rules::CheckReport`] because it also
/// exposes the post-filter violation count and the typed pass/fail flag
/// the runner uses to decide the exit code (1 vs 0). Keeping it a
/// dedicated `*Result` type means the envelope-schema entry for `check`
/// is derived from this struct — no hand-rolled JSON shape can drift
/// from what the command actually emits.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckResult {
    pub violations: Vec<crate::rules::Violation>,
    /// Rules the runner declined to evaluate, with their reasons.
    /// Surfaced alongside violations so a consumer never confuses
    /// "rule passed" with "rule never ran" — the same "no silent
    /// skips" discipline `query issues` follows.
    pub skipped_rules: Vec<crate::rules::SkippedRule>,
    /// What each evaluated rule had to run over. With `skipped_rules`
    /// this is the whole registry: a rule either declined or ran, and a
    /// rule that ran over nothing passed for a reason `violations` does
    /// not record. A declared rule reporting zero subjects governs
    /// nothing — the config says otherwise, and only this says so.
    pub rule_coverage: Vec<crate::rules::RuleCoverage>,
    /// Number of violations after `--severity` filtering. Cheap
    /// pre-computed counter for consumers building UI summaries
    /// without iterating the `violations` array.
    pub total: usize,
    /// `true` when any violation the rules judged has severity `Error` —
    /// drives the CLI exit-code-1 contract. Read from the judged set, not
    /// from `violations`: `--severity` narrows what is listed, and a verdict
    /// that followed the listing would report a project holding errors as
    /// green to whoever filtered them out of view. A finding the response
    /// stops reporting at all ([`Self::reported_beside_the_list`]) is
    /// disclosed as `gate_suppression`, so `has_errors` disagreeing with the
    /// list on screen always has a stated reason — either that warning, or
    /// `standing` still holding the finding the list dropped.
    pub has_errors: bool,
    /// Per-proposal verdicts, present only for `check --content` (one
    /// entry per `PATH=SOURCE` pair, in invocation order). `None` for
    /// project-wide and `--since` checks, which have no proposals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposals: Option<Vec<ProposalEntry>>,
    /// Warning-severity violations the proposed nodes carry in the
    /// proposed state (`check --content` only) — the absolute view,
    /// and therefore a superset of `violations`' warning entries on
    /// the proposal paths: a warning the proposal itself introduces
    /// appears in both lists. `violations` is the introduced delta
    /// (what the proposal adds vs the working tree — the gating set),
    /// so a node's pre-existing housekeeping warnings (`stale_review`,
    /// `git_drift`) cancel out of it; an advisory consumer surfacing
    /// "what does this doc carry right now" reads the absolute answer
    /// here without a second project-wide check. `None` outside
    /// content mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standing: Option<Vec<crate::rules::Violation>>,
}

impl CheckResult {
    /// Whether this response reports `violation` somewhere other than
    /// [`Self::violations`]. The question a display filter has to answer
    /// about a finding it took out of that list: one the response still
    /// reports elsewhere was never hidden, and the code that says otherwise
    /// is the only one a consumer has for "there is a finding you cannot
    /// see".
    ///
    /// `violations` is excluded because the filter's own predicate put it
    /// there — a finding it removed cannot be in the list it removed the
    /// finding from, and scanning for it anyway costs the product of the two
    /// sets. Every other field is destructured, so one that comes to carry
    /// findings is a compile error here instead of a silent hole in the
    /// disclosure.
    pub fn reported_beside_the_list(&self, violation: &crate::rules::Violation) -> bool {
        let Self {
            standing,
            violations: _,
            total: _,
            skipped_rules: _,
            rule_coverage: _,
            has_errors: _,
            proposals: _,
        } = self;
        standing
            .as_ref()
            .is_some_and(|shown| shown.contains(violation))
    }
}
