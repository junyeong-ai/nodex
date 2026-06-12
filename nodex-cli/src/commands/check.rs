use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nodex_core::check;
use nodex_core::rules::Severity;

use crate::format::emit_read_with;

use super::content_source::read_content_source;
use super::git_worktree::{BaselineResolution, ensure_work_tree};

/// Severity filter accepted by `nodex check --severity`.
#[derive(Clone, Copy, ValueEnum)]
pub enum CheckSeverity {
    Error,
    Warning,
}

impl From<CheckSeverity> for Severity {
    fn from(s: CheckSeverity) -> Self {
        match s {
            CheckSeverity::Error => Self::Error,
            CheckSeverity::Warning => Self::Warning,
        }
    }
}

/// Args for `nodex check`.
#[derive(Args)]
pub struct CheckArgs {
    /// Document whose proposed content is validated (with `--content`).
    #[arg(value_name = "PATH", requires = "content")]
    pub path: Option<PathBuf>,
    /// Validate the bytes from this source as the *future* content of
    /// `<PATH>` before they are written — `-` reads stdin, otherwise a
    /// file path resolved against the invoking directory (not `-C DIR`;
    /// the proposed bytes may legitimately live outside the project).
    /// The graph is built with the proposed content overlaid onto the
    /// working tree, so the same immutability / schema / cross-field
    /// rules gate the edit at its source instead of every agent
    /// reimplementing them. Mutually exclusive with `--since`.
    #[arg(
        long,
        value_name = "SOURCE",
        requires = "path",
        conflicts_with = "since"
    )]
    pub content: Option<String>,
    /// Filter by severity.
    #[arg(long, value_enum)]
    pub severity: Option<CheckSeverity>,
    /// Restrict violations to nodes that changed since the given git
    /// ref. Activates diff-aware rules (e.g. `frontmatter_immutable`).
    #[arg(long, value_name = "REF")]
    pub since: Option<String>,
}

pub fn run(root: &Path, args: CheckArgs, pretty: bool) -> Result<()> {
    let severity_filter = args.severity.map(Severity::from);
    let config = nodex_core::load_project(root)?;

    let target = resolve_target(root, &args, &config)?;

    let check_report = check(&target.graph, &config, root, target.diff.as_ref());

    // Scoping is per-mode. `--content` uses the before/after delta
    // (`rules::introduced_violations` — the count-aware multiset
    // difference shared with scaffold's gate): a violation also present
    // in the pre-overlay report is pre-existing and never refuses the
    // proposal; one the overlay introduces — whatever node it lands on,
    // including a node-less parse_failure for a proposal that destroys
    // its own node — does. `--since` keeps the pure set-membership
    // filter, where node-less violations (project-wide problems, e.g.
    // cycle detection) are *kept* so a narrowed scope never silently
    // drops a finding that can't be attributed to a specific id; the
    // "no silent skips" doctrine applies to violations as well as rules.
    let violations_filtered: Vec<_> = if let Some(before) = &target.baseline_violations {
        nodex_core::rules::introduced_violations(check_report.violations, before)
    } else {
        match &target.changed_ids {
            Some(ids) => check_report
                .violations
                .into_iter()
                .filter(|v| match &v.node_id {
                    Some(id) => ids.contains(id),
                    None => true,
                })
                .collect(),
            None => check_report.violations,
        }
    };

    // `--severity` is an exact-match display filter: `--severity warning`
    // shows only warnings and therefore drops every Error-severity
    // violation, taking `has_errors` (and the exit code) to 0 with it. An
    // operator who reaches for it in a gate would get a false green —
    // surface the suppression as a warning so it is never silent.
    let errors_hidden_by_filter = if severity_filter == Some(Severity::Warning) {
        violations_filtered
            .iter()
            .filter(|v| v.severity == Severity::Error)
            .count()
    } else {
        0
    };

    let violations_final: Vec<_> = match severity_filter {
        Some(target) => violations_filtered
            .into_iter()
            .filter(|v| v.severity == target)
            .collect(),
        None => violations_filtered,
    };

    let has_errors = violations_final
        .iter()
        .any(|v| v.severity == Severity::Error);

    let mut warnings = target.warnings;
    if errors_hidden_by_filter > 0 {
        warnings.push(format!(
            "--severity warning hid {errors_hidden_by_filter} error-severity violation(s); the \
             exit code reflects the shown (warning) set only — drop --severity, or use \
             --severity error, to gate on errors"
        ));
    }

    emit_read_with(
        nodex_core::CheckResult {
            total: violations_final.len(),
            violations: violations_final,
            skipped_rules: check_report.skipped_rules,
            has_errors,
        },
        warnings,
        &config,
        pretty,
    );

    if has_errors {
        std::process::exit(1);
    }

    Ok(())
}

