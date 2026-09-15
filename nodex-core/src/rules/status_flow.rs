//! Hold documents to the lifecycle `statuses.flow` declares.
//!
//! Two rules, because a lifecycle has two ways to be wrong and a record
//! can only be in one of them. [`StatusTransitionRule`] judges a record
//! the baseline holds: it moved, and the flow has to name the move.
//! [`StatusEntryRule`] judges a record the baseline does not hold: it
//! arrived, and a record that arrives already accepted never made the
//! move at all — the one path around the flow that no transition check
//! can see, because nothing transitioned.
//!
//! The two populations are complementary by construction, so between
//! them every document in scope is guarded and each reports the half it
//! stands over. Neither is registered unless the project declares
//! `statuses.flow`; a project that declares no flow has no flow to
//! break, and a rule with nothing to judge is left out of the registry
//! rather than reported as skipped.
//!
//! Both are scoped by the flow's own `kinds`, because an acceptance event
//! belongs to the kinds that have one. A runbook written live has no
//! promotion step, and judging it against an ADR's lifecycle would demand
//! an event it never has.
//!
//! Both read the transition stream and the added-node set off
//! [`crate::diff::GraphDiff`], so they stay pure functions of
//! `(graph, config)` like every other check-time rule.
//!
//! What neither rule judges is a record whose id changed. A node is its
//! id, so a re-key removes one record and adds another, and the document
//! standing at that path has no prior record under a name either rule can
//! look it up by. `frontmatter_immutable` refuses to lock `id` on the same
//! ground. `StatusEntryRule` counts those apart as `unjudged` rather than
//! reading them as births, because a re-keyed record arriving at `active`
//! says nothing about where the document it continues was authored.

use serde_json::{Map, Value, json};

use super::{Rule, RuleContext, RuleRun, Severity, SubjectUnit, Violation, ViolationDetails};

/// A status change the declared flow does not name.
pub struct StatusTransitionRule;

impl Rule for StatusTransitionRule {
    fn id(&self) -> &str {
        "status_transition"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "A document's status moves only where `statuses.flow` declares it may, over the \
         kinds that flow governs; needs a diff context from `--since <ref>` or \
         `rules.immutable_baseline`"
    }

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        let flow = config.status_flow();
        let mut m = Map::new();
        m.insert("kinds".into(), json!(flow.map(|f| &f.kinds)));
        m.insert("transitions".into(), json!(flow.map(|f| &f.transitions)));
        m
    }

    fn diff_aware(&self) -> bool {
        true
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        ctx.since.is_some()
    }

    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        "no diff context — set `--since <ref>` or `rules.immutable_baseline`".to_string()
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (Some(diff), Some(flow)) = (ctx.since, ctx.config.status_flow()) else {
            return RuleRun::clean(0);
        };
        // Every record of a governed kind that the baseline holds is one
        // this rule stands over: any of them can move, and a run where none
        // did is a clean run, not an idle rule. A record the baseline has no
        // node for has no prior status to have moved from, so it is counted
        // apart — `StatusEntryRule` is what judges those.
        let unbacked = diff.added_ids();
        let (subjects, unjudged) =
            ctx.graph
                .nodes()
                .values()
                .fold((0, 0), |(backed, added), node| {
                    if !super::kind_allowed(
                        &flow.kinds,
                        diff.before_kind(&node.id, node.kind.as_str()),
                    ) {
                        return (backed, added);
                    }
                    match unbacked.contains(node.id.as_str()) {
                        false => (backed + 1, added),
                        true => (backed, added + 1),
                    }
                });

        let mut violations = Vec::new();
        for transition in &diff.status_transitions {
            let Some(node) = ctx.graph.node(&transition.id) else {
                continue;
            };
            if !super::kind_allowed(
                &flow.kinds,
                diff.before_kind(&transition.id, node.kind.as_str()),
            ) {
                continue;
            }
            let declared = flow
                .transitions
                .get(&transition.from)
                .map_or(&[][..], Vec::as_slice);
            if declared.contains(&transition.to) {
                continue;
            }
            violations.push(Violation::new(
                self.id().to_string(),
                self.severity(),
                Some(transition.id.clone()),
                Some(crate::path_guard::forward_string(&node.path)),
                ViolationDetails::StatusTransition {
                    from: transition.from.clone(),
                    to: transition.to.clone(),
                    declared: declared.to_vec(),
                },
            ));
        }
        RuleRun::new(subjects, violations).unjudged(unjudged)
    }
}

/// A document authored straight into a status the flow does not start at.
pub struct StatusEntryRule;

