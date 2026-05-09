//! CLI contract tests.
//!
//! Each test spins up a tempdir and runs the `nodex` binary against
//! it. The assertions target contract surfaces — JSON envelope shape,
//! exit codes, error classification — so future refactors that break
//! the advertised behaviour fail CI loudly.
//!
//! Whole-project flow tests (init → build → query → check → scaffold
//! → lifecycle) live below; focused format tests live above. Keep
//! each test self-contained: no shared mutable state, no ordering.
//!
//! These tests intentionally do **not** check log text, error prose,
//! or timing — only the stable contract each command promises.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────

fn nodex(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("nodex").expect("nodex binary in cargo target");
    cmd.arg("-C").arg(dir);
    cmd
}

/// Run the command and parse stdout as JSON, asserting the envelope
/// wrapper invariants. Returns the parsed `data` field on success.
fn run_json(cmd: &mut Command) -> Value {
    run_envelope(cmd)
        .get("data")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Same as [`run_json`] but returns the full envelope so callers can
/// inspect `ok` / `warnings` / `error` directly. Use this whenever the
/// assertion is about the envelope itself, not the `data` payload.
fn run_envelope(cmd: &mut Command) -> Value {
    let output = cmd.output().expect("command ran");
    assert!(
        output.status.success(),
        "command failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("stdout is parseable JSON");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(true)));
    parsed
}

fn scratch() -> TempDir {
    tempfile::tempdir().expect("create tempdir")
}

fn write_doc(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

fn init_project(root: &std::path::Path) {
    nodex(root).arg("init").assert().success();
}

/// Project tuned for the AI Memory Layer features: includes the
/// `session` kind in `kinds.allowed` and opts in to `[session]` so
/// `log` / `continue` / similarity / trust tests have a writeable
/// session log out of the box.
fn init_memory_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md", "_sessions/**/*.md"]

[kinds]
allowed = ["generic", "guide", "readme", "session"]

[statuses]
allowed = ["active", "superseded", "archived", "deprecated", "abandoned"]
terminal = ["superseded", "archived", "deprecated", "abandoned"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"

[session]
log_kind = "session"
session_dir = "_sessions"
max_events_per_session = 200
default_continue_days = 1
"#,
    )
    .unwrap();
}

// ─── init ───────────────────────────────────────────────────────────

#[test]
fn init_creates_config_and_writes_path_to_envelope() {
    let tmp = scratch();
    let data = run_json(nodex(tmp.path()).arg("init"));
    let path = data
        .get("path")
        .and_then(Value::as_str)
        .expect("data.path is a string");
    assert!(PathBuf::from(path).exists(), "nodex.toml was written");
    assert_eq!(
        PathBuf::from(path).file_name().unwrap().to_str().unwrap(),
        "nodex.toml"
    );
}

#[test]
fn init_twice_fails_with_nonzero_exit() {
    let tmp = scratch();
    nodex(tmp.path()).arg("init").assert().success();
    nodex(tmp.path()).arg("init").assert().failure();
}

// ─── build ──────────────────────────────────────────────────────────

#[test]
fn build_empty_scope_returns_zero_counts() {
    let tmp = scratch();
    init_project(tmp.path());
    let data = run_json(nodex(tmp.path()).arg("build"));
    assert_eq!(data.get("nodes").and_then(Value::as_u64), Some(0));
    assert_eq!(data.get("edges").and_then(Value::as_u64), Some(0));
    assert!(data.get("duration_ms").is_some());
}

#[test]
fn build_indexes_markdown_files() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/one.md",
        "---\nid: note-one\ntitle: One\nkind: generic\nstatus: active\n---\n# One\n",
    );
    write_doc(
        tmp.path(),
        "docs/two.md",
        "---\nid: note-two\ntitle: Two\nkind: generic\nstatus: active\n---\n[one](one.md)\n",
    );
    let data = run_json(nodex(tmp.path()).arg("build"));
    assert_eq!(data.get("nodes").and_then(Value::as_u64), Some(2));
    // Exactly one resolved edge (two → one).
    assert_eq!(data.get("edges").and_then(Value::as_u64), Some(1));
}

// ─── check ──────────────────────────────────────────────────────────

#[test]
fn check_on_empty_graph_exits_success() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path()).arg("check").assert().success();
}

