//! Token-budgeted context bundle. Given a seed node, walk supersession
//! / backlinks / references and return the largest reading list that
//! fits a caller-supplied token budget.
//!
//! Traversal is BFS by depth, with two priorities inside each depth:
//! healthy nodes are processed before terminal-status ones (defending
//! the "stale world model" failure mode), and within the same priority
//! class items are processed in insertion order. Implemented with a
//! min-heap keyed on `(depth, priority, sequence)` so the order is
//! both correct (no depth-N+1 jumps over depth-N) and deterministic.

use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap};
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{Graph, Node, ResolvedTarget};

use super::NodeRef;

#[derive(Debug, Serialize)]
pub struct PackedNode {
    #[serde(flatten)]
    pub node: NodeRef,
    /// `seed`, `superseded_by:<src>`, `supersedes:<dst>`, `backlink:<src>`,
    /// or `reference:<src>` — explains why this node was included.
    pub reason: String,
    pub depth: u32,
    pub tokens: usize,
    pub body_excerpt: String,
}

#[derive(Debug, Serialize)]
pub struct Pack {
    pub seed: String,
    pub token_budget: usize,
    pub total_tokens: usize,
    pub included: Vec<PackedNode>,
    /// Node ids the walk reached but couldn't fit under the budget.
    pub excluded: Vec<String>,
    pub max_depth: u32,
}

/// Default token budget for `nodex pack` and the MCP `nodex_pack`
/// tool. Sized for one Anthropic system-prompt-sized response.
pub const DEFAULT_TOKEN_BUDGET: usize = 4_000;

/// Default BFS depth — seed + immediate neighbours + grandparents.
pub const DEFAULT_MAX_DEPTH: u32 = 2;

/// Approximate tokens for arbitrary text. The English/code average is
/// ~4 chars/token across both GPT and Claude tokenisers; close enough
/// for budget arithmetic without pulling in a tokeniser dependency.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Build a context pack rooted at `seed_id`. `max_depth` caps how far
/// the walk may travel from the seed.
pub fn build_pack(
    graph: &Graph,
    config: &Config,
    root: &Path,
    seed_id: &str,
    token_budget: usize,
    max_depth: u32,
) -> Result<Pack> {
    let seed = graph
        .node(seed_id)
        .ok_or_else(|| Error::MissingNode(seed_id.to_string()))?;

    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut included: Vec<PackedNode> = Vec::new();
    let mut excluded: Vec<String> = Vec::new();
    let mut total_tokens: usize = 0;
    let mut seq: u64 = 0;

    visited.insert(seed.id.clone());
    // Seed file missing on disk is a hard failure — the caller asked
    // for context anchored on a node that no longer exists.
    let seed_packed = pack_node(root, seed, "seed", 0)?;
    total_tokens += seed_packed.tokens;
    included.push(seed_packed);

    let mut frontier: BinaryHeap<Reverse<Queued>> = BinaryHeap::new();
    enqueue_neighbours(graph, config, &seed.id, 1, &mut frontier, &mut seq);

    while let Some(Reverse(Queued {
        depth, id, reason, ..
    })) = frontier.pop()
    {
        if depth > max_depth {
            continue;
        }
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(node) = graph.node(&id) else {
            continue;
        };
        // A neighbour file missing on disk (graph indexed it before
        // someone deleted/moved it) must not abort the entire pack —
        // surface it in `excluded` and keep walking the rest.
        let packed = match pack_node(root, node, &reason, depth) {
            Ok(p) => p,
            Err(Error::Io { .. }) => {
                excluded.push(id);
                continue;
            }
            Err(other) => return Err(other),
        };
        if total_tokens + packed.tokens > token_budget {
            excluded.push(packed.node.id);
            continue;
        }
        total_tokens += packed.tokens;
        included.push(packed);
        if depth < max_depth {
            enqueue_neighbours(graph, config, &node.id, depth + 1, &mut frontier, &mut seq);
        }
    }

    Ok(Pack {
        seed: seed_id.to_string(),
        token_budget,
        total_tokens,
        included,
        excluded,
        max_depth,
    })
}

/// Min-heap entry. Field order is significant: derived `Ord` compares
/// `depth` first (BFS layer), then `priority` (healthy before terminal),
/// then `seq` (FIFO tiebreaker) — `id` and `reason` never participate
/// because `seq` is unique per push.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Queued {
    depth: u32,
    priority: u8,
    seq: u64,
    id: String,
    reason: String,
}

const PRIORITY_HEALTHY: u8 = 0;
const PRIORITY_TERMINAL: u8 = 1;

