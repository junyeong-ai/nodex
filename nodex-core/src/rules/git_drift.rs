//! "The world I documented has moved on" detector.
//!
//! For each non-terminal document with a `reviewed` date, count the
//! git commits that landed on the documents (and code paths declared
//! via `covers`) it references since that review. When the total
//! crosses `detection.git_drift_threshold`, the review is treated as
//! stale relative to the artefacts it covers — the canonical
//! doc-gardening signal.
//!
//! Disabled when `git_drift_threshold` is `None`. The runtime
//! environment (git on PATH + git work tree) is verified by
//! [`crate::rules::preflight`] before any command runs, so this rule
//! assumes the probe has already passed.

use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::NaiveDate;

use crate::model::ResolvedTarget;

use super::{Rule, RuleContext, Severity, Violation};

pub struct GitDriftRule;

impl Rule for GitDriftRule {
    fn id(&self) -> &str {
        "git_drift"
    }

    fn severity(&self) -> Severity {
        Severity::Warning
    }

    fn description(&self) -> &str {
        "Active docs are flagged when outgoing relation targets have accumulated \
         more than `detection.git_drift_threshold` git commits since `reviewed`"
    }

    fn scope(&self, config: &crate::config::Config) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert(
            "threshold".into(),
            serde_json::json!(config.detection.git_drift_threshold),
        );
        m.insert(
            "relations".into(),
            serde_json::json!(config.detection.git_drift_relations),
        );
        m
    }

    fn check(&self, ctx: &RuleContext<'_>) -> Vec<Violation> {
        let Some(threshold) = ctx.config.detection.git_drift_threshold else {
            return Vec::new();
        };
        let relations = &ctx.config.detection.git_drift_relations;
        let mut violations = Vec::new();

        for node in ctx.graph.nodes().values() {
            if ctx.config.is_terminal(node.status.as_str()) {
                continue;
            }
            let Some(reviewed) = node.reviewed else {
                continue;
            };

            let mut total_commits: u32 = 0;
            let mut hottest: Option<(String, u32)> = None;

            for edge in ctx.graph.outgoing_edges(&node.id) {
                if !relations.iter().any(|r| r == &edge.relation) {
                    continue;
                }
                let (path, label) = match &edge.target {
                    ResolvedTarget::Resolved { id } => match ctx.graph.node(id) {
                        Some(t) => (t.path.clone(), id.clone()),
                        None => continue,
                    },
                    // `covers` typically points at code paths that live
                    // outside the doc graph; resolve them against the
                    // project root and count their drift too.
                    ResolvedTarget::Unresolved { raw, .. } => {
                        let candidate = PathBuf::from(raw);
                        if !ctx.root.join(&candidate).is_file() {
                            continue;
                        }
                        (candidate, raw.clone())
                    }
                };
                let commits = commits_since(ctx.root, &path, reviewed);
                total_commits = total_commits.saturating_add(commits);
                if hottest.as_ref().is_none_or(|(_, c)| commits > *c) {
                    hottest = Some((label, commits));
                }
            }

            if total_commits > threshold {
                let suffix = hottest
                    .map(|(id, c)| format!(" (hottest: {id} with {c})"))
                    .unwrap_or_default();
                violations.push(Violation {
                    rule_id: self.id().to_string(),
                    severity: self.severity(),
                    node_id: Some(node.id.clone()),
                    path: Some(crate::path_guard::forward_string(&node.path)),
                    message: format!(
                        "{total_commits} commits to referenced docs since reviewed={reviewed} (threshold {threshold}){suffix}"
                    ),
                });
            }
        }

        violations
    }
}

/// Commit count touching `path` since `since` (inclusive). Returns 0
/// on any git failure — environmental issues are surfaced separately
/// by [`probe_environment`] so the per-path call site stays simple.
pub(crate) fn commits_since(root: &Path, path: &Path, since: NaiveDate) -> u32 {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "--pretty=format:%H", "--since"])
        .arg(since.to_string())
        .arg("--")
        .arg(path)
        .output();
    let Ok(output) = output else { return 0 };
    if !output.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count() as u32
}

/// Verify `git` is available and `root` lies inside a git work tree.
/// Returns a human-readable diagnostic when either condition fails.
/// Called by [`crate::rules::preflight`] when `git_drift_threshold`
/// is set, so the per-document loop above never has to handle a
/// missing-git case.
pub(crate) fn probe_environment(root: &Path) -> Result<(), String> {
    let probe = Command::new("git").arg("--version").output();
    match probe {
        Ok(o) if o.status.success() => {}
        _ => return Err("git binary not found on PATH".to_string()),
    }
    let inside = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    match inside {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "{} is not inside a git work tree: {}",
            root.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("git rev-parse failed: {e}")),
    }
}
