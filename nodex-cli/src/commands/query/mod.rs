mod detect;
mod filter;
mod markers;
mod score;
mod traverse;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

use nodex_core::config::Config;
use nodex_core::error::{Error as CoreError, ParseError};
use nodex_core::model::Graph;
use nodex_core::query::recent::{DEFAULT_LIMIT, DEFAULT_SINCE_DAYS, RecencyField};

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
    /// List every node whose state satisfies every named predicate.
    /// AND across categories, OR within a category. Empty filter
    /// returns every node in deterministic id order. See also
    /// `query orphans` / `stale` / `recent` for semantic predicates
    /// that aren't pure filters.
    Nodes {
        /// CSV of kinds to include (e.g. `--kind spec,adr`).
        #[arg(long, value_delimiter = ',')]
        kind: Vec<String>,
        /// CSV of statuses to include (e.g. `--status active,draft`).
        #[arg(long, value_delimiter = ',')]
        status: Vec<String>,
        /// CSV of tags to include (any-of by default).
        #[arg(long, value_delimiter = ',')]
        tag: Vec<String>,
        /// Require every tag in `--tag` to be present (switch from OR to AND).
        #[arg(long)]
        all_tags: bool,
        /// Cap the number of returned nodes (applied after deterministic id-sort).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show full node detail. Pass either the node `<id>` or
    /// `--path <file>` (mutually exclusive). `--path` is the natural
    /// form for editor / IDE integrations that hold the on-disk path
    /// rather than the node id.
    #[command(group(
        clap::ArgGroup::new("node_lookup")
            .required(true)
            .multiple(false)
            .args(["id", "path"])
    ))]
    Node {
        /// Node identifier (mutually exclusive with `--path`).
        #[arg(value_name = "ID")]
        id: Option<String>,
        /// Project-relative file path (mutually exclusive with `<id>`).
        #[arg(long, value_name = "FILE")]
        path: Option<String>,
    },
    /// Reverse lookup: docs claiming authority over a source-code path
    CoveredBy { path: String },
    /// Unified report of every actionable problem (orphans, stale, unresolved edges, rule violations)
    Issues,
    /// Composite reliability score: single-node lookup, or top-K /
    /// bottom-K listing of the whole graph. Exactly one of `<id>`,
    /// `--bottom`, or `--top` is required. `--kind` / `--below` are
    /// listing-only filters.
    #[command(group(
        clap::ArgGroup::new("trust_target")
            .required(true)
            .multiple(false)
            .args(["id", "bottom", "top"])
    ))]
    Trust {
        /// Node id for the single-node lookup. Mutually exclusive
        /// with `--bottom` / `--top`.
        #[arg(value_name = "ID")]
        id: Option<String>,
        /// Return the N lowest-trust nodes, ascending (most-needs-
        /// review-first). Mutually exclusive with `<id>` / `--top`.
        #[arg(long, conflicts_with_all = ["id", "top"])]
        bottom: Option<usize>,
        /// Return the N highest-trust nodes, descending. Mutually
        /// exclusive with `<id>` / `--bottom`.
        #[arg(long, conflicts_with_all = ["id", "bottom"])]
        top: Option<usize>,
        /// Restrict the listing to a single kind. Only valid with
        /// `--bottom` or `--top` (incompatible with the single-node
        /// form).
        #[arg(long, conflicts_with = "id", requires = "trust_target")]
        kind: Option<String>,
        /// Opt-in score cutoff: keep only entries whose composite is
        /// strictly below this value. Only valid with `--bottom` or
        /// `--top`. Omit for no cutoff.
        #[arg(long, conflicts_with = "id", requires = "trust_target")]
        below: Option<f64>,
    },
    /// Find documents similar to an existing node or a prospective one
    Similar(SimilarArgs),
    /// List documents whose configured date field falls inside a recent window
    Recent(RecentArgs),
    /// Partition the graph into connected components (undirected projection)
    Components,
    /// Nodes within `--depth` hops of `<id>` (undirected, no policy)
    Neighborhood {
        id: String,
        #[arg(long, default_value_t = 1)]
        depth: u32,
    },
    /// Group body-text annotations (`[[annotations]]`) by pattern + key
    Annotations {
        /// Restrict output to a single pattern name (matches `[[annotations]].name`).
        #[arg(long)]
        name: Option<String>,
        /// Frontmatter fields to attach to each source (comma-separated).
        /// Each name must be either a built-in frontmatter field or
        /// declared in `[schema]` (validated against
        /// `Config::declared_fields_universe` at load).
        #[arg(long, value_delimiter = ',')]
        with_frontmatter: Vec<String>,
        /// Drop entries whose occurrence `count` is below the
        /// threshold. `--min-count 3` surfaces only keys repeated at
        /// least three times. Groups left empty after the filter are
        /// dropped from the output entirely. Default `1` keeps every
        /// entry (no-op).
        #[arg(long, default_value_t = 1)]
        min_count: usize,
    },
    /// Every node whose dependency chain ultimately reaches `<id>`
    /// (transitive reverse traversal, follows incoming edges only).
    Dependents {
        id: String,
        /// Maximum hops to expand. Omit for unbounded.
        #[arg(long)]
        depth: Option<u32>,
        /// Restrict to specific edge relations (comma-separated).
        /// Defaults to "every known relation".
        #[arg(long, value_delimiter = ',')]
        relations: Vec<String>,
    },
}

