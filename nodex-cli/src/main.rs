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
        /// Filter by severity
        #[arg(long, value_enum)]
        severity: Option<CheckSeverity>,
    },

    /// Manage document lifecycle
    Lifecycle {
        #[command(subcommand)]
        sub: LifecycleCommand,
    },

    /// Generate reports
    Report {
        /// Output format
        #[arg(long, value_enum, default_value_t = ReportFormat::All)]
        format: ReportFormat,
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

/// Severity filter accepted by `nodex check`. Mapped to
/// [`nodex_core::rules::Severity`] at the command boundary.
#[derive(Clone, Copy, ValueEnum)]
enum CheckSeverity {
    Error,
    Warning,
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
    Md,
    Json,
    All,
}

impl ReportFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Md => "md",
            Self::Json => "json",
            Self::All => "all",
        }
    }
}

/// Lifecycle subcommands. Each variant carries exactly the arguments
/// its action needs, so clap enforces at parse time — `supersede`
/// cannot be invoked without `--to`, and the other actions cannot
/// receive a stray `--to`.
#[derive(Subcommand)]
enum LifecycleCommand {
    /// Mark a node superseded by another
    Supersede {
        id: String,
        /// Successor node ID
        #[arg(long)]
        to: String,
    },
    /// Archive a node
    Archive { id: String },
    /// Mark a node deprecated
    Deprecate { id: String },
    /// Mark a node abandoned
    Abandon { id: String },
    /// Refresh the reviewed date on a node
    Review { id: String },
}

impl LifecycleCommand {
    /// Extract the (action_str, id, optional_successor) tuple that the
    /// core lifecycle API expects. Centralised so `main()` has a single
    /// match arm for the whole lifecycle family.
    fn parts(&self) -> (&'static str, &str, Option<&str>) {
        match self {
            Self::Supersede { id, to } => ("supersede", id, Some(to.as_str())),
            Self::Archive { id } => ("archive", id, None),
            Self::Deprecate { id } => ("deprecate", id, None),
            Self::Abandon { id } => ("abandon", id, None),
            Self::Review { id } => ("review", id, None),
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
    // Parse into our JSON envelope on any clap error except the
    // informational --help / --version / "help <subcommand>" paths,
    // which remain human-readable per CLI convention.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => match err.kind() {
            clap::error::ErrorKind::DisplayHelp
            | clap::error::ErrorKind::DisplayVersion
            | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                err.exit();
            }
            _ => {
                let envelope = format::ErrorEnvelope::from_clap_error(&err);
                format::print_json(&envelope, false);
                std::process::exit(2);
            }
        },
    };

    let root = match cli.dir.or_else(|| std::env::current_dir().ok()) {
        Some(p) => p,
        None => {
            let err = nodex_core::error::Error::Io {
                path: std::path::PathBuf::new(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "cannot determine current directory",
                ),
            };
            let anyhow_err: anyhow::Error = err.into();
            let envelope = format::ErrorEnvelope::from_error(&anyhow_err);
            format::print_json(&envelope, false);
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
        Command::Check { severity } => commands::check::run(&root, severity.map(Into::into), pretty),
        Command::Lifecycle { sub } => {
            let (action, id, to) = sub.parts();
            commands::lifecycle::run(&root, action, id, to, pretty)
        }
        Command::Report { format: fmt } => {
            commands::report::run(&root, Some(fmt.as_str().to_string()), pretty)
        }
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

impl From<CheckSeverity> for nodex_core::rules::Severity {
    fn from(s: CheckSeverity) -> Self {
        match s {
            CheckSeverity::Error => Self::Error,
            CheckSeverity::Warning => Self::Warning,
        }
    }
}
