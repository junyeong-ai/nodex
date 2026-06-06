use nodex_core::Config;
use serde::Serialize;

/// Emit a read-only command's payload, appending the binary-compat
/// advisory (if the running binary falls outside `meta.nodex_version`)
/// to the envelope warnings. The single seam where read output merges
/// this cross-cutting advisory, so no query handler has to remember it.
pub fn emit_read<T: Serialize>(data: T, config: &Config, pretty: bool) {
    emit_read_with(data, vec![], config, pretty);
}

/// [`emit_read`] for commands that already carry domain warnings (e.g.
/// `build` surfacing skipped rules); the advisory is merged in.
pub fn emit_read_with<T: Serialize>(
    data: T,
    mut warnings: Vec<String>,
    config: &Config,
    pretty: bool,
) {
    if let Some(advisory) = nodex_core::binary_compat_warning(config) {
        warnings.push(advisory);
    }
    print_json(&Envelope::with_warnings(data, warnings), pretty);
}

/// Standard JSON envelope for all CLI output.
#[derive(Serialize)]
pub struct Envelope<T: Serialize> {
    pub ok: bool,
    pub data: T,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Canonical `{ items: [...], total: N }` payload for every list-style
/// query response. Constructing through [`ItemsEnvelope::new`] keeps
/// `total` in lockstep with `items.len()` — the single seam where the
/// invariant lives, so no command can ship the two fields out of sync.
#[derive(Serialize)]
pub struct ItemsEnvelope<T: Serialize> {
    pub items: Vec<T>,
    pub total: usize,
}

impl<T: Serialize> ItemsEnvelope<T> {
    pub fn new(items: Vec<T>) -> Self {
        let total = items.len();
        Self { items, total }
    }
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
    err.chain()
        .find_map(|cause| {
            cause
                .downcast_ref::<nodex_core::error::Error>()
                .map(|e| e.code())
        })
        .unwrap_or("INTERNAL_ERROR")
        .to_string()
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
