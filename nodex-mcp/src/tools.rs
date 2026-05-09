//! Tool registry. Each tool is a thin adapter over a `nodex-core`
//! function; the protocol layer never touches `nodex-core` directly,
//! so adding a tool is one entry here plus its descriptor below.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

use nodex_core::{
    Config, Error as CoreError, Graph, Kind, builder, lifecycle,
    query::{
        detect, issues, pack,
        recent::{self, RecencyField, RecencyOptions, RecencySince},
        search,
        similar::{self, SimilarityOptions, SimilarityTarget},
        traverse, trust,
    },
    rules,
    scaffold::{ScaffoldSpec, scaffold},
    session::{self, ContinueOptions, LogEventSpec},
};

#[derive(Debug)]
pub enum ToolError {
    Unknown,
    InvalidArgs(String),
    /// A typed nodex-core failure. Echoed to the client as a tool
    /// result with `isError: true` so the LLM sees it in-band rather
    /// than as a JSON-RPC transport error.
    Failure {
        code: &'static str,
        message: String,
    },
    Internal(String),
}

impl From<CoreError> for ToolError {
    fn from(err: CoreError) -> Self {
        Self::Failure {
            code: err.code(),
            message: err.to_string(),
        }
    }
}

/// JSON Schema descriptors for `tools/list`. Keep alphabetical so
/// clients render a stable order.
pub fn list_descriptors() -> Value {
    json!({
        "tools": [
            descriptor_validate(),
            descriptor_query_search(),
            descriptor_query_node(),
            descriptor_query_covered_by(),
            descriptor_query_backlinks(),
            descriptor_query_chain(),
            descriptor_query_orphans(),
            descriptor_query_stale(),
            descriptor_query_issues(),
            descriptor_query_recent(),
            descriptor_query_trust(),
            descriptor_query_low_trust(),
            descriptor_query_similar(),
            descriptor_pack(),
            descriptor_log_event(),
            descriptor_continue_session(),
            descriptor_scaffold(),
            descriptor_lifecycle("nodex_lifecycle_supersede", "Mark a node superseded by another", true),
            descriptor_lifecycle("nodex_lifecycle_archive", "Archive a node", false),
            descriptor_lifecycle("nodex_lifecycle_deprecate", "Mark a node deprecated", false),
            descriptor_lifecycle("nodex_lifecycle_abandon", "Mark a node abandoned", false),
            descriptor_lifecycle("nodex_lifecycle_review", "Refresh the reviewed date on a node", false),
        ]
    })
}

pub fn call(root: &Path, name: &str, args: Value) -> Result<Value, ToolError> {
    match name {
        "nodex_validate" => tool_validate(root),
        "nodex_query_search" => tool_query_search(root, args),
        "nodex_query_node" => tool_query_node(root, args),
        "nodex_query_covered_by" => tool_query_covered_by(root, args),
        "nodex_query_backlinks" => tool_query_backlinks(root, args),
        "nodex_query_chain" => tool_query_chain(root, args),
        "nodex_query_orphans" => tool_query_orphans(root),
        "nodex_query_stale" => tool_query_stale(root),
        "nodex_query_issues" => tool_query_issues(root),
        "nodex_query_recent" => tool_query_recent(root, args),
        "nodex_query_trust" => tool_query_trust(root, args),
        "nodex_query_low_trust" => tool_query_low_trust(root, args),
        "nodex_query_similar" => tool_query_similar(root, args),
        "nodex_pack" => tool_pack(root, args),
        "nodex_log_event" => tool_log_event(root, args),
        "nodex_continue_session" => tool_continue_session(root, args),
        "nodex_scaffold" => tool_scaffold(root, args),
        "nodex_lifecycle_supersede"
        | "nodex_lifecycle_archive"
        | "nodex_lifecycle_deprecate"
        | "nodex_lifecycle_abandon"
        | "nodex_lifecycle_review" => tool_lifecycle(root, name, args),
        _ => Err(ToolError::Unknown),
    }
}

// ─── adapters ───────────────────────────────────────────────────────

fn load_graph(root: &Path) -> Result<(Config, Graph), ToolError> {
    let config = nodex_core::load_project(root)?;
    let result = builder::build(root, &config, false)?;
    Ok((config, result.graph))
}

fn tool_validate(root: &Path) -> Result<Value, ToolError> {
    let (config, graph) = load_graph(root)?;
    let violations = rules::check_all(&graph, &config, root);
    Ok(json!({
        "ok": violations.is_empty(),
        "total": violations.len(),
        "violations": violations,
    }))
}

