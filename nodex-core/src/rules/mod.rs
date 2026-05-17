pub mod freshness;
pub mod frontmatter_immutable;
pub mod git_drift;
pub mod naming;
pub mod schema;

use std::path::Path;

use crate::config::Config;
use crate::diff::GraphDiff;
use crate::error::{Error, Result};
use crate::model::Graph;

/// Verify the runtime prerequisites of every opt-in rule. Today only
/// `git_drift_threshold` has any (git on PATH + git work tree at
/// `root`). Call once after [`Config::load`] and before any command
/// that could exercise the rules — failures surface as
/// [`Error::Config`] so the operator sees `CONFIG_ERROR` and exit 2,
/// not a buried check violation.
pub fn preflight(config: &Config, root: &Path) -> Result<()> {
    if config.detection.git_drift_threshold.is_some()
        && let Err(reason) = git_drift::probe_environment(root)
    {
        return Err(Error::Config(format!(
            "detection.git_drift_threshold is set but {reason}; \
             install git and run inside a git work tree, or remove the threshold"
        )));
    }
    Ok(())
}

/// Severity of a rule violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// A single rule violation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Violation {
    pub rule_id: String,
    pub severity: Severity,
    pub node_id: Option<String>,
    pub path: Option<String>,
    pub message: String,
}

/// Everything a [`Rule`] is allowed to read while evaluating. Bundling
/// these into a single context lets the trait grow new inputs (file
/// mtime cache, git history reader, …) without churning every
/// implementor's signature.
pub struct RuleContext<'a> {
    pub graph: &'a Graph,
    pub config: &'a Config,
    pub root: &'a Path,
    /// Structural delta from a past ref to the current graph. `None`
    /// for a plain `nodex check`; `Some(_)` when invoked with
    /// `check --since <ref>`. Rules whose semantic requires "this is
    /// what changed" (e.g. `frontmatter_immutable`) declare themselves
    /// non-applicable via [`Rule::is_applicable`] when this is `None`.
    pub since: Option<&'a GraphDiff>,
}

/// One rule that the runner declined to evaluate, with a one-line reason.
/// Symmetric to [`Violation`] — silent skipping would let a strict-mode
/// rule appear to "pass" when it never actually ran.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkippedRule {
    pub rule_id: String,
    pub reason: String,
}

/// Trait for validation rules.
pub trait Rule: Send + Sync {
    fn id(&self) -> &str;
    fn severity(&self) -> Severity;
    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation>;
    /// True when this rule's prerequisites are satisfied for the given
    /// context. Default: always applicable. Override for rules whose
    /// semantics requires a diff context, opt-in environment, etc.
    fn is_applicable(&self, _ctx: &RuleContext<'_>) -> bool {
        true
    }
    /// One-line reason returned when [`Rule::is_applicable`] returns
    /// false. Empty by default — required only for rules that override
    /// `is_applicable`.
    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        String::new()
    }
}

/// Test-only helper: build a [`RuleContext`] with a placeholder root.
/// Lives here so each rule's unit tests can construct a context
/// without redefining the same boilerplate.
#[cfg(test)]
pub(crate) fn test_ctx<'a>(graph: &'a Graph, config: &'a Config) -> RuleContext<'a> {
    RuleContext {
        graph,
        config,
        root: Path::new("."),
        since: None,
    }
}

/// Result of [`check_all`] — both the fires (`violations`) and the
/// declined fires (`skipped`). Surfacing skips alongside violations is
/// the only honest way to express "this rule was inert here" without
/// the silent-skip failure mode that
/// `.claude/rules/config-driven.md` calls out.
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct CheckReport {
    pub violations: Vec<Violation>,
    pub skipped: Vec<SkippedRule>,
}

/// Run all built-in rules. Rules whose
/// [`Rule::is_applicable`] returns `false` are listed in
/// [`CheckReport::skipped`] with their reason instead of being silently
/// dropped.
pub fn check_all(graph: &Graph, config: &Config, root: &Path) -> CheckReport {
    check_with_diff(graph, config, root, None)
}

/// Like [`check_all`] but with a diff context. Drives `check --since`
/// and lets diff-aware rules (e.g. `frontmatter_immutable`) opt in.
pub fn check_with_diff(
    graph: &Graph,
    config: &Config,
    root: &Path,
    since: Option<&GraphDiff>,
) -> CheckReport {
    let ctx = RuleContext {
        graph,
        config,
        root,
        since,
    };

    let rules: Vec<Box<dyn Rule>> = vec![
        // Schema family — required-field presence + declarative type,
        // enum, cross-field, and (under strict mode) unknown-key
        // detection. All driven by `nodex.toml [schema]`.
        Box::new(schema::RequiredFieldRule),
        Box::new(schema::FieldTypeRule),
        Box::new(schema::FieldEnumRule),
        Box::new(schema::CrossFieldRule),
        Box::new(schema::UnknownFieldRule),
        // Freshness family — calendar-based and (optionally) git-aware.
        Box::new(freshness::StaleReviewRule),
        Box::new(git_drift::GitDriftRule),
        // Naming family.
        Box::new(naming::FilenamePatternRule),
        Box::new(naming::SequentialNumberingRule),
        Box::new(naming::UniqueNumberingRule),
        // Diff-aware family.
        Box::new(frontmatter_immutable::FrontmatterImmutableRule),
    ];

    let mut violations: Vec<Violation> = Vec::new();
    let mut skipped: Vec<SkippedRule> = Vec::new();
    for rule in &rules {
        if rule.is_applicable(&ctx) {
            violations.extend(rule.check(&ctx));
        } else {
            skipped.push(SkippedRule {
                rule_id: rule.id().to_string(),
                reason: rule.skip_reason(&ctx),
            });
        }
    }

    violations.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    skipped.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));

    CheckReport {
        violations,
        skipped,
    }
}
