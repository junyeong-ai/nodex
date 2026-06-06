use schemars::JsonSchema;
use serde_json::{Map, Value};

pub mod body_immutable;
pub mod body_line;
pub mod freshness;
pub mod frontmatter_immutable;
pub mod git_drift;
pub mod graph_invariants;
pub mod naming;
pub mod schema;

use std::path::Path;

use crate::config::Config;
use crate::diff::GraphDiff;
use crate::error::{Error, Result};
use crate::model::Graph;

/// Provenance of a [`Rule`] — distinguishes nodex-shipped built-ins
/// from rules instantiated per `[[rules.body_line]]` (or future
/// per-block) config block. Consumers of `nodex export rules` use
/// this to render UIs that say "this rule disappears if the config
/// block is removed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleSource {
    /// Rule code is part of nodex (e.g. `required_field`,
    /// `frontmatter_immutable`). May still be inert when its driving
    /// config is absent — in which case `registered_rules` omits it
    /// from the registry entirely.
    Builtin,
    /// One rule per config block (`body_line/<name>`). Removing the
    /// block removes the rule from the registry.
    Config,
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// A single rule violation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
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
    /// when no diff context is available; `Some(_)` when `check` has one
    /// from `--since <ref>` or a configured `rules.immutable_baseline`.
    /// Rules whose semantic requires "this is what changed" (e.g.
    /// `frontmatter_immutable`) declare themselves non-applicable via
    /// [`Rule::is_applicable`] when this is `None`.
    pub since: Option<&'a GraphDiff>,
}

/// Whether a per-block `kinds` filter admits `kind` — an empty filter
/// admits every kind. The string-keyed counterpart to
/// [`crate::model::Node::matches_kinds`], for the diff-aware rules that
/// gate on a node's *before* kind (a bare `&str` carried by the diff)
/// rather than a live [`crate::model::Node`].
pub(crate) fn kind_allowed(kinds: &[String], kind: &str) -> bool {
    kinds.is_empty() || kinds.iter().any(|k| k == kind)
}

/// One rule that the runner declined to evaluate, with a one-line reason.
/// Symmetric to [`Violation`] — silent skipping would let a strict-mode
/// rule appear to "pass" when it never actually ran.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonSchema)]
pub struct SkippedRule {
    pub rule_id: String,
    pub reason: String,
}

/// Self-describing validation rule. The single source of truth for
/// everything that [`check`] runs *and* everything that
/// `export::export_rules` surfaces in the manifest — there is no
/// parallel hand-written description / params / source / diff-aware
/// list in `export.rs` to keep in sync.
///
/// Adding a new built-in rule is a single-file change: implement this
/// trait, then add an entry to [`registered_rules`].
pub trait Rule: Send + Sync {
    fn id(&self) -> &str;
    fn severity(&self) -> Severity;
    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation>;

    /// One-line human-readable description of what this rule enforces.
    /// Surfaced in `nodex export rules` so downstream consumers don't
    /// hardcode rule semantics. Static for built-ins; instances can
    /// override when the description varies per construction (none
    /// today).
    fn description(&self) -> &str;

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
    /// Self-report whether this rule semantically requires a diff
    /// context (`check --since <ref>`) to fire. Surfaced on the
    /// rules manifest so downstream tooling (CI gates, PR-only
    /// validators) can dispatch on this without hardcoding the list
    /// of diff-aware rules. Default `false` for rules that operate on
    /// a single graph snapshot.
    fn diff_aware(&self) -> bool {
        false
    }
    /// Where the rule comes from — built-in code or per-config-block
    /// instance. Default [`RuleSource::Builtin`]; per-block rules
    /// (e.g. `BodyLineRule`) override to [`RuleSource::Config`].
    fn source(&self) -> RuleSource {
        RuleSource::Builtin
    }
    /// Rule-specific parameters surfaced on the manifest entry — the
    /// configured values that distinguish this rule instance from
    /// another in the same family (regex pattern, kinds,
    /// mode, enums, thresholds, …). Default empty; rules whose
    /// behaviour depends on declarative config (e.g. `stale_review`
    /// reads `detection.stale_days`) override to surface the live
    /// values. The schema is per-rule (described in
    /// [`Self::description`]) — kept as a free-form object so adding
    /// a new built-in rule doesn't reshape the manifest.
    fn params(&self, _config: &Config) -> Map<String, Value> {
        Map::new()
    }
}

/// Build the registered rule set for the project. Single source of
/// truth for both [`check`] (runs them) and
/// `nodex_core::export::export_rules` (emits the manifest). Adding a
/// new rule = adding it here.
///
/// Rules whose driving config block is absent are omitted from the
/// registry entirely — they are not "skipped" because there was
/// nothing to skip. The skipped-rule surface remains for rules whose
/// config IS present but whose runtime prerequisites aren't met
/// (e.g. `frontmatter_immutable` configured but `check` invoked
/// without `--since`).
pub fn registered_rules(config: &Config) -> Vec<Box<dyn Rule>> {
    let mut rules: Vec<Box<dyn Rule>> = vec![
        Box::new(schema::RequiredFieldRule),
        Box::new(schema::FieldTypeRule),
        Box::new(schema::FieldEnumRule),
        Box::new(schema::CrossFieldRule),
    ];
    if matches!(config.schema.mode, crate::config::SchemaMode::Strict) {
        rules.push(Box::new(schema::UnknownFieldRule));
    }
    rules.push(Box::new(freshness::StaleReviewRule));
    if config.detection.git_drift_threshold.is_some() {
        rules.push(Box::new(git_drift::GitDriftRule));
    }
    if !config.rules.naming.is_empty() {
        rules.push(Box::new(naming::FilenamePatternRule));
        if config.rules.naming.iter().any(|n| n.sequential) {
            rules.push(Box::new(naming::SequentialNumberingRule));
        }
        if config.rules.naming.iter().any(|n| n.unique) {
            rules.push(Box::new(naming::UniqueNumberingRule));
        }
    }
    for block in &config.rules.frontmatter_immutable {
        rules.push(Box::new(
            frontmatter_immutable::FrontmatterImmutableRule::new(block.clone()),
        ));
    }
    for block in &config.rules.body_immutable {
        rules.push(Box::new(body_immutable::BodyImmutableRule::new(
            block.clone(),
        )));
    }
    for block in &config.rules.body_line {
        rules.push(Box::new(body_line::BodyLineRule::new(block.clone())));
    }
    // DAG cycle detection over the resolved edge graph.
    rules.push(Box::new(graph_invariants::CycleDetectionRule::new(
        vec![], // empty = the default DAG relations (implements)
    )));
    rules
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

/// Result of [`check`] — both the fires (`violations`) and the
/// declined fires (`skipped_rules`). Surfacing skips alongside
/// violations is the only honest way to express "this rule was inert
/// here" without the silent-skip failure mode that
/// `.claude/rules/config-driven.md` calls out.
#[derive(Debug, Clone, serde::Serialize, Default, JsonSchema)]
pub struct CheckReport {
    pub violations: Vec<Violation>,
    pub skipped_rules: Vec<SkippedRule>,
}

/// One pass of every registered rule against the supplied context.
/// Rules that report themselves non-applicable (e.g. diff-aware rules
/// invoked without `--since`) are surfaced in [`CheckReport::skipped_rules`]
/// with their reason — silent non-fires are forbidden under
/// `.claude/rules/config-driven.md`.
pub fn check(
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

    let rules = registered_rules(config);

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
        skipped_rules: skipped,
    }
}
