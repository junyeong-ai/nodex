//! Append-only session event log for AI-agent long-term memory.
//!
//! Each session is a single document that grows by one line per event.
//! When the configured event count is reached the session is archived
//! and a successor is created — the lineage walks via the existing
//! supersession chain so no new query primitive is needed.
//!
//! Time conventions:
//! - Session **ids** are stamped from UTC with microsecond precision so
//!   two `log_event` calls in the same wall-clock second cannot collide.
//! - Frontmatter `created` / `updated` and the human-facing
//!   `session_age_days` use the local date — matching every other
//!   date-stamping path in the codebase (lifecycle, scaffold, freshness,
//!   recent, trust). Mixing the two used to produce negative ages on
//!   non-UTC hosts.

use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::builder;
use crate::config::Config;
use crate::error::{Error, ParseError, Result};
use crate::lifecycle::ARCHIVED;
use crate::model::Node;
use crate::parser::editor::{FrontmatterEditor, Scalar};
use crate::parser::frontmatter::split_frontmatter;
use crate::path_guard;
use crate::query::{
    NodeRef,
    pack::{self, Pack},
    recent::{self, RecencyField, RecencyOptions, RecencySince},
};
use crate::yaml_text;

const EVENT_COUNT_KEY: &str = "event_count";
const EVENTS_HEADING: &str = "## Events";

/// What the caller asks `log_event` to record.
#[derive(Debug, Clone, Default)]
pub struct LogEventSpec {
    /// `Some` → append to that session. `None` → create a fresh
    /// session whose id is derived from the current UTC timestamp.
    pub session_id: Option<String>,
    pub summary: String,
    /// Doc ids the event touched. Unioned with the session's existing
    /// `related` (dedup, original-order preserved).
    pub related: Vec<String>,
    /// Tags merged into the session's existing `tags`.
    pub tags: Vec<String>,
}

