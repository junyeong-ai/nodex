//! "Nothing points here" detector.
//!
//! A document no other document references is unreachable by navigation
//! and by traversal — the graph carries it without the graph leading to
//! it. Four things say a document is not expected to be reached:
//! terminal status, because every remedy here is a maintenance action
//! and the project has stopped maintaining it; `detection.orphan_ok_kinds`
//! for kinds that are leaf-by-design; the per-node `orphan_ok` flag for
//! the heterogeneous ones; and `detection.orphan_grace_days` for
//! documents too new to have been linked yet. What they leave is the
//! population this guards.
//!
//! Orphanhood is decided entirely by *other* documents' edges, so the
//! edit that creates one touches the neighbour and never the document
//! itself. That is what the rule tells a diff ([`Rule::touched_by`]): a
//! finding here is the diff's when the document's own record moved *or*
//! an edge to it was added or removed — the second is how a document is
//! orphaned by an edit elsewhere, and reading only the first would drop
//! the finding exactly when it is new. What the diff did not reach stays
//! out of a narrowed report, so a corpus of standing orphans is not
//! re-reported on every pull request.
//!
//! Always registered: there is no threshold to omit, and the exemptions
//! narrow the population rather than switching the rule off. A project
//! that expects nothing to be referenced says so by naming its kinds,
//! and the reach then reports zero — the one reading an empty findings
//! list cannot give on its own.

use serde_json::{Map, Value, json};

use super::{Rule, RuleContext, RuleRun, Severity, SubjectUnit, Violation, ViolationDetails};
use crate::diff::Touched;

pub struct OrphanRule;

impl Rule for OrphanRule {
    fn id(&self) -> &str {
        "orphan"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn description(&self) -> &str {
        "Live documents no other document references, outside `detection.orphan_ok_kinds`, \
         the per-node `orphan_ok` flag, and `detection.orphan_grace_days`"
    }

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "orphan_grace_days".into(),
            json!(config.detection.orphan_grace_days),
        );
        m.insert(
            "orphan_ok_kinds".into(),
            json!(config.detection.orphan_ok_kinds),
        );
        m
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    /// A diff answers for an orphan finding when it reached the document
    /// either way: its own record moved, or who points at it did.
    fn touched_by(&self, _ctx: &RuleContext<'_>, since: &Touched, violation: &Violation) -> bool {
        violation
            .node_id
            .as_deref()
            .is_none_or(|id| since.document(id) || since.relinked(id))
    }

