use crate::config::Config;
use crate::model::Graph;

use super::{Rule, Severity, Violation};

/// Check that nodes have all required frontmatter fields.
pub struct RequiredFields;

impl Rule for RequiredFields {
    fn id(&self) -> &str {
        "required_fields"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let mut violations = Vec::new();

        for node in graph.nodes().values() {
            let required = config.required_fields_for(node.kind.as_str());

            for field in required {
                let missing = match field.as_str() {
                    "id" => node.id.is_empty(),
                    "title" => node.title.is_empty(),
                    "kind" => node.kind.as_str().is_empty(),
                    "status" => node.status.as_str().is_empty(),
                    "created" => node.created.is_none(),
                    "updated" => node.updated.is_none(),
                    "reviewed" => node.reviewed.is_none(),
                    "owner" => node.owner.is_none(),
                    _ => false,
                };

                if missing {
                    violations.push(Violation {
                        rule_id: self.id().to_string(),
                        severity: self.severity(),
                        node_id: Some(node.id.clone()),
                        path: Some(node.path.to_string_lossy().to_string()),
                        message: format!("missing required field: {field}"),
                    });
                }
            }
        }

        violations
    }
}
