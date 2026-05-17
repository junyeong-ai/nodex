//! Recency query — surface what has changed lately.
//!
//! AI agents use this to bootstrap "what happened since I was last
//! here" without scraping git log: nodes are filtered by their
//! frontmatter `created` / `updated` / `reviewed` dates and returned
//! newest-first.

use chrono::{Local, NaiveDate};
use serde::Serialize;

use crate::model::{Graph, Node};

use super::NodeRef;

/// Cut-off for "recent". The two constructors are mutually exclusive
/// at the type level so callers cannot pass both an absolute date and
/// a relative window.
#[derive(Debug, Clone, Copy)]
pub enum RecencySince {
    /// Last `N` days, anchored to today (inclusive).
    Days(u32),
    /// Absolute cut-off date (inclusive).
    Date(NaiveDate),
}

/// Which date field is consulted to decide whether a node is recent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecencyField {
    Created,
    Updated,
    Reviewed,
    /// Use whichever of `created` / `updated` / `reviewed` is newest.
    /// The default — agents typically want "what changed" rather than
    /// a specific lifecycle event.
    #[default]
    Any,
}

/// Default lookback when no explicit window is supplied. Centralised
/// here so the CLI flag's `default_value_t` and
/// [`RecencyOptions::default`] cannot drift apart.
pub const DEFAULT_SINCE_DAYS: u32 = 7;

/// Default cap on returned entries — same rationale as
/// [`DEFAULT_SINCE_DAYS`].
pub const DEFAULT_LIMIT: usize = 20;

#[derive(Debug, Clone)]
pub struct RecencyOptions {
    pub since: RecencySince,
    pub kind: Option<String>,
    pub field: RecencyField,
    pub limit: Option<usize>,
}

impl Default for RecencyOptions {
    fn default() -> Self {
        Self {
            since: RecencySince::Days(DEFAULT_SINCE_DAYS),
            kind: None,
            field: RecencyField::default(),
            limit: Some(DEFAULT_LIMIT),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RecentEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    /// Which date field actually matched the cut-off.
    pub field: RecencyField,
    pub date: NaiveDate,
    /// Whole days between `date` and today. Future-dated documents
    /// (clock skew, deliberate post-dating) clamp to 0 rather than
    /// surfacing a negative age callers would have to defensively
    /// handle.
    pub days_ago: u32,
}

/// Find nodes whose configured date field is on/after the cut-off.
/// Sorted newest-first with id as a stable tie-break.
pub fn find_recent(graph: &Graph, opts: &RecencyOptions) -> Vec<RecentEntry> {
    let today = Local::now().date_naive();
    let cutoff = match opts.since {
        RecencySince::Date(d) => d,
        RecencySince::Days(n) => match today.checked_sub_days(chrono::Days::new(u64::from(n))) {
            Some(d) => d,
            // Pathological window — chrono can't represent it. Treat
            // every doc as in-window (no cut-off filter).
            None => return collect_all(graph, opts, today),
        },
    };

    let mut entries: Vec<RecentEntry> = graph
        .nodes()
        .values()
        .filter(|node| match_kind(node, opts.kind.as_deref()))
        .filter_map(|node| {
            let (field, date) = pick_field(node, opts.field)?;
            (date >= cutoff).then(|| RecentEntry {
                node: NodeRef::from_node(node),
                field,
                date,
                days_ago: super::days_between_clamped(today, date),
            })
        })
        .collect();

    sort_and_truncate(&mut entries, opts.limit);
    entries
}

fn collect_all(graph: &Graph, opts: &RecencyOptions, today: NaiveDate) -> Vec<RecentEntry> {
    let mut entries: Vec<RecentEntry> = graph
        .nodes()
        .values()
        .filter(|node| match_kind(node, opts.kind.as_deref()))
        .filter_map(|node| {
            let (field, date) = pick_field(node, opts.field)?;
            Some(RecentEntry {
                node: NodeRef::from_node(node),
                field,
                date,
                days_ago: super::days_between_clamped(today, date),
            })
        })
        .collect();
    sort_and_truncate(&mut entries, opts.limit);
    entries
}

fn sort_and_truncate(entries: &mut Vec<RecentEntry>, limit: Option<usize>) {
    entries.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| a.node.id.cmp(&b.node.id)));
    if let Some(n) = limit {
        entries.truncate(n);
    }
}

fn match_kind(node: &Node, kind: Option<&str>) -> bool {
    kind.is_none_or(|k| node.kind.as_str() == k)
}

fn pick_field(node: &Node, field: RecencyField) -> Option<(RecencyField, NaiveDate)> {
    match field {
        RecencyField::Created => node.created.map(|d| (RecencyField::Created, d)),
        RecencyField::Updated => node.updated.map(|d| (RecencyField::Updated, d)),
        RecencyField::Reviewed => node.reviewed.map(|d| (RecencyField::Reviewed, d)),
        RecencyField::Any => newest_of(node),
    }
}

