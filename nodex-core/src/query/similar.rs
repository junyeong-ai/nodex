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

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{Config, SimilarityWeights};
use crate::error::Result;
use crate::model::{Graph, ResolvedTarget};

use super::NodeRef;

#[derive(Debug, Clone, Serialize)]
pub struct SimilarEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub similarity: f64,
    pub components: SimilarityComponents,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimilarityComponents {
    pub title: f64,
    pub tags: f64,
    pub kind: f64,
    pub directory: f64,
    pub linked: f64,
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
            let kind = if Some(n.kind.as_str()) == target_view.kind {
                1.0
            } else {
                0.0
            };
            let directory = if same_parent_directory(target_view.parent_dir, &n.path) {
                1.0
            } else {
                0.0
            };

            // Stage 1: cheap signals. If even a perfect linked match
            // (1.0 × w_linked) couldn't push the composite to the
            // threshold, skip the O(n) neighbour computation.
            let denom =
                weights.title + weights.tags + weights.kind + weights.directory + weights.linked;
            let cheap_numerator = title * weights.title
                + tags * weights.tags
                + kind * weights.kind
                + directory * weights.directory;
            let upper_bound = (cheap_numerator + 1.0 * weights.linked) / denom.max(f64::EPSILON);
            if upper_bound < opts.threshold {
                return None;
            }

            // Stage 2: linked Jaccard.
            let linked = jaccard(&target_neighbours, &neighbour_set(graph, Some(&n.id)));
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

fn jaccard<T: Ord>(a: &BTreeSet<T>, b: &BTreeSet<T>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn same_parent_directory(target_dir: Option<&Path>, node_path: &Path) -> bool {
    let Some(target_dir) = target_dir else {
        return false;
    };
    node_path.parent().is_some_and(|p| p == target_dir)
}

fn compose(w: &SimilarityWeights, c: &SimilarityComponents) -> f64 {
    let weighted = c.title * w.title
        + c.tags * w.tags
        + c.kind * w.kind
        + c.directory * w.directory
        + c.linked * w.linked;
    let weight_sum = w.title + w.tags + w.kind + w.directory + w.linked;
    if weight_sum <= 0.0 {
        0.0
    } else {
        (weighted / weight_sum).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Node, Status};
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
        }
    }

    fn graph_with(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![])
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
        assert!(entries[0].components.title > 0.5);
        assert_eq!(entries[0].components.kind, 1.0);
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
        assert_eq!(b_entry.components.title, 0.0);
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
        // bytes, 1 char) must also be dropped. The pre-fix byte-length
        // filter kept the latter, treating Latin and CJK inconsistently.
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
        assert_eq!(jaccard::<&str>(&empty, &empty), 0.0);
        assert_eq!(jaccard(&one, &empty), 0.0);
        assert_eq!(jaccard(&one, &one), 1.0);
    }
}
