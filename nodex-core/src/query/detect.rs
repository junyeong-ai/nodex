use chrono::NaiveDate;
use schemars::JsonSchema;

use crate::config::Config;
use crate::model::Graph;

use super::{DetectionOutcome, NodeRef};

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct OrphanEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub created: Option<NaiveDate>,
}

/// Find the documents nothing references, as of `today`.
///
/// The population is the live documents the exemptions leave: a kind in
/// `detection.orphan_ok_kinds`, a per-node `orphan_ok`, and a document
/// younger than `detection.orphan_grace_days` are each leaf-by-design
/// or not yet expected to be linked, so none of them is a document this
/// asks anything of.
///
/// Neither is a retired one. Every remedy for orphanhood is a
/// maintenance action — link the document, declare it leaf, widen the
/// exempt kinds — and a terminal document is one the project has
/// stopped maintaining, often literally: a `body_immutable` block
/// freezes it on terminal entry. The detection plane reads terminal
/// status the same way throughout — `stale_review` does not review what
/// the project retired, `git_drift` does not measure it against source,
/// and the trust composite places neither review-anchored component on
/// it — and orphan reading it differently is what made a project spend
/// a whole `orphan_ok_kinds` entry to say so.
///
/// `orphan_grace_days` is a user-supplied `u32`; the cutoff is
/// subtracted through the checked API, and a horizon no document can be
/// placed against guards nothing — the reading `find_stale` gives its
/// own.
pub fn find_orphans(
    graph: &Graph,
    config: &Config,
    today: NaiveDate,
) -> DetectionOutcome<OrphanEntry> {
    let Some(grace_cutoff) = today.checked_sub_days(chrono::Days::new(u64::from(
        config.detection.orphan_grace_days,
    ))) else {
        return DetectionOutcome::inert();
    };

    let mut subjects = 0;
    let mut entries: Vec<OrphanEntry> = graph
        .nodes()
        .values()
        .filter(|node| !config.is_terminal(node.status.as_str()))
        .filter(|node| !config.is_orphan_ok_kind(node.kind.as_str()))
        .filter(|node| !node.orphan_ok)
        .filter(|node| !node.created.is_some_and(|created| created > grace_cutoff))
        .filter_map(|node| {
            subjects += 1;
            // A self-reference (a→a) is not "attention from outside",
            // so a doc whose only incoming edge is its own does not
            // escape orphan classification. Honest-graph queries
            // (`query node`, `query backlinks` from another node)
            // still see the self-edge; only this isolation metric
            // filters it out.
            graph
                .external_incoming_edges(&node.id)
                .is_empty()
                .then(|| OrphanEntry {
                    node: NodeRef::from_node(node),
                    created: node.created,
                })
        })
        .collect();

    entries.sort_by(|a, b| a.node.id.cmp(&b.node.id));
    DetectionOutcome { entries, subjects }
}

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct StaleEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub reviewed: NaiveDate,
    /// Whole days between the `reviewed` date and today. Mirrors
    /// [`crate::query::recent::RecentEntry::days_ago`] — same
    /// invariant (always ≥ 0), same type, same clamp.
    pub days_since: u32,
}

