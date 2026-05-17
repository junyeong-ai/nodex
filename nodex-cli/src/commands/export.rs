use anyhow::Result;
use clap::Subcommand;
use std::path::Path;

use crate::format::{Envelope, print_json};

#[derive(Subcommand)]
pub enum ExportCommand {
    /// Emit the project's frontmatter JSON Schema (draft 2020-12)
    Schema,
    /// Emit closed-enum manifest (kinds, statuses, per-field enums)
    Enums,
}

pub fn run(root: &Path, cmd: ExportCommand, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    match cmd {
        ExportCommand::Schema => {
            let manifest = nodex_core::export::export_schema(&config);
            print_json(&Envelope::success(manifest), pretty);
        }
        ExportCommand::Enums => {
            let manifest = nodex_core::export::export_enums(&config);
            print_json(&Envelope::success(manifest), pretty);
        }
    }
    Ok(())
}
