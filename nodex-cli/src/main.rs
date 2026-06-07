mod commands;
mod format;

use clap::{Parser, Subcommand};
use commands::build::BuildArgs;
use commands::check::CheckArgs;
use commands::diff::DiffArgs;
use commands::export::ExportCommand;
use commands::impact::ImpactArgs;
use commands::lifecycle::LifecycleCommand;
use commands::migrate::MigrateArgs;
use commands::query::QueryCommand;
use commands::rename::RenameArgs;
use commands::report::ReportArgs;
use commands::retarget::RetargetArgs;
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
    /// What depends on what a diff changed: removed/modified nodes paired with their transitive dependents
    Impact(ImpactArgs),
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
    /// Repoint references from one node id to another (e.g. after a supersession)
    Retarget(RetargetArgs),
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
            // Only an explicit `--help` / `--version` prints human text
            // (exit 0). Every other clap error — including a missing
            // required subcommand or argument — is a failure that must
            // emit the JSON error envelope, so a JSON-first consumer
            // never gets bare help text on stderr with empty stdout.
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
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
        Command::Impact(args) => commands::impact::run(&root, args, pretty),
        Command::Query { sub } => commands::query::run(&root, sub, pretty),
        Command::Check(args) => commands::check::run(&root, args, pretty),
        Command::Lifecycle { sub } => commands::lifecycle::run(&root, sub, pretty),
        Command::Report(args) => commands::report::run(&root, args, pretty),
        Command::Migrate(args) => commands::migrate::run(&root, args, pretty),
        Command::Rename(args) => commands::rename::run(&root, args, pretty),
        Command::Retarget(args) => commands::retarget::run(&root, args, pretty),
        Command::Scaffold(args) => commands::scaffold::run(&root, args, pretty),
        Command::Export { sub } => commands::export::run(&root, sub, pretty),
    };

    if let Err(err) = result {
        let envelope = format::ErrorEnvelope::from_error(&err);
        format::print_json(&envelope, pretty);
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// `per_command` envelope-schema keys that intentionally have no 1:1
    /// CLI leaf — a second response shape an existing command emits.
    /// Each must stay justified; adding one is a deliberate decision.
    const SYNTHETIC_PER_COMMAND_KEYS: &[&str] = &[
        // `query trust --top/--bottom` returns a list shape distinct
        // from the single-id `query trust <id>` (key `query.trust`).
        "query.trust-list",
    ];

    /// Collect every leaf subcommand of `cmd` as a dotted path
    /// (`query.trust`, `lifecycle.set`, `build`). A leaf has no further
    /// subcommands; clap's auto-generated `help` is skipped.
    fn collect_leaf_commands(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if name == "help" {
                continue;
            }
            let dotted = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}.{name}")
            };
            if sub.get_subcommands().next().is_none() {
                out.push(dotted);
            } else {
                collect_leaf_commands(sub, &dotted, out);
            }
        }
    }

    /// Exhaustiveness guard, mirroring core's
    /// `rules_manifest_mirrors_registered_rules_exactly`: a new leaf
    /// subcommand cannot ship without its typed-codegen envelope schema.
    /// The clap tree is the single source of truth for the command
    /// surface; `export envelope-schema` is the codegen contract — this
    /// proves the latter covers the former, and keeps every synthetic
    /// key (no CLI leaf) honest.
    #[test]
    fn every_cli_leaf_has_a_per_command_schema() {
        let mut leaves = Vec::new();
        collect_leaf_commands(&Cli::command(), "", &mut leaves);
        assert!(!leaves.is_empty(), "clap walk found no leaf commands");

        let manifest = nodex_core::export_envelope_schema();
        for leaf in &leaves {
            assert!(
                manifest.per_command.contains_key(leaf),
                "CLI command `{}` has no per_command envelope schema; register it in \
                 nodex_core::export::per_command_schemas",
                leaf.replace('.', " ")
            );
        }

        for synthetic in SYNTHETIC_PER_COMMAND_KEYS {
            assert!(
                manifest.per_command.contains_key(*synthetic),
                "synthetic per_command key `{synthetic}` is gone; drop it from \
                 SYNTHETIC_PER_COMMAND_KEYS"
            );
            assert!(
                !leaves.iter().any(|l| l == synthetic),
                "`{synthetic}` is now a real CLI leaf; drop it from SYNTHETIC_PER_COMMAND_KEYS"
            );
        }

        // And the reverse: every per_command key is a real CLI leaf or a
        // declared synthetic, so a removed command can never leave a
        // stale schema entry behind in the codegen contract.
        for key in manifest.per_command.keys() {
            assert!(
                leaves.iter().any(|l| l == key)
                    || SYNTHETIC_PER_COMMAND_KEYS.contains(&key.as_str()),
                "per_command key `{key}` matches no CLI leaf and is not a declared synthetic; \
                 remove it from nodex_core::export::per_command_schemas or register the command"
            );
        }
    }
}