/// What `log_event` did with the request.
///
/// The discriminator is explicit so callers don't have to infer the
/// outcome from a combination of booleans / option fields. The
/// `RolledOver` variant carries the *previous* session id — which the
/// caller asked to append to — because the new `session_id` is already
/// surfaced at the top level of [`LogEventResult`].
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogEventOutcome {
    /// Event appended to an existing session.
    Appended,
    /// A new session document was created by this call.
    Created,
    /// The targeted session reached its event-count limit; it was
    /// archived and the event was recorded in the named successor.
    RolledOver {
        /// The id the caller passed to `log_event` — now archived.
        from_session_id: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEventResult {
    pub session_id: String,
    /// Forward-slash path relative to the project root, matching
    /// [`crate::query::NodeRef::path`] so every nodex JSON surface uses
    /// the same string shape for paths.
    pub session_path: String,
    pub event_index: usize,
    pub outcome: LogEventOutcome,
}

/// Append (or create) a session event. Errors when `session.log_kind`
/// is not configured — session-log is opt-in.
pub fn log_event(root: &Path, config: &Config, spec: LogEventSpec) -> Result<LogEventResult> {
    let log_kind = require_log_kind(config)?;
    let now_utc = Utc::now();
    let session_id = spec
        .session_id
        .clone()
        .unwrap_or_else(|| generate_session_id(now_utc));
    let rel_path = session_relative_path(config, &session_id);
    let abs_path = root.join(&rel_path);
    let today = Local::now().date_naive();

    if abs_path.exists() {
        if path_guard::is_symlink(&abs_path) {
            return Err(Error::OutsideRoot(rel_path));
        }
        append_to_existing(
            root,
            config,
            log_kind,
            &session_id,
            &rel_path,
            &abs_path,
            today,
            spec,
        )
    } else {
        write_new_session(log_kind, &session_id, &abs_path, today, &spec, &[])?;
        Ok(LogEventResult {
            session_id,
            session_path: path_guard::forward_string(&rel_path),
            event_index: 1,
            outcome: LogEventOutcome::Created,
        })
    }
}

fn require_log_kind(config: &Config) -> Result<&str> {
    config.session.log_kind.as_deref().ok_or_else(|| {
        Error::Config(
            "session.log_kind is not configured; set it in nodex.toml [session] log_kind".into(),
        )
    })
}

/// `session-YYYY-MM-DD-HHMMSS-NNNNNN` (UTC, 6-digit microseconds).
/// Microsecond precision keeps two `log_event` calls within the same
/// wall-clock second from collapsing onto one session document by
/// accident.
fn generate_session_id(now: DateTime<Utc>) -> String {
    format!("session-{}", now.format("%Y-%m-%d-%H%M%S-%6f"))
}

fn session_relative_path(config: &Config, session_id: &str) -> PathBuf {
    Path::new(&config.session.session_dir).join(format!("{session_id}.md"))
}

#[allow(clippy::too_many_arguments)]
fn append_to_existing(
    root: &Path,
    config: &Config,
    log_kind: &str,
    session_id: &str,
    rel_path: &Path,
    abs_path: &Path,
    today: chrono::NaiveDate,
    spec: LogEventSpec,
) -> Result<LogEventResult> {
    let content = std::fs::read_to_string(abs_path).map_err(|source| Error::Io {
        path: abs_path.to_path_buf(),
        source,
    })?;
    let (yaml_opt, body) = split_frontmatter(&content);
    let yaml_str = yaml_opt.ok_or_else(|| Error::Parse {
        path: abs_path.to_path_buf(),
        source: ParseError::FrontmatterDelimiter,
    })?;
    let mut editor = FrontmatterEditor::parse(yaml_str, abs_path)?;
    let current_count = read_event_count(&editor);

    if current_count >= config.session.max_events_per_session {
        return rollover(
            root, config, log_kind, session_id, abs_path, &content, today, spec,
        );
    }

    let new_count = current_count + 1;
    let merged_related = merge_list(yaml_str, "related", &spec.related);
    let merged_tags = merge_list(yaml_str, "tags", &spec.tags);

    editor.set("updated", &today.to_string());
    editor.set(EVENT_COUNT_KEY, &new_count.to_string());
    set_list_strings(&mut editor, "related", &merged_related);
    set_list_strings(&mut editor, "tags", &merged_tags);

    let new_body = append_event_line(body, today, &spec.summary);
    let new_content = format!("---\n{}---\n{}", editor.render(), new_body);
    path_guard::write_atomic(abs_path, &new_content)?;

    Ok(LogEventResult {
        session_id: session_id.to_string(),
        session_path: path_guard::forward_string(rel_path),
        event_index: new_count,
        outcome: LogEventOutcome::Appended,
    })
}

#[allow(clippy::too_many_arguments)]
fn rollover(
    root: &Path,
    config: &Config,
    log_kind: &str,
    old_session_id: &str,
    old_abs_path: &Path,
    old_full_content: &str,
    today: chrono::NaiveDate,
    mut spec: LogEventSpec,
) -> Result<LogEventResult> {
    let (new_session_id, new_rel_path, new_abs_path) =
        unique_successor_path(root, config, old_session_id)?;

    archive_with_successor(old_abs_path, old_full_content, &new_session_id, today)?;

    spec.related.insert(0, old_session_id.to_string());
    write_new_session(
        log_kind,
        &new_session_id,
        &new_abs_path,
        today,
        &spec,
        &[old_session_id],
    )?;

    Ok(LogEventResult {
        session_id: new_session_id,
        session_path: path_guard::forward_string(&new_rel_path),
        event_index: 1,
        outcome: LogEventOutcome::RolledOver {
            from_session_id: old_session_id.to_string(),
        },
    })
}

/// Bound on how many sequence bumps `unique_successor_path` will try
/// before giving up. The session-naming scheme is collision-free in
/// normal use; this is a safety stop for pathological setups (manual
/// session files at every increment).
const MAX_ROLLOVER_RESOLVE_ATTEMPTS: usize = 100;

/// Find a successor session id whose target path is free. Sequence
/// bumps via [`next_rollover_id`] until an unused path is found, or
/// [`Error::Exists`] is returned after `MAX_ROLLOVER_RESOLVE_ATTEMPTS`.
fn unique_successor_path(
    root: &Path,
    config: &Config,
    base: &str,
) -> Result<(String, PathBuf, PathBuf)> {
    let mut candidate = next_rollover_id(base);
    for _ in 0..MAX_ROLLOVER_RESOLVE_ATTEMPTS {
        let rel = session_relative_path(config, &candidate);
        let abs = root.join(&rel);
        if !abs.exists() {
            return Ok((candidate, rel, abs));
        }
        candidate = next_rollover_id(&candidate);
    }
    Err(Error::Exists(
        root.join(session_relative_path(config, &candidate)),
    ))
}

fn next_rollover_id(old: &str) -> String {
    if let Some((base, suffix)) = old.rsplit_once("-cont-")
        && let Ok(n) = suffix.parse::<u32>()
    {
        return format!("{base}-cont-{}", n + 1);
    }
    format!("{old}-cont-2")
}

fn archive_with_successor(
    abs_path: &Path,
    full_content: &str,
    successor_id: &str,
    today: chrono::NaiveDate,
) -> Result<()> {
    let (yaml_opt, body) = split_frontmatter(full_content);
    let yaml_str = yaml_opt.ok_or_else(|| Error::Parse {
        path: abs_path.to_path_buf(),
        source: ParseError::FrontmatterDelimiter,
    })?;
    let mut editor = FrontmatterEditor::parse(yaml_str, abs_path)?;
    editor.set("status", ARCHIVED);
    editor.set("superseded_by", successor_id);
    editor.set("updated", &today.to_string());
    let new_content = format!("---\n{}---\n{}", editor.render(), body);
    path_guard::write_atomic(abs_path, &new_content)
}

/// Write a fresh session document in a single atomic operation.
///
/// `supersedes` carries the predecessor session id during rollover —
/// keeping it inline (rather than a follow-up `update_frontmatter`
/// write) means the chain link is established by the same atomic
/// rename as the rest of the document. A crash between the two writes
/// previously left the new session lacking its backward link while the
/// old session already advertised the forward one.
fn write_new_session(
    log_kind: &str,
    session_id: &str,
    abs_path: &Path,
    today: chrono::NaiveDate,
    spec: &LogEventSpec,
    supersedes: &[&str],
) -> Result<()> {
    let title = format!("Session {today}");
    let related = dedup_preserve_order(&spec.related);
    let tags = dedup_preserve_order(&spec.tags);

    use std::fmt::Write;
    let mut fm = String::new();
    writeln!(fm, "id: {}", yaml_text::quote(session_id)).unwrap();
    writeln!(fm, "title: {}", yaml_text::quote(&title)).unwrap();
    writeln!(fm, "kind: {}", yaml_text::quote(log_kind)).unwrap();
    writeln!(fm, "status: {}", yaml_text::quote("active")).unwrap();
    writeln!(fm, "created: {today}").unwrap();
    writeln!(fm, "updated: {today}").unwrap();
    writeln!(fm, "{EVENT_COUNT_KEY}: {}", yaml_text::quote("1")).unwrap();
    write_yaml_list(&mut fm, "supersedes", supersedes);
    let related_refs: Vec<&str> = related.iter().map(String::as_str).collect();
    let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
    write_yaml_list(&mut fm, "related", &related_refs);
    write_yaml_list(&mut fm, "tags", &tag_refs);

    let body = format!(
        "# {title}\n\n{EVENTS_HEADING}\n\n{}\n",
        format_event_line(today, &spec.summary),
    );
    let content = format!("---\n{fm}---\n\n{body}");
    path_guard::write_atomic(abs_path, &content)
}

fn write_yaml_list(buf: &mut String, key: &str, items: &[&str]) {
    if items.is_empty() {
        return;
    }
    use std::fmt::Write;
    writeln!(buf, "{key}:").unwrap();
    for item in items {
        writeln!(buf, "  - {}", yaml_text::quote(item)).unwrap();
    }
}

fn read_event_count(editor: &FrontmatterEditor) -> usize {
    match editor.scalar(EVENT_COUNT_KEY) {
        Scalar::Value(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn merge_list(yaml: &str, key: &str, additions: &[String]) -> Vec<String> {
    let existing = read_string_list(yaml, key);
    let mut out: Vec<String> = Vec::with_capacity(existing.len() + additions.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for item in existing.iter().chain(additions.iter()) {
        if !item.is_empty() && seen.insert(item.clone()) {
            out.push(item.clone());
        }
    }
    out
}

fn read_string_list(yaml: &str, key: &str) -> Vec<String> {
    let value: yaml_serde::Value = match yaml_serde::from_str(yaml) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mapping = match value.as_mapping() {
        Some(m) => m,
        None => return Vec::new(),
    };
    let v = match mapping.get(yaml_serde::Value::String(key.to_string())) {
        Some(v) => v,
        None => return Vec::new(),
    };
    if let Some(seq) = v.as_sequence() {
        seq.iter()
            .filter_map(|i| i.as_str().map(String::from))
            .collect()
    } else if let Some(s) = v.as_str() {
        vec![s.to_string()]
    } else {
        Vec::new()
    }
}

fn dedup_preserve_order(items: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
    for item in items {
        if !item.is_empty() && seen.insert(item.clone()) {
            out.push(item.clone());
        }
    }
    out
}

fn set_list_strings(editor: &mut FrontmatterEditor, key: &str, items: &[String]) {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    editor.set_list(key, &refs);
}

fn format_event_line(today: chrono::NaiveDate, summary: &str) -> String {
    format!("- **{today}** — {summary}")
}

/// Insert a new event line at the end of the `## Events` section.
///
/// Heading detection is line-anchored so a heading like `## Events
/// Summary` further down the body is not confused for the canonical
/// section. If no `## Events` section exists, one is appended.
fn append_event_line(body: &str, today: chrono::NaiveDate, summary: &str) -> String {
    let event = format_event_line(today, summary);
    let lines: Vec<&str> = body.lines().collect();

    let Some(events_idx) = lines.iter().position(|l| *l == EVENTS_HEADING) else {
        let trimmed = body.trim_end();
        return format!("{trimmed}\n\n{EVENTS_HEADING}\n\n{event}\n");
    };

    // Find the next top-level heading after the Events section, if any.
    let next_section_idx = lines
        .iter()
        .enumerate()
        .skip(events_idx + 1)
        .find(|(_, l)| l.starts_with("## "))
        .map(|(i, _)| i);

    let head_end = next_section_idx.unwrap_or(lines.len());
    let mut head: Vec<&str> = lines[..head_end].to_vec();
    while matches!(head.last(), Some(s) if s.is_empty()) {
        head.pop();
    }
    let mut out = head.join("\n");
    out.push('\n');
    out.push_str(&event);
    out.push('\n');
    if let Some(idx) = next_section_idx {
        out.push('\n');
        out.push_str(&lines[idx..].join("\n"));
        if body.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Resume context for the most recent session.
#[derive(Debug, Serialize)]
pub struct Continuation {
    #[serde(flatten)]
    pub session: NodeRef,
    /// Whole-day age — frontmatter dates are day-precision so anything
    /// finer would be invented data. Clamps to 0 for future-dated
    /// sessions (clock skew, post-dating).
    pub session_age_days: u32,
    pub event_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_summary: Option<String>,
    pub pack: Pack,
}

#[derive(Debug, Clone, Default)]
pub struct ContinueOptions {
    /// Override `config.session.default_continue_days`.
    pub since_days: Option<u32>,
    /// Override [`crate::query::pack::DEFAULT_TOKEN_BUDGET`].
    pub token_budget: Option<usize>,
    /// Override [`crate::query::pack::DEFAULT_MAX_DEPTH`].
    pub max_depth: Option<u32>,
}

/// Locate the most recent session and return a context bundle anchored
/// to it. `Ok(None)` when no session exists inside the window — agents
/// then fall back to bootstrapping from scratch.
pub fn continue_from_last_session(
    root: &Path,
    config: &Config,
    opts: ContinueOptions,
) -> Result<Option<Continuation>> {
    let log_kind = require_log_kind(config)?;
    // `Config::validate` guarantees `default_continue_days >= 1`, so a
    // defensive `.max(1)` here would be dead code that misled future
    // readers about whether 0 was a real possibility.
    let since_days = opts
        .since_days
        .unwrap_or(config.session.default_continue_days);
    let token_budget = opts.token_budget.unwrap_or(pack::DEFAULT_TOKEN_BUDGET);
    let max_depth = opts.max_depth.unwrap_or(pack::DEFAULT_MAX_DEPTH);

    let result = builder::build(root, config, false)?;
    let graph = &result.graph;

    let mut recent_entries = recent::find_recent(
        graph,
        &RecencyOptions {
            since: RecencySince::Days(since_days),
            kind: Some(log_kind.to_string()),
            field: RecencyField::Updated,
            limit: Some(1),
        },
    );
    let Some(entry) = recent_entries.pop() else {
        return Ok(None);
    };

    let session_node = graph.require_node(&entry.node.id)?;
    let session_abs = root.join(&session_node.path);
    let content = std::fs::read_to_string(&session_abs).map_err(|source| Error::Io {
        path: session_abs.clone(),
        source,
    })?;
    let (_, body) = split_frontmatter(&content);

    let event_count = read_event_count_attr(session_node);
    let last_event_summary = extract_last_event_summary(body);
    let session_age_days = age_days(session_node);

    let pack = pack::build_pack(
        graph,
        config,
        root,
        &session_node.id,
        token_budget,
        max_depth,
    )?;

    Ok(Some(Continuation {
        session: NodeRef::from_node(session_node),
        session_age_days,
        event_count,
        last_event_summary,
        pack,
    }))
}

fn read_event_count_attr(node: &Node) -> usize {
    node.attrs
        .get(EVENT_COUNT_KEY)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Whole days since the node was last touched. Always ≥ 0 — a future
/// `updated` (clock skew, deliberate post-dating) clamps to 0 rather
/// than producing a negative age that callers must defensively handle.
fn age_days(node: &Node) -> u32 {
    let Some(date) = node.updated.or(node.created) else {
        return 0;
    };
    crate::query::days_between_clamped(Local::now().date_naive(), date)
}

/// Last `- **<date>** — <text>` line *inside the `## Events` section*,
/// returning the text portion. Returns `None` when no event line exists
/// in that section.
///
/// Scoping the search to the Events section means a stray
/// `- **note** — important` bullet elsewhere in the body cannot be
/// mistaken for an event entry.
fn extract_last_event_summary(body: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.iter().position(|l| *l == EVENTS_HEADING)? + 1;
    let end = lines
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, l)| l.starts_with("## "))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());

    lines[start..end].iter().rev().find_map(|raw| {
        let trimmed = raw.trim_start();
        if !trimmed.starts_with("- **") {
            return None;
        }
        trimmed
            .split_once("— ")
            .map(|(_, summary)| summary.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SessionConfig};
    use tempfile::TempDir;

    fn project_with_session() -> (TempDir, Config) {
        let tmp = TempDir::new().unwrap();
        let mut config = Config::default();
        config.kinds.allowed = vec!["generic".into(), "session".into()];
        config.session = SessionConfig {
            log_kind: Some("session".into()),
            session_dir: "_sessions".into(),
            max_events_per_session: 3,
            default_continue_days: 1,
        };
        (tmp, config)
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    fn outcome_kind(result: &LogEventResult) -> &'static str {
        match result.outcome {
            LogEventOutcome::Created => "created",
            LogEventOutcome::Appended => "appended",
            LogEventOutcome::RolledOver { .. } => "rolled_over",
        }
    }

    #[test]
    fn first_event_creates_session_with_event_count_one() {
        let (tmp, config) = project_with_session();
        let result = log_event(
            tmp.path(),
            &config,
            LogEventSpec {
                session_id: Some("session-test-1".into()),
                summary: "first event".into(),
                related: vec!["adr-001".into()],
                tags: vec!["auth".into()],
            },
        )
        .unwrap();

        assert_eq!(outcome_kind(&result), "created");
        assert_eq!(result.event_index, 1);

        let content = read(&tmp.path().join(&result.session_path));
        assert!(content.contains("id: \"session-test-1\""));
        assert!(content.contains("kind: \"session\""));
        assert!(content.contains("status: \"active\""));
        assert!(content.contains("event_count: \"1\""));
        assert!(content.contains("related:\n  - \"adr-001\""));
        assert!(content.contains("tags:\n  - \"auth\""));
        assert!(content.contains("## Events"));
        assert!(content.contains("— first event"));
    }

    #[test]
    fn second_event_appends_and_merges_related() {
        let (tmp, config) = project_with_session();
        let id = "session-test-2";
        log_event(
            tmp.path(),
            &config,
            LogEventSpec {
                session_id: Some(id.into()),
                summary: "first".into(),
                related: vec!["a".into()],
                ..Default::default()
            },
        )
        .unwrap();
        let r = log_event(
            tmp.path(),
            &config,
            LogEventSpec {
                session_id: Some(id.into()),
                summary: "second".into(),
                related: vec!["a".into(), "b".into()],
                tags: vec!["x".into()],
            },
        )
        .unwrap();

        assert_eq!(outcome_kind(&r), "appended");
        assert_eq!(r.event_index, 2);

        let content = read(&tmp.path().join(&r.session_path));
        assert!(content.contains("event_count: \"2\""));
        assert!(content.contains("related:\n  - \"a\"\n  - \"b\""));
        assert!(content.contains("tags:\n  - \"x\""));
        assert!(content.contains("— first"));
        assert!(content.contains("— second"));
    }

    #[test]
    fn rollover_creates_successor_with_supersedes_in_one_write() {
        let (tmp, config) = project_with_session();
        let id = "session-test-3";
        for i in 1..=3 {
            log_event(
                tmp.path(),
                &config,
                LogEventSpec {
                    session_id: Some(id.into()),
                    summary: format!("event {i}"),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let r = log_event(
            tmp.path(),
            &config,
            LogEventSpec {
                session_id: Some(id.into()),
                summary: "rollover trigger".into(),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(r.event_index, 1, "successor starts fresh");
        match &r.outcome {
            LogEventOutcome::RolledOver { from_session_id } => {
                assert_eq!(from_session_id, id);
            }
            other => panic!("expected RolledOver outcome, got {other:?}"),
        }

        let new_id = r.session_id.clone();
        let new_path = tmp.path().join(&r.session_path);
        let new_content = read(&new_path);
        assert!(new_content.contains(&format!("id: \"{new_id}\"")));
        // Single atomic write established both the supersedes link
        // and the seed event — neither should be missing.
        assert!(new_content.contains(&format!("supersedes:\n  - \"{id}\"")));
        assert!(new_content.contains(&format!("related:\n  - \"{id}\"")));
        assert!(new_content.contains("— rollover trigger"));

        let old_path = tmp.path().join("_sessions").join(format!("{id}.md"));
        let old_content = read(&old_path);
        assert!(old_content.contains("status: \"archived\""));
        assert!(old_content.contains(&format!("superseded_by: \"{new_id}\"")));
    }

    #[test]
    fn auto_id_generates_timestamped_session() {
        let (tmp, config) = project_with_session();
        let r = log_event(
            tmp.path(),
            &config,
            LogEventSpec {
                summary: "auto id".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(r.session_id.starts_with("session-"));
        assert_eq!(outcome_kind(&r), "created");
    }

    #[test]
    fn refuses_when_log_kind_not_configured() {
        let tmp = TempDir::new().unwrap();
        let config = Config::default();
        let err = log_event(
            tmp.path(),
            &config,
            LogEventSpec {
                summary: "x".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        match err {
            Error::Config(msg) => assert!(msg.contains("session.log_kind")),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn next_rollover_id_handles_chain() {
        assert_eq!(next_rollover_id("session-A"), "session-A-cont-2");
        assert_eq!(next_rollover_id("session-A-cont-2"), "session-A-cont-3");
        assert_eq!(next_rollover_id("session-A-cont-99"), "session-A-cont-100");
    }

    #[test]
    fn rollover_skips_pre_existing_successor_paths() {
        let (tmp, config) = project_with_session();
        std::fs::create_dir_all(tmp.path().join("_sessions")).unwrap();
        std::fs::write(
            tmp.path().join("_sessions").join("session-A-cont-2.md"),
            "---\nid: collision\n---\n",
        )
        .unwrap();

        let id = "session-A";
        for i in 1..=3 {
            log_event(
                tmp.path(),
                &config,
                LogEventSpec {
                    session_id: Some(id.into()),
                    summary: format!("ev{i}"),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let rolled = log_event(
            tmp.path(),
            &config,
            LogEventSpec {
                session_id: Some(id.into()),
                summary: "trigger".into(),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(rolled.session_id, "session-A-cont-3");
        let preexisting =
            std::fs::read_to_string(tmp.path().join("_sessions").join("session-A-cont-2.md"))
                .unwrap();
        assert!(preexisting.contains("collision"));
    }

    #[test]
    fn auto_generated_id_includes_microseconds() {
        let now = Utc::now();
        let id = generate_session_id(now);
        let suffix = id.rsplit_once('-').map(|(_, s)| s).unwrap_or("");
        assert_eq!(suffix.len(), 6, "expected 6-digit micros, got {suffix:?}");
        assert!(suffix.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn extract_last_event_summary_picks_last_event_in_section() {
        let body = "## Events\n\n- **2026-05-08** — first\n- **2026-05-09** — second event\n";
        assert_eq!(
            extract_last_event_summary(body),
            Some("second event".to_string())
        );
    }

    #[test]
    fn extract_last_event_summary_ignores_bullets_outside_events_section() {
        // A bullet matching the event shape but outside the Events
        // section must NOT be mistaken for an event.
        let body = "## Events\n\n- **2026-05-08** — real event\n\n## Notes\n\n- **decoy** — not an event\n";
        assert_eq!(
            extract_last_event_summary(body),
            Some("real event".to_string())
        );
    }

    #[test]
    fn extract_last_event_summary_returns_none_on_empty_body() {
        assert_eq!(extract_last_event_summary(""), None);
        assert_eq!(
            extract_last_event_summary("# Title\n\nNo events yet.\n"),
            None
        );
    }

    #[test]
    fn append_event_line_inserts_inside_events_section_only() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
        let body = "# Title\n\n## Events\n\n- **2026-05-08** — old\n\n## Notes\n\nsome notes\n";
        let out = append_event_line(body, today, "new event");
        // New event must appear before ## Notes and after ## Events.
        let new_pos = out.find("— new event").expect("event missing");
        let notes_pos = out.find("## Notes").expect("notes section preserved");
        assert!(new_pos < notes_pos, "event must land before next section");
        // Old event still present.
        assert!(out.contains("— old"));
        // Notes body untouched.
        assert!(out.contains("some notes"));
    }

    #[test]
    fn append_event_line_creates_section_when_missing() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
        let body = "# Title\n\nNo events yet.\n";
        let out = append_event_line(body, today, "first");
        assert!(out.contains("## Events"));
        assert!(out.contains("— first"));
    }

    #[test]
    fn append_event_line_does_not_match_substring_headings() {
        // "## Events Summary" is NOT the events section. New event
        // must create the canonical heading rather than appending into
        // the lookalike.
        let today = chrono::NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
        let body = "# Title\n\n## Events Summary\n\nan overview.\n";
        let out = append_event_line(body, today, "first");
        assert!(out.contains("## Events Summary\n\nan overview"));
        // The canonical "## Events" heading is appended afterwards.
        let summary_pos = out.find("## Events Summary").unwrap();
        let events_pos = out
            .find("\n## Events\n")
            .expect("canonical heading appended");
        assert!(summary_pos < events_pos);
    }

    #[test]
    fn age_days_clamps_future_dates_to_zero() {
        use crate::model::{Kind, Status};
        use std::collections::BTreeMap;
        let mut node = Node {
            id: "x".into(),
            path: PathBuf::from("x.md"),
            title: "x".into(),
            kind: Kind::new("generic"),
            status: Status::new("active"),
            created: None,
            updated: Some(Local::now().date_naive() + chrono::Duration::days(5)),
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
        };
        assert_eq!(age_days(&node), 0, "future updated date must clamp to 0");
        node.updated = Some(Local::now().date_naive() - chrono::Duration::days(3));
        assert_eq!(age_days(&node), 3);
    }
}
