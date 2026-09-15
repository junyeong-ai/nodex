//! Hold documents to the lifecycle `statuses.flow` declares.
//!
//! A lifecycle is a sequence, so both rules judge history one step at a time
//! ([`crate::ancestry`]): the uncommitted change against the commits it will
//! be committed onto, and each commit a `check --since` range adds against its
//! parents. An endpoint diff cannot stand in for that — a record authored at
//! its entry status and accepted in the next commit reads, across both, as a
//! record that arrived accepted, and a detour that ends where a declared move
//! would have reads as that move. The history is git's, not
//! `rules.immutable_baseline`'s: a step finding is about a commit and nothing
//! short of rewriting that commit clears it, so a plain `check` judges only the
//! change being made, and a range is judged when one is asked for.
//!
//! In each step a record the flow governs is one of two things.
//! [`StatusTransitionRule`] judges a record the flow also governed on a
//! parent: if its status differs from every parent's, the flow has to name
//! the move from one of them. [`StatusEntryRule`] judges a record the flow
//! governed on no parent — written there, moved under an id that follows its
//! path, re-keyed, back after a commit that deleted it, or given a governed
//! kind: it enters the flow, and a record that enters past the entry status
//! never made the moves. A document a commit could not parse holds the record
//! it last held ([`crate::ancestry`]), so one broken and repaired is judged as
//! the record it was; where a shallow clone cuts that reading off, what may
//! have stood there is counted rather than judged. A record leaving the
//! governed kinds is judged by neither rule, since the flow makes no claim
//! about a kind it does not govern, and one that comes back enters again.
//!
//! Neither is registered unless the project declares `statuses.flow`, and
//! both are scoped by the flow's own `kinds`: a runbook written live has no
//! promotion step, and judging it against an ADR's lifecycle would demand an
//! event it never has.

use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use super::{Rule, RuleContext, RuleRun, Severity, SubjectUnit, Violation, ViolationDetails};
use crate::ancestry::{Position, Step};
use crate::config::StatusFlowConfig;
use crate::diff::Touched;
use crate::model::Graph;

/// Every record the flow governs in each step's snapshot, with the positions
/// the flow governed it at on that step's parents — none for a record
/// entering the flow.
fn governed<'a>(
    steps: &'a [Step],
    flow: &'a StatusFlowConfig,
) -> impl Iterator<Item = (&'a Step, &'a str, &'a Position, Vec<&'a Position>)> {
    let governs = move |position: &Position| super::kind_allowed(&flow.kinds, &position.kind);
    steps.iter().flat_map(move |step| {
        step.child
            .iter()
            .filter(move |(_, now)| governs(now))
            .map(move |(id, now)| {
                (
                    step,
                    id,
                    now,
                    step.priors(id).filter(|p| governs(p)).collect(),
                )
            })
    })
}

/// Every record the flow governs in the graph the steps end at. A step holds
/// each of them but a document git ignores, which no commit can record, so
/// that one is selected and never judged.
fn selected<'a>(graph: &'a Graph, flow: &'a StatusFlowConfig) -> impl Iterator<Item = &'a str> {
    graph
        .nodes()
        .values()
        .filter(|node| super::kind_allowed(&flow.kinds, node.kind.as_str()))
        .map(|node| node.id.as_str())
}

/// How much a rule stood over and did not judge: what it selected and never
/// judged, and what it could not judge where it stood — which no later step
/// judging the same record takes back, since a count is all an unjudged
/// record leaves behind.
fn unjudged<'a>(
    stood_over: impl Iterator<Item = &'a str>,
    judged: &BTreeSet<&str>,
    unknown: BTreeSet<&'a str>,
) -> usize {
    stood_over
        .filter(|id| !judged.contains(id))
        .chain(unknown)
        .collect::<BTreeSet<_>>()
        .len()
}

/// The moves `statuses.flow` declares out of `from`.
fn declared<'a>(flow: &'a StatusFlowConfig, from: &str) -> &'a [String] {
    flow.transitions.get(from).map_or(&[][..], Vec::as_slice)
}

