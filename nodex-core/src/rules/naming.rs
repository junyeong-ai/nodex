use globset::Glob;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;

use super::{Rule, RuleContext, Severity, Violation, ViolationDetails};
use crate::config::{Config, NamingRuleConfig};

/// The first `rules.naming` entry `rel_path` violates — its glob matches
/// the path but the filename fails the pattern — or `None` when the path
/// satisfies every naming rule. The graph-wide [`FilenamePatternRule`]
/// and the write seams (`scaffold`, `rename`) consult the SAME match
/// predicate, so a tool never writes a file its own `filename_pattern`
/// rule would flag.
pub fn first_filename_violation<'a>(
    config: &'a Config,
    rel_path: &Path,
) -> Option<&'a NamingRuleConfig> {
    let path_str = crate::path_guard::forward_string(rel_path);
    let filename = rel_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    config
        .rules
        .naming
        .iter()
        .find(|rule| filename_flagged(rule, &path_str, filename))
}

/// The single match predicate both [`FilenamePatternRule`] and
/// [`first_filename_violation`] use: the rule's glob matches the path and
/// its regex does NOT match the filename.
fn filename_flagged(rule: &NamingRuleConfig, path_str: &str, filename: &str) -> bool {
    let matcher = Glob::new(&rule.glob)
        .expect("validated by Config::load")
        .compile_matcher();
    let re = Regex::new(&rule.pattern).expect("validated by Config::load");
    matcher.is_match(path_str) && !re.is_match(filename)
}

/// The `^(\d+)` leading-digit-run extractor — each caller compiles it once
/// (outside its node loop) and shares it so `scaffold` and the numbering
/// rules read the same prefix.
pub(crate) fn leading_digits_re() -> Regex {
    Regex::new(r"^(\d+)").expect("static regex compiles")
}

/// The sequence number a file contributes under a numbering rule, paired with
/// the width of its digit run — or `None` when the filename is outside the
/// rule's `pattern` (not a numbered file in the operator's own vocabulary) or
/// its leading digits exceed `u64`. The single definition of "what is the
/// number", shared by [`SequentialNumberingRule`] / [`UniqueNumberingRule`]
/// and `scaffold::next_sequence`, so a scaffolded file can never land a number
/// its own `check` would then flag — the write/check self-consistency
/// `.claude/rules/config-driven.md` mandates. The `pattern` matches the file
/// name (it carries the extension); the digit run is read from the stem.
pub(crate) fn numbering_sequence(
    path: &Path,
    pattern_re: &Regex,
    digits_re: &Regex,
) -> Option<(u64, usize)> {
    let filename = path.file_name().and_then(|n| n.to_str())?;
    if !pattern_re.is_match(filename) {
        return None;
    }
    let stem = path.file_stem().and_then(|n| n.to_str())?;
    let digits = digits_re.captures(stem)?.get(1)?.as_str();
    digits.parse::<u64>().ok().map(|n| (n, digits.len()))
}

