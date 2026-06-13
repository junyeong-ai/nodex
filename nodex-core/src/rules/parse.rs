//! Parse-honesty rules: parsing problems are Error-severity check
//! violations, never envelope warnings a gate ignores. `field_parse`
//! flags built-in fields whose authored value failed coercion on a
//! still-present node; `parse_failure` flags in-scope documents that
//! dropped from the graph entirely. Both are unconditional built-ins —
//! parsing is intrinsic, not config-gated — so `check` and
//! `export rules` always carry them.

use super::{Rule, RuleContext, Severity, Violation, ViolationDetails};

/// One Error-severity violation per [`crate::model::FieldParseIssue`]
/// on a present node — the sibling of `field_type`, which does the
/// same for config-declared `attrs` keys. The failed field reads as
/// absent everywhere downstream, so this rule is the only place the
/// authored-but-unusable value surfaces.
pub struct FieldParseRule;

impl Rule for FieldParseRule {
    fn id(&self) -> &str {
        "field_parse"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Built-in frontmatter fields must parse as their type; a failed value reads as absent"
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let mut violations = Vec::new();
        for node in ctx.graph.nodes().values() {
            for issue in &node.parse_issues {
                violations.push(Violation::new(
                    self.id(),
                    self.severity(),
                    Some(node.id.clone()),
                    Some(crate::path_guard::forward_string(&node.path)),
                    ViolationDetails::FieldParse {
                        field: issue.field.clone(),
                        expected: issue.expected.clone(),
                        found: issue.found.clone(),
                    },
                ));
            }
        }
        violations
    }
}

/// One node-less Error-severity violation per
/// [`crate::model::ParseFailure`] — an in-scope document that failed to
/// parse has no node to attribute the finding to, so `node_id` is
/// `None` and `path` carries the file (the cycle-detection convention:
/// node-less violations survive `--since` / `--content`
/// set-membership narrowing). A dropped document reds the gate; it can
/// never pass CI as a warning.
pub struct ParseFailureRule;

impl Rule for ParseFailureRule {
    fn id(&self) -> &str {
        "parse_failure"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Every in-scope document must parse; a dropped document is an error, not a warning"
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        ctx.graph
            .parse_failures()
            .iter()
            .map(|failure| {
                // The digest makes the violation byte-state specific:
                // two different broken contents at one path compare
                // unequal, so a `--content` proposal that swaps one
                // broken byte-state for another is refused by the
                // before/after delta — only a byte-identical proposal
                // (a true no-op) cancels against the on-disk failure.
                let content_digest = failure
                    .content_hash
                    .get(..12)
                    .unwrap_or(&failure.content_hash)
                    .to_string();
                Violation::new(
                    self.id(),
                    self.severity(),
                    None,
                    Some(failure.path.clone()),
                    ViolationDetails::ParseFailure {
                        reason: failure.message.clone(),
                        content_digest,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::{FieldParseIssue, Graph, GraphMeta, Kind, Node, ParseFailure, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node_with_issues(id: &str, issues: Vec<FieldParseIssue>) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.to_string(),
            kind: Kind::new("generic"),
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
            parse_issues: issues,
            inferred_fields: vec![],
        }
    }

    fn graph_with(nodes: Vec<Node>, failures: Vec<ParseFailure>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![], vec![], failures, GraphMeta::default())
    }

    #[test]
    fn field_parse_emits_one_error_violation_per_issue() {
        let node = node_with_issues(
            "a",
            vec![
                FieldParseIssue {
                    field: "created".into(),
                    expected: "date (YYYY-MM-DD)".into(),
                    found: "string \"yesterday\"".into(),
                },
                FieldParseIssue {
                    field: "orphan_ok".into(),
                    expected: "bool".into(),
                    found: "string \"maybe\"".into(),
                },
            ],
        );
        let graph = graph_with(vec![node, node_with_issues("clean", vec![])], vec![]);
        let config = Config::default();
        let v = FieldParseRule.check(&super::super::test_ctx(&graph, &config));
        assert_eq!(v.len(), 2);
        for violation in &v {
            assert_eq!(violation.rule_id, "field_parse");
            assert_eq!(violation.severity, Severity::Error);
            assert_eq!(violation.node_id.as_deref(), Some("a"));
            assert_eq!(violation.path.as_deref(), Some("a.md"));
        }
        assert!(v[0].message.contains("\"created\""));
        assert!(v[0].message.contains("expected date (YYYY-MM-DD)"));
        assert!(v[0].message.contains("reads as absent"));
    }

    #[test]
    fn parse_failure_emits_node_less_error_violations() {
        let graph = graph_with(
            vec![],
            vec![ParseFailure {
                path: "docs/bad.md".into(),
                message: "parse error at docs/bad.md: yaml: …".into(),
                content_hash: "abc".into(),
            }],
        );
        let config = Config::default();
        let v = ParseFailureRule.check(&super::super::test_ctx(&graph, &config));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule_id, "parse_failure");
        assert_eq!(v[0].severity, Severity::Error);
        assert_eq!(v[0].node_id, None, "no node exists to attribute to");
        assert_eq!(v[0].path.as_deref(), Some("docs/bad.md"));
        assert!(v[0].message.contains("yaml"));
        assert!(
            v[0].message.contains("(content abc)"),
            "the message carries the content digest so two different broken \
             byte-states at one path produce unequal violations: {}",
            v[0].message
        );
    }

    #[test]
    fn parse_failure_violations_differ_per_byte_state() {
        // Violation equality is the substrate of the `--content` delta:
        // the same error class over different bytes must not compare
        // equal, or a proposal could swap one broken state for another
        // and cancel against the on-disk failure.
        let failure = |hash: &str| ParseFailure {
            path: "docs/bad.md".into(),
            message: "parse error at docs/bad.md: frontmatter missing closing delimiter".into(),
            content_hash: hash.into(),
        };
        let config = Config::default();
        let violation = |hash: &str| {
            ParseFailureRule
                .check(&super::super::test_ctx(
                    &graph_with(vec![], vec![failure(hash)]),
                    &config,
                ))
                .remove(0)
        };
        let a = violation("aaaaaaaaaaaaaaaa");
        let b = violation("bbbbbbbbbbbbbbbb");
        assert_ne!(a, b, "same error class, different bytes — unequal");
        assert_eq!(a, violation("aaaaaaaaaaaaaaaa"), "same bytes — equal");
    }

    #[test]
    fn both_rules_are_always_registered_builtins() {
        // Unconditional registration is what makes the rules appear in
        // `export rules` automatically — check and the manifest read
        // the same registry.
        let config = Config::default();
        let ids: Vec<String> = crate::rules::registered_rules(&config)
            .iter()
            .map(|r| r.id().to_string())
            .collect();
        assert!(ids.contains(&"field_parse".to_string()));
        assert!(ids.contains(&"parse_failure".to_string()));

        let manifest = crate::export::export_rules(&config);
        for id in ["field_parse", "parse_failure"] {
            let entry = manifest
                .rules
                .iter()
                .find(|r| r.id == id)
                .unwrap_or_else(|| panic!("{id} must be in the rules manifest"));
            assert_eq!(entry.source, crate::rules::RuleSource::Builtin);
            assert_eq!(entry.severity, Severity::Error);
        }
    }
}
