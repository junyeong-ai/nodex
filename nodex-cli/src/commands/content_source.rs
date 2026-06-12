//! The byte-source grammar shared by commands that accept proposed
//! document bytes (`check <path> --content`, `scaffold --body`): `-`
//! reads stdin, anything else is a file path resolved against the
//! invoking directory (not `-C DIR` — the proposed bytes may
//! legitimately live outside the project).

use anyhow::Result;
use std::io::Read;
use std::path::PathBuf;

/// Read proposed content from `-` (stdin) or a file path. Failures are
/// typed through [`nodex_core::error::Error::Io`] so the envelope
/// classifier maps them to `IO_ERROR` — never the `INTERNAL_ERROR`
/// catch-all (see `.claude/rules/json-output.md`).
pub(crate) fn read_content_source(source: &str) -> Result<String> {
    if source == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| nodex_core::error::Error::Io {
                path: PathBuf::from("-"),
                source: e,
            })?;
        Ok(buf)
    } else {
        Ok(
            std::fs::read_to_string(source).map_err(|e| nodex_core::error::Error::Io {
                path: PathBuf::from(source),
                source: e,
            })?,
        )
    }
}
