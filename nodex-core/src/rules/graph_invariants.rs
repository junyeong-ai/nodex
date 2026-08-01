use std::collections::{HashMap, HashSet, VecDeque};

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
            for cycle in find_cycles_in_relation(ctx.graph, relation) {
                // A cycle spans every document in the region — it is a
                // project-wide structural finding, not attributable to one
                // id. `None` keeps it whole under `--since` narrowing
                // (node-less violations are never dropped) and mirrors the
                // relational numbering rules.
                let path = cycle
                    .members
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
                        members: cycle.members,
                        ring: cycle.ring,
                    },
                ));
            }
        }

        violations
    }
}

/// A region of the relation's edge graph whose documents all reach each
/// other: every one of them, and one route around them.
struct CyclicComponent {
    /// Sorted, and the whole region — a document is in the cycle or it is
    /// not, and no rearrangement of the edges inside changes which cycle
    /// this is.
    members: Vec<String>,
    /// The shortest route from the smallest member back to itself, closed.
    /// A witness: which documents are tangled is the finding, and this is
    /// the concrete thing to break.
    ring: Vec<String>,
}

/// One entry per cyclic region of the relation's edge graph, in the order of
/// their smallest members.
///
/// A relation is a DAG exactly when none of its strongly connected
/// components is cyclic, so the components are what this reports — never
/// the rings a walk happens to close. Which rings a walk closes depends on
/// where it enters, because a node retires the first time any root reaches
/// it: a chord inside a tangle is then reported or missed according to the
/// order the graph was walked in, and the entry point moves when an edge
/// that is nowhere near the tangle moves. `Violation` equality is what the
/// proposal gates diff, so a ring that surfaced only because the walk came
/// in elsewhere reads as a cycle the mutation closed and refuses it. A
/// component decomposition is a partition of the nodes — one answer,
/// whatever the walk order — and every member of a cyclic component lies on
/// a cycle, so the tightest ring through its smallest member is a witness
/// that exists by construction.
fn find_cycles_in_relation(graph: &crate::model::Graph, relation: &str) -> Vec<CyclicComponent> {
    // Walk the resolved edge graph, not raw frontmatter vectors: an edge
    // target is a real node id (or absent, for an unresolved reference),
    // so a cycle can only close through documents that exist in the graph.
    // Sorted so the ring a component renders as is the graph's, not the
    // order its author happened to list a relation in.
    let children_of = |node: &str| -> Vec<String> {
        let mut children: Vec<String> = graph
            .outgoing_edges(node)
            .iter()
            .filter(|e| e.relation == relation)
            .filter_map(|e| e.target.id().map(str::to_string))
            .collect();
        children.sort();
        children.dedup();
        children
    };

    let node_ids: Vec<String> = graph.nodes().keys().cloned().collect();
    let mut cycles: Vec<CyclicComponent> = strongly_connected_components(&node_ids, &children_of)
        .into_iter()
        .filter_map(|component| {
            let mut members = component;
            members.sort();
            let start = members
                .first()
                .expect("a component holds the node that rooted it")
                .clone();
            // A lone node is cyclic only through an edge to itself;
            // anything larger is cyclic by what makes it a component.
            if members.len() == 1 && !children_of(&start).contains(&start) {
                return None;
            }
            let reachable: HashSet<String> = members.iter().cloned().collect();
            let ring = ring_through(&start, &reachable, &children_of);
            Some(CyclicComponent { members, ring })
        })
        .collect();
    cycles.sort_by(|a, b| a.members.cmp(&b.members));
    cycles
}

/// One stack frame of the iterative component walk: the node, its
/// relation-children (resolved once on entry), and the cursor into them.
struct ComponentFrame {
    node: String,
    children: Vec<String>,
    idx: usize,
}

/// Tarjan's bookkeeping: when each node was first reached, the lowest
/// index it can reach back to, and the nodes whose component is still open.
#[derive(Default)]
struct ComponentWalk {
    index: HashMap<String, usize>,
    lowlink: HashMap<String, usize>,
    on_stack: HashSet<String>,
    pending: Vec<String>,
    next_index: usize,
    components: Vec<Vec<String>>,
}

impl ComponentWalk {
    fn enter(&mut self, node: &str) {
        self.index.insert(node.to_string(), self.next_index);
        self.lowlink.insert(node.to_string(), self.next_index);
        self.next_index += 1;
        self.pending.push(node.to_string());
        self.on_stack.insert(node.to_string());
    }

