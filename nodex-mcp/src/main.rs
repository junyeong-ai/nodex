//! `nodex-mcp` — Model Context Protocol stdio server.
//!
//! Speaks newline-delimited JSON-RPC 2.0 on stdin/stdout, the canonical
//! MCP stdio transport. Wraps `nodex-core` directly: every tool call
//! reloads the project config and rebuilds the graph in process, so the
//! server reflects on-disk state per request without a watcher.

use clap::Parser;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

mod protocol;
mod resources;
mod tools;

#[derive(Parser)]
#[command(name = "nodex-mcp", version, about)]
struct Args {
    /// Project root the server operates against. Must contain `nodex.toml`.
    #[arg(long, short = 'C', default_value = ".")]
    root: PathBuf,
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    let root = args.root.canonicalize().unwrap_or(args.root);

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    let mut buffer = String::new();
    while input.read_line(&mut buffer)? > 0 {
        let line = buffer.trim();
        if line.is_empty() {
            buffer.clear();
            continue;
        }

        let response = match serde_json::from_str::<protocol::Request>(line) {
            Ok(req) => protocol::dispatch(&root, req),
            Err(e) => Some(protocol::Response::error(
                serde_json::Value::Null,
                protocol::INVALID_REQUEST,
                &format!("invalid JSON-RPC request: {e}"),
            )),
        };

        if let Some(resp) = response {
            let json = serde_json::to_string(&resp).expect("response is JSON-serialisable");
            writeln!(output, "{json}")?;
            output.flush()?;
        }

        buffer.clear();
    }

    Ok(())
}