#[test]
fn check_exits_1_when_violations_present() {
    let tmp = scratch();
    init_project(tmp.path());
    // Default init template ships a cross_field rule that requires
    // `superseded_by` whenever status is superseded. Write a doc that
    // violates it to exercise the full check → exit-1 pipeline.
    write_doc(
        tmp.path(),
        "docs/bad.md",
        "---\nid: bad\ntitle: Bad\nkind: generic\nstatus: superseded\n---\nbody\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let assertion = nodex(tmp.path()).arg("check").assert().failure();
    let code = assertion.get_output().status.code().unwrap_or(-1);
    assert_eq!(code, 1, "violations should exit 1, not 2");
}

// ─── query ──────────────────────────────────────────────────────────

#[test]
fn query_orphans_returns_items_total_shape() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "orphans"]));
    assert!(data.get("items").is_some(), "items key present");
    assert!(data.get("total").is_some(), "total key present");
}

#[test]
fn detection_orphan_ok_kinds_excludes_listed_kinds() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[kinds]
allowed = ["generic", "skill"]

[[identity.kind_rules]]
glob = "docs/skill/**"
kind = "skill"

[[identity.kind_rules]]
glob = "docs/**"
kind = "generic"

[detection]
orphan_grace_days = 0
orphan_ok_kinds = ["skill"]
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/skill/entry.md",
        "---\nid: skill-entry\ntitle: Entry\nkind: skill\nstatus: active\n---\n# Entry\n",
    );
    write_doc(
        tmp.path(),
        "docs/regular.md",
        "---\nid: regular\ntitle: Regular\nkind: generic\nstatus: active\n---\n# Regular\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "orphans"]));
    let total = data.get("total").and_then(Value::as_u64).unwrap_or(99);
    assert_eq!(
        total, 1,
        "skill kind exempted; only generic counts as orphan"
    );
    let items = data
        .get("items")
        .and_then(Value::as_array)
        .expect("items array");
    assert!(
        items
            .iter()
            .all(|n| n.get("kind").and_then(Value::as_str) != Some("skill")),
        "no skill nodes in orphan list"
    );
    assert!(
        items
            .iter()
            .any(|n| n.get("id").and_then(Value::as_str) == Some("regular")),
        "non-exempt orphan still surfaces"
    );
}

#[test]
fn detection_orphan_ok_kinds_default_is_empty_no_exemption() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[kinds]
allowed = ["generic", "skill"]

[[identity.kind_rules]]
glob = "docs/skill/**"
kind = "skill"

[[identity.kind_rules]]
glob = "docs/**"
kind = "generic"

[detection]
orphan_grace_days = 0
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/skill/entry.md",
        "---\nid: skill-entry\ntitle: Entry\nkind: skill\nstatus: active\n---\n# Entry\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "orphans"]));
    let total = data.get("total").and_then(Value::as_u64).unwrap_or(0);
    assert_eq!(
        total, 1,
        "no exemption — skill node IS an orphan by default"
    );
}

#[test]
fn detection_orphan_ok_kinds_typo_rejected_at_load() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[kinds]
allowed = ["generic", "skill"]

[detection]
orphan_ok_kinds = ["skll"]
"#,
    )
    .unwrap();
    let assert = nodex(tmp.path())
        .args(["query", "orphans"])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout).expect("error envelope");
    let msg = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("orphan_ok_kinds") && msg.contains("\"skll\""),
        "expected typo to surface at load, got: {msg}"
    );
}

#[test]
fn query_issues_returns_summary_shape() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "issues"]));
    let summary = data.get("summary").expect("summary key present");
    assert!(summary.get("total").is_some());
    assert!(summary.get("by_category").is_some());
}

// ─── scaffold ───────────────────────────────────────────────────────

#[test]
fn scaffold_dry_run_does_not_write_and_returns_plan() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Hello"])
            .args(["--path", "misc/hello.md", "--dry-run"]),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(false));
    assert!(data.get("id").and_then(Value::as_str).is_some());
    assert!(!tmp.path().join("misc/hello.md").exists());
}

#[test]
fn scaffold_writes_file_on_non_dry_run() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Written"])
            .args(["--path", "docs/written.md"]),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    assert!(tmp.path().join("docs/written.md").exists());
    // Frontmatter round-trips through YAML parser (no Debug-escape drift).
    let content = fs::read_to_string(tmp.path().join("docs/written.md")).unwrap();
    assert!(content.contains("title: \"Written\""));
}