fn newest_of(node: &Node) -> Option<(RecencyField, NaiveDate)> {
    [
        (RecencyField::Updated, node.updated),
        (RecencyField::Reviewed, node.reviewed),
        (RecencyField::Created, node.created),
    ]
    .into_iter()
    .filter_map(|(f, d)| d.map(|d| (f, d)))
    .max_by_key(|(_, d)| *d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_node(
        id: &str,
        kind: &str,
        created: Option<NaiveDate>,
        updated: Option<NaiveDate>,
        reviewed: Option<NaiveDate>,
    ) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
            kind: Kind::new(kind),
            status: Status::new("active"),
            created,
            updated,
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
    fn cutoff_excludes_older_dates() {
        let today = Local::now().date_naive();
        let recent = today - chrono::Duration::days(2);
        let stale = today - chrono::Duration::days(30);
        let g = graph_with(vec![
            make_node("recent", "adr", None, Some(recent), None),
            make_node("stale", "adr", None, Some(stale), None),
        ]);
        let opts = RecencyOptions {
            since: RecencySince::Days(7),
            field: RecencyField::Updated,
            ..Default::default()
        };
        let entries = find_recent(&g, &opts);
        let ids: Vec<&str> = entries.iter().map(|e| e.node.id.as_str()).collect();
        assert_eq!(ids, vec!["recent"]);
    }

    #[test]
    fn any_field_picks_newest_date_per_node() {
        let today = Local::now().date_naive();
        let n = make_node(
            "x",
            "adr",
            Some(today - chrono::Duration::days(5)),
            Some(today - chrono::Duration::days(1)), // newest
            Some(today - chrono::Duration::days(3)),
        );
        let g = graph_with(vec![n]);
        let entries = find_recent(
            &g,
            &RecencyOptions {
                since: RecencySince::Days(7),
                field: RecencyField::Any,
                ..Default::default()
            },
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].field, RecencyField::Updated);
    }

    #[test]
    fn kind_filter_isolates_one_kind() {
        let today = Local::now().date_naive();
        let g = graph_with(vec![
            make_node("a", "adr", None, Some(today), None),
            make_node("g", "guide", None, Some(today), None),
        ]);
        let entries = find_recent(
            &g,
            &RecencyOptions {
                since: RecencySince::Days(7),
                kind: Some("adr".into()),
                field: RecencyField::Updated,
                ..Default::default()
            },
        );
        let ids: Vec<&str> = entries.iter().map(|e| e.node.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
    }

    #[test]
    fn newest_first_with_id_tie_break() {
        let today = Local::now().date_naive();
        let g = graph_with(vec![
            make_node(
                "zzz",
                "adr",
                None,
                Some(today - chrono::Duration::days(2)),
                None,
            ),
            make_node(
                "aaa",
                "adr",
                None,
                Some(today - chrono::Duration::days(1)),
                None,
            ),
            make_node(
                "mmm",
                "adr",
                None,
                Some(today - chrono::Duration::days(2)),
                None,
            ),
        ]);
        let entries = find_recent(
            &g,
            &RecencyOptions {
                since: RecencySince::Days(7),
                field: RecencyField::Updated,
                ..Default::default()
            },
        );
        let ids: Vec<&str> = entries.iter().map(|e| e.node.id.as_str()).collect();
        assert_eq!(ids, vec!["aaa", "mmm", "zzz"]);
    }

    #[test]
    fn limit_truncates_after_sort() {
        let today = Local::now().date_naive();
        let g = graph_with(
            (0..5)
                .map(|i| {
                    make_node(
                        &format!("n{i:02}"),
                        "adr",
                        None,
                        Some(today - chrono::Duration::days(i as i64)),
                        None,
                    )
                })
                .collect(),
        );
        let entries = find_recent(
            &g,
            &RecencyOptions {
                since: RecencySince::Days(30),
                field: RecencyField::Updated,
                limit: Some(2),
                ..Default::default()
            },
        );
        let ids: Vec<&str> = entries.iter().map(|e| e.node.id.as_str()).collect();
        assert_eq!(ids, vec!["n00", "n01"]);
    }

    #[test]
    fn nodes_without_matching_field_are_skipped() {
        let g = graph_with(vec![make_node("dateless", "adr", None, None, None)]);
        let entries = find_recent(
            &g,
            &RecencyOptions {
                since: RecencySince::Days(7),
                field: RecencyField::Updated,
                ..Default::default()
            },
        );
        assert!(entries.is_empty());
    }

    #[test]
    fn future_date_clamps_days_ago_to_zero() {
        // A document dated tomorrow (clock skew or post-dating) must
        // surface with `days_ago = 0`, never a negative count.
        let today = Local::now().date_naive();
        let g = graph_with(vec![make_node(
            "future",
            "adr",
            None,
            Some(today + chrono::Duration::days(2)),
            None,
        )]);
        let entries = find_recent(
            &g,
            &RecencyOptions {
                since: RecencySince::Days(7),
                field: RecencyField::Updated,
                ..Default::default()
            },
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].days_ago, 0);
    }

    #[test]
    fn absolute_date_cutoff() {
        let cutoff = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let g = graph_with(vec![
            make_node(
                "before",
                "adr",
                None,
                Some(cutoff - chrono::Duration::days(1)),
                None,
            ),
            make_node("on", "adr", None, Some(cutoff), None),
            make_node(
                "after",
                "adr",
                None,
                Some(cutoff + chrono::Duration::days(1)),
                None,
            ),
        ]);
        let entries = find_recent(
            &g,
            &RecencyOptions {
                since: RecencySince::Date(cutoff),
                field: RecencyField::Updated,
                ..Default::default()
            },
        );
        let ids: Vec<&str> = entries.iter().map(|e| e.node.id.as_str()).collect();
        // Inclusive of cutoff date, sorted desc.
        assert_eq!(ids, vec!["after", "on"]);
    }
}
