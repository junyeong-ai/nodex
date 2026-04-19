use std::path::PathBuf;

/// All errors that can occur in nodex-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("frontmatter parse error at {path}: {message}")]
    Frontmatter { path: PathBuf, message: String },

    #[error("YAML parse error at {path}: {source}")]
    Yaml {
        path: PathBuf,
        source: yaml_serde::Error,
    },

    #[error("config error: {0}")]
    Config(String),

    #[error("duplicate node id {id:?} found at {first} and {second}")]
    DuplicateId {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("supersedes cycle detected: {chain:?}")]
    SupersedesCycle { chain: Vec<String> },

    #[error("invalid lifecycle transition: {from:?} -> {to:?} for node {node_id:?}")]
    InvalidTransition {
        node_id: String,
        from: String,
        to: String,
    },

    #[error("node not found: {0}")]
    NodeNotFound(String),

    #[error("already exists: {path}")]
    AlreadyExists { path: PathBuf },

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