/// Args for `query similar`. Either `--id` (existing node) or `--title`
/// (pre-creation probe) is required; clap rejects both.
#[derive(Args)]
pub struct SimilarArgs {
    /// Existing node id to search neighbours of. Mutually exclusive
    /// with `--title` (pick exactly one).
    #[arg(long, conflicts_with = "title")]
    pub id: Option<String>,
    /// Title text for a not-yet-created document. Mutually exclusive
    /// with `--id` (pick exactly one).
    #[arg(long, conflicts_with = "id")]
    pub title: Option<String>,
    /// Kind for the prospective document (with `--title`).
    #[arg(long)]
    pub kind: Option<String>,
    /// Tags for the prospective document (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Parent directory for the prospective document.
    #[arg(long)]
    pub parent_dir: Option<PathBuf>,
    /// Maximum candidates returned. Defaults to
    /// `config.similarity.default_limit` — the operator-capacity cap.
    #[arg(long)]
    pub limit: Option<usize>,
    /// Opt-in score cutoff: keep only candidates whose composite
    /// similarity is at least this value. Defaults to no cutoff
    /// (every candidate up to `--limit` is returned).
    #[arg(long)]
    pub min_score: Option<f64>,
}

/// Flags for `query recent`. Grouped so clap rejects passing both
/// `--since` (absolute) and `--days` (relative) at parse time.
#[derive(Args)]
pub struct RecentArgs {
    /// Absolute cut-off date (YYYY-MM-DD); entries on or after are returned.
    #[arg(long, conflicts_with = "days")]
    pub since: Option<NaiveDate>,
    /// Last N days, anchored to today.
    #[arg(long, default_value_t = DEFAULT_SINCE_DAYS)]
    pub days: u32,
    /// Filter by document kind (must be in `kinds.allowed`).
    #[arg(long)]
    pub kind: Option<String>,
    /// Which date field to consult.
    #[arg(long, value_enum, default_value_t = FieldArg::Any)]
    pub field: FieldArg,
    /// Maximum entries returned.
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    pub limit: usize,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum FieldArg {
    Created,
    Updated,
    Reviewed,
    Any,
}

impl From<FieldArg> for RecencyField {
    fn from(f: FieldArg) -> Self {
        match f {
            FieldArg::Created => Self::Created,
            FieldArg::Updated => Self::Updated,
            FieldArg::Reviewed => Self::Reviewed,
            FieldArg::Any => Self::Any,
        }
    }
}

pub fn run(root: &Path, cmd: QueryCommand, pretty: bool) -> Result<()> {
    match cmd {
        QueryCommand::Search { keyword, status } => {
            let statuses = status.map(|s| s.split(',').map(|s| s.trim().to_string()).collect());
            filter::run_search(root, &keyword, statuses, pretty)
        }
        QueryCommand::Backlinks { id } => traverse::run_backlinks(root, &id, pretty),
        QueryCommand::Chain { id } => traverse::run_chain(root, &id, pretty),
        QueryCommand::Orphans => detect::run_orphans(root, pretty),
        QueryCommand::Stale => detect::run_stale(root, pretty),
        QueryCommand::Nodes {
            kind,
            status,
            tag,
            all_tags,
            limit,
        } => filter::run_nodes(root, kind, status, tag, all_tags, limit, pretty),
        QueryCommand::Node { id, path } => {
            traverse::run_node(root, id.as_deref(), path.as_deref(), pretty)
        }
        QueryCommand::CoveredBy { path } => traverse::run_covered_by(root, &path, pretty),
        QueryCommand::Issues => detect::run_issues(root, pretty),
        QueryCommand::Trust {
            id,
            bottom,
            top,
            kind,
            below,
        } => score::run_trust(root, id, bottom, top, kind, below, pretty),
        QueryCommand::Similar(args) => score::run_similar(root, args, pretty),
        QueryCommand::Recent(args) => filter::run_recent(root, args, pretty),
        QueryCommand::Components => traverse::run_components(root, pretty),
        QueryCommand::Neighborhood { id, depth } => {
            traverse::run_neighborhood(root, &id, depth, pretty)
        }
        QueryCommand::Annotations {
            name,
            with_frontmatter,
            min_count,
        } => markers::run_annotations(root, name.as_deref(), with_frontmatter, min_count, pretty),
        QueryCommand::Dependents {
            id,
            depth,
            relations,
        } => traverse::run_dependents(root, &id, depth, relations, pretty),
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

fn reject_empty_csv_entries(flag: &str, values: &[String]) -> Result<()> {
    if values.iter().any(|v| v.is_empty()) {
        return Err(nodex_core::error::Error::Config(format!(
            "{flag} contains an empty entry — drop the stray comma"
        ))
        .into());
    }
    Ok(())
}

fn reject_unknown_vocabulary(flag: &str, values: &[String], allowed: &[String]) -> Result<()> {
    let unknown: Vec<&str> = values
        .iter()
        .filter(|v| !allowed.iter().any(|a| a == *v))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return Err(nodex_core::error::Error::Config(format!(
            "{flag} contains unknown value(s) {unknown:?}; declared: {allowed:?}"
        ))
        .into());
    }
    Ok(())
}
