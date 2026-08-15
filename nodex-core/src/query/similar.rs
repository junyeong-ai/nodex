//! Vector-free similarity / duplicate detection.
//!
//! AI agents call this before scaffolding new memory to ask "have we
//! already discussed this?" — duplicate ADRs, redundant runbooks, and
//! near-restatements are surfaced as candidates with a per-component
//! breakdown so the agent can decide whether to supersede an existing
//! doc instead of creating noise.
//!
//! No embeddings — similarity is the weighted average of five
//! surface-level signals: title token Jaccard, tag overlap, kind
//! match, parent-directory match, and graph-neighbour overlap.
//!
//! Each component is `Option<f64>`: `None` means the *target* carries
//! nothing to rank on — an empty token or tag set, a spec field the
//! caller omitted, a pre-creation target with no graph id — so the
//! component is absent for every candidate alike and `compose`
//! renormalises over the signals the query does carry, instead of
//! conflating "nothing to compare" with "definitely dissimilar" (which
//! a hardcoded `0.0` would do). What the *candidate* lacks is never an
//! absence: no overlap with a set the target has is `0.0`, a
//! measurement. Renormalising there would rescale the composite for
//! precisely the candidates carrying the least evidence, and rank one
//! above a better match for declaring nothing. Presence being the
//! target's, so is the composite's: a query carrying no
//! positively-weighted signal gives no candidate a composite at all —
//! every candidate is outside the ranking's domain and counted in
//! [`RankingOutcome::unscored`], never ranked at a fabricated score.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{Config, SimilarityWeights};
use crate::error::Result;
use crate::model::Graph;

use super::{NodeRef, RankingOutcome};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SimilarityEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    /// Composite in `[0, 1]`. Always present: an entry exists only for
    /// a scored candidate — one with no composite never reaches the
    /// wire (it is excluded from the ranking and counted in
    /// [`RankingOutcome::unscored`]). Named `score`, identical in spine
    /// to every other item-list ranking entry ([`super::trust::TrustEntry`],
    /// [`super::search::SearchEntry`]).
    pub score: f64,
    pub components: SimilarityComponents,
}

/// Per-signal breakdown. Every field is `Option<f64>` so callers can
/// distinguish "we measured 0.0" from "no signal to measure". When a
/// component is `None`, its weight is excluded from the composite
/// denominator — see `compose`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SimilarityComponents {
    /// `None` when the *target's* title tokenises to an empty set
    /// after stopword + min-char filtering — the query carries no
    /// title signal, so no candidate can be ranked on one. A candidate
    /// whose own tokens are empty scores `Some(0.0)`: zero overlap
    /// against a token set that exists is a measurement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<f64>,
    /// `None` when the *target* carries no tags, for the reason
    /// `title` does. A tagless candidate scores `Some(0.0)` against a
    /// tagged target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<f64>,
    /// `None` when the target is a pre-creation spec without an
    /// explicit kind. Comparing "no kind" to a real kind would
    /// fabricate a 0.0 from absence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<f64>,
    /// `None` when the target has no parent directory to compare
    /// against — a pre-creation spec without `--parent-dir`. A node
    /// always has one: a document at the project root sits in `""`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<f64>,
    /// `None` when the target is a pre-creation spec (no graph id, so
    /// the neighbour set is undefined), or when the target node has no
    /// neighbours to compare a candidate's against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked: Option<f64>,
}

/// What we are comparing against. `Node` looks up an existing graph
/// node; `Spec` lets agents probe similarity *before* the document
/// they're about to scaffold exists.
pub enum SimilarityTarget<'a> {
    Node(&'a str),
    Spec {
        title: &'a str,
        kind: Option<&'a str>,
        tags: &'a [String],
        parent_dir: Option<&'a Path>,
    },
}

#[derive(Debug, Clone)]
pub struct SimilarityOptions {
    pub limit: usize,
}

impl SimilarityOptions {
    /// Default cap sourced from `[similarity].default_limit`. Score
    /// cutoffs are not part of the ranking primitive — callers that
    /// want them attach the filter at the CLI / consumer layer.
    pub fn from_config(config: &Config) -> Self {
        Self {
            limit: config.similarity.default_limit,
        }
    }
}

