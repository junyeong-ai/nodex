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

    /// The skill's entry point is what a session carries, and a compacted
    /// session carries only the first 5,000 tokens of it — so a body that
    /// outgrows that budget does not merely cost more, it silently loses its
    /// tail, and the tail is where the vocabularies and workflows sit. The
    /// detail lives in bundled references instead, read on demand and never
    /// competing for the budget.
    ///
    /// Both directions of that pointing are asserted, because they fail
    /// differently: a bundled file nothing names is one nothing will open,
    /// and a name pointing at no file costs a reader the Read it took to
    /// find out. The second is the one an author produces by renaming.
    #[test]
    fn the_skill_body_fits_what_a_compacted_session_carries() {
        /// Every file the skill bundles lives here, so the prefix a reader
        /// follows is also the one the check reads.
        const BUNDLE: &str = "reference/";

        let dir = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.claude/skills/nodex"
        ));
        let skill = std::fs::read_to_string(dir.join("SKILL.md"))
            .expect("the packaged skill is part of the repository");

        // The whole file, not the body: what a compacted session re-attaches
        // is the message the skill arrived in, frontmatter included. Four
        // chars per token slightly *over*-counts English prose, so the assert
        // fires a little before the real budget rather than after it.
        const BUDGET_CHARS: usize = 5_000 * 4;
        assert!(
            skill.len() <= BUDGET_CHARS,
            "SKILL.md is {} chars (~{} tokens); compaction re-attaches only the first 5,000 \
             tokens, so everything past that is dropped. Move detail into reference/.",
            skill.len(),
            skill.len() / 4
        );

        // The frontmatter is what decides whether the skill is offered at
        // all: a document a YAML parser rejects loads with empty metadata,
        // so the skill keeps working under `/nodex` and silently stops being
        // matched. Parsed here rather than scanned, because the ways to
        // break YAML are not a list anyone maintains — an unquoted `: ` in a
        // plain scalar is one, and it is invisible to every check that reads
        // the file as text.
        let frontmatter = skill
            .split("---")
            .nth(1)
            .expect("SKILL.md opens with a YAML frontmatter block");
        let parsed: yaml_serde::Value =
            yaml_serde::from_str(frontmatter).expect("SKILL.md frontmatter must parse as YAML");
        // The Agent Skills spec's whole vocabulary. Claude Code accepts more,
        // but a key outside this set is what claude.ai uploads, the Skills
        // API and `package_skill.py` reject the file over — and a rejected
        // skill is one nobody notices is gone. A closed set is the only
        // reading that catches a key nobody has thought of yet, which is
        // exactly the shape the one that shipped here had.
        const SPEC_FIELDS: [&str; 6] = [
            "name",
            "description",
            "license",
            "compatibility",
            "metadata",
            "allowed-tools",
        ];
        let declared: BTreeSet<&str> = parsed
            .as_mapping()
            .expect("SKILL.md frontmatter is a YAML mapping")
            .keys()
            .filter_map(yaml_serde::Value::as_str)
            .collect();
        let unexpected: Vec<&&str> = declared
            .iter()
            .filter(|key| !SPEC_FIELDS.contains(key))
            .collect();
        assert!(
            unexpected.is_empty(),
            "SKILL.md frontmatter declares {unexpected:?}, outside the Agent Skills spec \
             {SPEC_FIELDS:?}; a packaging path that validates against the spec rejects the file"
        );

        let description = parsed
            .get("description")
            .and_then(yaml_serde::Value::as_str)
            .expect("the skill declares a description");
        // `description` and `when_to_use` share one listing budget; this
        // skill folds both into `description`, so the whole of it counts.
        assert!(
            description.len() <= 1_536,
            "description is {} chars; the skill listing truncates at 1,536",
            description.len()
        );
        // The release refuses a mismatch, which is one push too late: this is
        // the file a bump is easiest to forget, and `check.sh` is what runs
        // before the push.
        assert_eq!(
            parsed
                .get("metadata")
                .and_then(|m| m.get("version"))
                .and_then(yaml_serde::Value::as_str),
            Some(env!("CARGO_PKG_VERSION")),
            "SKILL.md metadata.version drifted from the crate version"
        );

        // Everything the skill bundles lives under one prefix, so one
        // reading answers both directions: what the directory holds against
        // what the prose names. A second namespace would need a second
        // mechanism, and the two would disagree about exactly the file
        // neither was written for.
        let mut bundled: BTreeSet<String> = BTreeSet::new();
        let mut stack = vec![dir.join(BUNDLE)];
        while let Some(next) = stack.pop() {
            for entry in std::fs::read_dir(&next).expect("the skill bundles a reference directory")
            {
                let path = entry.expect("a readable directory entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                bundled.insert(
                    path.strip_prefix(dir)
                        .expect("every bundled file sits under the skill directory")
                        .to_str()
                        .expect("a UTF-8 file name")
                        .replace('\\', "/"),
                );
            }
        }
        assert_eq!(
            std::fs::read_dir(dir)
                .expect("the skill directory is readable")
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_file())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["SKILL.md".to_string()]),
            "the skill's only loose file is its entry point; a bundle lives under {BUNDLE}/ so one \
             reading of the prose answers for all of them"
        );

        // Read off the spelling a reader would follow, to its own end: a
        // prefix match would let `reference/config.md` in the prose answer
        // for a bundled `reference/config.md.bak` nothing names.
        let named: BTreeSet<String> = skill
            .match_indices(BUNDLE)
            .map(|(at, _)| {
                let rest = &skill[at..];
                let end = rest
                    .find(|c: char| !(c.is_alphanumeric() || "._/-".contains(c)))
                    .unwrap_or(rest.len());
                rest[..end].to_string()
            })
            .collect();

        assert_eq!(
            bundled, named,
            "SKILL.md and {BUNDLE}/ disagree: a bundled file the prose never names is one nothing \
             opens, and a name that resolves to no file costs a reader the Read that discovers it"
        );
    }

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
    /// The haystack is the published list itself, delimited in the skill by
    /// `<!-- published:… -->`. Any wider reading answers with prose: asked of
    /// the whole file, the question was satisfied by a sentence elsewhere —
    /// which is how `GRAPH_OUTDATED` stayed off the error-code list while the
    /// test passed — and asked of the enclosing section it is satisfied by the
    /// paragraphs that explain the very codes the list is supposed to
    /// enumerate. The list is what presents itself as complete, so the list is
    /// what must be.
    #[test]
    fn the_skill_names_every_published_vocabulary() {
        let skill = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.claude/skills/nodex/SKILL.md"
        ))
        .expect("the packaged skill is part of the repository");
        let published = |name: &str| {
            let open = format!("<!-- published:{name} -->");
            let close = format!("<!-- /published:{name} -->");
            let rest = skill
                .split_once(&open)
                .unwrap_or_else(|| panic!("SKILL.md must delimit the published {name} with {open}"))
                .1;
            rest.split_once(&close)
                .unwrap_or_else(|| panic!("SKILL.md must close the published {name} with {close}"))
                .0
                .to_string()
        };
        let error_codes = published("error-codes");
        let warning_codes = published("warning-codes");

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

    /// Exhaustiveness guard, mirroring core's
    /// `rules_manifest_mirrors_registered_rules_exactly`: the commands
    /// manifest is derived from the same clap tree the binary parses,
    /// and the envelope-schema registry must biject with it — so the
    /// grammar manifest, the clap tree, and the typed-codegen contract
    /// are provably in lockstep. A new leaf cannot ship without its
    /// schema; a removed leaf cannot leave a stale entry; a flag-mode
    /// declared in `FLAG_MODES` must name real flags on its leaf.
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