#[test]
fn scaffold_respects_global_schema_type_default() {
    // A top-level `[schema] types = { priority = "integer" }` (no
    // per-kind override) flows through scaffold's defaults so the
    // generated frontmatter passes the same `FieldTypeRule` it would
    // be checked against.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[[identity.kind_rules]]
glob = "docs/**"
kind = "guide"

[[identity.id_rules]]
kind = "guide"
template = "guide-{stem}"

[schema]
required = ["id", "title", "kind", "status", "priority"]
types = { priority = "integer" }
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "guide", "--title", "Test"])
            .args(["--path", "docs/test.md"]),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    let content = fs::read_to_string(tmp.path().join("docs/test.md")).unwrap();
    assert!(
        content.contains("priority: 0"),
        "expected integer default, got:\n{content}"
    );

    // Subsequent build + check should not flag the scaffolded file.
    nodex(tmp.path()).arg("build").assert().success();
    let check_data = run_json(nodex(tmp.path()).arg("check"));
    assert_eq!(
        check_data.get("has_errors").and_then(Value::as_bool),
        Some(false),
        "check should find no errors; got: {check_data}"
    );
}

#[test]
fn scaffold_rejects_existing_without_force() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/exists.md", "existing content");
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Clash"])
        .args(["--path", "docs/exists.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("ALREADY_EXISTS"),
        "existing scaffold target classified as ALREADY_EXISTS, not CONFIG_ERROR"
    );
}

#[test]
fn scaffold_with_force_overwrites() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/ow.md", "existing content");
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Overwritten"])
        .args(["--path", "docs/ow.md", "--force"])
        .assert()
        .success();
    let content = fs::read_to_string(tmp.path().join("docs/ow.md")).unwrap();
    assert!(content.contains("title: \"Overwritten\""));
}

#[test]
fn scaffold_rejects_non_md_extension() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/wrong.txt"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("JSON");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(false)));
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

// ─── error-code classification ──────────────────────────────────────

#[test]
fn superseded_by_surfaces_as_incoming_supersedes_edge() {
    let tmp = scratch();
    init_project(tmp.path());
    // doc-old declares superseded_by only — no `supersedes` on doc-new.
    write_doc(
        tmp.path(),
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n",
    );
    write_doc(
        tmp.path(),
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // Canonical supersedes edge direction is newer → older. Deriving it
    // from `superseded_by` on doc-old means doc-new now has an
    // *outgoing* supersedes edge and doc-old has an *incoming* one.
    //
    //   query backlinks doc-old → should include doc-new
    //   query node doc-new.outgoing → should include doc-old
    let data = run_json(nodex(tmp.path()).args(["query", "backlinks", "doc-old"]));
    let items = data.get("items").and_then(Value::as_array).unwrap();
    let relations: Vec<&str> = items
        .iter()
        .filter_map(|v| v.get("relation").and_then(Value::as_str))
        .collect();
    assert!(
        relations.contains(&"supersedes"),
        "backlinks of doc-old should include a supersedes edge, got {relations:?}"
    );

    // chain still walks the supersession graph using the same edges.
    let data = run_json(nodex(tmp.path()).args(["query", "chain", "doc-old"]));
    let total = data.get("total").and_then(Value::as_u64).unwrap_or(0);
    assert_eq!(total, 2, "chain length must be 2 (doc-old → doc-new)");
}

#[test]
fn duplicate_supersedes_and_superseded_by_dedup_to_single_edge() {
    let tmp = scratch();
    init_project(tmp.path());
    // Both sides declare the supersession — scanner must dedupe.
    write_doc(
        tmp.path(),
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n",
    );
    write_doc(
        tmp.path(),
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\nsupersedes: [doc-old]\n---\n# New\n",
    );
    let data = run_json(nodex(tmp.path()).arg("build"));
    // 2 nodes, exactly 1 supersedes edge (not 2).
    assert_eq!(data.get("edges").and_then(Value::as_u64), Some(1));
}

#[test]
fn output_dir_is_auto_excluded_from_scope() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/real.md",
        "---\nid: real\ntitle: Real\nkind: generic\nstatus: active\n---\n# Real\n",
    );
    // First build creates _index/GRAPH.md via report.
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path()).arg("report").assert().success();
    // Rebuild and verify _index/GRAPH.md wasn't indexed as a user doc.
    let data = run_json(nodex(tmp.path()).arg("build").arg("--full"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "_index/GRAPH.md must not be indexed"
    );
    // migrate must not offer to touch the generated GRAPH.md either.
    let migrate = run_json(nodex(tmp.path()).arg("migrate"));
    let changes = migrate
        .get("changes")
        .and_then(Value::as_array)
        .expect("changes array");
    for change in changes {
        let path = change.get("path").and_then(Value::as_str).unwrap_or("");
        assert!(
            !path.starts_with("_index/"),
            "migrate should not target _index/* but saw {path}"
        );
    }
}

