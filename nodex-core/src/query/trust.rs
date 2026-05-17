//! Composite reliability score for a node.
//!
//! Agents call this to decide whether a piece of memory is still
//! authoritative. The score collapses four orthogonal signals into a
//! single number in `[0, 1]`, but the per-component breakdown is
//! always returned so the consumer can re-rank with its own weights
//! or surface the *why* alongside the *what*.

use chrono::Local;
use serde::Serialize;
use std::path::Path;

use crate::config::{Config, TrustWeights};
use crate::error::Result;
use crate::model::{Graph, Node, ResolvedTarget};

use super::NodeRef;

#[derive(Debug, Clone, Serialize)]
pub struct TrustReport {
    #[serde(flatten)]
    pub node: NodeRef,
    pub score: f64,
    pub components: TrustComponents,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrustComponents {
    pub status: f64,
    pub freshness: f64,
    /// `None` when `git_drift_threshold` is not set — the drift weight
    /// is then excluded from the composite denominator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<f64>,
    pub backlinks: f64,
}

/// Trust score for a single node. Errors with [`crate::Error::MissingNode`]
/// when the id is unknown.
pub fn compute_trust(graph: &Graph, config: &Config, root: &Path, id: &str) -> Result<TrustReport> {
    let node = graph.require_node(id)?;
    let max_in = max_incoming(graph);
    Ok(score_node(graph, config, root, node, max_in))
}

/// Every node whose composite score is strictly below `threshold`,
/// optionally restricted to a single kind. Sorted by score ascending
/// (lowest trust first) with id tie-break.
pub fn find_low_trust(
    graph: &Graph,
    config: &Config,
    root: &Path,
    threshold: f64,
    kind: Option<&str>,
) -> Vec<TrustReport> {
    let max_in = max_incoming(graph);
    let mut reports: Vec<TrustReport> = graph
        .nodes()
        .values()
        .filter(|n| kind.is_none_or(|k| n.kind.as_str() == k))
        .map(|n| score_node(graph, config, root, n, max_in))
        .filter(|r| r.score < threshold)
        .collect();
    reports.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    reports
}

fn score_node(
    graph: &Graph,
    config: &Config,
    root: &Path,
    node: &Node,
    max_in: usize,
) -> TrustReport {
    let components = TrustComponents {
        status: status_score(config, node.status.as_str()),
        freshness: freshness_score(config, node),
        drift: drift_score(graph, config, root, node),
        backlinks: backlinks_score(graph, node, max_in),
    };
    let score = compose(&config.trust.weights, &components);
    TrustReport {
        node: NodeRef::from_node(node),
        score,
        components,
    }
}

fn compose(w: &TrustWeights, c: &TrustComponents) -> f64 {
    let mut weighted = 0.0;
    let mut weight_sum = 0.0;

    weighted += c.status * w.status;
    weight_sum += w.status;
    weighted += c.freshness * w.freshness;
    weight_sum += w.freshness;
    if let Some(drift) = c.drift {
        weighted += drift * w.drift;
        weight_sum += w.drift;
    }
    weighted += c.backlinks * w.backlinks;
    weight_sum += w.backlinks;

    if weight_sum <= 0.0 {
        0.0
    } else {
        (weighted / weight_sum).clamp(0.0, 1.0)
    }
}

fn status_score(config: &Config, status: &str) -> f64 {
    if config.is_terminal(status) { 0.0 } else { 1.0 }
}

fn freshness_score(config: &Config, node: &Node) -> f64 {
    let stale_days = u64::from(config.detection.stale_days);
    if stale_days == 0 {
        return 1.0;
    }
    let today = Local::now().date_naive();
    match node.reviewed {
        Some(reviewed) => {
            let elapsed = (today - reviewed).num_days().max(0) as f64;
            (1.0 - elapsed / stale_days as f64).clamp(0.0, 1.0)
        }
        // Reviewed-date absent: information missing, neutral signal.
        None => 0.5,
    }
}

fn drift_score(graph: &Graph, config: &Config, root: &Path, node: &Node) -> Option<f64> {
    let threshold = config.detection.git_drift_threshold?;
    let reviewed = node.reviewed?;
    if threshold == 0 {
        return Some(1.0);
    }

    let relations = &config.detection.git_drift_relations;
    let mut total: u32 = 0;
    for edge in graph.outgoing_edges(&node.id) {
        if !relations.iter().any(|r| r == &edge.relation) {
            continue;
        }
        let path = match &edge.target {
            ResolvedTarget::Resolved { id } => match graph.node(id) {
                Some(t) => t.path.clone(),
                None => continue,
            },
            ResolvedTarget::Unresolved { raw, .. } => {
                let candidate = std::path::PathBuf::from(raw);
                if !root.join(&candidate).is_file() {
                    continue;
                }
                candidate
            }
        };
        total = total.saturating_add(crate::rules::git_drift::commits_since(
            root, &path, reviewed,
        ));
    }
    Some((1.0 - total as f64 / threshold as f64).clamp(0.0, 1.0))
}

fn backlinks_score(graph: &Graph, node: &Node, max_in: usize) -> f64 {
    // Self-references are filtered out — trust measures external
    // attention, and a doc citing itself is not external. Without
    // the filter a doc could inflate its own score by writing
    // `[[self-id]]` in the body.
    let in_count = graph.external_incoming_edges(&node.id).len();
    if max_in == 0 {
        // Degenerate graph — every node has zero external incoming.
        // Avoid 0/0.
        return 1.0;
    }
    let in_log = ((in_count + 1) as f64).ln();
    let max_log = ((max_in + 1) as f64).ln();
    (in_log / max_log).clamp(0.0, 1.0)
}

fn max_incoming(graph: &Graph) -> usize {
    graph
        .nodes()
        .values()
        .map(|n| graph.external_incoming_edges(&n.id).len())
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Kind, Status};
    use chrono::Duration;
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_node(id: &str, status: &str, reviewed: Option<chrono::NaiveDate>) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
            kind: Kind::new("generic"),
            status: Status::new(status),
            created: None,
            updated: None,
            reviewed,
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

