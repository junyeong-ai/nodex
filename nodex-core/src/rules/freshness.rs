use serde_json::{Map, Value, json};

use super::{
    Rule, RuleContext, RuleRun, Severity, SubjectUnit, Violation, ViolationDetails,
    detail::Evidence,
};

/// Warn about active documents not reviewed within the threshold.
pub struct StaleReviewRule;

impl Rule for StaleReviewRule {
    fn id(&self) -> &str {
        "stale_review"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn description(&self) -> &str {
        "Active docs are flagged when `reviewed` is older than `detection.stale_days`"
    }

    fn params(&self, config: &crate::config::Config) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("stale_days".into(), json!(config.detection.stale_days));
        m
    }

    fn is_applicable(&self, ctx: &RuleContext<'_>) -> bool {
        ctx.config.detection.stale_days.is_some()
    }

    fn skip_reason(&self, _ctx: &RuleContext<'_>) -> String {
        "stale review detection disabled (detection.stale_days is None)".into()
    }

    fn subject_unit(&self) -> SubjectUnit {
        SubjectUnit::Nodes
    }

    /// The predicate lives in [`crate::query::detect::find_stale`], which
    /// `query stale` and the `GRAPH.md` report read too — one definition
    /// of "past the horizon", so the gate and the listings cannot describe
    /// different corpora. The rule supplies the severity, the message and
    /// the threshold the finding carries; the reach is the reviewable
    /// population that same pass counted.
    fn check(&self, ctx: &RuleContext<'_>) -> RuleRun {
        let Some(stale_days) = ctx.config.detection.stale_days else {
            return RuleRun::clean(0);
        };
        let outcome = crate::query::detect::find_stale(ctx.graph, ctx.config, ctx.today);
        let violations = outcome
            .entries
            .into_iter()
            .map(|entry| {
                Violation::new(
                    self.id(),
                    self.severity(),
                    Some(entry.node.id),
                    Some(entry.node.path),
                    ViolationDetails::StaleReview {
                        days: Evidence(i64::from(entry.days_since)),
                        threshold_days: stale_days,
                    },
                )
            })
            .collect();
        RuleRun::new(outcome.subjects, violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::{Graph, Kind, Node, Status};
    use chrono::Duration;
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// The gate and the listings answer for one predicate, and this is
    /// the assertion that keeps them one: a rule that restated the
    /// predicate could drift from `query stale` and the `GRAPH.md`
    /// report an edit at a time, with the reach counted by one reading
    /// and the findings produced by another.
    #[test]
    fn the_rule_reports_exactly_what_the_listing_finds() {
        let today = crate::test_today();
        let mut config = Config::default();
        config.detection.stale_days = Some(180);

        let doc = |id: &str, status: &str, reviewed: Option<chrono::NaiveDate>| Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
            kind: Kind::new("generic"),
            status: Status::new(status),
            created: None,
            updated: None,
            reviewed,
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
        };
        let mut nodes = IndexMap::new();
        for n in [
            doc("past", "active", Some(today - Duration::days(300))),
            doc("fresh", "active", Some(today - Duration::days(1))),
            doc("undated", "active", None),
            doc("retired", "archived", Some(today - Duration::days(300))),
        ] {
            nodes.insert(n.id.clone(), n);
        }
        let graph = Graph::new(
            nodes,
            vec![],
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        );

        let listing = crate::query::detect::find_stale(&graph, &config, today);
        let run = StaleReviewRule.check(&RuleContext {
            today,
            graph: &graph,
            config: &config,
            files: crate::builder::scanner::ProjectFiles::working_tree(Path::new(".")),
            repository: None,
            since: None,
        });

        assert_eq!(
            run.subjects, listing.subjects,
            "the reach is the population that one pass counted"
        );
        let flagged: Vec<Option<&str>> = run
            .violations
            .iter()
            .map(|v| v.node_id.as_deref())
            .collect();
        let listed: Vec<Option<&str>> = listing
            .entries
            .iter()
            .map(|e| Some(e.node.id.as_str()))
            .collect();
        assert_eq!(flagged, listed, "the findings are the same documents");
        assert_eq!(listed, vec![Some("past")]);
    }
}
