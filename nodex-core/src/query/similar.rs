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
//! Each component is `Option<f64>`: `None` means "no signal" (empty
//! token set, missing spec field, pre-creation target with no graph
//! id). Absence is propagated honestly through `compose`, which
//! renormalises over the *present* signals' weights instead of
//! conflating "no signal" with "definitely dissimilar" (which a
//! hardcoded `0.0` would do).

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{Config, SimilarityWeights};
use crate::error::Result;
use crate::model::{Graph, ResolvedTarget};

use super::NodeRef;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SimilarEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub similarity: f64,
    pub components: SimilarityComponents,
}

/// Per-signal breakdown. Every field is `Option<f64>` so callers can
/// distinguish "we measured 0.0" from "no signal to measure". When a
/// component is `None`, its weight is excluded from the composite
/// denominator — see [`compose`].
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SimilarityComponents {
    /// `None` when both target and candidate have empty title token
    /// sets after stopword + min-char filtering — empty-vs-empty is
    /// not a signal. One-side-empty returns `Some(0.0)` because zero
    /// overlap against a present token set is a meaningful signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<f64>,
    /// `None` when both target and candidate have empty tag sets —
    /// two tagless docs carry no tag signal. One-side-empty returns
    /// `Some(0.0)` for the same reason `title` does: zero overlap is
    /// a meaningful signal when the other side has tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<f64>,
    /// `None` when the target is a pre-creation spec without an
    /// explicit kind. Comparing "no kind" to a real kind would
    /// fabricate a 0.0 from absence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<f64>,
    /// `None` when the target has no parent directory to compare
    /// against — pre-creation spec without `--parent-dir`, or a node
    /// stored at the repo root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<f64>,
    /// `None` when the target is a pre-creation spec (no graph id, so
    /// the neighbour set is undefined), or when both target and
    /// candidate have empty neighbour sets.
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
    pub threshold: f64,
    pub limit: usize,
}

impl SimilarityOptions {
    /// Defaults sourced from `[similarity]` config block.
    pub fn from_config(config: &Config) -> Self {
        Self {
            threshold: config.similarity.threshold,
            limit: config.similarity.default_limit,
        }
    }
}

