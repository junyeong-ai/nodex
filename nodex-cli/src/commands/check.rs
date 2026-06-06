use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use std::collections::BTreeSet;
use std::path::Path;

use nodex_core::check;
use nodex_core::rules::Severity;

use crate::format::emit_read_with;

use super::git_worktree::{Worktree, ensure_work_tree, is_work_tree, scratch_dir};

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

    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let (changed_ids, diff, baseline_warnings) = resolve_diff(root, &args, &config, &result.graph)?;

    let check_report = check(&result.graph, &config, root, diff.as_ref());

    // Pure set-membership filter when `--since` is supplied. Node-less
    // violations (project-wide schema problems, e.g. cycle detection)
    // are *kept* so a narrowed scope never silently drops a finding
    // that can't be attributed to a specific id; the "no silent skips"
    // doctrine applies to violations as well as rules.
    let violations_filtered: Vec<_> = match &changed_ids {
        Some(ids) => check_report
            .violations
            .into_iter()
            .filter(|v| match &v.node_id {
                Some(id) => ids.contains(id),
                None => true,
            })
            .collect(),
        None => check_report.violations,
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

    emit_read_with(
        nodex_core::CheckResult {
            total: violations_final.len(),
            violations: violations_final,
            skipped_rules: check_report.skipped_rules,
            has_errors,
        },
        baseline_warnings,
        &config,
        pretty,
    );

    if has_errors {
        std::process::exit(1);
    }

    Ok(())
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
/// nodes that changed since that ref. When `--since` is omitted, a
/// configured `rules.immutable_baseline` supplies a diff so the
/// immutability rules run by default — but it deliberately does NOT
/// narrow the violation set, because the operator never asked to scope
/// the report. The diff is computed against the already-built `current`
/// graph, never a rebuild.
///
/// When a baseline is configured but the project isn't a git work tree,
/// it can't be resolved — surfaced as a warning (not a silent skip, and
/// not the misleading "needs --since" skip reason the rules would emit).
fn resolve_diff(
    root: &Path,
    args: &CheckArgs,
    config: &nodex_core::Config,
    current: &nodex_core::Graph,
) -> Result<DiffResolution> {
    if let Some(git_ref) = args.since.as_deref() {
        let (ids, diff) = changed_ids_against_ref(root, git_ref, current)?;
        return Ok((Some(ids), Some(diff), vec![]));
    }
    if let Some(baseline) = config.rules.immutable_baseline.as_deref()
        && config.has_immutable_rules()
    {
        if !is_work_tree(root) {
            return Ok((
                None,
                None,
                vec![format!(
                    "rules.immutable_baseline {baseline:?} is set but the project is not a git \
                     work tree; immutability rules are inert this run"
                )],
            ));
        }
        let (_, diff) = changed_ids_against_ref(root, baseline, current)?;
        return Ok((None, Some(diff), vec![]));
    }
    Ok((None, None, vec![]))
}

/// Resolve `git_ref` to the set of node ids that changed between that
/// ref and the working tree's `current` graph. Builds the *before*
/// graph at the ref via a detached `git worktree`, computes the diff,
/// then collects every id touched (added, removed, status-changed,
/// field-changed, edge endpoints, body fingerprint changed). Every
/// variant of [`GraphDiff`] that names a node id MUST contribute here —
/// otherwise diff-aware rules whose only signal is that variant (e.g.
/// `body_immutable` on a body-only edit) would silently never fire,
/// violating `.claude/rules/config-driven.md` ("No silent runtime skips").
fn changed_ids_against_ref(
    root: &Path,
    git_ref: &str,
    current: &nodex_core::Graph,
) -> Result<(BTreeSet<String>, nodex_core::diff::GraphDiff)> {
    ensure_work_tree(root, "nodex check --since")?;

    let scratch = scratch_dir(root, ".nodex-check")?;
    let before_target = scratch.join("before");
    let before = Worktree::add(root, git_ref, &before_target, Some(scratch.clone()))?;

    let before_config = nodex_core::load_project(before.path())?;
    let before_result = nodex_core::builder::build(before.path(), &before_config, true)?;

    let diff = nodex_core::diff::compute_diff(&before_result.graph, current);

    let mut ids: BTreeSet<String> = BTreeSet::new();
    for n in &diff.added_nodes {
        ids.insert(n.id.clone());
    }
    for n in &diff.removed_nodes {
        ids.insert(n.id.clone());
    }
    for t in &diff.status_transitions {
        ids.insert(t.id.clone());
    }
    for c in &diff.field_changes {
        ids.insert(c.id.clone());
    }
    for c in &diff.body_changes {
        ids.insert(c.id.clone());
    }
    for e in &diff.added_edges {
        ids.insert(e.source.clone());
    }
    for e in &diff.removed_edges {
        ids.insert(e.source.clone());
    }

    Ok((ids, diff))
}
