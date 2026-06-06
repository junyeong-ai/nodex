use std::collections::HashSet;

use super::{Rule, RuleContext, Severity, Violation};

/// Detects cycles in directed graph relations that should form a DAG.
/// A cycle in `implements` is a design defect (circular dependency).
pub struct CycleDetectionRule {
    /// Relations to check for cycles. Empty = the default DAG relations.
    pub relations: Vec<String>,
}

impl CycleDetectionRule {
    pub fn new(relations: Vec<String>) -> Self {
        Self { relations }
    }
}

impl Rule for CycleDetectionRule {
    fn id(&self) -> &str {
        "graph_invariants/cycle-detection"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Documents should not form cycles in the implements relation"
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let mut violations = Vec::new();

        // If relations list is empty, check the default DAG relations.
        let target_relations: Vec<&str> = if self.relations.is_empty() {
            vec!["implements"]
        } else {
            self.relations.iter().map(|s| s.as_str()).collect()
        };

        for relation in target_relations {
            let cycles = find_cycles_in_relation(ctx.graph, relation);
            for cycle in cycles {
                violations.push(Violation {
                    rule_id: self.id().to_string(),
                    severity: self.severity(),
                    node_id: Some(cycle.first().cloned().unwrap_or_default()),
                    path: ctx
                        .graph
                        .nodes()
                        .get(cycle.first().unwrap_or(&String::new()))
                        .map(|n| crate::path_guard::forward_string(&n.path)),
                    message: format!(
                        "cycle detected in '{}' relation: {}",
                        relation,
                        cycle.join(" → ")
                    ),
                });
            }
        }

        violations
    }
}

/// Find all cycles in a specific relation type using DFS.
/// Returns Vec of cycles, where each cycle is a Vec of node IDs forming the cycle.
fn find_cycles_in_relation(graph: &crate::model::Graph, relation: &str) -> Vec<Vec<String>> {
    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();
    let mut cycles = Vec::new();

    for node_id in graph.nodes().keys() {
        if !visited.contains(node_id) {
            let mut path = Vec::new();
            dfs_cycle(
                graph,
                node_id,
                relation,
                &mut visited,
                &mut rec_stack,
                &mut path,
                &mut cycles,
            );
        }
    }

    cycles
}

/// DFS helper for cycle detection.
/// When we encounter a node in rec_stack, we've found a cycle.
fn dfs_cycle(
    graph: &crate::model::Graph,
    node_id: &str,
    relation: &str,
    visited: &mut HashSet<String>,
    rec_stack: &mut HashSet<String>,
    path: &mut Vec<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    visited.insert(node_id.to_string());
    rec_stack.insert(node_id.to_string());
    path.push(node_id.to_string());

    // Walk the resolved edge graph, not raw frontmatter vectors: an
    // edge target is a real node id (or absent, for an unresolved
    // reference), so a cycle can only close through documents that
    // actually exist in the graph.
    for edge in graph.outgoing_edges(node_id) {
        if edge.relation != relation {
            continue;
        }
        let Some(target) = edge.target.id() else {
            continue;
        };
        if rec_stack.contains(target) {
            // Found a cycle: extract the cycle portion and close the loop
            if let Some(start_idx) = path.iter().position(|x| x == target) {
                let mut cycle = path[start_idx..].to_vec();
                // Close the cycle by appending the first node (a → b → c → a)
                if let Some(first) = cycle.first() {
                    cycle.push(first.clone());
                }
                cycles.push(cycle);
            }
        } else if !visited.contains(target) {
            dfs_cycle(graph, target, relation, visited, rec_stack, path, cycles);
        }
    }

    rec_stack.remove(node_id);
    path.pop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Graph, Kind, Node, ResolvedTarget, Status};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn implements_edge(source: &str, target: &str) -> Edge {
        Edge {
            source: source.to_string(),
            target: ResolvedTarget::resolved(target),
            relation: "implements".to_string(),
            location: "frontmatter:implements".to_string(),
        }
    }

    fn make_node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("{}.md", id)),
            title: id.to_string(),
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
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
        }
    }

    #[test]
    fn detects_simple_cycle_in_implements() {
        let mut nodes = indexmap::IndexMap::new();
        for id in ["a", "b", "c"] {
            nodes.insert(id.to_string(), make_node(id));
        }
        // cycle: a → b → c → a
        let edges = vec![
            implements_edge("a", "b"),
            implements_edge("b", "c"),
            implements_edge("c", "a"),
        ];

        let graph = Graph::new(nodes, edges, vec![], vec![]);
        let rule = CycleDetectionRule::new(vec!["implements".to_string()]);

        let config = crate::config::Config::default();
        let root = std::path::Path::new("/tmp");
        let ctx = RuleContext {
            graph: &graph,
            config: &config,
            root,
            since: None,
        };

        let violations = rule.check(&ctx);
        assert!(!violations.is_empty(), "should detect cycle a → b → c → a");
    }

    #[test]
    fn detects_self_loop() {
        let mut nodes = indexmap::IndexMap::new();
        nodes.insert("a".to_string(), make_node("a"));
        let edges = vec![implements_edge("a", "a")]; // self-loop

        let graph = Graph::new(nodes, edges, vec![], vec![]);
        let rule = CycleDetectionRule::new(vec!["implements".to_string()]);

        let config = crate::config::Config::default();
        let root = std::path::Path::new("/tmp");
        let ctx = RuleContext {
            graph: &graph,
            config: &config,
            root,
            since: None,
        };

        let violations = rule.check(&ctx);
        assert!(!violations.is_empty(), "should detect self-loop");
    }

    #[test]
    fn no_violation_on_acyclic_graph() {
        let mut nodes = indexmap::IndexMap::new();
        for id in ["a", "b", "c"] {
            nodes.insert(id.to_string(), make_node(id));
        }
        // a → b → c, no cycle
        let edges = vec![implements_edge("a", "b"), implements_edge("b", "c")];

        let graph = Graph::new(nodes, edges, vec![], vec![]);
        let rule = CycleDetectionRule::new(vec!["implements".to_string()]);

        let config = crate::config::Config::default();
        let root = std::path::Path::new("/tmp");
        let ctx = RuleContext {
            graph: &graph,
            config: &config,
            root,
            since: None,
        };

        let violations = rule.check(&ctx);
        assert!(violations.is_empty(), "should not detect cycles in DAG");
    }
}