#[test]
fn migrate_fills_required_fields_under_strict_schema() {
    // `migrate --apply` walks `required_for(kind)` + `cross_field_for(kind)`
    // through scaffold's shared frontmatter generator, so the injected
    // frontmatter passes the same schema rules the document will be
    // checked against.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[schema]
required = ["id", "title", "kind", "status", "decision_date"]
types = { decision_date = "date" }
"#,
    )
    .unwrap();
    write_doc(tmp.path(), "bare.md", "# Bare Doc\nBody.\n");

    nodex(tmp.path())
        .args(["migrate", "--apply"])
        .assert()
        .success();

    let content = fs::read_to_string(tmp.path().join("bare.md")).unwrap();
    assert!(
        content.contains("decision_date:"),
        "required field must be written; got:\n{content}"
    );

    // Build + check should not flag any violation on the migrated doc.
    nodex(tmp.path()).arg("build").assert().success();
    let check = run_json(nodex(tmp.path()).arg("check"));
    assert_eq!(
        check.get("has_errors").and_then(Value::as_bool),
        Some(false),
        "migrated doc should pass check; got: {check}"
    );
}

#[test]
fn malformed_config_emits_config_error_code_and_exit_2() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "this is not toml = [unclosed",
    )
    .unwrap();
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let parsed: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("JSON envelope");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(false)));
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn corrupt_graph_json_emits_parse_error_code() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    // Corrupt the graph.json the scanner wrote.
    let graph_path = tmp.path().join("_index/graph.json");
    fs::write(&graph_path, b"not valid json").unwrap();
    let output = nodex(tmp.path())
        .args(["query", "orphans"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PARSE_ERROR"),
        "corrupt graph.json must classify as PARSE_ERROR"
    );
}

#[test]
fn lifecycle_supersede_writes_minimal_diff() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: active\n# author note\n---\n# Old\n",
    );
    write_doc(
        tmp.path(),
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["lifecycle", "supersede", "doc-old", "--to", "doc-new"])
        .assert()
        .success();
    let content = fs::read_to_string(tmp.path().join("docs/old.md")).unwrap();
    // Touched fields use the canonical quoted form.
    assert!(content.contains(r#"status: "superseded""#));
    assert!(content.contains(r#"superseded_by: "doc-new""#));
    // Untouched lines — including the author's comment — are preserved
    // verbatim. A full YAML round-trip would have rewritten them.
    assert!(content.contains("id: doc-old"));
    assert!(content.contains("title: Old"));
    assert!(content.contains("kind: generic"));
    assert!(content.contains("# author note"));
    assert!(content.contains("# Old"));
    // Subsequent build picks up the change and materialises the
    // canonical supersedes edge.
    nodex(tmp.path())
        .arg("build")
        .arg("--full")
        .assert()
        .success();
    let data = run_json(nodex(tmp.path()).args(["query", "chain", "doc-old"]));
    assert_eq!(
        data.get("total").and_then(Value::as_u64),
        Some(2),
        "chain should walk old → new after lifecycle write"
    );
}

#[test]
fn missing_project_dir_emits_io_error_code() {
    // -C into a path that doesn't exist must classify as IO_ERROR,
    // not the catch-all INTERNAL_ERROR. Catches regression of the
    // `with_context` pattern that swallowed typed io::Error.
    let nonexistent = "/nonexistent-nodex-dir-abc-xyz";
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nodex"))
        .args(["-C", nonexistent, "query", "orphans"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("IO_ERROR"),
        "missing project dir must surface as IO_ERROR, not INTERNAL_ERROR"
    );
}

#[test]
fn init_twice_emits_already_exists_code() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path()).arg("init").output().expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(false)));
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("ALREADY_EXISTS")
    );
}

#[test]
fn query_backlinks_unknown_id_emits_not_found_code() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "backlinks", "ghost-id"])
        .output()
        .expect("ran");
    assert!(
        !output.status.success(),
        "missing id must error, not silently return empty"
    );
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("NOT_FOUND")
    );
}

#[test]
fn query_chain_unknown_id_emits_not_found_code() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "chain", "ghost-id"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("NOT_FOUND")
    );
}

#[test]
fn query_node_unknown_emits_not_found_code() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "node", "does-not-exist"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("NOT_FOUND")
    );
}

#[test]
fn rename_source_missing_emits_io_error_code() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["rename", "docs/nope.md", "docs/elsewhere.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("IO_ERROR")
    );
}

