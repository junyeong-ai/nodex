use chrono::Local;
use serde_json::{Map, Value, json};

use super::{Rule, RuleContext, Severity, Violation};

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

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let Some(stale_days) = ctx.config.detection.stale_days else {
            return Vec::new();
        };

        let today = Local::now().date_naive();
        // `stale_days` is a user-supplied u32; subtract via the checked
        // API so a pathological `u32::MAX` doesn't panic the whole CLI.
        // If the cutoff underflows chrono's representable range, treat
        // every doc as within threshold (nothing is stale).
        let Some(cutoff) = today.checked_sub_days(chrono::Days::new(u64::from(stale_days))) else {
            return Vec::new();
        };

        ctx.graph
            .nodes()
            .values()
            .filter_map(|node| {
                if ctx.config.is_terminal(node.status.as_str()) {
                    return None;
                }
                let reviewed = node.reviewed?;
                // `stale_days = n` means "flag docs not reviewed for n+
                // days" (elapsed >= n), i.e. reviewed on/before the
                // cutoff. A doc reviewed *after* the cutoff is fresh.
                if reviewed > cutoff {
                    return None;
                }
                let days = (today - reviewed).num_days();
                Some(Violation {
                    rule_id: self.id().to_string(),
                    severity: self.severity(),
                    node_id: Some(node.id.clone()),
                    path: Some(crate::path_guard::forward_string(&node.path)),
                    message: format!("not reviewed for {days} days (threshold: {stale_days} days)"),
                })
            })
            .collect()
    }
}
