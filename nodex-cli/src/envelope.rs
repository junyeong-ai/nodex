//! The one JSON envelope encoder shared by every bin target in this
//! crate: the `nodex` binary consumes it through `format`, and the
//! `contract-gate` binary includes the same file via `#[path]` — one
//! stable error-envelope contract, one encoder
//! (`.claude/rules/json-output.md`).

use serde::Serialize;

/// Standard error envelope: `{"ok": false, "error": {code, message}}`.
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
    /// The minimal constructor every classifier funnels into: the
    /// caller supplies the machine-dispatch `code` and the human
    /// `message`; the envelope shape itself is owned here.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: ErrorDetail {
                code: code.into(),
                message: message.into(),
            },
        }
    }
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
