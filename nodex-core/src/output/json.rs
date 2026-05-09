use std::path::Path;

use crate::error::Result;
use crate::model::Graph;
use crate::path_guard;

/// Render the canonical `graph.json` payload. nodex types are
/// JSON-serialisable by construction, so a serialiser failure here is
/// a programmer bug.
pub fn render_graph_json(graph: &Graph) -> String {
    serde_json::to_string(graph).expect("nodex types are JSON-serialisable")
}

/// Write `graph.json` to the output directory via the project-wide
/// atomic-write primitive. Derived indices (backlinks, supersession
/// chains, …) are intentionally not materialised — every consumer
/// reads from the single source of truth and computes what it needs
/// in O(degree).
pub fn write_json_outputs(graph: &Graph, output_dir: &Path) -> Result<()> {
    let graph_path = output_dir.join("graph.json");
    path_guard::write_atomic(&graph_path, &render_graph_json(graph))
}
