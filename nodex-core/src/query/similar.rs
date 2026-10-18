//! Lexical and graph-overlap candidates for comparison by a reader.
//!
//! Kind, status and directory constrain the candidate set. Scores combine
//! title-token, tag and graph-neighbour Jaccard overlap; they do not establish
//! semantic equivalence. Only candidates with positive weighted overlap enter
//! the result. A signal absent from the target is omitted uniformly, while a
//! candidate lacking that signal measures zero.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeSet, BinaryHeap};
use std::path::PathBuf;

use crate::config::{Config, SimilarityWeights};
use crate::error::Result;
use crate::model::Graph;

use super::{NodeRef, RankingOutcome};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SimilarityEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    /// Weighted overlap in `(0, 1]`, comparable within this query.
    pub score: f64,
    pub components: SimilarityComponents,
}

/// Jaccard overlaps. Omitted fields have no target-side set to compare.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SimilarityComponents {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked: Option<f64>,
}

pub enum SimilarityTarget<'a> {
    Node(&'a str),
    Spec { title: &'a str, tags: &'a [String] },
}

#[derive(Debug, Clone)]
pub struct SimilarityOptions {
    pub limit: usize,
    pub kind: Option<String>,
    pub status: Option<String>,
    /// Exact project-relative parent directory; the CLI normalizes authored paths.
    pub parent_dir: Option<PathBuf>,
}

impl SimilarityOptions {
    pub fn from_config(config: &Config) -> Self {
        Self {
            limit: config.similarity.default_limit,
            kind: None,
            status: None,
            parent_dir: None,
        }
    }
}

/// Select positive-overlap candidates, descending by score and then by id.
/// Filters precede ranking, so excluded documents never consume a result slot.
pub fn compute_similarity(
    graph: &Graph,
    config: &Config,
    target: &SimilarityTarget<'_>,
    opts: &SimilarityOptions,
) -> Result<RankingOutcome<SimilarityEntry>> {
    let (title, tags, exclude_id) = match target {
        SimilarityTarget::Node(id) => {
            let node = graph.require_node(id)?;
            (node.title.as_str(), node.tags.as_slice(), Some(*id))
        }
        SimilarityTarget::Spec { title, tags } => (*title, *tags, None),
    };
    let stop_words = config
        .similarity
        .title_stop_words
        .iter()
        .map(String::as_str)
        .collect();
    let title_tokens = tokenize_title(title, &stop_words);
    let tag_set = tags.iter().map(|tag| tag.to_lowercase()).collect();
    let neighbours = neighbour_set(graph, exclude_id);
    let weights = &config.similarity.weights;
    let linked_bound = (!neighbours.is_empty()).then_some(1.0);
    let mut top: BinaryHeap<HeapEntry> = BinaryHeap::new();
    let mut unscored = 0;

    for node in graph.nodes().values().filter(|node| {
        exclude_id.is_none_or(|id| node.id != id)
            && opts
                .kind
                .as_deref()
                .is_none_or(|kind| node.kind.as_str() == kind)
            && opts
                .status
                .as_deref()
                .is_none_or(|status| node.status.as_str() == status)
            && opts
                .parent_dir
                .as_deref()
                .is_none_or(|dir| node.path.parent() == Some(dir))
    }) {
        let title = jaccard(&title_tokens, &tokenize_title(&node.title, &stop_words));
        let tags = jaccard(
            &tag_set,
            &node.tags.iter().map(|tag| tag.to_lowercase()).collect(),
        );
        // Only graph overlap remains unmeasured. Assuming full overlap gives
        // an upper bound with the same target-side denominator as the score.
        let upper = compose(
            weights,
            &SimilarityComponents {
                title,
                tags,
                linked: linked_bound,
            },
        );
        let Some(upper) = upper else {
            unscored += 1;
            continue;
        };
        if opts.limit == 0 || upper <= 0.0 {
            continue;
        }
        if top.len() == opts.limit && upper < top.peek().expect("non-empty heap").0.score {
            continue;
        }
        let components = SimilarityComponents {
            title,
            tags,
            linked: jaccard(&neighbours, &neighbour_set(graph, Some(&node.id))),
        };
        let score = compose(weights, &components).expect("target-side signals are unchanged");
        if score <= 0.0 {
            continue;
        }
        top.push(HeapEntry(SimilarityEntry {
            node: NodeRef::from_node(node),
            score,
            components,
        }));
        if top.len() > opts.limit {
            top.pop();
        }
    }
    let mut entries: Vec<_> = top.into_iter().map(|entry| entry.0).collect();
    entries.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    Ok(RankingOutcome { entries, unscored })
}

