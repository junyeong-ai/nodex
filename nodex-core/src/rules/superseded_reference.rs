//! Citations of a document its lineage has moved past.
//!
//! A retired document whose lineage continues in a live one has a
//! replacement, and a live document citing it points its reader at a decision
//! the project replaced. Both halves decide it. A retired document with no live
//! successor — archived, deprecated, abandoned, or superseded only by others
//! since retired — leaves nothing to repoint to and stays a legitimate thing to
//! cite; a live one that a draft claims to supersede is still in force by its
//! own status. The succession is read from the resolved `supersedes` edges
//! alone, which the build materialises from both authoring styles, so it is the
//! lineage `query chain` walks.
//!
//! What is asked is every citation a live document makes of another document,
//! less what is not a claim about what is current. Succession itself
//! (`supersedes`) is the record of the replacement. A document of a kind in
//! `detection.superseded_reference_ok_kinds` catalogues history by design — an
//! ADR index, a decision log. And a citation read from a part a lock holds
//! records what was true when it was written: the lock would refuse the edit
//! the finding asks for. Without a baseline the run cannot tell which parts a
//! lock holds, so a citation one could hold is counted as unjudged instead. A
//! citation from the superseding lineage is asked and passes: it is the
//! successor saying what it replaced.
//!
//! Warning severity, like the rest of the detection plane, because the finding
//! is made by another document's record: `lifecycle supersede` is the write
//! that creates it on every live citer, and a write gate refusing Error
//! findings would refuse the supersession instead of the stale citations.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::diff::{GraphDiff, Touched};
use crate::model::edge::{ID_RESOLVED_RELATIONS, PATH_ONLY_RELATION};
use crate::model::{Graph, Node};
use crate::query::structure::{Direction, adjacent};

use super::{
    DocumentPart, Rule, RuleContext, RuleRun, Severity, SubjectUnit, Violation, ViolationDetails,
    detail::Evidence,
};

const SUCCESSION: &str = "supersedes";

pub struct SupersededReferenceRule;

impl Rule for SupersededReferenceRule {
    fn id(&self) -> &str {
        "superseded_reference"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn description(&self) -> &str {
        "Live documents citing a terminal document whose `supersedes` lineage continues in a \
         live one; the lineage's own citations pass, and `supersedes` itself, \
         `detection.superseded_reference_ok_kinds` and parts a lock holds are not asked"
    }

    fn params(&self, config: &Config) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "superseded_reference_ok_kinds".into(),
            json!(config.detection.superseded_reference_ok_kinds),
        );
        m
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Edges
    }

    /// The verdict reads the citing document's record and the records of the
    /// target's lineage, so a diff answers for a finding when it touched the
    /// citer, or the target or any successor either way — a status, a
    /// succession declared or dropped.
    fn touched_by(&self, ctx: &RuleContext<'_>, since: &Touched, violation: &Violation) -> bool {
        let ViolationDetails::SupersededReference { target, .. } = &violation.details else {
            unreachable!("superseded_reference emits only its own details")
        };
        let moved = |id: &str| since.document(id) || since.relinked(id);
        violation
            .node_id
            .as_deref()
            .is_some_and(|id| since.document(id))
            || moved(target)
            || lineage(ctx.graph, ctx.config, target)
                .successors
                .into_iter()
                .any(moved)
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let baseline = ctx.since.map(Baseline::of);
        let mut lineages: BTreeMap<&str, Lineage<'_>> = BTreeMap::new();
        let mut subjects = 0;
        let mut unjudged = 0;
        let mut violations = Vec::new();
        for edge in ctx.graph.edges() {
            if edge.relation == SUCCESSION {
                continue;
            }
            let (Some(source), Some(target)) = (
                ctx.graph.node(&edge.source),
                edge.target.id().and_then(|id| ctx.graph.node(id)),
            ) else {
                continue;
            };
            if source.id == target.id
                || ctx.config.is_terminal(source.status.as_str())
                || ctx
                    .config
                    .is_superseded_reference_ok_kind(source.kind.as_str())
            {
                continue;
            }
            let part = part_of(&edge.relation);
            match &baseline {
                Some(baseline) if baseline.holds(ctx.config, source, &part) => continue,
                None if locks(
                    ctx.config,
                    source.kind.as_str(),
                    source.status.as_str(),
                    &part,
                ) =>
                {
                    unjudged += 1;
                    continue;
                }
                _ => {}
            }
            subjects += 1;
            if !ctx.config.is_terminal(target.status.as_str()) {
                continue;
            }
            let lineage = lineages
                .entry(target.id.as_str())
                .or_insert_with(|| lineage(ctx.graph, ctx.config, &target.id));
            if lineage.current.is_empty() || lineage.successors.contains(source.id.as_str()) {
                continue;
            }
            violations.push(Violation::new(
                self.id(),
                self.severity(),
                Some(source.id.clone()),
                Some(crate::path_guard::forward_string(&source.path)),
                ViolationDetails::SupersededReference {
                    relation: edge.relation.clone(),
                    target: target.id.clone(),
                    location: Evidence(edge.location.clone()),
                    current: Evidence(lineage.current.clone()),
                },
            ));
        }
        RuleRun::new(subjects, violations).unjudged(unjudged)
    }
}