/// Push every neighbour of `node_id` onto the frontier. Within each
/// depth, healthy nodes are popped before terminal ones, with FIFO
/// order inside each priority class.
fn enqueue_neighbours(
    graph: &Graph,
    config: &Config,
    node_id: &str,
    depth: u32,
    frontier: &mut BinaryHeap<Reverse<Queued>>,
    seq: &mut u64,
) {
    // Supersession forward: this node → its successor.
    for edge in graph.outgoing_edges(node_id) {
        if edge.relation != "supersedes" {
            continue;
        }
        if let ResolvedTarget::Resolved { id } = &edge.target {
            push(
                graph,
                config,
                frontier,
                seq,
                id,
                format!("superseded_by:{node_id}"),
                depth,
            );
        }
    }
    // Supersession backward: what this node replaced.
    for edge in graph.incoming_edges(node_id) {
        if edge.relation != "supersedes" {
            continue;
        }
        push(
            graph,
            config,
            frontier,
            seq,
            &edge.source,
            format!("supersedes:{node_id}"),
            depth,
        );
    }
    // Backlinks (every incoming relation other than supersedes).
    for edge in graph.incoming_edges(node_id) {
        if edge.relation == "supersedes" {
            continue;
        }
        push(
            graph,
            config,
            frontier,
            seq,
            &edge.source,
            format!("backlink:{node_id}"),
            depth,
        );
    }
    // Outgoing references (every outgoing relation other than supersedes).
    for edge in graph.outgoing_edges(node_id) {
        if edge.relation == "supersedes" {
            continue;
        }
        if let ResolvedTarget::Resolved { id } = &edge.target {
            push(
                graph,
                config,
                frontier,
                seq,
                id,
                format!("reference:{node_id}"),
                depth,
            );
        }
    }
}

fn push(
    graph: &Graph,
    config: &Config,
    frontier: &mut BinaryHeap<Reverse<Queued>>,
    seq: &mut u64,
    id: &str,
    reason: String,
    depth: u32,
) {
    let priority = match graph.node(id) {
        Some(n) if !config.is_terminal(n.status.as_str()) => PRIORITY_HEALTHY,
        _ => PRIORITY_TERMINAL,
    };
    *seq += 1;
    frontier.push(Reverse(Queued {
        depth,
        priority,
        seq: *seq,
        id: id.to_string(),
        reason,
    }));
}

fn pack_node(root: &Path, node: &Node, reason: &str, depth: u32) -> Result<PackedNode> {
    let abs_path = root.join(&node.path);
    let body = std::fs::read_to_string(&abs_path).map_err(|source| Error::Io {
        path: abs_path,
        source,
    })?;
    let (_, body_only) = crate::parser::frontmatter::split_frontmatter(&body);
    let excerpt = take_excerpt(body_only, 1200);
    let tokens = estimate_tokens(&excerpt);
    Ok(PackedNode {
        node: NodeRef::from_node(node),
        reason: reason.to_string(),
        depth,
        tokens,
        body_excerpt: excerpt,
    })
}

