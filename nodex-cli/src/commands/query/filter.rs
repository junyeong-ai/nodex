use anyhow::Result;
use std::path::Path;

use nodex_core::query::recent::{RecentOptions, RecentSince};

use crate::format::{ItemsEnvelope, emit_read_with};

use super::{
    NodesArgs, RecentArgs, reject_empty_csv_entries, reject_unknown_vocabulary, reject_zero_u32,
    reject_zero_usize,
};

/// Vocabulary for `--fields`, owned by core next to `NodeRef` so the
/// flag and the struct cannot drift.
fn node_ref_fields() -> Vec<String> {
    nodex_core::query::NODE_REF_FIELDS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

pub(crate) fn run_nodes(root: &Path, args: NodesArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    reject_empty_csv_entries("--kind", &args.kind)?;
    reject_empty_csv_entries("--status", &args.status)?;
    reject_empty_csv_entries("--tag", &args.tag)?;
    reject_empty_csv_entries("--fields", &args.fields)?;
    reject_unknown_vocabulary("--kind", &args.kind, &config.kinds.allowed)?;
    reject_unknown_vocabulary("--status", &args.status, &config.statuses.allowed)?;
    reject_unknown_vocabulary("--fields", &args.fields, &node_ref_fields())?;
    if let Some(n) = args.limit {
        reject_zero_usize(n, "--limit")?;
    }

    let (graph, warnings) = nodex_core::load_graph(root, &config)?;
    let filter = nodex_core::NodeFilter {
        kinds: args.kind,
        statuses: args.status,
        tags: args.tag,
        require_all_tags: args.all_tags,
    };
    let items: Vec<nodex_core::query::NodeRefProjection> = nodex_core::find_nodes(&graph, &filter)
        .into_iter()
        .map(|r| nodex_core::query::NodeRefProjection::from_node_ref(r, &args.fields))
        .collect();
    emit_read_with(
        ItemsEnvelope::capped(items, args.limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}

pub(crate) fn run_search(
    root: &Path,
    keyword: &str,
    statuses: Option<Vec<String>>,
    limit: Option<usize>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // An unknown status would silently match zero nodes and return a
    // successful empty result — the silent-skip failure mode every
    // other vocabulary-taking flag (`query nodes --status`, `--kind`)
    // already refuses. Omit `--status` entirely to search every status.
    if let Some(statuses) = &statuses {
        reject_empty_csv_entries("--status", statuses)?;
        reject_unknown_vocabulary("--status", statuses, &config.statuses.allowed)?;
    }
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;
    let items = nodex_core::query::search::search(
        &graph,
        &config.search.weights,
        keyword,
        statuses.as_deref(),
    );
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
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
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;

    let since = match args.since {
        Some(d) => RecentSince::Date(d),
        None => RecentSince::Days(args.days),
    };
    let opts = RecentOptions {
        since,
        kind: args.kind,
        field: args.field.into(),
        limit: Some(args.limit),
    };
    let items = nodex_core::query::recent::find_recent(&graph, &opts);
    emit_read_with(ItemsEnvelope::new(items), warnings, &config, pretty);
    Ok(())
}
