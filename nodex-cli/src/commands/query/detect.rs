use anyhow::Result;
use std::path::Path;

use crate::format::{ItemsEnvelope, emit_read, emit_read_with};

use super::{load_graph, reject_zero_usize};

pub(crate) fn run_orphans(root: &Path, limit: Option<usize>, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::detect::find_orphans(&graph, &config);
    emit_read(ItemsEnvelope::capped(items, limit), &config, pretty);
    Ok(())
}

pub(crate) fn run_stale(root: &Path, limit: Option<usize>, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::detect::find_stale(&graph, &config);
    emit_read(ItemsEnvelope::capped(items, limit), &config, pretty);
    Ok(())
}

pub(crate) fn run_issues(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    // The same diff context a default `check` runs under — the
    // configured `rules.immutable_baseline`, resolved through the one
    // shared substrate — so "what's broken?" and `check` can never
    // disagree about the immutability violations. The baseline build's
    // own warnings (e.g. a document unparseable at the baseline, which
    // silently disables its diff-aware rules) ride along to the envelope.
    let baseline =
        crate::commands::git_worktree::baseline_diff(root, &config, &graph, ".nodex-issues")?;
    let (diff, warnings) = match baseline {
        Some(b) => (Some(b.diff), b.warnings),
        None => (None, vec![]),
    };
    let report = nodex_core::query::issues::find_issues(&graph, &config, root, diff.as_ref());
    emit_read_with(report, warnings, &config, pretty);
    Ok(())
}
