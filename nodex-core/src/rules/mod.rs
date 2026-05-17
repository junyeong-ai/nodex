use schemars::JsonSchema;
use serde_json::{Map, Value};

pub mod body_block;
pub mod body_immutable;
pub mod body_line;
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

/// Whether a check covers the whole project or is narrowed to a single
/// document. Drives [`Rule::supports_scope`]: rules whose semantic only
/// makes sense across many nodes (multi-file numbering, dup detection)
/// decline the `Document` variant; the runner records the decline in
/// [`SkippedRule`] so a `--path` invocation never silently understates
/// what was checked.
///
/// The cross-graph rules (numbering uniqueness / sequentiality) own the
/// `Document` refusal because they are the entire failure mode being
/// guarded against — a project-wide invariant cannot honestly be
/// evaluated from one document's perspective. Per-node rules
/// (schema, freshness, body_line, frontmatter / body immutability)
/// honour both scopes verbatim.
#[derive(Debug, Clone)]
pub enum CheckScope {
    /// Every node in the graph. The default for `nodex check`.
    Project,
    /// One node, identified by id. Drives `check --path <file>`: the
    /// CLI resolves the path to a node id, then narrows the runner to
    /// that node. The id is owned (rather than borrowed) so the scope
    /// can be constructed once and passed across function boundaries
    /// without lifetime annotations on every caller.
    Document { node_id: String },
}

impl CheckScope {
    /// One-word label used in skip reasons (`"project"` / `"document"`).
    /// Centralised so a future variant adds a single arm here rather
    /// than touching every formatter.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Document { .. } => "document",
        }
    }
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
    /// What part of the graph the runner is checking. Per-node rules
    /// honour both [`CheckScope::Project`] and
    /// [`CheckScope::Document`]; multi-node-comparison rules decline
    /// the latter via [`Rule::supports_scope`].
    pub scope: CheckScope,
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
    /// True when this rule can be evaluated under the given
    /// [`CheckScope`]. Default: every scope. Override (to `false` on
    /// [`CheckScope::Document`]) for rules whose meaning requires
    /// comparing many nodes — sequentiality, uniqueness across a
    /// directory — and would silently return "no violations" if asked
    /// about one document at a time. The runner converts a refusal
    /// into a [`SkippedRule`] entry so a narrowed scope never hides
    /// the gap.
    fn supports_scope(&self, _scope: &CheckScope) -> bool {
        true
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
    /// another in the same family (regex pattern, applies_to_*,
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
    for block in &config.rules.body_block {
        rules.push(Box::new(body_block::BodyBlockRule::new(block.clone())));
    }
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
        scope: CheckScope::Project,
    }
}

/// Result of [`check`] — both the fires (`violations`) and the
/// declined fires (`skipped`). Surfacing skips alongside violations is
/// the only honest way to express "this rule was inert here" without
/// the silent-skip failure mode that
/// `.claude/rules/config-driven.md` calls out.
#[derive(Debug, Clone, serde::Serialize, Default, JsonSchema)]
pub struct CheckReport {
    pub violations: Vec<Violation>,
    pub skipped: Vec<SkippedRule>,
}

/// Convenience entry point for the most common call: project-wide,
/// no diff context. Equivalent to `check(graph, config, root, None,
/// CheckScope::Project)` and named to mirror the [`CheckScope`]
/// variant it implies. Used by internal call sites (e.g. scaffold's
/// pre-write validation) that have no diff or document scope to
/// supply; external callers with richer state call [`check`] directly.
pub fn check_project(graph: &Graph, config: &Config, root: &Path) -> CheckReport {
    check(graph, config, root, None, CheckScope::Project)
}

