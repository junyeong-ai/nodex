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
/// guarded write primitive — `root` enforces containment, so an
/// `output.dir` that resolves outside the project through a symlinked
/// ancestor is refused at the write. Derived indices (backlinks,
/// supersession chains, …) are intentionally not materialised — every
/// consumer reads from the single source of truth and computes what it
/// needs in O(degree).
pub fn write_json_outputs(root: &Path, graph: &Graph, output_dir: &Path) -> Result<()> {
    let graph_path = output_dir.join("graph.json");
    path_guard::write_atomic_in_root(root, &graph_path, &render_graph_json(graph))
}

// The suite exercises symlink containment, which needs unix symlink
// creation; the helper would be unused on other targets.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::model::GraphMeta;
    use indexmap::IndexMap;

    fn empty_graph() -> Graph {
        Graph::new(
            IndexMap::new(),
            vec![],
            vec![],
            vec![],
            vec![],
            GraphMeta::default(),
        )
    }

    #[test]
    fn write_json_outputs_refuses_output_dir_escaping_via_symlinked_ancestor() {
        use std::os::unix::fs as unix_fs;
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        unix_fs::symlink(outside.path(), root.path().join("_index")).unwrap();

        let err = write_json_outputs(root.path(), &empty_graph(), &root.path().join("_index"))
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::OutsideRoot(_)));
        assert!(
            !outside.path().join("graph.json").exists(),
            "the external target must stay untouched"
        );
    }

    #[test]
    fn write_json_outputs_accepts_in_root_symlinked_output_dir() {
        // Containment is the guard, not symlink-ness: a symlinked output
        // dir that resolves inside the root keeps working.
        use std::os::unix::fs as unix_fs;
        let root = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(root.path().join("real")).unwrap();
        unix_fs::symlink(root.path().join("real"), root.path().join("_index")).unwrap();

        write_json_outputs(root.path(), &empty_graph(), &root.path().join("_index"))
            .expect("in-root symlinked output dir is contained");
        assert!(root.path().join("real/graph.json").exists());
    }
}