/// Rank every candidate against `target` by composite similarity, sort
/// descending with id tie-break, truncate to `opts.limit`. No
/// threshold gate — the operator decides what enters the ranking via
/// `limit`, and any score-cutoff filter is applied by the caller.
///
/// Presence is the target's, so a query carrying no positively-weighted
/// signal gives no candidate a composite: each is skipped before the
/// heap push and counted in [`RankingOutcome::unscored`]. The prune
/// cannot mask the exclusion — it fires only once the heap holds a
/// scored candidate, which such a query never produces.
///
/// The cheap-signals upper-bound prune is still in play: we maintain a
/// min-heap of size `limit` and skip candidates whose optimistic
/// composite cannot enter the top-K. This keeps the worst case linear
/// in node count without a hardcoded cutoff to drift against.
pub fn compute_similarity(
    graph: &Graph,
    config: &Config,
    target: &SimilarityTarget<'_>,
    opts: &SimilarityOptions,
) -> Result<RankingOutcome<SimilarityEntry>> {
    let target_view = TargetView::extract(graph, target)?;
    let stop_words: BTreeSet<&str> = config
        .similarity
        .title_stop_words
        .iter()
        .map(String::as_str)
        .collect();
    let target_title_tokens = tokenize_title(target_view.title, &stop_words);
    let target_tag_set: BTreeSet<String> = target_view.tags.iter().cloned().collect();
    // Pre-creation specs have no graph id → neighbour set is
    // undefined, not "empty". Track the distinction so the `linked`
    // component reports `None` instead of fabricating a 0.0.
    let target_in_graph = target_view.exclude_id.is_some();
    let target_neighbours = neighbour_set(graph, target_view.exclude_id);
    let weights = config.similarity.weights;

    // Top-K min-heap: smallest composite at the top, so we can cheaply
    // ask "is this candidate worth computing in full?". A composite
    // whose optimistic upper bound is below the current K-th score
    // can never enter the heap.
    let mut top: std::collections::BinaryHeap<HeapEntry> =
        std::collections::BinaryHeap::with_capacity(opts.limit.saturating_add(1));
    let mut unscored = 0usize;

    for n in graph
        .nodes()
        .values()
        .filter(|n| target_view.exclude_id.is_none_or(|id| n.id != id))
    {
        let title = jaccard(&target_title_tokens, &tokenize_title(&n.title, &stop_words));
        let tags = jaccard(
            &target_tag_set,
            &n.tags.iter().cloned().collect::<BTreeSet<_>>(),
        );
        let kind = kind_match(target_view.kind, n.kind.as_str());
        let directory = directory_match(target_view.parent_dir, n.path.as_path());

        // Stage 1: cheap signals. Establish the optimistic upper bound
        // on the final composite using only the components computed
        // so far. `linked` and any absent component contribute 1.0 to
        // both numerator and denominator, so the bound is always
        // `>= compose(...)`. Skip the candidate when the heap is
        // already full and the bound cannot break in.
        if opts.limit == 0 {
            break;
        }
        if top.len() >= opts.limit {
            let upper_bound = compose_upper_bound(&weights, title, tags, kind, directory);
            let kth = top.peek().expect("heap non-empty when len >= limit").score;
            if upper_bound < kth {
                continue;
            }
        }

        // Stage 2: linked Jaccard. Only meaningful when the target
        // has a graph id; otherwise the neighbour set is undefined,
        // not "empty".
        let linked = if target_in_graph {
            jaccard(&target_neighbours, &neighbour_set(graph, Some(&n.id)))
        } else {
            None
        };
        let components = SimilarityComponents {
            title,
            tags,
            kind,
            directory,
            linked,
        };
        // The target carries no positively-weighted signal → no
        // composite for any candidate. Each is outside the ranking's
        // domain; counted, not ranked at a fabricated minimum.
        let Some(score) = compose(&weights, &components) else {
            unscored += 1;
            continue;
        };
        top.push(HeapEntry {
            score,
            entry: SimilarityEntry {
                node: NodeRef::from_node(n),
                score,
                components,
            },
        });
        if top.len() > opts.limit {
            top.pop();
        }
    }

    let mut entries: Vec<SimilarityEntry> = top.into_iter().map(|h| h.entry).collect();
    entries.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    Ok(RankingOutcome { entries, unscored })
}

/// Heap entry that orders so [`std::collections::BinaryHeap`] (a
/// max-heap) behaves as a min-heap over the composite score, with id
/// tie-break inverted to match the final descending sort. NaN scores
/// fold to `Ordering::Equal` so the heap never panics.
struct HeapEntry {
    score: f64,
    entry: SimilarityEntry,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score && self.entry.node.id == other.entry.node.id
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse the score comparison so the smallest score sits at
        // the heap's top; on ties, the *higher* id sits at the top so
        // popping during overflow drops the would-be-last entry from
        // the final descending order. The final sort orders equal
        // scores by ascending id (lower id wins), so the heap must
        // evict the higher id on a tie — `BinaryHeap::pop` returns
        // the `Ord::Greater` element, so this entry compares
        // `Greater` when its id is larger.
        other
            .score
            .partial_cmp(&self.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.entry.node.id.cmp(&other.entry.node.id))
    }
}