fn skip_reason(ctx: &RuleContext<'_>) -> String {
    match ctx.since {
        Some(_) => {
            "judges history a commit at a time, and a proposal judged against the \
                    working tree is not a commit — `check` judges it once written"
        }
        None => "no commit to step from — the project is not in a git work tree",
    }
    .to_string()
}

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
         kinds that flow governs, judged a step at a time — uncommitted changes against HEAD, \
         and each commit `--since <ref>` adds against its parents; needs a git work tree"
    }

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        let flow = config.status_flow();
        let mut m = Map::new();
        m.insert("kinds".into(), json!(flow.map(|f| &f.kinds)));
        m.insert("transitions".into(), json!(flow.map(|f| &f.transitions)));
        m
    }

    fn judges_steps(&self) -> bool {
        true
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        ctx.steps.is_some()
    }

    fn skip_reason(&self, ctx: &RuleContext<'_>) -> String {
        skip_reason(ctx)
    }

    /// Every finding is a step inside the range the run was asked about,
    /// including a move the range went on to undo, which leaves nothing at
    /// the endpoints for the diff to have touched.
    fn touched_by(&self, _ctx: &RuleContext<'_>, _since: &Touched, _violation: &Violation) -> bool {
        true
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (Some(steps), Some(flow)) = (ctx.steps, ctx.config.status_flow()) else {
            return RuleRun::clean(0);
        };
        // A record is judged by this rule at every step a parent already held
        // it governed, whether or not it moved there. One that only ever
        // entered has no prior status to have moved from — `StatusEntryRule`
        // judges those — so it is counted apart, beside the ones no step held.
        let mut judged = BTreeSet::new();
        let mut entered = BTreeSet::new();
        let mut unknown = BTreeSet::new();
        let mut violations = Vec::new();
        for (step, id, now, priors) in governed(steps, flow) {
            if priors.is_empty() {
                match step.priors_known() {
                    true => entered.insert(id),
                    false => unknown.insert(id),
                };
                continue;
            }
            judged.insert(id);
            if priors.iter().any(|prior| {
                prior.status == now.status || declared(flow, &prior.status).contains(&now.status)
            }) {
                continue;
            }
            let froms: BTreeSet<&str> = priors.iter().map(|prior| prior.status.as_str()).collect();
            violations.extend(froms.into_iter().map(|from| {
                Violation::new(
                    self.id().to_string(),
                    self.severity(),
                    Some(id.to_string()),
                    Some(now.path.clone()),
                    ViolationDetails::StatusTransition {
                        from: from.to_string(),
                        to: now.status.clone(),
                        declared: declared(flow, from).to_vec(),
                        commit: step.commit.clone(),
                    },
                )
            }));
        }
        let stood_over = entered.into_iter().chain(selected(ctx.graph, flow));
        RuleRun::new(judged.len(), violations).unjudged(unjudged(stood_over, &judged, unknown))
    }
}

/// A document entering the flow at a status other than the one it starts at.
pub struct StatusEntryRule;