#[test]
fn unknown_subcommand_emits_invalid_argument_envelope() {
    let tmp = scratch();
    let output = nodex(tmp.path()).arg("notacommand").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(false)));
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT")
    );
}

#[test]
fn check_severity_invalid_value_rejected_by_clap() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["check", "--severity", "bogus"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT")
    );
}

#[test]
fn lifecycle_supersede_missing_to_rejected_by_clap() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    // clap now rejects supersede without --to at parse time.
    let output = nodex(tmp.path())
        .args(["lifecycle", "supersede", "a"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT")
    );
}

#[test]
#[cfg(unix)]
fn rename_rewriter_does_not_follow_symlinks() {
    // The link rewriter touches every in-scope file. Symlinks are
    // skipped so a link pointing outside the project root cannot have
    // its target mutated through the symlink.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    let external = outside.path().join("external.md");
    fs::write(
        &external,
        "---\nid: external\ntitle: External\nkind: generic\nstatus: active\n---\n# External\n\
         See [a](a.md) for details.\n",
    )
    .unwrap();
    let before = fs::read_to_string(&external).unwrap();
    unix_fs::symlink(&external, tmp.path().join("linked.md")).unwrap();

    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a.md", "renamed.md"])
        .assert()
        .success();

    // External file byte-identical even though it contained a link
    // to `a.md` that the rewriter would otherwise have updated.
    let after = fs::read_to_string(&external).unwrap();
    assert_eq!(
        before, after,
        "rename must not mutate external files reached through a symlink"
    );
    // The in-project file still got renamed.
    assert!(tmp.path().join("renamed.md").exists());
}

#[test]
#[cfg(unix)]
fn lifecycle_refuses_to_mutate_through_symlink() {
    // `lifecycle::transition` refuses to write through a symlink, so
    // `nodex lifecycle archive <id>` on a symlinked doc cannot reach
    // a target outside the project root.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    let external = outside.path().join("external.md");
    fs::write(
        &external,
        "---\nid: ext\ntitle: Ext\nkind: generic\nstatus: active\n---\n# Ext\n",
    )
    .unwrap();
    let before = fs::read_to_string(&external).unwrap();
    unix_fs::symlink(&external, tmp.path().join("linked.md")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["lifecycle", "archive", "ext"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "must reject symlink mutation");
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );

    // The external file must be byte-identical to before.
    let after = fs::read_to_string(&external).unwrap();
    assert_eq!(
        before, after,
        "external file through symlink must not be mutated"
    );
}

#[test]
fn rename_rewrites_markdown_links_but_not_prose() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n\
         # A\n\
         Prose mention of docs/b.md must survive verbatim.\n\
         But this [link](docs/b.md) and [anchored](docs/b.md#section) must update.\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "docs/b.md", "docs/c.md"])
        .assert()
        .success();
    let content = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    // Prose occurrence must NOT be rewritten.
    assert!(
        content.contains("Prose mention of docs/b.md must survive verbatim."),
        "prose was corrupted: {content}"
    );
    // Both markdown links MUST be rewritten, preserving anchor.
    assert!(content.contains("[link](docs/c.md)"), "link not updated");
    assert!(
        content.contains("[anchored](docs/c.md#section)"),
        "anchored link not updated"
    );
}

