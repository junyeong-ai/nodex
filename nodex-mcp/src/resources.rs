//! MCP resources — read-only knowledge surfaces an LLM client can
//! attach as ambient context. Curated to a small set so `resources/list`
//! responses stay short and meaningful even on huge graphs.

use serde_json::{Value, json};
use std::path::Path;

use nodex_core::query::{
    issues,
    recent::{self, RecencyOptions, RecencySince},
};

use crate::tools::ToolError;

const URI_SUMMARY: &str = "nodex://graph/summary";
const URI_ISSUES: &str = "nodex://graph/issues";
const URI_RECENT: &str = "nodex://graph/recent";

/// Static resource catalogue returned by `resources/list`.
pub fn list_descriptors() -> Value {
    json!([
        {
            "uri": URI_SUMMARY,
            "name": "Graph summary",
            "description": "Overall health of the document graph: counts by kind/status, open issue total, recent change count.",
            "mimeType": "application/json"
        },
        {
            "uri": URI_ISSUES,
            "name": "Open issues",
            "description": "Every actionable problem in the graph (orphans, stale, unresolved edges, rule violations).",
            "mimeType": "application/json"
        },
        {
            "uri": URI_RECENT,
            "name": "Recent changes (7d)",
            "description": "Documents whose created/updated/reviewed date falls inside the last 7 days, newest first.",
            "mimeType": "application/json"
        }
    ])
}

/// Returned content of one resource read.
pub struct ResourceContent {
    pub uri: String,
    pub mime_type: &'static str,
    pub text: String,
}

/// Read the body of a resource by URI.
pub fn read(root: &Path, uri: &str) -> Result<ResourceContent, ToolError> {
    let config = nodex_core::load_project(root)?;
    let result = nodex_core::builder::build(root, &config, false)?;

    let payload = match uri {
        URI_SUMMARY => build_summary(&result.graph, &config, root),
        URI_ISSUES => serde_json::to_value(issues::collect_issues(&result.graph, &config, root))
            .map_err(|e| ToolError::Internal(e.to_string()))?,
        URI_RECENT => {
            let entries = recent::find_recent(
                &result.graph,
                &RecencyOptions {
                    since: RecencySince::Days(7),
                    kind: None,
                    field: recent::RecencyField::Any,
                    limit: Some(50),
                },
            );
            json!({ "items": entries, "total": entries.len() })
        }
        other => {
            return Err(ToolError::Failure {
                code: "NOT_FOUND",
                message: format!("unknown resource uri: {other}"),
            });
        }
    };

    let text = serde_json::to_string_pretty(&payload).expect("payload is JSON-serialisable");
    Ok(ResourceContent {
        uri: uri.to_string(),
        mime_type: "application/json",
        text,
    })
}

fn build_summary(graph: &nodex_core::Graph, config: &nodex_core::Config, root: &Path) -> Value {
    use std::collections::BTreeMap;

    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    for node in graph.nodes().values() {
        *by_kind.entry(node.kind.to_string()).or_insert(0) += 1;
        *by_status.entry(node.status.to_string()).or_insert(0) += 1;
    }

    let issue_report = issues::collect_issues(graph, config, root);
    let recent = recent::find_recent(
        graph,
        &RecencyOptions {
            since: RecencySince::Days(7),
            limit: None,
            ..Default::default()
        },
    );

    json!({
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "by_kind": by_kind,
        "by_status": by_status,
        "open_issue_count": issue_report.summary.total,
        "recent_change_count_7d": recent.len(),
        "schema_version": nodex_core::model::graph::SCHEMA_VERSION
    })
}
