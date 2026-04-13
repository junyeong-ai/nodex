use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};
use crate::model::Graph;

/// Export graph as graph.json.
pub fn render_graph_json(graph: &Graph) -> Result<String> {
    serde_json::to_string(graph).map_err(|e| Error::Other(format!("JSON serialization error: {e}")))
}

/// Export backlinks index as backlinks.json.
pub fn render_backlinks_json(graph: &Graph) -> Result<String> {
    let mut backlinks: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for node_id in graph.nodes().keys() {
        let sources: Vec<String> = graph
            .incoming_edges(node_id)
            .iter()
            .map(|e| e.source.clone())
            .collect();
        if !sources.is_empty() {
            backlinks.insert(node_id.clone(), sources);
        }
    }

    serde_json::to_string(&backlinks)
        .map_err(|e| Error::Other(format!("JSON serialization error: {e}")))
}

/// Write graph.json and backlinks.json to the output directory.
pub fn write_json_outputs(graph: &Graph, output_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(output_dir).map_err(|e| Error::Io {
        path: output_dir.to_path_buf(),
        source: e,
    })?;

    let graph_json = render_graph_json(graph)?;
    let graph_path = output_dir.join("graph.json");
    std::fs::write(&graph_path, &graph_json).map_err(|e| Error::Io {
        path: graph_path,
        source: e,
    })?;

    let backlinks_json = render_backlinks_json(graph)?;
    let backlinks_path = output_dir.join("backlinks.json");
    std::fs::write(&backlinks_path, &backlinks_json).map_err(|e| Error::Io {
        path: backlinks_path,
        source: e,
    })?;

    Ok(())
}
