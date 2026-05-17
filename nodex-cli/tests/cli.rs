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
fn query_low_trust_lists_below_cutoff() {
    // Two docs: a fresh active one (trust ≈ 1.0) and an archived one
    // (status = 0 → composite well below 0.5 default). `low-trust`
    // must include the archived doc and exclude the fresh one.
    let tmp = scratch();
    init_project(tmp.path());
    let today = chrono::Local::now().date_naive();
    write_doc(
        tmp.path(),
        "docs/fresh.md",
        &format!(
            "---\nid: doc-fresh\ntitle: Fresh\nkind: generic\nstatus: active\nreviewed: {today}\n---\n# Fresh\n"
        ),
    );
    write_doc(
        tmp.path(),
        "docs/dead.md",
        "---\nid: doc-dead\ntitle: Dead\nkind: generic\nstatus: archived\n---\n# Dead\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "low-trust"]));
    let ids: Vec<&str> = data
        .get("items")
        .and_then(serde_json::Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        ids.contains(&"doc-dead"),
        "archived doc must surface; got {ids:?}"
    );
    assert!(
        !ids.contains(&"doc-fresh"),
        "fresh active doc must not surface; got {ids:?}"
    );
}

#[test]
fn query_low_trust_threshold_override() {
    // `--threshold 1.0` includes everything (every score < 1.0).
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "low-trust", "--threshold", "1.0"]));
    assert!(
        data.get("total")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            >= 1,
        "threshold 1.0 must surface at least one node",
    );
}

#[test]
fn query_low_trust_entries_always_carry_components() {
    // Every entry returned by `query low-trust` must include the
    // per-component breakdown — composite-score-only is a forbidden
    // shape (we deliberately surface `components` so callers can
    // re-rank without consulting a second endpoint).
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "low-trust", "--threshold", "1.0"]));
    let items = data.get("items").and_then(Value::as_array).expect("items");
    assert!(!items.is_empty(), "fixture must produce at least one entry");
    for item in items {
        assert!(
            item.pointer("/components/status").is_some(),
            "components.status missing on {item}"
        );
        assert!(
            item.pointer("/components/freshness").is_some(),
            "components.freshness missing on {item}"
        );
        assert!(
            item.pointer("/components/backlinks").is_some(),
            "components.backlinks missing on {item}"
        );
    }
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
fn query_issues_distinguishes_excluded_target_from_missing_one() {
    // Two body links from doc-a:
    //   - to docs/missing.md (truly absent on disk) → kind == Missing
    //   - to specs/x/sub.md, which exists but is dropped from scope
    //     by conditional_exclude because specs/x/spec.md is terminal
    //     → kind == ExcludedFromScope
    //
    // Without the typed `kind` field a consumer would have to either
    // stat the disk themselves or guess from a generic reason string;
    // the contract is that `query issues` answers the "is this a
    // missing file or an excluded file" question directly so the
    // remediation (create vs. re-include / delete link) is obvious.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md", "specs/**/*.md"]
[[scope.conditional_exclude]]
parent_glob = "specs/*/spec.md"
condition = "status_terminal"
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nSee [gone](docs/missing.md) and [excluded](specs/x/sub.md).\n",
    );
    write_doc(
        tmp.path(),
        "specs/x/spec.md",
        "---\nid: spec-x\ntitle: Spec X\nkind: generic\nstatus: archived\n---\n# Spec\n",
    );
    write_doc(
        tmp.path(),
        "specs/x/sub.md",
        "---\nid: spec-x-sub\ntitle: Sub\nkind: generic\nstatus: active\n---\n# Sub\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["query", "issues"]));
    let unresolved = data
        .get("unresolved_edges")
        .and_then(Value::as_array)
        .expect("unresolved_edges array");
    let by_target: std::collections::BTreeMap<&str, &str> = unresolved
        .iter()
        .filter_map(|e| {
            let target = e.get("raw_target").and_then(Value::as_str)?;
            let kind = e.get("kind").and_then(Value::as_str)?;
            Some((target, kind))
        })
        .collect();
    assert_eq!(
        by_target.get("docs/missing.md").copied(),
        Some("missing"),
        "truly absent target must be `missing`; got {by_target:?}"
    );
    assert_eq!(
        by_target.get("specs/x/sub.md").copied(),
        Some("excluded_from_scope"),
        "on-disk-but-excluded target must be `excluded_from_scope`; got {by_target:?}"
    );
}

