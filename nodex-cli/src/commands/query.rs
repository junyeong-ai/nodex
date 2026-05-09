use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::Path;

use nodex_core::config::Config;
use nodex_core::error::{Error as CoreError, ParseError};
use nodex_core::model::Graph;

use crate::format::{Envelope, ItemsEnvelope, print_json};

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
    /// Reverse lookup: docs claiming authority over a source-code path
    CoveredBy { path: String },
    /// Unified report of every actionable problem (orphans, stale, unresolved edges, rule violations)
    Issues,
    /// List nodes whose composite trust score is below the threshold
    LowTrust {
        /// Override `config.trust.low_trust_threshold` (in [0, 1]).
        #[arg(long)]
        threshold: Option<f64>,
        /// Only include nodes of this kind.
        #[arg(long)]
        kind: Option<String>,
    },
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
        QueryCommand::CoveredBy { path } => run_covered_by(root, &path, pretty),
        QueryCommand::Issues => run_issues(root, pretty),
        QueryCommand::LowTrust { threshold, kind } => {
            run_low_trust(root, threshold, kind.as_deref(), pretty)
        }
    }
}

pub fn load_graph(root: &Path, config: &Config) -> Result<Graph> {
    let graph_path = root.join(&config.output.dir).join("graph.json");
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
    let graph: Graph = serde_json::from_str(&content).map_err(|e| CoreError::Parse {
        path: graph_path.clone(),
        source: ParseError::Json(e),
    })?;
    Ok(graph)
}

fn run_search(
    root: &Path,
    keyword: &str,
    statuses: Option<Vec<String>>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::search::search(&graph, keyword, statuses.as_deref());
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_backlinks(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    graph.require_node(node_id)?;
    let items = nodex_core::query::traverse::find_backlinks(&graph, node_id);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_chain(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    graph.require_node(node_id)?;
    let items = nodex_core::query::traverse::find_chain(&graph, node_id);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_orphans(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::detect::find_orphans(&graph, &config);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_stale(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::detect::find_stale(&graph, &config);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_tags(root: &Path, tags: Vec<String>, match_all: bool, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::search::search_by_tags(&graph, &tags, match_all, None);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_covered_by(root: &Path, code_path: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::traverse::find_covered_by(&graph, code_path);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_node(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    graph.require_node(node_id)?;
    let detail = nodex_core::query::traverse::find_node_detail(&graph, node_id)
        .expect("require_node guarantees presence");

    print_json(&Envelope::success(detail), pretty);
    Ok(())
}

fn run_issues(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    let report = nodex_core::query::issues::collect_issues(&graph, &config, root);
    print_json(&Envelope::success(report), pretty);
    Ok(())
}

fn run_low_trust(
    root: &Path,
    threshold: Option<f64>,
    kind: Option<&str>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let cutoff = threshold.unwrap_or(config.trust.low_trust_threshold);
    let items = nodex_core::query::trust::find_low_trust(&graph, &config, root, cutoff, kind);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
