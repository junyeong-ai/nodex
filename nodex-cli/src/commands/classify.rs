use anyhow::Result;
use clap::Args;
use std::path::Path;

use crate::format::{ItemsEnvelope, emit_read_with};

#[derive(Args)]
pub struct ClassifyArgs {
    /// Project paths to classify, existing or not, relative to the project root
    #[arg(required = true, value_name = "PATH")]
    pub paths: Vec<String>,
}

/// `nodex classify` — how the configuration reads each path, answered from the
/// scan and the identity rules without building or reading the graph.
pub fn run(root: &Path, args: ClassifyArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let outcome = nodex_core::classify(root, &config, &args.paths)?;
    emit_read_with(
        ItemsEnvelope::new(outcome.items),
        outcome.warnings,
        &config,
        pretty,
    );
    Ok(())
}
