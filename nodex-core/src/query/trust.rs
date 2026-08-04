//! Composite reliability score for a node.
//!
//! Agents call this to decide whether a piece of memory is still
//! authoritative. The score collapses four orthogonal signals into a
//! single number in `[0, 1]`, but the per-component breakdown is
//! always returned so the consumer can re-rank with its own weights
//! or surface the *why* alongside the *what*.

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::Path;

use crate::config::{Config, TrustWeights};
use crate::error::Result;
use crate::model::{Graph, Node, ResolvedTarget};

use super::{NodeRef, RankingOutcome};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TrustEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    /// Composite in `[0, 1]`. `None` — omitted on the wire, the same
    /// honest-absence convention as the components — exactly when no
    /// positively-weighted component is present (the weight sum over
    /// present components is zero): a composite exists only where a
    /// signal exists. Ranking listings always carry it (an unrankable
    /// node is excluded from the ranking's domain and counted in
    /// [`RankingOutcome::unscored`]); only the single-node form can
    /// omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub components: TrustComponents,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TrustComponents {
    pub status: f64,
    /// `None` — and omitted from the JSON — iff `reviewed` is unset on
    /// the node OR `detection.stale_days` is unset; the freshness
    /// weight is then excluded from the composite denominator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<f64>,
    /// `None` — and omitted from the JSON — iff
    /// `detection.git_drift_threshold` is unset OR `reviewed` is unset
    /// on the node OR no matched drift edge was measured (the node has
    /// none, or git cannot measure them — e.g. outside a work tree);
    /// the drift weight is then excluded from the composite denominator
    /// rather than fabricating "no drift" from absence.
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
pub fn compute_trust(
    graph: &Graph,
    config: &Config,
    root: &Path,
    id: &str,
    today: NaiveDate,
) -> Result<TrustEntry> {
    let node = graph.require_node(id)?;
    let max_in = max_incoming(graph);
    let repository = crate::rules::git_drift::drift_binding(config, root);
    Ok(score_node(
        graph,
        config,
        root,
        repository.as_ref(),
        node,
        max_in,
        today,
    ))
}

/// Which end of the trust distribution `compute_trust_ranking` walks.
///
/// `Bottom` ranks ascending (lowest trust first) — the operator's
/// "what needs attention" query. `Top` ranks descending — the "what
/// can I rely on right now" query. Both share the same filter +
/// ranking pipeline so a `--below` cutoff is interpreted identically:
/// keep only entries whose composite is strictly below the cutoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustExtreme {
    Top,
    Bottom,
}

/// Knobs for [`compute_trust_ranking`]. `limit` is the operator's capacity (top-K
/// is the timeless contract); `below` is an explicit opt-in cutoff and
/// defaults to "no cutoff — every node enters the ranking". `status`
/// restricts the corpus to one lifecycle status — the review-queue
/// read (`--status active`) where terminal nodes legitimately score
/// near zero and would otherwise drown the signal.
#[derive(Debug, Clone)]
pub struct TrustListOptions {
    pub extreme: TrustExtreme,
    pub limit: usize,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub below: Option<f64>,
}

