//! Policy-classified unresolved references, shared by checks and issue listings.
//! Non-info rows and the unmatched fallback each register one rule. Classification
//! uses stat-only probes and is shared once within a registry evaluation.

use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use crate::config::UnresolvedPolicyRuleConfig;
use crate::query::issues::{UnresolvedEdge, find_unresolved_edges};

use super::{
    Rule, RuleContext, RuleRun, RuleSource, Severity, SubjectUnit, Violation, ViolationDetails,
    detail::Evidence,
};

/// The unresolved-edge classification shared by every unresolved-reference rule
/// instance in one registry, computed once on first check.
pub type SharedClassification = Arc<OnceLock<Vec<UnresolvedEdge>>>;

/// One non-info policy row or the unmatched fallback as a rule.
pub struct UnresolvedReferenceRule {
    row: Option<UnresolvedPolicyRuleConfig>,
    qualified_id: String,
    classified: SharedClassification,
}

impl UnresolvedReferenceRule {
    pub fn fallback(classified: SharedClassification) -> Self {
        Self {
            row: None,
            qualified_id: "unresolved_reference".into(),
            classified,
        }
    }

    /// Construct a rule instance for one policy row. `qualified_id`
    /// is cached so [`Rule::id`] can return `&str` without allocating
    /// every call; `classified` is the registry-wide shared cell.
    pub fn new(row: UnresolvedPolicyRuleConfig, classified: SharedClassification) -> Self {
        let qualified_id = format!("unresolved_reference/{}", row.name);
        Self {
            row: Some(row),
            qualified_id,
            classified,
        }
    }
}

impl Rule for UnresolvedReferenceRule {
    fn id(&self) -> &str {
        &self.qualified_id
    }

    fn severity(&self) -> Severity {
        match self.row.as_ref().map(|row| row.severity) {
            Some(crate::config::UnresolvedSeverity::Error) => Severity::Error,
            _ => Severity::Warning,
        }
    }

    fn description(&self) -> &str {
        "Unresolved references classified by this policy rule"
    }

    fn source(&self) -> RuleSource {
        if self.row.is_some() {
            RuleSource::Config
        } else {
            RuleSource::Builtin
        }
    }

    fn params(&self, _config: &crate::config::Config) -> Map<String, Value> {
        // Per-row params come from the rule's own captured config —
        // mirrors the row's public surface (`name` is in the id) so the
        // manifest entry is self-describing; an empty `relations` is a
        // row over every relation and a `null` glob one over every
        // target.
        let mut m = Map::new();
        if let Some(row) = &self.row {
            m.insert("cause".into(), json!(row.cause));
            m.insert("relations".into(), json!(row.relations));
            m.insert("glob".into(), json!(row.glob));
        }
        m
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Edges
    }

    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let edges = self
            .classified
            .get_or_init(|| find_unresolved_edges(ctx.graph, ctx.config, ctx.files));
        // Row names are unique (Config::validate), so an edge whose
        // `policy_name` equals this row's name was classified by this
        // row. The fallback matches edges without a policy name.
        let violations: Vec<Violation> = edges
            .iter()
            .filter(|e| e.policy_name.as_deref() == self.row.as_ref().map(|row| row.name.as_str()))
            .map(|e| {
                Violation::new(
                    self.qualified_id.clone(),
                    self.configured_severity(ctx.config),
                    Some(e.source.clone()),
                    Some(e.source_path.clone()),
                    ViolationDetails::UnresolvedReference {
                        relation: e.relation.clone(),
                        raw_target: e.raw_target.clone(),
                        location: Evidence(e.location.clone()),
                        cause: e.cause,
                    },
                )
            })
            .collect();
        // Every unresolved edge is offered to every row; which one claims it
        // is the ordered first-match. So the reach is the whole offer, and a
        // row that claimed none of a large one is a row the project has
        // outgrown — a distinction its own violation count cannot draw.
        RuleRun::new(edges.len(), violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, UnresolvedSeverity};
    use crate::model::{
        Edge, Graph, GraphMeta, Kind, Node, ResolvedTarget, Status, UnresolvedCause,
    };
    use indexmap::IndexMap;
    use std::path::PathBuf;

