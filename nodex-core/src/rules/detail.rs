//! Typed, machine-actionable payload of a [`crate::rules::Violation`].
//!
//! Every violation carries a [`ViolationDetails`] — an internally-tagged
//! enum whose `type` discriminator is the stable machine category a
//! consumer branches on, and whose variant fields are the structured
//! parameters an agent needs to propose a fix (the offending field, the
//! expected set, the failing value) without parsing the human `message`.
//!
//! The `message` on a [`crate::rules::Violation`] is a *rendered projection* of the
//! details, produced once by [`ViolationDetails::render_message`] — the
//! same single-source discipline [`crate::model::edge::UnresolvedCause`]
//! uses for its `Display`. Structured data and prose can never disagree
//! because there is only one constructor ([`crate::rules::Violation::new`]) and it
//! derives the message from the details.
//!
//! The tagged-enum shape mirrors the project's sole internally-tagged
//! precedent, [`crate::model::edge::ResolvedTarget`] (`#[serde(tag =
//! "type")]`). Adding a rule means adding a variant; the exhaustive
//! `match` in `render_message` then forces a prose rendering for it at
//! compile time, so a new rule can never ship a category without a
//! message.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{BodyImmutableMode, FieldType, ImmutableTrigger};
use crate::model::edge::UnresolvedCause;

/// The runtime shape of a JSON value, for type-mismatch reporting. A
/// consumer reads this to know what the document actually held versus
/// the declared [`FieldType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    Null,
    Bool,
    Integer,
    Float,
    String,
    Array,
    Object,
}

impl ValueKind {
    /// Classify a parsed JSON value. A number is `Integer` when it fits
    /// `i64`/`u64`, else `Float` — matching the validator's own split.
    pub fn of(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Bool,
            Value::Number(n) if n.is_i64() || n.is_u64() => Self::Integer,
            Value::Number(_) => Self::Float,
            Value::String(_) => Self::String,
            Value::Array(_) => Self::Array,
            Value::Object(_) => Self::Object,
        }
    }

    fn prose(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "bool",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::String => "string",
            Self::Array => "array",
            Self::Object => "object",
        }
    }
}

/// The document that contributed the most commits to a `git_drift`
/// total — the single edge an operator should review first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DriftHotspot {
    /// Referenced document id (or raw path for an unresolved covered path).
    pub id: String,
    /// Commits to that document since the source was reviewed.
    pub commits: u32,
}

/// The structured cause of a [`crate::rules::Violation`]. Internally tagged on
/// `type`; each variant carries exactly the data its human `message`
/// renders from, so [`render_message`](Self::render_message) is a total
/// function over the typed payload and never loses information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViolationDetails {
    /// An in-scope document failed to parse and has no node. `path` is what
    /// the finding is about — a document that failed to parse has no id to
    /// be known by, so the path is the whole of its identity — and
    /// `content_digest` is the byte state it failed in, so the write-gate
    /// before/after delta cancels only a proposal that leaves the same
    /// document broken the same way. The digest is empty — never a hash of
    /// nothing — when the file could not be read at all, because there were
    /// no bytes to hash. `reason` is the operator's full error chain, which
    /// `render_message` shows with a short prefix of the digest.
    ParseFailure {
        path: String,
        reason: String,
        content_digest: String,
    },
    /// A built-in frontmatter field failed its type and reads as absent.
    FieldParse {
        field: String,
        expected: String,
        found: String,
    },
    /// A required frontmatter field is missing.
    RequiredField { field: String },
    /// An inferrable built-in (`id` / `title` / `kind` / `status`) was
    /// left to inference where `schema.require_explicit` demands it be
    /// authored.
    ExplicitField { field: String },
    /// A typed `attrs` field does not parse as its declared [`FieldType`].
    /// `invalid_date` carries the offending string when the value *is* a
    /// string but not a valid `YYYY-MM-DD` date.
    FieldType {
        field: String,
        expected: FieldType,
        found: ValueKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invalid_date: Option<String>,
    },
    /// An enum-constrained field holds a value outside its allowed set.
    FieldEnum {
        field: String,
        found: String,
        allowed: Vec<String>,
    },
    /// Strict mode: a frontmatter key is neither built-in nor declared.
    UnknownField { field: String },
    /// A `when X require Y` predicate held but `Y` is absent.
    CrossField { when: String, require: String },
    /// An active document has not been reviewed within the stale threshold.
    StaleReview { days: i64, threshold_days: u32 },
    /// Referenced documents accrued more commits since review than the
    /// drift threshold allows.
    GitDrift {
        total_commits: u32,
        threshold: u32,
        reviewed: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hottest: Option<DriftHotspot>,
    },
    /// A filename does not match its configured pattern.
    FilenamePattern { filename: String, pattern: String },
    /// A gap in a directory's sequential file numbering. The number width
    /// matches `scaffold`'s `u64` sequence so write and check agree.
    SequentialNumbering { previous: u64, current: u64 },
    /// Two files in a directory share the same number.
    UniqueNumbering {
        number: u64,
        /// The documents sharing the number, by node id and sorted — what
        /// makes this conflict *this* conflict, since a document keeps its id
        /// wherever it sits.
        members: Vec<String>,
        /// Where they sit, for the operator to go and look. Evidence, not
        /// identity: a member that moved is the same member.
        paths: Vec<String>,
    },
    /// A captured body-line token is outside its declared enum.
    BodyLine {
        line: usize,
        capture: String,
        value: String,
        allowed: Vec<String>,
    },
    /// A locked frontmatter field changed after the document went terminal.
    FrontmatterFieldImmutable {
        field: String,
        before_status: String,
    },
    /// `status` itself changed after the document was already terminal.
    StatusImmutable { from: String, to: String },
    /// A locked body changed. `trigger`/`mode` are the policy that locked
    /// it; the optional fields carry what the policy's message reports.
    BodyImmutable {
        trigger: ImmutableTrigger,
        mode: BodyImmutableMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_lines: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_lines: Option<usize>,
    },
    /// A configured acyclic relation contains a cycle. Node-less; the
    /// `ring` is the full cycle.
    Cycle { relation: String, ring: Vec<String> },
    /// A reference does not resolve. Reuses the build resolver's typed
    /// [`UnresolvedCause`] so the gate and the report share one cause
    /// vocabulary.
    UnresolvedReference {
        relation: String,
        raw_target: String,
        location: String,
        cause: UnresolvedCause,
    },
}

