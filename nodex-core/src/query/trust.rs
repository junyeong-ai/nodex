//! Composite reliability score for a node.
//!
//! Agents call this to decide whether a piece of memory is still
//! authoritative. The score collapses four orthogonal signals into a
//! single number in `[0, 1]`, but the per-component breakdown is
//! always returned so the consumer can re-rank with its own weights
//! or surface the *why* alongside the *what*.
//!
//! A component goes missing for two reasons that read alike and score
//! nothing alike. Either nothing this document declares would produce
//! it in this run — no staleness horizon is configured, no repository
//! can be measured, the node covers no source, no document anywhere is
//! linked — or the run *can* measure it and this document declares no
//! input. The first is a property of the run and holds for every node
//! alike, so the composite renormalises over it: a mean over the
//! components that apply is the best-defined summary there is. The
//! second is a property of the document, and renormalising over it is
//! not an exclusion at all. Dropping a component and rescaling the
//! rest imputes, for the missing one, exactly the score the present
//! ones produced — a high value for any document that looks healthy on
//! what is left, granted *because* the datum is absent. A ranking
//! built that way pays a document to withhold evidence, and pays most
//! the ones with the least to show.
//!
//! So a composite exists only over a complete basis. A
//! positively-weighted component the document could have declared and
//! did not leaves the node with no score, named in
//! [`TrustEntry::undeclared`] and outside every ranking's domain. A
//! project that does not track review dates declares that in
//! `[trust.weights]`, per kind if it varies: a zero weight carries no
//! evidence either way, so it neither suppresses a composite nor asks
//! for a declaration.

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::config::{Config, TrustWeights};
use crate::error::Result;
use crate::model::{Graph, Node};

use super::{NodeRef, RankingOutcome};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TrustEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    /// Composite in `[0, 1]`. `None` — omitted on the wire, the same
    /// honest-absence convention as the components — exactly when the
    /// basis is incomplete: a positively-weighted component is
    /// `undeclared`, or no positively-weighted component is present at
    /// all. Ranking listings always carry it (a node with no composite
    /// is excluded from the ranking's domain and counted in
    /// [`RankingOutcome::unscored`]); only the single-node form can
    /// omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub components: TrustComponents,
    /// The positively-weighted components this run can measure and
    /// this document declares no input for — non-empty exactly when
    /// that is why `score` is absent. The entry names them rather than
    /// only reporting the absence, because a document is repaired by a
    /// specific declaration and a bare "unscored" names none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undeclared: Vec<TrustComponent>,
}

/// The four component names — the vocabulary [`TrustComponents`],
/// [`TrustWeights`] and [`TrustEntry::undeclared`] share, kept beside
/// the struct so the three cannot drift. Only `Freshness` and `Drift`
/// ever reach `undeclared`: `Status` and `Backlinks` are derived from
/// the graph, and nothing a document declares produces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustComponent {
    Status,
    Freshness,
    Drift,
    Backlinks,
}

/// Per-component breakdown. A component is present exactly when this
/// run measured it; an absent one is omitted from the JSON rather than
/// reported as `null` or `0.0`, and [`TrustEntry::undeclared`] tells
/// the two kinds of absence apart.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TrustComponents {
    /// Always measured — every node carries a status.
    pub status: f64,
    /// Absent when `detection.stale_days` is unset — freshness places
    /// a review date on the staleness horizon, and a project declaring
    /// no horizon has no scale to place one on — when the node is
    /// terminal, or when it declares no `reviewed` date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<f64>,
    /// Absent when `detection.git_drift_threshold` is unset, no
    /// repository can be measured, the node is terminal, it has no
    /// resolvable edge in a `detection.git_drift_relations` relation,
    /// git cannot measure the edges it has, or it declares no
    /// `reviewed` anchor to count commits from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<f64>,
    /// Absent when the graph carries no external incoming edges on any
    /// node — there is no signal to compare against, and a `1.0` here
    /// would be fabricated from absence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlinks: Option<f64>,
}

