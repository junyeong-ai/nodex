mod commands;
mod format;

use clap::{Parser, Subcommand};
use commands::build::BuildArgs;
use commands::check::CheckArgs;
use commands::diff::DiffArgs;
use commands::export::ExportCommand;
use commands::lifecycle::LifecycleCommand;
use commands::migrate::MigrateArgs;
use commands::query::QueryCommand;
use commands::rename::RenameArgs;
use commands::report::ReportArgs;
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

    /// Refuse to run unless the binary version satisfies the SemVer
    /// requirement (e.g. `0.5`, `>=0.5,<0.6`). CI sets this to pin
    /// the installed binary.
    #[arg(long, global = true, value_name = "REQ")]
    check_version: Option<String>,

    #[command(subcommand)]
    command: Command,
}

/// Top-level subcommand. Each variant is a single-argument forward to
/// the matching `commands::<name>::run` — every command's CLI shape
/// lives in its own file (`nodex-cli/CLAUDE.md` rule "main.rs never
/// contains a command's CLI shape"), so this enum stays a thin
/// dispatch table.
#[derive(Subcommand)]
enum Command {
    /// Create a nodex.toml in current directory
    Init,
    /// Parse all in-scope docs and build the graph
    Build(BuildArgs),
    /// Structural diff between two git refs (added/removed nodes & edges, status transitions, field changes)
    Diff(DiffArgs),
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
    /// Emit authoritative manifests of the project's schema / enums / rules
    Export {
        #[command(subcommand)]
        sub: ExportCommand,
    },
}

fn main() {
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

    if let Some(req) = cli.check_version.as_deref()
        && let Err(err) = nodex_core::verify_version(req)
    {
        let anyhow_err: anyhow::Error = err.into();
        let envelope = format::ErrorEnvelope::from_error(&anyhow_err);
        format::print_json(&envelope, pretty);
        std::process::exit(2);
    }

    let result = match cli.command {
        Command::Init => commands::init::run(&root, pretty),
        Command::Build(args) => commands::build::run(&root, args, pretty),
        Command::Diff(args) => commands::diff::run(&root, args, pretty),
        Command::Query { sub } => commands::query::run(&root, sub, pretty),
        Command::Check(args) => commands::check::run(&root, args, pretty),
        Command::Lifecycle { sub } => commands::lifecycle::run(&root, sub, pretty),
        Command::Report(args) => commands::report::run(&root, args, pretty),
        Command::Migrate(args) => commands::migrate::run(&root, args, pretty),
        Command::Rename(args) => commands::rename::run(&root, args, pretty),
        Command::Scaffold(args) => commands::scaffold::run(&root, args, pretty),
        Command::Export { sub } => commands::export::run(&root, sub, pretty),
    };

    if let Err(err) = result {
        let envelope = format::ErrorEnvelope::from_error(&err);
        format::print_json(&envelope, pretty);
        std::process::exit(2);
    }
}