#[test]
fn migrate_rejects_self_collision_between_bare_files() {
    // Two bare files in distinct directories both infer the same id
    // (`{kind}-{stem}` template, both stems = "foo"). Migrating both
    // would write the same `id:` to both → next build fails with
    // DUPLICATE_ID. The atomic refuse must surface DUPLICATE_ID at
    // exit 2 and leave both files byte-for-byte unchanged.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
    let bare = "# Foo\n\nbody\n";
    write_doc(tmp.path(), "docs/foo.md", bare);
    write_doc(tmp.path(), "specs/foo.md", bare);

    let output = nodex(tmp.path())
        .args(["migrate", "--apply"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("DUPLICATE_ID")
    );
    let docs_foo = fs::read_to_string(tmp.path().join("docs/foo.md")).unwrap();
    let specs_foo = fs::read_to_string(tmp.path().join("specs/foo.md")).unwrap();
    assert_eq!(docs_foo, bare, "first bare file must remain untouched");
    assert_eq!(specs_foo, bare, "second bare file must remain untouched");
}

#[test]
fn migrate_rejects_collision_against_existing_explicit_id() {
    // A bare file's inferred id collides with an existing file's
    // explicit id. Same DUPLICATE_ID atomic refuse contract.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/existing.md",
        "---\nid: generic-bare\ntitle: Existing\nkind: generic\nstatus: active\n---\n# Existing\n",
    );
    let bare = "# Bare\n\nbody\n";
    write_doc(tmp.path(), "docs/bare.md", bare);

    let output = nodex(tmp.path())
        .args(["migrate", "--apply"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("DUPLICATE_ID")
    );
    let after = fs::read_to_string(tmp.path().join("docs/bare.md")).unwrap();
    assert_eq!(
        after, bare,
        "rejected migration must leave the bare file untouched"
    );
}

#[test]
fn migrate_apply_succeeds_when_inferred_ids_are_distinct() {
    // Sanity contract for the happy path: distinct stems → distinct
    // inferred ids → migration writes frontmatter and the resulting
    // graph builds without a DUPLICATE_ID surface. Lock-in so a
    // future tightening of the collision check doesn't accidentally
    // reject valid input.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
    write_doc(tmp.path(), "docs/alpha.md", "# Alpha\n");
    write_doc(tmp.path(), "docs/beta.md", "# Beta\n");

    let data = run_json(nodex(tmp.path()).args(["migrate", "--apply"]));
    assert_eq!(data.get("total").and_then(Value::as_u64), Some(2));
    nodex(tmp.path()).arg("build").assert().success();
}

#[test]
fn build_cache_prunes_entries_for_deleted_files() {
    // Lock the cache-pruning invariant: when a previously-tracked
    // file is deleted (or moved out of scope), its cache entry must
    // be dropped on the next build. Otherwise `_index/cache.json`
    // would accumulate orphaned entries forever, eventually bloating
    // load time. The `builder::build` pipeline calls
    // `BuildCache::retain_paths` against the current scan results;
    // this test is the regression gate that catches a refactor
    // accidentally removing that call.
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

    let cache_path = tmp.path().join("_index").join("cache.json");
    let before: Value = serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    let entries_before = before
        .get("entries")
        .and_then(Value::as_object)
        .expect("cache.entries object");
    assert!(
        entries_before.keys().any(|k| k.ends_with("a.md"))
            && entries_before.keys().any(|k| k.ends_with("b.md")),
        "initial cache must hold both docs: {entries_before:?}"
    );

    fs::remove_file(tmp.path().join("docs/b.md")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    let after: Value = serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    let entries_after = after
        .get("entries")
        .and_then(Value::as_object)
        .expect("cache.entries object");
    assert!(
        entries_after.keys().any(|k| k.ends_with("a.md")),
        "surviving doc must remain cached: {entries_after:?}"
    );
    assert!(
        !entries_after.keys().any(|k| k.ends_with("b.md")),
        "deleted doc's cache entry must be pruned: {entries_after:?}"
    );
}

#[test]
fn query_similar_rejects_unknown_kind() {
    // `--kind not-in-allowed` would silently mismatch every doc on
    // the kind component and return an empty list — a quiet "no
    // duplicates" instead of an honest "your argument is invalid".
    // The contract is fail-fast with CONFIG_ERROR.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "similar", "--title", "X", "--kind", "ghost-kind"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn query_covered_by_normalises_dot_prefix_and_dot_dot_segments() {
    // `covers: ["src/lib.rs"]` must match queries written as
    // `./src/lib.rs`, `src/../src/lib.rs`, or backslash-flavoured
    // `src\\lib.rs` (Windows). All four authoring styles refer to
    // the same source file; the reverse lookup is useless if it's
    // case-sensitive to incidental syntax.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\ncovers:\n  - src/lib.rs\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    for needle in [
        "src/lib.rs",
        "./src/lib.rs",
        "src/./lib.rs",
        "src/../src/lib.rs",
    ] {
        let data = run_json(nodex(tmp.path()).args(["query", "covered-by", needle]));
        let total = data.get("total").and_then(Value::as_u64).unwrap_or(0);
        assert!(
            total >= 1,
            "needle {needle:?} must match covers: [src/lib.rs] (total={total}): {data}"
        );
    }
}

#[test]
fn self_reference_excluded_from_orphan_and_backlinks_but_visible_in_node_detail() {
    // A doc whose only incoming edge is its own self-reference is
    // *isolated* in the external-attention sense and must surface
    // as an orphan. `query backlinks` mirrors that semantic. But
    // `query node` is the honest graph view — the self-edge stays
    // visible there so structural inspection isn't lossy.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"

[detection]
orphan_grace_days = 0
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\nSee [self](a.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let orphans = run_json(nodex(tmp.path()).args(["query", "orphans"]));
    let ids: Vec<&str> = orphans
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert!(
        ids.contains(&"doc-a"),
        "self-only doc must surface as orphan: {ids:?}"
    );

    let backlinks = run_json(nodex(tmp.path()).args(["query", "backlinks", "doc-a"]));
    assert_eq!(
        backlinks.get("total").and_then(Value::as_u64),
        Some(0),
        "self-edge must not appear as a backlink: {backlinks}"
    );

    let node = run_json(nodex(tmp.path()).args(["query", "node", "doc-a"]));
    let outgoing = node.get("outgoing").and_then(Value::as_array).unwrap();
    assert_eq!(
        outgoing.len(),
        1,
        "honest node detail must keep the self-edge in outgoing: {node}"
    );
}

#[test]
fn build_full_purges_cache_entries_for_files_no_longer_in_scope() {
    // `--full` starts from an empty cache, so a file deleted between
    // builds must not survive in `cache.json`. Companion lock-in to
    // the incremental-build prune test — covers the rebuild path.
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

    fs::remove_file(tmp.path().join("docs/b.md")).unwrap();
    nodex(tmp.path())
        .args(["build", "--full"])
        .assert()
        .success();

    let cache_path = tmp.path().join("_index").join("cache.json");
    let cache: Value = serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    let entries = cache
        .get("entries")
        .and_then(Value::as_object)
        .expect("cache.entries object");
    assert!(
        !entries.keys().any(|k| k.ends_with("b.md")),
        "deleted doc must be absent from --full rebuild cache: {entries:?}"
    );
}

#[test]
fn build_produces_deterministic_node_detail_across_rebuilds() {
    // The JSON envelope from `query node` must be byte-identical
    // across two `build` cycles on the same input — adjacency
    // ordering, field serialisation, and edge collection are all
    // deterministic by construction; this lock-in catches an
    // accidental introduction of HashMap iteration or a clock
    // dependency in the rebuild path.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\nSee [b](docs/b.md) and [c](docs/c.md).\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\nSee [a](docs/a.md).\n",
    );
    write_doc(
        tmp.path(),
        "docs/c.md",
        "---\nid: doc-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\nSee [a](docs/a.md).\n",
    );

    nodex(tmp.path()).arg("build").assert().success();
    let first = run_json(nodex(tmp.path()).args(["query", "node", "doc-a"]));
    nodex(tmp.path())
        .args(["build", "--full"])
        .assert()
        .success();
    let second = run_json(nodex(tmp.path()).args(["query", "node", "doc-a"]));
    assert_eq!(
        first, second,
        "node detail must be deterministic across rebuilds"
    );
}

#[test]
fn lifecycle_review_refuses_to_overwrite_future_reviewed_date() {
    // Future-dated `reviewed` carries real information (clock skew or
    // intentional "approved through" marker). `Action::Review` must
    // refuse rather than silently replace it with today — that loss
    // would violate the monotonicity assumption the audit trail
    // relies on. INVALID_TRANSITION with the existing date in the
    // `from → to` payload is the contract.
    let tmp = scratch();
    init_project(tmp.path());
    let original =
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nreviewed: 2099-01-01\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["lifecycle", "review", "doc-a"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_TRANSITION")
    );
    let after = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    assert_eq!(
        after, original,
        "rejected review must leave source byte-for-byte unchanged: {after}"
    );
}

#[test]
fn lifecycle_review_updates_past_reviewed_date() {
    // Happy path: a past `reviewed` date is bumped to today. Lock in
    // so the future-date guard above doesn't accidentally reject the
    // normal case too.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nreviewed: 2000-01-01\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["lifecycle", "review", "doc-a"])
        .assert()
        .success();
    let after = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    assert!(
        !after.contains("reviewed: 2000-01-01") && !after.contains("reviewed: \"2000-01-01\""),
        "past reviewed date must be bumped: {after}"
    );
}

#[test]
fn lifecycle_supersede_rejects_unknown_successor() {
    // Self-consistency invariant: nodex must never write a frontmatter
    // value the next `build` would surface as a broken edge. A
    // supersede pointing at a non-existent id must be rejected BEFORE
    // any file mutation, with NOT_FOUND surfaced at exit 2. The
    // source file's frontmatter must be byte-for-byte unchanged.
    let tmp = scratch();
    init_project(tmp.path());
    let original = "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["lifecycle", "supersede", "a", "--to", "ghost"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("NOT_FOUND")
    );
    let after = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    assert_eq!(
        after, original,
        "rejected lifecycle must leave source untouched: {after}"
    );
}