/// What this run could learn about one component of one node.
#[derive(Debug, Clone, Copy)]
enum Signal {
    Measured(f64),
    /// Nothing this document declares would produce the component in
    /// this run: the scale is unconfigured, the environment cannot
    /// measure it, or the node offers no subject to measure.
    Inapplicable,
    /// The run can measure the component and the document declares no
    /// input for it.
    Undeclared,
}

impl Signal {
    /// The measured value, for the wire breakdown — where both kinds
    /// of absence are the same omitted key, and
    /// [`TrustEntry::undeclared`] is what tells them apart.
    fn measured(self) -> Option<f64> {
        match self {
            Signal::Measured(value) => Some(value),
            Signal::Inapplicable | Signal::Undeclared => None,
        }
    }
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
/// The ranking is a total order over composite scores, and two
/// composites are only comparable over the same basis, so a node with
/// no composite is excluded before the cutoff, the sort, and the
/// truncation — it can never occupy a slot, satisfy `below`, or sort
/// as an extreme — and is counted in [`RankingOutcome::unscored`].
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
    let status = status_score(config, node.status.as_str());
    let weights = config.trust_weights_for(node.kind.as_str());
    let freshness = freshness_signal(config, node, today);
    let drift = drift_signal(graph, config, root, repository, node);
    let backlinks = backlinks_signal(graph, node, max_in);
    let components = TrustComponents {
        status,
        freshness: freshness.measured(),
        drift: drift.measured(),
        backlinks: backlinks.measured(),
    };
    let (score, undeclared) = compose(
        status,
        &weights,
        [
            (TrustComponent::Freshness, weights.freshness, freshness),
            (TrustComponent::Drift, weights.drift, drift),
            (TrustComponent::Backlinks, weights.backlinks, backlinks),
        ],
    );
    TrustEntry {
        node: NodeRef::from_node(node),
        score,
        components,
        undeclared,
    }
}

/// Weighted average over the measured components, and the
/// positively-weighted ones the document left undeclared. Both leave
/// the same fold: a composite that could disagree with the reason it
/// is absent would be worse than carrying no reason at all.
///
/// `None` when a positively-weighted component is undeclared —
/// renormalising there imputes, for the missing component, exactly the
/// score the present ones produced — and when the weight sum over
/// measured components is zero, where no positively-weighted signal is
/// present at all (weights are load-validated finite and non-negative,
/// so a zero sum means nothing else).
fn compose(
    status: f64,
    w: &TrustWeights,
    basis: [(TrustComponent, f64, Signal); 3],
) -> (Option<f64>, Vec<TrustComponent>) {
    let mut weighted = status * w.status;
    let mut weight_sum = w.status;
    let mut undeclared = Vec::new();
    for (component, weight, signal) in basis {
        match signal {
            Signal::Measured(value) => {
                weighted += value * weight;
                weight_sum += weight;
            }
            // A zero-weighted component carries no evidence either way,
            // so its absence neither suppresses the composite nor asks
            // the document for a declaration.
            Signal::Undeclared if weight > 0.0 => undeclared.push(component),
            Signal::Undeclared | Signal::Inapplicable => {}
        }
    }
    let score = (undeclared.is_empty() && weight_sum > 0.0)
        .then(|| (weighted / weight_sum).clamp(0.0, 1.0));
    (score, undeclared)
}

fn status_score(config: &Config, status: &str) -> f64 {
    if config.is_terminal(status) { 0.0 } else { 1.0 }
}

