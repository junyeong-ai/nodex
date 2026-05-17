//! Conform structured body blocks to closed vocabularies.
//!
//! One [`BodyBlockRule`] instance per `[[rules.body_block]]` config
//! block. Each instance validates the pre-extracted body-block
//! matches whose `rule_name` belongs to its block, checking every
//! capture listed in `enums` against the declared allowed set.
//!
//! Pure graph consumer — no filesystem access at check time. The
//! frame extraction is owned by
//! [`crate::parser::body::extract_body_block_matches`] and lives on
//! the graph as [`crate::model::BodyBlockMatch`] records, symmetric
//! with body_line and annotations. This keeps every check-time rule
//! a pure function of `(graph, config)`.
//!
//! Captures come from the *start* line's regex match — body_block is
//! a framing primitive, not a per-line scanner. A project that needs
//! both framing and per-line conformance composes
//! `[[rules.body_block]]` with `[[rules.body_line]]`; the two rules
//! see independent match sets.

use serde_json::{Map, Value, json};

use crate::config::BodyBlockRuleConfig;

use super::{Rule, RuleContext, RuleSource, Severity, Violation};

/// One `[[rules.body_block]]` block as a `Rule` trait object.
pub struct BodyBlockRule {
    config: BodyBlockRuleConfig,
    qualified_id: String,
}

impl BodyBlockRule {
    /// Construct a rule instance for one config block. `qualified_id`
    /// is cached so [`Rule::id`] returns `&str` without allocating
    /// per call — same convention as
    /// [`crate::rules::body_line::BodyLineRule`].
    pub fn new(config: BodyBlockRuleConfig) -> Self {
        let qualified_id = format!("body_block/{}", config.name);
        Self {
            config,
            qualified_id,
        }
    }
}

impl Rule for BodyBlockRule {
    fn id(&self) -> &str {
        &self.qualified_id
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Body-block conformance: captures from the `start_pattern` match \
         must carry values from declared enums"
    }

    fn source(&self) -> RuleSource {
        RuleSource::Config
    }

