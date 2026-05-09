use std::path::PathBuf;

/// Errors raised by `nodex-core`.
///
/// Every variant maps to a single stable `code()` string, which is the
/// only error surface CLI / MCP / IDE consumers should pattern-match on.
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

    #[error("parse error at {path}: {source}")]
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
    MissingNode(String),

    #[error("path already exists: {0}")]
    Exists(PathBuf),

    #[error("path escapes project root: {0}")]
    OutsideRoot(PathBuf),
}

impl Error {
    /// Stable error code consumed by the CLI / MCP envelope.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "IO_ERROR",
            Self::Parse { .. } => "PARSE_ERROR",
            Self::Config(_) => "CONFIG_ERROR",
            Self::Cycle { .. } => "CYCLE_DETECTED",
            Self::DuplicateId { .. } => "DUPLICATE_ID",
            Self::Transition { .. } => "INVALID_TRANSITION",
            Self::MissingNode(_) => "NOT_FOUND",
            Self::Exists(_) => "ALREADY_EXISTS",
            Self::OutsideRoot(_) => "PATH_ESCAPES_ROOT",
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
    InvalidField {
        field: String,
        expected: &'static str,
    },

    #[error("yaml: {0}")]
    Yaml(#[from] yaml_serde::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
