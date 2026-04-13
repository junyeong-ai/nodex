use crate::config::Config;
use crate::model::Graph;

use super::{Rule, Severity, Violation};

/// Nodes with status=superseded must have superseded_by set.
pub struct SupersededByRequired;

impl Rule for SupersededByRequired {
    fn id(&self) -> &str {
        "superseded_by_required"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, graph: &Graph, _config: &Config) -> Vec<Violation> {
        graph
            .nodes()
            .values()
            .filter(|node| node.status.as_str() == "superseded" && node.superseded_by.is_none())
            .map(|node| Violation {
                rule_id: self.id().to_string(),
                severity: self.severity(),
                node_id: Some(node.id.clone()),
                path: Some(node.path.to_string_lossy().to_string()),
                message: "status is 'superseded' but superseded_by is not set".to_string(),
            })
            .collect()
    }
}

/// Terminal-status documents should not be modified (advisory check).
pub struct TerminalImmutability;

impl Rule for TerminalImmutability {
    fn id(&self) -> &str {
        "terminal_immutability"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        // This is an advisory rule — actual enforcement happens in pre-commit hooks.
        // Here we flag terminal nodes that have updated > created as a hint.
        graph
            .nodes()
            .values()
            .filter(|node| {
                config.is_terminal(node.status.as_str())
                    && node.updated.is_some()
                    && node.created.is_some()
                    && node.updated > node.created
            })
            .map(|node| Violation {
                rule_id: self.id().to_string(),
                severity: self.severity(),
                node_id: Some(node.id.clone()),
                path: Some(node.path.to_string_lossy().to_string()),
                message: format!(
                    "terminal document (status={}) has been modified after creation",
                    node.status
                ),
            })
            .collect()
    }
}
