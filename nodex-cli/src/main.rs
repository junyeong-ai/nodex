mod commands;
mod format;

use clap::{Parser, Subcommand};
use commands::check::CheckSeverity;
use commands::lifecycle::LifecycleCommand;
use commands::query::QueryCommand;
use commands::report::ReportFormat;
use commands::scaffold::ScaffoldArgs;
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
    Scaffold(ScaffoldArgs),
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
        Command::Query { sub } => commands::query::run(&root, sub, pretty),
        Command::Check { severity } => commands::check::run(&root, severity, pretty),
        Command::Lifecycle { sub } => commands::lifecycle::run(&root, sub, pretty),
        Command::Report { format } => commands::report::run(&root, format, pretty),
        Command::Migrate { apply } => commands::migrate::run(&root, apply, pretty),
        Command::Rename { old, new } => commands::rename::run(&root, &old, &new, pretty),
        Command::Scaffold(args) => commands::scaffold::run(&root, args, pretty),
    };

    if let Err(err) = result {
        let envelope = format::ErrorEnvelope::from_error(&err);
        format::print_json(&envelope, pretty);
        std::process::exit(2);
    }
}
