//! Conform structured-body lines to closed vocabularies.
//!
//! Each `[[rules.body_line]]` config block declares a regex with
//! named captures; for every match outside a fenced or indented
//! code block, the captures named in `enums` must hold values from
//! their declared allowed sets. Non-matching lines are silently
//! ignored — this rule enforces *conformance when matched*, not
//! presence. Project-specific vocabularies (spec phase gates, ADR
//! decision values, conventional-commit categories) live entirely
//! in `nodex.toml`, never in code.

use std::path::Path;

use regex::Regex;

use crate::config::{BodyLineRuleConfig, Config};
use crate::model::Node;
use crate::parser::body::iter_body_lines;
use crate::parser::frontmatter::split_frontmatter;
use crate::path_guard;

use super::{Rule, RuleContext, Severity, Violation};

pub struct BodyLineRule;

impl Rule for BodyLineRule {
    fn id(&self) -> &str {
        "body_line"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        !ctx.config.rules.body_line.is_empty()
    }

    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        "[[rules.body_line]] not configured".to_string()
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let blocks = &ctx.config.rules.body_line;
        if blocks.is_empty() {
            return Vec::new();
        }
        // Compile every pattern once — `Config::validate` already
        // guarantees compilation succeeds, so `expect` here is an
        // invariant statement, not a fail-stop.
        let compiled: Vec<(&BodyLineRuleConfig, Regex)> = blocks
            .iter()
            .map(|b| {
                let re = Regex::new(&b.pattern)
                    .expect("body_line patterns are validated by Config::load");
                (b, re)
            })
            .collect();

        let mut violations = Vec::new();
        for node in ctx.graph.nodes().values() {
            // Per-node block filter via `applies_to_kind`. Empty list
            // means "any kind" — no work skipped, no false positives.
            let applicable: Vec<&(&BodyLineRuleConfig, Regex)> = compiled
                .iter()
                .filter(|(b, _)| {
                    b.applies_to_kind.is_empty()
                        || b.applies_to_kind.iter().any(|k| k == node.kind.as_str())
                })
                .collect();
            if applicable.is_empty() {
                continue;
            }

            let Some(body) = read_body(ctx.root, node) else {
                continue;
            };
            scan_node(node, &body, &applicable, ctx.config, &mut violations);
        }

        violations
    }
}

/// Read the body of a node, returning `None` when the file is gone
/// or unreadable. Frontmatter is stripped via the same parser the
/// graph itself used, so an empty body and a body-less file land on
/// the same path.
fn read_body(root: &Path, node: &Node) -> Option<String> {
    let abs = root.join(&node.path);
    let content = std::fs::read_to_string(&abs).ok()?;
    let (_, body) = split_frontmatter(&content);
    Some(body.to_string())
}

