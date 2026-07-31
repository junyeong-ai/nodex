use std::path::PathBuf;

/// What a graph lookup asked for. A reverse lookup names a path rather than
/// an id, and both can miss, so the subject is carried as a type instead of
/// being encoded into the id field — a `path=…` sentinel inside a string
/// named `id` reads back as an id no document could ever have, and the
/// message that renders it says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Id(String),
    Path(PathBuf),
}

impl std::fmt::Display for Lookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Id(id) => write!(f, "id {id:?}"),
            Self::Path(path) => {
                write!(f, "path {:?}", crate::path_guard::forward_string(path))
            }
        }
    }
}

/// Errors raised by `nodex-core`.
///
/// Every variant maps to a single stable `code()` string, which is the
/// only error surface CLI / IDE consumers should pattern-match on.
/// Adding a variant requires extending `code()` — the compiler enforces
/// it, so the JSON envelope contract cannot silently drift.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    // Display names only this layer; the `#[source]` cause is appended
    // once by the chain renderer (`{:#}`). Interpolating `{source}` here
    // too would double it — the `Io` variant is the pattern to match.
    #[error("parse error at {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: ParseError,
    },

    #[error("config error: {0}")]
    Config(String),

    #[error("graph cycle: {chain:?}")]
    Cycle { chain: Vec<String> },

    #[error("duplicate node id {id:?} at {first} and {second}")]
    DuplicateId {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("invalid transition for {node_id:?}: {from:?} → {to:?}")]
    Transition {
        node_id: String,
        from: String,
        to: String,
    },

    #[error("missing node: {0}")]
    MissingNode(Lookup),

    /// No graph snapshot exists at the project's `<output.dir>/graph.json`.
    /// Distinct from [`Error::Io`] so "unbuilt project" is machine-
    /// distinguishable from a real read failure — consumers dispatch on
    /// the code, never on message text.
    #[error("graph snapshot missing at {path} — run `nodex build`")]
    MissingGraph { path: PathBuf },

    /// A lookup missed against a graph snapshot that no longer matches the
    /// working tree, so absence from the snapshot is not absence from the
    /// project. Distinct from [`Error::MissingNode`] because the remedy
    /// differs — a rebuild, not a corrected id — and because a consumer
    /// dispatching on `NOT_FOUND` would otherwise conclude the document does
    /// not exist when it is sitting on disk.
    #[error(
        "{asked} is absent from a graph snapshot that no longer matches the working tree \
             ({divergence}) — run `nodex build`"
    )]
    StaleGraph { asked: Lookup, divergence: String },

    #[error("path already exists: {0}")]
    Exists(PathBuf),

    #[error("path escapes project root: {0}")]
    OutsideRoot(PathBuf),

    /// A write gate refused a proposal because the project's own `check`
    /// flags what it would produce: each finding is `rule_id: message` for
    /// an Error-severity violation the proposal *introduces* (absent from
    /// the pre-proposal report — pre-existing project violations never
    /// refuse a write). The remediation is the proposal, never
    /// `nodex.toml`. `subject` is what the seam was asked to do, so one
    /// code carries every write seam's own words.
    #[error("{subject} introduces check violations: {}", findings.join("; "))]
    ContentViolations {
        subject: String,
        findings: Vec<String>,
    },

    #[error("version mismatch: nodex {actual} does not satisfy {requirement:?}")]
    VersionMismatch {
        actual: &'static str,
        requirement: String,
    },

    #[error("git: {context} — {stderr}")]
    Git { context: String, stderr: String },
}

impl Error {
    /// Stable error code consumed by the JSON envelope.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "IO_ERROR",
            Self::Parse { .. } => "PARSE_ERROR",
            Self::Config(_) => "CONFIG_ERROR",
            Self::Cycle { .. } => "CYCLE_DETECTED",
            Self::DuplicateId { .. } => "DUPLICATE_ID",
            Self::Transition { .. } => "INVALID_TRANSITION",
            Self::MissingNode(_) => "NOT_FOUND",
            Self::MissingGraph { .. } => "GRAPH_MISSING",
            Self::StaleGraph { .. } => "GRAPH_OUTDATED",
            Self::Exists(_) => "ALREADY_EXISTS",
            Self::OutsideRoot(_) => "PATH_ESCAPES_ROOT",
            Self::ContentViolations { .. } => "CONTENT_VIOLATIONS",
            Self::VersionMismatch { .. } => "VERSION_MISMATCH",
            Self::Git { .. } => "GIT_ERROR",
        }
    }
}

/// Structured input parse failures. Always wrapped in [`Error::Parse`]
/// so the failing path is preserved alongside the cause.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("frontmatter missing closing delimiter")]
    FrontmatterDelimiter,

    #[error("frontmatter is not a YAML mapping")]
    FrontmatterShape,

    #[error("field {field:?} expected {expected}")]
    InvalidField { field: String, expected: String },

    // Display names only this layer; the wrapped error is the `#[from]`
    // source, appended once by the chain renderer (`{:#}`) — never
    // interpolate it here too.
    #[error("yaml")]
    Yaml(#[from] yaml_serde::Error),

    #[error("json")]
    Json(#[from] serde_json::Error),
}

/// Render an error with its full `source` chain, layers joined by `: `.
/// The library-side equivalent of anyhow's `{:#}` (which nodex-core
/// cannot use — `thiserror` only): every `Display` here names a single
/// layer, so the few places nodex-core formats an error into a message
/// itself (e.g. a build warning) use this to surface the wrapped cause.
pub fn chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        out.push_str(": ");
        out.push_str(&inner.to_string());
        source = inner.source();
    }
    out
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn chain_renders_each_layer_once() {
        // A `serde_json` custom error wrapped in `ParseError::Json` wrapped
        // in `Error::Parse`. Each layer's Display names only itself, so the
        // chain is the three layers joined once — never the doubling that a
        // source-interpolating Display produces under `{:#}`.
        let json_err: serde_json::Error = serde_json::from_str::<i32>("not json").unwrap_err();
        let detail = json_err.to_string();
        let err = Error::Parse {
            path: PathBuf::from("graph.json"),
            source: ParseError::Json(json_err),
        };
        let rendered = chain(&err);
        assert_eq!(
            rendered,
            format!("parse error at graph.json: json: {detail}")
        );
        // The cause text appears exactly once (no `{:#}`/Display doubling).
        assert_eq!(rendered.matches(&detail).count(), 1);
    }
}