/// Shared params payload for the naming family — every entry advertises
/// the per-glob patterns it consults so a manifest reader sees which
/// directories the rule applies to.
fn naming_params(config: &crate::config::Config) -> Map<String, Value> {
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

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        naming_params(config)
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
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        Some(node.id.clone()),
                        Some(path_str),
                        ViolationDetails::FilenamePattern {
                            filename: filename.to_string(),
                            pattern: rule.pattern.clone(),
                        },
                    ));
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

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        naming_params(config)
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();
        let digits_re = leading_digits_re();

        for rule in &config.rules.naming {
            if !rule.sequential {
                continue;
            }

            let matcher = Glob::new(&rule.glob)
                .expect("validated by Config::load")
                .compile_matcher();
            let pattern_re = Regex::new(&rule.pattern).expect("validated by Config::load");

            let mut numbers: Vec<(u64, String)> = Vec::new();

            for node in graph.nodes().values() {
                let path_str = crate::path_guard::forward_string(&node.path);
                if !matcher.is_match(&path_str) {
                    continue;
                }
                // Only files whose name matches the rule's own `pattern` are in
                // the numbering domain — the same filter `scaffold` applies when
                // it computes the next number, so a date-prefixed sibling the
                // pattern rejects is never mistaken for a sequence member.
                if let Some((n, _)) = numbering_sequence(&node.path, &pattern_re, &digits_re) {
                    numbers.push((n, path_str));
                }
            }

            numbers.sort_by_key(|(n, _)| *n);

            for window in numbers.windows(2) {
                let (prev, _) = &window[0];
                let (curr, path) = &window[1];
                // Only a true gap (`curr > prev + 1`) is reported here; a
                // duplicate (`curr == prev`) is not a gap and is already
                // surfaced by `UniqueNumberingRule`, so reporting it as
                // "gap N → N" would be a misleading double-report.
                // `saturating_add` guards the `prev == u64::MAX` edge: the
                // expected-next pins at `u64::MAX`, and since `curr` can
                // never exceed it, no spurious gap is reported (and no
                // debug-build overflow panic).
                if *curr > prev.saturating_add(1) {
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        None,
                        Some(path.clone()),
                        ViolationDetails::SequentialNumbering {
                            previous: *prev,
                            current: *curr,
                        },
                    ));
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

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        naming_params(config)
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let (graph, config) = (ctx.graph, ctx.config);
        let mut violations = Vec::new();
        let digits_re = leading_digits_re();

        for rule in &config.rules.naming {
            if !rule.unique {
                continue;
            }

            let matcher = Glob::new(&rule.glob)
                .expect("validated by Config::load")
                .compile_matcher();
            let pattern_re = Regex::new(&rule.pattern).expect("validated by Config::load");

            let mut seen: BTreeMap<u64, Vec<String>> = BTreeMap::new();

            for node in graph.nodes().values() {
                let path_str = crate::path_guard::forward_string(&node.path);
                if !matcher.is_match(&path_str) {
                    continue;
                }
                // Only files whose name matches the rule's own `pattern` count
                // as numbered files — a date-prefixed sibling the pattern
                // rejects is out of the numbering domain (and, if in scope,
                // already flagged by `filename_pattern`).
                if let Some((n, _)) = numbering_sequence(&node.path, &pattern_re, &digits_re) {
                    seen.entry(n).or_default().push(path_str);
                }
            }

            for (num, paths) in &seen {
                if paths.len() > 1 {
                    violations.push(Violation::new(
                        self.id(),
                        self.severity(),
                        None,
                        Some(paths[0].clone()),
                        ViolationDetails::UniqueNumbering {
                            number: *num,
                            paths: paths.clone(),
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
    use crate::config::{Config, NamingRuleConfig};
    use crate::model::{Graph, GraphMeta, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(path: &str) -> Node {
        Node {
            id: path.to_string(),
            path: PathBuf::from(path),
            title: path.to_string(),
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
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn graph(paths: &[&str]) -> Graph {
        let mut map = IndexMap::new();
        for p in paths {
            map.insert(p.to_string(), node(p));
        }
        Graph::new(map, vec![], vec![], vec![], vec![], GraphMeta::default())
    }

    fn config_with_naming(rule: NamingRuleConfig) -> Config {
        let mut config = Config::default();
        config.rules.naming = vec![rule];
        config
    }

    fn run(rule: &dyn Rule, config: &Config, g: &Graph) -> Vec<Violation> {
        let ctx = RuleContext {
            graph: g,
            config,
            root: std::path::Path::new("/tmp"),
            since: None,
        };
        rule.check(&ctx)
    }

    /// An ADR pattern: four digits, a dash, then a letter-led slug. A
    /// date-prefixed sibling (`2026-01-01-…`) fails it — the part after the
    /// year starts with a digit, not `[a-z]`.
    fn adr_rule(sequential: bool, unique: bool) -> NamingRuleConfig {
        NamingRuleConfig {
            glob: "docs/decisions/**".into(),
            pattern: r"^\d{4}-[a-z][a-z0-9-]*\.md$".into(),
            sequential,
            unique,
        }
    }

    #[test]
    fn date_prefixed_sibling_is_outside_the_numbering_domain() {
        // Two real ADRs plus two date-prefixed retros under the same glob.
        // The retros fail the rule's `pattern`, so they are not numbered
        // files: no false `unique_numbering` (both would otherwise read as
        // 2026) and no false `sequential_numbering` gap (2 → 2026). Before
        // the pattern filter, `^(\d+)` captured `2026` from any glob match
        // and broke CI on legitimate content.
        let g = graph(&[
            "docs/decisions/0001-first.md",
            "docs/decisions/0002-second.md",
            "docs/decisions/2026-01-01-standup.md",
            "docs/decisions/2026-02-02-standup.md",
        ]);

        assert!(
            run(
                &UniqueNumberingRule,
                &config_with_naming(adr_rule(false, true)),
                &g
            )
            .is_empty(),
            "date-prefixed siblings must not collide as duplicate number 2026"
        );
        assert!(
            run(
                &SequentialNumberingRule,
                &config_with_naming(adr_rule(true, false)),
                &g
            )
            .is_empty(),
            "a date-prefixed sibling must not open a 2 → 2026 gap"
        );
    }

    #[test]
    fn real_duplicate_number_still_fires() {
        let g = graph(&[
            "docs/decisions/0001-first.md",
            "docs/decisions/0001-also-first.md",
        ]);
        let v = run(
            &UniqueNumberingRule,
            &config_with_naming(adr_rule(false, true)),
            &g,
        );
        assert_eq!(v.len(), 1, "two ADRs numbered 0001 must collide: {v:?}");
        assert!(matches!(
            v[0].details,
            ViolationDetails::UniqueNumbering { number: 1, .. }
        ));
    }

    #[test]
    fn real_gap_still_fires() {
        let g = graph(&[
            "docs/decisions/0001-first.md",
            "docs/decisions/0003-third.md",
        ]);
        let v = run(
            &SequentialNumberingRule,
            &config_with_naming(adr_rule(true, false)),
            &g,
        );
        assert_eq!(v.len(), 1, "1 → 3 is a gap: {v:?}");
        assert!(matches!(
            v[0].details,
            ViolationDetails::SequentialNumbering {
                previous: 1,
                current: 3
            }
        ));
    }

    #[test]
    fn number_beyond_u32_is_not_truncated() {
        // A number that overflows `u32` but fits `u64` (5e9 > u32::MAX) must
        // be read at full width — the same `u64` scaffold uses — so write
        // and check agree on "what is the number".
        let g = graph(&[
            "docs/decisions/5000000000-a.md",
            "docs/decisions/5000000000-b.md",
        ]);
        let rule = NamingRuleConfig {
            glob: "docs/decisions/**".into(),
            pattern: r"^\d+-[a-z][a-z0-9-]*\.md$".into(),
            sequential: false,
            unique: true,
        };
        let v = run(&UniqueNumberingRule, &config_with_naming(rule), &g);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(matches!(
            v[0].details,
            ViolationDetails::UniqueNumbering {
                number: 5_000_000_000,
                ..
            }
        ));
    }
}