/// Every document that supersedes one, directly or through each other, and
/// the live ones the lineage continues in — on each path forward, the first
/// successor not retired. `current` is empty where every path ends retired.
struct Lineage<'g> {
    successors: BTreeSet<&'g str>,
    current: Vec<String>,
}

fn lineage<'g>(graph: &'g Graph, config: &Config, id: &str) -> Lineage<'g> {
    let succession = BTreeSet::from([SUCCESSION]);
    let newer = |id: &str| adjacent(graph, id, Direction::Incoming, Some(&succession));
    let retired = |id: &str| {
        graph
            .node(id)
            .is_some_and(|node| config.is_terminal(node.status.as_str()))
    };
    let mut successors = BTreeSet::new();
    let mut frontier = newer(id);
    while let Some(next) = frontier.pop() {
        if successors.insert(next) {
            frontier.extend(newer(next));
        }
    }
    let mut current = BTreeSet::new();
    let mut passed = BTreeSet::new();
    let mut frontier = newer(id);
    while let Some(next) = frontier.pop() {
        if !retired(next) {
            current.insert(next);
        } else if passed.insert(next) {
            frontier.extend(newer(next));
        }
    }
    Lineage {
        successors,
        current: current.into_iter().map(str::to_string).collect(),
    }
}

/// The part of its document a citation was read from. The id relations and
/// `covers` are each produced only by the frontmatter field of the same name —
/// `Config::validate` keeps them off link patterns — and every other relation
/// only by the body.
fn part_of(relation: &str) -> DocumentPart {
    if ID_RESOLVED_RELATIONS.contains(&relation) || relation == PATH_ONLY_RELATION {
        DocumentPart::Field(relation.to_string())
    } else {
        DocumentPart::Body
    }
}

/// The frame the immutability locks judge in. A run without a diff has none,
/// and cannot tell a part a lock holds from one it does not.
struct Baseline<'a> {
    diff: &'a GraphDiff,
    added: BTreeSet<&'a str>,
}

impl<'a> Baseline<'a> {
    fn of(diff: &'a GraphDiff) -> Self {
        Self {
            diff,
            added: diff.added_ids(),
        }
    }

    /// Whether a lock holds `part` of `node` in this run, read as the locks
    /// read it: over a record the baseline holds, at the kind and status the
    /// baseline gave it. A document written since, or one only now moving into
    /// an arming status, is not held — this is the edit where its citation can
    /// still change.
    fn holds(&self, config: &Config, node: &Node, part: &DocumentPart) -> bool {
        !self.added.contains(node.id.as_str())
            && locks(
                config,
                self.diff.before_kind(&node.id, node.kind.as_str()),
                self.diff.before_status(&node.id, node.status.as_str()),
                part,
            )
    }
}

