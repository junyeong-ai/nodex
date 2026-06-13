//! Conform structured-body lines to closed vocabularies.
//!
//! One [`BodyLineRule`] instance per `[[rules.body_line]]` config
//! block. Each instance validates the pre-extracted body-line
//! matches whose `rule_name` belongs to its block, checking every
//! capture listed in `enums` against the declared allowed set.
//!
//! Pure graph consumer — no filesystem access at check time. The
//! match extraction is owned by `parser::body::extract_body_line_matches`
//! and lives on the graph as [`crate::model::BodyLineMatch`] records,
//! symmetric with annotations. This keeps every check-time rule a
//! pure function of `(graph, config)`.

use serde_json::{Map, Value, json};

use crate::config::BodyLineRuleConfig;

use super::{Rule, RuleContext, RuleSource, Severity, Violation, ViolationDetails};

/// One `[[rules.body_line]]` block as a `Rule` trait object.
pub struct BodyLineRule {
    config: BodyLineRuleConfig,
    qualified_id: String,
}

impl BodyLineRule {
    /// Construct a rule instance for one config block. `qualified_id`
    /// is cached so [`Rule::id`] can return `&str` without allocating
    /// every call.
    pub fn new(config: BodyLineRuleConfig) -> Self {
        let qualified_id = format!("body_line/{}", config.name);
        Self {
            config,
            qualified_id,
        }
    }
}

impl Rule for BodyLineRule {
    fn id(&self) -> &str {
        &self.qualified_id
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Body-line conformance: lines matching `pattern` outside code blocks \
         must carry capture values from declared enums"
    }

    fn source(&self) -> RuleSource {
        RuleSource::Config
    }

