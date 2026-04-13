use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::model::Edge;

/// Validate that supersedes edges form a DAG (no cycles).
/// Uses iterative 3-color DFS.
pub fn validate_supersedes_dag(edges: &[Edge]) -> Result<()> {
    // Build adjacency list for supersedes edges only
    let mut adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut all_nodes: BTreeSet<String> = BTreeSet::new();

    for edge in edges {
        if edge.relation != "supersedes" {
            continue;
        }
        let Some(target_id) = edge.target.id() else {
            continue;
        };

        adj.entry(edge.source.clone())
            .or_default()
            .push(target_id.to_string());
        all_nodes.insert(edge.source.clone());
        all_nodes.insert(target_id.to_string());
    }

    if adj.is_empty() {
        return Ok(());
    }

    // 3-color DFS: White(0) = unvisited, Gray(1) = in progress, Black(2) = done
    let mut color: BTreeMap<&str, u8> = BTreeMap::new();
    for node in &all_nodes {
        color.insert(node.as_str(), 0);
    }

    for start in &all_nodes {
        if color[start.as_str()] != 0 {
            continue;
        }

        // Iterative DFS with explicit stack
        let mut stack: Vec<(&str, usize)> = vec![(start.as_str(), 0)];
        let mut path: Vec<&str> = vec![];

        while let Some((node, child_idx)) = stack.last_mut() {
            let node = *node;

            if *child_idx == 0 {
                color.insert(node, 1); // Gray
                path.push(node);
            }

            let children = adj.get(node).map(|v| v.as_slice()).unwrap_or_default();

            if *child_idx < children.len() {
                let child = children[*child_idx].as_str();
                *child_idx += 1;

                match color.get(child).copied().unwrap_or(0) {
                    1 => {
                        // Found cycle — extract it from the current DFS path
                        let cycle: Vec<String> = match path.iter().position(|&n| n == child) {
                            Some(start) => path[start..].iter().map(|s| s.to_string()).collect(),
                            None => path.iter().map(|s| s.to_string()).collect(),
                        };
                        return Err(Error::SupersedesCycle { chain: cycle });
                    }
                    0 => {
                        stack.push((child, 0));
                    }
                    _ => {} // Black — already fully explored
                }
            } else {
                color.insert(node, 2); // Black
                path.pop();
                stack.pop();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Confidence, ResolvedTarget};

    fn make_supersedes_edge(source: &str, target: &str) -> Edge {
        Edge {
            source: source.to_string(),
            target: ResolvedTarget::resolved(target),
            relation: "supersedes".to_string(),
            confidence: Confidence::Extracted,
            location: "frontmatter:supersedes".to_string(),
        }
    }

    #[test]
    fn valid_dag() {
        let edges = vec![
            make_supersedes_edge("adr-003", "adr-002"),
            make_supersedes_edge("adr-002", "adr-001"),
        ];
        assert!(validate_supersedes_dag(&edges).is_ok());
    }

    #[test]
    fn detects_cycle() {
        let edges = vec![
            make_supersedes_edge("a", "b"),
            make_supersedes_edge("b", "c"),
            make_supersedes_edge("c", "a"),
        ];
        let err = validate_supersedes_dag(&edges).unwrap_err();
        assert!(matches!(err, Error::SupersedesCycle { .. }));
    }

    #[test]
    fn empty_edges_ok() {
        assert!(validate_supersedes_dag(&[]).is_ok());
    }

    #[test]
    fn non_supersedes_edges_ignored() {
        let edges = vec![Edge {
            source: "a".to_string(),
            target: ResolvedTarget::resolved("b"),
            relation: "references".to_string(),
            confidence: Confidence::Extracted,
            location: "L1".to_string(),
        }];
        assert!(validate_supersedes_dag(&edges).is_ok());
    }
}
