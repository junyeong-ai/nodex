use anyhow::Result;
use clap::Args;
use std::path::Path;

use nodex_core::session::{ContinueOptions, continue_from_last_session};

use crate::format::{Envelope, print_json};

/// Args for `nodex continue`.
#[derive(Args)]
pub struct ContinueArgs {
    /// Lookback window in days (overrides `config.session.default_continue_days`).
    #[arg(long)]
    pub since_days: Option<u32>,
    /// Token budget for the assembled context pack.
    #[arg(long)]
    pub token_budget: Option<usize>,
    /// BFS depth for the pack walk.
    #[arg(long)]
    pub depth: Option<u32>,
}

pub fn run(root: &Path, args: ContinueArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let result = continue_from_last_session(
        root,
        &config,
        ContinueOptions {
            since_days: args.since_days,
            token_budget: args.token_budget,
            max_depth: args.depth,
        },
    )?;
    print_json(&Envelope::success(result), pretty);
    Ok(())
}
