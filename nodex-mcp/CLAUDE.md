# nodex-mcp

Stdio MCP server. Speaks newline-delimited JSON-RPC 2.0 (MCP spec
2025-11-25) and adapts every `nodex-core` surface to a tool or resource.
The protocol layer never reaches into `nodex-core` directly — adapter
functions in `tools.rs` are the only seam.

## Module Map

- `main.rs` — stdio reader loop. Parses one JSON-RPC request per line,
  dispatches via [`protocol::dispatch`], writes the response. Holds the
  `--root` clap parser and nothing else.
- `protocol.rs` — JSON-RPC 2.0 envelope (`Request` / `Response` /
  `RpcError`), method routing, error-code constants. `dispatch` returns
  `Option<Response>` so notifications (id is null) yield `None` per
  spec. `tools/call` and `resources/read` results are wrapped to MCP's
  `{ content: [...], isError, structuredContent }` shape here, not in
  the adapter.
- `tools.rs` — every MCP tool. Each tool is one descriptor in
  [`list_descriptors`] + one adapter `fn tool_<name>(root, args) ->
  Result<Value, ToolError>`. The dispatch in [`call`] is alphabetical;
  the descriptor list mirrors that order so `tools/list` is stable.
- `resources.rs` — read-only ambient context. Three URIs
  (`nodex://graph/{summary,issues,recent}`) are advertised by
  [`list_descriptors`] and served by [`read`]. Each one runs the build,
  composes a small JSON payload, and returns it as `application/json`
  text — a client attaches the resource and reads the body as data.

## Adding a Tool

1. Define a private adapter `fn tool_<name>(root: &Path, args: Value)
   -> Result<Value, ToolError>` in `tools.rs`. Convert clap-style
   typed args from JSON via the existing helpers (`require_string`,
   `optional_string`, `optional_string_list`).
2. Add a descriptor `fn descriptor_<name>() -> Value` returning the
   JSON Schema. Document defaults that come from config in the
   `description` rather than the schema's `default` (so a config
   change can't make the schema lie). When numeric defaults *are*
   schema-level (e.g. `nodex_query_recent`'s `since_days`), reference
   the same `pub const` the runtime fallback uses — the const is the
   single source of truth.
3. Wire both: add the descriptor to [`list_descriptors`] (alphabetical)
   and the adapter to [`call`]'s match arm.
4. Add an integration test in `tests/server.rs` that drives the
   compiled binary through stdio. Assert on `structuredContent` —
   not the human-readable `content[0].text` — so a serde rename or a
   tag attribute change cannot pass silently.

## Adding a Resource

1. Reserve a URI constant at the top of `resources.rs`.
2. Add a static `descriptor` to [`list_descriptors`].
3. Extend the `match uri` in [`read`] to compose the JSON payload.
4. Add a `resources_read_<name>` test that asserts the parsed
   payload has the expected shape (not just an arbitrary string).

## Error Taxonomy

`ToolError` maps cleanly onto JSON-RPC + MCP error semantics —
`protocol::handle_tools_call` is the only place this conversion lives:

| `ToolError`        | Wire shape                                                                |
|--------------------|---------------------------------------------------------------------------|
| `Unknown`          | JSON-RPC `-32601` (method not found)                                      |
| `InvalidArgs(msg)` | JSON-RPC `-32602` (invalid params)                                        |
| `Failure { code, message }` | `result.isError = true`, `structuredContent.error.{code,message}` (in-band so the LLM sees it) |
| `Internal(msg)`    | JSON-RPC `-32603` (internal error)                                        |

`Failure` is reserved for typed `nodex_core::Error` outcomes — every
`From<CoreError> for ToolError` route lands here so the stable
`Error::code()` string surfaces unchanged at the MCP boundary. Anything
else should be `Internal` (genuine bugs) or `InvalidArgs` (caller
contract violation), never `Failure`.