impl ViolationDetails {
    /// Render the one human-readable line for this violation. The single
    /// source every `Violation::message` derives from.
    /// What makes this the *same finding* as another — the cause, with any
    /// payload that merely locates it normalised away.
    ///
    /// A proposal gate pairs before and after findings to answer for exactly
    /// what a mutation introduces, and it pairs on this. Most variants are
    /// all cause: a missing field, a value outside its enum, a ring of ids.
    /// Two are not, and both carry their subject alongside what merely
    /// renders it — `UniqueNumbering` the documents sharing a number beside
    /// the files they sit in, `ParseFailure` the document that failed beside
    /// the error chain read from it.
    ///
    /// The match is exhaustive rather than a wildcard so a variant added
    /// later has to decide, at compile time, whether any of its payload is
    /// evidence — the same discipline [`Self::render_message`] enforces for
    /// the prose.
    pub(crate) fn cause(&self) -> Self {
        match self {
            // The paths are how the operator finds the conflict; the members
            // are what the conflict is between, and they keep their ids
            // wherever they sit. The number alone would pair two different
            // conflicts that happen to share it.
            Self::UniqueNumbering {
                number, members, ..
            } => Self::UniqueNumbering {
                number: *number,
                members: members.clone(),
                paths: Vec::new(),
            },
            // The document that failed and the bytes it failed in. The
            // reason renders from both and adds the operating system's
            // wording for a read that failed, which differs between the
            // platforms one project is checked on.
            Self::ParseFailure {
                path,
                content_digest,
                ..
            } => Self::ParseFailure {
                path: path.clone(),
                reason: String::new(),
                content_digest: content_digest.clone(),
            },
            Self::FieldParse { .. }
            | Self::RequiredField { .. }
            | Self::ExplicitField { .. }
            | Self::FieldType { .. }
            | Self::FieldEnum { .. }
            | Self::UnknownField { .. }
            | Self::CrossField { .. }
            | Self::StaleReview { .. }
            | Self::GitDrift { .. }
            | Self::FilenamePattern { .. }
            | Self::SequentialNumbering { .. }
            | Self::BodyLine { .. }
            | Self::FrontmatterFieldImmutable { .. }
            | Self::StatusImmutable { .. }
            | Self::BodyImmutable { .. }
            | Self::Cycle { .. }
            | Self::UnresolvedReference { .. } => self.clone(),
        }
    }