fn scan_node(
    node: &Node,
    body: &str,
    applicable: &[&(&BodyLineRuleConfig, Regex)],
    _config: &Config,
    out: &mut Vec<Violation>,
) {
    for body_line in iter_body_lines(body) {
        for (block, re) in applicable {
            for caps in re.captures_iter(body_line.text) {
                for (capture_name, allowed) in &block.enums {
                    let Some(m) = caps.name(capture_name) else {
                        continue;
                    };
                    if !allowed.iter().any(|v| v == m.as_str()) {
                        out.push(Violation {
                            rule_id: format!("body_line/{}", block.name),
                            severity: Severity::Error,
                            node_id: Some(node.id.clone()),
                            path: Some(path_guard::forward_string(&node.path)),
                            message: format!(
                                "line {ln}: capture {cap:?} value {val:?} is not in declared \
                                 enum {allowed:?}",
                                ln = body_line.number,
                                cap = capture_name,
                                val = m.as_str(),
                                allowed = allowed
                            ),
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BodyLineRuleConfig, Config};
    use crate::model::{Graph, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_node(id: &str, kind: &str, rel_path: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(rel_path),
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
        }
    }

    fn write_doc(root: &Path, rel: &str, body: &str) {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&abs, body).unwrap();
    }

    fn build_graph(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![])
    }

    fn cfg_with(block: BodyLineRuleConfig) -> Config {
        Config {
            kinds: crate::config::KindsConfig {
                allowed: vec!["spec".into(), "generic".into()],
            },
            rules: crate::config::RulesConfig {
                naming: vec![],
                frontmatter_immutable: None,
                body_line: vec![block],
            },
            ..Config::default()
        }
    }

    fn ctx<'a>(graph: &'a Graph, config: &'a Config, root: &'a Path) -> RuleContext<'a> {
        RuleContext {
            graph,
            config,
            root,
            since: None,
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
            pattern: r"^- \*\*(?P<gate>[a-z-]+)\*\*:".into(),
            applies_to_kind: vec!["spec".into()],
            enums,
        }
    }

    #[test]
    fn inert_when_no_blocks_configured() {
        let tmp = TempDir::new().unwrap();
        let g = build_graph(vec![]);
        let cfg = Config::default();
        let rule = BodyLineRule;
        assert!(!rule.is_applicable(&ctx(&g, &cfg, tmp.path())));
    }

    #[test]
    fn passes_when_capture_value_in_enum() {
        let tmp = TempDir::new().unwrap();
        write_doc(tmp.path(), "specs/a.md", "- **scope**: settled\n");
        let g = build_graph(vec![make_node("a", "spec", "specs/a.md")]);
        let cfg = cfg_with(decision_log_block());
        let rule = BodyLineRule;
        assert!(rule.check(&ctx(&g, &cfg, tmp.path())).is_empty());
    }

    #[test]
    fn fires_when_capture_value_outside_enum() {
        let tmp = TempDir::new().unwrap();
        write_doc(tmp.path(), "specs/a.md", "- **scop**: typo here\n");
        let g = build_graph(vec![make_node("a", "spec", "specs/a.md")]);
        let cfg = cfg_with(decision_log_block());
        let rule = BodyLineRule;
        let vs = rule.check(&ctx(&g, &cfg, tmp.path()));
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].rule_id, "body_line/spec-decision-log");
        assert_eq!(vs[0].node_id.as_deref(), Some("a"));
        assert!(vs[0].message.contains("scop"));
        assert!(vs[0].message.contains("line 1"));
    }

    #[test]
    fn skips_node_outside_applies_to_kind() {
        let tmp = TempDir::new().unwrap();
        // A `generic` doc with the same body — block is restricted
        // to kind=spec, so no violation should fire here even though
        // the literal text would match if applied.
        write_doc(tmp.path(), "g/a.md", "- **scop**: typo\n");
        let g = build_graph(vec![make_node("a", "generic", "g/a.md")]);
        let cfg = cfg_with(decision_log_block());
        let rule = BodyLineRule;
        assert!(rule.check(&ctx(&g, &cfg, tmp.path())).is_empty());
    }

    #[test]
    fn skips_code_block_lines() {
        let tmp = TempDir::new().unwrap();
        // The bad token is inside a fenced code block — must not fire.
        write_doc(
            tmp.path(),
            "specs/a.md",
            "Intro\n```\n- **scop**: example syntax\n```\nReal: - **scope**: ok\n",
        );
        let g = build_graph(vec![make_node("a", "spec", "specs/a.md")]);
        let cfg = cfg_with(decision_log_block());
        let rule = BodyLineRule;
        assert!(rule.check(&ctx(&g, &cfg, tmp.path())).is_empty());
    }

    #[test]
    fn ignores_non_matching_lines() {
        let tmp = TempDir::new().unwrap();
        // No bullet matches the pattern — silence.
        write_doc(tmp.path(), "specs/a.md", "just prose, no bullets here\n");
        let g = build_graph(vec![make_node("a", "spec", "specs/a.md")]);
        let cfg = cfg_with(decision_log_block());
        let rule = BodyLineRule;
        assert!(rule.check(&ctx(&g, &cfg, tmp.path())).is_empty());
    }

    #[test]
    fn reports_one_violation_per_failed_capture_per_line() {
        let tmp = TempDir::new().unwrap();
        // Multi-capture pattern with two enums; line fails both.
        let mut enums = BTreeMap::new();
        enums.insert("g".into(), vec!["a".into()]);
        enums.insert("d".into(), vec!["x".into()]);
        let block = BodyLineRuleConfig {
            name: "two-caps".into(),
            pattern: r"(?P<g>\w+):(?P<d>\w+)".into(),
            applies_to_kind: vec![],
            enums,
        };
        write_doc(tmp.path(), "n.md", "bad:wrong\n");
        let g = build_graph(vec![make_node("n", "generic", "n.md")]);
        let cfg = Config {
            rules: crate::config::RulesConfig {
                naming: vec![],
                frontmatter_immutable: None,
                body_line: vec![block],
            },
            ..Config::default()
        };
        let rule = BodyLineRule;
        let vs = rule.check(&ctx(&g, &cfg, tmp.path()));
        // Two violations: one for capture `d` (=wrong), one for `g` (=bad).
        assert_eq!(vs.len(), 2);
        assert!(vs.iter().all(|v| v.rule_id == "body_line/two-caps"));
        let messages: Vec<&str> = vs.iter().map(|v| v.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("\"bad\"")));
        assert!(messages.iter().any(|m| m.contains("\"wrong\"")));
    }
}