#[test]
fn lifecycle_supersede_rejects_cycle() {
    // Pre-existing chain A -> B -> C; closing it via `supersede C --to A`
    // would silently corrupt the graph. The pre-check must surface
    // CYCLE_DETECTED at exit 2 *before* mutating C's frontmatter.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: superseded\nsuperseded_by: b\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: superseded\nsuperseded_by: c\n---\n# B\n",
    );
    let c_original = "---\nid: c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n";
    write_doc(tmp.path(), "docs/c.md", c_original);
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["lifecycle", "supersede", "c", "--to", "a"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CYCLE_DETECTED")
    );
    let after = fs::read_to_string(tmp.path().join("docs/c.md")).unwrap();
    assert_eq!(
        after, c_original,
        "rejected cycle-introducing supersede must leave source untouched: {after}"
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

/// Project that derives every id from the file stem
/// (`{kind}-{stem}`), so renaming a file mechanically changes the
/// inferred id. This is the exact configuration where the rename
/// command has to anchor an explicit `id:` to keep cross-document
/// references valid — and the test fixture used by the suite below
/// to lock that contract in.
fn init_path_derived_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
}

#[test]
fn rename_explicit_id_doc_does_not_touch_frontmatter() {
    // When `id:` is pinned explicitly, the rename is path-only by
    // construction. The doc's frontmatter must NOT be rewritten — a
    // minimal-diff invariant the `id_stability` envelope locks in.
    let tmp = scratch();
    init_path_derived_project(tmp.path());
    let original = "---\nid: explicit-anchor\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["rename", "docs/a.md", "docs/renamed.md"]));
    assert_eq!(
        data.pointer("/id_stability/kind").and_then(Value::as_str),
        Some("already_anchored")
    );
    let moved = fs::read_to_string(tmp.path().join("docs/renamed.md")).unwrap();
    assert_eq!(
        moved, original,
        "explicit-id doc must move byte-for-byte unchanged"
    );
}

