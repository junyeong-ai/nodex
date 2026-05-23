use anyhow::Result;
use std::path::Path;

use nodex_core::query::recent::{RecencyOptions, RecencySince};

use crate::format::{Envelope, ItemsEnvelope, print_json};

use super::{
    RecentArgs, load_graph, reject_empty_csv_entries, reject_unknown_vocabulary, reject_zero_u32,
    reject_zero_usize,
};

pub(crate) fn run_nodes(
    root: &Path,
    kind: Vec<String>,
    status: Vec<String>,
    tag: Vec<String>,
    all_tags: bool,
    limit: Option<usize>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    reject_empty_csv_entries("--kind", &kind)?;
    reject_empty_csv_entries("--status", &status)?;
    reject_empty_csv_entries("--tag", &tag)?;
    reject_unknown_vocabulary("--kind", &kind, &config.kinds.allowed)?;
    reject_unknown_vocabulary("--status", &status, &config.statuses.allowed)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }

    let graph = load_graph(root, &config)?;
    let filter = nodex_core::NodeFilter {
        kinds: kind,
        statuses: status,
        tags: tag,
        require_all_tags: all_tags,
        limit,
    };
    let items = nodex_core::find_nodes(&graph, &filter);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

pub(crate) fn run_search(
    root: &Path,
    keyword: &str,
    statuses: Option<Vec<String>>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;
    let items = nodex_core::query::search::search(&graph, keyword, statuses.as_deref());
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}

pub(crate) fn run_recent(root: &Path, args: RecentArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // Validate inputs BEFORE `load_graph` so an invalid flag surfaces
    // as `CONFIG_ERROR` even when `graph.json` is missing — symmetric
    // with `run_trust` / `run_similar`. Reject zero on `--days` /
    // `--limit` (a zero-day window is degenerate; a zero limit
    // silently empties the listing).
    if args.since.is_none() {
        reject_zero_u32(args.days, "--days")?;
    }
    reject_zero_usize(args.limit, "--limit")?;
    if let Some(k) = &args.kind {
        reject_unknown_vocabulary("--kind", std::slice::from_ref(k), &config.kinds.allowed)?;
    }
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
    let items = nodex_core::query::recent::find_recent(&graph, &opts);
    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