fn tool_query_search(root: &Path, args: Value) -> Result<Value, ToolError> {
    let keyword = require_string(&args, "keyword")?;
    let statuses = optional_string_list(&args, "status");
    let (_, graph) = load_graph(root)?;
    let items = search::search(&graph, &keyword, statuses.as_deref());
    Ok(items_total(&items))
}

fn tool_query_node(root: &Path, args: Value) -> Result<Value, ToolError> {
    let id = require_string(&args, "id")?;
    let (_, graph) = load_graph(root)?;
    graph.require_node(&id)?;
    let detail = traverse::find_node_detail(&graph, &id).expect("require_node guarantees presence");
    serde_json::to_value(detail).map_err(|e| ToolError::Internal(e.to_string()))
}

fn tool_query_covered_by(root: &Path, args: Value) -> Result<Value, ToolError> {
    let path = require_string(&args, "path")?;
    let (_, graph) = load_graph(root)?;
    Ok(items_total(&traverse::find_covered_by(&graph, &path)))
}

fn tool_query_backlinks(root: &Path, args: Value) -> Result<Value, ToolError> {
    let id = require_string(&args, "id")?;
    let (_, graph) = load_graph(root)?;
    graph.require_node(&id)?;
    Ok(items_total(&traverse::find_backlinks(&graph, &id)))
}

fn tool_query_chain(root: &Path, args: Value) -> Result<Value, ToolError> {
    let id = require_string(&args, "id")?;
    let (_, graph) = load_graph(root)?;
    graph.require_node(&id)?;
    Ok(items_total(&traverse::find_chain(&graph, &id)))
}

fn tool_query_orphans(root: &Path) -> Result<Value, ToolError> {
    let (config, graph) = load_graph(root)?;
    Ok(items_total(&detect::find_orphans(&graph, &config)))
}

fn tool_query_stale(root: &Path) -> Result<Value, ToolError> {
    let (config, graph) = load_graph(root)?;
    Ok(items_total(&detect::find_stale(&graph, &config)))
}

fn tool_query_issues(root: &Path) -> Result<Value, ToolError> {
    let (config, graph) = load_graph(root)?;
    serde_json::to_value(issues::collect_issues(&graph, &config, root))
        .map_err(|e| ToolError::Internal(e.to_string()))
}

fn tool_query_recent(root: &Path, args: Value) -> Result<Value, ToolError> {
    let since = match (
        args.get("since_date").and_then(Value::as_str),
        args.get("since_days").and_then(Value::as_u64),
    ) {
        (Some(_), Some(_)) => {
            return Err(ToolError::InvalidArgs(
                "supply at most one of `since_date` or `since_days`".into(),
            ));
        }
        (Some(s), None) => match chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            Ok(d) => RecencySince::Date(d),
            Err(e) => {
                return Err(ToolError::InvalidArgs(format!(
                    "since_date {s:?} is not YYYY-MM-DD: {e}"
                )));
            }
        },
        (None, Some(n)) => RecencySince::Days(n as u32),
        (None, None) => RecencySince::Days(recent::DEFAULT_SINCE_DAYS),
    };
    let field = match args.get("field").and_then(Value::as_str) {
        None | Some("any") => RecencyField::Any,
        Some("created") => RecencyField::Created,
        Some("updated") => RecencyField::Updated,
        Some("reviewed") => RecencyField::Reviewed,
        Some(other) => {
            return Err(ToolError::InvalidArgs(format!(
                "field {other:?} must be one of created/updated/reviewed/any"
            )));
        }
    };
    let opts = RecencyOptions {
        since,
        kind: optional_string(&args, "kind"),
        field,
        limit: args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .or(Some(recent::DEFAULT_LIMIT)),
    };
    let (_, graph) = load_graph(root)?;
    Ok(items_total(&recent::find_recent(&graph, &opts)))
}

fn tool_query_trust(root: &Path, args: Value) -> Result<Value, ToolError> {
    let id = require_string(&args, "id")?;
    let (config, graph) = load_graph(root)?;
    let report = trust::trust_of(&graph, &config, root, &id)?;
    serde_json::to_value(report).map_err(|e| ToolError::Internal(e.to_string()))
}

fn tool_query_low_trust(root: &Path, args: Value) -> Result<Value, ToolError> {
    let (config, graph) = load_graph(root)?;
    let threshold = args
        .get("threshold")
        .and_then(Value::as_f64)
        .unwrap_or(config.trust.low_trust_threshold);
    let kind = optional_string(&args, "kind");
    let reports = trust::find_low_trust(&graph, &config, root, threshold, kind.as_deref());
    Ok(items_total(&reports))
}

