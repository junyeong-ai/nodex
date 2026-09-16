//! Lock declared frontmatter fields once the block's trigger engages.
//!
//! Diff-aware: needs a "before" snapshot to compare against, supplied
//! by `--since <ref>` or, by default, `rules.immutable_baseline`.
//! Without one the rule reports itself as non-applicable via
//! [`Rule::is_applicable`] rather than silently passing —
//! `.claude/rules/config-driven.md` ("No silent runtime skips").
//!
//! One [`FrontmatterImmutableRule`] instance per
//! `[[rules.frontmatter_immutable]]` config block, symmetric with
//! [`crate::rules::body_immutable::BodyImmutableRule`]. The block
//! carries a unique `name`, the kind filter, an
//! [`crate::config::ImmutableTrigger`] saying when the lock engages, and
//! the per-block `fields` payload.
//!
//! Arming is read through [`crate::config::Config::lock_arms`], in the
//! BEFORE snapshot's frame, so the single write that first drives a
//! document into the lock — legitimately setting `superseded_by` and
//! friends in the same edit — is allowed; the lock bites only on edits to
//! a document the lock was *already* armed over. `kind` is read in that
//! frame too, so a write that takes a document out of the block's kinds
//! is judged by the block that held it. A locked field reaches the rule
//! through whichever diff channel carries it:
//! - ordinary fields surface as a [`crate::diff::FieldChange`];
//! - `status` is a [`crate::diff::StatusTransition`] — locking it freezes
//!   the status of a document the lock was armed over.
//!
//! Locking `kind` is how a project says which lifecycle a record answers
//! to is part of the record. Under `terminal` that is settled only once
//! the record is finished with, which is after every rule keyed on kind
//! has already been asked; the other two triggers are how a project
//! settles it at acceptance, or from creation.
//!
//! `id` is not lockable here and `Config::validate` rejects it: it is the
//! snapshot join key, so a present doc cannot change its id without
//! becoming a different node, and `rename` anchors it before moving.
//! Graph removal alone cannot tell a deletion from a scope change or an
//! id-rule re-key, so a diff signal could only ever fire as a false
//! positive. `id` immutability is structural, so a lock that could never
//! correctly fire is refused at load rather than accepted and silently
//! ignored.

use serde_json::{Map, Value, json};

use crate::config::{FrontmatterImmutableRuleConfig, ImmutableTrigger};

use super::{
    Rule, RuleContext, RuleRun, RuleSource, Severity, SubjectUnit, Violation, ViolationDetails,
};

/// One `[[rules.frontmatter_immutable]]` block as a `Rule` trait
/// object.
pub struct FrontmatterImmutableRule {
    config: FrontmatterImmutableRuleConfig,
    qualified_id: String,
}

impl FrontmatterImmutableRule {
    /// Construct a rule instance for one config block. `qualified_id`
    /// is cached so [`Rule::id`] returns `&str` without allocating
    /// per call — same convention every other config-driven rule
    /// follows.
    pub fn new(config: FrontmatterImmutableRuleConfig) -> Self {
        let qualified_id = format!("frontmatter_immutable/{}", config.name);
        Self {
            config,
            qualified_id,
        }
    }

    /// Build a violation for this block — one seam so every channel emits
    /// the same `rule_id` / severity shape.
    fn violation(&self, node_id: &str, path: String, details: ViolationDetails) -> Violation {
        Violation::new(
            self.qualified_id.clone(),
            Severity::Error,
            Some(node_id.to_string()),
            Some(path),
            details,
        )
    }
}

impl Rule for FrontmatterImmutableRule {
    fn id(&self) -> &str {
        &self.qualified_id
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Listed frontmatter fields are immutable once the block's trigger engages — \
         `terminal` locks at terminal status, `status` at the statuses the block names, \
         `creation` from the document's first committed snapshot; needs a diff context \
         from `--since <ref>` or `rules.immutable_baseline`"
    }

    fn source(&self) -> RuleSource {
        RuleSource::Config
    }

