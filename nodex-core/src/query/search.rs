use schemars::JsonSchema;
use serde::Serialize;

use crate::config::SearchWeights;
use crate::model::Graph;

use super::NodeRef;

/// One ranked hit from [`search`]. Carries the standard `NodeRef`
/// flattened so the JSON shape stays `{ id, title, kind, status, path,
/// score, components }`, identical in spine to every other item-list
/// query. `score` is the sum of the matched fields' weights; `components`
/// breaks that sum down per field so a consumer can see *why* a node
/// ranked — the same "surface the why" contract `trust` and `similar`
/// expose.
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct SearchEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub score: f64,
    pub components: SearchComponents,
}

/// Per-field keyword-match breakdown. Each field is `Option<f64>`
/// carrying the weight that field contributed, or `None` when the
/// keyword did not match it — the honest-absence discipline `trust` and
/// `similar` follow (a non-matching field is *absent*, never a
/// fabricated `0.0`). An entry reaches the wire only when at least one
/// component is `Some` (search ranks matches, not the whole corpus), so
/// the breakdown is never all-`None`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchComponents {
    /// Weight contributed by the id match — `id_exact` when the id
    /// equals the keyword, else `id_partial` for a substring match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<f64>,
    /// Weight contributed by the title match — `title_exact` for an
    /// equal title, else `title_partial` for a substring match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<f64>,
    /// `tag` weight when any tag contains the keyword as a substring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<f64>,
}

/// Search nodes by keyword (case-insensitive substring match on title,
/// id, tags). Additive ranking: a node's score is the sum of the
/// weights of the fields its keyword matched (`search.weights`); a node
/// that matches nothing is excluded. This is deliberately *not* the
/// renormalise-over-present model `trust` / `similar` use — those rank
/// the whole corpus, search ranks only the matches.
pub fn search(
    graph: &Graph,
    weights: &SearchWeights,
    keyword: &str,
    statuses: Option<&[String]>,
) -> Vec<SearchEntry> {
    let kw = keyword.to_lowercase();
    let mut results: Vec<SearchEntry> = graph
        .nodes()
        .values()
        .filter(|node| {
            if let Some(statuses) = statuses
                && !statuses.is_empty()
                && !statuses.iter().any(|s| s == node.status.as_str())
            {
                return false;
            }
            true
        })
        .filter_map(|node| {
            let id_lower = node.id.to_lowercase();
            let title_lower = node.title.to_lowercase();

            // Exact tier wins over partial for the same field (an exact
            // match is also a substring, but the stronger signal is the
            // one that should count) — the per-field weight that fired
            // is the component, `None` when the field did not match.
            let id = if id_lower == kw {
                Some(weights.id_exact)
            } else if id_lower.contains(&kw) {
                Some(weights.id_partial)
            } else {
                None
            };
            let title = if title_lower == kw {
                Some(weights.title_exact)
            } else if title_lower.contains(&kw) {
                Some(weights.title_partial)
            } else {
                None
            };
            let tags = node
                .tags
                .iter()
                .any(|t| t.to_lowercase().contains(&kw))
                .then_some(weights.tag);

            // The explicit match gate: a node with no matching field is
            // excluded. Without it, search would rank the whole corpus
            // at score 0.0 — the opposite of a keyword search.
            let score = id.unwrap_or(0.0) + title.unwrap_or(0.0) + tags.unwrap_or(0.0);
            if id.is_none() && title.is_none() && tags.is_none() {
                return None;
            }

            Some(SearchEntry {
                node: NodeRef::from_node(node),
                score,
                components: SearchComponents { id, title, tags },
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Graph, GraphMeta, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str, title: &str, tags: &[&str]) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("{id}.md")),
            title: title.to_string(),
            kind: Kind::new("generic"),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            superseded_by: None,
            supersedes: vec![],
            implements: vec![],
            related: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
            body_hash: String::new(),
            content_hash: String::new(),
            body_lines_hash: vec![],
        }
    }

    fn graph(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![], vec![], vec![], GraphMeta::default())
    }

    #[test]
    fn score_is_the_additive_sum_of_matched_field_weights() {
        let w = SearchWeights::default();
        // id partial (1.5) + title partial (1.0) + tag (0.5) = 3.0.
        let g = graph(vec![node("auth-guide", "Auth Guide", &["auth"])]);
        let hits = search(&g, &w, "auth", None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].score, w.id_partial + w.title_partial + w.tag);
        assert_eq!(hits[0].components.id, Some(w.id_partial));
        assert_eq!(hits[0].components.title, Some(w.title_partial));
        assert_eq!(hits[0].components.tags, Some(w.tag));
    }

    #[test]
    fn exact_tier_outranks_partial_for_the_same_field() {
        let w = SearchWeights::default();
        let g = graph(vec![node("auth", "x", &[]), node("auth-extra", "y", &[])]);
        let hits = search(&g, &w, "auth", None);
        // Exact id ("auth") scores id_exact; the substring id scores
        // id_partial — exact ranks first.
        assert_eq!(hits[0].node.id, "auth");
        assert_eq!(hits[0].components.id, Some(w.id_exact));
        assert_eq!(hits[1].components.id, Some(w.id_partial));
    }

    #[test]
    fn non_matching_fields_are_absent_not_zero() {
        let w = SearchWeights::default();
        // Keyword in the title only — id and tags must be `None`, the
        // honest-absence discipline (never a fabricated 0.0).
        let g = graph(vec![node("doc-1", "Authentication", &["unrelated"])]);
        let hits = search(&g, &w, "auth", None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].components.id, None);
        assert_eq!(hits[0].components.title, Some(w.title_partial));
        assert_eq!(hits[0].components.tags, None);
    }

    #[test]
    fn a_match_on_a_zero_weight_field_is_still_returned() {
        // Presence, not score magnitude, gates inclusion: a config that
        // zeroes `tag` (legal as long as the weight set sums positive)
        // still surfaces a tag-only match — at score 0.0 — rather than
        // silently dropping it. Gating on `score > 0.0` would lose it.
        let w = SearchWeights {
            tag: 0.0,
            ..SearchWeights::default()
        };
        let g = graph(vec![node("doc-1", "unrelated", &["auth"])]);
        let hits = search(&g, &w, "auth", None);
        assert_eq!(
            hits.len(),
            1,
            "tag-only match must survive a 0.0 tag weight"
        );
        assert_eq!(hits[0].score, 0.0);
        assert_eq!(hits[0].components.tags, Some(0.0));
    }

    #[test]
    fn a_node_matching_nothing_is_excluded() {
        let w = SearchWeights::default();
        let g = graph(vec![node("doc-1", "unrelated", &["misc"])]);
        assert!(search(&g, &w, "auth", None).is_empty());
    }
}
