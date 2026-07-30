use chrono::NaiveDate;
use schemars::JsonSchema;

use crate::config::Config;
use crate::model::Graph;

use super::NodeRef;

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct OrphanEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub created: Option<NaiveDate>,
}

/// Find nodes with zero incoming edges (orphans), as of `today`.
pub fn find_orphans(graph: &Graph, config: &Config, today: NaiveDate) -> Vec<OrphanEntry> {
    // Grace period: documents created within `orphan_grace_days` are
    // exempt from orphan detection, so a freshly-created doc can exist
    // before it is linked. Complements `orphan_ok_kinds` and the
    // per-node `orphan_ok` flag. `orphan_grace_days` is a user-supplied
    // u32; subtract via the checked API so a pathological value can't
    // panic. On underflow the cutoff would predate every doc, exempting
    // them all — return empty directly, which is that same conservative
    // result.
    let Some(grace_cutoff) = today.checked_sub_days(chrono::Days::new(u64::from(
        config.detection.orphan_grace_days,
    ))) else {
        return Vec::new();
    };

    let mut orphans: Vec<OrphanEntry> = graph
        .nodes()
        .values()
        .filter(|node| {
            if config.is_orphan_ok_kind(node.kind.as_str()) {
                return false;
            }
            if node.orphan_ok {
                return false;
            }
            // A self-reference (a→a) is not "attention from outside",
            // so a doc whose only incoming edge is its own does not
            // escape orphan classification. Honest-graph queries
            // (`query node`, `query backlinks` from another node)
            // still see the self-edge; only this isolation metric
            // filters it out.
            if !graph.external_incoming_edges(&node.id).is_empty() {
                return false;
            }
            if let Some(created) = node.created
                && created > grace_cutoff
            {
                return false;
            }
            true
        })
        .map(|node| OrphanEntry {
            node: NodeRef::from_node(node),
            created: node.created,
        })
        .collect();

    orphans.sort_by(|a, b| a.node.id.cmp(&b.node.id));
    orphans
}

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct StaleEntry {
    #[serde(flatten)]
    pub node: NodeRef,
    pub reviewed: NaiveDate,
    /// Whole days between the `reviewed` date and today. Mirrors
    /// [`crate::query::recent::RecentEntry::days_ago`] — same
    /// invariant (always ≥ 0), same type, same clamp.
    pub days_since: u32,
}

/// Find active documents that haven't been reviewed within the threshold,
/// as of `today`. Returns empty if stale detection is disabled
/// (stale_days is None).
pub fn find_stale(graph: &Graph, config: &Config, today: NaiveDate) -> Vec<StaleEntry> {
    let Some(stale_days) = config.detection.stale_days else {
        return Vec::new();
    };

    let Some(cutoff) = today.checked_sub_days(chrono::Days::new(u64::from(stale_days))) else {
        return Vec::new();
    };

    let mut stale: Vec<StaleEntry> = graph
        .nodes()
        .values()
        .filter(|node| {
            if config.is_terminal(node.status.as_str()) {
                return false;
            }
            match node.reviewed {
                // `stale_days = n` flags docs not reviewed for n+ days
                // (elapsed >= n), i.e. reviewed on/before the cutoff.
                Some(reviewed) => reviewed <= cutoff,
                None => false,
            }
        })
        .filter_map(|node| {
            let reviewed = node.reviewed?;
            Some(StaleEntry {
                node: NodeRef::from_node(node),
                reviewed,
                days_since: super::days_between_clamped(today, reviewed),
            })
        })
        .collect();

    stale.sort_by(|a, b| {
        a.reviewed
            .cmp(&b.reviewed)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    stale
}
