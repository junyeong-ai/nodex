pub mod freshness;
pub mod integrity;
pub mod naming;
pub mod schema;

use crate::config::Config;
use crate::model::Graph;

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

/// Trait for validation rules.
pub trait Rule: Send + Sync {
    fn id(&self) -> &str;
    fn severity(&self) -> Severity;
    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation>;
}

/// Run all built-in rules and return violations.
pub fn check_all(graph: &Graph, config: &Config) -> Vec<Violation> {
    let rules: Vec<Box<dyn Rule>> = vec![
        // Schema family — required-field presence + declarative type,
        // enum, and cross-field constraints driven by nodex.toml.
        Box::new(schema::RequiredField),
        Box::new(schema::FieldTypeRule),
        Box::new(schema::FieldEnumRule),
        Box::new(schema::CrossFieldRule),
        // Integrity family — advisories that cannot be expressed
        // declaratively (e.g. "superseded_by required when status=
        // superseded" is config-driven via cross_field now).
        Box::new(integrity::TerminalImmutability),
        // Freshness family.
        Box::new(freshness::StaleReview),
        // Naming family.
        Box::new(naming::FilenamePattern),
        Box::new(naming::SequentialNumbering),
        Box::new(naming::UniqueNumbering),
    ];

    let mut violations: Vec<Violation> = rules
        .iter()
        .flat_map(|rule| rule.check(graph, config))
        .collect();

    violations.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    violations
}
