//! Composite reliability score for a node.
//!
//! Agents call this to decide whether a piece of memory is still
//! authoritative. The score collapses four orthogonal signals into a
//! single number in `[0, 1]`, but the per-component breakdown is
//! always returned so the consumer can re-rank with its own weights
//! or surface the *why* alongside the *what*.

use chrono::Local;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::Path;

use crate::config::{Config, TrustWeights};
use crate::error::Result;
use crate::model::{Graph, Node, ResolvedTarget};

use super::NodeRef;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TrustReport {
    #[serde(flatten)]
    pub node: NodeRef,
    pub score: f64,
    pub components: TrustComponents,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TrustComponents {
    pub status: f64,
    /// `None` when `reviewed` is unset on the node — the freshness
    /// weight is then excluded from the composite denominator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<f64>,
    /// `None` when `git_drift_threshold` is not set — the drift weight
    /// is then excluded from the composite denominator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<f64>,
    /// `None` when the graph carries no external incoming edges on any
    /// node — there is no signal to compare against, so the backlinks
    /// weight is excluded from the composite denominator rather than
    /// fabricating a `1.0` from absence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlinks: Option<f64>,
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
    let weights = config.trust_weights_for(node.kind.as_str());
    let score = compose(&weights, &components);
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
    if let Some(freshness) = c.freshness {
        weighted += freshness * w.freshness;
        weight_sum += w.freshness;
    }
    if let Some(drift) = c.drift {
        weighted += drift * w.drift;
        weight_sum += w.drift;
    }
    if let Some(backlinks) = c.backlinks {
        weighted += backlinks * w.backlinks;
        weight_sum += w.backlinks;
    }

    if weight_sum <= 0.0 {
        0.0
    } else {
        (weighted / weight_sum).clamp(0.0, 1.0)
    }
}

fn status_score(config: &Config, status: &str) -> f64 {
    if config.is_terminal(status) { 0.0 } else { 1.0 }
}

fn freshness_score(config: &Config, node: &Node) -> Option<f64> {
    let reviewed = node.reviewed?;
    let stale_days = u64::from(config.detection.stale_days);
    if stale_days == 0 {
        // Decay disabled — anchor present, so the signal is still
        // active, just non-decaying.
        return Some(1.0);
    }
    let today = Local::now().date_naive();
    let elapsed = (today - reviewed).num_days().max(0) as f64;
    Some((1.0 - elapsed / stale_days as f64).clamp(0.0, 1.0))
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

fn backlinks_score(graph: &Graph, node: &Node, max_in: usize) -> Option<f64> {
    if max_in == 0 {
        // No external incoming edges anywhere — the backlinks signal
        // is absent from the graph. Returning `Some(1.0)` here would
        // fabricate maximum trust from absence of evidence; instead
        // drop the component so the composite renormalises over the
        // signals that are actually present.
        return None;
    }
    // Self-references are filtered out — trust measures external
    // attention, and a doc citing itself is not external. Without
    // the filter a doc could inflate its own score by writing
    // `[[self-id]]` in the body.
    let in_count = graph.external_incoming_edges(&node.id).len();
    let in_log = ((in_count + 1) as f64).ln();
    let max_log = ((max_in + 1) as f64).ln();
    Some((in_log / max_log).clamp(0.0, 1.0))
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
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
        }
    }

    fn graph_with(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, edges, vec![], vec![])
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
        assert!(fresh.components.freshness.unwrap() > 0.99);
        assert!((mid.components.freshness.unwrap() - 0.5).abs() < 0.05);
        assert_eq!(stale.components.freshness, Some(0.0));
    }

    #[test]
    fn missing_reviewed_date_drops_freshness_from_composite() {
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        assert!(r.components.freshness.is_none());
    }

    #[test]
    fn drift_excluded_when_threshold_unset() {
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        assert!(r.components.drift.is_none());
    }

    #[test]
    fn composite_renormalises_when_signals_missing() {
        // Active + missing reviewed (freshness absent) + no git_drift_threshold
        // (drift absent) + no external incoming edges anywhere
        // (backlinks absent) → composite renormalises over `status`
        // alone. Default weights: status 0.4.
        // Expected: (1.0 × 0.4) / 0.4 = 1.0
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        assert!(r.components.freshness.is_none());
        assert!(r.components.drift.is_none());
        assert!(r.components.backlinks.is_none());
        let expected = (1.0 * 0.4) / 0.4;
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
                make_node("a", "active", None), // freshness absent → dropped from composite
                make_node("b", "archived", None), // status 0
                make_node("c", "active", Some(Local::now().date_naive())), // freshness 1
            ],
            vec![],
        );
        let low = find_low_trust(&g, &Config::default(), Path::new("."), 1.0, None);
        let ids: Vec<&str> = low.iter().map(|r| r.node.id.as_str()).collect();
        // No external incoming edges anywhere in the graph → backlinks
        // absent on every node. Composites:
        // 'b' archived: status=0, all others absent → 0/0.4 = 0.0
        // 'a' active, no reviewed: status=1, all others absent → 1.0 (excluded by < 1.0)
        // 'c' active+today: status=1, freshness=1, drift/backlinks absent → 1.0 (excluded by < 1.0)
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn backlinks_absent_when_no_external_incoming() {
        // No external incoming edges anywhere in the graph → the
        // backlinks signal is absent and must report `None` rather
        // than fabricate a `1.0` from absence of evidence.
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        assert!(r.components.backlinks.is_none());
    }

    #[test]
    fn backlinks_score_excludes_self_loops() {
        // A doc that cites itself is not external attention — trust
        // measures attention from outside, so the self-edge must not
        // inflate the score. Without the filter a malicious or
        // accidental `[[my-own-id]]` could bump backlinks upward.
        //
        // We give the graph a second node `y` with a real external
        // incoming edge so `max_in > 0` and the backlinks signal is
        // present — otherwise both nodes would correctly report
        // `None` and the self-loop filter would not be exercised.
        let self_edge = Edge {
            source: "x".into(),
            target: ResolvedTarget::resolved("x"),
            relation: "references".into(),
            location: "L1".into(),
        };
        let external_edge = Edge {
            source: "x".into(),
            target: ResolvedTarget::resolved("y"),
            relation: "references".into(),
            location: "L2".into(),
        };
        let g = graph_with(
            vec![
                make_node("x", "active", None),
                make_node("y", "active", None),
            ],
            vec![self_edge, external_edge],
        );
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        // `y` has one external incoming edge so max_in = 1.
        // `x` has zero external incoming (the self-edge is filtered)
        // so backlinks_score(x) = ln(1)/ln(2) = 0.0.
        assert_eq!(r.components.backlinks, Some(0.0));
    }

    fn make_node_with_kind(
        id: &str,
        kind: &str,
        status: &str,
        reviewed: Option<chrono::NaiveDate>,
    ) -> Node {
        Node {
            kind: Kind::new(kind),
            ..make_node(id, status, reviewed)
        }
    }

    #[test]
    fn override_weights_applied_per_kind() {
        use crate::config::TrustWeightOverride;
        // ADR kind: backlinks weight 1.0, everything else 0.0.
        // Fixture must surface a real backlinks signal — the ADR
        // receives an external incoming edge from a sibling so
        // max_in > 0 and `backlinks` is `Some(1.0)` (the ADR is the
        // most-linked node).
        let mut cfg = Config::default();
        cfg.trust.overrides = vec![TrustWeightOverride {
            kinds: vec!["adr".into()],
            weights: TrustWeights {
                status: 0.0,
                freshness: 0.0,
                drift: 0.0,
                backlinks: 1.0,
            },
        }];
        let incoming = Edge {
            source: "b".into(),
            target: ResolvedTarget::resolved("a"),
            relation: "references".into(),
            location: "L1".into(),
        };
        let g = graph_with(
            vec![
                make_node_with_kind("a", "adr", "active", None),
                make_node_with_kind("b", "generic", "active", None),
            ],
            vec![incoming],
        );
        let r = compute_trust(&g, &cfg, Path::new("."), "a").unwrap();
        // backlinks_score(a) = ln(2)/ln(2) = 1.0; only weight active
        // is backlinks → composite = (1.0 × 1.0) / 1.0 = 1.0.
        assert_eq!(r.components.backlinks, Some(1.0));
        assert!(
            (r.score - 1.0).abs() < 1e-9,
            "expected 1.0 with backlinks-only weights, got {}",
            r.score
        );
    }

    #[test]
    fn json_omits_absent_components() {
        // Per-component absence is encoded as `Option::None` +
        // `#[serde(skip_serializing_if = "Option::is_none")]`. The
        // unit tests above assert the in-memory `Option` is `None`;
        // this one anchors the *wire* contract — consumers reading
        // the JSON payload (CLI envelope, codegen schema) see the
        // key missing entirely, not present as `null` or `0.0`.
        use serde_json::Value;
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &Config::default(), Path::new("."), "x").unwrap();
        let json: Value = serde_json::to_value(&r.components).unwrap();
        let obj = json
            .as_object()
            .expect("components must serialize as object");
        assert!(
            !obj.contains_key("freshness"),
            "freshness must be omitted when reviewed is absent; got {obj:?}"
        );
        assert!(
            !obj.contains_key("drift"),
            "drift must be omitted when git_drift_threshold is unset; got {obj:?}"
        );
        assert!(
            !obj.contains_key("backlinks"),
            "backlinks must be omitted when no external incoming edges exist; got {obj:?}"
        );
        assert!(
            obj.contains_key("status"),
            "status is unconditional and must always be present; got {obj:?}"
        );
    }

    #[test]
    fn drift_score_is_one_when_threshold_zero_with_reviewed() {
        // `git_drift_threshold = Some(0)` is the user-explicit "drift
        // is disabled" knob — distinct from `None` (signal absent).
        // With a reviewed anchor present, drift must report `Some(1.0)`
        // (no decay) rather than `None`, so the component
        // contributes to the composite renormalisation.
        let mut cfg = Config::default();
        cfg.detection.git_drift_threshold = Some(0);
        let g = graph_with(
            vec![make_node("x", "active", Some(Local::now().date_naive()))],
            vec![],
        );
        let r = compute_trust(&g, &cfg, Path::new("."), "x").unwrap();
        assert_eq!(r.components.drift, Some(1.0));
    }

    #[test]
    fn drift_absent_when_threshold_set_but_reviewed_missing() {
        // The drift score requires both a threshold *and* a reviewed
        // anchor — without the anchor there is nothing to count
        // commits against. The threshold alone must not fabricate a
        // signal; the component must report `None` so the composite
        // renormalises over the present components only.
        let mut cfg = Config::default();
        cfg.detection.git_drift_threshold = Some(5);
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &cfg, Path::new("."), "x").unwrap();
        assert!(
            r.components.drift.is_none(),
            "drift requires reviewed anchor even when threshold is set"
        );
    }

    #[test]
    fn global_weights_used_when_no_override() {
        use crate::config::TrustWeightOverride;
        // Override targets "adr" only. A "generic" node should use
        // global weights unchanged.
        let mut cfg = Config::default();
        cfg.trust.overrides = vec![TrustWeightOverride {
            kinds: vec!["adr".into()],
            weights: TrustWeights {
                status: 0.0,
                freshness: 0.0,
                drift: 0.0,
                backlinks: 1.0,
            },
        }];
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &cfg, Path::new("."), "x").unwrap();
        // Global default weights: status=0.4, freshness=0.3,
        // drift=0.2, backlinks=0.1. On this fixture the only active
        // signal is `status` (no reviewed, no drift threshold, no
        // external incoming edges anywhere).
        // Expected: (1.0 × 0.4) / 0.4 = 1.0
        assert_eq!(r.components.status, 1.0);
        assert!(r.components.freshness.is_none());
        assert!(r.components.drift.is_none());
        assert!(r.components.backlinks.is_none());
        let expected = 1.0;
        assert!(
            (r.score - expected).abs() < 1e-9,
            "expected {expected}, got {}",
            r.score
        );
    }
}