#[test]
fn rename_path_derived_id_anchors_into_frontmatter() {
    // The blocker scenario: id is derived from stem, the rename
    // changes the stem, and another doc references the old id via
    // frontmatter. Without anchoring, the reference would dangle on
    // the next build. Lock in: after rename, the moved doc carries
    // the old id explicitly, and the referencing doc's `related`
    // edge still resolves end-to-end.
    let tmp = scratch();
    init_path_derived_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\nrelated:\n  - generic-a\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["rename", "docs/a.md", "docs/a-renamed.md"]));
    assert_eq!(
        data.pointer("/id_stability/kind").and_then(Value::as_str),
        Some("anchored")
    );
    assert_eq!(
        data.pointer("/id_stability/id").and_then(Value::as_str),
        Some("generic-a")
    );

    // The moved file now has an explicit `id: generic-a` line.
    let moved = fs::read_to_string(tmp.path().join("docs/a-renamed.md")).unwrap();
    assert!(
        moved.contains("id: \"generic-a\"") || moved.contains("id: generic-a"),
        "anchored id must appear in moved frontmatter; got:\n{moved}"
    );

    // End-to-end witness: rebuild and confirm the cross-doc edge
    // still resolves. A backlink query on `generic-a` must surface
    // `generic-b` — the original guarantee a path-derived rename was
    // silently breaking.
    nodex(tmp.path()).arg("build").assert().success();
    let backlinks = run_json(nodex(tmp.path()).args(["query", "backlinks", "generic-a"]));
    let ids: Vec<&str> = backlinks
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert!(
        ids.contains(&"generic-b"),
        "cross-doc reference must survive the rename: {ids:?}"
    );
}

