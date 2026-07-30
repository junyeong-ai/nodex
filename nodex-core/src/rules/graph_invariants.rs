use std::collections::HashSet;

use super::{Rule, RuleContext, Severity, Violation, ViolationDetails};

/// Detects cycles in directed graph relations that must form a DAG —
/// a cycle is a design defect (circular dependency). The relation set
/// comes from `rules.acyclic_relations`; `Config::validate` guarantees
/// it is non-empty and every entry is a known relation.
pub struct CycleDetectionRule {
    /// Relations to check for cycles; sourced from
    /// `rules.acyclic_relations`, never empty (load-rejected).
    pub relations: Vec<String>,
}

impl CycleDetectionRule {
    pub fn new(relations: Vec<String>) -> Self {
        Self { relations }
    }
}

impl Rule for CycleDetectionRule {
    fn id(&self) -> &str {
        "acyclic_relation"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Documents must not form cycles in the configured acyclic relations"
    }

    fn params(
        &self,
        _config: &crate::config::Config,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("relations".into(), serde_json::json!(self.relations));
        m
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let mut violations = Vec::new();

        for relation in &self.relations {
            let cycles = find_cycles_in_relation(ctx.graph, relation);
            for cycle in cycles {
                // A cycle spans every node on the ring — it is a
                // project-wide structural finding, not attributable to one
                // id. `None` keeps it whole under `--since` narrowing
                // (node-less violations are never dropped) and mirrors the
                // relational numbering rules.
                let path = cycle
                    .first()
                    .and_then(|first| ctx.graph.nodes().get(first))
                    .map(|n| crate::path_guard::forward_string(&n.path));
                violations.push(Violation::new(
                    self.id(),
                    self.severity(),
                    None,
                    path,
                    ViolationDetails::Cycle {
                        relation: relation.clone(),
                        ring: cycle,
                    },
                ));
            }
        }

        violations
    }
}

/// Find all cycles in a specific relation type using DFS.
/// Returns Vec of cycles, where each cycle is a Vec of node IDs forming the cycle.
fn find_cycles_in_relation(graph: &crate::model::Graph, relation: &str) -> Vec<Vec<String>> {
    let mut visited = HashSet::new();
    let mut cycles = Vec::new();

    for node_id in graph.nodes().keys() {
        if !visited.contains(node_id) {
            dfs_cycle(graph, node_id, relation, &mut visited, &mut cycles);
        }
    }

    cycles
}

/// One stack frame of the iterative DFS: the node, its relation-children
/// (resolved once on entry), and the cursor into them.
struct CycleFrame {
    node: String,
    children: Vec<String>,
    idx: usize,
}

