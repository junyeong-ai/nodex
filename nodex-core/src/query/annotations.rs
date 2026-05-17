//! Group body annotations by `(pattern, key)`. Operates purely on the
//! pre-extracted [`crate::model::Annotation`] records that live on the
//! graph — no filesystem access at query time, no regex re-evaluation.

use serde::Serialize;

use crate::model::{Annotation, Graph};

/// Every marker for one `[[annotations]]` pattern, grouped by the
/// captured key. Patterns whose extraction yielded no entries are
/// omitted from the result entirely.
#[derive(Debug, Clone, Serialize)]
pub struct AnnotationGroup {
    pub name: String,
    pub entries: Vec<AnnotationEntry>,
}

/// One grouping key inside a pattern: how many times it was captured,
/// and where each occurrence lives.
#[derive(Debug, Clone, Serialize)]
pub struct AnnotationEntry {
    pub key: String,
    pub count: usize,
    pub sources: Vec<AnnotationSourceRef>,
}

/// One occurrence of a marker. `path` is the forward-slashed source
/// path so callers can render it without re-derivation.
#[derive(Debug, Clone, Serialize)]
pub struct AnnotationSourceRef {
    pub source_id: String,
    pub path: String,
    pub line: usize,
}

/// All annotations on the graph, grouped by pattern → key.
///
/// `pattern_filter` restricts the output to a single named pattern
/// (matching `[[annotations]].name`). An empty graph or a filter that
/// matches no pattern returns an empty `Vec` — never an error: the
/// query's contract is "tell me what's there", and "nothing" is a
/// valid answer that callers can act on directly.
///
/// Output ordering is deterministic: groups are sorted by `name`;
/// within a group, entries are sorted by `key`; within a marker,
/// sources are sorted by `(source_id, line)`.
pub fn find_annotations(graph: &Graph, pattern_filter: Option<&str>) -> Vec<AnnotationGroup> {
    let mut by_pattern: std::collections::BTreeMap<&str, Vec<&Annotation>> =
        std::collections::BTreeMap::new();
    for ann in graph.annotations() {
        if let Some(filter) = pattern_filter
            && ann.pattern_name != filter
        {
            continue;
        }
        by_pattern
            .entry(ann.pattern_name.as_str())
            .or_default()
            .push(ann);
    }

    by_pattern
        .into_iter()
        .map(|(name, anns)| build_group(graph, name, anns))
        .collect()
}

fn build_group(graph: &Graph, name: &str, mut anns: Vec<&Annotation>) -> AnnotationGroup {
    // Already sorted as a side-effect of the builder's canonical
    // ordering — but the slice we received is *partitioned* by pattern,
    // not necessarily sorted within. Re-sort by (key, source_id, line)
    // so the per-group view is independently deterministic.
    anns.sort_by(|a, b| {
        a.key
            .cmp(&b.key)
            .then_with(|| a.source_id.cmp(&b.source_id))
            .then_with(|| a.line.cmp(&b.line))
    });

    let mut entries: Vec<AnnotationEntry> = Vec::new();
    let mut cursor = 0;
    while cursor < anns.len() {
        let current_key = anns[cursor].key.as_str();
        let mut end = cursor;
        while end < anns.len() && anns[end].key == current_key {
            end += 1;
        }
        let group = &anns[cursor..end];
        entries.push(AnnotationEntry {
            key: current_key.to_string(),
            count: group.len(),
            sources: group
                .iter()
                .map(|a| AnnotationSourceRef {
                    source_id: a.source_id.clone(),
                    path: graph
                        .node(&a.source_id)
                        .map(|n| crate::path_guard::forward_string(&n.path))
                        .unwrap_or_default(),
                    line: a.line,
                })
                .collect(),
        });
        cursor = end;
    }

    AnnotationGroup {
        name: name.to_string(),
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Annotation, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
            kind: Kind::new("learning"),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
        }
    }

    fn graph(nodes: Vec<Node>, anns: Vec<Annotation>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], anns, vec![])
    }

    fn ann(source: &str, pattern: &str, key: &str, line: usize) -> Annotation {
        Annotation {
            source_id: source.into(),
            pattern_name: pattern.into(),
            key: key.into(),
            line,
        }
    }

    #[test]
    fn empty_graph_returns_empty() {
        let g = graph(vec![], vec![]);
        assert!(find_annotations(&g, None).is_empty());
    }

    #[test]
    fn groups_by_key_and_counts() {
        let g = graph(
            vec![node("a"), node("b")],
            vec![
                ann("a", "promotes", "spec-x", 5),
                ann("b", "promotes", "spec-x", 12),
                ann("a", "promotes", "spec-y", 9),
            ],
        );
        let groups = find_annotations(&g, None);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "promotes");
        assert_eq!(groups[0].entries.len(), 2);
        let spec_x = groups[0]
            .entries
            .iter()
            .find(|m| m.key == "spec-x")
            .unwrap();
        assert_eq!(spec_x.count, 2);
        assert_eq!(spec_x.sources.len(), 2);
    }

    #[test]
    fn pattern_filter_isolates_one_pattern() {
        let g = graph(
            vec![node("a")],
            vec![ann("a", "promotes", "x", 1), ann("a", "research", "y", 2)],
        );
        let only_promotes = find_annotations(&g, Some("promotes"));
        assert_eq!(only_promotes.len(), 1);
        assert_eq!(only_promotes[0].name, "promotes");
        let unknown = find_annotations(&g, Some("ghost"));
        assert!(unknown.is_empty());
    }

    #[test]
    fn sources_are_sorted_by_source_id_then_line() {
        let g = graph(
            vec![node("alpha"), node("beta")],
            vec![
                ann("beta", "promotes", "k", 4),
                ann("alpha", "promotes", "k", 9),
                ann("alpha", "promotes", "k", 3),
            ],
        );
        let groups = find_annotations(&g, None);
        let sources = &groups[0].entries[0].sources;
        // alpha (line 3) < alpha (line 9) < beta (line 4).
        assert_eq!(sources[0].source_id, "alpha");
        assert_eq!(sources[0].line, 3);
        assert_eq!(sources[1].source_id, "alpha");
        assert_eq!(sources[1].line, 9);
        assert_eq!(sources[2].source_id, "beta");
    }

    #[test]
    fn source_path_resolved_from_node() {
        let g = graph(vec![node("doc-1")], vec![ann("doc-1", "promotes", "k", 1)]);
        let groups = find_annotations(&g, None);
        assert_eq!(groups[0].entries[0].sources[0].path, "docs/doc-1.md");
    }
}
