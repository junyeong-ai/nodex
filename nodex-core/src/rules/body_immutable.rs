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
//!   re-litigated.
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
//! The rule reads body fingerprints (`body_hash`, `body_lines_hash`)
//! off [`crate::diff::BodyChange`] entries the diff layer already
//! computed — it never touches the filesystem, never re-parses, and
//! never stores body text. That keeps every check-time rule a pure
//! function of `(graph, config)`, same discipline schema /
//! frontmatter_immutable / body_line already follow.

use serde_json::{Map, Value, json};

use crate::config::{BodyImmutableMode, BodyImmutableRuleConfig, ImmutableTrigger};

use super::{Rule, RuleContext, RuleSource, Severity, Violation, ViolationDetails};

/// One `[[rules.body_immutable]]` block as a `Rule` trait object.
pub struct BodyImmutableRule {
    config: BodyImmutableRuleConfig,
    qualified_id: String,
}

impl BodyImmutableRule {
    /// Construct a rule instance for one config block. `qualified_id`
    /// is cached so [`Rule::id`] returns `&str` without allocating
    /// per call — same convention as [`crate::rules::body_line::BodyLineRule`].
    pub fn new(config: BodyImmutableRuleConfig) -> Self {
        let qualified_id = format!("body_immutable/{}", config.name);
        Self {
            config,
            qualified_id,
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
         `append_only` rejects non-prefix changes. Needs a diff context \
         from `--since <ref>` or `rules.immutable_baseline`"
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

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let Some(diff) = ctx.since else {
            return Vec::new();
        };
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

            // append_only with the prior body preserved verbatim as a
            // prefix is exactly what the mode permits — no violation.
            if matches!(self.config.mode, BodyImmutableMode::AppendOnly)
                && change
                    .after_lines_hash
                    .starts_with(&change.before_lines_hash)
            {
                continue;
            }

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
                    before_lines,
                    after_lines,
                },
            ));
        }
        violations
    }
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
            repository: None,
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
        let v = rule.check(&ctx(&graph, &config, Some(&d)));
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
        let v = rule.check(&ctx(&graph, &config, Some(&d)));
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
            rule.check(&ctx(&graph, &config, Some(&d))).is_empty(),
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
            rule.check(&ctx(&graph, &config, Some(&d))).is_empty(),
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
        let v = rule.check(&ctx(&graph, &config, Some(&d)));
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
        let v = rule.check(&ctx(&graph, &config, Some(&d)));
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
        assert_eq!(rule.check(&ctx(&graph, &config, Some(&d))).len(), 1);
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
        assert!(rule.check(&ctx(&graph, &config, Some(&d))).is_empty());
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
        let v = rule.check(&ctx(&graph, &config, Some(&d)));
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
        assert!(rule.check(&ctx(&graph, &config, Some(&d))).is_empty());
    }

    #[test]
    fn creation_append_only_allows_appends_rejects_edits() {
        // The two axes compose: a changelog frozen-from-creation in
        // shape but append-only in mode grows forever, rewrites never.
        let config = cfg_creation(BodyImmutableMode::AppendOnly, vec![]);
        let graph = build_graph(vec![make_node("a", "active", "generic")]);
        let rule = rule_for(&config);

        let append = diff_with(vec![body_change("a", &["l1"], &["l1", "l2"])]);
        assert!(rule.check(&ctx(&graph, &config, Some(&append))).is_empty());

        let edit = diff_with(vec![body_change("a", &["l1"], &["l1-mod"])]);
        assert_eq!(rule.check(&ctx(&graph, &config, Some(&edit))).len(), 1);
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
            rule.check(&ctx(&graph, &config, Some(&d))).is_empty(),
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
        assert_eq!(rule.check(&ctx(&graph, &c, Some(&d))).len(), 1);
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
            },
            BodyImmutableRuleConfig {
                name: "runbook-append-only".into(),
                mode: BodyImmutableMode::AppendOnly,
                trigger: ImmutableTrigger::Terminal,
                kinds: vec!["runbook".into()],
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

        let adr_v = adr_rule.check(&ctx(&graph, &config, Some(&d)));
        let rb_v = rb_rule.check(&ctx(&graph, &config, Some(&d)));

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
