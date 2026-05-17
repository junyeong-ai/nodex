use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nodex_core::rules::Severity;
use nodex_core::{CheckScope, check};

use crate::format::{Envelope, print_json};

use super::git_worktree::{Worktree, ensure_work_tree, scratch_dir};

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
    /// Restrict the check to a single document. The path is resolved
    /// against the project's tracked node set; rules that require a
    /// project-wide view (sequential / unique numbering) surface as
    /// `skipped_rules` with a reason instead of producing a misleading
    /// "no violations" result. Combinable with `--since`: only nodes
    /// changed *and* matching this path produce violations.
    #[arg(long, value_name = "FILE")]
    pub path: Option<PathBuf>,
}

pub fn run(root: &Path, args: CheckArgs, pretty: bool) -> Result<()> {
    let severity_filter = args.severity.map(Severity::from);
    let config = nodex_core::load_project(root)?;

    let result = nodex_core::builder::build(root, &config, false).context("graph build failed")?;

    let (changed_ids, diff) = match args.since.as_deref() {
        Some(git_ref) => {
            let (ids, d) = changed_ids_against_ref(root, git_ref)?;
            (Some(ids), Some(d))
        }
        None => (None, None),
    };

    // Resolve `--path` into a `CheckScope` early so an unrecognised
    // path surfaces as a typed error (not as an empty "no violations"
    // result). The lookup goes through the same normalisation
    // `nodex query node --path` uses so the same string works for
    // either command.
    let scope = match args.path.as_deref() {
        Some(p) => {
            let p_str = p.to_string_lossy();
            let normalised = nodex_core::path_guard::normalize_for_lookup(&p_str, root)?;
            let node = result.graph.require_node_by_path(Path::new(&normalised))?;
            CheckScope::Document {
                node_id: node.id.clone(),
            }
        }
        None => CheckScope::Project,
    };

    let check_report = check(&result.graph, &config, root, diff.as_ref(), scope);

    // Pure set-membership filter when `--since` is supplied. No
    // neighbour expansion — the changed source node carries violations
    // of broken outgoing links itself. Node-less violations (project-
    // wide schema problems, e.g. cycle detection) are *kept* so a
    // narrowed scope never silently drops a finding that can't be
    // attributed to a specific id; the "no silent skips" doctrine
    // applies to violations as well as rules. Combines with `--path`
    // by intersection: when both are set, only nodes that changed
    // *and* match the path produce violations.
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

    print_json(
        &Envelope::success(nodex_core::CheckResult {
            total: violations_final.len(),
            violations: violations_final,
            skipped_rules: check_report.skipped,
            has_errors,
        }),
        pretty,
    );

    if has_errors {
        std::process::exit(1);
    }

    Ok(())
}

/// Resolve `git_ref` to the set of node ids that changed between that
/// ref and the working tree's graph. Builds a graph at the ref via a
/// detached `git worktree`, computes the diff, then collects every id
/// touched (added, removed, status-changed, field-changed, edge endpoints,
/// body fingerprint changed). Every variant of [`GraphDiff`] that names a
/// node id MUST contribute here — otherwise diff-aware rules whose only
/// signal is that variant (e.g. `body_immutable` on a body-only edit)
/// would silently never fire, violating `.claude/rules/config-driven.md`
/// ("No silent runtime skips").
fn changed_ids_against_ref(
    root: &Path,
    git_ref: &str,
) -> Result<(BTreeSet<String>, nodex_core::diff::GraphDiff)> {
    ensure_work_tree(root, "nodex check --since")?;

    let scratch = scratch_dir(root, ".nodex-check")?;
    let before_target = scratch.join("before");
    let before = Worktree::add(root, git_ref, &before_target, Some(scratch.clone()))?;

    let before_config = nodex_core::load_project(before.path())?;
    let before_result = nodex_core::builder::build(before.path(), &before_config, true)?;

    let after_config = nodex_core::load_project(root)?;
    let after_result = nodex_core::builder::build(root, &after_config, false)?;

    let diff = nodex_core::diff::compute_diff(&before_result.graph, &after_result.graph);

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
