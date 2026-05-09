mod commands;
mod format;

use clap::{Parser, Subcommand};
use commands::build::BuildArgs;
use commands::check::CheckArgs;
use commands::continue_cmd::ContinueArgs;
use commands::lifecycle::LifecycleCommand;
use commands::log::LogArgs;
use commands::migrate::MigrateArgs;
use commands::pack::PackArgs;
use commands::query::QueryCommand;
use commands::recent::RecentArgs;
use commands::rename::RenameArgs;
use commands::report::ReportArgs;
use commands::scaffold::ScaffoldArgs;
use commands::similar::SimilarArgs;
use commands::trust::TrustArgs;
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

/// Top-level subcommand. Each variant is a single-argument forward
/// to the matching `commands::<name>::run` — every command's CLI
/// shape lives in its own file (`nodex-cli/CLAUDE.md` rule
/// "main.rs never contains a command's CLI shape"), so this enum
/// stays a thin dispatch table.
#[derive(Subcommand)]
enum Command {
    /// Create a nodex.toml in current directory
    Init,
    /// Parse all in-scope docs and build the graph
    Build(BuildArgs),
    /// Search and explore the graph
    Query {
        #[command(subcommand)]
        sub: QueryCommand,
    },
    /// Run validation rules
    Check(CheckArgs),
    /// Manage document lifecycle
    Lifecycle {
        #[command(subcommand)]
        sub: LifecycleCommand,
    },
    /// Generate reports
    Report(ReportArgs),
    /// Inject missing frontmatter into legacy docs
    Migrate(MigrateArgs),
    /// Move file and update references
    Rename(RenameArgs),
    /// Create a new document node with valid frontmatter
    Scaffold(ScaffoldArgs),
    /// List documents whose configured date field falls inside a recent window
    Recent(RecentArgs),
    /// Append an event to the current (or a named) session log
    Log(LogArgs),
    /// Resume context from the most recent session log
    Continue(ContinueArgs),
    /// Composite reliability score for a single document
    Trust(TrustArgs),
    /// Find documents similar to an existing node or a prospective one
    Similar(SimilarArgs),
    /// Build a token-budgeted context pack rooted at the given node
    Pack(PackArgs),
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
        Command::Build(args) => commands::build::run(&root, args, pretty),
        Command::Query { sub } => commands::query::run(&root, sub, pretty),
        Command::Check(args) => commands::check::run(&root, args, pretty),
        Command::Lifecycle { sub } => commands::lifecycle::run(&root, sub, pretty),
        Command::Report(args) => commands::report::run(&root, args, pretty),
        Command::Migrate(args) => commands::migrate::run(&root, args, pretty),
        Command::Rename(args) => commands::rename::run(&root, args, pretty),
        Command::Scaffold(args) => commands::scaffold::run(&root, args, pretty),
        Command::Recent(args) => commands::recent::run(&root, args, pretty),
        Command::Log(args) => commands::log::run(&root, args, pretty),
        Command::Continue(args) => commands::continue_cmd::run(&root, args, pretty),
        Command::Trust(args) => commands::trust::run(&root, args, pretty),
        Command::Similar(args) => commands::similar::run(&root, args, pretty),
        Command::Pack(args) => commands::pack::run(&root, args, pretty),
    };

    if let Err(err) = result {
        let envelope = format::ErrorEnvelope::from_error(&err);
        format::print_json(&envelope, pretty);
        std::process::exit(2);
    }
}
