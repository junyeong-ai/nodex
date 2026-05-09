use anyhow::Result;
use clap::Args;
use std::path::Path;

use nodex_core::query::trust;

use super::query::load_graph;
use crate::format::{Envelope, print_json};

/// Args for `nodex trust`.
#[derive(Args)]
pub struct TrustArgs {
    /// Node id to score.
    pub id: String,
}

pub fn run(root: &Path, args: TrustArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let report = trust::trust_of(&graph, &config, root, &args.id)?;
    print_json(&Envelope::success(report), pretty);
    Ok(())
}
