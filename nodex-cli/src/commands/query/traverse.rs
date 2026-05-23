use anyhow::Result;
use std::path::Path;

use crate::format::{Envelope, ItemsEnvelope, print_json};

use super::load_graph;

pub(crate) fn run_backlinks(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    graph.require_node(node_id)?;
    let items = nodex_core::query::traverse::find_backlinks(&graph, node_id);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

pub(crate) fn run_chain(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    graph.require_node(node_id)?;
    let items = nodex_core::query::traverse::find_chain(&graph, node_id);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

pub(crate) fn run_node(
    root: &Path,
    id: Option<&str>,
    path: Option<&str>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    let resolved_id: String = match (id, path) {
        (Some(id), None) => graph.require_node(id)?.id.clone(),
        (None, Some(p)) => {
            let normalised = nodex_core::path_guard::normalize_for_lookup(p, root)?;
            graph
                .require_node_by_path(Path::new(&normalised))?
                .id
                .clone()
        }
        _ => unreachable!("clap ArgGroup enforces exactly one of <id> or --path"),
    };

    let detail = nodex_core::query::traverse::find_node_entry(&graph, &resolved_id)
        .expect("require_node / node_by_path guarantees presence");

    print_json(&Envelope::success(detail), pretty);
    Ok(())
}

pub(crate) fn run_covered_by(root: &Path, code_path: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let normalised = nodex_core::path_guard::normalize_for_lookup(code_path, root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::traverse::find_covered_by(&graph, &normalised);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

pub(crate) fn run_dependents(
    root: &Path,
    id: &str,
    depth: Option<u32>,
    relations: Vec<String>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if !relations.is_empty() {
        let known = config.known_relations();
        let unknown: Vec<&str> = relations
            .iter()
            .filter(|r| !known.contains(r.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let known_sorted: Vec<&str> = known.iter().map(String::as_str).collect();
            return Err(nodex_core::error::Error::Config(format!(
                "--relations contains unknown value(s) {unknown:?}; known: {known_sorted:?}"
            ))
            .into());
        }
    }
    let graph = load_graph(root, &config)?;
    let report = nodex_core::query::dependents::find_dependents(&graph, id, depth, &relations)?;
    print_json(&Envelope::success(report), pretty);
    Ok(())
}

pub(crate) fn run_neighborhood(root: &Path, id: &str, depth: u32, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let result = nodex_core::query::structure::find_neighborhood(&graph, id, depth)?;
    print_json(&Envelope::success(result), pretty);
    Ok(())
}

pub(crate) fn run_components(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::structure::find_components(&graph);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
