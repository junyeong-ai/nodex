//! End-to-end smoke tests for `nodex-mcp`. Each spawns the compiled
//! binary, drives it through stdio with newline-delimited JSON-RPC,
//! and asserts on the structured response.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn project() -> TempDir {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
"#,
    )
    .unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(
        docs.join("a.md"),
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    )
    .unwrap();
    tmp
}

/// Project pre-configured for the AI Memory Layer tools (session log
/// and continue). The `session` kind is whitelisted and `[session]`
/// is opted-in so `nodex_log_event` and `nodex_continue_session`
/// work out of the box.
fn memory_project() -> TempDir {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md", "_sessions/**/*.md"]

[kinds]
allowed = ["generic", "session"]

[session]
log_kind = "session"
session_dir = "_sessions"
"#,
    )
    .unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(
        docs.join("a.md"),
        "---\nid: doc-a\ntitle: Auth Retry Policy\nkind: generic\nstatus: active\n---\n# A\n",
    )
    .unwrap();
    tmp
}

fn run_session(root: &std::path::Path, requests: &[&str]) -> Vec<serde_json::Value> {
    let bin = env!("CARGO_BIN_EXE_nodex-mcp");
    let mut child = Command::new(bin)
        .arg("--root")
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn nodex-mcp");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for req in requests {
            writeln!(stdin, "{req}").unwrap();
        }
    }

    let output = child.wait_with_output().expect("wait");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("response is JSON"))
        .collect()
}

#[test]
fn initialize_returns_server_info() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#],
    );
    assert_eq!(responses.len(), 1);
    let init = &responses[0];
    assert_eq!(
        init.pointer("/result/serverInfo/name")
            .and_then(|v| v.as_str()),
        Some("nodex-mcp")
    );
    assert!(init.pointer("/result/protocolVersion").is_some());
}

#[test]
fn tools_list_includes_query_search() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        ],
    );
    assert_eq!(responses.len(), 2);
    let tools = responses[1].pointer("/result/tools").expect("tools array");
    let names: Vec<&str> = tools
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
        .collect();
    assert!(names.contains(&"nodex_query_search"));
    assert!(names.contains(&"nodex_lifecycle_supersede"));
    assert!(names.contains(&"nodex_validate"));
}

#[test]
fn tools_call_query_node_returns_structured_content() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_query_node","arguments":{"id":"doc-a"}}}"#,
        ],
    );
    let call = &responses[1];
    assert_eq!(
        call.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(false)
    );
    let structured = call
        .pointer("/result/structuredContent/node")
        .expect("node");
    assert_eq!(structured.get("id").and_then(|v| v.as_str()), Some("doc-a"));
}

#[test]
fn missing_node_surfaces_as_in_band_tool_error() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_query_node","arguments":{"id":"nope"}}}"#,
        ],
    );
    let call = &responses[1];
    assert_eq!(
        call.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        call.pointer("/result/structuredContent/error/code")
            .and_then(|v| v.as_str()),
        Some("NOT_FOUND")
    );
}

#[test]
fn notifications_produce_no_response() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            // `notifications/initialized` carries no `id` and must not be answered.
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#,
        ],
    );
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0].pointer("/id").and_then(|v| v.as_u64()),
        Some(1)
    );
}

#[test]
fn initialize_advertises_current_protocol_version_and_instructions() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#],
    );
    let init = &responses[0];
    assert_eq!(
        init.pointer("/result/protocolVersion")
            .and_then(|v| v.as_str()),
        Some("2025-11-25"),
        "must advertise the current MCP spec version"
    );
    assert!(
        init.pointer("/result/instructions")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "instructions field must be populated"
    );
}

#[test]
fn nodex_validate_returns_ok_on_clean_project() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_validate","arguments":{}}}"#,
        ],
    );
    let call = &responses[1];
    assert_eq!(
        call.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(false)
    );
    let ok = call
        .pointer("/result/structuredContent/ok")
        .and_then(|v| v.as_bool());
    assert_eq!(ok, Some(true), "clean project must validate as ok");
}

#[test]
fn nodex_pack_returns_seed_in_included() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_pack","arguments":{"id":"doc-a","token_budget":2000,"depth":1}}}"#,
        ],
    );
    let call = &responses[1];
    assert_eq!(
        call.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        call.pointer("/result/structuredContent/seed")
            .and_then(|v| v.as_str()),
        Some("doc-a")
    );
    let included = call
        .pointer("/result/structuredContent/included")
        .and_then(|v| v.as_array())
        .expect("included array");
    assert!(
        included
            .iter()
            .any(|n| n.get("id").and_then(|v| v.as_str()) == Some("doc-a"))
    );
}

