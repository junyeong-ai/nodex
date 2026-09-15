//! Lock document bodies against edits.
//!
//! Diff-aware: needs a "before" snapshot to compare body fingerprints
//! against, supplied by `--since <ref>` or, by default,
//! `rules.immutable_baseline`. Without one the rule reports itself as
//! non-applicable via [`Rule::is_applicable`] rather than silently
//! passing — `.claude/rules/config-driven.md` ("No silent runtime skips").
//!
//! Two modes:
//!
//! - [`BodyImmutableMode::Frozen`]: any body change fires a
//!   violation. The natural mode for ADRs / contracts / signed-off
//!   specs — the body is the decision, and the decision does not
//!   move once shipped.
//! - [`BodyImmutableMode::AppendOnly`]: the locked body must remain a
//!   prefix of the new body. Suits log-shaped documents where new
//!   entries land at the bottom but earlier entries are never
//!   re-litigated. With `append_section`, growth is confined to the
//!   section that heading opens, which must end the body, and may not
//!   change how a committed line reads: the policy for a record that takes
//!   corrections while everything committed above them stays as it was.
//!
//! Two triggers ([`ImmutableTrigger`]):
//!
//! - `terminal` (default): the lock engages once the before-snapshot
//!   status is terminal.
//! - `creation`: the lock engages as soon as a prior committed
//!   snapshot exists, regardless of status — the creating commit is
//!   structurally exempt because the diff layer only emits a body
//!   change for nodes present in both snapshots.
//!
//! A `creation` block deliberately freezes the body while frontmatter
//! (including `status`) stays editable — supersession metadata moves,
//! the record does not. Guard policy: only locks that can *never*
//! fire correctly are refused at load (`frontmatter_immutable` rejects
//! `id` on that basis); a creation body lock fires exactly as
//! declared, so it is configuration, not a mistake — do not add a
//! load-time guard against it.
//!
//! The rule reads body fingerprints (`body_hash`, `body_lines_hash`,
//! `body_structure`) off [`crate::diff::BodyChange`] entries the diff
//! layer already computed — it never touches the filesystem, never
//! re-parses, and never reads body text. That keeps every check-time rule a pure
//! function of `(graph, config)`, same discipline schema /
//! frontmatter_immutable / body_line already follow.

use serde_json::{Map, Value, json};

use crate::config::{BodyImmutableMode, BodyImmutableRuleConfig, ImmutableTrigger};
use crate::diff::BodyChange;
use crate::model::SectionHeading;

use super::{
    AppendRefusal, Rule, RuleContext, RuleRun, RuleSource, Severity, SubjectUnit, Violation,
    ViolationDetails, detail::Evidence,
};

/// One `[[rules.body_immutable]]` block as a `Rule` trait object.
pub struct BodyImmutableRule {
    config: BodyImmutableRuleConfig,
    qualified_id: String,
    append_section: Option<SectionHeading>,
}

impl BodyImmutableRule {
    /// Construct a rule instance for one config block. `qualified_id`
    /// is cached so [`Rule::id`] returns `&str` without allocating
    /// per call — same convention as [`crate::rules::body_line::BodyLineRule`].
    pub fn new(config: BodyImmutableRuleConfig) -> Self {
        let qualified_id = format!("body_immutable/{}", config.name);
        let append_section = config.append_section.as_deref().map(|spelling| {
            crate::parser::body::parse_heading(spelling).expect("validated by Config::load")
        });
        Self {
            config,
            qualified_id,
            append_section,
        }
    }
}

impl Rule for BodyImmutableRule {
    fn id(&self) -> &str {
        &self.qualified_id
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Document bodies are locked once the block's trigger engages — \
         `terminal` locks at terminal status, `creation` locks once a \
         prior committed snapshot exists; `frozen` rejects any change, \
         `append_only` rejects non-prefix changes and, with `append_section`, \
         anything appended outside that closing section. Needs a diff \
         context from `--since <ref>` or `rules.immutable_baseline`"
    }

    fn source(&self) -> RuleSource {
        RuleSource::Config
    }

    fn params(&self, _config: &crate::config::Config) -> Map<String, Value> {
        // Per-block params mirror the config's public surface so the
        // manifest entry is self-describing — same shape body_line
        // uses for its params payload.
        let mut m = Map::new();
        m.insert(
            "mode".into(),
            json!(match self.config.mode {
                BodyImmutableMode::Frozen => "frozen",
                BodyImmutableMode::AppendOnly => "append_only",
            }),
        );
        m.insert(
            "trigger".into(),
            json!(match self.config.trigger {
                ImmutableTrigger::Terminal => "terminal",
                ImmutableTrigger::Creation => "creation",
            }),
        );
        m.insert("kinds".into(), json!(self.config.kinds));
        m.insert("append_section".into(), json!(self.config.append_section));
        m
    }

