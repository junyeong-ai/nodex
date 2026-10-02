use anyhow::Result;
use std::path::Path;

use crate::format::{ItemsEnvelope, emit_read_with};

use super::{reject_zero_u32, reject_zero_usize};

pub(crate) fn run_backlinks(
    context: &super::QueryContext<'_>,
    node_id: &str,
    limit: Option<usize>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let snapshot = context.load_graph(&config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    snapshot.require(context.root, &config, graph.require_node(node_id))?;
    let items = nodex_core::query::traverse::find_backlinks(graph, node_id);
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}

pub(crate) fn run_chain(
    context: &super::QueryContext<'_>,
    node_id: &str,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    let snapshot = context.load_graph(&config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    snapshot.require(context.root, &config, graph.require_node(node_id))?;
    let items = nodex_core::query::traverse::find_chain(graph, node_id);
    emit_read_with(ItemsEnvelope::new(items), warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_node(
    context: &super::QueryContext<'_>,
    id: Option<&str>,
    path: Option<&str>,
    with_body: bool,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    let snapshot = context.load_graph(&config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());

    let resolved_id: String = match (id, path) {
        (Some(id), None) => snapshot
            .require(context.root, &config, graph.require_node(id))?
            .id
            .clone(),
        (None, Some(p)) => {
            let normalised = nodex_core::path_guard::normalize_for_lookup(p, context.root)?;
            snapshot
                .require(
                    context.root,
                    &config,
                    graph.require_node_by_path(Path::new(&normalised)),
                )?
                .id
                .clone()
        }
        _ => unreachable!("clap ArgGroup enforces exactly one of <id> or --path"),
    };

    let mut detail = nodex_core::query::traverse::find_node_entry(graph, &resolved_id)
        .expect("require_node / node_by_path guarantees presence");

    if with_body {
        detail.body = Some(snapshot.body(context.root, &resolved_id)?);
    }

    emit_read_with(detail, warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_covered_by(
    context: &super::QueryContext<'_>,
    code_path: &str,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    let normalised = nodex_core::path_guard::normalize_for_lookup(code_path, context.root)?;
    let snapshot = context.load_graph(&config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let items =
        nodex_core::query::traverse::find_covered_by(graph, &normalised, &config.parser.extensions);
    emit_read_with(ItemsEnvelope::new(items), warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_dependents(
    context: &super::QueryContext<'_>,
    id: &str,
    depth: Option<u32>,
    relations: Vec<String>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    // Validate inputs BEFORE `load_graph` so a missing graph cannot
    // mask a flag bug behind `GRAPH_MISSING`. `--depth 0` is rejected for
    // symmetry with every other zero-cap input — at depth 0 the
    // traversal would never expand past the seed and report zero
    // dependents regardless of the corpus, which the operator never
    // asked for.
    if let Some(d) = depth {
        reject_zero_u32(d, "--depth")?;
    }
    if !relations.is_empty() {
        let known = config.known_relations();
        let unknown: Vec<&str> = relations
            .iter()
            .filter(|r| !known.contains(r.as_str()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let known_sorted: Vec<&str> = known.iter().map(String::as_str).collect();
            return Err(nodex_core::error::Error::Config(format!(
                "--relations contains unknown value(s) {unknown:?}; known: {known_sorted:?}"
            ))
            .into());
        }
    }
    let snapshot = context.load_graph(&config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let report = snapshot.require(
        context.root,
        &config,
        nodex_core::query::dependents::find_dependents(graph, id, depth, &relations),
    )?;
    emit_read_with(report, warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_neighborhood(
    context: &super::QueryContext<'_>,
    id: &str,
    depth: u32,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    // `--depth 0` would return the seed alone — `find_neighborhood`
    // supports that semantic at the library level (it's a legitimate
    // "no traversal" probe for composed callers), but at the CLI the
    // input is degenerate: the operator typed "give me a
    // neighbourhood" and asked for a corpus of one. Reject up-front,
    // symmetric with every other zero-cap input.
    reject_zero_u32(depth, "--depth")?;
    let snapshot = context.load_graph(&config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let result = snapshot.require(
        context.root,
        &config,
        nodex_core::query::structure::find_neighborhood(graph, id, depth),
    )?;
    emit_read_with(result, warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_components(
    context: &super::QueryContext<'_>,
    limit: Option<usize>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(context.root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let snapshot = context.load_graph(&config)?;
    let (graph, warnings) = (snapshot.graph(), snapshot.warnings());
    let items = nodex_core::query::structure::find_components(graph);
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}
