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
    parsed.get("data").cloned().unwrap_or(Value::Null)
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
fn lifecycle_supersede_roundtrips_through_yaml() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
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
    // YAML must still parse; status and superseded_by updated.
    let content = fs::read_to_string(tmp.path().join("docs/old.md")).unwrap();
    assert!(content.contains("status: superseded"));
    assert!(content.contains("superseded_by: doc-new"));
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
    // All three commands used to panic on NaiveDate - Duration overflow.
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