impl Rule for StatusEntryRule {
    fn id(&self) -> &str {
        "status_entry"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "A document of a kind `statuses.flow` governs arrives at `statuses.initial` and \
         reaches every other status by a declared transition; needs a diff context from \
         `--since <ref>` or `rules.immutable_baseline`"
    }

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "kinds".into(),
            json!(config.status_flow().map(|flow| &flow.kinds)),
        );
        m.insert("initial".into(), json!(config.initial_status()));
        m
    }

    fn diff_aware(&self) -> bool {
        true
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        ctx.since.is_some()
    }

    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        "no diff context — set `--since <ref>` or `rules.immutable_baseline`".to_string()
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (Some(diff), Some(flow)) = (ctx.since, ctx.config.status_flow()) else {
            return RuleRun::clean(0);
        };
        // A document standing where the baseline held one under another id
        // was re-keyed, not authored: the record it continues is present in
        // both snapshots under two names, and its status was already
        // whatever it was. Reading that as a birth would report every
        // re-keyed accepted record as born accepted, so it is counted apart
        // instead — the id is what nodex identifies a record by, and this
        // rule does not go looking for a second answer.
        let vacated: std::collections::BTreeSet<&str> = diff
            .removed_nodes
            .iter()
            .map(|node| node.path.as_str())
            .collect();
        let initial = ctx.config.initial_status();

        let mut subjects = 0;
        let mut unjudged = 0;
        let mut violations = Vec::new();
        for added in &diff.added_nodes {
            if !super::kind_allowed(&flow.kinds, &added.kind) {
                continue;
            }
            if vacated.contains(added.path.as_str()) {
                unjudged += 1;
                continue;
            }
            subjects += 1;
            if added.status == initial {
                continue;
            }
            violations.push(Violation::new(
                self.id().to_string(),
                self.severity(),
                Some(added.id.clone()),
                Some(added.path.clone()),
                ViolationDetails::StatusEntry {
                    status: added.status.clone(),
                    initial: initial.to_string(),
                },
            ));
        }
        RuleRun::new(subjects, violations).unjudged(unjudged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::diff::{GraphDiff, StatusTransition};
    use crate::model::{Graph, Kind, Node, Status};
    use crate::query::NodeRef;
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn config() -> Config {
        toml::from_str(
            r#"
[statuses]
allowed = ["proposed", "active", "superseded", "archived"]
terminal = ["superseded", "archived"]
initial = "proposed"

[statuses.flow]
transitions = { proposed = ["active", "archived"], active = ["superseded", "archived"] }
"#,
        )
        .expect("parses")
    }

    fn node(id: &str, status: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.into(),
            kind: Kind::new("generic"),
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
            body_structure: Default::default(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn graph(nodes: &[Node]) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n.clone());
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

    fn empty_diff() -> GraphDiff {
        GraphDiff {
            added_nodes: Vec::new(),
            removed_nodes: Vec::new(),
            added_edges: Vec::new(),
            removed_edges: Vec::new(),
            status_transitions: Vec::new(),
            field_changes: Vec::new(),
            path_changes: Vec::new(),
            added_annotations: Vec::new(),
            removed_annotations: Vec::new(),
            body_changes: Vec::new(),
        }
    }

    fn node_ref(id: &str, status: &str, path: &str) -> NodeRef {
        NodeRef {
            id: id.into(),
            title: id.into(),
            kind: "generic".into(),
            status: status.into(),
            path: path.into(),
        }
    }

    fn run(rule: &dyn Rule, config: &Config, graph: &Graph, diff: &GraphDiff) -> RuleRun {
        rule.check(&RuleContext {
            today: crate::test_today(),
            graph,
            config,
            files: crate::builder::scanner::ProjectFiles::working_tree(std::path::Path::new(".")),
            history: &crate::rules::UNMEASURED,
            since: Some(diff),
        })
    }

    #[test]
    fn a_declared_transition_passes() {
        let g = graph(&[node("a", "active")]);
        let mut diff = empty_diff();
        diff.status_transitions.push(StatusTransition {
            id: "a".into(),
            from: "proposed".into(),
            to: "active".into(),
        });
        let run = run(&StatusTransitionRule, &config(), &g, &diff);
        assert!(run.violations.is_empty(), "{:?}", run.violations);
        assert_eq!(run.subjects, 1, "the record the baseline holds is guarded");
    }

    #[test]
    fn a_transition_the_flow_does_not_name_is_refused() {
        // The demotion that would otherwise disarm a status-armed body
        // lock: legal frontmatter, no declared transition.
        let g = graph(&[node("a", "proposed")]);
        let mut diff = empty_diff();
        diff.status_transitions.push(StatusTransition {
            id: "a".into(),
            from: "active".into(),
            to: "proposed".into(),
        });
        let run = run(&StatusTransitionRule, &config(), &g, &diff);
        assert_eq!(run.violations.len(), 1);
        assert!(matches!(
            &run.violations[0].details,
            ViolationDetails::StatusTransition { from, to, declared }
                if from == "active" && to == "proposed"
                    && declared == &["superseded".to_string(), "archived".to_string()]
        ));
    }

    #[test]
    fn leaving_a_terminal_status_is_refused() {
        // A terminal status names no transition, so every move out of one
        // is undeclared — the same refusal the `lifecycle` write seam
        // already gives, now reaching an edit that did not go through it.
        let g = graph(&[node("a", "active")]);
        let mut diff = empty_diff();
        diff.status_transitions.push(StatusTransition {
            id: "a".into(),
            from: "superseded".into(),
            to: "active".into(),
        });
        let run = run(&StatusTransitionRule, &config(), &g, &diff);
        assert_eq!(run.violations.len(), 1);
    }

    #[test]
    fn an_added_record_is_outside_the_transition_population() {
        let g = graph(&[node("a", "active"), node("b", "proposed")]);
        let mut diff = empty_diff();
        diff.added_nodes.push(node_ref("b", "proposed", "b.md"));
        let run = run(&StatusTransitionRule, &config(), &g, &diff);
        assert_eq!(run.subjects, 1, "only the record the baseline holds");
        assert_eq!(run.unjudged, 1, "the added one is counted apart");
    }

    #[test]
    fn a_record_authored_at_the_initial_status_passes() {
        let g = graph(&[node("a", "proposed")]);
        let mut diff = empty_diff();
        diff.added_nodes.push(node_ref("a", "proposed", "a.md"));
        let run = run(&StatusEntryRule, &config(), &g, &diff);
        assert!(run.violations.is_empty());
        assert_eq!(run.subjects, 1);
    }

    #[test]
    fn a_record_authored_straight_into_acceptance_is_refused() {
        let g = graph(&[node("a", "active")]);
        let mut diff = empty_diff();
        diff.added_nodes.push(node_ref("a", "active", "a.md"));
        let run = run(&StatusEntryRule, &config(), &g, &diff);
        assert_eq!(run.violations.len(), 1);
        assert!(matches!(
            &run.violations[0].details,
            ViolationDetails::StatusEntry { status, initial }
                if status == "active" && initial == "proposed"
        ));
    }

    #[test]
    fn a_kind_the_flow_does_not_govern_is_judged_by_neither_rule() {
        // The shape a real corpus has: ADRs are proposed and then accepted,
        // while a runbook is written live and has no promotion step at all.
        // Judging the runbook against the ADR lifecycle would demand an
        // event it never has.
        let mut config = config();
        config.kinds.allowed = vec!["adr".into(), "runbook".into(), "generic".into()];
        config.statuses.flow.as_mut().expect("declared").kinds = vec!["adr".into()];

        let mut runbook = node("r", "active");
        runbook.kind = Kind::new("runbook");
        let g = graph(&[runbook]);
        let mut diff = empty_diff();
        diff.added_nodes.push(NodeRef {
            kind: "runbook".into(),
            ..node_ref("r", "active", "r.md")
        });
        diff.status_transitions.push(StatusTransition {
            id: "r".into(),
            from: "active".into(),
            to: "proposed".into(),
        });

        let entry = run(&StatusEntryRule, &config, &g, &diff);
        assert!(entry.violations.is_empty(), "{:?}", entry.violations);
        assert_eq!(entry.subjects, 0, "a kind with no lifecycle is not guarded");

        let moved = run(&StatusTransitionRule, &config, &g, &diff);
        assert!(moved.violations.is_empty(), "{:?}", moved.violations);
        assert_eq!(moved.subjects, 0);
    }

    #[test]
    fn a_re_keyed_record_is_counted_apart_rather_than_read_as_a_birth() {
        // Same document, same path, same status — only the id moved. It was
        // not authored here and its arrival says nothing about where it was.
        let g = graph(&[node("new", "active")]);
        let mut diff = empty_diff();
        diff.added_nodes.push(node_ref("new", "active", "a.md"));
        diff.removed_nodes.push(node_ref("old", "active", "a.md"));
        let run = run(&StatusEntryRule, &config(), &g, &diff);
        assert!(run.violations.is_empty(), "{:?}", run.violations);
        assert_eq!(run.subjects, 0);
        assert_eq!(run.unjudged, 1);
    }

    #[test]
    fn a_record_added_where_an_unrelated_one_was_removed_is_still_a_birth() {
        // Different path: nothing ties the two, so the added record is one
        // the project authored and the rule judges it.
        let g = graph(&[node("a", "active")]);
        let mut diff = empty_diff();
        diff.added_nodes.push(node_ref("a", "active", "a.md"));
        diff.removed_nodes.push(node_ref("b", "active", "b.md"));
        let run = run(&StatusEntryRule, &config(), &g, &diff);
        assert_eq!(run.violations.len(), 1);
        assert_eq!(run.subjects, 1);
        assert_eq!(run.unjudged, 0);
    }
}