fn freshness_signal(config: &Config, node: &Node, today: NaiveDate) -> Signal {
    // Freshness places a review date on the staleness horizon, so a
    // project declaring no horizon has no scale to place one on.
    let Some(stale_days) = config.detection.stale_days else {
        return Signal::Inapplicable;
    };
    // A terminal document is off that scale whatever it declares: the
    // `stale_review` rule reads the same field against the same
    // horizon and does not review what the project has retired, so
    // asking a retired document for a review date would name a remedy
    // its own project would not take.
    if config.is_terminal(node.status.as_str()) {
        return Signal::Inapplicable;
    }
    let Some(reviewed) = node.reviewed else {
        return Signal::Undeclared;
    };
    let elapsed = (today - reviewed).num_days().max(0) as f64;
    Signal::Measured((1.0 - elapsed / stale_days as f64).clamp(0.0, 1.0))
}

fn drift_signal(
    graph: &Graph,
    config: &Config,
    root: &Path,
    repository: Option<&crate::git::Repository>,
    node: &Node,
) -> Signal {
    let Some(threshold) = config.detection.git_drift_threshold else {
        return Signal::Inapplicable;
    };
    // No repository means the signal is unmeasurable here, and a `0.0`
    // would report maximum drift from absence of evidence.
    let Some(repository) = repository else {
        return Signal::Inapplicable;
    };
    if threshold == 0 {
        // Unreachable under a loaded config — `Config::validate` rejects
        // `git_drift_threshold = 0` — so the backstop for unvalidated
        // library callers reports honest absence, never fabricated credit.
        return Signal::Inapplicable;
    }
    // The `git_drift` rule measures live documents against the source
    // they cover; a retired one has stopped tracking it by design, and
    // the component reading the same edges answers the same way.
    if config.is_terminal(node.status.as_str()) {
        return Signal::Inapplicable;
    }
    // What the node offers to measure is established before the anchor
    // it would be measured from: a node covering nothing has no drift
    // whatever it declares, and asking it for a `reviewed` date names a
    // remedy that would not produce the component. A target the project
    // does not hold is one the rule reports and a score cannot, so it
    // leaves the same absence here as an edge that was never offered.
    let targets: Vec<PathBuf> = crate::rules::git_drift::drift_targets(
        graph,
        config,
        crate::builder::scanner::ProjectFiles::working_tree(root),
        node,
    )
    .into_iter()
    .filter_map(crate::rules::git_drift::DriftTarget::path)
    .collect();
    if targets.is_empty() {
        return Signal::Inapplicable;
    }
    let Some(reviewed) = node.reviewed else {
        return Signal::Undeclared;
    };

    let mut total: u32 = 0;
    for target in &targets {
        // `None` means git could not measure this edge. Drop the whole
        // drift component rather than fabricate "no drift", mirroring
        // `backlinks_signal`'s treatment of an absent signal.
        let Some(commits) = crate::rules::git_drift::commits_since(repository, target, reviewed)
        else {
            return Signal::Inapplicable;
        };
        total = total.saturating_add(commits);
    }
    Signal::Measured((1.0 - total as f64 / threshold as f64).clamp(0.0, 1.0))
}

