mod commands;
mod format;

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "nodex", about = "Universal graph-based document tool", version)]
struct Cli {
    /// Run as if started in DIR
    #[arg(short = 'C', global = true)]
    dir: Option<PathBuf>,

    /// Pretty-print JSON output
    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a nodex.toml in current directory
    Init,

    /// Parse all in-scope docs and build the graph
    Build {
        /// Force full rebuild (ignore cache)
        #[arg(long)]
        full: bool,
    },

    /// Search and explore the graph
    Query {
        #[command(subcommand)]
        sub: QueryCommand,
    },

    /// Run validation rules
    Check {
        /// Filter by severity: error, warning, or all
        #[arg(long)]
        severity: Option<String>,
    },

    /// Manage document lifecycle
    Lifecycle {
        /// Action to perform
        action: LifecycleAction,
        /// Target node ID
        id: String,
        /// Successor node ID (required for supersede)
        #[arg(long)]
        to: Option<String>,
    },

    /// Generate reports
    Report {
        /// Output format: md, json, or all
        #[arg(long, default_value = "all")]
        format: String,
    },

    /// Inject missing frontmatter into legacy docs
    Migrate {
        /// Actually write files (default: dry-run)
        #[arg(long)]
        apply: bool,
    },

    /// Move file and update references
    Rename {
        /// Source path (relative to root)
        old: String,
        /// Target path (relative to root)
        new: String,
    },

    /// Create a new document node with valid frontmatter
    Scaffold {
        /// Document kind (must be in config.kinds.allowed)
        #[arg(long)]
        kind: String,
        /// Document title (free-form; also used to slugify the filename)
        #[arg(long)]
        title: String,
        /// Override the auto-inferred node id
        #[arg(long)]
        id: Option<String>,
        /// Override the auto-inferred path (relative to root)
        #[arg(long)]
        path: Option<PathBuf>,
        /// Print the plan as JSON without writing the file
        #[arg(long)]
        dry_run: bool,
        /// Overwrite existing file at the target path
        #[arg(long)]
        force: bool,
    },
}

/// Lifecycle actions validated at parse time by clap.
#[derive(Clone, ValueEnum)]
enum LifecycleAction {
    Supersede,
    Archive,
    Deprecate,
    Abandon,
    Review,
}

impl LifecycleAction {
    fn as_str(&self) -> &str {
        match self {
            Self::Supersede => "supersede",
            Self::Archive => "archive",
            Self::Deprecate => "deprecate",
            Self::Abandon => "abandon",
            Self::Review => "review",
        }
    }
}

#[derive(Subcommand)]
enum QueryCommand {
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

fn main() {
    let cli = Cli::parse();
    let root = match cli.dir.or_else(|| std::env::current_dir().ok()) {
        Some(p) => p,
        None => {
            eprintln!(
                "{{\"ok\":false,\"error\":{{\"code\":\"IO_ERROR\",\"message\":\"cannot determine current directory\"}}}}"
            );
            std::process::exit(2);
        }
    };
    let pretty = cli.pretty;

    let result = match cli.command {
        Command::Init => commands::init::run(&root, pretty),
        Command::Build { full } => commands::build::run(&root, full, pretty),
        Command::Query { sub } => match sub {
            QueryCommand::Search { keyword, status } => {
                let statuses = status.map(|s| s.split(',').map(|s| s.trim().to_string()).collect());
                commands::query::run_search(&root, &keyword, statuses, pretty)
            }
            QueryCommand::Backlinks { id } => commands::query::run_backlinks(&root, &id, pretty),
            QueryCommand::Chain { id } => commands::query::run_chain(&root, &id, pretty),
            QueryCommand::Orphans => commands::query::run_orphans(&root, pretty),
            QueryCommand::Stale => commands::query::run_stale(&root, pretty),
            QueryCommand::Tags { tags, all } => commands::query::run_tags(&root, tags, all, pretty),
            QueryCommand::Node { id } => commands::query::run_node(&root, &id, pretty),
            QueryCommand::Issues => commands::query::run_issues(&root, pretty),
        },
        Command::Check { severity } => commands::check::run(&root, severity, pretty),
        Command::Lifecycle { action, id, to } => {
            commands::lifecycle::run(&root, action.as_str(), &id, to.as_deref(), pretty)
        }
        Command::Report { format } => commands::report::run(&root, Some(format), pretty),
        Command::Migrate { apply } => commands::migrate::run(&root, apply, pretty),
        Command::Rename { old, new } => commands::rename::run(&root, &old, &new, pretty),
        Command::Scaffold {
            kind,
            title,
            id,
            path,
            dry_run,
            force,
        } => commands::scaffold::run(&root, &kind, &title, id, path, dry_run, force, pretty),
    };

    if let Err(err) = result {
        let envelope = format::ErrorEnvelope::from_error(&err);
        format::print_json(&envelope, pretty);
        std::process::exit(2);
    }
}
