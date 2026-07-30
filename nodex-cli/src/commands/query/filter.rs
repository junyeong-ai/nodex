use anyhow::Result;
use std::path::Path;

use nodex_core::query::recent::{RecentOptions, RecentSince};

use crate::format::{ItemsEnvelope, emit_read_with};

use super::{
    NodesArgs, RecentArgs, reject_empty_csv_entries, reject_unknown_vocabulary, reject_zero_u32,
    reject_zero_usize,
};

/// True when `field` is one of the five `NodeRef` identity spine
/// fields (projected in place), as opposed to an enrichment field that
/// lands under `attrs`.
fn is_spine_field(field: &str) -> bool {
    nodex_core::query::NODE_REF_FIELDS.contains(&field)
}

pub(crate) fn run_nodes(root: &Path, args: NodesArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    reject_empty_csv_entries("--kind", &args.kind)?;
    reject_empty_csv_entries("--status", &args.status)?;
    reject_empty_csv_entries("--tag", &args.tag)?;
    reject_empty_csv_entries("--fields", &args.fields)?;
    reject_unknown_vocabulary("--kind", &args.kind, &config.kinds.allowed)?;
    reject_unknown_vocabulary("--status", &args.status, &config.statuses.allowed)?;
    // `--fields` accepts the NodeRef identity spine PLUS any field the
    // project declares (other built-ins like `owner` / `created` / `tags`,
    // and `attrs` keys) — so an agent pulls a document's own frontmatter
    // in one listing instead of reparsing files. The vocabulary is the
    // spine ∪ `Config::declared_fields_universe()`; anything else is a
    // CONFIG_ERROR, never a silently dropped field.
    let mut field_vocab = config.declared_fields_universe();
    field_vocab.extend(
        nodex_core::query::NODE_REF_FIELDS
            .iter()
            .map(|s| (*s).to_string()),
    );
    let field_vocab: Vec<String> = field_vocab.into_iter().collect();
    reject_unknown_vocabulary("--fields", &args.fields, &field_vocab)?;
    // `--where field=value`: exact equality over the same scalar
    // vocabulary as `--fields`, read with the same logic as a
    // `cross_field` when predicate. Parse FIELD=VALUE (first `=` splits),
    // reject a missing separator / empty field, and reject an unknown
    // field — an undeclared field would silently match nothing (the
    // silent-skip failure mode the other vocabulary flags already refuse).
    // A collection-valued built-in is refused for the same reason
    // `cross_field` refuses equals/in on one at load: equality compares
    // against a comma-joined string and silently misses multi-value
    // documents. The value is otherwise unconstrained: a free-form attr
    // legitimately holds any sentinel.
    let mut field_equals: Vec<(String, String)> = Vec::new();
    for clause in &args.where_ {
        let (key, value) = clause.split_once('=').ok_or_else(|| {
            nodex_core::error::Error::Config(format!(
                "--where {clause:?} is not FIELD=VALUE — exact equality only \
                 (e.g. --where owner=alice)"
            ))
        })?;
        if key.is_empty() {
            return Err(nodex_core::error::Error::Config(format!(
                "--where {clause:?} has an empty field name"
            ))
            .into());
        }
        if nodex_core::config::is_collection_builtin(key) {
            return Err(nodex_core::error::Error::Config(format!(
                "--where {clause:?}: {key:?} is a collection-valued field; equality would \
                 compare against a comma-joined string and silently miss multi-value \
                 documents. Use --tag for tag membership"
            ))
            .into());
        }
        field_equals.push((key.to_string(), value.to_string()));
    }
    let where_keys: Vec<String> = field_equals.iter().map(|(k, _)| k.clone()).collect();
    reject_unknown_vocabulary("--where field", &where_keys, &field_vocab)?;
    if let Some(n) = args.limit {
        reject_zero_usize(n, "--limit")?;
    }

    let snapshot = nodex_core::load_graph(root, &config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let filter = nodex_core::NodeFilter {
        kinds: args.kind,
        statuses: args.status,
        tags: args.tag,
        require_all_tags: args.all_tags,
        field_equals,
    };
    // Route each requested field: identity spine projects in place,
    // everything else enriches `attrs`. With no `--fields`, project the
    // full spine explicitly so the default listing always carries the
    // five-field identity (never a bare object).
    let (spine_fields, extra_fields): (Vec<String>, Vec<String>) = if args.fields.is_empty() {
        (
            nodex_core::query::NODE_REF_FIELDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            Vec::new(),
        )
    } else {
        args.fields.iter().cloned().partition(|f| is_spine_field(f))
    };
    let items = nodex_core::find_nodes_projected(graph, &filter, &spine_fields, &extra_fields);
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
    // An empty keyword is a substring of every document, so it would
    // "match" the whole corpus at partial weight — the opposite of a
    // keyword search and a silent surprise, not an error the operator
    // intended. Refuse it up front (no config needed), symmetric with the
    // other degenerate-input guards (`--status`, `--limit`).
    if keyword.is_empty() {
        return Err(nodex_core::error::Error::Config(
            "search keyword must not be empty — an empty keyword matches every document, \
             which is never a meaningful search"
                .into(),
        )
        .into());
    }
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
    let snapshot = nodex_core::load_graph(root, &config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let items = nodex_core::query::search::search(
        graph,
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
    let snapshot = nodex_core::load_graph(root, &config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());

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
    let items = nodex_core::query::recent::find_recent(graph, &opts);
    emit_read_with(ItemsEnvelope::new(items), warnings, &config, pretty);
    Ok(())
}