/// Rank every node's trust composite, filter by kind / status /
/// `below` if supplied, sort by score (asc for `Bottom`, desc for `Top`) with id
/// tie-break, truncate to `limit`. Top-K is the operator-capacity
/// contract; the cutoff is opt-in.
///
/// The ranking is a total order over composite scores, so a node with
/// no composite (no positively-weighted signal present) is excluded
/// before the cutoff, the sort, and the truncation — it can never
/// occupy a slot, satisfy `below`, or sort as an extreme — and is
/// counted in [`RankingOutcome::unscored`].
pub fn compute_trust_ranking(
    graph: &Graph,
    config: &Config,
    root: &Path,
    opts: &TrustListOptions,
    today: NaiveDate,
) -> RankingOutcome<TrustEntry> {
    let max_in = max_incoming(graph);
    // Resolved once for the whole ranking: every node's drift component
    // measures the same repository, and a corpus-wide read costs one
    // probe rather than one per node.
    let repository = crate::rules::git_drift::drift_binding(config, root);
    let kind = opts.kind.as_deref();
    let status = opts.status.as_deref();
    let mut unscored = 0usize;
    let mut scored: Vec<(f64, TrustEntry)> = Vec::new();
    for node in graph
        .nodes()
        .values()
        .filter(|n| kind.is_none_or(|k| n.kind.as_str() == k))
        .filter(|n| status.is_none_or(|s| n.status.as_str() == s))
    {
        let entry = score_node(
            graph,
            config,
            root,
            repository.as_ref(),
            node,
            max_in,
            today,
        );
        match entry.score {
            Some(score) => {
                if opts.below.is_none_or(|cutoff| score < cutoff) {
                    scored.push((score, entry));
                }
            }
            None => unscored += 1,
        }
    }
    scored.sort_by(|a, b| {
        let primary = match opts.extreme {
            TrustExtreme::Bottom => a.0.partial_cmp(&b.0),
            TrustExtreme::Top => b.0.partial_cmp(&a.0),
        };
        primary
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.node.id.cmp(&b.1.node.id))
    });
    scored.truncate(opts.limit);
    RankingOutcome {
        entries: scored.into_iter().map(|(_, entry)| entry).collect(),
        unscored,
    }
}

fn score_node(
    graph: &Graph,
    config: &Config,
    root: &Path,
    repository: Option<&crate::git::Repository>,
    node: &Node,
    max_in: usize,
    today: NaiveDate,
) -> TrustEntry {
    let components = TrustComponents {
        status: status_score(config, node.status.as_str()),
        freshness: freshness_score(config, node, today),
        drift: drift_score(graph, config, root, repository, node),
        backlinks: backlinks_score(graph, node, max_in),
    };
    let weights = config.trust_weights_for(node.kind.as_str());
    let score = compose(&weights, &components);
    TrustEntry {
        node: NodeRef::from_node(node),
        score,
        components,
    }
}

/// Weighted average over the *present* components. `None` exactly when
/// the weight sum over present components is zero (weights are
/// load-validated finite and non-negative, so zero sum means no
/// positively-weighted signal is present) — a composite exists only
/// where a signal exists, never fabricated from absence.
fn compose(w: &TrustWeights, c: &TrustComponents) -> Option<f64> {
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
        None
    } else {
        Some((weighted / weight_sum).clamp(0.0, 1.0))
    }
}

fn status_score(config: &Config, status: &str) -> f64 {
    if config.is_terminal(status) { 0.0 } else { 1.0 }
}

fn freshness_score(config: &Config, node: &Node, today: NaiveDate) -> Option<f64> {
    let reviewed = node.reviewed?;
    let Some(stale_days) = config.detection.stale_days else {
        // Stale detection disabled
        return None;
    };
    let elapsed = (today - reviewed).num_days().max(0) as f64;
    Some((1.0 - elapsed / stale_days as f64).clamp(0.0, 1.0))
}