#[test]
fn rename_path_derived_id_no_anchor_when_id_would_not_change() {
    // Edge case: a path-derived id project where the rename happens
    // to preserve the inferred id (e.g., the stem stays the same and
    // only the directory changes, under a glob that produces the
    // same template output). Anchoring would be unnecessary churn —
    // the contract is minimal-diff. Lock in: `id_stability == "unchanged"`.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md", "specs/**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
    let original = "---\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "docs/a.md", "specs/a.md"]));
    assert_eq!(
        data.pointer("/id_stability/kind").and_then(Value::as_str),
        Some("unchanged")
    );
    let moved = fs::read_to_string(tmp.path().join("specs/a.md")).unwrap();
    assert_eq!(
        moved, original,
        "no-id-change rename must leave frontmatter byte-for-byte intact"
    );
}

#[test]
fn rename_bare_markdown_warns_about_id_shift() {
    // A bare markdown file (no frontmatter) still gets an inferred
    // id from the path. Renaming it changes that id, but the file
    // has no frontmatter for us to anchor into — silently moving on
    // would break references. Contract: `id_stability ==
    // "bare_no_frontmatter"` with an envelope-level warning so the
    // caller cannot miss it.
    let tmp = scratch();
    init_path_derived_project(tmp.path());
    write_doc(tmp.path(), "docs/bare.md", "# Bare\n\nNo frontmatter.\n");
    nodex(tmp.path()).arg("build").assert().success();

    let envelope =
        run_envelope(nodex(tmp.path()).args(["rename", "docs/bare.md", "docs/bare-renamed.md"]));
    assert_eq!(
        envelope
            .pointer("/data/id_stability/kind")
            .and_then(Value::as_str),
        Some("bare_no_frontmatter")
    );
    let warnings: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("envelope-level warnings array")
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("inferred id changed")),
        "bare-file rename must surface a warning, not silently drift: {warnings:?}"
    );
}

