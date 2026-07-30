use anyhow::Result;
use chrono::NaiveDate;
use std::path::Path;

use crate::format::{ItemsEnvelope, emit_read_with};

use super::reject_zero_usize;

pub(crate) fn run_orphans(
    root: &Path,
    limit: Option<usize>,
    pretty: bool,
    today: NaiveDate,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let snapshot = nodex_core::load_graph(root, &config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let items = nodex_core::query::detect::find_orphans(graph, &config, today);
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}

pub(crate) fn run_stale(
    root: &Path,
    limit: Option<usize>,
    pretty: bool,
    today: NaiveDate,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let snapshot = nodex_core::load_graph(root, &config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let items = nodex_core::query::detect::find_stale(graph, &config, today);
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}

pub(crate) fn run_issues(root: &Path, pretty: bool, today: NaiveDate) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let snapshot = nodex_core::load_graph(root, &config)?;
    let (graph, mut warnings) = (snapshot.graph(), snapshot.warnings());

    // The same diff context a default `check` runs under — the
    // configured `rules.immutable_baseline`, resolved through the one
    // shared substrate — so "what's broken?" and `check` can never
    // disagree about the immutability violations, nor about the inert
    // advisory (baseline set, immutability rules declared, root not a
    // git work tree — one wording, constructed in the substrate). The
    // baseline build's own warnings (e.g. a document unparseable at
    // the baseline, which silently disables its diff-aware rules) ride
    // along to the envelope.
    use crate::commands::git_worktree::BaselineResolution;
    let diff = match crate::commands::git_worktree::baseline_diff(
        root,
        &config,
        graph,
        ".nodex-issues",
    )? {
        BaselineResolution::Resolved(baseline) => {
            warnings.extend(baseline.warnings);
            Some(baseline.diff)
        }
        BaselineResolution::Inert { warning } => {
            warnings.push(warning);
            None
        }
        BaselineResolution::NotApplicable => None,
    };
    let report = nodex_core::query::issues::find_issues(graph, &config, root, diff.as_ref(), today);
    emit_read_with(report, warnings, &config, pretty);
    Ok(())
}