    fn diff_aware(&self) -> bool {
        true
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        // The block exists by construction (`registered_rules` only
        // instantiates this rule when the user authored the block).
        // The remaining gate is the diff context — body immutability
        // is meaningless without a "before" snapshot.
        ctx.since.is_some()
    }

    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        "no diff context — set `--since <ref>` or `rules.immutable_baseline`".to_string()
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let Some(diff) = ctx.since else {
            return RuleRun::clean(0);
        };
        // The records the lock is armed over — every one whose body it would
        // refuse an edit to, not the few that were edited this run. A lock
        // protecting hundreds of frozen records and a lock protecting none
        // both see an empty diff on a clean tree, and the standing reach is
        // what says which of the two this is. Read in the baseline's frame,
        // because that is the frame the verdict below judges in: a record
        // that has since left terminal is one this lock was armed over and
        // someone moved anyway, so it is the first thing the population must
        // contain, not the one thing it would drop. A record the baseline
        // holds no node for has no frame to be read in and no channel that
        // could reach it, so it is not in the population however terminal it
        // looks now — counted apart, and selected on what it looks like now
        // because that is the only frame such a record has.
        let unbacked = diff.added_ids();
        let (subjects, unjudged) = ctx.graph.nodes().values().fold((0, 0), |(kept, lost), n| {
            let selected =
                super::kind_allowed(&self.config.kinds, diff.before_kind(&n.id, n.kind.as_str()))
                    && match self.config.trigger {
                        ImmutableTrigger::Terminal => ctx
                            .config
                            .is_terminal(diff.before_status(&n.id, n.status.as_str())),
                        ImmutableTrigger::Creation => true,
                    };
            match (selected, unbacked.contains(n.id.as_str())) {
                (true, false) => (kept + 1, lost),
                (true, true) => (kept, lost + 1),
                (false, _) => (kept, lost),
            }
        });
        let mut violations = Vec::new();
        for change in &diff.body_changes {
            let Some(node) = ctx.graph.node(&change.id) else {
                continue;
            };
            // The status the lock keys on is the *before*-snapshot one,
            // not the after — an edit that un-terminalizes the doc in
            // the same commit must still report the status that armed
            // the lock, mirroring `frontmatter_immutable`.
            let before_status = diff.before_status(&change.id, node.status.as_str());
            match self.config.trigger {
                // The lock applies to a body that was *already* terminal
                // before this edit, judged against the before snapshot —
                // same convention `frontmatter_immutable` uses, so the two
                // rules report on the same boundary. This lets the single
                // write that first drives a doc terminal finalise its body
                // in the same edit without being rejected.
                ImmutableTrigger::Terminal => {
                    if !ctx.config.is_terminal(before_status) {
                        continue;
                    }
                }
                // A prior committed snapshot exists by construction:
                // `body_changes` only carries nodes present in both
                // snapshots, so the creating commit never reaches here.
                ImmutableTrigger::Creation => {}
            }
            if !super::kind_allowed(
                &self.config.kinds,
                diff.before_kind(&change.id, node.kind.as_str()),
            ) {
                continue;
            }

            let refusal = match self.config.mode {
                BodyImmutableMode::Frozen => None,
                BodyImmutableMode::AppendOnly => match self.append_refusal(change) {
                    Some(refusal) => Some(refusal),
                    None => continue,
                },
            };

            // The typed payload names the engaged trigger so the operator
            // (or agent) sees exactly which lock fired — a creation lock on
            // an `active` doc must never claim the status was terminal, and
            // a terminal lock reports the before-status it keyed on rather
            // than an after-status that may have moved in the same edit.
            let (before_status, current_status) = match self.config.trigger {
                ImmutableTrigger::Terminal => (Some(before_status.to_string()), None),
                ImmutableTrigger::Creation => (None, Some(node.status.as_str().to_string())),
            };
            // append_only reports the body sizes it compared; frozen has no
            // size to report.
            let (before_lines, after_lines) = match self.config.mode {
                BodyImmutableMode::Frozen => (None, None),
                BodyImmutableMode::AppendOnly => (
                    Some(change.before_lines_hash.len()),
                    Some(change.after_lines_hash.len()),
                ),
            };

            violations.push(Violation::new(
                self.qualified_id.clone(),
                Severity::Error,
                Some(change.id.clone()),
                Some(crate::path_guard::forward_string(&node.path)),
                ViolationDetails::BodyImmutable {
                    trigger: self.config.trigger,
                    mode: self.config.mode,
                    before_status,
                    current_status,
                    before_lines: before_lines.map(Evidence),
                    after_lines: after_lines.map(Evidence),
                    append_section: self.config.append_section.clone(),
                    refusal,
                },
            ));
        }
        RuleRun::new(subjects, violations).unjudged(unjudged)
    }
}