struct HeapEntry(SimilarityEntry);

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
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
        other
            .0
            .score
            .total_cmp(&self.0.score)
            .then_with(|| self.0.node.id.cmp(&other.0.node.id))
    }
}

fn neighbour_set<'a>(graph: &'a Graph, id: Option<&str>) -> BTreeSet<&'a str> {
    id.map(|id| {
        super::structure::adjacent_undirected(graph, id, None)
            .into_iter()
            .collect()
    })
    .unwrap_or_default()
}

fn tokenize_title(title: &str, stop_words: &BTreeSet<&str>) -> BTreeSet<String> {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty() && !stop_words.contains(*token))
        .map(String::from)
        .collect()
}

fn jaccard<T: Ord>(target: &BTreeSet<T>, candidate: &BTreeSet<T>) -> Option<f64> {
    if target.is_empty() {
        None
    } else {
        Some(target.intersection(candidate).count() as f64 / target.union(candidate).count() as f64)
    }
}

fn compose(weights: &SimilarityWeights, components: &SimilarityComponents) -> Option<f64> {
    let mut weighted = 0.0;
    let mut sum = 0.0;
    for (value, weight) in [
        (components.title, weights.title),
        (components.tags, weights.tags),
        (components.linked, weights.linked),
    ] {
        if let Some(value) = value {
            weighted += value * weight;
            sum += weight;
        }
    }
    (sum > 0.0).then(|| (weighted / sum).clamp(0.0, 1.0))
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
            frontmatter_relations: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            body_structure: Default::default(),
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
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
        )
        .unwrap()
        .entries;
        assert!(entries.is_empty());
    }

    #[test]
    fn unrelated_documents_are_excluded() {
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
        assert_eq!(ids, ["close"]);
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
            tags: &["auth".to_string()],
        };
        let entries = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
        )
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
            &SimilarityOptions {
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
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
    fn title_tokens_preserve_short_latin_and_korean_terms() {
        let tokens = tokenize_title("R 가 ab 가나", &BTreeSet::new());
        assert_eq!(
            tokens,
            ["r", "가", "ab", "가나"]
                .map(String::from)
                .into_iter()
                .collect()
        );
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

    #[test]
    fn an_absent_title_does_not_dilute_matching_tags() {
        let graph = graph_with(vec![
            node("a", "the", "adr", vec!["auth"], "docs/a.md"),
            node("b", "and", "guide", vec!["auth"], "other/b.md"),
        ]);
        let config = Config::default();
        let result = compute_similarity(
            &graph,
            &config,
            &SimilarityTarget::Node("a"),
            &SimilarityOptions::from_config(&config),
        )
        .unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].components.title, None);
        assert_eq!(result.entries[0].components.tags, Some(1.0));
        assert_eq!(result.entries[0].score, 1.0);
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
            compute_similarity(
                &g,
                &cfg,
                &target,
                &SimilarityOptions {
                    limit: 10,
                    status: None,
                    kind: None,
                    parent_dir: None,
                },
            )
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
            tags: &[],
        };
        let entries = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
        )
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
        // Every measured overlap is identical.
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
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
        )
        .unwrap()
        .entries;
        let b = entries.iter().find(|e| e.node.id == "b").unwrap();
        assert_eq!(b.components.title, Some(1.0));
        assert_eq!(b.components.tags, None);
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
            &SimilarityOptions {
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
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
                reference: None,
                source: "a".to_string(),
                target: ResolvedTarget::Resolved {
                    id: "c".to_string(),
                },
                relation: "related".to_string(),
                location: "frontmatter:related".to_string(),
            },
            Edge {
                reference: None,
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
            &SimilarityOptions {
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
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

    #[test]
    fn json_omits_absent_similarity_components() {
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
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
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
        assert!(obj.contains_key("title"), "{obj:?}");
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
            title: "a", // single ASCII char → tokenises empty → title None
            tags: &[],  // → tags None
        };
        let outcome = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
        )
        .unwrap();
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
                tags: &[],
            },
            &SimilarityOptions {
                limit: 1,
                status: None,
                kind: None,
                parent_dir: None,
            },
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
                tags: &[],
            },
            &SimilarityOptions {
                limit: 1,
                status: None,
                kind: None,
                parent_dir: None,
            },
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
                tags: &[],
            },
            &SimilarityOptions {
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
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
            tags: &[],
        };
        let entries = compute_similarity(
            &g,
            &cfg,
            &target,
            &SimilarityOptions {
                limit: 2,
                status: None,
                kind: None,
                parent_dir: None,
            },
        )
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
            &SimilarityOptions {
                limit: 0,
                status: None,
                kind: None,
                parent_dir: None,
            },
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
            &SimilarityOptions {
                limit: 10,
                status: None,
                kind: None,
                parent_dir: None,
            },
        )
        .unwrap()
        .entries;
        assert!(
            entries.is_empty(),
            "single-node graph with target excluded must return empty; got {entries:?}",
        );
    }
    #[test]
    fn bounded_ranking_matches_complete_ranking_across_overlap_signals() {
        use crate::model::{Edge, ResolvedTarget};
        let nodes = (0..32)
            .map(|index| {
                node(
                    &format!("n-{index:02}"),
                    &format!(
                        "{} {}",
                        if index % 2 == 0 { "policy" } else { "ledger" },
                        index % 3
                    ),
                    "adr",
                    if index % 4 == 0 { vec!["auth"] } else { vec![] },
                    &format!("docs/{index}.md"),
                )
            })
            .collect();
        let mut graph = graph_with(nodes);
        let edges = (0..24)
            .map(|index| Edge {
                reference: None,
                source: format!("n-{index:02}"),
                target: ResolvedTarget::resolved(format!("n-{:02}", 24 + index % 4)),
                relation: "references".into(),
                location: "L1".into(),
            })
            .collect();
        graph = Graph::new(
            graph.nodes().clone(),
            edges,
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );
        for weights in [
            SimilarityWeights::default(),
            SimilarityWeights {
                title: 0.0,
                tags: 1.0,
                linked: 0.0,
            },
            SimilarityWeights {
                title: 0.0,
                tags: 0.0,
                linked: 1.0,
            },
        ] {
            let mut config = Config::default();
            config.similarity.weights = weights;
            let full = compute_similarity(
                &graph,
                &config,
                &SimilarityTarget::Node("n-00"),
                &SimilarityOptions {
                    limit: usize::MAX,
                    ..SimilarityOptions::from_config(&config)
                },
            )
            .unwrap();
            for limit in [1, 2, 7] {
                let bounded = compute_similarity(
                    &graph,
                    &config,
                    &SimilarityTarget::Node("n-00"),
                    &SimilarityOptions {
                        limit,
                        ..SimilarityOptions::from_config(&config)
                    },
                )
                .unwrap();
                let ids = |entries: &[SimilarityEntry]| {
                    entries
                        .iter()
                        .map(|entry| (entry.node.id.clone(), entry.score))
                        .collect::<Vec<_>>()
                };
                assert_eq!(
                    ids(&bounded.entries),
                    ids(&full.entries[..full.entries.len().min(limit)])
                );
                assert_eq!(bounded.unscored, full.unscored);
            }
        }
    }
}
