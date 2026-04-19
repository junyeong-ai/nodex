use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::error::Error as CoreError;
use nodex_core::model::Graph;

use crate::format::{Envelope, print_json};

/// Query subcommands. Each variant carries exactly the arguments its
/// query needs; the top-level dispatcher just passes this value to
/// [`run`].
#[derive(Subcommand)]
pub enum QueryCommand {
    /// Keyword search (title/id/tags)
    Search {
        keyword: String,
        /// Filter by status (comma-separated)
        #[arg(long)]
        status: Option<String>,
    },
    /// Show nodes linking to target
    Backlinks { id: String },
    /// Show supersession chain
    Chain { id: String },
    /// List nodes with no incoming edges
    Orphans,
    /// List docs past review threshold
    Stale,
    /// Search by tags
    Tags {
        tags: Vec<String>,
        /// Require all tags (default: any)
        #[arg(long)]
        all: bool,
    },
    /// Show full node detail
    Node { id: String },
    /// Unified report of every actionable problem (orphans, stale, unresolved edges, rule violations)
    Issues,
}

pub fn run(root: &Path, cmd: QueryCommand, pretty: bool) -> Result<()> {
    match cmd {
        QueryCommand::Search { keyword, status } => {
            let statuses = status.map(|s| s.split(',').map(|s| s.trim().to_string()).collect());
            run_search(root, &keyword, statuses, pretty)
        }
        QueryCommand::Backlinks { id } => run_backlinks(root, &id, pretty),
        QueryCommand::Chain { id } => run_chain(root, &id, pretty),
        QueryCommand::Orphans => run_orphans(root, pretty),
        QueryCommand::Stale => run_stale(root, pretty),
        QueryCommand::Tags { tags, all } => run_tags(root, tags, all, pretty),
        QueryCommand::Node { id } => run_node(root, &id, pretty),
        QueryCommand::Issues => run_issues(root, pretty),
    }
}

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

fn run_search(
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

fn run_backlinks(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::traverse::find_backlinks(&graph, node_id);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

fn run_chain(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::traverse::find_chain(&graph, node_id);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

fn run_orphans(root: &Path, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::detect::find_orphans(&graph, &config);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

fn run_stale(root: &Path, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::detect::find_stale(&graph, &config);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

fn run_tags(root: &Path, tags: Vec<String>, match_all: bool, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let items = nodex_core::query::search::search_by_tags(&graph, &tags, match_all, None);
    let total = items.len();

    print_json(&Envelope::success(QueryOutput { items, total }), pretty);
    Ok(())
}

fn run_node(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let detail = nodex_core::query::traverse::find_node_detail(&graph, node_id)
        .ok_or_else(|| CoreError::NodeNotFound(node_id.to_string()))?;

    print_json(&Envelope::success(detail), pretty);
    Ok(())
}

fn run_issues(root: &Path, pretty: bool) -> Result<()> {
    let config = Config::load(root)?;
    let graph = load_graph(root, &config)?;

    let report = nodex_core::query::issues::collect_issues(&graph, &config);
    print_json(&Envelope::success(report), pretty);
    Ok(())
}
