use anyhow::Result;
use chrono::NaiveDate;
use clap::{Args, ValueEnum};
use std::path::Path;

use nodex_core::query::recent::{
    self, DEFAULT_LIMIT, DEFAULT_SINCE_DAYS, RecencyField, RecencyOptions, RecencySince,
};

use super::query::load_graph;
use crate::format::{Envelope, ItemsEnvelope, print_json};

/// Flags for `nodex recent`. Grouped so clap rejects passing both
/// `--since` (absolute) and `--days` (relative) at parse time.
#[derive(Args)]
pub struct RecentArgs {
    /// Absolute cut-off date (YYYY-MM-DD); entries on or after are returned.
    #[arg(long, conflicts_with = "days")]
    pub since: Option<NaiveDate>,
    /// Last N days, anchored to today.
    #[arg(long, default_value_t = DEFAULT_SINCE_DAYS)]
    pub days: u32,
    /// Filter by document kind (must be in `kinds.allowed`).
    #[arg(long)]
    pub kind: Option<String>,
    /// Which date field to consult.
    #[arg(long, value_enum, default_value_t = FieldArg::Any)]
    pub field: FieldArg,
    /// Maximum entries returned.
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    pub limit: usize,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum FieldArg {
    Created,
    Updated,
    Reviewed,
    Any,
}

impl From<FieldArg> for RecencyField {
    fn from(f: FieldArg) -> Self {
        match f {
            FieldArg::Created => Self::Created,
            FieldArg::Updated => Self::Updated,
            FieldArg::Reviewed => Self::Reviewed,
            FieldArg::Any => Self::Any,
        }
    }
}

pub fn run(root: &Path, args: RecentArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    let since = match args.since {
        Some(d) => RecencySince::Date(d),
        None => RecencySince::Days(args.days),
    };
    let opts = RecencyOptions {
        since,
        kind: args.kind,
        field: args.field.into(),
        limit: Some(args.limit),
    };
    let items = recent::find_recent(&graph, &opts);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