fn backlinks_signal(graph: &Graph, node: &Node, max_in: usize) -> Signal {
    if max_in == 0 {
        // No external incoming edges anywhere — the backlinks signal
        // is absent from the graph. Reporting `1.0` here would
        // fabricate maximum trust from absence of evidence; instead
        // drop the component so the composite renormalises over the
        // signals that are actually present.
        return Signal::Inapplicable;
    }
    // Self-references are filtered out — trust measures external
    // attention, and a doc citing itself is not external. Without
    // the filter a doc could inflate its own score by writing
    // `[[self-id]]` in the body.
    let in_count = distinct_linkers(graph, &node.id);
    let in_log = ((in_count + 1) as f64).ln();
    let max_log = ((max_in + 1) as f64).ln();
    Signal::Measured((in_log / max_log).clamp(0.0, 1.0))
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
    use crate::model::{Edge, Kind, ResolvedTarget, Status};
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

    /// The two absences a missing `reviewed` date produces, and the
    /// composites they leave. With no staleness horizon declared,
    /// nothing the document could write would produce freshness — the
    /// component is absent for every node alike and the composite
    /// renormalises over the rest. With a horizon declared, the run can
    /// measure freshness and this document supplies nothing: the
    /// component is named in `undeclared` and there is no composite,
    /// because renormalising would impute, for the component the
    /// document withheld, the score its other components produced.
    #[test]
    fn a_missing_review_date_reads_as_absence_or_as_no_composite() {
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let today = crate::test_today();

        let no_horizon = compute_trust(&g, &Config::default(), Path::new("."), "x", today).unwrap();
        assert!(no_horizon.components.freshness.is_none());
        assert!(
            no_horizon.undeclared.is_empty(),
            "an unmeasurable component asks the document for nothing: {:?}",
            no_horizon.undeclared
        );
        assert!(
            no_horizon.score.is_some(),
            "the composite renormalises over the components that apply"
        );

        let mut cfg = Config::default();
        cfg.detection.stale_days = Some(180);
        let horizon = compute_trust(&g, &cfg, Path::new("."), "x", today).unwrap();
        assert!(horizon.components.freshness.is_none());
        assert_eq!(horizon.undeclared, vec![TrustComponent::Freshness]);
        assert!(
            horizon.score.is_none(),
            "a withheld component leaves no composite; got {:?}",
            horizon.score
        );
    }

    /// The scoring rule cannot pay a document to withhold evidence. A
    /// review date long past scores below one recorded today, and
    /// declaring none is not a third, better answer between them — the
    /// document leaves the ranking's domain and is counted, because the
    /// composite it would carry is its other components under a name
    /// that claims more.
    #[test]
    fn withholding_a_review_date_never_buys_a_rank() {
        let today = crate::test_today();
        let mut cfg = Config::default();
        cfg.detection.stale_days = Some(180);
        let g = graph_with(
            vec![
                make_node("stale", "active", Some(today - Duration::days(170))),
                make_node("fresh", "active", Some(today)),
                make_node("silent", "active", None),
            ],
            vec![],
        );
        let out = compute_trust_ranking(
            &g,
            &cfg,
            Path::new("."),
            &TrustListOptions {
                extreme: TrustExtreme::Bottom,
                limit: 100,
                kind: None,
                status: None,
                below: None,
            },
            today,
        );
        let ids: Vec<&str> = out.entries.iter().map(|r| r.node.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["stale", "fresh"],
            "the ranking orders the documents that declared a date"
        );
        assert_eq!(
            out.unscored, 1,
            "the silent document is counted, not ranked"
        );
    }

    /// A zero-weighted component carries no evidence either way, so its
    /// absence is not a withheld declaration. A project that does not
    /// track review dates says so in `[trust.weights]` and keeps a
    /// composite over the components it does track.
    #[test]
    fn a_zero_weighted_component_is_never_undeclared() {
        let mut cfg = Config::default();
        cfg.detection.stale_days = Some(180);
        cfg.trust.weights = TrustWeights {
            status: 0.8,
            freshness: 0.0,
            drift: 0.0,
            backlinks: 0.2,
        };
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &cfg, Path::new("."), "x", crate::test_today()).unwrap();
        assert!(
            r.undeclared.is_empty(),
            "a zero weight asks for no declaration; got {:?}",
            r.undeclared
        );
        assert_eq!(r.score, Some(1.0));
    }

    /// `undeclared` names components by the keys `TrustComponents`
    /// serialises, so the two halves of the breakdown — what was
    /// measured and what could have been — join up on the wire.
    #[test]
    fn undeclared_names_a_component_the_breakdown_would_carry() {
        use serde_json::Value;
        let mut cfg = Config::default();
        cfg.detection.stale_days = Some(180);
        let g = graph_with(vec![make_node("x", "active", None)], vec![]);
        let r = compute_trust(&g, &cfg, Path::new("."), "x", crate::test_today()).unwrap();
        let json = serde_json::to_value(&r).unwrap();
        let obj = json.as_object().expect("entry must serialize as object");
        assert!(
            !obj.contains_key("score"),
            "an absent composite is omitted, never null or 0.0; got {obj:?}"
        );
        assert_eq!(obj.get("undeclared"), Some(&Value::from(vec!["freshness"])));

        let components = serde_json::to_value(TrustComponents {
            status: 1.0,
            freshness: Some(1.0),
            drift: Some(1.0),
            backlinks: Some(1.0),
        })
        .unwrap();
        for component in [
            TrustComponent::Status,
            TrustComponent::Freshness,
            TrustComponent::Drift,
            TrustComponent::Backlinks,
        ] {
            let name = serde_json::to_value(component).unwrap();
            let name = name.as_str().expect("component names serialise as strings");
            assert!(
                components.get(name).is_some(),
                "`{name}` must be a TrustComponents key; got {components:?}"
            );
        }
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
        // Active + no staleness horizon (freshness inapplicable) + no
        // git_drift_threshold (drift inapplicable) + no external incoming
        // edges anywhere (backlinks inapplicable) → nothing the document
        // could declare would produce any of the three, so the composite
        // renormalises over `status` alone. Default weights: status 0.4.
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
        assert!(matches!(err, crate::error::Error::MissingNode { .. }));
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
        // The default config declares no staleness horizon and no drift
        // threshold, and the graph carries no external incoming edges, so
        // every component but `status` is inapplicable for every node —
        // a review date changes nothing here. Composites:
        // 'b' archived: status=0 → 0/0.4 = 0.0
        // 'a' active: status=1 → 1.0 (excluded by < 1.0)
        // 'c' active: status=1 → 1.0 (excluded by < 1.0)
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
    fn backlinks_signal_excludes_self_loops() {
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
        // so backlinks_signal(x) = ln(1)/ln(2) = 0.0.
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
        // backlinks_signal(a) = ln(2)/ln(2) = 1.0; only weight active
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
    fn drift_signal_threshold_zero_reports_absence() {
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

    /// Drift's two absences, told apart by what the node offers to
    /// measure. A document covering no source has nothing to drift
    /// from, so no declaration would produce the component and asking
    /// for one would name a remedy that cannot work. A document that
    /// covers source and declares no `reviewed` anchor has a subject
    /// this run can measure and supplies nothing to measure it from.
    #[test]
    fn drift_tells_no_subject_apart_from_no_anchor() {
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
        let g = graph_with(
            vec![
                make_node("uncovered", "active", None),
                make_node("covering", "active", None),
            ],
            vec![Edge {
                source: "covering".to_string(),
                target: ResolvedTarget::unresolved(
                    "src/auth.rs",
                    crate::model::UnresolvedCause::Missing,
                ),
                relation: "covers".to_string(),
                location: "frontmatter:covers".to_string(),
            }],
        );
        let today = crate::test_today();

        let uncovered = compute_trust(&g, &cfg, dir.path(), "uncovered", today).unwrap();
        assert!(uncovered.components.drift.is_none());
        assert!(
            uncovered.undeclared.is_empty(),
            "nothing to measure asks the document for nothing: {:?}",
            uncovered.undeclared
        );
        assert!(
            uncovered.score.is_some(),
            "the composite renormalises over the components that apply"
        );

        let covering = compute_trust(&g, &cfg, dir.path(), "covering", today).unwrap();
        assert!(covering.components.drift.is_none());
        assert_eq!(covering.undeclared, vec![TrustComponent::Drift]);
        assert!(
            covering.score.is_none(),
            "a measurable subject with no anchor leaves no composite; got {:?}",
            covering.score
        );
    }

    #[test]
    fn drift_absent_when_git_cannot_measure() {
        // A reviewed node with an `implements` edge (a drift relation) to
        // another doc, threshold set, but `root` is not a git work tree:
        // git can't measure drift, so the component must report `None` —
        // never fabricate `1.0` (no drift) from absence of evidence, the
        // same discipline `backlinks_signal` follows.
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
