//! Lock declared frontmatter fields once a node reaches terminal status.
//!
//! Diff-aware: requires a `since_ref` context to compare "before" and
//! "after" snapshots. Without it the rule reports itself as
//! non-applicable via [`Rule::is_applicable`] rather than silently
//! passing — see `.claude/rules/config-driven.md` ("No silent runtime
//! skips").

use super::{Rule, RuleContext, Severity, Violation};

pub struct FrontmatterImmutableRule;

impl Rule for FrontmatterImmutableRule {
    fn id(&self) -> &str {
        "frontmatter_immutable"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        ctx.config.rules.frontmatter_immutable.is_some() && ctx.since.is_some()
    }

    fn skip_reason(&self, ctx: &RuleContext<'_>) -> String {
        if ctx.config.rules.frontmatter_immutable.is_none() {
            "[rules.frontmatter_immutable] not configured".to_string()
        } else {
            "no `--since` ref — diff-aware rules require two snapshots".to_string()
        }
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let Some(cfg) = &ctx.config.rules.frontmatter_immutable else {
            return Vec::new();
        };
        let Some(diff) = ctx.since else {
            return Vec::new();
        };
        let locked: std::collections::BTreeSet<&str> =
            cfg.fields.iter().map(String::as_str).collect();

        let mut violations = Vec::new();
        for change in &diff.field_changes {
            if !locked.contains(change.field.as_str()) {
                continue;
            }
            // The "before" node (in the after-graph) carries the *current*
            // status. We check the after-graph because the lock applies to
            // any node *now* in terminal status whose declared field
            // changed against a prior snapshot.
            let Some(node) = ctx.graph.node(&change.id) else {
                continue;
            };
            if !ctx.config.is_terminal(node.status.as_str()) {
                continue;
            }
            violations.push(Violation {
                rule_id: self.id().to_string(),
                severity: self.severity(),
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
    use crate::config::{Config, FrontmatterImmutableConfig};
    use crate::diff::{FieldChange, GraphDiff};
    use crate::model::{Graph, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_node(id: &str, status: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.into(),
            kind: Kind::new("adr"),
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
        }
    }

    fn build_graph(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![])
    }

    fn cfg_with_lock() -> Config {
        let mut c = Config::default();
        c.statuses.terminal = vec!["superseded".into()];
        c.rules.frontmatter_immutable = Some(FrontmatterImmutableConfig {
            fields: vec!["id".into(), "superseded_by".into()],
        });
        c
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

    #[test]
    fn inert_when_config_absent() {
        let mut config = cfg_with_lock();
        config.rules.frontmatter_immutable = None;
        let graph = build_graph(vec![make_node("a", "superseded")]);
        let diff = GraphDiff {
            added_nodes: vec![],
            removed_nodes: vec![],
            added_edges: vec![],
            removed_edges: vec![],
            status_transitions: vec![],
            field_changes: vec![FieldChange {
                id: "a".into(),
                field: "superseded_by".into(),
                before: Some(serde_json::Value::String("x".into())),
                after: Some(serde_json::Value::String("y".into())),
            }],
        };
        let rule = FrontmatterImmutableRule;
        assert!(!rule.is_applicable(&ctx(&graph, &config, Some(&diff))));
    }

    #[test]
    fn inert_when_no_since_ref() {
        let config = cfg_with_lock();
        let graph = build_graph(vec![make_node("a", "superseded")]);
        let rule = FrontmatterImmutableRule;
        assert!(!rule.is_applicable(&ctx(&graph, &config, None)));
    }

    #[test]
    fn fires_when_terminal_node_locked_field_changes() {
        let config = cfg_with_lock();
        let graph = build_graph(vec![make_node("a", "superseded")]);
        let diff = GraphDiff {
            added_nodes: vec![],
            removed_nodes: vec![],
            added_edges: vec![],
            removed_edges: vec![],
            status_transitions: vec![],
            field_changes: vec![FieldChange {
                id: "a".into(),
                field: "superseded_by".into(),
                before: Some(serde_json::Value::String("old".into())),
                after: Some(serde_json::Value::String("new".into())),
            }],
        };
        let rule = FrontmatterImmutableRule;
        let v = rule.check(&ctx(&graph, &config, Some(&diff)));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "frontmatter_immutable");
        assert!(v[0].message.contains("\"superseded_by\""));
    }

    #[test]
    fn silent_when_changed_field_not_locked() {
        let config = cfg_with_lock();
        let graph = build_graph(vec![make_node("a", "superseded")]);
        let diff = GraphDiff {
            added_nodes: vec![],
            removed_nodes: vec![],
            added_edges: vec![],
            removed_edges: vec![],
            status_transitions: vec![],
            field_changes: vec![FieldChange {
                id: "a".into(),
                field: "title".into(),
                before: Some(serde_json::Value::String("Old".into())),
                after: Some(serde_json::Value::String("New".into())),
            }],
        };
        let rule = FrontmatterImmutableRule;
        assert!(rule.check(&ctx(&graph, &config, Some(&diff))).is_empty());
    }

    #[test]
    fn silent_when_node_not_terminal() {
        let config = cfg_with_lock();
        let graph = build_graph(vec![make_node("a", "active")]);
        let diff = GraphDiff {
            added_nodes: vec![],
            removed_nodes: vec![],
            added_edges: vec![],
            removed_edges: vec![],
            status_transitions: vec![],
            field_changes: vec![FieldChange {
                id: "a".into(),
                field: "superseded_by".into(),
                before: None,
                after: Some(serde_json::Value::String("z".into())),
            }],
        };
        let rule = FrontmatterImmutableRule;
        assert!(rule.check(&ctx(&graph, &config, Some(&diff))).is_empty());
    }
}
