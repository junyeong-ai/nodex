pub mod freshness;
pub mod git_drift;
pub mod naming;
pub mod schema;

use std::path::Path;

use crate::config::Config;
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
}

/// Trait for validation rules.
pub trait Rule: Send + Sync {
    fn id(&self) -> &str;
    fn severity(&self) -> Severity;
    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation>;
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
    }
}

/// Run all built-in rules and return violations, sorted deterministically.
pub fn check_all(graph: &Graph, config: &Config, root: &Path) -> Vec<Violation> {
    let ctx = RuleContext {
        graph,
        config,
        root,
    };

    let rules: Vec<Box<dyn Rule>> = vec![
        // Schema family — required-field presence + declarative type,
        // enum, and cross-field constraints driven by nodex.toml.
        Box::new(schema::RequiredFieldRule),
        Box::new(schema::FieldTypeRule),
        Box::new(schema::FieldEnumRule),
        Box::new(schema::CrossFieldRule),
        // Freshness family — calendar-based and (optionally) git-aware.
        Box::new(freshness::StaleReviewRule),
        Box::new(git_drift::GitDriftRule),
        // Naming family.
        Box::new(naming::FilenamePatternRule),
        Box::new(naming::SequentialNumberingRule),
        Box::new(naming::UniqueNumberingRule),
    ];

    let mut violations: Vec<Violation> = rules.iter().flat_map(|rule| rule.check(&ctx)).collect();

    violations.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    violations
}