fn tool_query_similar(root: &Path, args: Value) -> Result<Value, ToolError> {
    let id = optional_string(&args, "id");
    let title = optional_string(&args, "title");
    if id.is_some() && title.is_some() {
        return Err(ToolError::InvalidArgs(
            "supply at most one of `id` or `title`".into(),
        ));
    }
    let (config, graph) = load_graph(root)?;
    let opts = SimilarityOptions {
        threshold: args
            .get("threshold")
            .and_then(Value::as_f64)
            .unwrap_or(config.similarity.threshold),
        limit: args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(config.similarity.default_limit),
    };
    let kind = optional_string(&args, "kind");
    let tags = optional_string_list(&args, "tags").unwrap_or_default();
    let parent_dir_str = optional_string(&args, "parent_dir");
    let parent_dir = parent_dir_str.as_deref().map(Path::new);

    let entries = match (id.as_deref(), title.as_deref()) {
        (Some(id), _) => {
            similar::find_similar(&graph, &config, &SimilarityTarget::Node(id), &opts)?
        }
        (None, Some(title)) => {
            let target = SimilarityTarget::Spec {
                title,
                kind: kind.as_deref(),
                tags: &tags,
                parent_dir,
            };
            similar::find_similar(&graph, &config, &target, &opts)?
        }
        (None, None) => {
            return Err(ToolError::InvalidArgs(
                "either `id` or `title` must be supplied".into(),
            ));
        }
    };
    Ok(items_total(&entries))
}

fn tool_pack(root: &Path, args: Value) -> Result<Value, ToolError> {
    let id = require_string(&args, "id")?;
    let token_budget = args
        .get("token_budget")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(pack::DEFAULT_TOKEN_BUDGET);
    let depth = args
        .get("depth")
        .and_then(Value::as_u64)
        .map(|n| n as u32)
        .unwrap_or(pack::DEFAULT_MAX_DEPTH);
    let (config, graph) = load_graph(root)?;
    let bundle = pack::build_pack(&graph, &config, root, &id, token_budget, depth)?;
    serde_json::to_value(bundle).map_err(|e| ToolError::Internal(e.to_string()))
}

fn tool_log_event(root: &Path, args: Value) -> Result<Value, ToolError> {
    let summary = require_string(&args, "summary")?;
    let session_id = optional_string(&args, "session_id");
    let related = optional_string_list(&args, "related").unwrap_or_default();
    let tags = optional_string_list(&args, "tags").unwrap_or_default();
    let config = nodex_core::load_project(root)?;
    let result = session::log_event(
        root,
        &config,
        LogEventSpec {
            session_id,
            summary,
            related,
            tags,
        },
    )?;
    serde_json::to_value(result).map_err(|e| ToolError::Internal(e.to_string()))
}

fn tool_continue_session(root: &Path, args: Value) -> Result<Value, ToolError> {
    let opts = ContinueOptions {
        since_days: args
            .get("since_days")
            .and_then(Value::as_u64)
            .map(|n| n as u32),
        token_budget: args
            .get("token_budget")
            .and_then(Value::as_u64)
            .map(|n| n as usize),
        max_depth: args.get("depth").and_then(Value::as_u64).map(|n| n as u32),
    };
    let config = nodex_core::load_project(root)?;
    let result = session::continue_from_last_session(root, &config, opts)?;
    serde_json::to_value(result).map_err(|e| ToolError::Internal(e.to_string()))
}

fn tool_scaffold(root: &Path, args: Value) -> Result<Value, ToolError> {
    let kind = require_string(&args, "kind")?;
    let title = require_string(&args, "title")?;
    let id = optional_string(&args, "id");
    let path = optional_string(&args, "path").map(PathBuf::from);
    let write = args.get("write").and_then(Value::as_bool).unwrap_or(false);
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);

    let (config, graph) = load_graph(root)?;
    let (result, warnings) = scaffold(
        root,
        ScaffoldSpec {
            kind: Kind::new(kind),
            title,
            id,
            path,
        },
        &graph,
        &config,
        write,
        force,
    )?;
    // Compose the result + warnings into one flat object so the MCP
    // structuredContent surfaces warnings at the top level — same
    // contract the CLI envelope holds, no nested "warnings inside
    // data" trap from the previous shape.
    let mut payload =
        serde_json::to_value(result).map_err(|e| ToolError::Internal(e.to_string()))?;
    if !warnings.is_empty()
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("warnings".into(), serde_json::json!(warnings));
    }
    Ok(payload)
}

