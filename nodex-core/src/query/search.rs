use crate::model::Graph;

/// Search result with relevance score.
#[derive(Debug, serde::Serialize)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub path: String,
    pub score: f64,
}

/// Search nodes by keyword (case-insensitive substring match on title, id, tags).
pub fn search(graph: &Graph, keyword: &str, statuses: Option<&[String]>) -> Vec<SearchResult> {
    let kw = keyword.to_lowercase();
    let mut results: Vec<SearchResult> = graph
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
                Some(SearchResult {
                    id: node.id.clone(),
                    title: node.title.clone(),
                    kind: node.kind.to_string(),
                    status: node.status.to_string(),
                    path: node.path.to_string_lossy().to_string(),
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
            .then_with(|| a.id.cmp(&b.id))
    });

    results
}

/// Search nodes by tags.
pub fn search_by_tags(
    graph: &Graph,
    tags: &[String],
    match_all: bool,
    statuses: Option<&[String]>,
) -> Vec<SearchResult> {
    let needle: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();

    let mut results: Vec<SearchResult> = graph
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
        .filter(|node| {
            let node_tags: Vec<String> = node.tags.iter().map(|t| t.to_lowercase()).collect();
            if match_all {
                needle.iter().all(|n| node_tags.contains(n))
            } else {
                needle.iter().any(|n| node_tags.contains(n))
            }
        })
        .map(|node| SearchResult {
            id: node.id.clone(),
            title: node.title.clone(),
            kind: node.kind.to_string(),
            status: node.status.to_string(),
            path: node.path.to_string_lossy().to_string(),
            score: 1.0,
        })
        .collect();

    results.sort_by(|a, b| a.id.cmp(&b.id));
    results
}
