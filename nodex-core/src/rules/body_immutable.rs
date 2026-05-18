//! Lock document bodies once a node reaches terminal status.
//!
//! Diff-aware: requires a `--since` ref so the rule can compare
//! "before" and "after" body fingerprints. Without it the rule
//! reports itself as non-applicable via [`Rule::is_applicable`]
//! rather than silently passing — `.claude/rules/config-driven.md`
//! ("No silent runtime skips").
//!
//! Two modes:
//!
//! - [`BodyImmutableMode::Frozen`]: any body change fires a
//!   violation. The natural mode for ADRs / contracts / signed-off
//!   specs — the body is the decision, and the decision does not
//!   move once shipped.
//! - [`BodyImmutableMode::AppendOnly`]: the pre-terminal body must
//!   remain a prefix of the new body. Suits log-shaped documents
//!   where new entries land at the bottom but earlier entries are
//!   never re-litigated.
//!
//! The rule reads body fingerprints (`body_hash`, `body_lines_hash`)
//! off [`crate::diff::BodyChange`] entries the diff layer already
//! computed — it never touches the filesystem, never re-parses, and
//! never stores body text. That keeps every check-time rule a pure
//! function of `(graph, config)`, same discipline schema /
//! frontmatter_immutable / body_line already follow.

use serde_json::{Map, Value, json};

use crate::config::{BodyImmutableMode, BodyImmutableRuleConfig};

use super::{Rule, RuleContext, RuleSource, Severity, Violation};

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
        "Document bodies are locked once status is terminal; \
         `frozen` rejects any change, `append_only` rejects \
         non-prefix changes. Requires `check --since <ref>` to activate"
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
        "no `--since` ref — diff-aware rules require two snapshots".to_string()
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let Some(diff) = ctx.since else {
            return Vec::new();
        };
        let mut violations = Vec::new();
        for change in &diff.body_changes {
            // The current (after-graph) node carries the *current*
            // status and *current* kind. The lock applies to any node
            // that is *now* in terminal status — same convention
            // `frontmatter_immutable` uses, so the two rules report on
            // the same boundary.
            let Some(node) = ctx.graph.node(&change.id) else {
                continue;
            };
            if !ctx.config.is_terminal(node.status.as_str()) {
                continue;
            }
            if !node.matches_kinds(&self.config.kinds) {
                continue;
            }

            let detail = match self.config.mode {
                BodyImmutableMode::Frozen => Some(format!(
                    "body changed while status is terminal (current: {:?}); \
                     mode=frozen forbids any body edit",
                    node.status.as_str()
                )),
                BodyImmutableMode::AppendOnly => {
                    if change
                        .after_lines_hash
                        .starts_with(&change.before_lines_hash)
                    {
                        // Pre-terminal body is preserved verbatim and
                        // new lines were appended — exactly what
                        // append-only permits.
                        None
                    } else {
                        Some(format!(
                            "body changed while status is terminal (current: {:?}); \
                             mode=append_only requires the previous body to remain a \
                             prefix of the new body (before={} lines, after={} lines)",
                            node.status.as_str(),
                            change.before_lines_hash.len(),
                            change.after_lines_hash.len()
                        ))
                    }
                }
            };

            if let Some(message) = detail {
                violations.push(Violation {
                    rule_id: self.qualified_id.clone(),
                    severity: Severity::Error,
                    node_id: Some(change.id.clone()),
                    path: Some(crate::path_guard::forward_string(&node.path)),
                    message,
                });
            }
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
        }
    }

    fn build_graph(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![], vec![])
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
            graph,
            config,
            root: std::path::Path::new("."),
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
                kinds: vec!["adr".into()],
            },
            BodyImmutableRuleConfig {
                name: "runbook-append-only".into(),
                mode: BodyImmutableMode::AppendOnly,
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