/// Whether a lock block covering `part` arms over a record of `kind` at
/// `status`.
fn locks(config: &Config, kind: &str, status: &str, part: &DocumentPart) -> bool {
    let armed = |kinds: &[String], trigger, statuses: &[String]| {
        super::kind_allowed(kinds, kind) && config.lock_arms(trigger, statuses, status)
    };
    match part {
        DocumentPart::Body => config
            .rules
            .body_immutable
            .iter()
            .any(|block| armed(&block.kinds, block.trigger, &block.statuses)),
        DocumentPart::Field(field) => config.rules.frontmatter_immutable.iter().any(|block| {
            block.fields.contains(field) && armed(&block.kinds, block.trigger, &block.statuses)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        BodyImmutableMode, BodyImmutableRuleConfig, FrontmatterImmutableRuleConfig,
        ImmutableTrigger,
    };
    use crate::model::{Edge, GraphMeta, Kind, ResolvedTarget, Status};
    use indexmap::IndexMap;
    use std::path::PathBuf;

    fn doc(id: &str, status: &str) -> Node {
        typed(id, "generic", status)
    }

    fn typed(id: &str, kind: &str, status: &str) -> Node {
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
            attrs: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            body_structure: Default::default(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn edge(source: &str, target: &str, relation: &str) -> Edge {
        Edge {
            source: source.to_string(),
            target: ResolvedTarget::resolved(target),
            relation: relation.to_string(),
            location: if part_of(relation) == DocumentPart::Body {
                "L1".to_string()
            } else {
                format!("frontmatter:{relation}")
            },
        }
    }

    fn cites(source: &str, target: &str) -> Edge {
        edge(source, target, "references")
    }

    fn supersedes(newer: &str, older: &str) -> Edge {
        edge(newer, older, SUCCESSION)
    }

    fn graph_of(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, edges, vec![], vec![], vec![], GraphMeta::default())
    }

    fn run(graph: &Graph, config: &Config) -> RuleRun {
        SupersededReferenceRule.check(&super::super::test_ctx(graph, config))
    }

    fn run_since(graph: &Graph, config: &Config, diff: &GraphDiff) -> RuleRun {
        SupersededReferenceRule.check(&RuleContext {
            since: Some(diff),
            ..super::super::test_ctx(graph, config)
        })
    }

    /// `(citer, target, current)` for every finding, in report order.
    fn findings(run: &RuleRun) -> Vec<(&str, &str, Vec<&str>)> {
        run.violations
            .iter()
            .map(|v| match &v.details {
                ViolationDetails::SupersededReference {
                    target, current, ..
                } => (
                    v.node_id.as_deref().expect("a citation has a citer"),
                    target.as_str(),
                    current.iter().map(String::as_str).collect(),
                ),
                other => panic!("unexpected details {other:?}"),
            })
            .collect()
    }

    fn body_lock(
        trigger: ImmutableTrigger,
        kinds: &[&str],
        statuses: &[&str],
    ) -> BodyImmutableRuleConfig {
        BodyImmutableRuleConfig {
            name: "lock".into(),
            mode: BodyImmutableMode::Frozen,
            trigger,
            kinds: kinds.iter().map(|k| k.to_string()).collect(),
            statuses: statuses.iter().map(|s| s.to_string()).collect(),
            append_section: None,
        }
    }

    /// A citation belongs at the end of the lineage, not at the next link of
    /// it: repointing to a successor that is itself superseded would only
    /// move the finding.
    #[test]
    fn a_citation_of_a_superseded_document_names_where_its_lineage_stands() {
        let graph = graph_of(
            vec![
                doc("v1", "superseded"),
                doc("v2", "superseded"),
                doc("v3", "active"),
                doc("reader", "active"),
            ],
            vec![
                supersedes("v2", "v1"),
                supersedes("v3", "v2"),
                cites("reader", "v1"),
                edge("reader", "v2", "related"),
            ],
        );
        let run = run(&graph, &Config::default());
        assert_eq!(
            findings(&run),
            vec![("reader", "v1", vec!["v3"]), ("reader", "v2", vec!["v3"])]
        );
        assert_eq!(run.subjects, 2, "the two citations; succession is not one");
    }

    /// A fork has no single replacement, and the finding does not pick one.
    #[test]
    fn a_split_names_every_current_document() {
        let graph = graph_of(
            vec![
                doc("whole", "superseded"),
                doc("left", "active"),
                doc("right", "active"),
                doc("reader", "active"),
            ],
            vec![
                supersedes("left", "whole"),
                supersedes("right", "whole"),
                cites("reader", "whole"),
            ],
        );
        assert_eq!(
            findings(&run(&graph, &Config::default())),
            vec![("reader", "whole", vec!["left", "right"])]
        );
    }

    /// Retirement without a successor leaves nothing to repoint to, and a
    /// successor without retirement leaves the cited document in force: each
    /// half alone is a citation the rule guards and passes.
    #[test]
    fn retirement_and_succession_are_each_needed() {
        let graph = graph_of(
            vec![
                doc("archived", "archived"),
                doc("in-force", "active"),
                doc("draft", "draft"),
                doc("reader", "active"),
            ],
            vec![
                supersedes("draft", "in-force"),
                cites("reader", "archived"),
                cites("reader", "in-force"),
            ],
        );
        let run = run(&graph, &Config::default());
        assert!(run.violations.is_empty(), "{:?}", run.violations);
        assert_eq!(run.subjects, 2);
    }

    /// The lineage continues where a successor is still in force, whatever
    /// claims to supersede that one in turn, and it ends where every path runs
    /// into retired documents — there is then nothing to repoint to.
    #[test]
    fn the_lineage_continues_only_in_a_live_document() {
        let graph = graph_of(
            vec![
                doc("v1", "superseded"),
                doc("v2", "active"),
                doc("v3", "draft"),
                doc("w1", "superseded"),
                doc("w2", "archived"),
                doc("f", "superseded"),
                doc("kept", "active"),
                doc("dropped", "abandoned"),
                doc("reader", "active"),
            ],
            vec![
                supersedes("v2", "v1"),
                supersedes("v3", "v2"),
                supersedes("w2", "w1"),
                supersedes("kept", "f"),
                supersedes("dropped", "f"),
                cites("reader", "v1"),
                cites("reader", "w1"),
                cites("reader", "f"),
            ],
        );
        assert_eq!(
            findings(&run(&graph, &Config::default())),
            vec![("reader", "v1", vec!["v2"]), ("reader", "f", vec!["kept"])],
            "the draft is not where v1 continues, and w1's line ended retired"
        );
    }

    /// The superseding lineage citing what it replaced is the replacement
    /// described, not a stale pointer — however many links down it sits.
    #[test]
    fn the_superseding_lineage_may_cite_what_it_replaced() {
        let graph = graph_of(
            vec![
                doc("v1", "superseded"),
                doc("v2", "superseded"),
                doc("v3", "active"),
            ],
            vec![
                supersedes("v2", "v1"),
                supersedes("v3", "v2"),
                cites("v3", "v1"),
                cites("v3", "v2"),
            ],
        );
        let run = run(&graph, &Config::default());
        assert!(run.violations.is_empty(), "{:?}", run.violations);
        assert_eq!(run.subjects, 2, "guarded and passed, not exempt");
    }

    /// A retired document is not asked to maintain what it cites.
    #[test]
    fn a_retired_citer_is_outside_the_population() {
        let graph = graph_of(
            vec![
                doc("old", "superseded"),
                doc("new", "active"),
                doc("history", "archived"),
            ],
            vec![supersedes("new", "old"), cites("history", "old")],
        );
        let run = run(&graph, &Config::default());
        assert!(run.violations.is_empty());
        assert_eq!(run.subjects, 0);
    }

    /// An index lists what was superseded because it was superseded; the
    /// project says which kinds do that, and they are not asked.
    #[test]
    fn a_kind_that_catalogues_history_is_outside_the_population() {
        let mut config = Config::default();
        config.detection.superseded_reference_ok_kinds = vec!["readme".into()];
        let graph = graph_of(
            vec![
                doc("old", "superseded"),
                doc("new", "active"),
                typed("index", "readme", "active"),
                doc("guide", "active"),
            ],
            vec![
                supersedes("new", "old"),
                cites("index", "old"),
                cites("index", "new"),
                cites("guide", "old"),
            ],
        );
        let run = run(&graph, &config);
        assert_eq!(findings(&run), vec![("guide", "old", vec!["new"])]);
        assert_eq!(run.subjects, 1);
    }

    /// A lock holds a part, not a document: the held body's citation is a
    /// record of what was true when it was written, while the same document's
    /// unlocked field is still asked.
    #[test]
    fn a_part_a_lock_holds_is_outside_the_population() {
        let mut config = Config::default();
        config.rules.body_immutable = vec![body_lock(ImmutableTrigger::Status, &[], &["accepted"])];
        let graph = graph_of(
            vec![
                doc("old", "superseded"),
                doc("new", "active"),
                doc("adopted", "accepted"),
                doc("draft", "draft"),
            ],
            vec![
                supersedes("new", "old"),
                cites("adopted", "old"),
                edge("adopted", "old", "related"),
                cites("draft", "old"),
            ],
        );
        let unchanged = crate::diff::compute_diff(&graph, &graph);
        let body_held = run_since(&graph, &config, &unchanged);
        assert_eq!(
            findings(&body_held),
            vec![
                ("adopted", "old", vec!["new"]),
                ("draft", "old", vec!["new"])
            ],
            "the held body citation is not asked; the field and the unarmed draft are"
        );
        assert_eq!(body_held.subjects, 2);

        config.rules.frontmatter_immutable = vec![FrontmatterImmutableRuleConfig {
            name: "adopted-links".into(),
            fields: vec!["related".into()],
            trigger: ImmutableTrigger::Status,
            kinds: vec![],
            statuses: vec!["accepted".into()],
        }];
        let both_held = run_since(&graph, &config, &unchanged);
        assert_eq!(findings(&both_held), vec![("draft", "old", vec!["new"])]);
        assert_eq!(both_held.subjects, 1);
    }

    /// A lock holds what the baseline armed it over, and nothing else: a
    /// document written since and one only now entering an arming status can
    /// still change their citations in this edit, while one leaving it cannot.
    /// A run with no baseline cannot tell, and counts what a lock could hold
    /// as unjudged rather than asking it to change.
    #[test]
    fn a_lock_holds_only_what_the_baseline_armed() {
        let mut config = Config::default();
        config.rules.body_immutable = vec![
            body_lock(ImmutableTrigger::Creation, &["adr"], &[]),
            body_lock(ImmutableTrigger::Status, &["generic"], &["accepted"]),
        ];
        let succession = || vec![doc("old", "superseded"), doc("new", "active")];
        let citations = |ids: &[&str]| {
            std::iter::once(supersedes("new", "old"))
                .chain(ids.iter().map(|id| cites(id, "old")))
                .collect::<Vec<_>>()
        };
        let before = graph_of(
            [
                succession(),
                vec![
                    typed("committed", "adr", "active"),
                    doc("entering", "draft"),
                    doc("leaving", "accepted"),
                ],
            ]
            .concat(),
            citations(&["committed", "entering", "leaving"]),
        );
        let after = graph_of(
            [
                succession(),
                vec![
                    typed("committed", "adr", "active"),
                    typed("fresh", "adr", "active"),
                    doc("entering", "accepted"),
                    doc("leaving", "active"),
                ],
            ]
            .concat(),
            citations(&["committed", "fresh", "entering", "leaving"]),
        );
        let diff = crate::diff::compute_diff(&before, &after);
        let citers = |run: &RuleRun| -> Vec<String> {
            findings(run)
                .iter()
                .map(|(citer, ..)| citer.to_string())
                .collect()
        };
        let since = run_since(&after, &config, &diff);
        assert_eq!(citers(&since), ["fresh", "entering"]);
        assert_eq!((since.subjects, since.unjudged), (2, 0));

        let unanchored = run(&after, &config);
        assert_eq!(citers(&unanchored), ["leaving"]);
        assert_eq!((unanchored.subjects, unanchored.unjudged), (1, 3));
    }

    /// Retiring the target is an edit to another document's record, and it
    /// is the edit that makes a standing citation stale.
    #[test]
    fn a_diff_answers_for_the_citations_its_retirement_made_stale() {
        let before = graph_of(
            vec![
                doc("old", "active"),
                doc("new", "active"),
                doc("reader", "active"),
            ],
            vec![supersedes("new", "old"), cites("reader", "old")],
        );
        let after = graph_of(
            vec![
                doc("old", "superseded"),
                doc("new", "active"),
                doc("reader", "active"),
            ],
            vec![supersedes("new", "old"), cites("reader", "old")],
        );
        let config = Config::default();
        let ctx = super::super::test_ctx(&after, &config);
        let run = SupersededReferenceRule.check(&ctx);
        assert_eq!(run.violations.len(), 1);

        let retired = crate::diff::compute_diff(&before, &after).touched("HEAD");
        assert!(SupersededReferenceRule.touched_by(&ctx, &retired, &run.violations[0]));

        let unrelated = graph_of(
            vec![
                doc("old", "superseded"),
                doc("new", "active"),
                doc("reader", "active"),
                doc("elsewhere", "active"),
            ],
            vec![supersedes("new", "old"), cites("reader", "old")],
        );
        let quiet = crate::diff::compute_diff(&after, &unrelated).touched("HEAD");
        let later = super::super::test_ctx(&unrelated, &config);
        let standing = SupersededReferenceRule.check(&later);
        assert!(!SupersededReferenceRule.touched_by(&later, &quiet, &standing.violations[0]));
    }
}