/// The graph a check run evaluates, plus how its violations are scoped.
struct CheckTarget {
    /// Graph the rules run against — the working tree, or the working
    /// tree with a proposed-content overlay (`--content`).
    graph: nodex_core::Graph,
    /// Node ids to narrow violations to (set-membership) for `--since`,
    /// or `None` for an unscoped project-wide check.
    changed_ids: Option<BTreeSet<String>>,
    /// Violations of the pre-overlay working tree (`--content` only).
    /// The reported set is the count-aware multiset difference
    /// (`rules::introduced_violations`): each occurrence here cancels
    /// at most one identical occurrence in the overlay report, so
    /// every violation the proposal introduces gates it — including a
    /// duplicate of a pre-existing one.
    baseline_violations: Option<Vec<nodex_core::Violation>>,
    /// Diff that activates diff-aware rules, when one is available.
    diff: Option<nodex_core::diff::GraphDiff>,
    /// Non-fatal advisories to surface on the envelope.
    warnings: Vec<String>,
}

/// Resolve what to check and how to scope it.
///
/// `--content` validates an unwritten proposal: the *before* graph is
/// the working tree and the *after* graph overlays the proposed bytes
/// onto `<PATH>`, so the diff names exactly what the edit changes and
/// the diff-aware immutability rules see "already on disk" as the
/// baseline (the launder-safe boundary — never an older committed ref).
/// Both graphs are built read-only, so a write-time check never touches
/// `cache.json`. Otherwise the working tree is the target, scoped by
/// `--since` / `rules.immutable_baseline` via [`resolve_diff`].
fn resolve_target(
    root: &Path,
    args: &CheckArgs,
    config: &nodex_core::Config,
) -> Result<CheckTarget> {
    if let Some(source) = args.content.as_deref() {
        let path = args
            .path
            .clone()
            .expect("clap guarantees --content requires <path>");
        // The one canonical normalization every user-supplied document
        // path gets (symmetric with scaffold and rename): fold `\` to
        // `/`, refuse traversal / absolute forms, collapse `.` segments
        // — so `./docs/a.md`, `docs\a.md`, and `docs/a.md` all key on
        // the scanner's root-relative form and gate the same document.
        let path = PathBuf::from(nodex_core::path_guard::normalize_doc_path(
            &path.to_string_lossy(),
        )?);
        let proposed = read_content_source(source)?;
        let overlay = [(path, proposed)];
        // The gate applies to exactly the bytes the scan would admit:
        // an out-of-scope path is vacuously clean whatever it contains
        // (nodex governs no node there). An unparseable admitted
        // proposal needs no special case — it drops from the overlay
        // graph as a typed `Graph::parse_failures` record, and the
        // delta below refuses on the new `parse_failure` violation.
        let scan = nodex_core::builder::scanner::scan_scope_with_overlay(root, config, &overlay)
            .context("scope scan failed")?;
        // Alias refusal runs BEFORE the admission branch: a permissive
        // include glob admits an aliased spelling as a phantom second
        // node, a narrow one leaves it vacuously clean — either way the
        // gate would otherwise approve bytes that overwrite the real
        // document. The one filesystem-alias test lives in `path_guard`.
        if let Some(canonical) = nodex_core::path_guard::find_scope_alias(
            root,
            &overlay[0].0,
            scan.paths.iter().map(PathBuf::as_path),
        ) {
            return Err(nodex_core::error::Error::Config(format!(
                "path {:?} resolves to the tracked document {:?} (a filesystem spelling \
                 alias); use the exact spelling so the gate checks the right node",
                nodex_core::path_guard::forward_string(&overlay[0].0),
                nodex_core::path_guard::forward_string(&canonical)
            ))
            .into());
        }
        let admitted = scan.paths.iter().any(|p| p == &overlay[0].0);
        let before = nodex_core::builder::build_with_overlay(root, config, &[])
            .context("graph build failed")?
            .graph;
        let after = nodex_core::builder::build_with_overlay(root, config, &overlay)
            .context("proposed-content graph build failed")?;
        let mut warnings = after.warnings;
        // A path the scan does not admit is vacuously clean whatever it
        // contains — a write gate would pass on a misaimed/out-of-scope
        // path having validated nothing. Surface it so the green is never
        // silent.
        if !admitted {
            warnings.push(format!(
                "path {:?} is out of scope — the proposed content was validated against no \
                 rule (nodex governs no document there); verify the path or scope.include",
                nodex_core::path_guard::forward_string(&overlay[0].0)
            ));
        }
        let after = after.graph;
        let diff = nodex_core::diff::compute_diff(&before, &after);
        // The before-report anchors the delta: it runs without a diff
        // (diff-aware rules need "what changed", and nothing has), so
        // any diff-aware violation in the after-report is new by
        // construction and gates the proposal.
        let baseline = check(&before, config, root, None).violations;
        return Ok(CheckTarget {
            graph: after,
            changed_ids: None,
            baseline_violations: Some(baseline),
            diff: Some(diff),
            warnings,
        });
    }

    let outcome = nodex_core::builder::build(root, config, false).context("graph build failed")?;
    let current = outcome.graph;
    let (changed_ids, diff, baseline_warnings) = resolve_diff(root, args, config, &current)?;
    // Surface the build's non-fatal advisories (scope coverage gaps,
    // cache problems); the diff-baseline advisory follows. Dropped
    // documents — unreadable, non-UTF-8, or unparseable — are not
    // advisories: they are `parse_failure` violations the rule pass
    // reports from the graph itself.
    let mut warnings = outcome.warnings;
    warnings.extend(baseline_warnings);
    Ok(CheckTarget {
        graph: current,
        changed_ids,
        baseline_violations: None,
        diff,
        warnings,
    })
}