/// One pass of every registered rule against the supplied context.
/// Single seam for both the project-wide path and the document-scoped
/// path (driven by `nodex check --path`). Rules that decline the
/// current scope or report themselves non-applicable are surfaced in
/// [`CheckReport::skipped`] with their reason — silent non-fires are
/// forbidden under `.claude/rules/config-driven.md`.
///
/// When `scope` is [`CheckScope::Document`], violations attributable
/// to a different node are filtered out as well, so the report stays
/// honest about which document's findings it contains. Project-wide
/// violations (those with no `node_id`) are preserved so a narrowed
/// invocation never silently drops a finding that cannot be
/// attributed to a specific id — same discipline the CLI applies for
/// `--since`.
pub fn check(
    graph: &Graph,
    config: &Config,
    root: &Path,
    since: Option<&GraphDiff>,
    scope: CheckScope,
) -> CheckReport {
    let ctx = RuleContext {
        graph,
        config,
        root,
        since,
        scope,
    };

    let rules = registered_rules(config);

    let mut violations: Vec<Violation> = Vec::new();
    let mut skipped: Vec<SkippedRule> = Vec::new();
    for rule in &rules {
        if !rule.supports_scope(&ctx.scope) {
            skipped.push(SkippedRule {
                rule_id: rule.id().to_string(),
                reason: format!("rule does not support {} scope", ctx.scope.label()),
            });
            continue;
        }
        if rule.is_applicable(&ctx) {
            violations.extend(rule.check(&ctx));
        } else {
            skipped.push(SkippedRule {
                rule_id: rule.id().to_string(),
                reason: rule.skip_reason(&ctx),
            });
        }
    }

    // Document-scoped narrowing: drop violations attributed to a
    // different node, keep node-less ones (project-wide findings that
    // cannot honestly be attributed to a specific id). The match is
    // exact equality on `node_id`, never a prefix or substring — the
    // graph guarantees ids are unique and unambiguous.
    if let CheckScope::Document { node_id } = &ctx.scope {
        violations.retain(|v| match &v.node_id {
            Some(id) => id == node_id,
            None => true,
        });
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

#[cfg(test)]
mod check_scope_tests {
    //! `CheckScope` + `supports_scope` are foundational — every
    //! later phase wires them through. These tests pin the contract:
    //!
    //! - the default `supports_scope` accepts every variant;
    //! - the narrow rules (sequential / unique numbering) decline
    //!   `Document` and the runner surfaces that as `skipped_rules`;
    //! - document-scoped checks drop violations from other nodes
    //!   while preserving project-wide (node-less) findings.
    //!
    //! No silent skips: every decline path produces a
    //! [`SkippedRule`] entry with a human-readable reason.
    use super::*;
    use crate::config::{Config, NamingRuleConfig};
    use crate::model::{Graph, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str, path: &str, kind: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(path),
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

    fn graph_with(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, vec![], vec![], vec![], vec![])
    }

    #[test]
    fn check_scope_label_pins_each_variant() {
        // The label is the visible surface of the enum — anything that
        // appears in a `SkippedRule` reason. Pin the strings here so
        // a careless variant rename doesn't silently change the
        // operator-visible vocabulary.
        assert_eq!(CheckScope::Project.label(), "project");
        assert_eq!(
            CheckScope::Document {
                node_id: "x".into()
            }
            .label(),
            "document"
        );
    }

    #[test]
    fn sequential_numbering_declines_document_scope() {
        // The rule operates over the cohort of numbered files in a
        // directory; a single document has no neighbours, so honest
        // behaviour is to refuse and let the runner record the skip.
        let rule = crate::rules::naming::SequentialNumberingRule;
        assert!(rule.supports_scope(&CheckScope::Project));
        assert!(!rule.supports_scope(&CheckScope::Document {
            node_id: "x".into()
        }));
    }

    #[test]
    fn unique_numbering_declines_document_scope() {
        // Uniqueness is the same shape: the duplicate sits in a
        // different document by definition.
        let rule = crate::rules::naming::UniqueNumberingRule;
        assert!(rule.supports_scope(&CheckScope::Project));
        assert!(!rule.supports_scope(&CheckScope::Document {
            node_id: "x".into()
        }));
    }

    #[test]
    fn schema_rules_support_both_scopes() {
        // Per-node rules — schema validation, filename pattern — must
        // honour both scopes. A single-document `check --path` would
        // be useless if it lost frontmatter validation.
        let r = crate::rules::schema::RequiredFieldRule;
        assert!(r.supports_scope(&CheckScope::Project));
        assert!(r.supports_scope(&CheckScope::Document {
            node_id: "x".into()
        }));
    }

    #[test]
    fn runner_records_scope_decline_in_skipped_with_reason() {
        // End-to-end: a project that has sequential numbering
        // configured, checked under `Document` scope, must surface
        // the rule as skipped with a reason naming the offending
        // scope. No violation is ever emitted from a declined rule.
        let g = graph_with(vec![node("a", "docs/0001-a.md", "generic")]);
        let mut config = Config::default();
        config.rules.naming.push(NamingRuleConfig {
            glob: "docs/**".into(),
            pattern: r"^\d{4}-.+\.md$".into(),
            sequential: true,
            unique: true,
        });

        let report = check(
            &g,
            &config,
            std::path::Path::new("."),
            None,
            CheckScope::Document {
                node_id: "a".into(),
            },
        );

        let declined: Vec<&str> = report
            .skipped
            .iter()
            .filter(|s| s.reason.contains("does not support document scope"))
            .map(|s| s.rule_id.as_str())
            .collect();
        assert!(
            declined.contains(&"sequential_numbering"),
            "sequential_numbering must surface as scope-declined: {:?}",
            report.skipped
        );
        assert!(
            declined.contains(&"unique_numbering"),
            "unique_numbering must surface as scope-declined: {:?}",
            report.skipped
        );
    }

    #[test]
    fn document_scope_filters_other_nodes_violations() {
        // Two nodes; one is missing the required `created` field
        // through schema override, the other isn't. `Document` scope
        // on the second must drop the first's violation while still
        // running every per-node rule that supports the scope.
        use crate::config::{FieldType, SchemaOverride};
        let mut a = node("a", "a.md", "spec");
        let mut b = node("b", "b.md", "spec");
        // Both lack `created`; the override demands it.
        a.created = None;
        b.created = None;

        let mut config = Config::default();
        config.kinds.allowed.push("spec".into());
        config.schema.overrides.push(SchemaOverride {
            kinds: vec!["spec".into()],
            required: vec!["created".into()],
            types: [("created".into(), FieldType::Date)].into_iter().collect(),
            enums: BTreeMap::new(),
            cross_field: vec![],
        });

        let g = graph_with(vec![a, b]);
        let report = check(
            &g,
            &config,
            std::path::Path::new("."),
            None,
            CheckScope::Document {
                node_id: "b".into(),
            },
        );

        // Exactly the node-scoped violations: none from "a".
        let attributed: Vec<&str> = report
            .violations
            .iter()
            .filter_map(|v| v.node_id.as_deref())
            .collect();
        assert!(
            attributed.iter().all(|id| *id == "b"),
            "document scope must filter out other nodes' violations, got {:?}",
            attributed
        );
        assert!(
            !attributed.is_empty(),
            "the target node must still receive its own violations: {:?}",
            report.violations
        );
    }
}