fn tool_lifecycle(root: &Path, name: &str, args: Value) -> Result<Value, ToolError> {
    let id = require_string(&args, "id")?;
    let action = match name
        .strip_prefix("nodex_lifecycle_")
        .ok_or(ToolError::Unknown)?
    {
        "supersede" => lifecycle::Action::Supersede {
            successor: require_string(&args, "successor")?,
        },
        "archive" => lifecycle::Action::Archive,
        "deprecate" => lifecycle::Action::Deprecate,
        "abandon" => lifecycle::Action::Abandon,
        "review" => lifecycle::Action::Review,
        _ => return Err(ToolError::Unknown),
    };
    run_lifecycle(root, id, action)
}

fn run_lifecycle(root: &Path, id: String, action: lifecycle::Action) -> Result<Value, ToolError> {
    let action_name = action.name().to_string();
    let (config, graph) = load_graph(root)?;
    let rel_path = graph.require_node(&id)?.path.clone();
    lifecycle::transition(root, &rel_path, action, &config)?;
    Ok(json!({
        "node_id": id,
        "action": action_name,
        "path": rel_path.to_string_lossy(),
    }))
}

// ─── helpers ────────────────────────────────────────────────────────

fn items_total<T: serde::Serialize>(items: &[T]) -> Value {
    json!({
        "items": items,
        "total": items.len(),
    })
}

fn require_string(args: &Value, key: &str) -> Result<String, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidArgs(format!("argument {key:?} is required (string)")))
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn optional_string_list(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key).and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    })
}

// ─── descriptors ────────────────────────────────────────────────────