#[test]
fn malformed_frontmatter_yaml_surfaces_as_envelope_warning() {
    // Malformed YAML in a single document does NOT halt the build.
    // The file is dropped from the graph (no node), and the failure
    // surfaces as an envelope-level warning naming the failing path.
    // This mirrors the read-error handling — one bad file should not
    // block the operator from inspecting the rest of the project.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/broken.md",
        "---\ntags: [a, b\n---\n# Broken\n",
    );
    write_doc(
        tmp.path(),
        "docs/good.md",
        "---\nid: doc-good\ntitle: Good\nkind: generic\nstatus: active\n---\n# Good\n",
    );
    let envelope = run_envelope(nodex(tmp.path()).arg("build"));
    // Build must succeed; the good doc enters the graph, the broken
    // doc surfaces as a warning.
    assert_eq!(
        envelope.pointer("/data/nodes").and_then(Value::as_u64),
        Some(1),
        "only the well-formed doc must appear in the graph: {envelope}"
    );
    let warnings: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("envelope-level warnings array")
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("parse failed")
            && (w.contains("docs/broken.md") || w.contains("docs\\broken.md"))),
        "parse failure must name the failing file at envelope level: {warnings:?}"
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

    let active = run_json(nodex(tmp.path()).args(["query", "trust", "doc-active"]));
    let archived = run_json(nodex(tmp.path()).args(["query", "trust", "doc-archived"]));

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

    let data = run_json(nodex(tmp.path()).args(["query", "similar", "--id", "doc-a"]));
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
    let data =
        run_json(nodex(tmp.path()).args(["query", "recent", "--days", "7", "--field", "updated"]));
    let items = data.get("items").and_then(Value::as_array).expect("items");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(ids, vec!["doc-recent"]);
    assert_eq!(data.get("total").and_then(Value::as_u64), Some(1));
}

#[test]
fn check_version_passes_when_matches_and_fails_otherwise() {
    let tmp = scratch();
    init_project(tmp.path());

    // A wildcard requirement always matches the binary's own version.
    let envelope = run_envelope(nodex(tmp.path()).args(["--check-version", "*", "build"]));
    assert_eq!(envelope.get("ok"), Some(&Value::Bool(true)));

    // An impossible upper bound forces a mismatch — VERSION_MISMATCH
    // surfaces with exit 2.
    let output = nodex(tmp.path())
        .args(["--check-version", "<0.0.1", "build"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("VERSION_MISMATCH")
    );
}

#[test]
fn export_emits_schema_and_enums_manifests() {
    let tmp = scratch();
    init_project(tmp.path());

    let schema = run_json(nodex(tmp.path()).args(["export", "schema"]));
    assert_eq!(
        schema.get("$schema").and_then(Value::as_str),
        Some("https://json-schema.org/draft/2020-12/schema")
    );

    let enums = run_json(nodex(tmp.path()).args(["export", "enums"]));
    let kinds: Vec<&str> = enums
        .get("kinds")
        .and_then(Value::as_array)
        .expect("kinds")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(kinds.contains(&"generic"));
}

#[test]
fn query_components_partitions_disconnected_subgraphs() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n[b](docs/b.md)\n",
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
    let data = run_json(nodex(tmp.path()).args(["query", "components"]));
    let items = data.get("items").and_then(Value::as_array).expect("items");
    // Largest first: {a,b} (size 2), then {c} (size 1).
    assert_eq!(items[0].get("size").and_then(Value::as_u64), Some(2));
    assert_eq!(items[1].get("size").and_then(Value::as_u64), Some(1));
}

