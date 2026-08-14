mod commands;
mod envelope;
mod format;

use chrono::{Local, NaiveDate};
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
pub(crate) struct Cli {
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

    /// Evaluate date-relative rules and queries as if today were DATE
    /// (`YYYY-MM-DD`), instead of reading the system clock. Staleness,
    /// orphan grace, recency windows, trust freshness, and the dates
    /// written into scaffolded documents are all measured from it, so
    /// pinning it makes a run reproducible: the same graph and the same
    /// date give the same answer on any machine, on any day.
    #[arg(long, global = true, value_name = "DATE")]
    today: Option<NaiveDate>,

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
    /// Report the graph snapshot's state (absent / unreadable / schema_mismatch / outdated / current)
    Status,
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

    // The project root is absolute from here on. `-C <dir>` accepts a
    // relative path, and a relative root would be re-resolved against
    // whatever working directory each consumer happens to run in — git
    // invocations run in the repository's work tree, so a relative
    // scratch path would land a checkout outside the project entirely.
    // Absolute, not canonical: the symlinked route the operator typed is
    // the route their paths and diagnostics should keep.
    let root = match cli
        .dir
        .or_else(|| std::env::current_dir().ok())
        .and_then(|dir| std::path::absolute(dir).ok())
    {
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
    // The one place the process reads a clock. Everything downstream
    // takes the date as an argument, so no library function can reach
    // ambient time and no verdict can change with the calendar alone.
    let today = cli.today.unwrap_or_else(|| Local::now().date_naive());

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
        Command::Status => commands::status::run(&root, pretty),
        Command::Diff(args) => commands::diff::run(&root, args, pretty),
        Command::Impact(args) => commands::impact::run(&root, args, pretty),
        Command::Query { sub } => commands::query::run(&root, sub, pretty, today),
        Command::Check(args) => commands::check::run(&root, args, pretty, today),
        Command::Lifecycle { sub } => commands::lifecycle::run(&root, sub, pretty, today),
        Command::Report(args) => commands::report::run(&root, args, pretty, today),
        Command::Migrate(args) => commands::migrate::run(&root, args, pretty, today),
        Command::Rename(args) => commands::rename::run(&root, args, pretty, today),
        Command::Retarget(args) => commands::retarget::run(&root, args, pretty, today),
        Command::Scaffold(args) => commands::scaffold::run(&root, args, pretty, today),
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
    use std::collections::BTreeSet;

    /// Resolve a manifest entry's `path` tokens to its clap leaf.
    fn find_leaf<'a>(mut cmd: &'a clap::Command, path: &[String]) -> &'a clap::Command {
        for token in path {
            cmd = cmd
                .get_subcommands()
                .find(|sub| sub.get_name() == *token)
                .unwrap_or_else(|| panic!("manifest path {path:?} names no clap subcommand"));
        }
        cmd
    }

    /// Exhaustiveness guard, mirroring core's
    /// `rules_manifest_mirrors_registered_rules_exactly`: the commands
    /// manifest is derived from the same clap tree the binary parses,
    /// and the envelope-schema registry must biject with it — so the
    /// grammar manifest, the clap tree, and the typed-codegen contract
    /// are provably in lockstep. A new leaf cannot ship without its
    /// schema; a removed leaf cannot leave a stale entry; a flag-mode
    /// declared in `FLAG_MODES` must name real flags on its leaf.
    /// The shipped skill names every vocabulary the binary publishes.
    ///
    /// `SKILL.md` tells its reader the binary is the source of truth and
    /// points at the generated manifests; this is that instruction enforced,
    /// so the pointer cannot outlive the prose it qualifies. The frontmatter
    /// version guards the other axis — which release the file describes — and
    /// cannot see this one: a code added and documented between releases
    /// leaves both versions equal and the file wrong, which is the ordinary
    /// state of an edit and exactly what a release must not ship.
    ///
    /// Presence, not explanation. These are closed generated sets of exact
    /// identifiers, and a consumer branches on them, so a name the skill
    /// never mentions is one an agent reading the skill cannot know exists.
    /// What each means is prose, and prose is reviewed, not asserted.
    ///
    /// A vocabulary is looked for in the section that publishes it, not in the
    /// file. Asked of the whole file, the question is satisfied by any prose
    /// that happens to spell the name — which is how `GRAPH_OUTDATED` stayed
    /// absent from the error-code list while the test passed, mentioned once
    /// in a paragraph about stale snapshots. A reader consults the list.
    #[test]
    fn the_skill_names_every_published_vocabulary() {
        let skill = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.claude/skills/nodex/SKILL.md"
        ))
        .expect("the packaged skill is part of the repository");
        let section = |heading: &str| {
            skill
                .split_once(heading)
                .unwrap_or_else(|| panic!("SKILL.md must carry a {heading:?} section"))
                .1
                .split("\n## ")
                .next()
                .expect("a split always yields a first part")
                .to_string()
        };
        let error_codes = section("\n## Error codes");
        let warning_codes = section("\n## Warning codes");

        let diagnostics = nodex_core::export::export_diagnostics();
        let mut missing: Vec<String> = Vec::new();
        for code in &diagnostics.error_codes {
            if !error_codes.contains(&code.code) {
                missing.push(format!("error code {}", code.code));
            }
        }
        for code in &diagnostics.warning_codes {
            if !warning_codes.contains(code) {
                missing.push(format!("warning code {code}"));
            }
        }
        for entry in &commands::export::commands_manifest().commands {
            let invocation = format!("nodex {}", entry.path.join(" "));
            if !skill.contains(&invocation) {
                missing.push(invocation);
            }
        }
        assert!(
            missing.is_empty(),
            "SKILL.md does not name: {}",
            missing.join(", ")
        );
    }

    #[test]
    fn every_cli_leaf_has_a_per_command_schema() {
        let manifest = commands::export::commands_manifest();
        assert!(
            !manifest.commands.is_empty(),
            "clap walk found no leaf commands"
        );

        // Each entry's schema key is exactly its dotted invocation path.
        for entry in &manifest.commands {
            assert_eq!(
                entry.schema,
                entry.path.join("."),
                "schema key must be the dotted path of {:?}",
                entry.path
            );
        }

        // Exact set equality, both directions: every declared schema
        // (leaf or flag-mode) is registered, and every registered
        // schema is declared by the grammar.
        let mut declared: BTreeSet<String> = manifest
            .commands
            .iter()
            .map(|entry| entry.schema.clone())
            .collect();
        for entry in &manifest.commands {
            for mode in &entry.modes {
                declared.insert(mode.schema.clone());
            }
        }
        let envelope = nodex_core::export_envelope_schema(false)
            .expect("the default emission form performs no inlining");
        let registered: BTreeSet<String> = envelope.per_command.keys().cloned().collect();
        assert_eq!(
            declared, registered,
            "commands manifest and per_command registry must biject; register new leaves in \
             nodex_core::export::per_command_schemas and flag-selected shapes in \
             commands::export::FLAG_MODES"
        );

        // Every declared mode flag exists as a real clap arg on its leaf.
        let cli = Cli::command();
        for entry in &manifest.commands {
            if entry.modes.is_empty() {
                continue;
            }
            let leaf = find_leaf(&cli, &entry.path);
            for mode in &entry.modes {
                for flag in &mode.flags {
                    assert!(
                        leaf.get_arguments()
                            .any(|arg| arg.get_long() == Some(flag.as_str())),
                        "FLAG_MODES declares `--{flag}` on `{}` but the leaf has no such flag",
                        entry.schema
                    );
                }
            }
        }
    }
}