/// Iterative 3-color DFS for cycle detection from `start`. Mirrors the
/// explicit-stack discipline of `builder::validator::validate_supersedes_dag`
/// so a deep — but valid, acyclic — relation chain can never overflow the
/// call stack (the recursive form aborted `check` with SIGABRT past ~25k
/// depth, escaping the JSON envelope). `rec_stack` marks the active path
/// (a back-edge into it closes a cycle) and `path` records it for ring
/// extraction; `visited` and `cycles` are shared across roots.
fn dfs_cycle(
    graph: &crate::model::Graph,
    start: &str,
    relation: &str,
    visited: &mut HashSet<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    // Walk the resolved edge graph, not raw frontmatter vectors: an edge
    // target is a real node id (or absent, for an unresolved reference),
    // so a cycle can only close through documents that exist in the graph.
    let children_of = |node: &str| -> Vec<String> {
        graph
            .outgoing_edges(node)
            .iter()
            .filter(|e| e.relation == relation)
            .filter_map(|e| e.target.id().map(str::to_string))
            .collect()
    };

    let mut rec_stack: HashSet<String> = HashSet::new();
    let mut path: Vec<String> = Vec::new();

    visited.insert(start.to_string());
    rec_stack.insert(start.to_string());
    path.push(start.to_string());
    let mut stack = vec![CycleFrame {
        node: start.to_string(),
        children: children_of(start),
        idx: 0,
    }];

    while !stack.is_empty() {
        let top = stack.len() - 1;
        let frame = &mut stack[top];
        if frame.idx < frame.children.len() {
            let target = frame.children[frame.idx].clone();
            frame.idx += 1;
            if rec_stack.contains(&target) {
                // Back-edge into the active path: extract the ring and
                // close the loop (a → b → c → a).
                if let Some(start_idx) = path.iter().position(|x| *x == target) {
                    let mut cycle = path[start_idx..].to_vec();
                    if let Some(first) = cycle.first() {
                        cycle.push(first.clone());
                    }
                    cycles.push(cycle);
                }
            } else if !visited.contains(&target) {
                visited.insert(target.clone());
                rec_stack.insert(target.clone());
                path.push(target.clone());
                let children = children_of(&target);
                stack.push(CycleFrame {
                    node: target,
                    children,
                    idx: 0,
                });
            }
        } else {
            // All children explored — leave the node (post-order).
            let done = stack.pop().expect("loop guard guarantees non-empty");
            rec_stack.remove(&done.node);
            path.pop();
        }
    }
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
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
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

        let graph = Graph::new(
            nodes,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );
        let rule = CycleDetectionRule::new(vec!["implements".to_string()]);

        let config = crate::config::Config::default();
        let root = std::path::Path::new("/tmp");
        let ctx = RuleContext {
            graph: &graph,
            config: &config,
            root,
            repository: None,
            since: None,
        };

        let violations = rule.check(&ctx);
        assert!(!violations.is_empty(), "should detect cycle a → b → c → a");
        // A cycle is project-wide: it must not be pinned to a single
        // node id, so `check --since` never narrows it away. The path
        // still points at a representative ring member for navigation.
        assert!(
            violations.iter().all(|v| v.node_id.is_none()),
            "cycle violations must be node-less: {violations:?}"
        );
        assert!(
            violations.iter().all(|v| v.path.is_some()),
            "cycle violations carry a representative path: {violations:?}"
        );
    }

    fn deep_implements_chain(depth: usize, close_cycle: bool) -> Graph {
        let mut nodes = indexmap::IndexMap::new();
        let mut edges = Vec::new();
        for i in 0..depth {
            nodes.insert(format!("n{i}"), make_node(&format!("n{i}")));
            if i + 1 < depth {
                edges.push(implements_edge(&format!("n{i}"), &format!("n{}", i + 1)));
            }
        }
        if close_cycle && depth > 1 {
            // Tail → head closes the chain into one big ring.
            edges.push(implements_edge(&format!("n{}", depth - 1), "n0"));
        }
        Graph::new(
            nodes,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    fn cycle_violations(graph: &Graph) -> Vec<crate::rules::Violation> {
        let rule = CycleDetectionRule::new(vec!["implements".to_string()]);
        let config = crate::config::Config::default();
        let ctx = RuleContext {
            graph,
            config: &config,
            root: std::path::Path::new("/tmp"),
            repository: None,
            since: None,
        };
        rule.check(&ctx)
    }

    #[test]
    fn deep_acyclic_chain_does_not_overflow_the_stack() {
        // A long but VALID (acyclic) `implements` chain must not blow the
        // call stack. The recursive DFS aborted `check` with SIGABRT on
        // deep chains (escaping the JSON envelope); the iterative form
        // walks a heap stack. 50k exceeds the 2 MB test-thread stack the
        // recursive version overflowed an order of magnitude below.
        let graph = deep_implements_chain(50_000, false);
        assert!(
            cycle_violations(&graph).is_empty(),
            "an acyclic chain must report no cycle"
        );
    }

    #[test]
    fn deep_chain_closing_into_a_cycle_is_still_detected() {
        // Correctness preserved at depth: a tail→head back-edge over a
        // long chain still reports the ring (node-less, project-wide).
        let graph = deep_implements_chain(50_000, true);
        let violations = cycle_violations(&graph);
        assert!(!violations.is_empty(), "deep cycle must still be detected");
        assert!(violations.iter().all(|v| v.node_id.is_none()));
    }

    #[test]
    fn detects_self_loop() {
        let mut nodes = indexmap::IndexMap::new();
        nodes.insert("a".to_string(), make_node("a"));
        let edges = vec![implements_edge("a", "a")]; // self-loop

        let graph = Graph::new(
            nodes,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );
        let rule = CycleDetectionRule::new(vec!["implements".to_string()]);

        let config = crate::config::Config::default();
        let root = std::path::Path::new("/tmp");
        let ctx = RuleContext {
            graph: &graph,
            config: &config,
            root,
            repository: None,
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

        let graph = Graph::new(
            nodes,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );
        let rule = CycleDetectionRule::new(vec!["implements".to_string()]);

        let config = crate::config::Config::default();
        let root = std::path::Path::new("/tmp");
        let ctx = RuleContext {
            graph: &graph,
            config: &config,
            root,
            repository: None,
            since: None,
        };

        let violations = rule.check(&ctx);
        assert!(violations.is_empty(), "should not detect cycles in DAG");
    }

    #[test]
    fn checks_exactly_the_configured_relations() {
        // A project that declares `depends_on` acyclic (and not
        // `implements`) must get cycle detection on depends_on edges
        // only — the relation set is config-sourced, never hardcoded.
        let custom_edge = |source: &str, target: &str| Edge {
            source: source.to_string(),
            target: ResolvedTarget::resolved(target),
            relation: "depends_on".to_string(),
            location: "body:depends_on".to_string(),
        };
        let mut nodes = indexmap::IndexMap::new();
        for id in ["a", "b"] {
            nodes.insert(id.to_string(), make_node(id));
        }
        // Both relations cycle; only depends_on is configured.
        let edges = vec![
            custom_edge("a", "b"),
            custom_edge("b", "a"),
            implements_edge("a", "b"),
            implements_edge("b", "a"),
        ];
        let graph = Graph::new(
            nodes,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );
        let rule = CycleDetectionRule::new(vec!["depends_on".to_string()]);

        let config = crate::config::Config::default();
        let ctx = RuleContext {
            graph: &graph,
            config: &config,
            root: std::path::Path::new("/tmp"),
            repository: None,
            since: None,
        };

        let violations = rule.check(&ctx);
        assert!(!violations.is_empty(), "depends_on cycle must fire");
        assert!(
            violations.iter().all(|v| v.message.contains("depends_on")),
            "only the configured relation is checked: {violations:?}"
        );
    }

    #[test]
    fn description_names_no_specific_relation() {
        // The JSON-emitted description must stay truthful for projects
        // configuring other relations; the live set is in `params()`.
        let rule = CycleDetectionRule::new(vec!["depends_on".to_string()]);
        assert!(!rule.description().contains("implements"));
        let params = rule.params(&crate::config::Config::default());
        assert_eq!(
            params.get("relations"),
            Some(&serde_json::json!(["depends_on"]))
        );
    }
}