    pub fn render_message(&self) -> String {
        match self {
            Self::ParseFailure {
                reason,
                content_digest,
                ..
            } => {
                // `content_digest` is the full hash; the human line shows a
                // short, readable prefix. The reason already names the path.
                let short = content_digest.get(..12).unwrap_or(content_digest);
                format!("{reason} (content {short})")
            }
            Self::FieldParse {
                field,
                expected,
                found,
            } => format!(
                "field {field:?}: expected {expected}, got {found} — the value was not parsed and \
                 the field reads as absent"
            ),
            Self::RequiredField { field } => format!("missing required field: {field}"),
            Self::ExplicitField { field } => format!(
                "field {field:?} must be authored explicitly; it was left to inference \
                 (schema.require_explicit)"
            ),
            Self::FieldType {
                field,
                expected,
                found,
                invalid_date,
            } => {
                let inner = if let Some(s) = invalid_date {
                    format!("invalid date {s:?}, expected YYYY-MM-DD")
                } else {
                    let expected_prose = match expected {
                        FieldType::String => "expected string",
                        FieldType::Integer => "expected integer",
                        FieldType::Bool => "expected bool",
                        FieldType::Date => "expected date (YYYY-MM-DD)",
                    };
                    format!("{expected_prose}, got {}", found.prose())
                };
                format!("field {field:?}: {inner}")
            }
            Self::FieldEnum {
                field,
                found,
                allowed,
            } => format!("field {field:?} has value {found:?}; expected one of {allowed:?}"),
            Self::UnknownField { field } => format!(
                "unknown frontmatter field {field:?}; declare it in [schema].types or \
                 [schema].enums (per-kind override allowed), or switch [schema].mode to \"lenient\""
            ),
            Self::CrossField { when, require } => {
                format!("when {when}, field {require:?} is required")
            }
            Self::StaleReview {
                days,
                threshold_days,
            } => format!("not reviewed for {days} days (threshold: {threshold_days} days)"),
            Self::GitDrift {
                total_commits,
                threshold,
                reviewed,
                hottest,
            } => {
                let suffix = hottest
                    .as_ref()
                    .map(|h| format!(" (hottest: {} with {})", h.id, h.commits))
                    .unwrap_or_default();
                format!(
                    "{total_commits} commits to referenced docs since reviewed={reviewed} \
                     (threshold {threshold}){suffix}"
                )
            }
            Self::FilenamePattern { filename, pattern } => {
                format!("filename {filename:?} does not match pattern {pattern:?}")
            }
            Self::SequentialNumbering { previous, current } => {
                format!("gap in numbering: {previous} → {current}")
            }
            Self::UniqueNumbering { number, paths, .. } => {
                format!("duplicate number {number} in files: {}", paths.join(", "))
            }
            Self::BodyLine {
                line,
                capture,
                value,
                allowed,
            } => format!(
                "line {line}: capture {capture:?} value {value:?} is not in declared enum \
                 {allowed:?}"
            ),
            Self::FrontmatterFieldImmutable {
                field,
                before_status,
            } => format!(
                "field {field:?} is immutable once status is terminal (was: {before_status:?})"
            ),
            Self::StatusImmutable { from, to } => {
                format!("field \"status\" is immutable once terminal: {from:?} → {to:?}")
            }
            Self::BodyImmutable {
                trigger,
                mode,
                before_status,
                current_status,
                before_lines,
                after_lines,
            } => {
                let locked_because = match trigger {
                    ImmutableTrigger::Terminal => format!(
                        "body changed while status is terminal (was: {:?})",
                        before_status.as_deref().unwrap_or_default()
                    ),
                    ImmutableTrigger::Creation => format!(
                        "body changed on a document locked from creation (trigger=creation; \
                         status {:?} does not exempt it)",
                        current_status.as_deref().unwrap_or_default()
                    ),
                };
                match mode {
                    BodyImmutableMode::Frozen => {
                        format!("{locked_because}; mode=frozen forbids any body edit")
                    }
                    BodyImmutableMode::AppendOnly => format!(
                        "{locked_because}; mode=append_only requires the previous body to remain a \
                         prefix of the new body (before={} lines, after={} lines)",
                        before_lines.unwrap_or_default(),
                        after_lines.unwrap_or_default()
                    ),
                }
            }
            Self::Cycle { relation, ring } => {
                format!(
                    "cycle detected in '{relation}' relation: {}",
                    ring.join(" → ")
                )
            }
            Self::UnresolvedReference {
                relation,
                raw_target,
                location,
                cause,
            } => format!(
                "{relation} reference {raw_target:?} ({location}) does not resolve: {cause}"
            ),
        }
    }
}