/// Find the documents past the staleness horizon, as of `today`.
///
/// The population is the *reviewable* documents — live and carrying a
/// `reviewed` date — because a horizon places a review date, and a
/// document with none is not one this asks anything of. Whether a
/// reviewable document turned out stale is the finding.
///
/// Guards nothing when stale detection is disabled (`stale_days` is
/// None) or the horizon underflows the representable range: both are a
/// horizon no document can be placed against.
pub fn find_stale(
    graph: &Graph,
    config: &Config,
    today: NaiveDate,
) -> DetectionOutcome<StaleEntry> {
    let Some(stale_days) = config.detection.stale_days else {
        return DetectionOutcome::inert();
    };
    let Some(cutoff) = today.checked_sub_days(chrono::Days::new(u64::from(stale_days))) else {
        return DetectionOutcome::inert();
    };

    let mut subjects = 0;
    let mut entries: Vec<StaleEntry> = graph
        .nodes()
        .values()
        .filter(|node| !config.is_terminal(node.status.as_str()))
        .filter_map(|node| {
            let reviewed = node.reviewed?;
            subjects += 1;
            // `stale_days = n` flags docs not reviewed for n+ days
            // (elapsed >= n), i.e. reviewed on/before the cutoff.
            if reviewed > cutoff {
                return None;
            }
            Some(StaleEntry {
                node: NodeRef::from_node(node),
                reviewed,
                days_since: super::days_between_clamped(today, reviewed),
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        a.reviewed
            .cmp(&b.reviewed)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    DetectionOutcome { entries, subjects }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Kind, Node, ResolvedTarget, Status};
    use chrono::Duration;
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str, kind: &str, status: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
            kind: Kind::new(kind),
            status: Status::new(status),
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

    fn horizon(days: u32) -> Config {
        let mut config = Config::default();
        config.detection.stale_days = Some(days);
        config
    }

    /// The reach is the population the horizon guards, never the
    /// offending subset: a document reviewed yesterday is as much a
    /// subject as one reviewed a year ago. Read the other way, a corpus
    /// where nothing is stale and a corpus where nothing is reviewable
    /// report the same empty list.
    #[test]
    fn reviewable_counts_the_population_not_the_findings() {
        let today = crate::test_today();
        let config = horizon(180);
        let g = graph_with(
            vec![
                Node {
                    reviewed: Some(today - Duration::days(300)),
                    ..node("past", "generic", "active")
                },
                Node {
                    reviewed: Some(today - Duration::days(1)),
                    ..node("fresh", "generic", "active")
                },
                node("undated", "generic", "active"),
                Node {
                    reviewed: Some(today - Duration::days(300)),
                    ..node("retired", "generic", "archived")
                },
            ],
            vec![],
        );
        let outcome = find_stale(&g, &config, today);
        let ids: Vec<&str> = outcome.entries.iter().map(|e| e.node.id.as_str()).collect();
        assert_eq!(ids, vec!["past"], "only the document past the horizon");
        assert_eq!(
            outcome.subjects, 2,
            "the fresh document is guarded; the undated and retired ones are not"
        );
    }

    /// A horizon the project never declared, and one that underflows the
    /// representable range, are the same state: no scale exists to place
    /// a review date on, so the predicate guards nothing rather than
    /// reporting a clean corpus.
    #[test]
    fn an_unplaceable_horizon_guards_nothing() {
        let today = crate::test_today();
        let g = graph_with(
            vec![Node {
                reviewed: Some(today - Duration::days(300)),
                ..node("past", "generic", "active")
            }],
            vec![],
        );
        for config in [Config::default(), horizon(u32::MAX)] {
            let outcome = find_stale(&g, &config, today);
            assert!(outcome.entries.is_empty());
            assert_eq!(
                outcome.subjects, 0,
                "a horizon nothing can be placed on guards nothing"
            );
        }
    }

    /// `stale_days = n` flags documents not reviewed for n+ days, so the
    /// cutoff day itself is stale and the day after it is not.
    #[test]
    fn the_horizon_boundary_is_inclusive() {
        let today = crate::test_today();
        let config = horizon(180);
        let g = graph_with(
            vec![
                Node {
                    reviewed: Some(today - Duration::days(180)),
                    ..node("on-the-day", "generic", "active")
                },
                Node {
                    reviewed: Some(today - Duration::days(179)),
                    ..node("day-inside", "generic", "active")
                },
            ],
            vec![],
        );
        let outcome = find_stale(&g, &config, today);
        let ids: Vec<&str> = outcome.entries.iter().map(|e| e.node.id.as_str()).collect();
        assert_eq!(ids, vec!["on-the-day"]);
        assert_eq!(outcome.subjects, 2);
    }

    /// A retired document is outside the population whatever references
    /// it has. Every remedy for orphanhood is a maintenance action, and
    /// the project has stopped maintaining it — the reading
    /// `stale_review` and `git_drift` already take of terminal status.
    #[test]
    fn a_retired_document_is_not_asked_for_references() {
        let today = crate::test_today();
        let config = Config::default();
        let g = graph_with(
            vec![
                node("live", "generic", "active"),
                node("retired", "generic", "archived"),
            ],
            vec![],
        );
        let outcome = find_orphans(&g, &config, today);
        let ids: Vec<&str> = outcome.entries.iter().map(|o| o.node.id.as_str()).collect();
        assert_eq!(ids, vec!["live"]);
        assert_eq!(
            outcome.subjects, 1,
            "the retired document is not a subject it could pass or fail"
        );
    }

    /// The three declared exemptions and the self-edge filter, each on
    /// its own document so a single over-broad exemption cannot hide
    /// behind another. `orphan_ok_kinds` and per-node `orphan_ok` form
    /// an OR; grace exempts by age; a document citing itself is not
    /// attention from outside. Terminal status, the fourth, has its own
    /// case above.
    #[test]
    fn orphan_exemptions_are_independent() {
        let today = crate::test_today();
        let mut config = Config::default();
        config.detection.orphan_ok_kinds = vec!["guide".into()];
        config.detection.orphan_grace_days = 14;
        let self_edge = Edge {
            source: "self-citing".into(),
            target: ResolvedTarget::resolved("self-citing"),
            relation: "references".into(),
            location: "L1".into(),
        };
        let g = graph_with(
            vec![
                node("bare", "generic", "active"),
                node("by-kind", "guide", "active"),
                Node {
                    orphan_ok: true,
                    ..node("by-flag", "generic", "active")
                },
                Node {
                    created: Some(today - Duration::days(3)),
                    ..node("in-grace", "generic", "active")
                },
                Node {
                    created: Some(today - Duration::days(30)),
                    ..node("past-grace", "generic", "active")
                },
                node("self-citing", "generic", "active"),
            ],
            vec![self_edge],
        );
        let outcome = find_orphans(&g, &config, today);
        let ids: Vec<String> = outcome.entries.into_iter().map(|o| o.node.id).collect();
        assert_eq!(
            ids,
            vec!["bare", "past-grace", "self-citing"],
            "each exemption removes exactly its own document"
        );
    }
}
