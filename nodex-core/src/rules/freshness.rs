use chrono::Local;

use crate::config::Config;
use crate::model::Graph;

use super::{Rule, Severity, Violation};

/// Warn about active documents not reviewed within the threshold.
pub struct StaleReviewRule;

impl Rule for StaleReviewRule {
    fn id(&self) -> &str {
        "stale_review"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, graph: &Graph, config: &Config) -> Vec<Violation> {
        let today = Local::now().date_naive();
        // `stale_days` is a user-supplied u32; subtract via the checked
        // API so a pathological `u32::MAX` doesn't panic the whole CLI.
        // If the cutoff underflows chrono's representable range, treat
        // every doc as within threshold (nothing is stale).
        let Some(cutoff) =
            today.checked_sub_days(chrono::Days::new(u64::from(config.detection.stale_days)))
        else {
            return Vec::new();
        };

        graph
            .nodes()
            .values()
            .filter_map(|node| {
                if config.is_terminal(node.status.as_str()) {
                    return None;
                }
                let reviewed = node.reviewed?;
                if reviewed >= cutoff {
                    return None;
                }
                let days = (today - reviewed).num_days();
                Some(Violation {
                    rule_id: self.id().to_string(),
                    severity: self.severity(),
                    node_id: Some(node.id.clone()),
                    path: Some(node.path.to_string_lossy().to_string()),
                    message: format!(
                        "not reviewed for {days} days (threshold: {} days)",
                        config.detection.stale_days
                    ),
                })
            })
            .collect()
    }
}
