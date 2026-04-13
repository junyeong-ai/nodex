use std::collections::BTreeMap;
use std::path::Path;

use crate::model::Node;
use crate::model::{Edge, RawEdge, ResolvedTarget};

/// Resolve raw edges (path-based targets) into edges with resolved node ids.
pub fn resolve_edges(
    source_id: &str,
    raw_edges: Vec<RawEdge>,
    source_path: &Path,
    path_index: &BTreeMap<String, String>,
    id_set: &BTreeMap<String, ()>,
) -> Vec<Edge> {
    raw_edges
        .into_iter()
        .map(|raw| {
            let target = resolve_target(
                &raw.target_path,
                &raw.relation,
                source_path,
                path_index,
                id_set,
            );
            Edge {
                source: source_id.to_string(),
                target,
                relation: raw.relation,
                confidence: raw.confidence,
                location: raw.location,
            }
        })
        .collect()
}

fn resolve_target(
    target: &str,
    relation: &str,
    source_path: &Path,
    path_index: &BTreeMap<String, String>,
    id_set: &BTreeMap<String, ()>,
) -> ResolvedTarget {
    // Frontmatter relations (supersedes, implements, related) use node ids directly
    match relation {
        "supersedes" | "implements" | "related" => {
            if id_set.contains_key(target) {
                return ResolvedTarget::resolved(target);
            }
            return ResolvedTarget::unresolved(target, "node id not found in graph");
        }
        _ => {}
    }

    // Path-based resolution for references/imports
    let normalized = target.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);

    // 1. Direct path match
    if let Some(id) = path_index.get(normalized) {
        return ResolvedTarget::resolved(id);
    }

    // 2. Resolve relative to source file's directory
    if let Some(parent) = source_path.parent() {
        let resolved = parent.join(normalized);
        let resolved_str = resolved.to_string_lossy().replace('\\', "/");
        if let Some(id) = path_index.get(resolved_str.as_str()) {
            return ResolvedTarget::resolved(id);
        }
    }

    ResolvedTarget::unresolved(target, "path not found in scope")
}

/// Build a path → node_id index from parsed nodes.
pub fn build_path_index(nodes: &[(String, Node)]) -> BTreeMap<String, String> {
    let mut index = BTreeMap::new();
    for (id, node) in nodes {
        let path_str = node.path.to_string_lossy().replace('\\', "/");
        index.insert(path_str, id.clone());
    }
    index
}

/// Build a set of known node ids for direct id-based resolution.
pub fn build_id_set(nodes: &[(String, Node)]) -> BTreeMap<String, ()> {
    nodes.iter().map(|(id, _)| (id.clone(), ())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Confidence, Kind, RawEdge, Status};
    use std::path::PathBuf;

    fn make_node(id: &str, path: &str) -> (String, Node) {
        (
            id.to_string(),
            Node {
                id: id.to_string(),
                path: PathBuf::from(path),
                title: "Test".to_string(),
                kind: Kind::new("generic"),
                status: Status::default(),
                created: None,
                updated: None,
                reviewed: None,
                owner: None,
                supersedes: vec![],
                superseded_by: None,
                implements: vec![],
                related: vec![],
                tags: vec![],
                orphan_ok: false,
                attrs: BTreeMap::new(),
            },
        )
    }

    #[test]
    fn resolve_direct_path() {
        let nodes = vec![make_node("guide-auth", "docs/guides/auth.md")];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "docs/guides/auth.md".to_string(),
                relation: "references".to_string(),
                confidence: Confidence::Extracted,
                location: "L5".to_string(),
            }],
            Path::new("docs/decisions/0001-auth.md"),
            &path_index,
            &id_set,
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-auth"));
    }

    #[test]
    fn resolve_relative_path() {
        let nodes = vec![make_node("guide-auth", "docs/guides/auth.md")];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "guide-index",
            vec![RawEdge {
                target_path: "auth.md".to_string(),
                relation: "references".to_string(),
                confidence: Confidence::Extracted,
                location: "L3".to_string(),
            }],
            Path::new("docs/guides/index.md"),
            &path_index,
            &id_set,
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-auth"));
    }

    #[test]
    fn resolve_frontmatter_relation_by_id() {
        let nodes = vec![
            make_node("adr-001", "docs/decisions/0001.md"),
            make_node("adr-002", "docs/decisions/0002.md"),
        ];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "adr-002",
            vec![RawEdge {
                target_path: "adr-001".to_string(),
                relation: "supersedes".to_string(),
                confidence: Confidence::Extracted,
                location: "frontmatter:supersedes".to_string(),
            }],
            Path::new("docs/decisions/0002.md"),
            &path_index,
            &id_set,
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("adr-001"));
    }

    #[test]
    fn unresolved_target() {
        let nodes: Vec<(String, Node)> = vec![];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "test",
            vec![RawEdge {
                target_path: "nonexistent.md".to_string(),
                relation: "references".to_string(),
                confidence: Confidence::Extracted,
                location: "L1".to_string(),
            }],
            Path::new("test.md"),
            &path_index,
            &id_set,
        );

        assert_eq!(edges.len(), 1);
        assert!(!edges[0].target.is_resolved());
    }
}
