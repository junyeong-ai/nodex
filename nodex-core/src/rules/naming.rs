use globset::Glob;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

use super::{Rule, RuleContext, Severity, Violation};

/// Shared scope payload for the naming family — every entry advertises
/// the per-glob patterns it consults so a manifest reader sees which
/// directories the rule applies to.
fn naming_scope(config: &crate::config::Config) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(
        "patterns".into(),
        json!(
            config
                .rules
                .naming
                .iter()
                .map(|n| json!({"glob": n.glob, "pattern": n.pattern}))
                .collect::<Vec<_>>()
        ),
    );
    m
}

/// Check that filenames match the configured pattern for their directory.
pub struct FilenamePatternRule;

impl Rule for FilenamePatternRule {
    fn id(&self) -> &str {
        "filename_pattern"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Filenames must match their directory's configured regex"
    }

    fn scope(&self, config: &crate::config::Config) -> Map<String, Value> {
        naming_scope(config)
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();

        for rule in &config.rules.naming {
            let matcher = Glob::new(&rule.glob)
                .expect("validated by Config::load")
                .compile_matcher();
            let re = Regex::new(&rule.pattern).expect("validated by Config::load");

            for node in graph.nodes().values() {
                let path_str = crate::path_guard::forward_string(&node.path);
                if !matcher.is_match(&path_str) {
                    continue;
                }

                let filename = node.path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                if !re.is_match(filename) {
                    violations.push(Violation {
                        rule_id: self.id().to_string(),
                        severity: self.severity(),
                        node_id: Some(node.id.clone()),
                        path: Some(path_str),
                        message: format!(
                            "filename {filename:?} does not match pattern {:?}",
                            rule.pattern
                        ),
                    });
                }
            }
        }

        violations
    }
}

/// Check that numbered files in a directory are sequential (no gaps).
pub struct SequentialNumberingRule;

impl Rule for SequentialNumberingRule {
    fn id(&self) -> &str {
        "sequential_numbering"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn description(&self) -> &str {
        "Numbered files in a directory must form a contiguous sequence"
    }

    fn scope(&self, config: &crate::config::Config) -> Map<String, Value> {
        naming_scope(config)
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();
        let number_re = Regex::new(r"^(\d+)").expect("hardcoded regex is valid");

        for rule in &config.rules.naming {
            if !rule.sequential {
                continue;
            }

            let matcher = Glob::new(&rule.glob)
                .expect("validated by Config::load")
                .compile_matcher();

            let mut numbers: Vec<(u32, String)> = Vec::new();

            for node in graph.nodes().values() {
                let path_str = crate::path_guard::forward_string(&node.path);
                if !matcher.is_match(&path_str) {
                    continue;
                }
                let filename = node.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if let Some(caps) = number_re.captures(filename)
                    && let Ok(n) = caps[1].parse::<u32>()
                {
                    numbers.push((n, path_str));
                }
            }

            numbers.sort_by_key(|(n, _)| *n);

            for window in numbers.windows(2) {
                let (prev, _) = &window[0];
                let (curr, path) = &window[1];
                if *curr != prev + 1 {
                    violations.push(Violation {
                        rule_id: self.id().to_string(),
                        severity: self.severity(),
                        node_id: None,
                        path: Some(path.clone()),
                        message: format!("gap in numbering: {prev} → {curr}"),
                    });
                }
            }
        }

        violations
    }
}

/// Check that numbered files have unique numbers.
pub struct UniqueNumberingRule;

impl Rule for UniqueNumberingRule {
    fn id(&self) -> &str {
        "unique_numbering"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn description(&self) -> &str {
        "Numbered files in a directory must have unique numbers"
    }

    fn scope(&self, config: &crate::config::Config) -> Map<String, Value> {
        naming_scope(config)
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();
        let number_re = Regex::new(r"^(\d+)").expect("hardcoded regex is valid");

        for rule in &config.rules.naming {
            if !rule.unique {
                continue;
            }

            let matcher = Glob::new(&rule.glob)
                .expect("validated by Config::load")
                .compile_matcher();

            let mut seen: BTreeMap<u32, Vec<String>> = BTreeMap::new();

            for node in graph.nodes().values() {
                let path_str = crate::path_guard::forward_string(&node.path);
                if !matcher.is_match(&path_str) {
                    continue;
                }
                let filename = node.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if let Some(caps) = number_re.captures(filename)
                    && let Ok(n) = caps[1].parse::<u32>()
                {
                    seen.entry(n).or_default().push(path_str);
                }
            }

            for (num, paths) in &seen {
                if paths.len() > 1 {
                    violations.push(Violation {
                        rule_id: self.id().to_string(),
                        severity: self.severity(),
                        node_id: None,
                        path: Some(paths[0].clone()),
                        message: format!("duplicate number {num} in files: {}", paths.join(", ")),
                    });
                }
            }
        }

        violations
    }
}