    fn params(&self, _config: &crate::config::Config) -> Map<String, Value> {
        // Per-block params come from the rule's own captured config
        // — the global Config is irrelevant here because each instance
        // carries its own block. Mirrors `BodyLineRuleConfig`'s public
        // surface so the manifest entry is self-describing.
        let mut m = Map::new();
        m.insert("pattern".into(), json!(self.config.pattern));
        m.insert("kinds".into(), json!(self.config.kinds));
        m.insert("enums".into(), json!(self.config.enums));
        m
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let matches = ctx
            .graph
            .body_line_matches_for_rule(self.config.name.as_str());
        if matches.is_empty() {
            return Vec::new();
        }

        let mut violations = Vec::new();
        for m in matches {
            // The match's source must still be a graph node — defensive
            // against any partial state. `body_line_matches_for_rule`
            // returns entries for the configured block, but a node
            // disappearing between build and check would surface here.
            let Some(node) = ctx.graph.node(&m.source) else {
                continue;
            };
            for (capture_name, allowed) in &self.config.enums {
                let Some(value) = m.captures.get(capture_name) else {
                    continue;
                };
                if !allowed.iter().any(|v| v == value) {
                    violations.push(Violation::new(
                        self.qualified_id.clone(),
                        Severity::Error,
                        Some(node.id.clone()),
                        Some(crate::path_guard::forward_string(&node.path)),
                        ViolationDetails::BodyLine {
                            line: m.line,
                            capture: capture_name.clone(),
                            value: value.clone(),
                            allowed: allowed.clone(),
                        },
                    ));
                }
            }
        }
        violations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BodyLineRuleConfig, Config};
    use crate::model::{BodyLineMatch, Graph, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn node(id: &str, kind: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.into(),
            kind: Kind::new(kind),
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
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn graph_with(nodes: Vec<Node>, matches: Vec<BodyLineMatch>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(
            map,
            vec![],
            vec![],
            matches,
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    fn body_match(
        rule: &str,
        source: &str,
        line: usize,
        captures: &[(&str, &str)],
    ) -> BodyLineMatch {
        BodyLineMatch {
            source: source.into(),
            rule_name: rule.into(),
            line,
            captures: captures
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    fn decision_log_block() -> BodyLineRuleConfig {
        let mut enums = BTreeMap::new();
        enums.insert(
            "gate".into(),
            vec!["scope".into(), "design".into(), "rollout".into()],
        );
        BodyLineRuleConfig {
            name: "spec-decision-log".into(),
            pattern: r"^- \*\*(?P<gate>[a-z-]+)\*\*".into(),
            kinds: vec!["spec".into()],
            enums,
        }
    }

    fn ctx<'a>(graph: &'a Graph, config: &'a Config) -> RuleContext<'a> {
        RuleContext {
            graph,
            config,
            root: Path::new("."),
            since: None,
        }
    }

    #[test]
    fn rule_id_is_qualified_with_block_name() {
        let rule = BodyLineRule::new(decision_log_block());
        assert_eq!(rule.id(), "body_line/spec-decision-log");
    }

    #[test]
    fn no_matches_yields_no_violations() {
        let g = graph_with(vec![node("a", "spec")], vec![]);
        let cfg = Config::default();
        let rule = BodyLineRule::new(decision_log_block());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }

    #[test]
    fn passes_when_capture_value_in_enum() {
        let g = graph_with(
            vec![node("a", "spec")],
            vec![body_match(
                "spec-decision-log",
                "a",
                1,
                &[("gate", "scope")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyLineRule::new(decision_log_block());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }

    #[test]
    fn fires_when_capture_value_outside_enum() {
        let g = graph_with(
            vec![node("a", "spec")],
            vec![body_match("spec-decision-log", "a", 7, &[("gate", "scop")])],
        );
        let cfg = Config::default();
        let rule = BodyLineRule::new(decision_log_block());
        let vs = rule.check(&ctx(&g, &cfg));
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].rule_id, "body_line/spec-decision-log");
        assert_eq!(vs[0].node_id.as_deref(), Some("a"));
        assert!(vs[0].message.contains("scop"));
        assert!(vs[0].message.contains("line 7"));
    }

    #[test]
    fn ignores_matches_from_other_rules() {
        // Match recorded under "other-rule"; this BodyLineRule
        // instance covers "spec-decision-log" only and must not
        // fire on out-of-scope match records.
        let g = graph_with(
            vec![node("a", "spec")],
            vec![body_match("other-rule", "a", 1, &[("gate", "bogus")])],
        );
        let cfg = Config::default();
        let rule = BodyLineRule::new(decision_log_block());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }

    #[test]
    fn missing_enum_capture_is_silently_skipped() {
        // The match has no `gate` capture (parser pattern variant
        // didn't bind it). The rule's enum check on `gate` cannot
        // run on a non-existent capture; no violation.
        let g = graph_with(
            vec![node("a", "spec")],
            vec![body_match(
                "spec-decision-log",
                "a",
                1,
                &[("other", "value")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyLineRule::new(decision_log_block());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }

    #[test]
    fn one_violation_per_failed_capture_per_match() {
        let mut enums = BTreeMap::new();
        enums.insert("g".into(), vec!["a".into()]);
        enums.insert("d".into(), vec!["x".into()]);
        let block = BodyLineRuleConfig {
            name: "two-caps".into(),
            pattern: r"(?P<g>\w+):(?P<d>\w+)".into(),
            enums,

            kinds: vec![],
        };
        let g = graph_with(
            vec![node("n", "generic")],
            vec![body_match(
                "two-caps",
                "n",
                1,
                &[("g", "bad"), ("d", "wrong")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyLineRule::new(block);
        let vs = rule.check(&ctx(&g, &cfg));
        assert_eq!(vs.len(), 2);
        let messages: Vec<&str> = vs.iter().map(|v| v.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("\"bad\"")));
        assert!(messages.iter().any(|m| m.contains("\"wrong\"")));
    }

    #[test]
    fn match_for_vanished_source_node_is_ignored() {
        // Defensive: the match's source id isn't in the node map
        // (mid-flight inconsistency). No violation, no panic.
        let g = graph_with(
            vec![],
            vec![body_match(
                "spec-decision-log",
                "ghost",
                1,
                &[("gate", "x")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyLineRule::new(decision_log_block());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }
}
