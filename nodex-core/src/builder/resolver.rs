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
    let normalized = crate::path_guard::forward_str(target);
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);

    // A root-anchored path inside a project-relative graph is
    // meaningless; keeping it would let `[link](/etc/passwd.md)`
    // accidentally hit a node with the literal path "/etc/passwd.md"
    // if one ever existed. `Path::has_root` (not `is_absolute`) is
    // the cross-platform predicate — on Windows the latter only
    // returns true for drive-letter or verbatim forms, missing
    // drive-relative `/etc/passwd` / `\etc\passwd`.
    if Path::new(normalized).has_root() {
        return ResolvedTarget::unresolved(target, "absolute paths are not in scope");
    }

    // 1. Direct path match
    if let Some(id) = path_index.get(normalized) {
        return ResolvedTarget::resolved(id);
    }

    // 2. Resolve relative to source file's directory
    if let Some(parent) = source_path.parent() {
        match normalize_path_segments(&parent.join(normalized)) {
            Ok(resolved) => {
                if let Some(id) = path_index.get(&resolved) {
                    return ResolvedTarget::resolved(id);
                }
            }
            Err(NormalizeError::Underflow) => {
                return ResolvedTarget::unresolved(target, "path escapes source scope");
            }
        }
    }

    ResolvedTarget::unresolved(target, "path not found in scope")
}

#[derive(Debug)]
enum NormalizeError {
    /// More `..` segments than directories to consume — the path
    /// escapes the project root.
    Underflow,
}

/// Resolve `.` and `..` segments without touching the filesystem.
/// Errors with [`NormalizeError::Underflow`] when `..` would consume
/// past the root — silently dropping it could let a crafted link
/// match an unrelated in-scope node.
fn normalize_path_segments(path: &Path) -> Result<String, NormalizeError> {
    let normalized = crate::path_guard::forward_string(path);
    let mut parts: Vec<&str> = Vec::new();
    for component in normalized.split('/') {
        match component {
            "." | "" => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(NormalizeError::Underflow);
                }
            }
            other => parts.push(other),
        }
    }
    Ok(parts.join("/"))
}

/// Build a path → node_id index from parsed nodes.
pub fn build_path_index(nodes: &[(String, Node)]) -> BTreeMap<String, String> {
    let mut index = BTreeMap::new();
    for (id, node) in nodes {
        let path_str = crate::path_guard::forward_string(&node.path);
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
    use crate::model::{Kind, RawEdge, Status};
    use std::path::PathBuf;

    fn make_node(id: &str, path: &str) -> (String, Node) {
        (
            id.to_string(),
            Node {
                id: id.to_string(),
                path: PathBuf::from(path),
                title: "Test".to_string(),
                kind: Kind::new("generic"),
                status: Status::new("active"),
                created: None,
                updated: None,
                reviewed: None,
                owner: None,
                supersedes: vec![],
                superseded_by: None,
                implements: vec![],
                related: vec![],
                tags: vec![],
                covers: vec![],
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
                location: "L1".to_string(),
            }],
            Path::new("test.md"),
            &path_index,
            &id_set,
        );

        assert_eq!(edges.len(), 1);
        assert!(matches!(
            edges[0].target,
            crate::model::ResolvedTarget::Unresolved { .. }
        ));
    }

    #[test]
    fn resolve_relative_path_with_dotdot() {
        let nodes = vec![make_node("guide-setup", "docs/guides/setup.md")];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "../guides/setup.md".to_string(),
                relation: "references".to_string(),
                location: "L5".to_string(),
            }],
            Path::new("docs/decisions/0001-auth.md"),
            &path_index,
            &id_set,
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-setup"));
    }

    #[test]
    fn normalize_dotdot_segments() {
        assert_eq!(
            normalize_path_segments(Path::new("docs/decisions/../guides/setup.md")).unwrap(),
            "docs/guides/setup.md"
        );
        assert_eq!(
            normalize_path_segments(Path::new("a/b/c/../../d.md")).unwrap(),
            "a/d.md"
        );
    }

    #[test]
    fn normalize_underflow_is_an_error() {
        assert!(matches!(
            normalize_path_segments(Path::new("../escape.md")),
            Err(NormalizeError::Underflow)
        ));
        assert!(matches!(
            normalize_path_segments(Path::new("a/../../escape.md")),
            Err(NormalizeError::Underflow)
        ));
    }

    #[test]
    fn underflow_link_is_unresolved_with_reason() {
        let nodes = vec![make_node("guide-setup", "docs/guides/setup.md")];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "../../../../escape.md".to_string(),
                relation: "references".to_string(),
                location: "L1".to_string(),
            }],
            Path::new("docs/decisions/0001.md"),
            &path_index,
            &id_set,
        );

        assert_eq!(edges.len(), 1);
        match &edges[0].target {
            crate::model::ResolvedTarget::Unresolved { reason, .. } => {
                assert!(reason.contains("escapes"), "reason was {reason:?}");
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn absolute_link_is_unresolved() {
        let nodes: Vec<(String, Node)> = vec![];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "x",
            vec![RawEdge {
                target_path: "/etc/passwd.md".to_string(),
                relation: "references".to_string(),
                location: "L1".to_string(),
            }],
            Path::new("docs/x.md"),
            &path_index,
            &id_set,
        );
        assert_eq!(edges.len(), 1);
        match &edges[0].target {
            crate::model::ResolvedTarget::Unresolved { reason, .. } => {
                assert!(reason.contains("absolute"), "reason was {reason:?}");
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }
}
