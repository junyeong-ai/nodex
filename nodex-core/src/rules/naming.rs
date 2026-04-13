use globset::Glob;
use regex::Regex;
use std::collections::BTreeMap;

use crate::config::Config;
use crate::model::Graph;

use super::{Rule, Severity, Violation};

/// Check that filenames match the configured pattern for their directory.
pub struct FilenamePattern;

impl Rule for FilenamePattern {
    fn id(&self) -> &str {
        "filename_pattern"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let mut violations = Vec::new();

        for rule in &config.rules.naming {
            let Ok(glob) = Glob::new(&rule.glob) else {
                continue;
            };
            let matcher = glob.compile_matcher();
            let Ok(re) = Regex::new(&rule.pattern) else {
                continue;
            };

            for node in graph.nodes().values() {
                let path_str = node.path.to_string_lossy().replace('\\', "/");
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
pub struct SequentialNumbering;

impl Rule for SequentialNumbering {
    fn id(&self) -> &str {
        "sequential_numbering"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let mut violations = Vec::new();
        let number_re = Regex::new(r"^(\d+)").expect("hardcoded regex is valid");

        for rule in &config.rules.naming {
            if !rule.sequential {
                continue;
            }

            let Ok(glob) = Glob::new(&rule.glob) else {
                continue;
            };
            let matcher = glob.compile_matcher();

            let mut numbers: Vec<(u32, String)> = Vec::new();

            for node in graph.nodes().values() {
                let path_str = node.path.to_string_lossy().replace('\\', "/");
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
pub struct UniqueNumbering;

impl Rule for UniqueNumbering {
    fn id(&self) -> &str {
        "unique_numbering"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let mut violations = Vec::new();
        let number_re = Regex::new(r"^(\d+)").expect("hardcoded regex is valid");

        for rule in &config.rules.naming {
            if !rule.unique {
                continue;
            }

            let Ok(glob) = Glob::new(&rule.glob) else {
                continue;
            };
            let matcher = glob.compile_matcher();

            let mut seen: BTreeMap<u32, Vec<String>> = BTreeMap::new();

            for node in graph.nodes().values() {
                let path_str = node.path.to_string_lossy().replace('\\', "/");
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