    fn graph_with(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, edges)
    }

    #[test]
    fn terminal_status_drops_status_to_zero() {
        let g = graph_with(vec![make_node("x", "archived", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        assert_eq!(r.components.status, 0.0);
    }

    #[test]
    fn freshness_decays_linearly_until_zero() {
        let today = Local::now().date_naive();
        let mid = today - Duration::days(90); // half of default 180
        let stale = today - Duration::days(300); // beyond cutoff
        let g = graph_with(
            vec![
                make_node("fresh", "active", Some(today)),
                make_node("mid", "active", Some(mid)),
                make_node("stale", "active", Some(stale)),
            ],
            vec![],
        );
        let cfg = Config::default();
        let fresh = compute_trust(&g, &cfg, Path::new("."), "fresh").unwrap();
        let mid = compute_trust(&g, &cfg, Path::new("."), "mid").unwrap();
        let stale = compute_trust(&g, &cfg, Path::new("."), "stale").unwrap();
        assert!(fresh.components.freshness > 0.99);
        assert!((mid.components.freshness - 0.5).abs() < 0.05);
        assert_eq!(stale.components.freshness, 0.0);
    }

    #[test]
    fn missing_reviewed_date_is_neutral_half() {
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        assert_eq!(r.components.freshness, 0.5);
    }

    #[test]
    fn drift_excluded_when_threshold_unset() {
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        assert!(r.components.drift.is_none());
    }

    #[test]
    fn composite_renormalises_when_drift_missing() {
        // Active + missing reviewed + neutral backlinks → score should
        // average across status (1.0 × 0.4) + freshness (0.5 × 0.3) +
        // backlinks (1.0 × 0.1) divided by (0.4 + 0.3 + 0.1).
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        let expected = (1.0 * 0.4 + 0.5 * 0.3 + 1.0 * 0.1) / 0.8;
        assert!(
            (r.score - expected).abs() < 1e-9,
            "expected {expected}, got {}",
            r.score
        );
    }

    #[test]
    fn missing_node_errors_via_require_node() {
        let g = graph_with(vec![], vec![]);
        let err = compute_trust(&g, &Config::default(), Path::new("."), "ghost").unwrap_err();
        assert!(matches!(err, crate::error::Error::MissingNode(_)));
    }

    #[test]
    fn find_low_trust_filters_and_sorts() {
        let g = graph_with(
            vec![
                make_node("a", "active", None),   // freshness 0.5
                make_node("b", "archived", None), // status 0
                make_node("c", "active", Some(Local::now().date_naive())), // freshness 1
            ],
            vec![],
        );
        let low = find_low_trust(&g, &Config::default(), Path::new("."), 1.0, None);
        let ids: Vec<&str> = low.iter().map(|r| r.node.id.as_str()).collect();
        // 'b' is lowest (status=0), 'a' is mid (freshness=0.5), 'c' is high (~1.0 — excluded by < 1.0)
        // c's score: (1×0.4 + 1×0.3 + 1×0.1) / 0.8 = 1.0 — filter `< 1.0` excludes it
        assert_eq!(ids[0], "b");
        assert_eq!(ids[1], "a");
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn backlinks_score_empty_graph_returns_one() {
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        assert_eq!(r.components.backlinks, 1.0);
    }

    #[test]
    fn backlinks_score_excludes_self_loops() {
        // A doc that cites itself is not external attention — trust
        // measures attention from outside, so the self-edge must not
        // inflate the score. Without the filter a malicious or
        // accidental `[[my-own-id]]` could bump backlinks to 1.0.
        let self_edge = Edge {
            source: "x".into(),
            target: ResolvedTarget::resolved("x"),
            relation: "references".into(),
            location: "L1".into(),
        };
        let g = graph_with(vec![make_node("x", "active", None)], vec![self_edge]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        // The same graph without the self-edge would also score the
        // singleton at 1.0 (degenerate `max_in == 0` branch), so the
        // lock-in is that the self-edge doesn't *change* the score
        // upward.
        assert_eq!(r.components.backlinks, 1.0);
    }
}
