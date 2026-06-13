//! Gate-plane unresolved references.
//!
//! One [`UnresolvedReferenceRule`] instance per error-severity
//! `[[detection.unresolved_policy]]` row. Each instance fires one
//! Error violation per unresolved edge the policy classifies to its
//! row, attributed to the edge's source node — so the violations
//! narrow correctly under `--since` / `--content` and `query issues`
//! counts them per row (`violation_unresolved_reference/<name>`).
//!
//! Classification performs stat-only filesystem probes under
//! `ctx.root` (the cause classifier's disk probe — the same in-root
//! access class as `git_drift`), never content reads. The probes run
//! once per registry: every error-row instance created by one
//! `registered_rules` pass shares one [`SharedClassification`] cell,
//! filled on first check against that pass's context — `rules::check`
//! builds a fresh registry per pass, so the cell can never carry a
//! stale graph's classification.

use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use crate::config::UnresolvedPolicyRuleConfig;
use crate::query::issues::{UnresolvedEdge, find_unresolved_edges};

use super::{Rule, RuleContext, RuleSource, Severity, Violation, ViolationDetails};

/// The unresolved-edge classification shared by every error-row rule
/// instance in one registry, computed once on first check.
pub type SharedClassification = Arc<OnceLock<Vec<UnresolvedEdge>>>;

/// One error-severity `[[detection.unresolved_policy]]` row as a
/// `Rule` trait object.
pub struct UnresolvedReferenceRule {
    row: UnresolvedPolicyRuleConfig,
    qualified_id: String,
    classified: SharedClassification,
}

impl UnresolvedReferenceRule {
    /// Construct a rule instance for one policy row. `qualified_id`
    /// is cached so [`Rule::id`] can return `&str` without allocating
    /// every call; `classified` is the registry-wide shared cell.
    pub fn new(row: UnresolvedPolicyRuleConfig, classified: SharedClassification) -> Self {
        let qualified_id = format!("unresolved_reference/{}", row.name);
        Self {
            row,
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
        Severity::Error
    }

    fn description(&self) -> &str {
        "Unresolved references this [[detection.unresolved_policy]] error row classifies \
         fail check"
    }

    fn source(&self) -> RuleSource {
        RuleSource::Config
    }

    fn params(&self, _config: &crate::config::Config) -> Map<String, Value> {
        // Per-row params come from the rule's own captured config —
        // mirrors the row's public surface (`name` is in the id) so
        // the manifest entry is self-describing; a `null` glob is a
        // cause-only row.
        let mut m = Map::new();
        m.insert("cause".into(), json!(self.row.cause));
        m.insert("glob".into(), json!(self.row.glob));
        m
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let edges = self
            .classified
            .get_or_init(|| find_unresolved_edges(ctx.graph, ctx.config, ctx.root));
        // Row names are unique (Config::validate), so an edge whose
        // `policy_name` equals this row's name was classified by this
        // row — and this instance exists only for error rows.
        edges
            .iter()
            .filter(|e| e.policy_name.as_deref() == Some(self.row.name.as_str()))
            .map(|e| {
                Violation::new(
                    self.qualified_id.clone(),
                    Severity::Error,
                    Some(e.source.clone()),
                    Some(e.source_path.clone()),
                    ViolationDetails::UnresolvedReference {
                        relation: e.relation.clone(),
                        raw_target: e.raw_target.clone(),
                        location: e.location.clone(),
                        cause: e.cause,
                    },
                )
            })
            .collect()
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
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn dangling(source: &str, raw: &str) -> Edge {
        Edge {
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
            graph: &graph,
            config: &config,
            root: root.path(),
            since: None,
        };

        let rule = UnresolvedReferenceRule::new(
            config.detection.unresolved_policy[1].clone(),
            SharedClassification::default(),
        );
        let violations = rule.check(&ctx);
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
    fn registered_rules_omits_rule_without_error_rows() {
        // The default policy (one info row) registers nothing — the
        // default-config registry carries no unresolved_reference rule.
        let config = Config::default();
        let ids: Vec<String> = crate::rules::registered_rules(&config)
            .iter()
            .map(|r| r.id().to_string())
            .collect();
        assert!(
            !ids.iter().any(|id| id.starts_with("unresolved_reference/")),
            "default registry must not carry an unresolved_reference rule: {ids:?}"
        );
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
        let report = crate::rules::check(&graph, &config, root.path(), None);
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
        let fresh = crate::rules::check(&graph, &config, root.path(), None);
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
        let seeded =
            crate::rules::check_with_unresolved(&graph, &config, root.path(), None, vec![]);
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
            graph: &graph,
            config: &config,
            root: root.path(),
            since: None,
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
        assert_eq!(docs_rule.check(&ctx).len(), 1);
        assert!(shared.get().is_some(), "first check fills the cell");
        assert_eq!(specs_rule.check(&ctx).len(), 1);
        assert_eq!(
            shared.get().map(Vec::len),
            Some(2),
            "both instances read the one shared classification"
        );
    }
}