fn drift_score(
    graph: &Graph,
    config: &Config,
    root: &Path,
    repository: Option<&crate::git::Repository>,
    node: &Node,
) -> Option<f64> {
    let threshold = config.detection.git_drift_threshold?;
    // No repository means the signal is unmeasurable here. Dropping the
    // component lets the composite renormalise over the signals that are
    // present, where a `0.0` would report maximum drift from absence of
    // evidence.
    let repository = repository?;
    let reviewed = node.reviewed?;
    if threshold == 0 {
        // Unreachable under a loaded config — `Config::validate` rejects
        // `git_drift_threshold = 0` — so the backstop for unvalidated
        // library callers reports honest absence, never fabricated credit.
        return None;
    }

    let relations = &config.detection.git_drift_relations;
    let mut total: u32 = 0;
    let mut measured: usize = 0;
    for edge in graph.outgoing_edges(&node.id) {
        if !relations.iter().any(|r| r == &edge.relation) {
            continue;
        }
        let path = match &edge.target {
            ResolvedTarget::Resolved { id } => match graph.node(id) {
                Some(t) => t.path.clone(),
                None => continue,
            },
            // A refused cause (absolute, source-escaping) carries no
            // in-root candidates and is skipped outright; the rest probe
            // the same normalized candidate ladder the resolver uses —
            // never the raw authored string, so the probe can never stat
            // outside the project root.
            ResolvedTarget::Unresolved { raw, cause } => {
                if !cause.has_path_candidates() {
                    continue;
                }
                let candidates = crate::builder::resolver::normalized_resolution_candidates(
                    raw,
                    Some(node.path.as_path()),
                    &config.parser.extensions,
                    crate::model::edge::is_document_ref_relation(&edge.relation),
                );
                match crate::builder::resolver::first_candidate_on_disk(
                    &candidates,
                    crate::builder::scanner::ProjectFiles::working_tree(root),
                    crate::model::edge::is_path_only_relation(&edge.relation),
                ) {
                    Some(candidate) => candidate,
                    None => continue,
                }
            }
        };
        // `None` means git could not measure this edge. Drop the whole
        // drift component rather than fabricate "no drift", mirroring
        // `backlinks_score`'s treatment of an absent signal.
        total = total.saturating_add(crate::rules::git_drift::commits_since(
            repository, &path, reviewed,
        )?);
        measured += 1;
    }
    if measured == 0 {
        // No matched drift edge exists to measure — the drift signal is
        // absent, and a score here would fabricate maximum drift credit
        // from absence of evidence. Drop the component so the composite
        // renormalises over the signals that are present.
        return None;
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
    let in_count = distinct_linkers(graph, &node.id);
    let in_log = ((in_count + 1) as f64).ln();
    let max_log = ((max_in + 1) as f64).ln();
    Some((in_log / max_log).clamp(0.0, 1.0))
}

/// Number of *distinct documents* linking to `id`, self-loops excluded.
/// Counting distinct sources (not edges) is what "external attention"
/// means — a single document citing a target through two relations
/// (`related` + `implements`) is one attendee, not two.
fn distinct_linkers(graph: &Graph, id: &str) -> usize {
    graph
        .external_incoming_edges(id)
        .iter()
        .map(|edge| edge.source.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

fn max_incoming(graph: &Graph) -> usize {
    graph
        .nodes()
        .values()
        .map(|n| distinct_linkers(graph, &n.id))
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
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn graph_with(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(
            map,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    #[test]
    fn distinct_linkers_counts_documents_not_edges() {
        // `a` links to `target` via two relations; `b` via one. External
        // attention is two distinct documents, not three edges.
        let edge = |src: &str, rel: &str| Edge {
            source: src.to_string(),
            target: ResolvedTarget::resolved("target"),
            relation: rel.to_string(),
            location: "L1".to_string(),
        };
        let g = graph_with(
            vec![
                make_node("target", "active", None),
                make_node("a", "active", None),
                make_node("b", "active", None),
            ],
            vec![
                edge("a", "related"),
                edge("a", "implements"),
                edge("b", "related"),
            ],
        );
        assert_eq!(distinct_linkers(&g, "target"), 2);
    }

    #[test]
    fn terminal_status_drops_status_to_zero() {
        let g = graph_with(vec![make_node("x", "archived", None)], vec![]);
        let r = compute_trust(
            &g,
            &Config::default(),
            Path::new("."),
            "x",
            crate::test_today(),
        )
        .unwrap();
        assert_eq!(r.components.status, 0.0);
    }

    #[test]
    fn freshness_decays_linearly_until_zero() {
        let today = crate::test_today();
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
        let mut cfg = Config::default();
        // Freshness is measured against the staleness horizon, so a project
        // that declares none has no scale to place a review date on.
        cfg.detection.stale_days = Some(180);
        let fresh = compute_trust(&g, &cfg, Path::new("."), "fresh", today).unwrap();
        let mid = compute_trust(&g, &cfg, Path::new("."), "mid", today).unwrap();
        let stale = compute_trust(&g, &cfg, Path::new("."), "stale", today).unwrap();
        assert!(fresh.components.freshness.unwrap() > 0.99);
        assert!((mid.components.freshness.unwrap() - 0.5).abs() < 0.05);
        assert_eq!(stale.components.freshness, Some(0.0));
    }

    #[test]
    fn missing_reviewed_date_drops_freshness_from_composite() {
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(
            &g,
            &Config::default(),
            Path::new("."),
            "x",
            crate::test_today(),
        )
        .unwrap();
        assert!(r.components.freshness.is_none());
    }

    #[test]
    fn drift_excluded_when_threshold_unset() {
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(
            &g,
            &Config::default(),
            Path::new("."),
            "x",
            crate::test_today(),
        )
        .unwrap();
        assert!(r.components.drift.is_none());
    }

    #[test]
    fn drift_excluded_when_no_matched_edge_exists() {
        // Threshold set + reviewed set, but the node has zero edges in
        // any `git_drift_relations` relation: there is no drift signal
        // to measure, so the component is absent — never a fabricated
        // perfect score — and the composite renormalises over the
        // signals that exist (status + freshness).
        let today = crate::test_today();
        let mut config = Config::default();
        config.detection.stale_days = Some(180);
        config.detection.git_drift_threshold = Some(10);
        let g = graph_with(vec![make_node("x", "active", Some(today))], vec![]);
        let r = compute_trust(&g, &config, Path::new("."), "x", today).unwrap();
        assert!(
            r.components.drift.is_none(),
            "zero matched drift edges must drop the component: {:?}",
            r.components
        );
        let weights = config.trust_weights_for("generic");
        let expected = (1.0 * weights.status + r.components.freshness.unwrap() * weights.freshness)
            / (weights.status + weights.freshness);
        let score = r.score.expect("status + freshness keep the composite");
        assert!(
            (score - expected).abs() < 1e-9,
            "composite must renormalise without drift: expected {expected}, got {score}"
        );
    }

    #[test]
    fn composite_renormalises_when_signals_missing() {
        // Active + missing reviewed (freshness absent) + no git_drift_threshold
        // (drift absent) + no external incoming edges anywhere
        // (backlinks absent) → composite renormalises over `status`
        // alone. Default weights: status 0.4.
        // Expected: (1.0 × 0.4) / 0.4 = 1.0
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(
            &g,
            &Config::default(),
            Path::new("."),
            "x",
            crate::test_today(),
        )
        .unwrap();
        assert!(r.components.freshness.is_none());
        assert!(r.components.drift.is_none());
        assert!(r.components.backlinks.is_none());
        let expected = (1.0 * 0.4) / 0.4;
        let score = r
            .score
            .expect("status weight keeps the denominator positive");
        assert!(
            (score - expected).abs() < 1e-9,
            "expected {expected}, got {score}"
        );
    }

    #[test]
    fn missing_node_errors_via_require_node() {
        let g = graph_with(vec![], vec![]);
        let err = compute_trust(
            &g,
            &Config::default(),
            Path::new("."),
            "ghost",
            crate::test_today(),
        )
        .unwrap_err();
        assert!(matches!(err, crate::error::Error::MissingNode(_)));
    }

    #[test]
    fn compute_trust_ranking_bottom_with_below_filters_and_sorts() {
        let g = graph_with(
            vec![
                make_node("a", "active", None), // freshness absent → dropped from composite
                make_node("b", "archived", None), // status 0
                make_node("c", "active", Some(crate::test_today())), // freshness 1
            ],
            vec![],
        );
        let low = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: None,
                status: None,
                below: Some(1.0),
            },
            crate::test_today(),
        );
        let ids: Vec<&str> = low.entries.iter().map(|r| r.node.id.as_str()).collect();
        // No external incoming edges anywhere in the graph → backlinks
        // absent on every node. Composites:
        // 'b' archived: status=0, all others absent → 0/0.4 = 0.0
        // 'a' active, no reviewed: status=1, all others absent → 1.0 (excluded by < 1.0)
        // 'c' active+today: status=1, freshness=1, drift/backlinks absent → 1.0 (excluded by < 1.0)
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn compute_trust_ranking_top_orders_descending() {
        let today = crate::test_today();
        let g = graph_with(
            vec![
                make_node("dead", "archived", None),       // composite 0.0
                make_node("fresh", "active", Some(today)), // composite 1.0
            ],
            vec![],
        );
        let top = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Top,
                limit: 100,
                kind: None,
                status: None,
                below: None,
            },
            today,
        );
        let ids: Vec<&str> = top.entries.iter().map(|r| r.node.id.as_str()).collect();
        assert_eq!(ids, vec!["fresh", "dead"]);
    }

    #[test]
    fn compute_trust_ranking_limit_truncates_after_ranking() {
        let today = crate::test_today();
        let g = graph_with(
            vec![
                make_node("dead-1", "archived", None),
                make_node("dead-2", "archived", None),
                make_node("fresh", "active", Some(today)),
            ],
            vec![],
        );
        let bottom = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 1,
                kind: None,
                status: None,
                below: None,
            },
            today,
        );
        // Two archived docs tie at 0.0; id tie-break orders dead-1
        // before dead-2 and the limit lops the rest.
        assert_eq!(bottom.entries.len(), 1);
        assert_eq!(bottom.entries[0].node.id, "dead-1");
    }

    #[test]
    fn compute_trust_ranking_with_limit_zero_returns_empty() {
        // Library-level contract: `limit=0` is a legitimate "no
        // results" request and must return an empty Vec without
        // panicking. (The CLI rejects zero up-front to surface the
        // operator footgun; the library accepts every non-negative
        // limit because callers may compose listings in tight loops
        // where zero means "skip this round".)
        let today = crate::test_today();
        let g = graph_with(
            vec![
                make_node("a", "archived", None),
                make_node("b", "active", Some(today)),
            ],
            vec![],
        );
        let out = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 0,
                kind: None,
                status: None,
                below: None,
            },
            today,
        );
        assert!(
            out.entries.is_empty(),
            "limit=0 must return empty; got {} entries",
            out.entries.len()
        );
    }

    #[test]
    fn compute_trust_ranking_returns_all_when_limit_exceeds_node_count() {
        // A `limit` larger than the corpus must return every node, not
        // pad nor truncate. Anchors the "limit is an upper bound, not
        // a target" semantic against future refactors.
        let g = graph_with(
            vec![
                make_node("a", "archived", None),
                make_node("b", "active", None),
            ],
            vec![],
        );
        let out = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: None,
                status: None,
                below: None,
            },
            crate::test_today(),
        );
        assert_eq!(out.entries.len(), 2, "limit > N must return every node");
    }

    #[test]
    fn compute_trust_ranking_with_empty_graph_returns_empty() {
        // No nodes → empty Vec, regardless of extreme / limit / kind.
        let g = graph_with(vec![], vec![]);
        let out = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Top,
                limit: 10,
                kind: None,
                status: None,
                below: None,
            },
            crate::test_today(),
        );
        assert!(
            out.entries.is_empty(),
            "empty graph must yield empty listing"
        );
    }

    #[test]
    fn compute_trust_ranking_kind_filter_restricts_corpus() {
        let g = graph_with(
            vec![
                make_node_with_kind("a", "adr", "archived", None),
                make_node_with_kind("b", "generic", "archived", None),
            ],
            vec![],
        );
        let only_adr = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: Some("adr".into()),
                status: None,
                below: None,
            },
            crate::test_today(),
        );
        let ids: Vec<&str> = only_adr
            .entries
            .iter()
            .map(|r| r.node.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a"]);
    }

    #[test]
    fn compute_trust_ranking_status_filter_restricts_corpus() {
        // The review-queue read: terminal nodes legitimately score near
        // zero (status component 0.0) and would dominate a bottom-K —
        // `status: active` keeps the listing to nodes a review can act on.
        let g = graph_with(
            vec![
                make_node("live", "active", None),
                make_node("done", "archived", None),
            ],
            vec![],
        );
        let only_active = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: None,
                status: Some("active".into()),
                below: None,
            },
            crate::test_today(),
        );
        let ids: Vec<&str> = only_active
            .entries
            .iter()
            .map(|r| r.node.id.as_str())
            .collect();
        assert_eq!(ids, vec!["live"]);
    }

    #[test]
    fn backlinks_absent_when_no_external_incoming() {
        // No external incoming edges anywhere in the graph → the
        // backlinks signal is absent and must report `None` rather
        // than fabricate a `1.0` from absence of evidence.
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(
            &g,
            &Config::default(),
            Path::new("."),
            "x",
            crate::test_today(),
        )
        .unwrap();
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
        let r = compute_trust(
            &g,
            &Config::default(),
            Path::new("."),
            "x",
            crate::test_today(),
        )
        .unwrap();
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
        let r = compute_trust(&g, &cfg, Path::new("."), "a", crate::test_today()).unwrap();
        // backlinks_score(a) = ln(2)/ln(2) = 1.0; only weight active
        // is backlinks → composite = (1.0 × 1.0) / 1.0 = 1.0.
        assert_eq!(r.components.backlinks, Some(1.0));
        let score = r
            .score
            .expect("backlinks signal present under the override");
        assert!(
            (score - 1.0).abs() < 1e-9,
            "expected 1.0 with backlinks-only weights, got {score}"
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
        let r = compute_trust(
            &g,
            &Config::default(),
            Path::new("."),
            "x",
            crate::test_today(),
        )
        .unwrap();
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

    /// Backlinks-only weights on a graph with zero external incoming
    /// edges anywhere: the single positively-weighted component is
    /// absent, so the weight sum over present components is zero and
    /// no composite exists. The score is `None` in memory and the
    /// `score` key is absent on the wire — the same honest-absence
    /// convention `json_omits_absent_components` anchors for the
    /// components, extended to the composite.
    #[test]
    fn score_absent_when_no_positively_weighted_signal() {
        use crate::config::TrustWeightOverride;
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
        let g = graph_with(
            vec![make_node_with_kind("a", "adr", "active", None)],
            vec![],
        );
        let r = compute_trust(&g, &cfg, Path::new("."), "a", crate::test_today()).unwrap();
        assert!(
            r.components.backlinks.is_none(),
            "no external incoming edges anywhere → backlinks absent"
        );
        assert!(
            r.score.is_none(),
            "no positively-weighted present component → no composite; got {:?}",
            r.score
        );

        let json = serde_json::to_value(&r).unwrap();
        let obj = json.as_object().expect("entry must serialize as object");
        assert!(
            !obj.contains_key("score"),
            "score must be omitted when absent, never null or 0.0; got {obj:?}"
        );
        assert!(
            obj.contains_key("components"),
            "components stay present so the absence is inspectable; got {obj:?}"
        );
    }

    /// An unrankable node is not in the ranking's domain: it is
    /// excluded before the cutoff / sort / truncation (so it can never
    /// occupy a bottom-N slot or satisfy `--below`) and counted in
    /// `unscored`. Scored siblings rank exactly as they would without
    /// the unrankable node present.
    #[test]
    fn ranking_excludes_unscored_nodes_and_counts_them() {
        use crate::config::TrustWeightOverride;
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
        // `no-signal` (adr): backlinks-only weights, no external
        // incoming edges anywhere → unscored. The generic nodes use
        // the default weights (status 0.4 always present) → scored.
        let g = graph_with(
            vec![
                make_node_with_kind("no-signal", "adr", "active", None),
                make_node("dead", "archived", None), // composite 0.0
                make_node("live", "active", None),   // composite 1.0
            ],
            vec![],
        );
        for extreme in [TrustExtreme::Bottom, TrustExtreme::Top] {
            let out = compute_trust_ranking(
                &g,
                &cfg,
                Path::new("."),
                &TrustListOptions {
                    extreme,
                    limit: 100,
                    kind: None,
                    status: None,
                    below: None,
                },
                crate::test_today(),
            );
            assert_eq!(out.unscored, 1, "the no-signal node is counted");
            let ids: Vec<&str> = out.entries.iter().map(|r| r.node.id.as_str()).collect();
            assert!(
                !ids.contains(&"no-signal"),
                "unrankable node must not occupy a slot ({extreme:?}); got {ids:?}"
            );
            let expected = match extreme {
                TrustExtreme::Bottom => vec!["dead", "live"],
                TrustExtreme::Top => vec!["live", "dead"],
            };
            assert_eq!(ids, expected, "scored ordering is undisturbed");
        }
        // `--below` is unsatisfiable by an absent composite by
        // construction: exclusion precedes the filter.
        let below = compute_trust_ranking(
            &g,
            &cfg,
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: None,
                status: None,
                below: Some(1.0),
            },
            crate::test_today(),
        );
        let ids: Vec<&str> = below.entries.iter().map(|r| r.node.id.as_str()).collect();
        assert_eq!(ids, vec!["dead"], "only the scored 0.0 passes < 1.0");
        assert_eq!(below.unscored, 1);
    }

    #[test]
    fn drift_score_threshold_zero_reports_absence() {
        // `git_drift_threshold = Some(0)` never survives `Config::load`
        // (`validate_detection` rejects it as ambiguous), so this value
        // only reaches the scorer through an unvalidated library
        // config. The backstop reports honest absence — a zero
        // threshold cannot fabricate maximum drift credit.
        let mut cfg = Config::default();
        cfg.detection.git_drift_threshold = Some(0);
        let g = graph_with(
            vec![make_node("x", "active", Some(crate::test_today()))],
            vec![],
        );
        let r = compute_trust(&g, &cfg, Path::new("."), "x", crate::test_today()).unwrap();
        assert!(
            r.components.drift.is_none(),
            "an unvalidated zero threshold must not score: {:?}",
            r.components.drift
        );
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
        let r = compute_trust(&g, &cfg, Path::new("."), "x", crate::test_today()).unwrap();
        assert!(
            r.components.drift.is_none(),
            "drift requires reviewed anchor even when threshold is set"
        );
    }

    #[test]
    fn drift_absent_when_git_cannot_measure() {
        // A reviewed node with an `implements` edge (a drift relation) to
        // another doc, threshold set, but `root` is not a git work tree:
        // git can't measure drift, so the component must report `None` —
        // never fabricate `1.0` (no drift) from absence of evidence, the
        // same discipline `backlinks_score` follows.
        let mut cfg = Config::default();
        cfg.detection.git_drift_threshold = Some(5);
        let reviewed = (crate::test_today()) - Duration::days(10);
        let mut x = make_node("x", "active", Some(reviewed));
        x.implements = vec!["target".into()];
        let g = graph_with(
            vec![x, make_node("target", "active", None)],
            vec![Edge {
                source: "x".to_string(),
                target: ResolvedTarget::resolved("target"),
                relation: "implements".to_string(),
                location: "frontmatter:implements".to_string(),
            }],
        );
        let r = compute_trust(
            &g,
            &cfg,
            Path::new("/nonexistent-not-a-repo"),
            "x",
            crate::test_today(),
        )
        .unwrap();
        assert!(
            r.components.drift.is_none(),
            "git-unmeasurable drift drops the component, never fabricates 1.0: {:?}",
            r.components.drift
        );
    }

    #[test]
    fn drift_skips_absolute_raw_target_without_probing_disk() {
        // An absolute authored target is refused at the resolver
        // (`UnresolvedCause::Absolute`) and carries no in-root
        // resolution candidates, so the drift probe skips the edge
        // outright — joining the raw string onto root would resolve to
        // the absolute path itself, measuring a file the build never
        // bound (and, for an out-of-root path, statting outside the
        // project). With the only drift edge skipped, no signal is
        // measured and the component reports absence.
        let dir = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            let out = crate::git::command(dir.path())
                .expect("git on PATH")
                .args(args)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .expect("git ran");
            assert!(out.status.success(), "git {args:?} failed");
        };
        run(&["init"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/auth.rs"), "fn main() {}\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "covered code"]);

        let mut cfg = Config::default();
        cfg.detection.git_drift_threshold = Some(5);
        let reviewed = crate::test_today() - Duration::days(10);
        let absolute_target = dir.path().join("src/auth.rs");
        let g = graph_with(
            vec![make_node("x", "active", Some(reviewed))],
            vec![Edge {
                source: "x".to_string(),
                target: ResolvedTarget::unresolved(
                    absolute_target.to_string_lossy(),
                    crate::model::UnresolvedCause::Absolute,
                ),
                relation: "covers".to_string(),
                location: "frontmatter:covers".to_string(),
            }],
        );
        let r = compute_trust(&g, &cfg, dir.path(), "x", crate::test_today()).unwrap();
        assert!(
            r.components.drift.is_none(),
            "an absolute raw target is skipped, never measured: {:?}",
            r.components.drift
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
        let r = compute_trust(&g, &cfg, Path::new("."), "x", crate::test_today()).unwrap();
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
        let score = r
            .score
            .expect("global status weight keeps the denominator positive");
        assert!(
            (score - expected).abs() < 1e-9,
            "expected {expected}, got {score}"
        );
    }

    #[test]
    fn compute_trust_ranking_single_node_graph_returns_one_entry() {
        // Smallest non-empty graph. Anchors the "every node enters the
        // listing" contract — there's no implicit floor on graph size
        // and no special-casing for the one-node case.
        let g = graph_with(vec![make_node("solo", "active", None)], vec![]);
        let out = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 10,
                kind: None,
                status: None,
                below: None,
            },
            crate::test_today(),
        );
        assert_eq!(
            out.entries.len(),
            1,
            "single-node graph must yield one entry"
        );
        assert_eq!(out.entries[0].node.id, "solo");
    }

    #[test]
    fn compute_trust_ranking_below_zero_returns_empty() {
        // `--below 0.0` is the strict-cutoff degenerate case: composite
        // scores live in `[0, 1]`, so no score can be `< 0.0`. The
        // listing must return empty rather than panic or surface the
        // zero-score entries (which `< 0.0` excludes by definition).
        let today = crate::test_today();
        let g = graph_with(
            vec![
                make_node("dead", "archived", None),
                make_node("fresh", "active", Some(today)),
            ],
            vec![],
        );
        let out = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: None,
                status: None,
                below: Some(0.0),
            },
            today,
        );
        assert!(
            out.entries.is_empty(),
            "--below 0.0 must return empty; got {} entries",
            out.entries.len()
        );
    }

    #[test]
    fn compute_trust_ranking_unknown_kind_returns_empty() {
        // The CLI rejects `--kind` values outside `kinds.allowed` up
        // front, but the library accepts every string so composed
        // callers can probe. An unknown kind must filter the corpus
        // empty rather than panic or surface every node.
        let g = graph_with(
            vec![make_node_with_kind("a", "adr", "archived", None)],
            vec![],
        );
        let out = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: Some("ghost-kind".into()),
                status: None,
                below: None,
            },
            crate::test_today(),
        );
        assert!(
            out.entries.is_empty(),
            "unknown kind must filter the corpus empty; got {} entries",
            out.entries.len()
        );
    }

    #[test]
    fn compute_trust_ranking_kind_and_below_combined_apply_both_filters() {
        // Confirms the two listing-only filters compose: kind narrows
        // the corpus first, then `below` strips by score. A node that
        // satisfies one but not the other must be excluded.
        let today = crate::test_today();
        let g = graph_with(
            vec![
                make_node_with_kind("adr-dead", "adr", "archived", None), // composite 0.0
                make_node_with_kind("adr-fresh", "adr", "active", Some(today)), // composite 1.0
                make_node_with_kind("gen-dead", "generic", "archived", None), // composite 0.0 (excluded by kind)
            ],
            vec![],
        );
        let out = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: Some("adr".into()),
                status: None,
                below: Some(0.5),
            },
            today,
        );
        let ids: Vec<&str> = out.entries.iter().map(|r| r.node.id.as_str()).collect();
        // gen-dead is excluded by kind; adr-fresh is excluded by below
        // (1.0 is not < 0.5); only adr-dead survives.
        assert_eq!(ids, vec!["adr-dead"]);
    }

    #[test]
    fn compute_trust_ranking_all_terminal_orders_by_id_at_score_zero() {
        // All-archived corpus: every composite is 0.0. The id
        // tie-break must produce ascending-id order regardless of the
        // extreme — same primary score, deterministic secondary key.
        let g = graph_with(
            vec![
                make_node("c", "archived", None),
                make_node("a", "archived", None),
                make_node("b", "archived", None),
            ],
            vec![],
        );
        let out = compute_trust_ranking(
            &g,
            &Config::default(),
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: None,
                status: None,
                below: None,
            },
            crate::test_today(),
        );
        let ids: Vec<&str> = out.entries.iter().map(|r| r.node.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["a", "b", "c"],
            "id tie-break must produce ascending order on equal scores; got {ids:?}",
        );
    }
}
