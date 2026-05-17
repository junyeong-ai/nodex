use crate::model::Graph;
use schemars::JsonSchema;

use super::NodeRef;

/// One ranked hit from [`search`]. Carries the standard `NodeRef`
/// flattened so the JSON shape stays `{ id, title, kind, status, path,
/// score }`, identical to every other item-list query.
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct SearchEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub score: f64,
}

/// Search nodes by keyword (case-insensitive substring match on title, id, tags).
pub fn search(graph: &Graph, keyword: &str, statuses: Option<&[String]>) -> Vec<SearchEntry> {
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

            let mut score = 0.0;

            // Exact id match
            if id_lower == kw {
                score += 3.0;
            } else if id_lower.contains(&kw) {
                score += 1.5;
            }

            // Title match
            if title_lower == kw {
                score += 2.5;
            } else if title_lower.contains(&kw) {
                score += 1.0;
            }

            // Tag match (element-by-element, no allocation)
            if node.tags.iter().any(|t| t.to_lowercase().contains(&kw)) {
                score += 0.5;
            }

            if score > 0.0 {
                Some(SearchEntry {
                    node: NodeRef::from_node(node),
                    score,
                })
            } else {
                None
            }
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
