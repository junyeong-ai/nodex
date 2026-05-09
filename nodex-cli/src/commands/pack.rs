use anyhow::{Context, Result};
use clap::Args;
use std::path::Path;

use nodex_core::{builder, query::pack};

use crate::format::{Envelope, print_json};

/// Args for `nodex pack`.
#[derive(Args)]
pub struct PackArgs {
    /// Seed node id.
    pub id: String,
    /// Maximum total tokens to include
    /// (default: [`nodex_core::query::pack::DEFAULT_TOKEN_BUDGET`]).
    #[arg(long)]
    pub token_budget: Option<usize>,
    /// Maximum BFS depth from the seed
    /// (default: [`nodex_core::query::pack::DEFAULT_MAX_DEPTH`]).
    #[arg(long)]
    pub depth: Option<u32>,
}

pub fn run(root: &Path, args: PackArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let result = builder::build(root, &config, false).context("graph build failed")?;

    let bundle = pack::build_pack(
        &result.graph,
        &config,
        root,
        &args.id,
        args.token_budget.unwrap_or(pack::DEFAULT_TOKEN_BUDGET),
        args.depth.unwrap_or(pack::DEFAULT_MAX_DEPTH),
    )?;

    print_json(&Envelope::success(bundle), pretty);
    Ok(())
}