    fn params(&self, _config: &crate::config::Config) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("fields".into(), json!(self.config.fields));
        m.insert(
            "trigger".into(),
            json!(match self.config.trigger {
                ImmutableTrigger::Terminal => "terminal",
                ImmutableTrigger::Creation => "creation",
                ImmutableTrigger::Status => "status",
            }),
        );
        m.insert("kinds".into(), json!(self.config.kinds));
        m.insert("statuses".into(), json!(self.config.statuses));
        m
    }

    fn diff_aware(&self) -> bool {
        true
    }

    fn is_lock(&self) -> bool {
        true
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        // The block exists by construction (`registered_rules` only
        // instantiates this rule when the user authored the block).
        // The remaining gate is the diff context — frontmatter
        // immutability is meaningless without a "before" snapshot.
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
        let locked: std::collections::BTreeSet<&str> =
            self.config.fields.iter().map(String::as_str).collect();

        // The records the lock is armed over — every one whose fields it
        // would refuse an edit to, not the few whose fields moved this run.
        // A clean tree hands the diff nothing, and the standing reach is
        // what tells a lock holding hundreds of records from one holding
        // none. Read in the baseline's frame, because that is the frame the
        // verdict below judges in: a record that has since left the arming
        // is one this lock was armed over and someone moved anyway, so it is
        // the first thing the population must contain, not the one thing it
        // would drop. A record the baseline holds no node for has no frame to
        // be read in and no channel that could reach it, so it is not in the
        // population however armed it looks now — counted apart, and
        // selected on what it looks like now because that is the only frame
        // such a record has.
        let unbacked = diff.added_ids();
        let (subjects, unjudged) = ctx.graph.nodes().values().fold((0, 0), |(kept, lost), n| {
            let selected =
                super::kind_allowed(&self.config.kinds, diff.before_kind(&n.id, n.kind.as_str()))
                    && ctx.config.lock_arms(
                        self.config.trigger,
                        &self.config.statuses,
                        diff.before_status(&n.id, n.status.as_str()),
                    );
            match (selected, unbacked.contains(n.id.as_str())) {
                (true, false) => (kept + 1, lost),
                (true, true) => (kept, lost + 1),
                (false, _) => (kept, lost),
            }
        });
        let mut violations = Vec::new();

        // Channel 1 — ordinary frontmatter field changes (kind, owner,
        // superseded_by, created, dates, project `attrs`, …). The lock
        // applies to a doc the trigger had *already* armed before this edit,
        // so the gate is the BEFORE status — otherwise the very write that
        // first arms the lock (which legitimately sets `superseded_by`,
        // etc.) would be rejected. Same for the kind filter: it gates on the
        // kind the node held before the edit, which is what lets a block
        // lock `kind` itself.
        for change in &diff.field_changes {
            if !locked.contains(change.field.as_str()) {
                continue;
            }
            let Some(node) = ctx.graph.node(&change.id) else {
                continue;
            };
            let before_status = diff.before_status(&change.id, node.status.as_str());
            if !ctx
                .config
                .lock_arms(self.config.trigger, &self.config.statuses, before_status)
            {
                continue;
            }
            if !super::kind_allowed(
                &self.config.kinds,
                diff.before_kind(&change.id, node.kind.as_str()),
            ) {
                continue;
            }
            // Which status the payload names is which status answers for the
            // lock firing: the one that armed it, except under `creation`,
            // where no status did and reporting one would claim it had.
            let (before_status, current_status) = match self.config.trigger {
                ImmutableTrigger::Terminal | ImmutableTrigger::Status => {
                    (Some(before_status.to_string()), None)
                }
                ImmutableTrigger::Creation => (None, Some(node.status.as_str().to_string())),
            };
            violations.push(self.violation(
                &change.id,
                crate::path_guard::forward_string(&node.path),
                ViolationDetails::FrontmatterFieldImmutable {
                    field: change.field.clone(),
                    trigger: self.config.trigger,
                    before_status,
                    current_status,
                },
            ));
        }

        // Channel 2 — `status` itself. A status change is a
        // [`StatusTransition`], never a [`FieldChange`], so a `status`
        // lock reads the transition stream. A document the lock was armed
        // over may not change status, so the gate is the before status
        // (`transition.from`); the write that first arms the lock is the
        // legitimate one and is allowed. Here `from` and `to` are the move
        // itself rather than the arming, so they are reported whatever the
        // trigger, and the trigger says which of them answers for the lock.
        if locked.contains("status") {
            for transition in &diff.status_transitions {
                if !ctx.config.lock_arms(
                    self.config.trigger,
                    &self.config.statuses,
                    &transition.from,
                ) {
                    continue;
                }
                let Some(node) = ctx.graph.node(&transition.id) else {
                    continue;
                };
                if !super::kind_allowed(
                    &self.config.kinds,
                    diff.before_kind(&transition.id, node.kind.as_str()),
                ) {
                    continue;
                }
                violations.push(self.violation(
                    &transition.id,
                    crate::path_guard::forward_string(&node.path),
                    ViolationDetails::StatusImmutable {
                        trigger: self.config.trigger,
                        from: transition.from.clone(),
                        to: transition.to.clone(),
                    },
                ));
            }
        }

        RuleRun::new(subjects, violations).unjudged(unjudged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, FrontmatterImmutableRuleConfig};
    use crate::diff::{FieldChange, GraphDiff};
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

    fn block(name: &str, fields: Vec<&str>) -> FrontmatterImmutableRuleConfig {
        FrontmatterImmutableRuleConfig {
            name: name.into(),
            fields: fields.into_iter().map(String::from).collect(),
            trigger: ImmutableTrigger::Terminal,
            kinds: vec![],
            statuses: vec![],
        }
    }

    fn cfg() -> Config {
        let mut c = Config::default();
        c.statuses.terminal = vec!["superseded".into()];
        c.rules.frontmatter_immutable = vec![block("identity", vec!["id", "superseded_by"])];
        c
    }

    fn diff_with(changes: Vec<FieldChange>) -> GraphDiff {
        GraphDiff {
            added_nodes: vec![],
            removed_nodes: vec![],
            added_edges: vec![],
            removed_edges: vec![],
            status_transitions: vec![],
            path_changes: vec![],
            added_annotations: vec![],
            removed_annotations: vec![],
            field_changes: changes,
            body_changes: vec![],
        }
    }

    fn field_change(id: &str, field: &str) -> FieldChange {
        FieldChange {
            id: id.into(),
            field: field.into(),
            before: Some(serde_json::Value::String("a".into())),
            after: Some(serde_json::Value::String("b".into())),
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
            steps: None,
        }
    }

    fn rule_for(config: &Config) -> FrontmatterImmutableRule {
        FrontmatterImmutableRule::new(config.rules.frontmatter_immutable[0].clone())
    }

    #[test]
    fn rule_id_is_qualified_with_block_name() {
        let c = cfg();
        let rule = rule_for(&c);
        assert_eq!(rule.id(), "frontmatter_immutable/identity");
    }

    #[test]
    fn inert_without_since_ref() {
        let c = cfg();
        let g = build_graph(vec![make_node("a", "superseded", "generic")]);
        let rule = rule_for(&c);
        assert!(!rule.is_applicable(&ctx(&g, &c, None)));
        assert!(
            rule.skip_reason(&ctx(&g, &c, None)).contains("--since"),
            "skip reason must mention --since"
        );
    }

    #[test]
    fn fires_when_terminal_node_locked_field_changes() {
        let c = cfg();
        let g = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_with(vec![field_change("a", "superseded_by")]);
        let rule = rule_for(&c);
        let v = rule.check(&ctx(&g, &c, Some(&d))).violations;
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "frontmatter_immutable/identity");
        assert!(v[0].message.contains("\"superseded_by\""));
    }

    #[test]
    fn silent_when_changed_field_not_locked() {
        let c = cfg();
        let g = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_with(vec![field_change("a", "title")]);
        let rule = rule_for(&c);
        assert!(rule.check(&ctx(&g, &c, Some(&d))).violations.is_empty());
    }

    #[test]
    fn silent_when_node_not_terminal() {
        let c = cfg();
        let g = build_graph(vec![make_node("a", "active", "generic")]);
        let d = diff_with(vec![field_change("a", "superseded_by")]);
        let rule = rule_for(&c);
        assert!(rule.check(&ctx(&g, &c, Some(&d))).violations.is_empty());
    }

    #[test]
    fn kinds_narrows_target_kinds() {
        // The block targets `adr` only — a `runbook` change must not
        // fire even when its terminal status would otherwise trigger
        // the lock.
        let mut c = cfg();
        c.kinds.allowed.push("adr".into());
        c.kinds.allowed.push("runbook".into());
        c.rules.frontmatter_immutable[0].kinds = vec!["adr".into()];
        let g = build_graph(vec![make_node("a", "superseded", "runbook")]);
        let d = diff_with(vec![field_change("a", "id")]);
        let rule = rule_for(&c);
        assert!(rule.check(&ctx(&g, &c, Some(&d))).violations.is_empty());
    }

    #[test]
    fn multi_block_each_fires_for_its_own_fields() {
        // Two blocks: one locks identity (`id`), another locks
        // `decision_date` for ADR-kind only. Both must report under
        // distinct rule_ids.
        let mut c = cfg();
        c.kinds.allowed.push("adr".into());
        c.schema
            .types
            .insert("decision_date".into(), crate::config::FieldType::Date);
        c.rules.frontmatter_immutable = vec![
            block("identity", vec!["id"]),
            FrontmatterImmutableRuleConfig {
                name: "adr-decision-date".into(),
                fields: vec!["decision_date".into()],
                trigger: ImmutableTrigger::Terminal,
                kinds: vec!["adr".into()],
                statuses: vec![],
            },
        ];
        let g = build_graph(vec![make_node("a", "superseded", "adr")]);
        let d = diff_with(vec![
            field_change("a", "id"),
            field_change("a", "decision_date"),
        ]);

        let identity = FrontmatterImmutableRule::new(c.rules.frontmatter_immutable[0].clone());
        let date = FrontmatterImmutableRule::new(c.rules.frontmatter_immutable[1].clone());
        let identity_v = identity.check(&ctx(&g, &c, Some(&d))).violations;
        let date_v = date.check(&ctx(&g, &c, Some(&d))).violations;
        assert_eq!(identity_v.len(), 1);
        assert_eq!(identity_v[0].rule_id, "frontmatter_immutable/identity");
        assert_eq!(date_v.len(), 1);
        assert_eq!(date_v[0].rule_id, "frontmatter_immutable/adr-decision-date");
    }

    // ─── before-state gating + status channel ──────────────────────────
    //
    // "Once terminal" is judged against the BEFORE snapshot. The single
    // write that first drives a doc terminal (and legitimately sets
    // `superseded_by`) must be allowed; only edits to an already-terminal
    // doc are locked. `status` rides the transition stream rather than a
    // `FieldChange`, so locking it must still fire.

    fn diff_full(
        field_changes: Vec<FieldChange>,
        status_transitions: Vec<crate::diff::StatusTransition>,
    ) -> GraphDiff {
        GraphDiff {
            added_nodes: vec![],
            removed_nodes: vec![],
            added_edges: vec![],
            removed_edges: vec![],
            status_transitions,
            added_annotations: vec![],
            removed_annotations: vec![],
            field_changes,
            path_changes: vec![],
            body_changes: vec![],
        }
    }

    fn transition(id: &str, from: &str, to: &str) -> crate::diff::StatusTransition {
        crate::diff::StatusTransition {
            id: id.into(),
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn field_lock_allows_terminalizing_write() {
        // active → superseded that sets `superseded_by` in the same edit
        // is the legitimate first write. The before status was active
        // (non-terminal), so the lock must NOT fire — otherwise every
        // supersession would be rejected.
        let c = cfg(); // locks id + superseded_by
        let g = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_full(
            vec![field_change("a", "superseded_by")],
            vec![transition("a", "active", "superseded")],
        );
        let rule = rule_for(&c);
        assert!(
            rule.check(&ctx(&g, &c, Some(&d))).violations.is_empty(),
            "the terminalizing write must be allowed"
        );
    }

    #[test]
    fn field_lock_fires_on_already_terminal_edit() {
        // superseded → archived (terminal → terminal) while editing a
        // locked field: the before status was terminal, so the edit is a
        // violation.
        let mut c = cfg();
        c.rules.frontmatter_immutable = vec![block("identity", vec!["superseded_by"])];
        let g = build_graph(vec![make_node("a", "archived", "generic")]);
        let d = diff_full(
            vec![field_change("a", "superseded_by")],
            vec![transition("a", "superseded", "archived")],
        );
        let rule = rule_for(&c);
        let v = rule.check(&ctx(&g, &c, Some(&d))).violations;
        assert_eq!(v.len(), 1);
        assert!(v[0].message.contains("superseded_by"), "{}", v[0].message);
    }

    #[test]
    fn status_lock_fires_when_terminal_status_changes() {
        let mut c = cfg();
        c.rules.frontmatter_immutable = vec![block("lifecycle", vec!["status"])];
        // after-status is `active`; the gate is the *before* status.
        let g = build_graph(vec![make_node("a", "active", "generic")]);
        let d = diff_full(vec![], vec![transition("a", "superseded", "active")]);
        let rule = rule_for(&c);
        let run = rule.check(&ctx(&g, &c, Some(&d)));
        // The record left terminal, so the graph's own status no longer says
        // this lock was ever armed over it. The verdict judges in the
        // baseline's frame and the reach must too, or the one document this
        // lock caught is the one document it claims never to have guarded.
        assert_eq!(run.subjects, 1);
        let v = run.violations;
        assert_eq!(v.len(), 1);
        assert!(v[0].message.contains("\"status\""), "{}", v[0].message);
    }

    #[test]
    fn status_lock_silent_on_first_transition_into_terminal() {
        // active → superseded is the legal entry into terminal: the
        // before status was not terminal, so the lock must not fire.
        let mut c = cfg();
        c.rules.frontmatter_immutable = vec![block("lifecycle", vec!["status"])];
        let g = build_graph(vec![make_node("a", "superseded", "generic")]);
        let d = diff_full(vec![], vec![transition("a", "active", "superseded")]);
        let rule = rule_for(&c);
        assert!(rule.check(&ctx(&g, &c, Some(&d))).violations.is_empty());
    }

    #[test]
    fn new_channels_respect_kind_filter() {
        let mut c = cfg();
        c.kinds.allowed.push("adr".into());
        c.kinds.allowed.push("runbook".into());
        c.rules.frontmatter_immutable = vec![FrontmatterImmutableRuleConfig {
            name: "identity".into(),
            fields: vec!["status".into()],
            trigger: ImmutableTrigger::Terminal,
            kinds: vec!["adr".into()],
            statuses: vec![],
        }];
        let g = build_graph(vec![
            make_node("adr-x", "active", "adr"),
            make_node("rb-x", "active", "runbook"),
        ]);
        let d = diff_full(
            vec![],
            vec![
                transition("adr-x", "superseded", "active"),
                transition("rb-x", "superseded", "active"),
            ],
        );
        let rule = rule_for(&c);
        let v = rule.check(&ctx(&g, &c, Some(&d))).violations;
        let ids: Vec<&str> = v.iter().filter_map(|x| x.node_id.as_deref()).collect();
        assert_eq!(ids, vec!["adr-x"], "only adr kind fires; runbook excluded");
    }

    // ─── triggers ──────────────────────────────────────────────────────

    /// The block from `cfg`, rearmed. `statuses` is read only under
    /// `trigger = "status"`, which is what the load guards enforce.
    fn armed(trigger: ImmutableTrigger, statuses: &[&str], fields: Vec<&str>) -> Config {
        let mut c = cfg();
        c.statuses.allowed = vec!["proposed".into(), "active".into(), "superseded".into()];
        c.rules.frontmatter_immutable = vec![FrontmatterImmutableRuleConfig {
            name: "identity".into(),
            fields: fields.into_iter().map(String::from).collect(),
            trigger,
            kinds: vec![],
            statuses: statuses.iter().map(|s| (*s).into()).collect(),
        }];
        c
    }

    #[test]
    fn a_status_armed_lock_is_silent_while_the_record_is_a_draft() {
        let c = armed(ImmutableTrigger::Status, &["active"], vec!["owner"]);
        let g = build_graph(vec![make_node("a", "proposed", "generic")]);
        let d = diff_with(vec![field_change("a", "owner")]);
        let run = rule_for(&c).check(&ctx(&g, &c, Some(&d)));
        assert!(run.violations.is_empty(), "a draft is not armed");
        assert_eq!(run.subjects, 0, "and the reach says the lock holds nothing");
    }

    #[test]
    fn a_status_armed_lock_fires_once_the_record_is_armed() {
        let c = armed(ImmutableTrigger::Status, &["active"], vec!["owner"]);
        let g = build_graph(vec![make_node("a", "active", "generic")]);
        let d = diff_with(vec![field_change("a", "owner")]);
        let run = rule_for(&c).check(&ctx(&g, &c, Some(&d)));
        assert_eq!(run.subjects, 1);
        assert_eq!(run.violations.len(), 1);
        assert!(
            run.violations[0].message.contains("trigger=status"),
            "the message names the arming that fired: {}",
            run.violations[0].message
        );
    }

    #[test]
    fn a_creation_lock_fires_at_a_status_no_other_trigger_arms_at() {
        // The gap `terminal` leaves open, and the one `status` closes only
        // from acceptance onward: an edit to a draft's locked field.
        let c = armed(ImmutableTrigger::Creation, &[], vec!["owner"]);
        let g = build_graph(vec![make_node("a", "proposed", "generic")]);
        let d = diff_with(vec![field_change("a", "owner")]);
        let run = rule_for(&c).check(&ctx(&g, &c, Some(&d)));
        assert_eq!(run.subjects, 1);
        assert_eq!(run.violations.len(), 1);
        let message = &run.violations[0].message;
        assert!(message.contains("trigger=creation"), "{message}");
        assert!(
            message.contains("\"proposed\""),
            "a creation lock reports the status it did not key on as the \
             status that does not exempt the record: {message}"
        );
    }

    #[test]
    fn a_write_that_takes_a_record_out_of_the_blocks_kinds_is_judged_by_it() {
        // Locking `kind` is only a lock if the write that changes it is
        // judged by the block the record was under, not the one it lands
        // in. The kind filter reads the before frame, so it is.
        let mut c = armed(ImmutableTrigger::Status, &["active"], vec!["kind"]);
        c.kinds.allowed.push("adr".into());
        c.rules.frontmatter_immutable[0].kinds = vec!["adr".into()];
        let g = build_graph(vec![make_node("a", "active", "generic")]);
        let d = diff_with(vec![FieldChange {
            id: "a".into(),
            field: "kind".into(),
            before: Some(serde_json::Value::String("adr".into())),
            after: Some(serde_json::Value::String("generic".into())),
        }]);
        let run = rule_for(&c).check(&ctx(&g, &c, Some(&d)));
        assert_eq!(run.subjects, 1);
        assert_eq!(run.violations.len(), 1);
        assert!(run.violations[0].message.contains("\"kind\""));
    }

    #[test]
    fn params_carry_the_trigger_and_the_statuses_it_reads() {
        let c = armed(ImmutableTrigger::Status, &["active"], vec!["owner"]);
        let params = rule_for(&c).params(&c);
        assert_eq!(params["trigger"], serde_json::json!("status"));
        assert_eq!(params["statuses"], serde_json::json!(["active"]));
        let terminal = armed(ImmutableTrigger::Terminal, &[], vec!["owner"]);
        assert_eq!(
            rule_for(&terminal).params(&terminal)["trigger"],
            serde_json::json!("terminal")
        );
    }

    #[test]
    fn a_block_without_a_trigger_keeps_locking_at_terminal() {
        let block: FrontmatterImmutableRuleConfig =
            toml::from_str("name = \"b\"\nfields = [\"owner\"]\n").expect("parses");
        assert_eq!(block.trigger, ImmutableTrigger::Terminal);
        assert!(block.statuses.is_empty());
    }
}
