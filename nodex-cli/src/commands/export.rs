use anyhow::Result;
use clap::Subcommand;
use std::path::Path;

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
    EnvelopeSchema,
}

pub fn run(root: &Path, cmd: ExportCommand, pretty: bool) -> Result<()> {
    // Envelope-schema is pure introspection — it does not consult
    // `nodex.toml` (envelope shape is the same in every project), so
    // skip the `load_project` round-trip that the other variants need.
    if matches!(cmd, ExportCommand::EnvelopeSchema) {
        let manifest = nodex_core::export::export_envelope_schema();
        print_json(&Envelope::success(manifest), pretty);
        return Ok(());
    }

    let config = nodex_core::load_project(root)?;
    match cmd {
        ExportCommand::Schema => {
            let manifest = nodex_core::export::export_schema(&config);
            emit_read(manifest, &config, pretty);
        }
        ExportCommand::Enums => {
            let manifest = nodex_core::export::export_enums(&config);
            emit_read(manifest, &config, pretty);
        }
        ExportCommand::Rules => {
            let manifest = nodex_core::export::export_rules(&config);
            emit_read(manifest, &config, pretty);
        }
        ExportCommand::EnvelopeSchema => unreachable!("handled above"),
    }
    Ok(())
}