impl BodyImmutableRule {
    /// Why this `append_only` block refuses `change`, or `None` when the mode
    /// permits it: the prior body preserved verbatim as a prefix and, under
    /// `append_section`, everything appended inside that section and leaving
    /// every committed reference resolving as it did.
    fn append_refusal(&self, change: &BodyChange) -> Option<AppendRefusal> {
        if !change
            .after_lines_hash
            .starts_with(&change.before_lines_hash)
        {
            return Some(AppendRefusal::Rewritten);
        }
        let section = self.append_section.as_ref()?;
        if !appends_inside(section, change) {
            Some(AppendRefusal::OutsideSection)
        } else if redefines_reference(change) {
            Some(AppendRefusal::RedefinesReference)
        } else {
            None
        }
    }
}

/// Whether every non-blank line `change` appends to a body it keeps as a
/// prefix falls inside the section `heading` opens, with nothing after that
/// section at its level or above.
///
/// The section either already ends the committed body or is opened by the
/// appended lines. A heading within the committed lines counts only where the
/// committed body already read one: appended beneath a committed paragraph
/// line, a setext underline would otherwise make frozen text the heading.
fn appends_inside(heading: &SectionHeading, change: &BodyChange) -> bool {
    let committed = change.before_lines_hash.len();
    let sections = &change.after_structure.sections;
    let closing = sections
        .iter()
        .rposition(|s| s.heading.as_ref().is_some_and(|h| h.level <= heading.level))
        .filter(|&i| {
            let section = &sections[i];
            section.heading.as_ref() == Some(heading)
                && (section.start >= committed
                    || change
                        .before_structure
                        .sections
                        .iter()
                        .any(|b| b.start == section.start && b.heading == section.heading))
        });
    sections[..closing.unwrap_or(sections.len())]
        .iter()
        .all(|s| s.content_end <= committed)
}