/// Take up to `max_chars` of `text`, breaking on the closest preceding
/// blank line so the excerpt ends at a paragraph boundary when possible.
fn take_excerpt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.trim().to_string();
    }
    let cutoff_byte = text
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let head = &text[..cutoff_byte];
    let split_at = head.rfind("\n\n").unwrap_or(cutoff_byte);
    text[..split_at].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Kind, ResolvedTarget, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn estimate_tokens_is_chars_div_ceil_four() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens("hello world"), 3);
    }

    #[test]
    fn excerpt_breaks_on_paragraph() {
        let text =
            "para one is short.\n\npara two carries on much longer than the cutoff would allow.";
        let excerpt = take_excerpt(text, 25);
        assert_eq!(excerpt, "para one is short.");
    }

    fn write_doc(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, body).unwrap();
    }

    fn make_node(id: &str, status: &str, rel_path: &str) -> Node {
        Node {
            id: id.to_string(),
            path: std::path::PathBuf::from(rel_path),
            title: id.to_string(),
            kind: Kind::new("generic"),
            status: Status::new(status),
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
        }
    }

    fn three_node_fixture() -> (TempDir, Graph, Config) {
        let tmp = TempDir::new().unwrap();
        write_doc(
            tmp.path(),
            "seed.md",
            "---\nid: seed\n---\n# Seed\n\nSeed body text.\n",
        );
        write_doc(
            tmp.path(),
            "a.md",
            "---\nid: a\n---\n# A\n\nHealthy doc body.\n",
        );
        write_doc(
            tmp.path(),
            "b.md",
            "---\nid: b\nstatus: archived\n---\n# B\n\nArchived doc body.\n",
        );
        let mut nodes = IndexMap::new();
        nodes.insert("seed".to_string(), make_node("seed", "active", "seed.md"));
        nodes.insert("a".to_string(), make_node("a", "active", "a.md"));
        nodes.insert("b".to_string(), make_node("b", "archived", "b.md"));
        let edges = vec![
            Edge {
                source: "seed".to_string(),
                target: ResolvedTarget::resolved("a"),
                relation: "references".to_string(),
                location: "L1".to_string(),
            },
            Edge {
                source: "seed".to_string(),
                target: ResolvedTarget::resolved("b"),
                relation: "references".to_string(),
                location: "L2".to_string(),
            },
        ];
        let graph = Graph::new(nodes, edges);
        let config = Config::default();
        (tmp, graph, config)
    }

    #[test]
    fn max_depth_zero_returns_only_seed() {
        let (tmp, graph, config) = three_node_fixture();
        let pack = build_pack(&graph, &config, tmp.path(), "seed", 10_000, 0).unwrap();
        let ids: Vec<&str> = pack.included.iter().map(|n| n.node.id.as_str()).collect();
        assert_eq!(ids, vec!["seed"], "depth=0 must not walk neighbours");
    }

    #[test]
    fn healthy_neighbours_outrank_terminal_in_included_order() {
        let (tmp, graph, config) = three_node_fixture();
        let pack = build_pack(&graph, &config, tmp.path(), "seed", 10_000, 1).unwrap();
        let ids: Vec<&str> = pack.included.iter().map(|n| n.node.id.as_str()).collect();
        let pos_a = ids.iter().position(|x| *x == "a").unwrap();
        let pos_b = ids.iter().position(|x| *x == "b").unwrap();
        assert!(
            pos_a < pos_b,
            "active node must be packed before archived one"
        );
    }

    #[test]
    fn tight_budget_excludes_overflow_without_duplicates() {
        let (tmp, graph, config) = three_node_fixture();
        let pack = build_pack(&graph, &config, tmp.path(), "seed", 10, 1).unwrap();
        for ex in &pack.excluded {
            assert!(
                !pack.included.iter().any(|n| &n.node.id == ex),
                "excluded id {ex:?} also appears in included"
            );
        }
        let mut seen = std::collections::HashSet::new();
        for ex in &pack.excluded {
            assert!(seen.insert(ex), "excluded contained {ex:?} twice");
        }
    }

    #[test]
    fn missing_neighbour_file_excluded_not_fatal() {
        // Graph indexes `b`, but the file was deleted after build.
        // The pack must still return successfully, listing `b` in
        // `excluded` and including the rest.
        let (tmp, graph, config) = three_node_fixture();
        std::fs::remove_file(tmp.path().join("b.md")).unwrap();
        let pack = build_pack(&graph, &config, tmp.path(), "seed", 10_000, 1).unwrap();
        let included_ids: Vec<&str> = pack.included.iter().map(|n| n.node.id.as_str()).collect();
        assert!(included_ids.contains(&"seed"));
        assert!(included_ids.contains(&"a"));
        assert!(
            pack.excluded.iter().any(|id| id == "b"),
            "missing neighbour must surface in excluded; got {:?}",
            pack.excluded
        );
    }

    #[test]
    fn missing_seed_returns_missing_node_error() {
        let (tmp, graph, config) = three_node_fixture();
        let err = build_pack(&graph, &config, tmp.path(), "ghost", 10_000, 2).unwrap_err();
        assert!(matches!(err, Error::MissingNode(_)));
    }

    #[test]
    fn bfs_processes_depth_one_terminal_before_depth_two_healthy() {
        // seed → a (terminal, depth 1)
        // seed → b (healthy, depth 1)
        // b → c (healthy, depth 2)
        // The BFS layer guarantee: a (depth 1) must be processed
        // before c (depth 2), even though c is healthier.
        let tmp = TempDir::new().unwrap();
        for (rel, body) in [
            ("seed.md", "---\nid: seed\n---\n# Seed\n"),
            ("a.md", "---\nid: a\nstatus: archived\n---\n# A\n"),
            ("b.md", "---\nid: b\n---\n# B\n"),
            ("c.md", "---\nid: c\n---\n# C\n"),
        ] {
            write_doc(tmp.path(), rel, body);
        }
        let mut nodes = IndexMap::new();
        nodes.insert("seed".into(), make_node("seed", "active", "seed.md"));
        nodes.insert("a".into(), make_node("a", "archived", "a.md"));
        nodes.insert("b".into(), make_node("b", "active", "b.md"));
        nodes.insert("c".into(), make_node("c", "active", "c.md"));
        let edges = vec![
            Edge {
                source: "seed".into(),
                target: ResolvedTarget::resolved("a"),
                relation: "references".into(),
                location: "L1".into(),
            },
            Edge {
                source: "seed".into(),
                target: ResolvedTarget::resolved("b"),
                relation: "references".into(),
                location: "L2".into(),
            },
            Edge {
                source: "b".into(),
                target: ResolvedTarget::resolved("c"),
                relation: "references".into(),
                location: "L1".into(),
            },
        ];
        let graph = Graph::new(nodes, edges);
        let config = Config::default();
        let pack = build_pack(&graph, &config, tmp.path(), "seed", 100_000, 5).unwrap();
        let ids: Vec<&str> = pack.included.iter().map(|n| n.node.id.as_str()).collect();
        let pos_a = ids
            .iter()
            .position(|x| *x == "a")
            .expect("a must be included");
        let pos_c = ids
            .iter()
            .position(|x| *x == "c")
            .expect("c must be included");
        assert!(
            pos_a < pos_c,
            "depth-1 terminal must be processed before depth-2 healthy; got order {ids:?}"
        );
    }
}