impl Rule for StatusEntryRule {
    fn id(&self) -> &str {
        "status_entry"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "A document of a kind `statuses.flow` governs enters at the flow's entry status and \
         reaches every other status by a declared transition, judged a step at a time; needs \
         a git work tree"
    }

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "kinds".into(),
            json!(config.status_flow().map(|flow| &flow.kinds)),
        );
        m.insert(
            "initial".into(),
            json!(
                config
                    .status_flow()
                    .and_then(|flow| flow.initial.as_deref())
            ),
        );
        m
    }

    fn judges_steps(&self) -> bool {
        true
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        ctx.steps.is_some()
    }

    fn skip_reason(&self, ctx: &RuleContext<'_>) -> String {
        skip_reason(ctx)
    }

    /// Every finding is a step inside the range the run was asked about,
    /// including an entry a later step removed again.
    fn touched_by(&self, _ctx: &RuleContext<'_>, _since: &Touched, _violation: &Violation) -> bool {
        true
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let (Some(steps), Some(flow)) = (ctx.steps, ctx.config.status_flow()) else {
            return RuleRun::clean(0);
        };
        // Every record a step holds under the flow is asked whether it entered
        // there, and one a parent already held has its answer. One arriving
        // where a parent could not be read has none either way.
        let mut judged = BTreeSet::new();
        let mut unknown = BTreeSet::new();
        let mut violations = Vec::new();
        for (step, id, now, priors) in governed(steps, flow) {
            if priors.is_empty() && !step.priors_known() {
                unknown.insert(id);
                continue;
            }
            judged.insert(id);
            if !priors.is_empty() {
                continue;
            }
            let initial = ctx.config.initial_status_for(&now.kind);
            if now.status == initial {
                continue;
            }
            violations.push(Violation::new(
                self.id().to_string(),
                self.severity(),
                Some(id.to_string()),
                Some(now.path.clone()),
                ViolationDetails::StatusEntry {
                    status: now.status.clone(),
                    initial: initial.to_string(),
                    commit: step.commit.clone(),
                },
            ));
        }
        let stood_over = selected(ctx.graph, flow);
        RuleRun::new(judged.len(), violations).unjudged(unjudged(stood_over, &judged, unknown))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ancestry::Positions;
    use crate::config::Config;
    use crate::model::{Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn config() -> Config {
        toml::from_str(
            r#"
[kinds]
allowed = ["adr", "runbook", "generic"]

[statuses]
allowed = ["proposed", "active", "superseded", "archived"]
terminal = ["superseded", "archived"]
initial = "proposed"

[statuses.flow]
kinds = ["adr"]
transitions = { proposed = ["active", "archived"], active = ["superseded", "archived"] }
"#,
        )
        .expect("parses")
    }

    fn node(id: &str, kind: &str, status: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.into(),
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
            body_structure: Default::default(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn adr(id: &str, status: &str) -> Node {
        node(id, "adr", status)
    }

    fn graph(nodes: &[Node]) -> Graph {
        let map: IndexMap<String, Node> = nodes.iter().map(|n| (n.id.clone(), n.clone())).collect();
        Graph::new(
            map,
            vec![],
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    fn snapshot(nodes: &[Node]) -> Arc<Positions> {
        Arc::new(Positions::of(&graph(nodes)))
    }

    /// A snapshot holding a document whose record is unknown: one a shallow
    /// clone cannot read back past its cut.
    fn cut(nodes: &[Node], path: &str) -> Arc<Positions> {
        let map: IndexMap<String, Node> = nodes.iter().map(|n| (n.id.clone(), n.clone())).collect();
        let broken = Graph::new(
            map,
            vec![],
            vec![],
            vec![],
            vec![crate::model::ParseFailure {
                path: path.to_string(),
                message: "unreadable".into(),
                content_hash: String::new(),
            }],
            crate::model::GraphMeta::default(),
        );
        Arc::new(Positions::of(&broken))
    }

    fn step(commit: &str, parents: &[&[Node]], child: &[Node]) -> Step {
        Step {
            commit: Some(commit.into()),
            parents: parents.iter().map(|nodes| snapshot(nodes)).collect(),
            child: snapshot(child),
        }
    }

    fn run(rule: &dyn Rule, steps: &[Step]) -> RuleRun {
        run_at(rule, &graph(&[]), steps)
    }

    fn run_at(rule: &dyn Rule, graph: &Graph, steps: &[Step]) -> RuleRun {
        let config = config();
        rule.check(&RuleContext {
            today: crate::test_today(),
            graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(std::path::Path::new(".")),
            history: &crate::rules::UNMEASURED,
            since: None,
            steps: Some(steps),
        })
    }

    #[test]
    fn a_declared_move_passes() {
        let steps = [step(
            "c1",
            &[&[adr("a", "proposed")]],
            &[adr("a", "active")],
        )];
        let moved = run(&StatusTransitionRule, &steps);
        assert!(moved.violations.is_empty(), "{:?}", moved.violations);
        assert_eq!(moved.subjects, 1);
    }

    #[test]
    fn a_move_the_flow_does_not_name_is_refused_where_it_was_made() {
        // The demotion that would otherwise disarm a status-armed body lock:
        // legal frontmatter, no declared transition.
        let steps = [step(
            "c1",
            &[&[adr("a", "active")]],
            &[adr("a", "proposed")],
        )];
        let moved = run(&StatusTransitionRule, &steps);
        assert_eq!(moved.violations.len(), 1);
        assert!(matches!(
            &moved.violations[0].details,
            ViolationDetails::StatusTransition { from, to, declared, commit }
                if from == "active" && to == "proposed"
                    && declared == &["superseded".to_string(), "archived".to_string()]
                    && commit.as_deref() == Some("c1")
        ));
    }

    #[test]
    fn leaving_a_terminal_status_is_refused() {
        let steps = [step(
            "c1",
            &[&[adr("a", "superseded")]],
            &[adr("a", "active")],
        )];
        assert_eq!(run(&StatusTransitionRule, &steps).violations.len(), 1);
    }

    #[test]
    fn a_record_entering_at_the_initial_status_passes_and_past_it_is_refused() {
        let steps = [step(
            "c1",
            &[&[]],
            &[adr("a", "proposed"), adr("b", "active")],
        )];
        let entered = run(&StatusEntryRule, &steps);
        assert_eq!(entered.subjects, 2);
        assert_eq!(entered.violations.len(), 1);
        assert!(matches!(
            &entered.violations[0].details,
            ViolationDetails::StatusEntry { status, initial, commit }
                if status == "active" && initial == "proposed" && commit.as_deref() == Some("c1")
        ));
    }

    #[test]
    fn a_record_authored_then_accepted_is_two_legal_steps_and_not_one_arrival() {
        // The same two snapshots read as one step are a record arriving
        // accepted, which is what an endpoint diff over the range reported.
        let steps = [
            step("c1", &[&[]], &[adr("a", "proposed")]),
            step("c2", &[&[adr("a", "proposed")]], &[adr("a", "active")]),
        ];
        assert!(run(&StatusEntryRule, &steps).violations.is_empty());
        assert!(run(&StatusTransitionRule, &steps).violations.is_empty());

        let folded = [step("range", &[&[]], &[adr("a", "active")])];
        assert_eq!(run(&StatusEntryRule, &folded).violations.len(), 1);
    }

    #[test]
    fn a_detour_ending_where_a_declared_move_would_is_refused_at_the_detour() {
        // proposed → archived → active: the endpoints read proposed → active,
        // which the flow declares; the second step leaves a terminal status.
        let steps = [
            step("c1", &[&[adr("a", "proposed")]], &[adr("a", "archived")]),
            step("c2", &[&[adr("a", "archived")]], &[adr("a", "active")]),
        ];
        let moved = run(&StatusTransitionRule, &steps);
        assert_eq!(moved.violations.len(), 1);
        assert!(matches!(
            &moved.violations[0].details,
            ViolationDetails::StatusTransition { from, commit, .. }
                if from == "archived" && commit.as_deref() == Some("c2")
        ));
    }

    #[test]
    fn a_merge_that_takes_a_side_whole_introduces_nothing() {
        // The mainline accepted the record; the branch merging it in still
        // held it at proposed. The acceptance is the mainline commit's step.
        let steps = [step(
            "merge",
            &[&[adr("a", "proposed")], &[adr("a", "active")]],
            &[adr("a", "active")],
        )];
        assert!(run(&StatusTransitionRule, &steps).violations.is_empty());
        assert!(run(&StatusEntryRule, &steps).violations.is_empty());
    }

    #[test]
    fn a_merge_resolving_to_a_status_is_legal_when_either_line_declares_the_move() {
        let declared_by_one = [step(
            "merge",
            &[&[adr("a", "proposed")], &[adr("a", "active")]],
            &[adr("a", "superseded")],
        )];
        assert!(
            run(&StatusTransitionRule, &declared_by_one)
                .violations
                .is_empty()
        );

        let declared_by_neither = [step(
            "merge",
            &[&[adr("a", "archived")], &[adr("a", "superseded")]],
            &[adr("a", "active")],
        )];
        let moved = run(&StatusTransitionRule, &declared_by_neither);
        assert_eq!(moved.violations.len(), 2, "one per status it left");
    }

    #[test]
    fn a_record_carried_in_by_a_merge_is_not_an_arrival() {
        let steps = [step(
            "merge",
            &[&[], &[adr("a", "active")]],
            &[adr("a", "active")],
        )];
        assert!(run(&StatusEntryRule, &steps).violations.is_empty());
    }

    #[test]
    fn a_kind_the_flow_does_not_govern_is_judged_by_neither_rule() {
        let steps = [step(
            "c1",
            &[&[node("r", "runbook", "active")]],
            &[
                node("r", "runbook", "proposed"),
                node("s", "runbook", "active"),
            ],
        )];
        let entered = run(&StatusEntryRule, &steps);
        let moved = run(&StatusTransitionRule, &steps);
        assert!(entered.violations.is_empty() && moved.violations.is_empty());
        assert_eq!(
            (entered.subjects, moved.subjects),
            (0, 0),
            "a kind with no lifecycle is not guarded"
        );
    }

    #[test]
    fn taking_a_governed_kind_enters_the_flow() {
        // Reclassifying an accepted runbook as an ADR, or laundering an ADR
        // through an ungoverned kind and back, puts a record past the entry
        // no step established it passed.
        let steps = [step(
            "c1",
            &[&[node("a", "generic", "active")]],
            &[adr("a", "active")],
        )];
        assert_eq!(run(&StatusEntryRule, &steps).violations.len(), 1);
        assert!(run(&StatusTransitionRule, &steps).violations.is_empty());
    }

    #[test]
    fn leaving_the_governed_kinds_is_judged_by_neither_rule() {
        let steps = [step(
            "c1",
            &[&[adr("a", "proposed")]],
            &[node("a", "generic", "superseded")],
        )];
        assert!(run(&StatusEntryRule, &steps).violations.is_empty());
        assert!(run(&StatusTransitionRule, &steps).violations.is_empty());
    }

    #[test]
    fn a_record_arriving_under_a_new_id_is_an_arrival() {
        // A document moved or re-keyed outside nodex: nothing links the
        // arriving record to the departed one.
        let steps = [step(
            "c1",
            &[&[adr("old", "active")]],
            &[adr("new", "active")],
        )];
        assert_eq!(run(&StatusEntryRule, &steps).violations.len(), 1);
    }

    #[test]
    fn a_record_that_only_entered_is_unjudged_by_the_transition_rule() {
        let steps = [
            step("c1", &[&[]], &[adr("a", "proposed")]),
            step(
                "c2",
                &[&[adr("a", "proposed")]],
                &[adr("a", "active"), adr("b", "proposed")],
            ),
        ];
        let moved = run(&StatusTransitionRule, &steps);
        assert_eq!(moved.subjects, 1, "a, judged at c2");
        assert_eq!(moved.unjudged, 1, "b, which no step held before");
        assert_eq!(run(&StatusEntryRule, &steps).subjects, 2);
    }

    #[test]
    fn the_entry_rule_guards_every_governed_record_on_a_clean_step() {
        // Nothing entered, and every record the flow governs was asked: a
        // zero here would read as a rule in effect over nothing.
        let steps = [step(
            "wt",
            &[&[adr("a", "active"), adr("b", "proposed")]],
            &[adr("a", "active"), adr("b", "proposed")],
        )];
        let entered = run(&StatusEntryRule, &steps);
        assert!(entered.violations.is_empty());
        assert_eq!((entered.subjects, entered.unjudged), (2, 0));
    }

    #[test]
    fn a_record_arriving_where_a_parent_could_not_be_read_is_unjudged() {
        // A shallow clone cannot say what the document it could not parse
        // held before its cut, so the record standing there may be this one
        // returning rather than a new one entering.
        let steps = [Step {
            commit: None,
            parents: vec![cut(&[adr("a", "proposed")], "adr-broken.md")],
            child: snapshot(&[adr("a", "proposed"), adr("b", "active")]),
        }];
        let entered = run(&StatusEntryRule, &steps);
        let moved = run(&StatusTransitionRule, &steps);
        assert!(entered.violations.is_empty(), "{:?}", entered.violations);
        assert_eq!((entered.subjects, entered.unjudged), (1, 1));
        assert_eq!((moved.subjects, moved.unjudged), (1, 1));
    }

    #[test]
    fn a_record_the_cut_leaves_unknown_stays_unjudged_where_a_later_step_judges_it() {
        // A count is all an unjudged record leaves behind, so a later step
        // holding the same id must not take it back.
        let steps = [
            Step {
                commit: Some("c1".into()),
                parents: vec![cut(&[], "adr-a.md")],
                child: snapshot(&[adr("a", "active")]),
            },
            step("c2", &[&[adr("a", "active")]], &[adr("a", "superseded")]),
        ];
        let entered = run(&StatusEntryRule, &steps);
        let moved = run(&StatusTransitionRule, &steps);
        assert!(entered.violations.is_empty() && moved.violations.is_empty());
        assert_eq!((entered.subjects, entered.unjudged), (1, 1));
        assert_eq!((moved.subjects, moved.unjudged), (1, 1));
    }

    #[test]
    fn a_governed_record_no_step_holds_is_unjudged_by_both_rules() {
        // A document git ignores stays in the graph and out of every step.
        let tracked = [adr("a", "active"), adr("b", "proposed")];
        let steps = [step("wt", &[&tracked], &tracked)];
        let now = graph(&[
            adr("a", "active"),
            adr("b", "proposed"),
            adr("p", "active"),
            node("r", "runbook", "active"),
        ]);
        let entered = run_at(&StatusEntryRule, &now, &steps);
        let moved = run_at(&StatusTransitionRule, &now, &steps);
        assert!(entered.violations.is_empty() && moved.violations.is_empty());
        assert_eq!((entered.subjects, entered.unjudged), (2, 1));
        assert_eq!((moved.subjects, moved.unjudged), (2, 1));
    }
}
