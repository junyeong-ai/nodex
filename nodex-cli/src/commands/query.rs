use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{Args, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};

use nodex_core::config::Config;
use nodex_core::error::{Error as CoreError, ParseError};
use nodex_core::model::Graph;
use nodex_core::query::recent::{
    DEFAULT_LIMIT, DEFAULT_SINCE_DAYS, RecencyField, RecencyOptions, RecencySince,
};
use nodex_core::query::similar::{SimilarityOptions, SimilarityTarget};

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
    /// List nodes whose composite trust score is below the threshold
    LowTrust {
        /// Override `config.trust.low_trust_threshold` (in [0, 1]).
        #[arg(long)]
        threshold: Option<f64>,
        /// Only include nodes of this kind.
        #[arg(long)]
        kind: Option<String>,
    },
    /// Composite reliability score for a single document
    Trust { id: String },
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
        /// least three times — the natural shape for promotion-
        /// candidate / repeated-topic queries that previously had
        /// to filter the full result in a downstream pipeline.
        /// Groups left empty after the filter are dropped from the
        /// output entirely. Default `1` keeps every entry (no-op).
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
    /// Existing node id to search neighbours of.
    #[arg(long, conflicts_with = "title")]
    pub id: Option<String>,
    /// Title text for a not-yet-created document.
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
    /// Override `config.similarity.threshold`.
    #[arg(long)]
    pub threshold: Option<f64>,
    /// Maximum candidates returned. Defaults to `config.similarity.default_limit`.
    #[arg(long)]
    pub limit: Option<usize>,
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
            run_search(root, &keyword, statuses, pretty)
        }
        QueryCommand::Backlinks { id } => run_backlinks(root, &id, pretty),
        QueryCommand::Chain { id } => run_chain(root, &id, pretty),
        QueryCommand::Orphans => run_orphans(root, pretty),
        QueryCommand::Stale => run_stale(root, pretty),
        QueryCommand::Nodes {
            kind,
            status,
            tag,
            all_tags,
            limit,
        } => run_nodes(root, kind, status, tag, all_tags, limit, pretty),
        QueryCommand::Node { id, path } => run_node(root, id.as_deref(), path.as_deref(), pretty),
        QueryCommand::CoveredBy { path } => run_covered_by(root, &path, pretty),
        QueryCommand::Issues => run_issues(root, pretty),
        QueryCommand::LowTrust { threshold, kind } => {
            run_low_trust(root, threshold, kind.as_deref(), pretty)
        }
        QueryCommand::Trust { id } => run_trust(root, &id, pretty),
        QueryCommand::Similar(args) => run_similar(root, args, pretty),
        QueryCommand::Recent(args) => run_recent(root, args, pretty),
        QueryCommand::Components => run_components(root, pretty),
        QueryCommand::Neighborhood { id, depth } => run_neighborhood(root, &id, depth, pretty),
        QueryCommand::Annotations {
            name,
            with_frontmatter,
            min_count,
        } => run_annotations(root, name.as_deref(), with_frontmatter, min_count, pretty),
        QueryCommand::Dependents {
            id,
            depth,
            relations,
        } => run_dependents(root, &id, depth, relations, pretty),
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

