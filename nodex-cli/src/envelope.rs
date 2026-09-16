//! The one JSON envelope encoder shared by every bin target in this
//! crate: the `nodex` binary consumes it through `format`, and the
//! `contract-gate` binary includes the same file via `#[path]` — one
//! stable error-envelope contract, one encoder
//! (`.claude/rules/json-output.md`).

use serde::Serialize;
use std::io::Write;

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
///
/// Stdout is the envelope's only channel, so a write that fails ends the
/// process with exit 2 whatever the command was about to exit with: a
/// `nodex check | head` under `pipefail` must not read as a check that passed.
/// A reader that closed the pipe asked for no more output, so that exit is
/// silent; any other failure — a full disk behind a redirect — is named on
/// stderr, the one channel left.
pub fn print_json<T: Serialize>(value: &T, pretty: bool) {
    // serde_json::to_string only fails on non-serializable types (e.g., maps with non-string keys).
    // All our types use String keys, so this is safe.
    let json = if pretty {
        serde_json::to_string_pretty(value).expect("all nodex types are JSON-serializable")
    } else {
        serde_json::to_string(value).expect("all nodex types are JSON-serializable")
    };
    let written = {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{json}").and_then(|()| stdout.flush())
    };
    if let Err(err) = written {
        if err.kind() != std::io::ErrorKind::BrokenPipe {
            let _ = writeln!(
                std::io::stderr(),
                "cannot write the JSON envelope to stdout: {err}"
            );
        }
        std::process::exit(2);
    }
}
