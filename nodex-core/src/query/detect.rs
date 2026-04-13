use chrono::{Local, NaiveDate};

use crate::config::Config;
use crate::model::Graph;

#[derive(Debug, serde::Serialize)]
pub struct OrphanEntry {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub path: String,
    pub created: Option<NaiveDate>,
}

/// Find nodes with zero incoming edges (orphans).
pub fn find_orphans(graph: &Graph, config: &Config) -> Vec<OrphanEntry> {
    let today = Local::now().date_naive();
    let grace_cutoff = today - chrono::Duration::days(config.detection.orphan_grace_days as i64);

    let mut orphans: Vec<OrphanEntry> = graph
        .nodes()
        .values()
        .filter(|node| {
            // Skip nodes explicitly marked as ok
            if node.orphan_ok {
                return false;
            }

            // Skip nodes with incoming edges
            if !graph.incoming_indices(&node.id).is_empty() {
                return false;
            }

            // Skip nodes within grace period
            if let Some(created) = node.created
                && created > grace_cutoff
            {
                return false;
            }

            true
        })
        .map(|node| OrphanEntry {
            id: node.id.clone(),
            title: node.title.clone(),
            kind: node.kind.to_string(),
            path: node.path.to_string_lossy().to_string(),
            created: node.created,
        })
        .collect();

    orphans.sort_by(|a, b| a.id.cmp(&b.id));
    orphans
}

#[derive(Debug, serde::Serialize)]
pub struct StaleEntry {
    pub id: String,
    pub title: String,
    pub path: String,
    pub reviewed: NaiveDate,
    pub days_since: i64,
}

/// Find active documents that haven't been reviewed within the threshold.
pub fn find_stale(graph: &Graph, config: &Config) -> Vec<StaleEntry> {
    let today = Local::now().date_naive();
    let cutoff = today - chrono::Duration::days(config.detection.stale_days as i64);

    let mut stale: Vec<StaleEntry> = graph
        .nodes()
        .values()
        .filter(|node| {
            // Only active nodes
            if config.is_terminal(node.status.as_str()) {
                return false;
            }

            // Must have a reviewed date that's older than cutoff
            match node.reviewed {
                Some(reviewed) => reviewed < cutoff,
                None => false, // No reviewed date = not trackable, not stale
            }
        })
        .filter_map(|node| {
            let reviewed = node.reviewed?; // safe: filter above ensures Some
            Some(StaleEntry {
                id: node.id.clone(),
                title: node.title.clone(),
                path: node.path.to_string_lossy().to_string(),
                reviewed,
                days_since: (today - reviewed).num_days(),
            })
        })
        .collect();

    stale.sort_by(|a, b| a.reviewed.cmp(&b.reviewed).then_with(|| a.id.cmp(&b.id)));
    stale
}
