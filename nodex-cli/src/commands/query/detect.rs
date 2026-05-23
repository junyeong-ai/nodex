use anyhow::Result;
use std::path::Path;

use crate::format::{Envelope, ItemsEnvelope, print_json};

use super::load_graph;

pub(crate) fn run_orphans(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::detect::find_orphans(&graph, &config);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

pub(crate) fn run_stale(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::detect::find_stale(&graph, &config);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

pub(crate) fn run_issues(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    let report = nodex_core::query::issues::collect_issues(&graph, &config, root);
    print_json(&Envelope::success(report), pretty);
    Ok(())
}