#[test]
fn scaffold_rejects_path_traversal() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "x"])
        .args(["--path", "../escaped.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
}

#[test]
fn rename_rewrites_both_root_and_file_relative_links() {
    let tmp = scratch();
    init_project(tmp.path());
    // Link written **file-relative** from sibling inside same dir.
    write_doc(
        tmp.path(),
        "docs/sibling.md",
        "---\nid: s\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n[x](first.md)\n",
    );
    // Link written **root-relative** from a different dir.
    write_doc(
        tmp.path(),
        "notes/n.md",
        "---\nid: n\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n[y](docs/first.md)\n",
    );
    write_doc(
        tmp.path(),
        "docs/first.md",
        "---\nid: f\ntitle: F\nkind: generic\nstatus: active\n---\n# F\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "docs/first.md", "docs/new.md"])
        .assert()
        .success();
    let sibling = fs::read_to_string(tmp.path().join("docs/sibling.md")).unwrap();
    assert!(
        sibling.contains("[x](new.md)"),
        "file-relative link should stay file-relative: {sibling}"
    );
    let note = fs::read_to_string(tmp.path().join("notes/n.md")).unwrap();
    assert!(
        note.contains("[y](docs/new.md)"),
        "root-relative link should stay root-relative: {note}"
    );
}

#[test]
fn kinds_allowed_empty_rejected_at_load() {
    let tmp = scratch();
    fs::write(tmp.path().join("nodex.toml"), "[kinds]\nallowed = []\n").unwrap();
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn rename_rejects_path_traversal() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["rename", "docs/a.md", "../escaped.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
}

#[test]
fn bom_prefixed_frontmatter_parses_correctly() {
    let tmp = scratch();
    init_project(tmp.path());
    // Write file prefixed with a UTF-8 BOM.
    let path = tmp.path().join("docs/bom.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    bytes.extend_from_slice(
        b"---\nid: bom-id\ntitle: BOM\nkind: generic\nstatus: active\n---\n# BOM\n",
    );
    fs::write(&path, bytes).unwrap();
    let data = run_json(nodex(tmp.path()).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "BOM-prefixed file should still produce exactly one node"
    );
    // And its id came from frontmatter, not from inferred filename.
    let detail = run_json(nodex(tmp.path()).args(["query", "node", "bom-id"]));
    assert!(
        detail.get("node").is_some(),
        "bom-id should resolve; BOM must be stripped"
    );
}

#[test]
fn huge_stale_days_does_not_panic() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[detection]
stale_days = 4294967295
orphan_grace_days = 4294967295
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\nreviewed: 2020-01-01\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    // Pathological `stale_days` values must not panic in date arithmetic.
    nodex(tmp.path()).arg("check").assert().success();
    nodex(tmp.path())
        .args(["query", "stale"])
        .assert()
        .success();
    nodex(tmp.path())
        .args(["query", "orphans"])
        .assert()
        .success();
}

#[test]
fn invalid_naming_rule_rejected_at_config_load() {
    let tmp = scratch();
    // Invalid regex — should fail fast at Config::validate.
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[[rules.naming]]
glob = "docs/**/*.md"
pattern = "[invalid("
"#,
    )
    .unwrap();
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("rules.naming"),
        "error message should identify which rule failed"
    );
}

#[test]
fn rename_target_existing_emits_already_exists_code() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["rename", "docs/a.md", "docs/b.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("ALREADY_EXISTS")
    );
}

#[test]
fn malformed_frontmatter_yaml_classifies_as_parse_error() {
    let tmp = scratch();
    init_project(tmp.path());
    // Closing delimiter present but the YAML body is unparsable
    // (unclosed flow-sequence) — this must be Error::Parse with the
    // failing file's path, not a panic or a generic Other.
    write_doc(
        tmp.path(),
        "docs/broken.md",
        "---\ntags: [a, b\n---\n# Broken\n",
    );
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert!(
        !output.status.success(),
        "build must fail on malformed YAML"
    );
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PARSE_ERROR")
    );
    let msg = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("docs/broken.md") || msg.contains("docs\\broken.md"),
        "error must name the failing file, got: {msg}"
    );
}