#[test]
fn query_neighborhood_expands_by_depth() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n[b](docs/b.md)\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n[c](docs/c.md)\n",
    );
    write_doc(
        tmp.path(),
        "docs/c.md",
        "---\nid: doc-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let d1 = run_json(nodex(tmp.path()).args(["query", "neighborhood", "doc-a", "--depth", "1"]));
    let ids_d1: Vec<&str> = d1
        .get("nodes")
        .and_then(Value::as_array)
        .expect("nodes")
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .collect();
    assert!(ids_d1.contains(&"doc-a"));
    assert!(ids_d1.contains(&"doc-b"));
    assert!(!ids_d1.contains(&"doc-c"));

    let d2 = run_json(nodex(tmp.path()).args(["query", "neighborhood", "doc-a", "--depth", "2"]));
    let ids_d2: Vec<&str> = d2
        .get("nodes")
        .and_then(Value::as_array)
        .expect("nodes")
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .collect();
    assert!(ids_d2.contains(&"doc-c"));
}

#[test]
fn check_envelope_always_lists_skipped_rules() {
    // The "no silent rule skips" doctrine: every `check` response — even
    // a green one with zero violations — must include `skipped_rules`,
    // so a consumer can never confuse "rule passed" with "rule never
    // ran". `frontmatter_immutable` is the natural witness: it's
    // unconfigured by default and therefore skipped.
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["check"]));
    let skipped = data
        .get("skipped_rules")
        .and_then(Value::as_array)
        .expect("skipped_rules array must be present");
    let rule_ids: Vec<&str> = skipped
        .iter()
        .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
        .collect();
    assert!(
        rule_ids.contains(&"frontmatter_immutable"),
        "frontmatter_immutable must self-report as skipped when unconfigured: {skipped:?}"
    );
}

#[test]
fn strict_schema_mode_rejects_unknown_fields_end_to_end() {
    // `[schema].mode = "strict"` end-to-end: a frontmatter typo
    // (`relatd:` instead of `related:`) must surface as an envelope
    // violation through `check`, not silently land in `attrs`.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"

[schema]
mode = "strict"
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nrelatd: doc-b\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path()).args(["check"]).output().expect("ran");
    // Strict-mode violation is an error → exit 1.
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    let violations = envelope
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str) == Some("field_unknown")
                && v.get("message")
                    .and_then(Value::as_str)
                    .map(|m| m.contains("\"relatd\""))
                    .unwrap_or(false)
        }),
        "strict mode must flag `relatd:` typo as field_unknown: {violations:?}"
    );
}

#[test]
fn diff_outside_git_work_tree_errors_cleanly() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
        .expect("ran");
    // Not a git work tree → GIT_ERROR with exit 2 (semantically distinct
    // from CONFIG_ERROR, which is reserved for `nodex.toml` problems).
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("GIT_ERROR")
    );
}

#[test]
fn check_since_activates_frontmatter_immutable_rule() {
    // End-to-end witness for the diff-aware rule path:
    // 1. commit a terminal-status node with `superseded_by: old`
    // 2. edit the locked field in the work tree (no new commit)
    // 3. `check --since HEAD` must surface the violation
    //
    // The rule must self-report as APPLIED (not skipped) under
    // `--since`, and must STAY skipped under plain `check`.
    let tmp = scratch();
    let root = tmp.path();

    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"

[rules.frontmatter_immutable]
fields = ["superseded_by"]
"#,
    )
    .unwrap();

    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git ran")
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);

    // Two docs: a successor (active) and a predecessor (terminal, locked
    // `superseded_by`).
    write_doc(
        root,
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);

    // Edit the locked field in the work tree — no new commit; this is
    // exactly the surface `check --since` guards.
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-tampered\n---\n# Old\n",
    );

    nodex(root).arg("build").assert().success();

    // Without `--since` the rule is inert; it self-reports as skipped.
    let plain = run_json(nodex(root).args(["check"]));
    let skipped: Vec<&str> = plain
        .get("skipped_rules")
        .and_then(Value::as_array)
        .expect("skipped_rules")
        .iter()
        .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
        .collect();
    assert!(
        skipped.contains(&"frontmatter_immutable"),
        "plain check must list frontmatter_immutable as skipped: {plain}"
    );

    // With `--since HEAD` the rule activates and surfaces the violation
    // (exit 1 because the rule's severity is error).
    let output = nodex(root)
        .args(["check", "--since", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let env: Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str) == Some("frontmatter_immutable")
                && v.get("node_id").and_then(Value::as_str) == Some("doc-old")
        }),
        "check --since must surface frontmatter_immutable on doc-old: {violations:?}"
    );

    // And the rule must NOT also appear in skipped under `--since`.
    let skipped_under_since: Vec<&str> = env
        .pointer("/data/skipped_rules")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !skipped_under_since.contains(&"frontmatter_immutable"),
        "frontmatter_immutable must not be skipped when --since is supplied: {env}"
    );
}

