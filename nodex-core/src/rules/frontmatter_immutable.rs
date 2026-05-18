//! Lock declared frontmatter fields once a node reaches terminal status.
//!
//! Diff-aware: requires a `--since` ref so the rule can compare
//! "before" and "after" snapshots. Without it the rule reports itself
//! as non-applicable via [`Rule::is_applicable`] rather than silently
//! passing — `.claude/rules/config-driven.md` ("No silent runtime
//! skips").
//!
//! One [`FrontmatterImmutableRule`] instance per
//! `[[rules.frontmatter_immutable]]` config block, symmetric with
//! [`crate::rules::body_immutable::BodyImmutableRule`]. The block
//! carries a unique `name`, the kind filter, and the per-block
//! `fields` payload; the rule fires only on field changes that
//! intersect the listed fields, where the *current* status is
//! terminal AND the kind filter matches.

use serde_json::{Map, Value, json};

use crate::config::FrontmatterImmutableRuleConfig;

use super::{Rule, RuleContext, RuleSource, Severity, Violation};

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
}

impl Rule for FrontmatterImmutableRule {
    fn id(&self) -> &str {
        &self.qualified_id
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Listed frontmatter fields are immutable once status is terminal; \
         requires `check --since <ref>` to activate"
    }

    fn source(&self) -> RuleSource {
        RuleSource::Config
    }

    fn params(&self, _config: &crate::config::Config) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("fields".into(), json!(self.config.fields));
        m.insert("kinds".into(), json!(self.config.kinds));
        m
    }

    fn diff_aware(&self) -> bool {
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
        "no `--since` ref — diff-aware rules require two snapshots".to_string()
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let Some(diff) = ctx.since else {
            return Vec::new();
        };
        let locked: std::collections::BTreeSet<&str> =
            self.config.fields.iter().map(String::as_str).collect();

        let mut violations = Vec::new();
        for change in &diff.field_changes {
            if !locked.contains(change.field.as_str()) {
                continue;
            }
            // The current (after-graph) node carries the *current*
            // status and *current* kind. The lock applies to any node
            // *now* in terminal status — same convention
            // `body_immutable` uses, so the two rules report on the
            // same boundary.
            let Some(node) = ctx.graph.node(&change.id) else {
                continue;
            };
            if !ctx.config.is_terminal(node.status.as_str()) {
                continue;
            }
            if !node.matches_kinds(&self.config.kinds) {
                continue;
            }
            violations.push(Violation {
                rule_id: self.qualified_id.clone(),
                severity: Severity::Error,
                node_id: Some(change.id.clone()),
                path: Some(crate::path_guard::forward_string(&node.path)),
                message: format!(
                    "field {:?} is immutable once status is terminal (current: {:?})",
                    change.field,
                    node.status.as_str()
                ),
            });
        }
        violations
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
        }
    }

    fn build_graph(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![], vec![])
    }

    fn block(name: &str, fields: Vec<&str>) -> FrontmatterImmutableRuleConfig {
        FrontmatterImmutableRuleConfig {
            name: name.into(),
            fields: fields.into_iter().map(String::from).collect(),

            kinds: vec![],
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
            graph,
            config,
            root: std::path::Path::new("."),
            since: diff,
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
        let v = rule.check(&ctx(&g, &c, Some(&d)));
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
        assert!(rule.check(&ctx(&g, &c, Some(&d))).is_empty());
    }

    #[test]
    fn silent_when_node_not_terminal() {
        let c = cfg();
        let g = build_graph(vec![make_node("a", "active", "generic")]);
        let d = diff_with(vec![field_change("a", "superseded_by")]);
        let rule = rule_for(&c);
        assert!(rule.check(&ctx(&g, &c, Some(&d))).is_empty());
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
        assert!(rule.check(&ctx(&g, &c, Some(&d))).is_empty());
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
                kinds: vec!["adr".into()],
            },
        ];
        let g = build_graph(vec![make_node("a", "superseded", "adr")]);
        let d = diff_with(vec![
            field_change("a", "id"),
            field_change("a", "decision_date"),
        ]);

        let identity = FrontmatterImmutableRule::new(c.rules.frontmatter_immutable[0].clone());
        let date = FrontmatterImmutableRule::new(c.rules.frontmatter_immutable[1].clone());
        let identity_v = identity.check(&ctx(&g, &c, Some(&d)));
        let date_v = date.check(&ctx(&g, &c, Some(&d)));
        assert_eq!(identity_v.len(), 1);
        assert_eq!(identity_v[0].rule_id, "frontmatter_immutable/identity");
        assert_eq!(date_v.len(), 1);
        assert_eq!(date_v[0].rule_id, "frontmatter_immutable/adr-decision-date");
    }
}
