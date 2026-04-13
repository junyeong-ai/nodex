use chrono::Local;

use crate::config::Config;
use crate::model::Graph;

use super::{Rule, Severity, Violation};

/// Warn about active documents not reviewed within the threshold.
pub struct StaleReview;

impl Rule for StaleReview {
    fn id(&self) -> &str {
        "stale_review"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let today = Local::now().date_naive();
        let cutoff = today - chrono::Duration::days(config.detection.stale_days as i64);

        graph
            .nodes()
            .values()
            .filter(|node| {
                !config.is_terminal(node.status.as_str())
                    && node.reviewed.map(|r| r < cutoff).unwrap_or(false)
            })
            .map(|node| {
                let reviewed = node.reviewed.unwrap();
                let days = (today - reviewed).num_days();
                Violation {
                    rule_id: self.id().to_string(),
                    severity: self.severity(),
                    node_id: Some(node.id.clone()),
                    path: Some(node.path.to_string_lossy().to_string()),
                    message: format!(
                        "not reviewed for {days} days (threshold: {} days)",
                        config.detection.stale_days
                    ),
                }
            })
            .collect()
    }
}
