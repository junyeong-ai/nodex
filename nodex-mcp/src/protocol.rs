//! MCP / JSON-RPC 2.0 message shapes and dispatch.
//!
//! Notifications (no `id` in the request) yield `None` from
//! [`dispatch`] — the JSON-RPC spec forbids responding to them. All
//! other errors flow back as structured envelopes so a misbehaving
//! client cannot wedge the server.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

use crate::{resources, tools};

pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Debug, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.to_string(),
            }),
        }
    }
}

/// Route an incoming request to the right handler.
/// Returns `None` for notifications (per JSON-RPC 2.0).
pub fn dispatch(root: &Path, req: Request) -> Option<Response> {
    if req.jsonrpc != "2.0" {
        return Some(Response::error(
            req.id,
            INVALID_REQUEST,
            "jsonrpc must be \"2.0\"",
        ));
    }

    let is_notification = req.id.is_null();

    let response = match req.method.as_str() {
        "initialize" => Some(handle_initialize(req.id.clone())),
        "initialized" | "notifications/initialized" => None,
        "tools/list" => Some(Response::ok(req.id.clone(), tools::list_descriptors())),
        "tools/call" => Some(handle_tools_call(root, req.id.clone(), req.params)),
        "resources/list" => Some(Response::ok(
            req.id.clone(),
            json!({ "resources": resources::list_descriptors() }),
        )),
        "resources/read" => Some(handle_resources_read(root, req.id.clone(), req.params)),
        "ping" => Some(Response::ok(req.id.clone(), json!({}))),
        "shutdown" => Some(Response::ok(req.id.clone(), Value::Null)),
        unknown => Some(Response::error(
            req.id.clone(),
            METHOD_NOT_FOUND,
            &format!("unknown method: {unknown}"),
        )),
    };

    if is_notification { None } else { response }
}

fn handle_initialize(id: Value) -> Response {
    Response::ok(
        id,
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false }
            },
            "serverInfo": {
                "name": "nodex-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "Document graph index serving as long-term memory for AI agents. \
                             Tools query the graph (search, backlinks, chain, orphans, stale, \
                             issues, covered-by, recent, node detail), build token-budgeted \
                             context packs, validate against rules, scaffold new documents, \
                             append session-log events, and run lifecycle transitions \
                             (supersede / archive / deprecate / abandon / review). \
                             Static resources expose graph health (`nodex://graph/summary`), \
                             actionable issues (`nodex://graph/issues`), and recent changes \
                             (`nodex://graph/recent`) for ambient context."
        }),
    )
}

fn handle_tools_call(root: &Path, id: Value, params: Value) -> Response {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => {
            return Response::error(id, INVALID_PARAMS, "tools/call params.name is required");
        }
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    match tools::call(root, name, arguments) {
        Ok(value) => Response::ok(
            id,
            json!({
                "content": [
                    { "type": "text", "text": serialize_pretty(&value) }
                ],
                "isError": false,
                "structuredContent": value,
            }),
        ),
        Err(tools::ToolError::Unknown) => {
            Response::error(id, METHOD_NOT_FOUND, &format!("unknown tool: {name}"))
        }
        Err(tools::ToolError::InvalidArgs(msg)) => Response::error(id, INVALID_PARAMS, &msg),
        Err(tools::ToolError::Failure { code, message }) => Response::ok(
            id,
            json!({
                "content": [
                    { "type": "text", "text": format!("{code}: {message}") }
                ],
                "isError": true,
                "structuredContent": {
                    "ok": false,
                    "error": { "code": code, "message": message }
                },
            }),
        ),
        Err(tools::ToolError::Internal(msg)) => Response::error(id, INTERNAL_ERROR, &msg),
    }
}

fn serialize_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("Value is JSON-serialisable")
}

fn handle_resources_read(root: &Path, id: Value, params: Value) -> Response {
    let uri = match params.get("uri").and_then(Value::as_str) {
        Some(u) => u,
        None => {
            return Response::error(id, INVALID_PARAMS, "resources/read params.uri is required");
        }
    };
    match resources::read(root, uri) {
        Ok(content) => Response::ok(
            id,
            json!({
                "contents": [{
                    "uri": content.uri,
                    "mimeType": content.mime_type,
                    "text": content.text,
                }]
            }),
        ),
        Err(tools::ToolError::Failure {
            code: "NOT_FOUND",
            message,
        }) => Response::error(id, METHOD_NOT_FOUND, &message),
        Err(tools::ToolError::Failure { code, message }) => {
            Response::error(id, INTERNAL_ERROR, &format!("{code}: {message}"))
        }
        Err(tools::ToolError::InvalidArgs(msg)) => Response::error(id, INVALID_PARAMS, &msg),
        Err(tools::ToolError::Internal(msg)) => Response::error(id, INTERNAL_ERROR, &msg),
        Err(tools::ToolError::Unknown) => Response::error(id, METHOD_NOT_FOUND, "unknown resource"),
    }
}