    /// An edge into a node whose component is still open: `node` reaches at
    /// least as far back as that node was first reached.
    fn reached(&mut self, node: &str, target: &str) {
        let reached = self.index[target];
        let low = self.lowlink[node];
        self.lowlink.insert(node.to_string(), low.min(reached));
    }

    /// Leave a fully explored node: carry what it reached to its parent,
    /// and close a component when it reached nothing older than itself —
    /// everything stacked above it then reaches it and is reached back.
    fn leave(&mut self, done: &str, parent: Option<&str>) {
        let done_low = self.lowlink[done];
        if let Some(parent) = parent {
            let parent_low = self.lowlink[parent];
            self.lowlink
                .insert(parent.to_string(), parent_low.min(done_low));
        }
        if done_low != self.index[done] {
            return;
        }
        let mut component = Vec::new();
        while let Some(member) = self.pending.pop() {
            self.on_stack.remove(&member);
            let rooted_here = member == done;
            component.push(member);
            if rooted_here {
                break;
            }
        }
        self.components.push(component);
    }
}

/// Tarjan's strongly connected components, over an explicit stack.
///
/// Mirrors the discipline of `builder::validator::validate_supersedes_dag`
/// so a deep — but valid, acyclic — relation chain can never overflow the
/// call stack (the recursive form aborted `check` with SIGABRT past ~25k
/// depth, escaping the JSON envelope).
fn strongly_connected_components(
    node_ids: &[String],
    children_of: &dyn Fn(&str) -> Vec<String>,
) -> Vec<Vec<String>> {
    let mut walk = ComponentWalk::default();

    for root in node_ids {
        if walk.index.contains_key(root) {
            continue;
        }
        walk.enter(root);
        let mut stack = vec![ComponentFrame {
            node: root.clone(),
            children: children_of(root),
            idx: 0,
        }];

        while !stack.is_empty() {
            let top = stack.len() - 1;
            let frame = &mut stack[top];
            if frame.idx < frame.children.len() {
                let target = frame.children[frame.idx].clone();
                frame.idx += 1;
                let node = frame.node.clone();
                if !walk.index.contains_key(&target) {
                    walk.enter(&target);
                    let children = children_of(&target);
                    stack.push(ComponentFrame {
                        node: target,
                        children,
                        idx: 0,
                    });
                } else if walk.on_stack.contains(&target) {
                    walk.reached(&node, &target);
                }
            } else {
                // All children explored — leave the node (post-order).
                let done = stack.pop().expect("loop guard guarantees non-empty");
                let parent = stack.last().map(|frame| frame.node.clone());
                walk.leave(&done.node, parent.as_deref());
            }
        }
    }

    walk.components
}

/// The tightest ring from `start` back to `start` inside `component`,
/// closed by repeating `start`.
///
/// Breadth-first with the smallest neighbour taken first, so a component
/// renders as one ring whatever order it was discovered in. A cyclic
/// component reaches its own members from every member, so the walk always
/// closes.
fn ring_through(
    start: &str,
    component: &HashSet<String>,
    children_of: &dyn Fn(&str) -> Vec<String>,
) -> Vec<String> {
    let mut came_from: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::from([start.to_string()]);
    let mut queue: VecDeque<String> = VecDeque::from([start.to_string()]);

    while let Some(node) = queue.pop_front() {
        for target in children_of(&node) {
            if target == start {
                let mut ring = vec![node.clone()];
                let mut cursor = node.clone();
                while let Some(previous) = came_from.get(&cursor) {
                    ring.push(previous.clone());
                    cursor = previous.clone();
                }
                ring.reverse();
                ring.push(start.to_string());
                return ring;
            }
            if component.contains(&target) && seen.insert(target.clone()) {
                came_from.insert(target.clone(), node.clone());
                queue.push_back(target);
            }
        }
    }

    unreachable!("a cyclic component reaches {start} from every member")
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
            today: crate::test_today(),
            graph: &graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(root),
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
            today: crate::test_today(),
            graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(std::path::Path::new(
                "/tmp",
            )),
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
            today: crate::test_today(),
            graph: &graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(root),
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
            today: crate::test_today(),
            graph: &graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(root),
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
            today: crate::test_today(),
            graph: &graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(std::path::Path::new(
                "/tmp",
            )),
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
