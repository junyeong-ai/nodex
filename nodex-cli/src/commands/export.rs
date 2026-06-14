use anyhow::Result;
use clap::Subcommand;
use std::path::Path;

use nodex_core::export::{CommandManifestEntry, CommandMode, CommandsManifest, PositionalEntry};

use crate::format::{Envelope, emit_read, print_json};

#[derive(Subcommand)]
pub enum ExportCommand {
    /// Emit the project's frontmatter JSON Schema (draft 2020-12)
    Schema,
    /// Emit closed-enum manifest (kinds, statuses, per-field enums)
    Enums,
    /// Emit the active-rules manifest (built-in + config-driven rules)
    Rules,
    /// Emit the JSON Schema of every CLI envelope shape (for codegen)
    EnvelopeSchema {
        /// Emit each per-command schema fully self-contained: every
        /// `#/$defs/...` reference resolved in place, for `$ref`-naive
        /// generators. The default `$defs`-bundled form is the right
        /// input for named-model codegen.
        #[arg(long)]
        inline_refs: bool,
    },
    /// Emit the resolved document-locating surface (scope, output, parser, identity, initial status)
    Config,
    /// Emit the authoritative CLI invocation grammar (leaf paths, positional arity, payload-schema keys)
    Commands,
    /// Emit the error-code and exit-code vocabularies (closed sets, for codegen)
    Diagnostics,
}

/// Flag-selected alternate payload shapes: `(leaf schema key, mode
/// schema key, long flags selecting the mode)`. clap cannot express
/// "these flags switch the leaf to a second payload schema", so the
/// knowledge lives in exactly this table — published through the
/// commands manifest and locked by the
/// `every_cli_leaf_has_a_per_command_schema` bijection test (every
/// flag must exist on its leaf; every schema key must exist in the
/// envelope-schema registry).
const FLAG_MODES: &[(&str, &str, &[&str])] =
    &[("query.trust", "query.trust-list", &["bottom", "top"])];

pub fn run(root: &Path, cmd: ExportCommand, pretty: bool) -> Result<()> {
    // `envelope-schema`, `commands`, and `diagnostics` are pure
    // introspection — none consults `nodex.toml` (the envelope, the CLI
    // grammar, and the error/exit-code vocabulary are the same in every
    // project), so they skip the `load_project` round-trip the
    // config-derived manifests need.
    match cmd {
        ExportCommand::EnvelopeSchema { inline_refs } => {
            let manifest = nodex_core::export::export_envelope_schema(inline_refs)?;
            print_json(&Envelope::success(manifest), pretty);
        }
        ExportCommand::Commands => {
            print_json(&Envelope::success(commands_manifest()), pretty);
        }
        ExportCommand::Diagnostics => {
            // Pure introspection — the error/exit-code vocabulary is the
            // same in every project, so no `load_project` round-trip.
            print_json(
                &Envelope::success(nodex_core::export::export_diagnostics()),
                pretty,
            );
        }
        ExportCommand::Schema => {
            let config = nodex_core::load_project(root)?;
            emit_read(nodex_core::export::export_schema(&config), &config, pretty);
        }
        ExportCommand::Enums => {
            let config = nodex_core::load_project(root)?;
            emit_read(nodex_core::export::export_enums(&config), &config, pretty);
        }
        ExportCommand::Rules => {
            let config = nodex_core::load_project(root)?;
            emit_read(nodex_core::export::export_rules(&config), &config, pretty);
        }
        ExportCommand::Config => {
            let config = nodex_core::load_project(root)?;
            emit_read(nodex_core::export::export_config(&config), &config, pretty);
        }
    }
    Ok(())
}

/// Build the commands manifest from the same clap tree the binary
/// parses (`Cli::command()`), so the published grammar can never drift
/// from the real surface — the bijection test in `main.rs` consumes
/// exactly this manifest.
pub(crate) fn commands_manifest() -> CommandsManifest {
    use clap::CommandFactory;
    let cli = crate::Cli::command();
    let mut commands = Vec::new();
    let mut path = Vec::new();
    collect_leaves(&cli, &mut path, &mut commands);
    CommandsManifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        commands,
    }
}

/// Walk every leaf subcommand (depth-first, clap declaration order). A
/// leaf has no further subcommands; clap's auto-generated `help` is
/// not a contract leaf.
fn collect_leaves(
    cmd: &clap::Command,
    path: &mut Vec<String>,
    out: &mut Vec<CommandManifestEntry>,
) {
    for sub in cmd.get_subcommands() {
        if sub.get_name() == "help" {
            continue;
        }
        path.push(sub.get_name().to_string());
        if sub.get_subcommands().next().is_none() {
            out.push(leaf_entry(sub, path));
        } else {
            collect_leaves(sub, path, out);
        }
        path.pop();
    }
}

fn leaf_entry(leaf: &clap::Command, path: &[String]) -> CommandManifestEntry {
    let schema = path.join(".");
    let modes = FLAG_MODES
        .iter()
        .filter(|(owner, _, _)| *owner == schema)
        .map(|(_, mode_schema, flags)| CommandMode {
            schema: (*mode_schema).to_string(),
            flags: flags.iter().map(|f| (*f).to_string()).collect(),
        })
        .collect();
    let positionals = leaf
        .get_positionals()
        .map(|arg| PositionalEntry {
            name: arg.get_id().to_string(),
            required: arg.is_required_set(),
        })
        .collect();
    CommandManifestEntry {
        path: path.to_vec(),
        schema,
        modes,
        positionals,
    }
}