#[test]
fn diff_reports_added_node_between_two_commits() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);

    // Initialise a real git repo so worktree add works.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git ran")
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);

    write_doc(
        root,
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);

    write_doc(
        root,
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "add b"]);

    let data = run_json(nodex(root).args(["diff", "HEAD~1", "HEAD"]));
    let added: Vec<&str> = data
        .get("added_nodes")
        .and_then(Value::as_array)
        .expect("added_nodes")
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(added, vec!["doc-b"]);
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

// ─── query annotations ──────────────────────────────────────────────

/// Append a `[[annotations]]` declaration to the project's nodex.toml.
/// Declares a `promotes` pattern with a `(?P<id>...)` capture so
/// tests stay focused on the CLI envelope contract rather than
/// reciting full configs.
fn append_annotations_block(root: &std::path::Path) {
    let path = root.join("nodex.toml");
    let mut content = fs::read_to_string(&path).expect("nodex.toml");
    // TOML single-quoted (literal) strings treat `\` literally — one
    // `\` in the Rust source becomes one `\` in the TOML file becomes
    // one `\` in the regex.
    content.push_str(
        "\n[[annotations]]\nname = \"promotes\"\npattern = '\\[PROMOTES:\\s*(?P<id>[\\w-]+)\\]'\nkey = \"id\"\n",
    );
    fs::write(&path, content).expect("nodex.toml writable");
}

#[test]
fn query_annotations_groups_by_pattern_and_key() {
    let tmp = scratch();
    init_project(tmp.path());
    append_annotations_block(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n\nrefer to [PROMOTES: spec-x] and [PROMOTES: spec-y]\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n\nalso [PROMOTES: spec-x] here\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["query", "annotations"]));
    let items = data["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "one pattern declared");
    assert_eq!(items[0]["name"], "promotes");
    let entries = items[0]["entries"].as_array().expect("entries array");
    let spec_x = entries
        .iter()
        .find(|e| e["key"] == "spec-x")
        .expect("spec-x entry present");
    assert_eq!(spec_x["count"].as_u64(), Some(2));
    assert_eq!(spec_x["sources"].as_array().map(Vec::len), Some(2));
}

#[test]
fn query_annotations_unknown_filter_emits_typed_config_error() {
    let tmp = scratch();
    init_project(tmp.path());
    append_annotations_block(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["query", "annotations", "--name", "promtes"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "typo must fail exit code");
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("promtes"),
        "error must echo the offending value"
    );
}

// ─── check body_line ────────────────────────────────────────────────

#[test]
fn check_fires_body_line_violation_with_qualified_rule_id() {
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("nodex.toml");
    let mut content = fs::read_to_string(&path).expect("nodex.toml");
    content.push_str(
        "\n[[rules.body_line]]\nname = \"decision-log\"\npattern = '^- \\*\\*(?P<gate>[a-z-]+)\\*\\*'\nenums.gate = [\"scope\", \"design\", \"ship\"]\n",
    );
    fs::write(&path, content).expect("nodex.toml writable");

    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n\n- **scope**: ok\n- **bogus**: typo\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path()).arg("check").output().expect("ran");
    assert_eq!(output.status.code(), Some(1), "violations → exit 1");
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    let violations = env["data"]["violations"]
        .as_array()
        .expect("violations array");
    assert_eq!(violations.len(), 1, "exactly one bogus capture value");
    assert_eq!(violations[0]["rule_id"], "body_line/decision-log");
    assert!(
        violations[0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("bogus"),
        "violation must echo the offending value"
    );
}