#[test]
fn frontmatter_handles_bom_and_crlf() {
    let tmp = scratch();
    init_project(tmp.path());
    // UTF-8 BOM followed by CRLF-terminated frontmatter — Windows
    // editors emit this combo. Both cleaning steps must happen.
    let mut content = Vec::new();
    content.extend_from_slice(b"\xEF\xBB\xBF");
    content.extend_from_slice(
        b"---\r\nid: bom-doc\r\ntitle: BOM\r\nkind: generic\r\nstatus: active\r\n---\r\n# Body\r\n",
    );
    let path = tmp.path().join("docs/bom.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "node", "bom-doc"]));
    assert_eq!(
        data.pointer("/node/title").and_then(Value::as_str),
        Some("BOM"),
        "BOM + CRLF frontmatter must parse cleanly"
    );
}

#[test]
fn covers_emits_edges_and_reverse_lookup_works() {
    let tmp = scratch();
    init_project(tmp.path());
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/auth.rs"), "// stub").unwrap();
    write_doc(
        tmp.path(),
        "docs/adr-auth.md",
        "---\nid: adr-auth\ntitle: Auth ADR\nkind: generic\nstatus: active\ncovers:\n  - src/auth.rs\n---\n# Auth\n",
    );
    write_doc(
        tmp.path(),
        "docs/runbook.md",
        "---\nid: runbook-auth\ntitle: Runbook\nkind: generic\nstatus: active\ncovers: src/auth.rs\n---\n# Runbook\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // Forward: each doc records its `covers` paths as outgoing edges.
    let detail = run_json(nodex(tmp.path()).args(["query", "node", "adr-auth"]));
    let outgoing = detail
        .pointer("/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing array");
    assert!(
        outgoing
            .iter()
            .any(|e| e.get("relation").and_then(Value::as_str) == Some("covers"))
    );

    // Reverse: covered_by surfaces every doc that claims coverage.
    let coverage = run_json(nodex(tmp.path()).args(["query", "covered-by", "src/auth.rs"]));
    assert_eq!(
        coverage.get("total").and_then(Value::as_u64),
        Some(2),
        "both docs covering the path must surface"
    );
}

#[test]
fn log_creates_session_and_appends_events() {
    let tmp = scratch();
    init_memory_project(tmp.path());

    let first = run_json(nodex(tmp.path()).args(["log", "first event"]));
    let session_id = first
        .get("session_id")
        .and_then(Value::as_str)
        .expect("session_id")
        .to_string();
    assert_eq!(first.get("event_index").and_then(Value::as_u64), Some(1));
    assert_eq!(
        first
            .get("outcome")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str),
        Some("created")
    );

    let second = run_json(nodex(tmp.path()).args([
        "log",
        "second event",
        "--session",
        &session_id,
        "--related",
        "doc-x,doc-y",
    ]));
    assert_eq!(second.get("event_index").and_then(Value::as_u64), Some(2));
    assert_eq!(
        second
            .get("outcome")
            .and_then(|o| o.get("kind"))
            .and_then(Value::as_str),
        Some("appended")
    );

    let session_path = tmp
        .path()
        .join("_sessions")
        .join(format!("{session_id}.md"));
    let body = fs::read_to_string(&session_path).unwrap();
    assert!(body.contains("event_count: \"2\""));
    assert!(body.contains("— first event"));
    assert!(body.contains("— second event"));
    assert!(body.contains("related:\n  - \"doc-x\"\n  - \"doc-y\""));
}

#[test]
fn continue_returns_last_session_with_pack() {
    let tmp = scratch();
    init_memory_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/related-doc.md",
        "---\nid: related-doc\ntitle: Related\nkind: generic\nstatus: active\n---\n# Related\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let logged =
        run_json(nodex(tmp.path()).args(["log", "started work", "--related", "related-doc"]));
    let session_id = logged
        .get("session_id")
        .and_then(Value::as_str)
        .expect("session_id")
        .to_string();
    nodex(tmp.path()).arg("build").assert().success();

    let cont = run_json(nodex(tmp.path()).args(["continue"]));
    assert_eq!(
        cont.get("id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    assert_eq!(cont.get("event_count").and_then(Value::as_u64), Some(1));
    assert_eq!(
        cont.get("last_event_summary").and_then(Value::as_str),
        Some("started work")
    );
    let pack_ids: Vec<&str> = cont
        .pointer("/pack/included")
        .and_then(Value::as_array)
        .expect("pack.included")
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .collect();
    assert!(pack_ids.contains(&session_id.as_str()));
    assert!(
        pack_ids.contains(&"related-doc"),
        "pack must include doc declared in session.related"
    );
}

#[test]
fn continue_returns_null_when_no_session_in_window() {
    let tmp = scratch();
    init_memory_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let cont = run_json(nodex(tmp.path()).args(["continue"]));
    assert!(cont.is_null(), "no session → null payload, got {cont}");
}

#[test]
fn trust_returns_score_with_components() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/active.md",
        "---\nid: doc-active\ntitle: Active\nkind: generic\nstatus: active\n---\n# Active\n",
    );
    write_doc(
        tmp.path(),
        "docs/archived.md",
        "---\nid: doc-archived\ntitle: Archived\nkind: generic\nstatus: archived\n---\n# Archived\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let active = run_json(nodex(tmp.path()).args(["trust", "doc-active"]));
    let archived = run_json(nodex(tmp.path()).args(["trust", "doc-archived"]));

    let active_score = active.get("score").and_then(Value::as_f64).unwrap();
    let archived_score = archived.get("score").and_then(Value::as_f64).unwrap();
    assert!(
        active_score > archived_score,
        "active ({active_score}) must outrank archived ({archived_score})"
    );
    assert!(active.pointer("/components/status").is_some());
    assert!(active.pointer("/components/freshness").is_some());
    assert!(active.pointer("/components/backlinks").is_some());
}

#[test]
fn similar_finds_existing_doc_with_token_overlap() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: Auth Retry Policy\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: Auth Retry Policy v2\nkind: generic\nstatus: active\n---\n# Auth v2\n",
    );
    write_doc(
        tmp.path(),
        "docs/c.md",
        "---\nid: doc-c\ntitle: Completely Unrelated Topic\nkind: generic\nstatus: active\n---\n# Other\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["similar", "--id", "doc-a"]));
    let items = data.get("items").and_then(Value::as_array).expect("items");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert!(ids.contains(&"doc-b"), "shared title tokens must surface");
    assert!(
        !ids.contains(&"doc-c"),
        "unrelated must stay below threshold"
    );
}

#[test]
fn scaffold_warns_when_similar_doc_exists() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/existing.md",
        "---\nid: doc-existing\ntitle: Auth Retry Policy\nkind: generic\nstatus: active\n---\n# Existing\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let envelope = run_envelope(nodex(tmp.path()).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Auth Retry Policy v2",
        "--id",
        "doc-new",
        "--path",
        "docs/new.md",
        "--dry-run",
    ]));
    // Per `.claude/rules/json-output.md`, warnings live at the
    // envelope level — never nested inside `data`. A consumer that
    // parses `envelope.warnings` is the one we promise to support.
    let warnings: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("envelope-level warnings array")
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("similar doc exists") && w.contains("doc-existing")),
        "scaffold must warn about similar existing doc at envelope level; got {warnings:?}"
    );
    // Negative side: `data` must NOT carry a stray `warnings` field —
    // that's the contract violation we just removed.
    assert!(
        envelope.pointer("/data/warnings").is_none(),
        "scaffold result must not nest warnings inside data: {envelope}"
    );
}