struct TargetView<'a> {
    title: &'a str,
    kind: Option<&'a str>,
    tags: Vec<String>,
    parent_dir: Option<&'a Path>,
    exclude_id: Option<&'a str>,
}

impl<'a> TargetView<'a> {
    fn extract(graph: &'a Graph, target: &SimilarityTarget<'a>) -> Result<Self> {
        match target {
            SimilarityTarget::Node(id) => {
                let n = graph.require_node(id)?;
                Ok(Self {
                    title: &n.title,
                    kind: Some(n.kind.as_str()),
                    tags: n.tags.clone(),
                    parent_dir: n.path.parent(),
                    exclude_id: Some(id),
                })
            }
            SimilarityTarget::Spec {
                title,
                kind,
                tags,
                parent_dir,
            } => Ok(Self {
                title,
                kind: *kind,
                tags: tags.to_vec(),
                parent_dir: *parent_dir,
                exclude_id: None,
            }),
        }
    }
}

fn neighbour_set<'a>(graph: &'a Graph, id: Option<&str>) -> BTreeSet<&'a str> {
    // The undirected one-hop set, through the shared reachability
    // primitive — the same "one hop from a node" definition `find_chain`
    // and the structural walks use. `linked` similarity is the Jaccard
    // overlap of two nodes' neighbour sets.
    match id {
        Some(id) => super::structure::adjacent_undirected(graph, id, None)
            .into_iter()
            .collect(),
        None => BTreeSet::new(),
    }
}

fn tokenize_title(title: &str, stop_words: &BTreeSet<&str>) -> BTreeSet<String> {
    // Char count, not byte length: a single Korean syllable is 3 bytes
    // but one character — applying a byte-length cutoff would treat
    // ASCII and CJK alphanumerics inconsistently.
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2 && !stop_words.contains(*t))
        .map(String::from)
        .collect()
}

/// Overlap of a candidate's set with the *target's*. `None` when the
/// target carries no such set: the query has nothing to rank by, so
/// the component is absent for every candidate alike — the same
/// uniform absence `kind_match` and `directory_match` report when the
/// target side is missing. A target that does carry one measures every
/// candidate, the empty ones included: no overlap with a set that
/// exists is `0.0`, a measurement rather than an absence.
///
/// Asymmetric on purpose. The comparison is one target against many
/// candidates, so which side is empty decides what the emptiness is
/// about. Read symmetrically, a candidate was absent when it happened
/// to share the target's emptiness and measured `0.0` when it did not,
/// which renormalised the composite for exactly the candidates
/// carrying the least — ranking a candidate above a better-matching
/// one for declaring nothing.
fn jaccard<T: Ord>(target: &BTreeSet<T>, candidate: &BTreeSet<T>) -> Option<f64> {
    if target.is_empty() {
        return None;
    }
    let intersection = target.intersection(candidate).count();
    let union = target.union(candidate).count();
    Some(intersection as f64 / union as f64)
}

fn kind_match(target_kind: Option<&str>, candidate_kind: &str) -> Option<f64> {
    let t = target_kind?;
    Some(if t == candidate_kind { 1.0 } else { 0.0 })
}

fn directory_match(target_dir: Option<&Path>, candidate_path: &Path) -> Option<f64> {
    let t = target_dir?;
    let candidate_dir = candidate_path.parent()?;
    Some(if t == candidate_dir { 1.0 } else { 0.0 })
}

/// Weighted average over the *present* components. `None` exactly when
/// the weight sum over present components is zero (weights are
/// load-validated finite and non-negative): the candidate shares no
/// positively-weighted signal with the target, so no composite exists
/// — the same honest-absence rule the components themselves follow.
fn compose(w: &SimilarityWeights, c: &SimilarityComponents) -> Option<f64> {
    let mut weighted = 0.0;
    let mut weight_sum = 0.0;
    if let Some(v) = c.title {
        weighted += v * w.title;
        weight_sum += w.title;
    }
    if let Some(v) = c.tags {
        weighted += v * w.tags;
        weight_sum += w.tags;
    }
    if let Some(v) = c.kind {
        weighted += v * w.kind;
        weight_sum += w.kind;
    }
    if let Some(v) = c.directory {
        weighted += v * w.directory;
        weight_sum += w.directory;
    }
    if let Some(v) = c.linked {
        weighted += v * w.linked;
        weight_sum += w.linked;
    }
    if weight_sum <= 0.0 {
        None
    } else {
        Some((weighted / weight_sum).clamp(0.0, 1.0))
    }
}