    /// The predicate lives in [`crate::query::detect::find_orphans`],
    /// which `query orphans`, `query issues` and the `GRAPH.md` report
    /// read too — one definition of "nothing points here", so the gate
    /// and the listings cannot describe different corpora. The rule
    /// supplies what a rule adds: the severity, the message, and the
    /// exemptions as parameters. The reach is the population that same
    /// pass counted.
    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let outcome = crate::query::detect::find_orphans(ctx.graph, ctx.config, ctx.today);
        let violations = outcome
            .entries
            .into_iter()
            .map(|entry| {
                Violation::new(
                    self.id(),
                    self.severity(),
                    Some(entry.node.id),
                    Some(entry.node.path),
                    ViolationDetails::Orphan,
                )
            })
            .collect();
        RuleRun::new(outcome.subjects, violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::{Edge, Graph, Kind, Node, ResolvedTarget, Status};
    use chrono::Duration;
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn doc(id: &str, kind: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
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

    fn run(graph: &Graph, config: &Config, today: chrono::NaiveDate) -> RuleRun {
        OrphanRule.check(&RuleContext {
            today,
            graph,
            config,
            files: crate::builder::scanner::ProjectFiles::working_tree(Path::new(".")),
            repository: None,
            since: None,
        })
    }

    fn subject(violation: &Violation) -> &str {
        violation
            .node_id
            .as_deref()
            .expect("an orphan finding is about the document it names")
    }

    fn graph_of(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
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

    /// The gate and the listings answer for one predicate, and this is
    /// the assertion that keeps them one: a rule that restated the
    /// predicate could drift from `query orphans` and the `GRAPH.md`
    /// report an edit at a time, with the reach counted by one reading
    /// and the findings produced by another.
    #[test]
    fn the_rule_reports_exactly_what_the_listing_finds() {
        let today = crate::test_today();
        let mut config = Config::default();
        config.detection.orphan_ok_kinds = vec!["guide".into()];
        let graph = graph_of(
            vec![
                doc("linked", "generic"),
                doc("bare", "generic"),
                doc("by-kind", "guide"),
                doc("linker", "generic"),
            ],
            vec![Edge {
                source: "linker".into(),
                target: ResolvedTarget::resolved("linked"),
                relation: "references".into(),
                location: "L1".into(),
            }],
        );

        let listing = crate::query::detect::find_orphans(&graph, &config, today);
        let run = run(&graph, &config, today);

        assert_eq!(
            run.subjects, listing.subjects,
            "the reach is the population that one pass counted"
        );
        let flagged: Vec<&str> = run.violations.iter().map(subject).collect();
        let listed: Vec<&str> = listing.entries.iter().map(|e| e.node.id.as_str()).collect();
        assert_eq!(flagged, listed, "the findings are the same documents");
        assert_eq!(
            listed,
            vec!["bare", "linker"],
            "the exempt kind is outside the population; the linked one passes"
        );
        assert_eq!(run.subjects, 3, "one of four documents is exempt by kind");
    }

    /// A predecessor that stops naming its successor is what orphans the
    /// successor, and it does so as a field change on its own record — the
    /// canonical edge, which the successor's own `supersedes` also holds
    /// up, does not move. The diff answers for that finding too.
    #[test]
    fn a_predecessor_that_stops_pointing_forward_relinks_its_successor() {
        let today = crate::test_today();
        let config = Config::default();
        let successor_edge = || Edge {
            source: "new".into(),
            target: ResolvedTarget::resolved("old"),
            relation: "supersedes".into(),
            location: "frontmatter:supersedes".into(),
        };
        let before = graph_of(
            vec![
                Node {
                    superseded_by: Some("new".into()),
                    ..doc("old", "generic")
                },
                doc("new", "generic"),
                doc("standing", "generic"),
            ],
            vec![successor_edge()],
        );
        let after = graph_of(
            vec![
                doc("old", "generic"),
                doc("new", "generic"),
                doc("standing", "generic"),
            ],
            vec![successor_edge()],
        );
        let diff = crate::diff::compute_diff(&before, &after);
        assert!(
            diff.added_edges.is_empty() && diff.removed_edges.is_empty(),
            "the canonical edge did not move: {diff:?}"
        );
        let touched = diff.touched("HEAD");
        let run = run(&after, &config, today);
        let ctx = RuleContext {
            today,
            graph: &after,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(Path::new(".")),
            repository: None,
            since: None,
        };
        let narrowed: Vec<&str> = run
            .violations
            .iter()
            .filter(|v| OrphanRule.touched_by(&ctx, &touched, v))
            .map(subject)
            .collect();
        assert_eq!(
            narrowed,
            vec!["new"],
            "the successor its predecessor stopped naming; `old` is still referenced by \
             `new`'s edge, and `standing` the diff never reached"
        );
    }

    /// A diff answers for the orphan it made whichever side it touched:
    /// the neighbour whose link was removed is the record that moved,
    /// and the document it stranded is the one relinked. Standing
    /// orphans the diff did not reach are not the diff's to report.
    #[test]
    fn a_diff_answers_for_the_orphan_it_made_and_not_for_standing_ones() {
        let today = crate::test_today();
        let config = Config::default();
        let before = graph_of(
            vec![
                doc("hub", "generic"),
                doc("leaf", "generic"),
                doc("standing", "generic"),
            ],
            vec![Edge {
                source: "hub".into(),
                target: ResolvedTarget::resolved("leaf"),
                relation: "references".into(),
                location: "L1".into(),
            }],
        );
        let after = graph_of(
            vec![
                doc("hub", "generic"),
                doc("leaf", "generic"),
                doc("standing", "generic"),
            ],
            vec![],
        );
        let touched = crate::diff::compute_diff(&before, &after).touched("HEAD");
        let run = run(&after, &config, today);
        let narrowed: Vec<&str> = run
            .violations
            .iter()
            .filter(|v| {
                OrphanRule.touched_by(
                    &RuleContext {
                        today,
                        graph: &after,
                        config: &config,
                        files: crate::builder::scanner::ProjectFiles::working_tree(Path::new(".")),
                        repository: None,
                        since: None,
                    },
                    &touched,
                    v,
                )
            })
            .map(subject)
            .collect();
        assert_eq!(
            narrowed,
            vec!["hub", "leaf"],
            "the neighbour that moved and the document it stranded; not `standing`"
        );
    }

    /// A corpus whose every kind is leaf-by-design is guarded by nothing,
    /// and reports that rather than a clean bill: an empty findings list
    /// is what a thorough pass and a vacuous one both look like.
    #[test]
    fn a_fully_exempt_corpus_reports_no_reach() {
        let today = crate::test_today();
        let mut config = Config::default();
        config.detection.orphan_ok_kinds = vec!["guide".into()];
        let graph = graph_of(vec![doc("a", "guide"), doc("b", "guide")], vec![]);
        let run = run(&graph, &config, today);
        assert!(run.violations.is_empty());
        assert_eq!(run.subjects, 0, "the rule stands over nothing");
    }

    /// Grace narrows the population, not the findings: a document inside
    /// it is not asked, and the same document past it is asked and fails.
    #[test]
    fn grace_moves_a_document_into_the_population() {
        let today = crate::test_today();
        let mut config = Config::default();
        config.detection.orphan_grace_days = 14;
        let fresh = Node {
            created: Some(today - Duration::days(3)),
            ..doc("new", "generic")
        };
        let settled = Node {
            created: Some(today - Duration::days(30)),
            ..doc("new", "generic")
        };

        let inside = run(&graph_of(vec![fresh], vec![]), &config, today);
        assert!(inside.violations.is_empty());
        assert_eq!(inside.subjects, 0);

        let outside = run(&graph_of(vec![settled], vec![]), &config, today);
        assert_eq!(outside.violations.len(), 1);
        assert_eq!(outside.subjects, 1);
    }
}
