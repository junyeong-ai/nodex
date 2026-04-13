pub mod cache;
pub mod resolver;
pub mod scanner;
pub mod validator;

use indexmap::IndexMap;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{Graph, Node, RawEdge};
use crate::parser::{self, ParsedDocument};

use cache::BuildCache;
use resolver::{build_id_set, build_path_index, resolve_edges};
use validator::validate_supersedes_dag;

/// Build result with stats for CLI output.
pub struct BuildResult {
    pub graph: Graph,
    pub stats: BuildStats,
}

#[derive(Debug, serde::Serialize)]
pub struct BuildStats {
    pub nodes: usize,
    pub edges: usize,
    pub cached: usize,
    pub parsed: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Build the full document graph.
pub fn build(root: &Path, config: &Config, full_rebuild: bool) -> Result<BuildResult> {
    // 1. Scan scope
    let paths = scanner::scan_scope(root, config)?;

    // 2. Load cache (unless full rebuild). Invalidates if config changed.
    let cache_path = root.join(&config.output.dir).join("cache.json");
    let config_hash = {
        let config_json = serde_json::to_string(config).unwrap_or_default();
        cache::compute_hash(&config_json)
    };
    let mut cache = if full_rebuild {
        BuildCache::default()
    } else {
        BuildCache::load(&cache_path, &config_hash)
    };
    cache.config_hash = config_hash;

    // 3. Read file contents (parallel). Collect read errors for warning.
    let read_results: Vec<(
        std::path::PathBuf,
        std::result::Result<String, std::io::Error>,
    )> = paths
        .par_iter()
        .map(|rel_path| {
            let abs_path = root.join(rel_path);
            let result = std::fs::read_to_string(&abs_path);
            (rel_path.clone(), result)
        })
        .collect();

    let mut read_warnings = Vec::new();
    let mut file_contents: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (rel_path, result) in read_results {
        match result {
            Ok(content) => file_contents.push((rel_path, content)),
            Err(e) => read_warnings.push(format!("skipped {}: {e}", rel_path.display())),
        }
    }

    // 4. Parse documents (parallel, with caching)
    let mut cached_count = 0usize;
    let mut parsed_count = 0usize;

    // Separate into cached hits and cache misses
    let mut cached_results: Vec<(Node, Vec<RawEdge>)> = Vec::new();
    let mut to_parse: Vec<(std::path::PathBuf, String)> = Vec::new();

    for (rel_path, content) in &file_contents {
        if let Some(entry) = cache.get(rel_path, content) {
            cached_results.push((
                entry.node.clone(),
                entry.raw_edges.iter().cloned().map(RawEdge::from).collect(),
            ));
            cached_count += 1;
        } else {
            to_parse.push((rel_path.clone(), content.clone()));
        }
    }

    // Parse cache misses in parallel
    let fresh_results: Vec<Result<(std::path::PathBuf, String, ParsedDocument)>> = to_parse
        .par_iter()
        .map(|(rel_path, content)| {
            let doc = parser::parse_document(rel_path, content, config)?;
            Ok((rel_path.clone(), content.clone(), doc))
        })
        .collect();

    let mut all_nodes: Vec<(String, Node)> = Vec::new();
    let mut all_raw_edges: Vec<(String, std::path::PathBuf, Vec<RawEdge>)> = Vec::new();

    // Collect cached results
    for (node, raw_edges) in cached_results {
        let id = node.id.clone();
        let path = node.path.clone();
        all_raw_edges.push((id.clone(), path, raw_edges));
        all_nodes.push((id, node));
    }

    // Collect fresh results and update cache
    for result in fresh_results {
        let (rel_path, content, doc) = result?;
        parsed_count += 1;

        cache.insert(rel_path, &content, doc.node.clone(), &doc.raw_edges);

        let id = doc.node.id.clone();
        let path = doc.node.path.clone();
        all_raw_edges.push((id.clone(), path, doc.raw_edges));
        all_nodes.push((id, doc.node));
    }

    // 5. Check for duplicate ids
    {
        let mut seen: BTreeMap<&str, &Path> = BTreeMap::new();
        for (id, node) in &all_nodes {
            if let Some(&first_path) = seen.get(id.as_str()) {
                return Err(Error::DuplicateId {
                    id: id.clone(),
                    first: first_path.to_path_buf(),
                    second: node.path.clone(),
                });
            }
            seen.insert(id.as_str(), &node.path);
        }
    }

    // 6. Build resolution indices
    let path_index = build_path_index(&all_nodes);
    let id_set = build_id_set(&all_nodes);

    // 7. Resolve edges
    let mut edges = Vec::new();
    for (source_id, source_path, raw_edges) in all_raw_edges {
        let resolved = resolve_edges(&source_id, raw_edges, &source_path, &path_index, &id_set);
        edges.extend(resolved);
    }

    // 8. Validate supersedes DAG
    validate_supersedes_dag(&edges)?;

    // 9. Sort edges for deterministic output
    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| a.location.cmp(&b.location))
    });

    // 10. Build sorted node map
    let mut node_map = IndexMap::new();
    all_nodes.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, node) in all_nodes {
        node_map.insert(id, node);
    }

    // 11. Clean cache and save
    let valid_paths: Vec<_> = file_contents.iter().map(|(p, _)| p.clone()).collect();
    cache.retain_paths(&valid_paths);
    let mut warnings = read_warnings;
    if let Err(e) = cache.save(&cache_path) {
        warnings.push(format!("cache save failed: {e}"));
    }

    let stats = BuildStats {
        nodes: node_map.len(),
        edges: edges.len(),
        cached: cached_count,
        parsed: parsed_count,
        warnings,
    };

    Ok(BuildResult {
        graph: Graph::new(node_map, edges),
        stats,
    })
}