    fn node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.to_string(),
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
            orphan_ok: true,
            attrs: Default::default(),
            frontmatter_relations: Default::default(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            body_structure: Default::default(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn dangling(source: &str, raw: &str) -> Edge {
        Edge {
            reference: None,
            source: source.to_string(),
            target: ResolvedTarget::unresolved(raw, UnresolvedCause::Missing),
            relation: "references".to_string(),
            location: "L1".to_string(),
        }
    }

    fn graph_of(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, edges, vec![], vec![], vec![], GraphMeta::default())
    }

    fn row(
        name: &str,
        cause: UnresolvedCause,
        glob: Option<&str>,
        severity: UnresolvedSeverity,
    ) -> UnresolvedPolicyRuleConfig {
        UnresolvedPolicyRuleConfig {
            name: name.to_string(),
            cause,
            relations: Vec::new(),
            glob: glob.map(str::to_string),
            severity,
        }
    }

    #[test]
    fn rule_id_is_qualified_with_row_name() {
        let rule = UnresolvedReferenceRule::new(
            row(
                "broken-docs-link",
                UnresolvedCause::Missing,
                Some("docs/**"),
                UnresolvedSeverity::Error,
            ),
            SharedClassification::default(),
        );
        assert_eq!(rule.id(), "unresolved_reference/broken-docs-link");
    }

    #[test]
    fn error_row_fires_only_for_edges_it_classifies() {
        // A preceding info row shields its matches from a later,
        // broader error row — the rule fires on exactly the edges the
        // policy classified to *its* row, not on every unresolved edge
        // its own (cause, glob) would match in isolation.
        let root = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.detection.unresolved_policy = vec![
            row(
                "ephemeral-specs",
                UnresolvedCause::Missing,
                Some("specs/**"),
                UnresolvedSeverity::Info,
            ),
            row(
                "any-missing",
                UnresolvedCause::Missing,
                Some("**"),
                UnresolvedSeverity::Error,
            ),
        ];
        let graph = graph_of(
            vec![node("a")],
            vec![dangling("a", "specs/x.md"), dangling("a", "docs/x.md")],
        );
        let ctx = RuleContext {
            today: crate::test_today(),
            graph: &graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(root.path()),
            history: &crate::rules::UNMEASURED,
            since: None,
            steps: None,
        };

        let rule = UnresolvedReferenceRule::new(
            config.detection.unresolved_policy[1].clone(),
            SharedClassification::default(),
        );
        let violations = rule.check(&ctx).violations;
        assert_eq!(
            violations.len(),
            1,
            "the specs edge is shielded: {violations:?}"
        );
        assert_eq!(violations[0].rule_id, "unresolved_reference/any-missing");
        assert_eq!(violations[0].severity, Severity::Error);
        assert_eq!(violations[0].node_id.as_deref(), Some("a"));
        assert_eq!(violations[0].path.as_deref(), Some("a.md"));
        assert!(violations[0].message.contains("docs/x.md"));
    }

    #[test]
    fn default_policy_registers_only_the_unmatched_unresolved_rule() {
        let config = Config::default();
        let ids: Vec<String> = crate::rules::registered_rules(&config)
            .iter()
            .map(|r| r.id().to_string())
            .collect();
        assert!(ids.iter().any(|id| id == "unresolved_reference"));
        assert!(!ids.iter().any(|id| id.starts_with("unresolved_reference/")));
    }

    #[test]
    fn error_rows_register_one_rule_each_sharing_one_classification() {
        let mut config = Config::default();
        config.detection.unresolved_policy = vec![
            row(
                "broken-docs-link",
                UnresolvedCause::Missing,
                Some("docs/**"),
                UnresolvedSeverity::Error,
            ),
            row(
                "shadowed-target",
                UnresolvedCause::ExcludedFromScope,
                None,
                UnresolvedSeverity::Error,
            ),
            row(
                "ephemeral-specs",
                UnresolvedCause::Missing,
                Some("specs/**"),
                UnresolvedSeverity::Info,
            ),
        ];
        let ids: Vec<String> = crate::rules::registered_rules(&config)
            .iter()
            .map(|r| r.id().to_string())
            .collect();
        assert!(ids.contains(&"unresolved_reference/broken-docs-link".to_string()));
        assert!(ids.contains(&"unresolved_reference/shadowed-target".to_string()));
        assert!(
            !ids.contains(&"unresolved_reference/ephemeral-specs".to_string()),
            "info rows register no check rule"
        );
    }

    #[test]
    fn rules_check_fires_error_rows_through_the_registry() {
        // End-to-end through `rules::check`: the registry path the CLI
        // takes, so `nodex check` exits 1 on an error-classified edge.
        let root = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.detection.unresolved_policy = vec![row(
            "broken-docs-link",
            UnresolvedCause::Missing,
            Some("docs/**"),
            UnresolvedSeverity::Error,
        )];
        let graph = graph_of(vec![node("a")], vec![dangling("a", "docs/x.md")]);
        let report = crate::rules::check(
            &graph,
            &config,
            crate::builder::scanner::ProjectFiles::working_tree(root.path()),
            &crate::rules::git_drift::DriftHistory::of(&config, root.path()),
            crate::rules::Since::None,
            None,
            crate::test_today(),
        );
        let fired: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.rule_id == "unresolved_reference/broken-docs-link")
            .collect();
        assert_eq!(fired.len(), 1, "{:?}", report.violations);
    }

