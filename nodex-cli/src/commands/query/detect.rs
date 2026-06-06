use anyhow::Result;
use std::path::Path;

use crate::format::{ItemsEnvelope, emit_read};

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

    let report = nodex_core::query::issues::find_issues(&graph, &config, root);
    emit_read(report, &config, pretty);
    Ok(())
}
