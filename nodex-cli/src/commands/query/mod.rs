mod detect;
mod filter;
mod markers;
mod score;
mod traverse;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

use nodex_core::query::recent::{DEFAULT_LIMIT, DEFAULT_SINCE_DAYS, RecentField};

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
        /// Cap returned hits (applied after score-then-id ranking;
        /// `total` still reports every match, `returned` the cap).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show nodes linking to target
    Backlinks {
        id: String,
        /// Cap returned backlinks (`total` still reports every match).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show supersession chain
    Chain { id: String },
    /// List nodes with no incoming edges
    Orphans {
        /// Cap returned orphans (applied after deterministic id-sort;
        /// `total` still reports every match).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// List docs past review threshold
    Stale {
        /// Cap returned entries (applied after staleness-then-id sort;
        /// `total` still reports every match).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// List every node whose state satisfies every named predicate.
    /// AND across categories, OR within a category. Empty filter
    /// returns every node in deterministic id order. See also
    /// `query orphans` / `stale` / `recent` for semantic predicates
    /// that aren't pure filters.
    Nodes(NodesArgs),
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
        /// Attach the document's body text to the entry. The graph
        /// stores fingerprints only, so this re-reads the file —
        /// saving the agent a separate read round-trip.
        #[arg(long)]
        with_body: bool,
    },
    /// Reverse lookup: docs claiming authority over a source-code path
    CoveredBy { path: String },
    /// Unified report of every actionable problem (orphans, stale, unresolved edges, rule violations)
    Issues,
    /// Composite reliability score: single-node lookup, or top-K /
    /// bottom-K listing of the whole graph. Exactly one of `<id>`,
    /// `--bottom`, or `--top` is required. `--kind` / `--status` /
    /// `--below` are listing-only filters.
    #[command(group(
        clap::ArgGroup::new("trust_target")
            .required(true)
            .multiple(false)
            .args(["id", "bottom", "top"])
    ))]
    Trust(TrustArgs),
    /// Find documents similar to an existing node or a prospective one
    Similar(SimilarityArgs),
    /// List documents whose configured date field falls inside a recent window
    Recent(RecentArgs),
    /// Partition the graph into connected components (undirected projection)
    Components {
        /// Cap returned components (applied after size-desc sort;
        /// `total` still reports every component).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Nodes within `--depth` hops of `<id>` (undirected, no policy).
    /// `--depth 0` is rejected at the CLI for symmetry with every
    /// other zero-cap input — use `--depth 1` for "seed plus its
    /// immediate neighbours".
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

/// Args for `query nodes` — the generic listing's predicate flags plus
/// its presentation knobs (`--limit` cap, `--fields` projection).
#[derive(Args)]
pub struct NodesArgs {
    /// CSV of kinds to include (e.g. `--kind spec,adr`).
    #[arg(long, value_delimiter = ',')]
    pub kind: Vec<String>,
    /// CSV of statuses to include (e.g. `--status active,draft`).
    #[arg(long, value_delimiter = ',')]
    pub status: Vec<String>,
    /// CSV of tags to include (any-of by default).
    #[arg(long, value_delimiter = ',')]
    pub tag: Vec<String>,
    /// Require every tag in `--tag` to be present (switch from OR to AND).
    #[arg(long)]
    pub all_tags: bool,
    /// Cap the number of returned nodes (applied after deterministic
    /// id-sort; `total` still reports every match).
    #[arg(long)]
    pub limit: Option<usize>,
    /// CSV of fields to project on each item: identity-spine fields
    /// (`id,title,kind,status,path`) in place, any project-declared
    /// field (other built-ins / `attrs` keys) under `attrs`. Omit for
    /// the full spine.
    #[arg(long, value_delimiter = ',')]
    pub fields: Vec<String>,
    /// Narrow to nodes where a field equals a value (repeatable;
    /// `--where owner=alice --where status=active`). Exact equality only
    /// — no operators — over the scalar fields of the same vocabulary as
    /// `--fields` (a collection-valued built-in is rejected; use `--tag`
    /// for tag membership), matched with the same read as a `cross_field`
    /// `when` predicate.
    #[arg(long = "where", value_name = "FIELD=VALUE")]
    pub where_: Vec<String>,
}

/// Args for `query trust` — the single-node lookup positional plus the
/// listing selectors and filters. The `trust_target` group on the
/// `Trust` variant enforces exactly one of `<id>` / `--bottom` / `--top`.
#[derive(Args)]
pub struct TrustArgs {
    /// Node id for the single-node lookup. Mutually exclusive
    /// with `--bottom` / `--top`.
    #[arg(value_name = "ID")]
    pub id: Option<String>,
    /// Return the N lowest-trust nodes, ascending (most-needs-
    /// review-first). Mutually exclusive with `<id>` / `--top`.
    #[arg(long, conflicts_with_all = ["id", "top"])]
    pub bottom: Option<usize>,
    /// Return the N highest-trust nodes, descending. Mutually
    /// exclusive with `<id>` / `--bottom`.
    #[arg(long, conflicts_with_all = ["id", "bottom"])]
    pub top: Option<usize>,
    /// Restrict the listing to a single kind. Only valid with
    /// `--bottom` or `--top` (incompatible with the single-node
    /// form).
    #[arg(long, conflicts_with = "id", requires = "trust_target")]
    pub kind: Option<String>,
    /// Restrict the listing to a single lifecycle status (e.g.
    /// `active`) — the review-queue read, where terminal nodes
    /// legitimately score near zero and would drown the signal.
    /// Only valid with `--bottom` or `--top`.
    #[arg(long, conflicts_with = "id", requires = "trust_target")]
    pub status: Option<String>,
    /// Opt-in score cutoff: keep only entries whose composite is
    /// strictly below this value. Only valid with `--bottom` or
    /// `--top`. Omit for no cutoff.
    #[arg(long, conflicts_with = "id", requires = "trust_target")]
    pub below: Option<f64>,
}

/// Args for `query similar`. Exactly one of `--id` (existing node) or
/// `--title` (pre-creation probe) is required; clap rejects both and
/// rejects neither via the `similar_target` group.
#[derive(Args)]
#[command(group(
    clap::ArgGroup::new("similar_target")
        .required(true)
        .multiple(false)
        .args(["id", "title"])
))]
pub struct SimilarityArgs {
    /// Existing node id to search neighbours of. Mutually exclusive
    /// with `--title` (pick exactly one).
    #[arg(long)]
    pub id: Option<String>,
    /// Title text for a not-yet-created document. Mutually exclusive
    /// with `--id` (pick exactly one).
    #[arg(long)]
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

impl From<FieldArg> for RecentField {
    fn from(f: FieldArg) -> Self {
        match f {
            FieldArg::Created => Self::Created,
            FieldArg::Updated => Self::Updated,
            FieldArg::Reviewed => Self::Reviewed,
            FieldArg::Any => Self::Any,
        }
    }
}

pub fn run(root: &Path, cmd: QueryCommand, pretty: bool, today: NaiveDate) -> Result<()> {
    match cmd {
        QueryCommand::Search {
            keyword,
            status,
            limit,
        } => {
            let statuses = status.map(|s| s.split(',').map(|s| s.trim().to_string()).collect());
            filter::run_search(root, &keyword, statuses, limit, pretty)
        }
        QueryCommand::Backlinks { id, limit } => traverse::run_backlinks(root, &id, limit, pretty),
        QueryCommand::Chain { id } => traverse::run_chain(root, &id, pretty),
        QueryCommand::Orphans { limit } => detect::run_orphans(root, limit, pretty, today),
        QueryCommand::Stale { limit } => detect::run_stale(root, limit, pretty, today),
        QueryCommand::Nodes(args) => filter::run_nodes(root, args, pretty),
        QueryCommand::Node {
            id,
            path,
            with_body,
        } => traverse::run_node(root, id.as_deref(), path.as_deref(), with_body, pretty),
        QueryCommand::CoveredBy { path } => traverse::run_covered_by(root, &path, pretty),
        QueryCommand::Issues => detect::run_issues(root, pretty, today),
        QueryCommand::Trust(args) => score::run_trust(root, args, pretty, today),
        QueryCommand::Similar(args) => score::run_similar(root, args, pretty),
        QueryCommand::Recent(args) => filter::run_recent(root, args, pretty, today),
        QueryCommand::Components { limit } => traverse::run_components(root, limit, pretty),
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

/// Reject a zero `usize` cap on a positional or `--flag` input. A zero
/// here is silently equivalent to "empty result" — the operator's
/// intent of "show me candidates" never matches that semantic, so we
/// fail fast at the CLI with a clear error that names the flag.
pub(super) fn reject_zero_usize(value: usize, flag: &str) -> Result<()> {
    if value == 0 {
        return Err(nodex_core::error::Error::Config(format!(
            "{flag} must be > 0 (use a positive cap, or omit for the default behaviour)"
        ))
        .into());
    }
    Ok(())
}

/// Reject a zero `u32` cap — same rationale as [`reject_zero_usize`],
/// for flags clap parsed as `u32` (`--depth`, `--days`, …).
pub(super) fn reject_zero_u32(value: u32, flag: &str) -> Result<()> {
    if value == 0 {
        return Err(nodex_core::error::Error::Config(format!(
            "{flag} must be > 0 (use a positive value, or omit for the default behaviour)"
        ))
        .into());
    }
    Ok(())
}

/// Reject `NaN` / `±Infinity` and any value outside `[0.0, 1.0]` on a
/// composite-score cutoff flag. `f64::parse` accepts every IEEE-754
/// value, so without this guard `--below=NaN` filters everything,
/// `--below=inf` keeps everything, and `--below=1.5` produces an
/// always-true or always-false cutoff the operator never asked for.
/// Composite scores are always in `[0, 1]` by construction, so any
/// cutoff outside that range is degenerate.
pub(super) fn reject_non_finite_or_out_of_unit_range(value: f64, flag: &str) -> Result<()> {
    if !value.is_finite() {
        return Err(nodex_core::error::Error::Config(format!(
            "{flag} {value} is not a finite number; supply a real cutoff or omit the flag"
        ))
        .into());
    }
    if !(0.0..=1.0).contains(&value) {
        return Err(nodex_core::error::Error::Config(format!(
            "{flag} {value} is out of range; composite scores live in [0.0, 1.0]"
        ))
        .into());
    }
    Ok(())
}
