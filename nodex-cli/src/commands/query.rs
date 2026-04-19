use anyhow::{Context, Result};
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::error::Error as CoreError;
use nodex_core::model::Graph;

use crate::format::{Envelope, print_json};

pub fn load_graph(root: &Path, config: &Config) -> Result<Graph> {
    let graph_path = root.join(&config.output.dir).join("graph.json");
    // Convert io::Error → CoreError::Io first so the typed chain
    // classifies as IO_ERROR. Anyhow's `.with_context` then adds the
    // human hint while preserving the typed cause for `format.rs`.
    let content = std::fs::read_to_string(&graph_path)
        .map_err(|source| CoreError::Io {
            path: graph_path.clone(),
            source,
        })
        .with_context(|| {
            format!(
                "graph.json not found at {}. Run `nodex build` first.",
                graph_path.display()
            )
        })?;
    // Corrupt graph.json is a typed frontmatter/parse concern, not a
    // plain anyhow string — routing it through Error::Frontmatter
    // keeps format.rs's classifier at PARSE_ERROR. (Graph files are
    // serialised with serde_json; the Frontmatter variant covers
    // "structured input failed to deserialise at this path", which
    // matches the semantics even though the body format differs.)
    let graph: Graph = serde_json::from_str(&content).map_err(|e| CoreError::Frontmatter {
        path: graph_path.clone(),
        message: format!("corrupt graph.json: {e}"),
    })?;
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

    let detail = nodex_core::query::traverse::find_node_detail(&graph, node_id)
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
