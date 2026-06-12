use anyhow::{Context, Result};
use std::path::Path;

use nodex_core::error::Error as CoreError;
use nodex_core::parser::frontmatter::{canonicalize, split_frontmatter};

use crate::format::{ItemsEnvelope, emit_read_with};

use super::{reject_zero_u32, reject_zero_usize};

pub(crate) fn run_backlinks(
    root: &Path,
    node_id: &str,
    limit: Option<usize>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;
    graph.require_node(node_id)?;
    let items = nodex_core::query::traverse::find_backlinks(&graph, node_id);
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}

pub(crate) fn run_chain(root: &Path, node_id: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;
    graph.require_node(node_id)?;
    let items = nodex_core::query::traverse::find_chain(&graph, node_id);
    emit_read_with(ItemsEnvelope::new(items), warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_node(
    root: &Path,
    id: Option<&str>,
    path: Option<&str>,
    with_body: bool,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;

    let resolved_id: String = match (id, path) {
        (Some(id), None) => graph.require_node(id)?.id.clone(),
        (None, Some(p)) => {
            let normalised = nodex_core::path_guard::normalize_for_lookup(p, root)?;
            graph
                .require_node_by_path(Path::new(&normalised))?
                .id
                .clone()
        }
        _ => unreachable!("clap ArgGroup enforces exactly one of <id> or --path"),
    };

    let mut detail = nodex_core::query::traverse::find_node_entry(&graph, &resolved_id)
        .expect("require_node / node_by_path guarantees presence");

    if with_body {
        // The graph stores body fingerprints, never text — re-read the
        // file through the canonical parse seam (BOM / line-ending
        // normalisation + frontmatter split) so the attached body is
        // byte-identical to what `body_hash` was computed over. A read
        // failure on a successfully-looked-up node means the graph is
        // stale (file moved or deleted since the last build) — surface
        // it as a typed error naming the path, never a silent drop.
        let abs = root.join(&detail.node.path);
        let content = std::fs::read_to_string(&abs)
            .map_err(|source| CoreError::Io {
                path: abs.clone(),
                source,
            })
            .with_context(|| {
                format!(
                    "{} is in the graph but unreadable on disk — the graph is stale; \
                     run `nodex build` and retry",
                    detail.node.path.display()
                )
            })?;
        let canonical = canonicalize(&content);
        let (_, body) = split_frontmatter(&canonical)
            .map_err(|source| CoreError::Parse {
                path: abs.clone(),
                source,
            })
            .with_context(|| {
                format!(
                    "{} is in the graph but no longer splits on disk — the graph is stale; \
                     run `nodex build` and retry",
                    detail.node.path.display()
                )
            })?;
        detail.body = Some(body.to_string());
    }

    emit_read_with(detail, warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_covered_by(root: &Path, code_path: &str, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let normalised = nodex_core::path_guard::normalize_for_lookup(code_path, root)?;
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;
    let items = nodex_core::query::traverse::find_covered_by(&graph, &normalised);
    emit_read_with(ItemsEnvelope::new(items), warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_dependents(
    root: &Path,
    id: &str,
    depth: Option<u32>,
    relations: Vec<String>,
    pretty: bool,
) -> Result<()> {
    let config = nodex_core::load_project(root)?;
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
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;
    let report = nodex_core::query::dependents::find_dependents(&graph, id, depth, &relations)?;
    emit_read_with(report, warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_neighborhood(root: &Path, id: &str, depth: u32, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    // `--depth 0` would return the seed alone — `find_neighborhood`
    // supports that semantic at the library level (it's a legitimate
    // "no traversal" probe for composed callers), but at the CLI the
    // input is degenerate: the operator typed "give me a
    // neighbourhood" and asked for a corpus of one. Reject up-front,
    // symmetric with every other zero-cap input.
    reject_zero_u32(depth, "--depth")?;
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;
    let result = nodex_core::query::structure::find_neighborhood(&graph, id, depth)?;
    emit_read_with(result, warnings, &config, pretty);
    Ok(())
}

pub(crate) fn run_components(root: &Path, limit: Option<usize>, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    if let Some(n) = limit {
        reject_zero_usize(n, "--limit")?;
    }
    let (graph, warnings) = nodex_core::load_graph(root, &config)?;
    let items = nodex_core::query::structure::find_components(&graph);
    emit_read_with(
        ItemsEnvelope::capped(items, limit),
        warnings,
        &config,
        pretty,
    );
    Ok(())
}
