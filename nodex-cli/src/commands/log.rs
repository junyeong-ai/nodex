use anyhow::Result;
use clap::Args;
use std::path::Path;

use nodex_core::session::{LogEventSpec, log_event};

use crate::format::{Envelope, print_json};

/// Args for `nodex log`.
#[derive(Args)]
pub struct LogArgs {
    /// One-line narrative — what just happened.
    pub summary: String,
    /// Doc ids the event touched (comma-separated). Unioned with the
    /// session's existing `related` list.
    #[arg(long, value_delimiter = ',')]
    pub related: Vec<String>,
    /// Tags to merge into the session's existing `tags`.
    #[arg(long, value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Append to an existing session id. When omitted, a new session
    /// is created with an auto-generated UTC-stamped id.
    #[arg(long)]
    pub session: Option<String>,
}

pub fn run(root: &Path, args: LogArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let result = log_event(
        root,
        &config,
        LogEventSpec {
            session_id: args.session,
            summary: args.summary,
            related: args.related,
            tags: args.tags,
        },
    )?;
    print_json(&Envelope::success(result), pretty);
    Ok(())
}
