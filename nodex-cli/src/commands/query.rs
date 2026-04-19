use anyhow::{Context, Result};
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::error::Error as CoreError;
use nodex_core::model::Graph;

use crate::format::{Envelope, print_json};

pub fn load_graph(root: &Path, config: &Config) -> Result<Graph> {
    let graph_path = root.join(&config.output.dir).join("graph.json");
    let content = std::fs::read_to_string(&graph_path).with_context(|| {
        format!(
            "graph.json not found at {}. Run `nodex build` first.",
            graph_path.display()
        )
    })?;
    let graph: Graph = serde_json::from_str(&content).context("failed to parse graph.json")?;
    Ok(graph)
}

/// Consistent query output structure: { items: [...], total: N }
#[derive(serde::Serialize)]
struct QueryOutput<T: serde::Serialize> {
    items: T,
    total: usize,
}

pub fn run_search(
    root: &Path,
    keyword: &str,
    statuses: Option<Vec<String>>,
    pretty: bool,
) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let results = nodex_core::query::search::search(&graph, keyword, statuses.as_deref());

    let total = results.len();
    print_json(
        &Envelope::success(QueryOutput {
            items: results,
            total,
        }),
        pretty,
    );
    Ok(())
}

pub fn run_backlinks(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::traverse::find_backlinks(&graph, node_id);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

pub fn run_chain(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::traverse::find_chain(&graph, node_id);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

pub fn run_orphans(root: &Path, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::detect::find_orphans(&graph, &config);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

pub fn run_stale(root: &Path, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::detect::find_stale(&graph, &config);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

pub fn run_tags(root: &Path, tags: Vec<String>, match_all: bool, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::search::search_by_tags(&graph, &tags, match_all, None);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

pub fn run_node(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let detail = nodex_core::query::traverse::node_detail(&graph, node_id)
        .ok_or_else(|| CoreError::NodeNotFound(node_id.to_string()))?;

    print_json(&Envelope::success(detail), pretty);
    Ok(())
}

pub fn run_issues(root: &Path, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let report = nodex_core::query::issues::collect_issues(&graph, &config);
    print_json(&Envelope::success(report), pretty);
    Ok(())
}