#[test]
fn recent_lists_docs_within_window_newest_first() {
    let tmp = scratch();
    init_project(tmp.path());
    let today = chrono::Local::now().date_naive();
    let recent_date = today - chrono::Duration::days(2);
    let stale_date = today - chrono::Duration::days(30);
    write_doc(
        tmp.path(),
        "docs/recent.md",
        &format!(
            "---\nid: doc-recent\ntitle: Recent\nkind: generic\nstatus: active\nupdated: {recent_date}\n---\n# Recent\n"
        ),
    );
    write_doc(
        tmp.path(),
        "docs/stale.md",
        &format!(
            "---\nid: doc-stale\ntitle: Stale\nkind: generic\nstatus: active\nupdated: {stale_date}\n---\n# Stale\n"
        ),
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["recent", "--days", "7", "--field", "updated"]));
    let items = data.get("items").and_then(Value::as_array).expect("items");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(ids, vec!["doc-recent"]);
    assert_eq!(data.get("total").and_then(Value::as_u64), Some(1));
}

#[test]
fn pack_returns_token_budgeted_bundle() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/seed.md",
        "---\nid: seed\ntitle: Seed\nkind: generic\nstatus: active\n---\n# Seed\n\nReferences [a](docs/a.md) and [b](docs/b.md).\n",
    );
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nLeaf doc.\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: superseded\nsuperseded_by: a\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let bundle = run_json(nodex(tmp.path()).args([
        "pack",
        "seed",
        "--token-budget",
        "5000",
        "--depth",
        "2",
    ]));
    assert_eq!(
        bundle.get("seed").and_then(Value::as_str),
        Some("seed"),
        "seed echoed back"
    );
    let included = bundle
        .get("included")
        .and_then(Value::as_array)
        .expect("included array");
    let ids: Vec<&str> = included
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .collect();
    assert!(ids.contains(&"seed"));
    assert!(ids.contains(&"a"));
    let total = bundle
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert!(total > 0, "total_tokens must be a positive estimate");
    // Healthy doc 'a' must appear before terminal 'b' if both included.
    if let (Some(pos_a), Some(pos_b)) = (
        ids.iter().position(|x| *x == "a"),
        ids.iter().position(|x| *x == "b"),
    ) {
        assert!(pos_a < pos_b, "healthy node must appear before terminal");
    }
}

#[test]
fn wikilinks_resolve_end_to_end_when_enabled() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[parser]
wikilink_enabled = true
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nReferences [[doc-b]] and [[docs/c.md]].\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        tmp.path(),
        "docs/c.md",
        "---\nid: doc-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    // doc-b receives a backlink from doc-a via the [[doc-b]] wikilink
    // (resolved against the path index since docs/doc-b.md doesn't
    // exist; falls through to id lookup is left as a future
    // refinement). doc-c receives one via the explicit-path form.
    let backlinks_c = run_json(nodex(tmp.path()).args(["query", "backlinks", "doc-c"]));
    assert!(
        backlinks_c
            .pointer("/total")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            >= 1,
        "[[docs/c.md]] wikilink must resolve to doc-c"
    );
}