fn descriptor_validate() -> Value {
    json!({
        "name": "nodex_validate",
        "description": "Run every configured rule against the document graph and return all violations.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

fn descriptor_query_search() -> Value {
    json!({
        "name": "nodex_query_search",
        "description": "Keyword search over node id / title / tags.",
        "inputSchema": {
            "type": "object",
            "required": ["keyword"],
            "properties": {
                "keyword": { "type": "string" },
                "status": { "type": "array", "items": { "type": "string" } }
            }
        }
    })
}

fn descriptor_query_node() -> Value {
    json!({
        "name": "nodex_query_node",
        "description": "Full detail for a node, including outgoing and incoming edges.",
        "inputSchema": {
            "type": "object",
            "required": ["id"],
            "properties": { "id": { "type": "string" } }
        }
    })
}

fn descriptor_query_covered_by() -> Value {
    json!({
        "name": "nodex_query_covered_by",
        "description": "Reverse lookup: docs whose `covers` declares coverage of the given source-code path.",
        "inputSchema": {
            "type": "object",
            "required": ["path"],
            "properties": { "path": { "type": "string" } }
        }
    })
}

fn descriptor_query_backlinks() -> Value {
    json!({
        "name": "nodex_query_backlinks",
        "description": "List nodes that reference the given node.",
        "inputSchema": {
            "type": "object",
            "required": ["id"],
            "properties": { "id": { "type": "string" } }
        }
    })
}

fn descriptor_query_chain() -> Value {
    json!({
        "name": "nodex_query_chain",
        "description": "Walk the supersession chain from the given node.",
        "inputSchema": {
            "type": "object",
            "required": ["id"],
            "properties": { "id": { "type": "string" } }
        }
    })
}

fn descriptor_query_orphans() -> Value {
    json!({
        "name": "nodex_query_orphans",
        "description": "Nodes with no incoming edges that are not exempt.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

fn descriptor_query_stale() -> Value {
    json!({
        "name": "nodex_query_stale",
        "description": "Nodes whose `reviewed` date is older than `detection.stale_days`.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

fn descriptor_query_issues() -> Value {
    json!({
        "name": "nodex_query_issues",
        "description": "Unified report of orphans, stale docs, unresolved edges, and rule violations.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

fn descriptor_query_recent() -> Value {
    json!({
        "name": "nodex_query_recent",
        "description": format!(
            "List documents whose date field falls inside a recent window. \
             Defaults to the last {} days, newest-first, picking the most \
             recent of created/updated/reviewed per node.",
            recent::DEFAULT_SINCE_DAYS,
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "since_date": { "type": "string", "format": "date", "description": "YYYY-MM-DD; mutually exclusive with since_days" },
                "since_days": { "type": "integer", "minimum": 0, "default": recent::DEFAULT_SINCE_DAYS },
                "kind": { "type": "string" },
                "field": { "type": "string", "enum": ["created", "updated", "reviewed", "any"], "default": "any" },
                "limit": { "type": "integer", "minimum": 1, "default": recent::DEFAULT_LIMIT }
            }
        }
    })
}

fn descriptor_query_trust() -> Value {
    json!({
        "name": "nodex_query_trust",
        "description": "Composite reliability score (0-1) for a node. Returns the score plus per-component breakdown (status, freshness, drift, backlinks) so the agent can re-rank with its own weights.",
        "inputSchema": {
            "type": "object",
            "required": ["id"],
            "properties": { "id": { "type": "string" } }
        }
    })
}

fn descriptor_query_low_trust() -> Value {
    json!({
        "name": "nodex_query_low_trust",
        "description": "List nodes whose composite trust score is below the given threshold (default: config.trust.low_trust_threshold, falling back to 0.5). Useful for memory-quality reviews — surface docs that should be reviewed, superseded, or archived.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "threshold": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "kind": { "type": "string" }
            }
        }
    })
}

fn descriptor_query_similar() -> Value {
    json!({
        "name": "nodex_query_similar",
        "description": "Vector-free similarity search. Supply `id` to find docs similar to an existing node, or `title` (with optional kind/tags/parent_dir) to probe before scaffolding so duplicates surface as candidates to supersede.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "title": { "type": "string" },
                "kind": { "type": "string" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "parent_dir": { "type": "string" },
                "threshold": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "limit": { "type": "integer", "minimum": 1, "description": "Defaults to config.similarity.default_limit (10)." }
            }
        }
    })
}

fn descriptor_pack() -> Value {
    json!({
        "name": "nodex_pack",
        "description": "Build a token-budgeted context pack rooted at the given node, walking supersession + backlinks + references in priority order.",
        "inputSchema": {
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string" },
                "token_budget": {
                    "type": "integer",
                    "minimum": 1,
                    "default": pack::DEFAULT_TOKEN_BUDGET
                },
                "depth": {
                    "type": "integer",
                    "minimum": 0,
                    "default": pack::DEFAULT_MAX_DEPTH
                }
            }
        }
    })
}

fn descriptor_log_event() -> Value {
    json!({
        "name": "nodex_log_event",
        "description": "Append an event to the current (or a named) session log. The session document grows by one line per event; rollover into a successor session happens automatically when `session.max_events_per_session` is reached. Requires `session.log_kind` to be configured.",
        "inputSchema": {
            "type": "object",
            "required": ["summary"],
            "properties": {
                "summary": { "type": "string", "description": "One-line narrative — what just happened" },
                "session_id": { "type": "string", "description": "Append to this existing session; omit to create a new auto-stamped session" },
                "related": { "type": "array", "items": { "type": "string" }, "description": "Doc ids touched by this event; merged into the session's related list" },
                "tags": { "type": "array", "items": { "type": "string" } }
            }
        }
    })
}

fn descriptor_continue_session() -> Value {
    json!({
        "name": "nodex_continue_session",
        "description": "Resume context from the most recent session log: locates the newest session inside the configured window, returns its metadata + a token-budgeted pack rooted at it. Returns null when no session exists in window.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "since_days": { "type": "integer", "minimum": 1, "description": "Override config.session.default_continue_days" },
                "token_budget": { "type": "integer", "minimum": 1 },
                "depth": { "type": "integer", "minimum": 0 }
            }
        }
    })
}

fn descriptor_scaffold() -> Value {
    json!({
        "name": "nodex_scaffold",
        "description": "Create a new document with valid frontmatter. Set write=true to persist.",
        "inputSchema": {
            "type": "object",
            "required": ["kind", "title"],
            "properties": {
                "kind": { "type": "string" },
                "title": { "type": "string" },
                "id": { "type": "string" },
                "path": { "type": "string" },
                "write": { "type": "boolean", "default": false },
                "force": { "type": "boolean", "default": false }
            }
        }
    })
}

fn descriptor_lifecycle(
    name: &'static str,
    description: &'static str,
    needs_successor: bool,
) -> Value {
    let mut props = json!({ "id": { "type": "string" } });
    let mut required = json!(["id"]);
    if needs_successor {
        props.as_object_mut().unwrap().insert(
            "successor".to_string(),
            json!({ "type": "string", "description": "Successor node id" }),
        );
        required.as_array_mut().unwrap().push(json!("successor"));
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "required": required,
            "properties": props
        }
    })
}