/// Whether an appended line belongs to a link reference definition that a
/// reference on a committed line resolves to. A definition applies to
/// references anywhere in the document, so such a line changes what a
/// committed line says without touching it.
fn redefines_reference(change: &BodyChange) -> bool {
    let committed = change.before_lines_hash.len();
    change
        .after_structure
        .definitions
        .iter()
        .any(|definition| definition.first_use < committed && definition.end > committed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BodyImmutableMode, BodyImmutableRuleConfig, Config};
    use crate::diff::{BodyChange, GraphDiff};
    use crate::model::{Graph, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_node(id: &str, status: &str, kind: &str) -> Node {
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

    fn build_graph(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
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

    fn cfg(mode: BodyImmutableMode, kinds: Vec<&str>) -> Config {
        let mut c = Config::default();
        c.statuses.terminal = vec!["superseded".into()];
        // Ensure any kinds named by the test are allowed.
        for k in &kinds {
            if !c.kinds.allowed.iter().any(|a| a == k) {
                c.kinds.allowed.push((*k).into());
            }
        }
        c.rules.body_immutable = vec![BodyImmutableRuleConfig {
            name: "body".into(),
            mode,
            trigger: ImmutableTrigger::Terminal,
            kinds: kinds.iter().map(|k| (*k).into()).collect(),
            append_section: None,
        }];
        c
    }

    fn diff_with(changes: Vec<BodyChange>) -> GraphDiff {
        GraphDiff {
            added_nodes: vec![],
            removed_nodes: vec![],
            added_edges: vec![],
            removed_edges: vec![],
            status_transitions: vec![],
            path_changes: vec![],
            field_changes: vec![],
            added_annotations: vec![],
            removed_annotations: vec![],
            body_changes: changes,
        }
    }

    fn body_change(id: &str, before: &[&str], after: &[&str]) -> BodyChange {
        BodyChange {
            id: id.into(),
            before_hash: format!("h-before-{}", id),
            after_hash: format!("h-after-{}", id),
            before_lines_hash: before.iter().map(|s| (*s).to_string()).collect(),
            after_lines_hash: after.iter().map(|s| (*s).to_string()).collect(),
            before_structure: Default::default(),
            after_structure: Default::default(),
        }
    }

    fn ctx<'a>(
        graph: &'a Graph,
        config: &'a Config,
        diff: Option<&'a GraphDiff>,
    ) -> RuleContext<'a> {
        RuleContext {
            today: crate::test_today(),
            graph,
            config,
            files: crate::builder::scanner::ProjectFiles::working_tree(std::path::Path::new(".")),
            history: &crate::rules::UNMEASURED,
            since: diff,
        }
    }

    fn rule_for(config: &Config) -> BodyImmutableRule {
        BodyImmutableRule::new(config.rules.body_immutable[0].clone())
    }

    // ─── applicability ─────────────────────────────────────────────────

    #[test]
    fn rule_id_is_qualified_with_block_name() {
        let cfg = cfg(BodyImmutableMode::Frozen, vec![]);
        let rule = rule_for(&cfg);
        assert_eq!(rule.id(), "body_immutable/body");
    }

    #[test]
    fn inert_without_since_ref() {
        // Diff-aware rules without a diff context must surface as
        // skipped, never as silent passes — same convention
        // frontmatter_immutable already follows.
        let config = cfg(BodyImmutableMode::Frozen, vec![]);
        let graph = build_graph(vec![make_node("a", "superseded", "generic")]);
        let rule = rule_for(&config);
        assert!(!rule.is_applicable(&ctx(&graph, &config, None)));
        assert!(
            rule.skip_reason(&ctx(&graph, &config, None))
                .contains("--since"),
            "skip reason must mention --since so operators know how to activate the rule"
        );
    }

    // ─── frozen mode ───────────────────────────────────────────────────

    #[test]
    fn frozen_fires_on_any_change_when_status_terminal() {
        let config = cfg(BodyImmutableMode::Frozen, vec![]);
        let graph = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_with(vec![body_change("a", &["l1"], &["l1-mod"])]);
        let rule = rule_for(&config);
        let v = rule.check(&ctx(&graph, &config, Some(&d))).violations;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "body_immutable/body");
        assert_eq!(v[0].node_id.as_deref(), Some("a"));
        assert!(v[0].message.contains("frozen"));
    }

    #[test]
    fn frozen_terminal_message_reports_before_status_not_after() {
        // A commit that both un-terminalizes the doc (superseded →
        // active) and edits its body still fires — the lock keys on the
        // *before* status — and the message must report that
        // before-status, never the after one, so it can't claim
        // "terminal" while showing a non-terminal value.
        let config = cfg(BodyImmutableMode::Frozen, vec![]);
        // The graph carries the AFTER node (active); the diff records
        // the superseded → active transition.
        let graph = build_graph(vec![make_node("a", "active", "generic")]);
        let mut d = diff_with(vec![body_change("a", &["l1"], &["l1-mod"])]);
        d.status_transitions.push(crate::diff::StatusTransition {
            id: "a".into(),
            from: "superseded".into(),
            to: "active".into(),
        });
        let rule = rule_for(&config);
        let run = rule.check(&ctx(&graph, &config, Some(&d)));
        // The record left terminal, so the graph's own status no longer says
        // this lock was ever armed over it. The verdict judges in the
        // baseline's frame and the reach must too, or the one body this lock
        // caught is the one body it claims never to have guarded.
        assert_eq!(run.subjects, 1);
        let v = run.violations;
        assert_eq!(v.len(), 1, "before-status terminal must fire");
        assert!(
            v[0].message.contains("was: \"superseded\""),
            "message reports the before-status the lock keyed on: {}",
            v[0].message
        );
        assert!(
            !v[0].message.contains("active"),
            "message must not surface the non-terminal after-status: {}",
            v[0].message
        );
    }

    #[test]
    fn frozen_silent_when_status_not_terminal() {
        // Pre-terminal documents are still drafts — edits are
        // expected. Same boundary frontmatter_immutable uses.
        let config = cfg(BodyImmutableMode::Frozen, vec![]);
        let graph = build_graph(vec![make_node("a", "active", "generic")]);
        let d = diff_with(vec![body_change("a", &["l1"], &["l1-mod"])]);
        let rule = rule_for(&config);
        assert!(
            rule.check(&ctx(&graph, &config, Some(&d)))
                .violations
                .is_empty(),
            "edits to a non-terminal document must not fire body_immutable"
        );
    }

    // ─── append_only mode ──────────────────────────────────────────────

    #[test]
    fn append_only_allows_strict_appends_at_end() {
        // The pre-terminal body is preserved verbatim; new lines
        // sit only at the tail. This is the success path for log /
        // changelog / decision-journal documents.
        let config = cfg(BodyImmutableMode::AppendOnly, vec![]);
        let graph = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_with(vec![body_change("a", &["l1", "l2"], &["l1", "l2", "l3"])]);
        let rule = rule_for(&config);
        assert!(
            rule.check(&ctx(&graph, &config, Some(&d)))
                .violations
                .is_empty(),
            "exact prefix + new tail entries must satisfy append_only"
        );
    }

    #[test]
    fn append_only_fires_on_middle_edit() {
        // An edit in the middle breaks the prefix relation even if
        // the body grew overall — the rule guards against "I'll just
        // rewrite the second line" sneaking past with extra padding.
        let config = cfg(BodyImmutableMode::AppendOnly, vec![]);
        let graph = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_with(vec![body_change(
            "a",
            &["l1", "l2", "l3"],
            &["l1", "l2-MOD", "l3", "l4"],
        )]);
        let rule = rule_for(&config);
        let v = rule.check(&ctx(&graph, &config, Some(&d))).violations;
        assert_eq!(v.len(), 1);
        assert!(v[0].message.contains("append_only"));
        assert!(v[0].message.contains("prefix"));
    }

    #[test]
    fn append_only_fires_on_deletion() {
        // Shrinking the body cannot satisfy "before is a prefix of
        // after" — the rule fires even though no line was rewritten.
        let config = cfg(BodyImmutableMode::AppendOnly, vec![]);
        let graph = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_with(vec![body_change("a", &["l1", "l2"], &["l1"])]);
        let rule = rule_for(&config);
        let v = rule.check(&ctx(&graph, &config, Some(&d))).violations;
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn append_only_fires_on_first_line_replacement() {
        // The first line changing is a strict-prefix violation, even
        // when the file length is unchanged.
        let config = cfg(BodyImmutableMode::AppendOnly, vec![]);
        let graph = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_with(vec![body_change("a", &["l1", "l2"], &["l1-MOD", "l2"])]);
        let rule = rule_for(&config);
        assert_eq!(
            rule.check(&ctx(&graph, &config, Some(&d))).violations.len(),
            1
        );
    }

    #[test]
    fn append_only_allows_first_content_when_before_was_empty() {
        // A document that started empty pre-terminal (theoretical
        // edge case — terminal documents typically have content)
        // and grows after must satisfy the rule. An empty `before`
        // is a prefix of any `after`.
        let config = cfg(BodyImmutableMode::AppendOnly, vec![]);
        let graph = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_with(vec![body_change("a", &[], &["l1"])]);
        let rule = rule_for(&config);
        assert!(
            rule.check(&ctx(&graph, &config, Some(&d)))
                .violations
                .is_empty()
        );
    }

    // ─── creation trigger ──────────────────────────────────────────────

    fn cfg_creation(mode: BodyImmutableMode, kinds: Vec<&str>) -> Config {
        let mut c = cfg(mode, kinds);
        c.rules.body_immutable[0].trigger = ImmutableTrigger::Creation;
        c
    }

    #[test]
    fn creation_fires_on_non_terminal_document() {
        // The gap the terminal trigger leaves open: an `active` ADR's
        // body edit. With trigger = creation the record is frozen from
        // its first committed snapshot onward, status notwithstanding.
        let config = cfg_creation(BodyImmutableMode::Frozen, vec![]);
        let graph = build_graph(vec![make_node("a", "active", "generic")]);
        let d = diff_with(vec![body_change("a", &["l1"], &["l1-mod"])]);
        let rule = rule_for(&config);
        let v = rule.check(&ctx(&graph, &config, Some(&d))).violations;
        assert_eq!(
            v.len(),
            1,
            "creation trigger must fire regardless of status"
        );
        assert_eq!(v[0].node_id.as_deref(), Some("a"));
        // The message must name the engaged trigger — a creation lock
        // on an `active` doc must never claim the status was terminal.
        assert!(
            v[0].message.contains("locked from creation"),
            "message names the creation trigger: {}",
            v[0].message
        );
        assert!(
            !v[0].message.contains("status is terminal"),
            "message must not claim a terminal status: {}",
            v[0].message
        );
    }

    #[test]
    fn creation_exempts_first_appearance_by_construction() {
        // A document present only in the after snapshot produces an
        // `added_nodes` entry, never a `body_changes` entry — the
        // creating commit cannot fire the lock. Pin the contract here:
        // an empty body_changes list yields no violations even with
        // the creation trigger armed.
        let config = cfg_creation(BodyImmutableMode::Frozen, vec![]);
        let graph = build_graph(vec![make_node("a", "active", "generic")]);
        let d = diff_with(vec![]); // creating commit: no intersection entry
        let rule = rule_for(&config);
        assert!(
            rule.check(&ctx(&graph, &config, Some(&d)))
                .violations
                .is_empty()
        );
    }

    #[test]
    fn creation_append_only_allows_appends_rejects_edits() {
        // The two axes compose: a changelog frozen-from-creation in
        // shape but append-only in mode grows forever, rewrites never.
        let config = cfg_creation(BodyImmutableMode::AppendOnly, vec![]);
        let graph = build_graph(vec![make_node("a", "active", "generic")]);
        let rule = rule_for(&config);

        let append = diff_with(vec![body_change("a", &["l1"], &["l1", "l2"])]);
        assert!(
            rule.check(&ctx(&graph, &config, Some(&append)))
                .violations
                .is_empty()
        );

        let edit = diff_with(vec![body_change("a", &["l1"], &["l1-mod"])]);
        assert_eq!(
            rule.check(&ctx(&graph, &config, Some(&edit)))
                .violations
                .len(),
            1
        );
    }

    #[test]
    fn trigger_defaults_to_terminal_when_omitted() {
        // serde default: an existing block without `trigger` keeps the
        // terminal semantic — and the explicit spelling parses to the
        // same value.
        let omitted: BodyImmutableRuleConfig =
            toml::from_str("name = \"b\"\nmode = \"frozen\"\n").expect("parses");
        assert_eq!(omitted.trigger, ImmutableTrigger::Terminal);
        let explicit: BodyImmutableRuleConfig =
            toml::from_str("name = \"b\"\nmode = \"frozen\"\ntrigger = \"creation\"\n")
                .expect("parses");
        assert_eq!(explicit.trigger, ImmutableTrigger::Creation);
    }

    #[test]
    fn params_carry_trigger() {
        let config = cfg_creation(BodyImmutableMode::Frozen, vec![]);
        let rule = rule_for(&config);
        let params = rule.params(&config);
        assert_eq!(params.get("trigger"), Some(&serde_json::json!("creation")));
        let terminal_cfg = cfg(BodyImmutableMode::Frozen, vec![]);
        let params = rule_for(&terminal_cfg).params(&terminal_cfg);
        assert_eq!(params.get("trigger"), Some(&serde_json::json!("terminal")));
    }

    // ─── append_section ────────────────────────────────────────────────

    const DECISION: &str = "# One\n\n## Decision\n\nWe do X.\n";

    fn cfg_section(section: &str) -> Config {
        let mut c = cfg_creation(BodyImmutableMode::AppendOnly, vec![]);
        c.rules.body_immutable[0].append_section = Some(section.into());
        c
    }

    /// Why a `## Corrections` block refuses an edit from `before` to
    /// `after`, with both bodies parsed and diffed the way a build would;
    /// `None` when it admits the edit.
    fn refusal(before: &str, after: &str) -> Option<AppendRefusal> {
        let doc = |body: &str| {
            let (mut node, _) =
                crate::parser::frontmatter::parse_frontmatter(std::path::Path::new("a.md"), body)
                    .expect("parses");
            node.id = "a".into();
            node.kind = Kind::new("generic");
            node.status = Status::new("active");
            node
        };
        let config = cfg_section("## Corrections");
        let after_graph = build_graph(vec![doc(after)]);
        let diff = crate::diff::compute_diff(&build_graph(vec![doc(before)]), &after_graph);
        let violations = rule_for(&config)
            .check(&ctx(&after_graph, &config, Some(&diff)))
            .violations;
        match violations.as_slice() {
            [] => None,
            [violation] => match &violation.details {
                ViolationDetails::BodyImmutable {
                    refusal: Some(refusal),
                    append_section: Some(section),
                    ..
                } if section == "## Corrections" => Some(*refusal),
                other => panic!("unexpected details: {other:?}"),
            },
            more => panic!("one violation at most: {more:?}"),
        }
    }

    #[test]
    fn append_section_admits_the_section_opened_by_the_appended_lines() {
        assert_eq!(
            refusal(
                DECISION,
                &format!("{DECISION}\n## Corrections\n\n- 2026-09-15 — the bound is 32, not 64\n")
            ),
            None
        );
        assert_eq!(
            refusal("", "## *Corrections* ##\n- 2026-09-15 — first\n"),
            None
        );
    }

    #[test]
    fn append_section_admits_growth_inside_the_section_that_ends_the_body() {
        let corrected = format!("{DECISION}\n## Corrections\n\n- 2026-09-15 — first\n");
        assert_eq!(
            refusal(
                &corrected,
                &format!("{corrected}- 2026-09-16 — second\n\n### Evidence\n\nmoved to #42\n")
            ),
            None
        );
    }

    #[test]
    fn append_section_admits_blank_lines_alone() {
        assert_eq!(refusal(DECISION, &format!("{DECISION}\n \t\n")), None);
    }

    #[test]
    fn append_section_refuses_lines_appended_outside_the_section() {
        // Before any section: the tail restates the decision.
        assert_eq!(
            refusal(DECISION, &format!("{DECISION}\nDecision 1 is reversed.\n")),
            Some(AppendRefusal::OutsideSection)
        );
        // Before the section it goes on to open: the tail grows `Decision`.
        assert_eq!(
            refusal(
                DECISION,
                &format!("{DECISION}Also Y.\n\n## Corrections\n\n- 2026-09-15 — fixed\n")
            ),
            Some(AppendRefusal::OutsideSection)
        );
    }

    #[test]
    fn append_section_refuses_a_heading_that_ends_the_section() {
        let corrected = format!("{DECISION}\n## Corrections\n\n- 2026-09-15 — first\n");
        for closing in [
            "\n## Decision, revisited\n\nWe do Y.\n",
            "\n# Corrections\n\n- 2026-09-16 — second\n",
        ] {
            assert_eq!(
                refusal(&corrected, &format!("{corrected}{closing}")),
                Some(AppendRefusal::OutsideSection),
                "{closing:?}"
            );
        }
    }

    #[test]
    fn append_section_refuses_turning_committed_text_into_the_heading() {
        // The underline makes the committed paragraph line `Corrections` a
        // setext heading of the right level and text.
        let before = "# One\n\nCorrections";
        assert_eq!(
            refusal(before, &format!("{before}\n---\n- 2026-09-15 — x\n")),
            Some(AppendRefusal::OutsideSection)
        );
    }

    #[test]
    fn append_section_refuses_a_heading_the_markdown_does_not_open() {
        let fenced = "# One\n\n```\ncode";
        assert_eq!(
            refusal(
                fenced,
                &format!("{fenced}\n## Corrections\n- 2026-09-15 — x\n")
            ),
            Some(AppendRefusal::OutsideSection)
        );
        assert_eq!(
            refusal(
                DECISION,
                &format!("{DECISION}\n> ## Corrections\n> - 2026-09-15 — x\n")
            ),
            Some(AppendRefusal::OutsideSection)
        );
    }

    #[test]
    fn append_section_still_requires_the_committed_body_as_a_prefix() {
        let corrected = format!("{DECISION}\n## Corrections\n\n- 2026-09-15 — first\n");
        assert_eq!(
            refusal(
                &corrected,
                &format!("{DECISION}\n## Corrections\n\n- 2026-09-15 — first, reworded\n")
            ),
            Some(AppendRefusal::Rewritten)
        );
    }

    #[test]
    fn append_section_opens_at_the_end_when_the_committed_one_is_not_last() {
        // A section the committed body carries mid-document stays frozen
        // with everything around it; the one that ends the body is the one
        // growth may go to.
        let before = "# One\n\n## Corrections\n\n## References\n\n- r1\n";
        assert_eq!(
            refusal(before, &format!("{before}- r2\n")),
            Some(AppendRefusal::OutsideSection)
        );
        assert_eq!(
            refusal(
                before,
                &format!("{before}\n## Corrections\n\n- 2026-09-15 — x\n")
            ),
            None
        );
    }

    #[test]
    fn append_section_refuses_a_definition_a_committed_reference_resolves_to() {
        let cited = "# One\n\n## Decision\n\nWe use [the store][db].\n";
        // A new definition turns committed text into a link.
        assert_eq!(
            refusal(
                cited,
                &format!("{cited}\n## Corrections\n\n[DB]: evil.md\n")
            ),
            Some(AppendRefusal::RedefinesReference)
        );
        // An appended line completes a committed definition.
        let pending = format!("{cited}\n## Corrections\n\n[db]:");
        assert_eq!(
            refusal(&pending, &format!("{pending}\nhttps://evil.example\n")),
            Some(AppendRefusal::RedefinesReference)
        );
        // An appended title retitles a committed definition.
        let defined = format!("{cited}\n## Corrections\n\n[db]: https://good.example");
        assert_eq!(
            refusal(&defined, &format!("{defined}\n\"Evil\"\n")),
            Some(AppendRefusal::RedefinesReference)
        );
    }

    #[test]
    fn append_section_admits_definitions_only_appended_lines_use() {
        let corrected = format!("{DECISION}\n## Corrections\n\n- 2026-09-15 — first\n");
        assert_eq!(
            refusal(
                &corrected,
                &format!(
                    "{corrected}- 2026-09-16 — see [the bench][bench]\n\n[bench]: bench.md\n[unused]: x.md\n"
                )
            ),
            None
        );
    }

    #[test]
    fn params_carry_append_section() {
        let config = cfg_section("## Corrections");
        let params = rule_for(&config).params(&config);
        assert_eq!(
            params.get("append_section"),
            Some(&serde_json::json!("## Corrections"))
        );
    }

    // ─── scoping ───────────────────────────────────────────────────────

    #[test]
    fn kinds_filters_out_other_kinds() {
        // The block targets `adr` only — a `runbook` change must not
        // fire even at terminal status.
        let config = cfg(BodyImmutableMode::Frozen, vec!["adr"]);
        let graph = build_graph(vec![make_node("a", "superseded", "runbook")]);
        let d = diff_with(vec![body_change("a", &["l1"], &["l2"])]);
        let rule = rule_for(&config);
        assert!(
            rule.check(&ctx(&graph, &config, Some(&d)))
                .violations
                .is_empty(),
            "node whose kind is outside the rule's `kinds` filter must not fire"
        );
    }

    #[test]
    fn empty_kinds_means_no_kind_restriction() {
        // Per the existing body_line convention, an empty list means
        // "every kind is in scope". Pin this here so a future
        // refactor can't accidentally reverse it.
        let config = cfg(BodyImmutableMode::Frozen, vec![]);
        let graph = build_graph(vec![make_node("a", "superseded", "anything")]);
        let mut c = config.clone();
        c.kinds.allowed.push("anything".into());
        let d = diff_with(vec![body_change("a", &["l1"], &["l2"])]);
        let rule = rule_for(&c);
        assert_eq!(rule.check(&ctx(&graph, &c, Some(&d))).violations.len(), 1);
    }

    // ─── multi-block ───────────────────────────────────────────────────

    #[test]
    fn multiple_blocks_fire_independently_for_their_kinds() {
        // A project may freeze ADRs and append-only-lock runbooks.
        // The runner instantiates one Rule per block; each block
        // sees only its own subset.
        let mut config = Config::default();
        config.statuses.terminal = vec!["superseded".into()];
        config.kinds.allowed.push("adr".into());
        config.kinds.allowed.push("runbook".into());
        config.rules.body_immutable = vec![
            BodyImmutableRuleConfig {
                name: "adr-frozen".into(),
                mode: BodyImmutableMode::Frozen,
                trigger: ImmutableTrigger::Terminal,
                kinds: vec!["adr".into()],
                append_section: None,
            },
            BodyImmutableRuleConfig {
                name: "runbook-append-only".into(),
                mode: BodyImmutableMode::AppendOnly,
                trigger: ImmutableTrigger::Terminal,
                kinds: vec!["runbook".into()],
                append_section: None,
            },
        ];

        let graph = build_graph(vec![
            make_node("a-adr", "superseded", "adr"),
            make_node("a-rb", "superseded", "runbook"),
        ]);
        let d = diff_with(vec![
            body_change("a-adr", &["x"], &["x", "y"]),
            body_change("a-rb", &["x"], &["x", "y"]),
        ]);

        let adr_rule = BodyImmutableRule::new(config.rules.body_immutable[0].clone());
        let rb_rule = BodyImmutableRule::new(config.rules.body_immutable[1].clone());

        let adr_v = adr_rule.check(&ctx(&graph, &config, Some(&d))).violations;
        let rb_v = rb_rule.check(&ctx(&graph, &config, Some(&d))).violations;

        assert_eq!(
            adr_v.len(),
            1,
            "adr-frozen must fire on the adr's body change"
        );
        assert_eq!(adr_v[0].node_id.as_deref(), Some("a-adr"));
        assert!(
            rb_v.is_empty(),
            "runbook-append-only is satisfied by the strict append on a-rb: {rb_v:?}"
        );
    }
}