fn run_nodes(
    root: &Path,
    kind: Vec<String>,
    status: Vec<String>,
    tag: Vec<String>,
    all_tags: bool,
    limit: Option<usize>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // Validate every predicate against the project vocabulary before
    // touching the graph. Silent-empty results on a typo
    // (`--kind spek` for `spec`) violate the "fail loud on user
    // errors" discipline `principles.md` declares — every other
    // vocabulary-consuming query (similar, dependents, annotations)
    // applies the same check.
    reject_empty_csv_entries("--kind", &kind)?;
    reject_empty_csv_entries("--status", &status)?;
    reject_empty_csv_entries("--tag", &tag)?;
    reject_unknown_vocabulary("--kind", &kind, &config.kinds.allowed)?;
    reject_unknown_vocabulary("--status", &status, &config.statuses.allowed)?;
    if let Some(0) = limit {
        return Err(nodex_core::error::Error::Config(
            "--limit must be > 0 (use a positive cap, or omit the flag for no limit)".into(),
        )
        .into());
    }

    let graph = load_graph(root, &config)?;
    let filter = nodex_core::NodeFilter {
        kinds: kind,
        statuses: status,
        tags: tag,
        require_all_tags: all_tags,
        limit,
    };
    let items = nodex_core::find_nodes(&graph, &filter);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

/// CSV-passed flags split `""` into `[""]`. An empty entry is never
/// a legitimate value and would silently match nothing. Fail loud.
fn reject_empty_csv_entries(flag: &str, values: &[String]) -> Result<()> {
    if values.iter().any(|v| v.is_empty()) {
        return Err(nodex_core::error::Error::Config(format!(
            "{flag} contains an empty entry — drop the stray comma"
        ))
        .into());
    }
    Ok(())
}

/// Reject any value not present in the project's declared vocabulary.
/// Mirrors the discipline used by `query similar --kind`, `query
/// dependents --relations`, etc.
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

fn run_covered_by(root: &Path, code_path: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // Symmetric with `query node --path`: editor / IDE integrations
    // routinely supply `./`-prefixed or absolute paths. Normalise to
    // the project-relative form before scanning `covers:` entries —
    // which are stored project-relative — so the same path the user
    // types in their editor reaches the matcher unchanged.
    let normalised = nodex_core::path_guard::normalize_for_lookup(code_path, root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::traverse::find_covered_by(&graph, &normalised);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_node(root: &Path, id: Option<&str>, path: Option<&str>, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    // clap's ArgGroup(required, !multiple) guarantees exactly one of
    // (id, path) is set — this branch is the only safe destructuring.
    let resolved_id: String = match (id, path) {
        (Some(id), None) => graph.require_node(id)?.id.clone(),
        (None, Some(p)) => {
            // Normalise `./`, absolute-under-root, and forward-slashes
            // so editor / IDE integrations can pass whichever form
            // they have in hand. Absolute paths outside the project
            // root surface as PATH_ESCAPES_ROOT before we touch the
            // graph.
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
    // Symmetric with `run_nodes`, `run_similar`, `run_recent`,
    // `run_dependents`: vocabulary typos fail loud, never silently
    // narrow the result set to empty.
    if let Some(k) = kind {
        reject_unknown_vocabulary(
            "--kind",
            std::slice::from_ref(&k.to_string()),
            &config.kinds.allowed,
        )?;
    }
    let graph = load_graph(root, &config)?;
    let cutoff = threshold.unwrap_or(config.trust.low_trust_threshold);
    let items = nodex_core::query::trust::find_low_trust(&graph, &config, root, cutoff, kind);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_trust(root: &Path, id: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let report = nodex_core::query::trust::compute_trust(&graph, &config, root, id)?;
    print_json(&Envelope::success(report), pretty);
    Ok(())
}

fn run_similar(root: &Path, args: SimilarArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    // Validate `--kind` against the project's vocabulary up front.
    // Without this check, a typo (`--kind adrr`) would silently
    // mismatch every doc on the kind component and return zero
    // candidates — a quiet "no duplicates" instead of a loud
    // "your kind argument is invalid". The same `kinds.allowed`
    // list every other surface enforces is the authority.
    if let Some(kind) = args.kind.as_deref()
        && !config.kinds.allowed.iter().any(|k| k == kind)
    {
        return Err(nodex_core::error::Error::Config(format!(
            "--kind {kind:?} is not in kinds.allowed ({:?}); pick a known kind or omit the flag",
            config.kinds.allowed
        ))
        .into());
    }

    let opts = SimilarityOptions {
        threshold: args.threshold.unwrap_or(config.similarity.threshold),
        limit: args.limit.unwrap_or(config.similarity.default_limit),
    };

    let items = match (args.id.as_deref(), args.title.as_deref()) {
        (Some(id), _) => nodex_core::query::similar::compute_similarity(
            &graph,
            &config,
            &SimilarityTarget::Node(id),
            &opts,
        )?,
        (None, Some(title)) => {
            let target = SimilarityTarget::Spec {
                title,
                kind: args.kind.as_deref(),
                tags: &args.tags,
                parent_dir: args.parent_dir.as_deref(),
            };
            nodex_core::query::similar::compute_similarity(&graph, &config, &target, &opts)?
        }
        (None, None) => {
            anyhow::bail!("either --id or --title must be supplied");
        }
    };

    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_recent(root: &Path, args: RecentArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // Vocabulary fail-loud — same discipline `run_nodes`,
    // `run_low_trust`, `run_similar`, `run_dependents` apply.
    if let Some(k) = &args.kind {
        reject_unknown_vocabulary("--kind", std::slice::from_ref(k), &config.kinds.allowed)?;
    }
    let graph = load_graph(root, &config)?;

    let since = match args.since {
        Some(d) => RecencySince::Date(d),
        None => RecencySince::Days(args.days),
    };
    let opts = RecencyOptions {
        since,
        kind: args.kind,
        field: args.field.into(),
        limit: Some(args.limit),
    };
    let items = nodex_core::query::recent::find_recent(&graph, &opts);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_components(root: &Path, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::structure::find_components(&graph);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

fn run_neighborhood(root: &Path, id: &str, depth: u32, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let result = nodex_core::query::structure::find_neighborhood(&graph, id, depth)?;
    print_json(&Envelope::success(result), pretty);
    Ok(())
}

fn run_dependents(
    root: &Path,
    id: &str,
    depth: Option<u32>,
    relations: Vec<String>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // Validate `--relations` against the project's known set before
    // touching the graph, so a typo (`--relations implments`) returns
    // a typed CONFIG_ERROR instead of an empty result that would read
    // as "nothing depends on this".
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

fn run_annotations(
    root: &Path,
    name: Option<&str>,
    with_frontmatter: Vec<String>,
    min_count: usize,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // Validate the filter eagerly so a typo (`--name promtes`) surfaces
    // as a typed error instead of an empty "no markers" result — same
    // discipline as `query similar`'s `--kind` check.
    if let Some(filter) = name
        && !config.annotations.iter().any(|a| a.name == filter)
    {
        let known: Vec<&str> = config.annotations.iter().map(|a| a.name.as_str()).collect();
        return Err(nodex_core::error::Error::Config(format!(
            "--name {filter:?} is not a declared annotation pattern; known: {known:?}"
        ))
        .into());
    }
    // `--min-count 0` would be a no-op (every count ≥ 0), but the
    // boundary case is operator-confusing — surface it as an explicit
    // typed error instead of accepting it silently. Authors who want
    // "every entry" omit the flag (default 1).
    if min_count == 0 {
        return Err(nodex_core::error::Error::Config(
            "--min-count must be ≥ 1; omit the flag to keep every entry".into(),
        )
        .into());
    }
    // Validate `--with-frontmatter <fields>` against the project's
    // declared field universe. An unknown name would otherwise silently
    // produce empty entries; same fail-loud discipline `--relations`
    // / `--kind` apply.
    if !with_frontmatter.is_empty() {
        let universe = config.declared_fields_universe();
        let unknown: Vec<&str> = with_frontmatter
            .iter()
            .filter(|f| !universe.contains(f.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let mut known_sorted: Vec<&str> = universe.iter().map(String::as_str).collect();
            known_sorted.sort_unstable();
            return Err(nodex_core::error::Error::Config(format!(
                "--with-frontmatter contains unknown field(s) {unknown:?}; declared: {known_sorted:?}"
            ))
            .into());
        }
    }
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::annotations::find_annotations(
        &graph,
        &nodex_core::AnnotationOptions {
            pattern: name,
            with_frontmatter: &with_frontmatter,
            min_count,
        },
    );
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