    fn params(&self, _config: &crate::config::Config) -> Map<String, Value> {
        // Per-block params mirror the config's public surface so the
        // manifest entry is self-describing — same shape body_line
        // uses.
        let mut m = Map::new();
        m.insert("start_pattern".into(), json!(self.config.start_pattern));
        m.insert("end_pattern".into(), json!(self.config.end_pattern));
        m.insert("applies_to_kind".into(), json!(self.config.applies.kinds));
        m.insert(
            "applies_to_status".into(),
            json!(self.config.applies.statuses),
        );
        m.insert("applies_to_tag".into(), json!(self.config.applies.tags));
        m.insert("enums".into(), json!(self.config.enums));
        m
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let matches = ctx
            .graph
            .body_block_matches_for_rule(self.config.name.as_str());
        if matches.is_empty() {
            return Vec::new();
        }

        let mut violations = Vec::new();
        for m in matches {
            // Defensive: the match's source must still be a graph node.
            // `body_block_matches_for_rule` returns entries for the
            // configured block, but a node disappearing between build
            // and check would surface here.
            let Some(node) = ctx.graph.node(&m.source_id) else {
                continue;
            };
            for (capture_name, allowed) in &self.config.enums {
                let Some(value) = m.captures.get(capture_name) else {
                    continue;
                };
                if !allowed.iter().any(|v| v == value) {
                    violations.push(Violation {
                        rule_id: self.qualified_id.clone(),
                        severity: Severity::Error,
                        node_id: Some(node.id.clone()),
                        path: Some(crate::path_guard::forward_string(&node.path)),
                        message: format!(
                            "lines {start}-{end}: capture {cap:?} value {val:?} is not in \
                             declared enum {allowed:?}",
                            start = m.start_line,
                            end = m.end_line,
                            cap = capture_name,
                            val = value,
                            allowed = allowed
                        ),
                    });
                }
            }
        }
        violations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApplyTo;
    use crate::config::{BodyBlockRuleConfig, Config};
    use crate::model::{BodyBlockMatch, Graph, Kind, Node, Status};
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
        }
    }

    fn graph_with(nodes: Vec<Node>, matches: Vec<BodyBlockMatch>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![], vec![], matches)
    }

    fn block_match(
        rule: &str,
        source: &str,
        start: usize,
        end: usize,
        captures: &[(&str, &str)],
    ) -> BodyBlockMatch {
        BodyBlockMatch {
            source_id: source.into(),
            rule_name: rule.into(),
            start_line: start,
            end_line: end,
            captures: captures
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    fn decision_block_config() -> BodyBlockRuleConfig {
        let mut enums = BTreeMap::new();
        enums.insert(
            "status".into(),
            vec!["accepted".into(), "rejected".into(), "deferred".into()],
        );
        BodyBlockRuleConfig {
            name: "adr-decision".into(),
            start_pattern: r"^## Decision \((?P<status>[a-z]+)\)".into(),
            end_pattern: r"^## ".into(),
            applies: ApplyTo::default(),
            enums,
        }
    }

    fn ctx<'a>(graph: &'a Graph, config: &'a Config) -> RuleContext<'a> {
        RuleContext {
            graph,
            config,
            root: Path::new("."),
            since: None,
            scope: super::super::CheckScope::Project,
        }
    }

    #[test]
    fn rule_id_is_qualified_with_block_name() {
        let rule = BodyBlockRule::new(decision_block_config());
        assert_eq!(rule.id(), "body_block/adr-decision");
    }

    #[test]
    fn no_matches_yields_no_violations() {
        let g = graph_with(vec![node("a", "adr")], vec![]);
        let cfg = Config::default();
        let rule = BodyBlockRule::new(decision_block_config());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }

    #[test]
    fn passes_when_capture_value_in_enum() {
        let g = graph_with(
            vec![node("a", "adr")],
            vec![block_match(
                "adr-decision",
                "a",
                3,
                10,
                &[("status", "accepted")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyBlockRule::new(decision_block_config());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }

    #[test]
    fn fires_when_capture_value_outside_enum() {
        // A typo in the start-line capture ("acceptd") fails the
        // enum check. The violation message names both the line
        // span and the offending value so reviewers can navigate.
        let g = graph_with(
            vec![node("a", "adr")],
            vec![block_match(
                "adr-decision",
                "a",
                3,
                10,
                &[("status", "acceptd")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyBlockRule::new(decision_block_config());
        let vs = rule.check(&ctx(&g, &cfg));
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].rule_id, "body_block/adr-decision");
        assert_eq!(vs[0].node_id.as_deref(), Some("a"));
        assert!(vs[0].message.contains("3-10"));
        assert!(vs[0].message.contains("acceptd"));
    }

    #[test]
    fn ignores_matches_from_other_rules() {
        // Match recorded under "other-block"; this instance covers
        // "adr-decision" only and must not fire on out-of-scope match
        // records — same isolation body_line provides.
        let g = graph_with(
            vec![node("a", "adr")],
            vec![block_match(
                "other-block",
                "a",
                1,
                2,
                &[("status", "bogus")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyBlockRule::new(decision_block_config());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }

    #[test]
    fn missing_enum_capture_is_silently_skipped() {
        // The block was framed but its start line didn't bind the
        // `status` capture (rare; would mean the regex matched via a
        // different alternation branch). No enum violation can fire
        // on a missing capture — defensive guard against assertions
        // on the wrong premise.
        let g = graph_with(
            vec![node("a", "adr")],
            vec![block_match(
                "adr-decision",
                "a",
                1,
                3,
                &[("other", "value")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyBlockRule::new(decision_block_config());
        assert!(rule.check(&ctx(&g, &cfg)).is_empty());
    }

    #[test]
    fn one_violation_per_failed_capture_per_block() {
        // Same pattern as body_line: a single block carrying two
        // failing captures fires two violations so a reviewer sees
        // every problem at once.
        let mut enums = BTreeMap::new();
        enums.insert("a".into(), vec!["ok".into()]);
        enums.insert("b".into(), vec!["ok".into()]);
        let block = BodyBlockRuleConfig {
            name: "twocaps".into(),
            start_pattern: r"^# (?P<a>\w+) (?P<b>\w+)".into(),
            end_pattern: r"^# ".into(),
            applies: ApplyTo::default(),
            enums,
        };
        let g = graph_with(
            vec![node("n", "generic")],
            vec![block_match(
                "twocaps",
                "n",
                1,
                3,
                &[("a", "bad"), ("b", "wrong")],
            )],
        );
        let cfg = Config::default();
        let rule = BodyBlockRule::new(block);
        let vs = rule.check(&ctx(&g, &cfg));
        assert_eq!(vs.len(), 2);
        let messages: Vec<&str> = vs.iter().map(|v| v.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("\"bad\"")));
        assert!(messages.iter().any(|m| m.contains("\"wrong\"")));
    }
}