/// `(changed_ids, diff, warnings)` from [`resolve_diff`]: which node ids
/// to narrow violations to (only for explicit `--since`), the diff that
/// activates diff-aware rules, and any non-fatal advisories.
type DiffResolution = (
    Option<BTreeSet<String>>,
    Option<nodex_core::diff::GraphDiff>,
    Vec<String>,
);

/// Resolve the diff baseline for a check run, returning
/// `(changed_ids, diff, warnings)`.
///
/// An explicit `--since` does double duty: it supplies the diff that
/// activates diff-aware rules AND narrows the reported violations to
/// nodes that changed since that ref. When `--since` is omitted, the
/// configured `rules.immutable_baseline` supplies a diff so the
/// immutability rules run by default — resolved through the one
/// shared substrate (`git_worktree::baseline_diff`, also consumed by
/// `query issues`), so the two commands surface the same violations
/// and the same inert advisory when the baseline cannot engage (not a
/// silent skip, and not the misleading "needs --since" skip reason
/// the rules would emit). The baseline deliberately does NOT narrow
/// the violation set, because the operator never asked to scope the
/// report. The diff is computed against the already-built `current`
/// graph, never a rebuild.
fn resolve_diff(
    root: &Path,
    args: &CheckArgs,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
) -> Result<DiffResolution> {
    if let Some(git_ref) = args.since.as_deref() {
        let (ids, diff, warnings) = changed_ids_against_ref(root, git_ref, config, current)?;
        return Ok((Some(ids), Some(diff), warnings));
    }
    Ok(
        match super::git_worktree::baseline_diff(root, config, current, ".nodex-check")? {
            BaselineResolution::Resolved(baseline) => {
                (None, Some(baseline.diff), baseline.warnings)
            }
            BaselineResolution::Inert { warning } => (None, None, vec![warning]),
            BaselineResolution::NotApplicable => (None, None, vec![]),
        },
    )
}

/// Resolve `git_ref` to the set of node ids that changed between that
/// ref and the working tree's `current` graph. Builds the *before* graph
/// at the ref via a detached `git worktree`, computes the diff, and reads
/// the canonical touched-id set off it ([`GraphDiff::touched_ids`]) so
/// diff-aware rules narrow to exactly the nodes the diff names.
///
/// Single-lens semantics: the working tree's `config` is the one lens
/// and the ref supplies *content only* — the before tree's own
/// `nodex.toml` is never loaded. The diff reports content changes under
/// today's contract (mirroring `--content`, where one config views two
/// content states), and a PR that migrates the config format itself can
/// still pass the gate — under per-ref configs such a PR deadlocks,
/// because the base ref's config no longer parses under the new binary.
fn changed_ids_against_ref(
    root: &Path,
    git_ref: &str,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
) -> Result<(BTreeSet<String>, nodex_core::diff::GraphDiff, Vec<String>)> {
    ensure_work_tree(root, "nodex check --since")?;

    let baseline =
        super::git_worktree::diff_against_ref(root, git_ref, config, current, ".nodex-check")?;
    let ids = baseline.diff.touched_ids();

    Ok((ids, baseline.diff, baseline.warnings))
}
