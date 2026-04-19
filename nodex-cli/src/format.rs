use serde::Serialize;

/// Standard JSON envelope for all CLI output.
#[derive(Serialize)]
pub struct Envelope<T: Serialize> {
    pub ok: bool,
    pub data: T,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl<T: Serialize> Envelope<T> {
    pub fn success(data: T) -> Self {
        Self {
            ok: true,
            data,
            warnings: vec![],
        }
    }

    pub fn with_warnings(data: T, warnings: Vec<String>) -> Self {
        Self {
            ok: true,
            data,
            warnings,
        }
    }
}

#[derive(Serialize)]
pub struct ErrorEnvelope {
    pub ok: bool,
    pub error: ErrorDetail,
}

#[derive(Serialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

impl ErrorEnvelope {
    pub fn from_error(err: &anyhow::Error) -> Self {
        let code = classify_error(err);
        Self {
            ok: false,
            error: ErrorDetail {
                code,
                message: format!("{err:#}"),
            },
        }
    }

    /// Convert a clap parse error into the JSON envelope. Covers
    /// unknown arguments, unknown subcommands, invalid values, missing
    /// required arguments — every parse-time mismatch. Informational
    /// exits (`--help`, `--version`) are NOT routed here; they remain
    /// human-readable per CLI convention.
    pub fn from_clap_error(err: &clap::Error) -> Self {
        Self {
            ok: false,
            error: ErrorDetail {
                code: "INVALID_ARGUMENT".to_string(),
                message: err.render().to_string(),
            },
        }
    }
}

fn classify_error(err: &anyhow::Error) -> String {
    // Walk the error chain and check root cause type for precise classification
    for cause in err.chain() {
        if let Some(core_err) = cause.downcast_ref::<nodex_core::error::Error>() {
            return match core_err {
                nodex_core::error::Error::SupersedesCycle { .. } => "CYCLE_DETECTED",
                nodex_core::error::Error::DuplicateId { .. } => "DUPLICATE_ID",
                nodex_core::error::Error::Frontmatter { .. }
                | nodex_core::error::Error::Yaml { .. } => "PARSE_ERROR",
                nodex_core::error::Error::InvalidTransition { .. } => "INVALID_TRANSITION",
                nodex_core::error::Error::NodeNotFound(_) => "NOT_FOUND",
                nodex_core::error::Error::AlreadyExists { .. } => "ALREADY_EXISTS",
                nodex_core::error::Error::PathEscapesRoot { .. } => "PATH_ESCAPES_ROOT",
                nodex_core::error::Error::Config(_) => "CONFIG_ERROR",
                nodex_core::error::Error::Io { .. } => "IO_ERROR",
                nodex_core::error::Error::Other(_) => "INTERNAL_ERROR",
            }
            .to_string();
        }
    }
    "INTERNAL_ERROR".to_string()
}

/// Print a serializable value as JSON to stdout.
pub fn print_json<T: Serialize>(value: &T, pretty: bool) {
    // serde_json::to_string only fails on non-serializable types (e.g., maps with non-string keys).
    // All our types use String keys, so this is safe.
    let json = if pretty {
        serde_json::to_string_pretty(value).expect("all nodex types are JSON-serializable")
    } else {
        serde_json::to_string(value).expect("all nodex types are JSON-serializable")
    };
    println!("{json}");
}