    #[test]
    fn check_with_unresolved_reads_the_seeded_classification() {
        // The seeded cell IS the classification: the rule pass derives
        // its violations from the supplied vector and never re-runs the
        // stat probes — pinned by seeding a classification that
        // disagrees with what a fresh probe over this graph computes.
        let root = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.detection.unresolved_policy = vec![row(
            "broken-docs-link",
            UnresolvedCause::Missing,
            Some("docs/**"),
            UnresolvedSeverity::Error,
        )];
        let graph = graph_of(vec![node("a")], vec![dangling("a", "docs/x.md")]);

        // A fresh classification fires the error row…
        let fresh = crate::rules::check(
            &graph,
            &config,
            crate::builder::scanner::ProjectFiles::working_tree(root.path()),
            &crate::rules::git_drift::DriftHistory::of(&config, root.path()),
            crate::rules::Since::None,
            None,
            crate::test_today(),
        );
        assert_eq!(
            fresh
                .violations
                .iter()
                .filter(|v| v.rule_id == "unresolved_reference/broken-docs-link")
                .count(),
            1
        );

        // …while a seeded empty classification is consumed as-is: zero
        // violations, because the rule read the seed, not the probes.
        let seeded = crate::rules::check_with_unresolved(
            &graph,
            &config,
            crate::builder::scanner::ProjectFiles::working_tree(root.path()),
            &crate::rules::git_drift::DriftHistory::of(&config, root.path()),
            crate::rules::Since::None,
            None,
            vec![],
            crate::test_today(),
        );
        assert!(
            !seeded
                .violations
                .iter()
                .any(|v| v.rule_id.starts_with("unresolved_reference/")),
            "{:?}",
            seeded.violations
        );
    }

    #[test]
    fn shared_cell_is_filled_once_across_instances() {
        // Two error rows share one classification cell — the stat
        // probes run once, and both instances read the same Vec.
        let root = tempfile::tempdir().expect("tempdir");
        let mut config = Config::default();
        config.detection.unresolved_policy = vec![
            row(
                "broken-docs-link",
                UnresolvedCause::Missing,
                Some("docs/**"),
                UnresolvedSeverity::Error,
            ),
            row(
                "broken-specs-link",
                UnresolvedCause::Missing,
                Some("specs/**"),
                UnresolvedSeverity::Error,
            ),
        ];
        let graph = graph_of(
            vec![node("a")],
            vec![dangling("a", "docs/x.md"), dangling("a", "specs/x.md")],
        );
        let ctx = RuleContext {
            today: crate::test_today(),
            graph: &graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(root.path()),
            history: &crate::rules::UNMEASURED,
            since: None,
            steps: None,
        };
        let shared = SharedClassification::default();
        let docs_rule = UnresolvedReferenceRule::new(
            config.detection.unresolved_policy[0].clone(),
            shared.clone(),
        );
        let specs_rule = UnresolvedReferenceRule::new(
            config.detection.unresolved_policy[1].clone(),
            shared.clone(),
        );
        assert_eq!(docs_rule.check(&ctx).violations.len(), 1);
        assert!(shared.get().is_some(), "first check fills the cell");
        assert_eq!(specs_rule.check(&ctx).violations.len(), 1);
        assert_eq!(
            shared.get().map(Vec::len),
            Some(2),
            "both instances read the one shared classification"
        );
    }
}