/// Find nodes whose composite similarity to `target` reaches
/// `opts.threshold`, sorted desc with id tie-break and capped at
/// `opts.limit`.
pub fn compute_similarity(
    graph: &Graph,
    config: &Config,
    target: &SimilarityTarget<'_>,
    opts: &SimilarityOptions,
) -> Result<Vec<SimilarEntry>> {
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

    let mut entries: Vec<SimilarEntry> = graph
        .nodes()
        .values()
        .filter(|n| target_view.exclude_id.is_none_or(|id| n.id != id))
        .filter_map(|n| {
            let title = jaccard(&target_title_tokens, &tokenize_title(&n.title, &stop_words));
            let tags = jaccard(
                &target_tag_set,
                &n.tags.iter().cloned().collect::<BTreeSet<_>>(),
            );
            let kind = kind_match(target_view.kind, n.kind.as_str());
            let directory = directory_match(target_view.parent_dir, n.path.as_path());

            // Stage 1: cheap signals. Establish an upper bound on the
            // final composite without computing neighbours yet. With
            // `None` components excluded from the denominator, we
            // can't simply add `weights.linked` to a fixed sum —
            // instead, model the most optimistic outcome: present
            // components keep their measured value, absent components
            // either stay absent (excluded from both sides of the
            // ratio) or hit their max of 1.0 (added to both). The
            // tightest upper bound is to assume every absent
            // component will materialise as a 1.0 — which is what the
            // original code did implicitly by initialising components
            // to 0.0 then adding `1.0 * w_linked` for the unknown
            // `linked`. We preserve that "don't prune too
            // aggressively" property here.
            let upper_bound = compose_upper_bound(&weights, title, tags, kind, directory);
            if upper_bound < opts.threshold {
                return None;
            }

            // Stage 2: linked Jaccard. Only meaningful when the
            // target has a graph id; otherwise the neighbour set is
            // not "empty", it's undefined.
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
            let similarity = compose(&weights, &components);
            (similarity >= opts.threshold).then_some(SimilarEntry {
                node: NodeRef::from_node(n),
                similarity,
                components,
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    entries.truncate(opts.limit);
    Ok(entries)
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
    let Some(id) = id else {
        return BTreeSet::new();
    };
    let mut set: BTreeSet<&str> = BTreeSet::new();
    for edge in graph.outgoing_edges(id) {
        if let ResolvedTarget::Resolved { id: target_id } = &edge.target {
            set.insert(target_id.as_str());
        }
    }
    for edge in graph.incoming_edges(id) {
        set.insert(edge.source.as_str());
    }
    set
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

/// Jaccard similarity that reports "no signal" honestly. Empty-vs-empty
/// is `None` (we have nothing to compare). One side empty against a
/// non-empty other side returns `Some(0.0)` — the non-empty side has
/// tokens that disagree with the empty side, which is a meaningful
/// zero-overlap signal, not absence.
fn jaccard<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> Option<f64> {
    if a.is_empty() && b.is_empty() {
        return None;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        None
    } else {
        Some(intersection as f64 / union as f64)
    }
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

fn compose(w: &SimilarityWeights, c: &SimilarityComponents) -> f64 {
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
        0.0
    } else {
        (weighted / weight_sum).clamp(0.0, 1.0)
    }
}

/// Upper bound on the final composite score using only the cheap
/// components. Each absent component is treated as the most
/// optimistic case (a future 1.0 contribution to both numerator and
/// denominator); each present component contributes its measured
/// value. The result is always `>= compose(...)`, so a candidate
/// pruned here can never have passed the threshold.
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
        }
    }

    fn graph_with(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![], vec![])
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
        .unwrap();
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
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
        // 'the' is dropped, 'auth' and 'payment' remain — different
        // tokens → title similarity = 0.
        let b_entry = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(b_entry.components.title, Some(0.0));
    }

    #[test]
    fn unrelated_docs_fall_below_threshold() {
        let g = graph_with(vec![
            node("a", "auth retry", "adr", vec![], "docs/a.md"),
            node(
                "b",
                "completely different topic",
                "guide",
                vec![],
                "other/b.md",
            ),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions::from_config(&cfg),
        )
        .unwrap();
        assert!(
            entries.is_empty(),
            "completely different doc must be filtered"
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
        let entries = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                threshold: 0.3,
                limit: 10,
            },
        )
        .unwrap();
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
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
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
        assert!(matches!(err, crate::error::Error::MissingNode(_)));
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
    fn jaccard_handles_empty_sets() {
        let empty: BTreeSet<&str> = BTreeSet::new();
        let one: BTreeSet<&str> = ["a"].into_iter().collect();
        // Empty-vs-empty must report "no signal", not "0.0".
        assert_eq!(jaccard::<&str>(&empty, &empty), None);
        // One side empty → no overlap → 0.0 (we do have a signal:
        // the non-empty side disagrees with the empty one).
        assert_eq!(jaccard(&one, &empty), Some(0.0));
        assert_eq!(jaccard(&one, &one), Some(1.0));
    }

    // ---- Honest absence regression tests --------------------------------

    /// Two docs whose titles tokenise to empty sets (only stop words /
    /// single chars) carry no title signal — composite must rely on
    /// kind/directory alone, not be dragged down by a fabricated 0.0.
    #[test]
    fn title_jaccard_absent_when_both_empty() {
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
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
        let b_entry = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(
            b_entry.components.title, None,
            "empty-vs-empty title tokens must report None, not 0.0"
        );
    }

    /// Two docs both with zero tags can't be compared on tags. The
    /// composite must exclude the tags weight from the denominator.
    #[test]
    fn tags_jaccard_absent_when_both_empty() {
        let g = graph_with(vec![
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
        let b_entry = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(
            b_entry.components.tags, None,
            "empty-vs-empty tag sets must report None, not 0.0"
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
        let entries = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
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
        let entries = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
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
        let entries = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
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
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
        let b = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(b.components.title, Some(1.0));
        assert_eq!(b.components.tags, None);
        assert_eq!(b.components.kind, Some(1.0));
        assert_eq!(b.components.directory, Some(1.0));
        assert_eq!(b.components.linked, None);
        assert!(
            (b.similarity - 1.0).abs() < 1e-9,
            "composite must be 1.0 when every present signal is 1.0; got {}",
            b.similarity
        );
    }

    /// `linked` is `None` when target IS in the graph but both target
    /// and candidate have zero neighbours (empty-vs-empty).
    #[test]
    fn linked_absent_when_both_neighbour_sets_empty() {
        let g = graph_with(vec![
            node("a", "auth retry policy", "adr", vec![], "docs/a.md"),
            node("b", "auth retry policy", "adr", vec![], "docs/b.md"),
        ]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
        let b = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(
            b.components.linked, None,
            "empty-vs-empty neighbour sets must report linked=None"
        );
    }

    /// `linked` IS present (and 1.0 / 0.0 / fractional) when target
    /// is a graph node and at least one side has neighbours. Sanity
    /// check that we haven't accidentally turned every linked into
    /// None.
    #[test]
    fn linked_present_when_either_side_has_neighbours() {
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
        let g = Graph::new(map, edges, vec![], vec![]);
        let cfg = Config::default();
        let entries = compute_similarity(
            &g,
            &cfg,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
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
        // a `SimilarityComponents` with every component absent must
        // serialize to an *empty* JSON object, not `{"title":null,...}`.
        // Build the absence via a pre-creation spec with no kind, no
        // tags, no `parent_dir`, and a stop-word-only title against a
        // candidate whose title also stops-out — this collapses every
        // component to `None` simultaneously.
        let g = graph_with(vec![node(
            "candidate",
            "a", // single ASCII char → tokenises to empty set
            "adr",
            vec![], // no tags
            "docs/candidate.md",
        )]);
        let cfg = Config::default();
        let target = SimilarityTarget::Spec {
            title: "a",
            kind: None,       // → kind component None
            tags: &[],        // empty + candidate empty → tags None
            parent_dir: None, // → directory component None
        };
        let entries = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                threshold: 0.0,
                limit: 10,
            },
        )
        .unwrap();
        let e = entries
            .iter()
            .find(|e| e.node.id == "candidate")
            .expect("candidate must appear at threshold 0.0");
        // Sanity: every component must indeed be None for the test
        // to exercise omission, not "serialised as 0.0".
        assert_eq!(e.components.title, None);
        assert_eq!(e.components.tags, None);
        assert_eq!(e.components.kind, None);
        assert_eq!(e.components.directory, None);
        assert_eq!(e.components.linked, None);

        let json = serde_json::to_value(&e.components).unwrap();
        let obj = json
            .as_object()
            .expect("components must serialize as object");
        for key in ["title", "tags", "kind", "directory", "linked"] {
            assert!(
                !obj.contains_key(key),
                "{key} must be omitted when absent; got {obj:?}"
            );
        }
    }
}