#[test]
fn nodex_query_covered_by_finds_declared_coverage() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
"#,
    )
    .unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/auth.rs"), "// stub").unwrap();
    std::fs::write(
        docs.join("a.md"),
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\ncovers: src/auth.rs\n---\n# A\n",
    )
    .unwrap();

    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_query_covered_by","arguments":{"path":"src/auth.rs"}}}"#,
        ],
    );
    let call = &responses[1];
    assert_eq!(
        call.pointer("/result/structuredContent/total")
            .and_then(|v| v.as_u64()),
        Some(1),
        "doc-a covers src/auth.rs and must surface"
    );
}

#[test]
fn nodex_log_event_creates_session_then_appends() {
    let tmp = memory_project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_log_event","arguments":{"summary":"first","session_id":"session-mcp-test"}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nodex_log_event","arguments":{"summary":"second","session_id":"session-mcp-test","related":["doc-a"]}}}"#,
        ],
    );
    let first = &responses[1];
    assert_eq!(
        first.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        first
            .pointer("/result/structuredContent/event_index")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    // Outcome enum carries a discriminator field — verify the tagged
    // shape so a future serde rename can't silently flip it.
    assert_eq!(
        first
            .pointer("/result/structuredContent/outcome/kind")
            .and_then(|v| v.as_str()),
        Some("created")
    );
    let second = &responses[2];
    assert_eq!(
        second
            .pointer("/result/structuredContent/event_index")
            .and_then(|v| v.as_u64()),
        Some(2)
    );
    assert_eq!(
        second
            .pointer("/result/structuredContent/outcome/kind")
            .and_then(|v| v.as_str()),
        Some("appended")
    );
    let body = std::fs::read_to_string(tmp.path().join("_sessions/session-mcp-test.md")).unwrap();
    assert!(body.contains("event_count: \"2\""));
    assert!(body.contains("related:\n  - \"doc-a\""));
    assert!(body.contains("— first"));
    assert!(body.contains("— second"));
}

#[test]
fn nodex_continue_session_returns_pack_for_last_session() {
    let tmp = memory_project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_log_event","arguments":{"summary":"started auth work","session_id":"session-cont","related":["doc-a"]}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nodex_continue_session","arguments":{}}}"#,
        ],
    );
    let cont = &responses[2];
    assert_eq!(
        cont.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        cont.pointer("/result/structuredContent/id")
            .and_then(|v| v.as_str()),
        Some("session-cont")
    );
    assert_eq!(
        cont.pointer("/result/structuredContent/last_event_summary")
            .and_then(|v| v.as_str()),
        Some("started auth work")
    );
    let included = cont
        .pointer("/result/structuredContent/pack/included")
        .and_then(|v| v.as_array())
        .expect("pack.included");
    let ids: Vec<&str> = included
        .iter()
        .filter_map(|n| n.get("id").and_then(|v| v.as_str()))
        .collect();
    assert!(ids.contains(&"session-cont"));
    assert!(ids.contains(&"doc-a"));
}

#[test]
fn nodex_query_low_trust_lists_below_threshold() {
    // Two docs: an active fresh one (high trust) and an archived one
    // (status=0 → composite drops well below the 0.5 default).
    // `low_trust` must include the archived doc and exclude the fresh.
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
"#,
    )
    .unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let today = chrono::Local::now().date_naive();
    std::fs::write(
        docs.join("fresh.md"),
        format!(
            "---\nid: doc-fresh\ntitle: Fresh\nkind: generic\nstatus: active\nreviewed: {today}\n---\n# Fresh\n"
        ),
    )
    .unwrap();
    std::fs::write(
        docs.join("dead.md"),
        "---\nid: doc-dead\ntitle: Dead\nkind: generic\nstatus: archived\n---\n# Dead\n",
    )
    .unwrap();

    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_query_low_trust","arguments":{}}}"#,
        ],
    );
    let call = &responses[1];
    assert_eq!(
        call.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(false)
    );
    let items = call
        .pointer("/result/structuredContent/items")
        .and_then(|v| v.as_array())
        .expect("items");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(|v| v.as_str()))
        .collect();
    assert!(
        ids.contains(&"doc-dead"),
        "archived doc must surface as low-trust; got {ids:?}"
    );
    assert!(
        !ids.contains(&"doc-fresh"),
        "fresh active doc must not surface; got {ids:?}"
    );
}