/// Upper bound on the final composite score using only the cheap
/// components. Each absent component is treated as the most
/// optimistic case (a future 1.0 contribution to both numerator and
/// denominator); each present component contributes its measured
/// value. The result is always `>= compose(...)`, so a candidate
/// pruned by the heap's top-K gate can never have entered the top-K.
fn compose_upper_bound(
    w: &SimilarityWeights,
    title: Option<f64>,
    tags: Option<f64>,
    kind: Option<f64>,
    directory: Option<f64>,
) -> f64 {
    // Numerator: known values get their measured score; unknown
    // values (including the not-yet-computed `linked`) get the
    // optimistic 1.0.
    let mut numerator = 0.0;
    let mut denominator = 0.0;
    for (val, weight) in [
        (title, w.title),
        (tags, w.tags),
        (kind, w.kind),
        (directory, w.directory),
    ] {
        // Even when val is None we add the weight on both sides to
        // model the "this might still come back as 1.0" case. (For
        // title/tags/kind/directory `None` is final — but assuming
        // 1.0 only inflates the upper bound, which is safe for
        // pruning.)
        let assumed = val.unwrap_or(1.0);
        numerator += assumed * weight;
        denominator += weight;
    }
    // `linked` is not yet computed → assume 1.0.
    numerator += 1.0 * w.linked;
    denominator += w.linked;
    if denominator <= 0.0 {
        0.0
    } else {
        (numerator / denominator).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Kind, Node, ResolvedTarget, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str, title: &str, kind: &str, tags: Vec<&str>, path: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(path),
            title: title.to_string(),
            kind: Kind::new(kind),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: tags.into_iter().map(String::from).collect(),
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

    fn graph_with(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(
            map,
            vec![],
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    #[test]
    fn identical_title_tokens_produce_high_similarity() {
        let g = graph_with(vec![
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy v2", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions::from_config(&cfg),
        )
        .unwrap()
        .entries;
        assert!(!entries.is_empty(), "should find similar via shared tokens");
        assert_eq!(entries[0].node.id, "b");
        assert!(entries[0].components.title.unwrap() > 0.5);
        assert_eq!(entries[0].components.kind, Some(1.0));
    }

    #[test]
    fn stop_words_are_ignored() {
        let g = graph_with(vec![
            node("a", "the auth", "adr", vec![], "docs/a.md"),
            node("b", "the payment", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions { limit: 10 },
        )
        .unwrap()
        .entries;
        // 'the' is dropped, 'auth' and 'payment' remain — different
        // tokens → title similarity = 0.
        let b_entry = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(b_entry.components.title, Some(0.0));
    }

    #[test]
    fn unrelated_docs_rank_below_related_ones() {
        // With no threshold gate every candidate enters the ranking;
        // the contract is that the more-similar doc outranks the
        // less-similar one. A consumer that wants to drop low-scoring
        // tail entries applies its own `--min-score` filter.
        let g = graph_with(vec![
            node("a", "auth retry", "adr", vec![], "docs/a.md"),
            node("close", "auth retry tweaks", "adr", vec![], "docs/close.md"),
            node(
                "far",
                "completely different topic",
                "guide",
                vec![],
                "other/far.md",
            ),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions::from_config(&cfg),
        )
        .unwrap()
        .entries;
        let ids: Vec<&str> = entries.iter().map(|e| e.node.id.as_str()).collect();
        let close_pos = ids
            .iter()
            .position(|i| *i == "close")
            .expect("close present");
        let far_pos = ids.iter().position(|i| *i == "far").expect("far present");
        assert!(
            close_pos < far_pos,
            "related doc must outrank unrelated; got {ids:?}"
        );
    }

    #[test]
    fn spec_target_works_pre_creation() {
        let g = graph_with(vec![node(
            "existing",
            "auth retry policy",
            "adr",
            vec!["auth"],
            "docs/existing.md",
        )]);
        let cfg = Config::default();
        let target = SimilarityTarget::Spec {
            title: "Auth retry policy v2",
            kind: Some("adr"),
            tags: &["auth".to_string()],
            parent_dir: Some(Path::new("docs")),
        };
        let entries = compute_similarity(&g, &cfg, &target, &SimilarityOptions { limit: 10 })
            .unwrap()
            .entries;
        assert!(!entries.is_empty(), "should warn about existing duplicate");
        assert_eq!(entries[0].node.id, "existing");
    }

    #[test]
    fn target_node_excluded_from_candidates() {
        let g = graph_with(vec![
            node("a", "auth retry", "adr", vec![], "docs/a.md"),
            node("b", "auth retry alternative", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions { limit: 10 },
        )
        .unwrap()
        .entries;
        assert!(entries.iter().all(|e| e.node.id != "a"));
    }

    #[test]
    fn missing_node_target_errors() {
        let g = graph_with(vec![]);
        let cfg = Config::default();
        let err = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("ghost"),
            &SimilarityOptions::from_config(&cfg),
        )
        .unwrap_err();
        assert!(matches!(err, crate::error::Error::MissingNode { .. }));
    }

    #[test]
    fn tokenize_title_uses_char_count_for_unicode() {
        // ASCII single chars are dropped; CJK single syllables (3
        // bytes, 1 char) must also be dropped. A byte-length filter
        // would keep CJK and treat Latin / CJK inconsistently —
        // tokenisation goes through char count for parity.
        let stop: BTreeSet<&str> = BTreeSet::new();
        let tokens = tokenize_title("a 가 ab 가나", &stop);
        assert!(!tokens.contains("a"), "ASCII 1-char must drop");
        assert!(!tokens.contains("가"), "CJK 1-char must drop too");
        assert!(tokens.contains("ab"));
        assert!(tokens.contains("가나"));
    }

    #[test]
    fn jaccard_absence_follows_the_target_side() {
        let empty: BTreeSet<&str> = BTreeSet::new();
        let one: BTreeSet<&str> = ["a"].into_iter().collect();
        // An empty target carries nothing to rank by, whatever the
        // candidate holds — the absence is the query's, so it reads the
        // same for every candidate.
        assert_eq!(jaccard::<&str>(&empty, &empty), None);
        assert_eq!(jaccard(&empty, &one), None);
        // A target that carries a set measures every candidate: no
        // overlap with a set that exists is 0.0, not absence.
        assert_eq!(jaccard(&one, &empty), Some(0.0));
        assert_eq!(jaccard(&one, &one), Some(1.0));
    }

    // ---- Honest absence regression tests --------------------------------

    /// A target whose title tokenises to an empty set (only stop words
    /// / single chars) carries no title signal — the composite must
    /// rely on kind/directory alone, not be dragged down by a
    /// fabricated 0.0.
    #[test]
    fn title_absent_when_the_target_tokenises_empty() {
        let g = graph_with(vec![
            // After tokenisation (drop 1-char, drop 'a'/'i'/etc. if
            // configured as stop words), titles are empty — but the
            // defaults don't include 'a'/'i', so use truly single-char
            // titles to guarantee empty token sets.
            node("a", "x", "adr", vec![], "docs/a.md"),
            node("b", "y", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions { limit: 10 },
        )
        .unwrap()
        .entries;
        let b_entry = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(
            b_entry.components.title, None,
            "a target that tokenises empty carries no title signal for any candidate"
        );
    }

    /// A target carrying no tags cannot be compared on tags, and the
    /// composite must exclude the tags weight from the denominator. A
    /// target that carries them measures every candidate, so the
    /// candidate holding none is `0.0` — the two directions are not
    /// the same question.
    #[test]
    fn tags_absent_for_a_tagless_target_and_zero_for_a_tagless_candidate() {
        let g = graph_with(vec![
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy", "adr", vec![], "docs/b.md"),
            node(
                "tagged",
                "auth retry policy",
                "adr",
                vec!["auth"],
                "docs/tagged.md",
            ),
        ]);
        let cfg = Config::default();
        let tags_of = |target, id: &str| {
            compute_similarity(&g, &cfg, &target, &SimilarityOptions { limit: 10 })
                .unwrap()
                .entries
                .iter()
                .find(|e| e.node.id == id)
                .expect("candidate ranks")
                .components
                .tags
        };
        assert_eq!(
            tags_of(SimilarityTarget::Node("a"), "tagged"),
            None,
            "a tagless target carries no tag signal, whatever the candidate holds"
        );
        assert_eq!(
            tags_of(SimilarityTarget::Node("tagged"), "b"),
            Some(0.0),
            "no overlap with a tag set that exists is a measurement, not an absence"
        );
    }

    /// Spec target without an explicit kind has no kind signal — the
    /// component must be `None` so the composite renormalises over
    /// the remaining four signals.
    #[test]
    fn kind_absent_for_spec_target_without_kind() {
        let g = graph_with(vec![node(
            "existing",
            "auth retry policy",
            "adr",
            vec![],
            "docs/existing.md",
        )]);
        let cfg = Config::default();
        let target = SimilarityTarget::Spec {
            title: "Auth retry policy",
            kind: None, // <- missing
            tags: &[],
            parent_dir: Some(Path::new("docs")),
        };
        let entries = compute_similarity(&g, &cfg, &target, &SimilarityOptions { limit: 10 })
            .unwrap()
            .entries;
        let e = entries.iter().find(|e| e.node.id == "existing").unwrap();
        assert_eq!(
            e.components.kind, None,
            "spec target without kind must report kind=None"
        );
    }

    /// Spec target without `parent_dir` has no directory signal. The
    /// `linked` component is also absent because there's no graph id
    /// to anchor the neighbour set.
    #[test]
    fn directory_absent_for_pre_creation_spec() {
        let g = graph_with(vec![node(
            "existing",
            "auth retry policy",
            "adr",
            vec![],
            "docs/existing.md",
        )]);
        let cfg = Config::default();
        let target = SimilarityTarget::Spec {
            title: "Auth retry policy",
            kind: Some("adr"),
            tags: &[],
            parent_dir: None, // <- missing
        };
        let entries = compute_similarity(&g, &cfg, &target, &SimilarityOptions { limit: 10 })
            .unwrap()
            .entries;
        let e = entries.iter().find(|e| e.node.id == "existing").unwrap();
        assert_eq!(
            e.components.directory, None,
            "spec target without parent_dir must report directory=None"
        );
    }

    /// Spec target has no graph id, so the neighbour set is undefined
    /// — `linked` must be `None`, not "Jaccard against an empty set".
    #[test]
    fn linked_absent_for_pre_creation_spec() {
        let g = graph_with(vec![node(
            "existing",
            "auth retry policy",
            "adr",
            vec![],
            "docs/existing.md",
        )]);
        let cfg = Config::default();
        let target = SimilarityTarget::Spec {
            title: "Auth retry policy",
            kind: Some("adr"),
            tags: &[],
            parent_dir: Some(Path::new("docs")),
        };
        let entries = compute_similarity(&g, &cfg, &target, &SimilarityOptions { limit: 10 })
            .unwrap()
            .entries;
        let e = entries.iter().find(|e| e.node.id == "existing").unwrap();
        assert_eq!(
            e.components.linked, None,
            "pre-creation spec must report linked=None (no graph id)"
        );
    }

    /// Composite must renormalise over the *present* signals only.
    /// Two perfectly identical surface signals with all other signals
    /// absent should still produce a score of 1.0 — not be dragged
    /// down by absent components being counted as 0.0.
    #[test]
    fn composite_renormalises_over_present_signals_only() {
        // Both docs: same title tokens, same kind, same directory.
        // No tags on either side → tags = None.
        // No edges → both neighbour sets empty → linked = None.
        let g = graph_with(vec![
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions { limit: 10 },
        )
        .unwrap()
        .entries;
        let b = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(b.components.title, Some(1.0));
        assert_eq!(b.components.tags, None);
        assert_eq!(b.components.kind, Some(1.0));
        assert_eq!(b.components.directory, Some(1.0));
        assert_eq!(b.components.linked, None);
        assert!(
            (b.score - 1.0).abs() < 1e-9,
            "composite must be 1.0 when every present signal is 1.0; got {}",
            b.score
        );
    }

    /// `linked` is `None` when the target IS in the graph but has no
    /// neighbours of its own to compare a candidate's against.
    #[test]
    fn linked_absent_when_the_target_has_no_neighbours() {
        let g = graph_with(vec![
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions { limit: 10 },
        )
        .unwrap()
        .entries;
        let b = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(
            b.components.linked, None,
            "a target with no neighbours carries no linked signal for any candidate"
        );
    }

    /// `linked` IS present (and 1.0 / 0.0 / fractional) when the
    /// target is a graph node with neighbours. Sanity check that we
    /// haven't accidentally turned every linked into None.
    #[test]
    fn linked_present_when_the_target_has_neighbours() {
        // a → c (edge), b → c (edge). Then a's neighbours = {c}, b's
        // neighbours = {c} → linked = 1.0.
        let mut map = IndexMap::new();
        for n in [
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy", "adr", vec![], "docs/b.md"),
            node("c", "shared peer", "adr", vec![], "docs/c.md"),
        ] {
            map.insert(n.id.clone(), n);
        }
        let edges = vec![
            Edge {
                source: "a".to_string(),
                target: ResolvedTarget::Resolved {
                    id: "c".to_string(),
                },
                relation: "related".to_string(),
                location: "frontmatter:related".to_string(),
            },
            Edge {
                source: "b".to_string(),
                target: ResolvedTarget::Resolved {
                    id: "c".to_string(),
                },
                relation: "related".to_string(),
                location: "frontmatter:related".to_string(),
            },
        ];
        let g = Graph::new(
            map,
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions { limit: 10 },
        )
        .unwrap()
        .entries;
        let b = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(
            b.components.linked,
            Some(1.0),
            "both nodes share neighbour {{c}}; linked must be 1.0"
        );
    }

    /// `directory_match`'s candidate-side `candidate_path.parent()?`
    /// branch reads as if a candidate without a parent directory is a
    /// real state to handle, but no real document graph can reach it.
    /// Nodex paths are project-relative `PathBuf` values constructed
    /// by the scanner from globbed files under the project root.
    /// `PathBuf::from("root.md").parent()` returns `Some("")` (empty
    /// path, not `None`) and any deeper path returns `Some("docs")`
    /// or similar. `Path::parent()` only returns `None` for the empty
    /// path or the filesystem root, neither of which the scanner can
    /// produce. The branch stays defensive but is unreachable from
    /// graph input — documented here in lieu of an unreachable test.
    #[test]
    fn directory_match_candidate_parent_is_structurally_always_some() {
        use std::path::PathBuf;
        // Anchors the structural invariant the comment above relies
        // on. If a future Rust release changes `Path::parent`
        // semantics, this test fails and the comment must be
        // re-examined.
        assert_eq!(
            PathBuf::from("root.md").parent(),
            Some(Path::new("")),
            "root-level docs report parent = empty path, never None"
        );
        assert_eq!(PathBuf::from("docs/a.md").parent(), Some(Path::new("docs")));
    }

    #[test]
    fn json_omits_absent_similarity_components() {
        // Wire-contract anchor (mirrors `trust::json_omits_absent_components`):
        // an absent component must be omitted from the JSON entirely,
        // never serialised as `null` or `0.0`. Partial-absence fixture:
        // both docs share title tokens / kind / directory (present) but
        // neither carries tags or edges (tags / linked absent) — a
        // scored candidate whose absent components must vanish from the
        // wire while the present ones survive.
        let g = graph_with(vec![
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions { limit: 10 },
        )
        .unwrap()
        .entries;
        let e = entries.iter().find(|e| e.node.id == "b").unwrap();
        // Sanity: the fixture must exercise both presence and absence.
        assert_eq!(e.components.title, Some(1.0));
        assert_eq!(e.components.tags, None);
        assert_eq!(e.components.linked, None);

        let json = serde_json::to_value(&e.components).unwrap();
        let obj = json
            .as_object()
            .expect("components must serialize as object");
        for key in ["tags", "linked"] {
            assert!(
                !obj.contains_key(key),
                "{key} must be omitted when absent; got {obj:?}"
            );
        }
        for key in ["title", "kind", "directory"] {
            assert!(
                obj.contains_key(key),
                "{key} is present and must serialize; got {obj:?}"
            );
        }
    }

    /// A query carrying no signal at all ranks nothing: every candidate
    /// is outside the ranking's domain and counted in `unscored` —
    /// never ranked at a fabricated minimum a consumer could misread as
    /// "measured 0.0".
    #[test]
    fn a_query_carrying_no_signal_ranks_nothing() {
        let g = graph_with(vec![node(
            "candidate",
            "Storage Layout Decisions",
            "adr",
            vec!["storage"],
            "docs/candidate.md",
        )]);
        let cfg = Config::default();
        let target = SimilarityTarget::Spec {
            title: "a",       // single ASCII char → tokenises empty → title None
            kind: None,       // → kind component None
            tags: &[],        // → tags None
            parent_dir: None, // → directory None; spec → linked None
        };
        let outcome =
            compute_similarity(&g, &cfg, &target, &SimilarityOptions { limit: 10 }).unwrap();
        assert!(
            outcome.entries.is_empty(),
            "a well-described candidate is unrankable by a query that asks nothing; got {:?}",
            outcome.entries
        );
        assert_eq!(outcome.unscored, 1, "the exclusion is counted, not silent");
    }

    /// The stage-1 prune decides what is *ranked*, never what is
    /// counted. A `limit` well below the corpus size fills the heap on
    /// the first candidate and prunes from the second onward, and the
    /// unscored tally has to come out the same as it would unpruned —
    /// on both sides of the split, since whether a candidate can be
    /// scored at all is decided by the target and so holds for the
    /// whole corpus at once.
    #[test]
    fn prune_decides_the_ranking_never_the_unscored_count() {
        let g = graph_with(vec![
            node("s1", "auth retry policy", "adr", vec![], "docs/s1.md"),
            node("s2", "payment ledger notes", "adr", vec![], "docs/s2.md"),
            node("s3", "deploy runbook steps", "adr", vec![], "docs/s3.md"),
            node("s4", "auth retry budget", "adr", vec![], "docs/s4.md"),
        ]);
        let cfg = Config::default();

        let ranked = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Spec {
                title: "auth retry policy",
                kind: Some("adr"),
                tags: &[],
                parent_dir: Some(Path::new("docs")),
            },
            &SimilarityOptions { limit: 1 },
        )
        .unwrap();
        assert_eq!(ranked.entries.len(), 1, "the limit caps the ranking");
        assert_eq!(
            ranked.unscored, 0,
            "every candidate is scorable against a signal-carrying target"
        );

        let blind = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Spec {
                title: "a", // tokenises empty
                kind: None,
                tags: &[],
                parent_dir: None,
            },
            &SimilarityOptions { limit: 1 },
        )
        .unwrap();
        assert!(
            blind.entries.is_empty(),
            "a target with no signal ranks nothing; got {:?}",
            blind.entries
        );
        assert_eq!(
            blind.unscored, 4,
            "every candidate is counted, not just the ones the limit had room for"
        );
    }

    /// A candidate is never advantaged by declaring less. Against a
    /// target carrying no tags, two candidates matching the title
    /// equally must score equally — read symmetrically, the tagged one
    /// measured `0.0` while the tagless one had its composite
    /// renormalised, ranking the candidate with the least evidence
    /// above its equal.
    #[test]
    fn a_candidate_is_not_advantaged_by_declaring_nothing() {
        let g = graph_with(vec![
            node(
                "tagged",
                "auth retry policy",
                "adr",
                vec!["auth"],
                "docs/tagged.md",
            ),
            node("bare", "auth retry policy", "adr", vec![], "docs/bare.md"),
        ]);
        let cfg = Config::default();
        let outcome = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Spec {
                title: "auth retry policy",
                kind: Some("adr"),
                tags: &[],
                parent_dir: Some(Path::new("docs")),
            },
            &SimilarityOptions { limit: 10 },
        )
        .unwrap();
        let tagged = outcome.entries.iter().find(|e| e.node.id == "tagged");
        let bare = outcome.entries.iter().find(|e| e.node.id == "bare");
        let (tagged, bare) = (tagged.unwrap(), bare.unwrap());
        assert_eq!(
            (tagged.components.tags, bare.components.tags),
            (None, None),
            "a tagless target carries no tag signal for either candidate"
        );
        assert!(
            (tagged.score - bare.score).abs() < 1e-9,
            "equal title match must score equally: tagged {}, bare {}",
            tagged.score,
            bare.score
        );
    }

    /// Regression: with three candidates that hash to the *same*
    /// composite score, `limit=2` must retain the two lowest ids.
    /// The final sort orders equal scores ascending by id, so the
    /// heap-eviction tie-break has to drop the highest id — not the
    /// lowest. (Prior bug: heap evicted the lower id, leaving the
    /// final result inconsistent with the documented ordering.)
    #[test]
    fn top_k_tie_break_keeps_lower_id() {
        let g = graph_with(vec![
            node("aaa", "auth retry policy", "adr", vec![], "docs/aaa.md"),
            node("bbb", "auth retry policy", "adr", vec![], "docs/bbb.md"),
            node("ccc", "auth retry policy", "adr", vec![], "docs/ccc.md"),
        ]);
        let cfg = Config::default();
        // Use a pre-creation Spec target so none of the candidates is
        // excluded, and so every candidate gets the same component
        // values across the board → identical composite scores.
        let target = SimilarityTarget::Spec {
            title: "auth retry policy",
            kind: Some("adr"),
            tags: &[],
            parent_dir: Some(Path::new("docs")),
        };
        let entries = compute_similarity(&g, &cfg, &target, &SimilarityOptions { limit: 2 })
            .unwrap()
            .entries;
        let ids: Vec<&str> = entries.iter().map(|e| e.node.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["aaa", "bbb"],
            "tied scores must keep the two lowest ids in ascending order; got {ids:?}",
        );
    }

    #[test]
    fn compute_similarity_limit_zero_returns_empty() {
        // Library-level contract: `limit=0` is a legitimate "skip this
        // ranking" request and must return an empty Vec without
        // panicking. The CLI rejects zero up-front to surface the
        // operator footgun, but the library accepts every limit
        // because composed callers may pass zero in a tight loop where
        // it means "no candidates this round". Anchors the heap's
        // `opts.limit == 0` short-circuit at `compute_similarity:160`.
        let g = graph_with(vec![
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy v2", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions { limit: 0 },
        )
        .unwrap()
        .entries;
        assert!(
            entries.is_empty(),
            "limit=0 must return empty; got {} entries",
            entries.len()
        );
    }

    #[test]
    fn compute_similarity_single_node_graph_excluded_target_returns_empty() {
        // Smallest possible Node-target graph: one node, which *is*
        // the target. The candidate filter at `compute_similarity:144`
        // excludes it; the corpus is now empty so the ranking must be
        // empty too. Defends the "self never ranks against itself"
        // contract on a graph with no other candidates to mask the
        // bug.
        let g = graph_with(vec![node(
            "solo",
            "auth retry policy",
            "adr",
            vec!["auth"],
            "docs/solo.md",
        )]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("solo"),
            &SimilarityOptions { limit: 10 },
        )
        .unwrap()
        .entries;
        assert!(
            entries.is_empty(),
            "single-node graph with target excluded must return empty; got {entries:?}",
        );
    }
}