#[test]
fn nodex_query_trust_returns_score_and_components() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_query_trust","arguments":{"id":"doc-a"}}}"#,
        ],
    );
    let res = &responses[1];
    assert_eq!(
        res.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(false)
    );
    let score = res
        .pointer("/result/structuredContent/score")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((0.0..=1.0).contains(&score));
    assert!(
        res.pointer("/result/structuredContent/components/status")
            .is_some()
    );
}

#[test]
fn nodex_query_similar_with_spec_finds_duplicate_candidate() {
    let tmp = project();
    std::fs::write(
        tmp.path().join("docs/auth.md"),
        "---\nid: doc-auth\ntitle: Auth Retry Policy\nkind: generic\nstatus: active\n---\n# Auth\n",
    )
    .unwrap();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_query_similar","arguments":{"title":"Auth retry policy v2","kind":"generic"}}}"#,
        ],
    );
    let res = &responses[1];
    let total = res
        .pointer("/result/structuredContent/total")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(total >= 1, "should find at least one similar doc");
    let first = res
        .pointer("/result/structuredContent/items/0/id")
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(first, "doc-auth");
}

#[test]
fn nodex_query_recent_filters_by_kind() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[kinds]
allowed = ["generic", "guide"]
"#,
    )
    .unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let today = chrono::Local::now().date_naive();
    std::fs::write(
        docs.join("a.md"),
        format!(
            "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nupdated: {today}\n---\n# A\n"
        ),
    )
    .unwrap();
    std::fs::write(
        docs.join("g.md"),
        format!(
            "---\nid: doc-g\ntitle: G\nkind: guide\nstatus: active\nupdated: {today}\n---\n# G\n"
        ),
    )
    .unwrap();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_query_recent","arguments":{"kind":"guide","field":"updated"}}}"#,
        ],
    );
    let items = responses[1]
        .pointer("/result/structuredContent/items")
        .and_then(|v| v.as_array())
        .expect("items");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(ids, vec!["doc-g"]);
}

#[test]
fn resources_list_includes_summary_issues_recent() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"resources/list","params":{}}"#,
        ],
    );
    let resources = responses[1]
        .pointer("/result/resources")
        .and_then(|v| v.as_array())
        .expect("resources array");
    let uris: Vec<&str> = resources
        .iter()
        .filter_map(|r| r.get("uri").and_then(|v| v.as_str()))
        .collect();
    assert!(uris.contains(&"nodex://graph/summary"));
    assert!(uris.contains(&"nodex://graph/issues"));
    assert!(uris.contains(&"nodex://graph/recent"));
}

#[test]
fn resources_read_summary_returns_node_count() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"nodex://graph/summary"}}"#,
        ],
    );
    let contents = responses[1]
        .pointer("/result/contents/0")
        .expect("contents[0]");
    assert_eq!(
        contents.get("uri").and_then(|v| v.as_str()),
        Some("nodex://graph/summary")
    );
    let text = contents.get("text").and_then(|v| v.as_str()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(text).expect("payload is JSON");
    assert!(parsed.get("node_count").is_some());
    assert!(parsed.get("by_kind").is_some());
}

#[test]
fn resources_read_unknown_uri_errors() {
    let tmp = project();
    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"nodex://nope"}}"#,
        ],
    );
    let err = &responses[1].pointer("/error").expect("error envelope");
    assert!(
        err.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .contains("nodex://nope")
    );
}

#[test]
fn nodex_lifecycle_supersede_writes_status_change() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
"#,
    )
    .unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(
        docs.join("old.md"),
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    )
    .unwrap();
    std::fs::write(
        docs.join("new.md"),
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    )
    .unwrap();

    let responses = run_session(
        tmp.path(),
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nodex_lifecycle_supersede","arguments":{"id":"doc-old","successor":"doc-new"}}}"#,
        ],
    );
    let call = &responses[1];
    assert_eq!(
        call.pointer("/result/isError").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        call.pointer("/result/structuredContent/action")
            .and_then(|v| v.as_str()),
        Some("supersede")
    );
    let written = std::fs::read_to_string(docs.join("old.md")).unwrap();
    assert!(written.contains(r#"status: "superseded""#));
    assert!(written.contains(r#"superseded_by: "doc-new""#));
}
