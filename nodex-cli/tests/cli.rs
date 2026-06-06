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

#[test]
fn build_warns_on_dead_scope_declarations() {
    // A populated project whose include glob points at a directory with
    // no docs gets a non-fatal coverage warning — the silent-config-drift
    // class that would otherwise leave a whole area unindexed unnoticed.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\", \"specs/**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );

    let envelope = run_envelope(nodex(tmp.path()).arg("build"));
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings present for dead include glob");
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|s| s.contains("specs/**/*.md") && s.contains("no files"))),
        "dead include glob must be flagged: {warnings:?}"
    );
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
fn query_trust_bottom_lists_lowest_first() {
    // Two docs: a fresh active one (composite ≈ 1.0) and an archived
    // one (status = 0 → composite well below 1.0). `--bottom 5` must
    // rank the archived doc ahead of the fresh one.
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
    let data = run_json(nodex(tmp.path()).args(["query", "trust", "--bottom", "5"]));
    let ids: Vec<&str> = data
        .get("items")
        .and_then(serde_json::Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(serde_json::Value::as_str))
        .collect();
    let dead_pos = ids
        .iter()
        .position(|i| *i == "doc-dead")
        .expect("archived doc must surface");
    let fresh_pos = ids
        .iter()
        .position(|i| *i == "doc-fresh")
        .expect("fresh doc must appear without a cutoff");
    assert!(
        dead_pos < fresh_pos,
        "archived doc must outrank fresh in --bottom listing; got {ids:?}"
    );
}

#[test]
fn query_trust_top_lists_highest_first() {
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
    let data = run_json(nodex(tmp.path()).args(["query", "trust", "--top", "5"]));
    let ids: Vec<&str> = data
        .get("items")
        .and_then(serde_json::Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(serde_json::Value::as_str))
        .collect();
    let fresh_pos = ids.iter().position(|i| *i == "doc-fresh").expect("fresh");
    let dead_pos = ids.iter().position(|i| *i == "doc-dead").expect("dead");
    assert!(
        fresh_pos < dead_pos,
        "fresh doc must outrank archived in --top listing; got {ids:?}"
    );
}

#[test]
fn query_trust_bottom_below_filters_by_score() {
    // Archived doc (status=0) surfaces below 1.0. Fresh doc lands at
    // 1.0 and the strict `< 1.0` cutoff drops it.
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
    let data =
        run_json(nodex(tmp.path()).args(["query", "trust", "--bottom", "5", "--below", "1.0"]));
    let ids: Vec<&str> = data
        .get("items")
        .and_then(serde_json::Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        ids.contains(&"doc-dead"),
        "archived doc must remain under cutoff; got {ids:?}"
    );
    assert!(
        !ids.contains(&"doc-fresh"),
        "fresh doc must be dropped by strict --below 1.0; got {ids:?}"
    );
}

#[test]
fn query_trust_bottom_entries_always_carry_components() {
    // Every entry returned by `query trust --bottom N` must include
    // the per-component breakdown for components that have a signal —
    // freshness, drift, and backlinks are omitted when their source
    // signal is absent (honest absence, never fabricated). Only
    // `status` is guaranteed because it is derived from the node's
    // own frontmatter and the config's `statuses.terminal` set, both
    // of which exist for every node.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "trust", "--bottom", "5"]));
    let items = data.get("items").and_then(Value::as_array).expect("items");
    assert!(!items.is_empty(), "fixture must produce at least one entry");
    for item in items {
        assert!(
            item.pointer("/components/status").is_some(),
            "components.status missing on {item}"
        );
    }
}

#[test]
fn query_trust_single_id_still_returns_one_report() {
    // The `<id>` form is the regression anchor: it shares a clap
    // group with `--bottom` / `--top` so a refactor mistake could
    // accidentally break the single-node case.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "trust", "doc-a"]));
    assert_eq!(
        data.get("id").and_then(Value::as_str),
        Some("doc-a"),
        "single-node form must return the report inline; got {data}"
    );
    assert!(
        data.pointer("/components/status").is_some(),
        "single-node form must carry components/status"
    );
}

#[test]
fn query_trust_rejects_id_with_bottom() {
    // clap's ArgGroup must reject the combination at parse time —
    // the listing form and the single-node form are not composable.
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "trust", "doc-a", "--bottom", "5"])
        .output()
        .expect("ran");
    assert!(
        !output.status.success(),
        "<id> and --bottom must be mutually exclusive"
    );
}

#[test]
fn query_trust_bottom_kind_filter_restricts_listing() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: guide\nstatus: archived\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data =
        run_json(nodex(tmp.path()).args(["query", "trust", "--bottom", "5", "--kind", "generic"]));
    let ids: Vec<&str> = data
        .get("items")
        .and_then(serde_json::Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(serde_json::Value::as_str))
        .collect();
    assert_eq!(ids, vec!["doc-a"]);
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
fn query_node_by_path_returns_same_envelope_as_by_id() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let by_id = run_json(nodex(tmp.path()).args(["query", "node", "doc-a"]));
    let by_path = run_json(nodex(tmp.path()).args(["query", "node", "--path", "docs/a.md"]));
    assert_eq!(
        by_id, by_path,
        "--path must yield the same envelope as <id>"
    );
}

#[test]
fn query_node_normalises_dot_slash_prefix() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let canonical = run_json(nodex(tmp.path()).args(["query", "node", "--path", "docs/a.md"]));
    let with_dot = run_json(nodex(tmp.path()).args(["query", "node", "--path", "./docs/a.md"]));
    assert_eq!(canonical, with_dot, "./prefix must normalise to bare form");
}

#[test]
fn query_node_normalises_absolute_path_under_root() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let abs = tmp.path().join("docs/a.md");
    let abs_str = abs.to_str().expect("utf-8 path");
    let by_rel = run_json(nodex(tmp.path()).args(["query", "node", "--path", "docs/a.md"]));
    let by_abs = run_json(nodex(tmp.path()).args(["query", "node", "--path", abs_str]));
    assert_eq!(
        by_rel, by_abs,
        "absolute path under root must normalise to project-relative"
    );
}

#[test]
fn query_node_absolute_path_outside_root_is_rejected() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "node", "--path", "/etc/passwd"])
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
fn query_node_parent_dir_traversal_is_rejected() {
    // Symmetric guard with `scaffold` / `rename` / `migrate` — every
    // command that takes a user-supplied path rejects `..` traversal
    // with PATH_ESCAPES_ROOT, never a misleading NOT_FOUND for a
    // node that was never addressable.
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "node", "--path", "../foo.md"])
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
fn query_node_unknown_path_emits_not_found_code() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "node", "--path", "docs/does-not-exist.md"])
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
fn query_node_rejects_both_id_and_path_set() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "node", "doc-a", "--path", "docs/a.md"])
        .output()
        .expect("ran");
    // clap's required/!multiple ArgGroup rejects this; exit code 2.
    assert!(!output.status.success(), "mutually exclusive must fail");
}

#[test]
fn query_node_rejects_neither_id_nor_path() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "node"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "required ArgGroup must fail");
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
[detection]
orphan_ok_kinds = ["generic"]
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
            let cause = e.get("cause").and_then(Value::as_str)?;
            Some((target, cause))
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

    // The excluded link points at a real file, so it is informational —
    // counted under `excluded_target`, kept out of `total`, which
    // reflects only the genuinely-broken `missing.md` link.
    let summary = data.get("summary").expect("summary");
    let by_category = summary.get("by_category").expect("by_category");
    assert_eq!(
        by_category.get("unresolved_edge").and_then(Value::as_u64),
        Some(1),
        "only the missing link is a broken edge: {summary}"
    );
    assert_eq!(
        by_category.get("excluded_target").and_then(Value::as_u64),
        Some(1),
        "the excluded link is surfaced informationally: {summary}"
    );
    assert_eq!(
        summary.get("total").and_then(Value::as_u64),
        Some(1),
        "excluded-target link must not inflate the issue total: {summary}"
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
fn query_trust_listing_rejects_unknown_kind() {
    // Symmetric with `query similar --kind`, `query nodes --kind`,
    // `query recent --kind`: typo → CONFIG_ERROR, never a silent
    // empty list.
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "trust", "--bottom", "5", "--kind", "ghost-kind"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn query_trust_bottom_zero_rejected() {
    // Symmetric with `query nodes --limit 0`: a zero cap silently
    // empties the result, which the operator never asked for. Fail
    // fast at the CLI with CONFIG_ERROR.
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "trust", "--bottom", "0"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("--bottom"),
        "error message must name the offending flag; got {msg:?}"
    );
}

#[test]
fn query_trust_top_zero_rejected() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "trust", "--top", "0"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("--top"),
        "error message must name the offending flag; got {msg:?}"
    );
}

#[test]
fn query_trust_below_nan_rejected() {
    // `f64::parse` accepts "NaN" / "inf"; the CLI must reject
    // non-finite cutoffs because NaN filters everything and Infinity
    // produces all-or-none cutoffs — never what the operator meant.
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "trust", "--bottom", "5", "--below", "NaN"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("--below") && msg.contains("finite"),
        "error message must explain the finite-number requirement; got {msg:?}"
    );
}

#[test]
fn query_trust_below_infinity_rejected() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "trust", "--bottom", "5", "--below", "inf"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn query_similar_limit_zero_rejected() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "similar", "--id", "doc-a", "--limit", "0"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("--limit"),
        "error message must name the offending flag; got {msg:?}"
    );
}

#[test]
fn query_similar_min_score_infinity_rejected() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "similar", "--id", "doc-a", "--min-score", "inf"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("--min-score") && msg.contains("finite"),
        "error message must explain the finite-number requirement; got {msg:?}"
    );
}

#[test]
fn query_similar_min_score_nan_rejected() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "similar", "--id", "doc-a", "--min-score", "NaN"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn query_recent_rejects_unknown_kind() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "recent", "--kind", "ghost-kind"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn query_covered_by_normalises_dot_slash_prefix() {
    // Symmetric with `query node --path`: `./` prefix folds before
    // the matcher sees the path.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\ncovers: [src/lib.rs]\n---\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let bare = run_json(nodex(tmp.path()).args(["query", "covered-by", "src/lib.rs"]));
    let with_dot = run_json(nodex(tmp.path()).args(["query", "covered-by", "./src/lib.rs"]));
    assert_eq!(bare, with_dot, "./prefix must normalise");
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
#[cfg(unix)]
fn rename_rejects_destination_through_symlinked_directory() {
    // A lexically clean destination (`linked-dir/evil.md`) whose
    // ancestor is a symlink pointing outside the project root must be
    // refused before the move — `reject_traversal` cannot see this,
    // `reject_outside_root` must.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    unix_fs::symlink(outside.path(), tmp.path().join("linked-dir")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["rename", "a.md", "linked-dir/evil.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "must reject escaping destination");
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
    // Source untouched, nothing materialised outside the root.
    assert!(tmp.path().join("a.md").exists(), "source must remain");
    assert!(
        !outside.path().join("evil.md").exists(),
        "no file may appear outside the project root"
    );
}

#[test]
#[cfg(unix)]
fn rename_rejects_source_through_symlinked_directory() {
    // The mirror of the destination guard: a SOURCE reached through a
    // symlinked ancestor would let `fs::rename` pull an out-of-root
    // file into the project (exfiltration). Both the bare and the
    // pinned-id source shapes must be refused — neither passes through
    // a write that would incidentally guard them.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    fs::write(
        outside.path().join("secret.md"),
        "TOP SECRET out-of-root content\n",
    )
    .unwrap();
    fs::write(
        outside.path().join("pinned.md"),
        "---\nid: pinned\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    unix_fs::symlink(outside.path(), tmp.path().join("docs").join("ext")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    for source in ["docs/ext/secret.md", "docs/ext/pinned.md"] {
        let output = nodex(tmp.path())
            .args(["rename", source, "docs/pulled-in.md"])
            .output()
            .expect("ran");
        assert!(!output.status.success(), "escaping source must be refused");
        let parsed: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
        assert_eq!(
            parsed.pointer("/error/code").and_then(Value::as_str),
            Some("PATH_ESCAPES_ROOT"),
            "source {source}"
        );
        assert!(
            !tmp.path().join("docs/pulled-in.md").exists(),
            "out-of-root file must not be pulled into the project"
        );
    }
    assert!(
        outside.path().join("secret.md").exists() && outside.path().join("pinned.md").exists(),
        "out-of-root files must remain where they are"
    );
}

#[test]
fn rename_cross_dir_moved_file_with_self_and_outbound_links() {
    // The moved file carries BOTH a self-reference (a link to its own
    // old path) and an outbound relative link to another file. A
    // cross-directory move must, in one read+write, repoint the
    // self-reference (pass 1) AND rebase the outbound link to the new
    // vantage point (pass 2) — exercising the `.or(pass1)` compose.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a/b/x.md",
        "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n\
         self [me](x.md) and out [auth](../../t/auth.md)\n",
    );
    write_doc(
        tmp.path(),
        "t/auth.md",
        "---\nid: auth\ntitle: Auth\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a/b/x.md", "a/x.md"])
        .assert()
        .success();

    let moved = fs::read_to_string(tmp.path().join("a/x.md")).unwrap();
    assert!(
        moved.contains("[me](x.md)"),
        "self-reference stays the same stem after the move: {moved}"
    );
    assert!(
        moved.contains("[auth](../t/auth.md)"),
        "outbound relative link rebased to the new directory: {moved}"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let issues = run_json(nodex(tmp.path()).args(["query", "issues"]));
    assert_eq!(
        issues["unresolved_edges"].as_array().map(Vec::len),
        Some(0),
        "both rewritten links must resolve in the rebuilt graph: {issues}"
    );
}

#[test]
fn rename_rewrites_custom_pattern_inbound_reference() {
    // A `[[parser.link_patterns]]` reference in another file must be
    // repointed by rename, exactly like a markdown link — the rewriter
    // shares the same candidate ladder.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [[parser.link_patterns]]\npattern = '@import\\s+(\\S+)'\nrelation = \"imports\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n@import a.md here\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "a.md", "c.md"]));
    assert!(
        data["references_updated"]
            .as_array()
            .map(|a| a.iter().any(|p| p == "b.md"))
            .unwrap_or(false),
        "custom-pattern inbound reference must be repointed: {data}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("b.md"))
            .unwrap()
            .contains("@import c.md here"),
        "the custom-pattern target is rewritten to the new path"
    );
}

#[test]
fn rename_terminal_parent_under_conditional_exclude_resolves_against_real_pre_move_scope() {
    // `conditional_exclude` makes scope location-dependent: a terminal
    // parent excludes its directory siblings. Renaming that parent
    // changes which files are in scope. The inbound rewrite must
    // resolve against the *real* pre-move scope (scanned before the
    // move), not a set fabricated from the post-move scan — otherwise a
    // sibling that only re-enters scope after the move could corrupt
    // resolution.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"docs/feat/SPEC.md\"\ncondition = \"status_terminal\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         terminal = [\"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    // Terminal parent → its sibling `notes.md` is excluded from scope.
    write_doc(
        tmp.path(),
        "docs/feat/SPEC.md",
        "---\nid: spec\ntitle: Spec\nkind: generic\nstatus: superseded\nsuperseded_by: ext\n---\n# Spec\n",
    );
    write_doc(
        tmp.path(),
        "docs/feat/notes.md",
        "scratch notes, out of scope\n",
    );
    // An in-scope file links the terminal parent.
    write_doc(
        tmp.path(),
        "ext.md",
        "---\nid: ext\ntitle: Ext\nkind: generic\nstatus: active\n---\n# Ext\n[s](docs/feat/SPEC.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "docs/feat/SPEC.md", "docs/archived-spec.md"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(tmp.path().join("ext.md"))
            .unwrap()
            .contains("[s](docs/archived-spec.md)"),
        "inbound link to the moved terminal parent must be repointed"
    );
}

#[test]
fn rename_rewrites_titled_and_pointy_markdown_links() {
    // The rewriter extracts markdown destinations via the same
    // pulldown parser as the builder, so titled (`(url "t")`) and
    // pointy (`(<url>)`) inline-link forms — which a regex matcher
    // misses — are repointed, preserving the title and brackets.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n\
         [t](a.md \"the title\") and [p](<a.md>) and [plain](a.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a.md", "c.md"])
        .assert()
        .success();
    let b = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(b.contains("[t](c.md \"the title\")"), "titled form: {b}");
    assert!(b.contains("[p](<c.md>)"), "pointy form: {b}");
    assert!(b.contains("[plain](c.md)"), "plain form: {b}");
}

#[test]
fn rename_repoints_reference_style_link_definitions() {
    // Reference / collapsed / shortcut markdown links carry their URL
    // in a `[label]: url` definition line. The builder binds an edge
    // for each use; the rewriter repoints the single definition so all
    // uses stay resolved. A move must leave no dangling edge.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n\
         See [full][r] and [coll][] and [short].\n\n\
         [r]: old.md\n[coll]: old.md\n[short]: old.md\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "old.md", "new.md"]));
    assert!(
        data["references_updated"]
            .as_array()
            .map(|a| a.iter().any(|p| p == "b.md"))
            .unwrap_or(false),
        "reference-style definitions must be repointed: {data}"
    );
    let b = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(
        !b.contains("old.md"),
        "no definition may still point at old: {b}"
    );
    assert_eq!(
        b.matches("new.md").count(),
        3,
        "all three definitions repointed: {b}"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let issues = run_json(nodex(tmp.path()).args(["query", "issues"]));
    assert_eq!(
        issues["unresolved_edges"].as_array().map(Vec::len),
        Some(0),
        "no edge may dangle after the move: {issues}"
    );
}

#[test]
fn rename_leaves_extensionless_markdown_link_untouched() {
    // A standard markdown link needs a configured extension to be a
    // graph edge (the builder's process_link_target guard). `[x](old)`
    // is not an edge, so rename must leave it byte-unchanged — only the
    // extension-bearing `[y](old.md)` is repointed.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n\
         bare [x](old) and full [y](old.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "old.md", "new.md"])
        .assert()
        .success();
    let b = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(
        b.contains("[x](old)"),
        "extensionless link must be untouched: {b}"
    );
    assert!(
        b.contains("[y](new.md)"),
        "extension-bearing link repointed: {b}"
    );
}

#[test]
fn retarget_leaves_wikilink_that_binds_a_file_by_path() {
    // `[[old]]` next to a file `old.md` is a path edge to that file
    // (resolver path-first), not an id reference. `retarget` repoints
    // ids only, so it must leave the path-bound wikilink alone even
    // when its text equals the retargeted id.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [parser]\nwikilink_enabled = true\nextensions = [\".md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"doc-{stem}\"\n",
    )
    .unwrap();
    // File old.md → node id doc-old; a separate node carries the bare id `old`.
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: doc-old\ntitle: F\nkind: generic\nstatus: active\n---\n# F\n",
    );
    write_doc(
        tmp.path(),
        "node.md",
        "---\nid: old\ntitle: Node\nkind: generic\nstatus: active\n---\n# Node\n",
    );
    write_doc(
        tmp.path(),
        "succ.md",
        "---\nid: succ\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    let b = "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n[[old]] here\n";
    write_doc(tmp.path(), "b.md", b);
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["retarget", "old", "succ"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("b.md")).unwrap(),
        b,
        "a path-bound wikilink must survive id retargeting byte-identical"
    );
}

#[test]
fn rename_repoints_reference_link_with_case_divergent_label() {
    // CommonMark matches reference labels case-insensitively, so
    // `[x][REF]` binds `[ref]: old.md` as a build edge. Rename must
    // repoint that definition despite the casing mismatch — otherwise
    // the edge silently dangles.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\nSee [x][REF].\n\n[ref]: old.md\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let pre = run_json(nodex(tmp.path()).args(["query", "backlinks", "a"]));
    assert_eq!(
        pre["total"].as_u64(),
        Some(1),
        "case-divergent ref is an edge: {pre}"
    );

    let data = run_json(nodex(tmp.path()).args(["rename", "old.md", "new.md"]));
    assert!(
        data["references_updated"]
            .as_array()
            .map(|a| a.iter().any(|p| p == "b.md"))
            .unwrap_or(false),
        "case-divergent reference definition must be repointed: {data}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("b.md"))
            .unwrap()
            .contains("[ref]: new.md"),
        "definition repointed"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let issues = run_json(nodex(tmp.path()).args(["query", "issues"]));
    assert_eq!(
        issues["unresolved_edges"].as_array().map(Vec::len),
        Some(0),
        "no edge may dangle after the rename: {issues}"
    );
}

#[test]
fn retarget_leaves_covers_relation_captures_untouched() {
    // `covers` references name out-of-graph code paths, never node ids.
    // `retarget` repoints id references only, so a `@covers <id>`
    // capture must be left alone while a real id wikilink is retargeted.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = '@covers (\\S+)'\nrelation = \"covers\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: doc-old\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    write_doc(
        tmp.path(),
        "new.md",
        "---\nid: doc-new\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n@covers doc-old\nalso [[doc-old]]\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["retarget", "doc-old", "doc-new"])
        .assert()
        .success();
    let b = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(
        b.contains("@covers doc-old"),
        "covers capture must be untouched: {b}"
    );
    assert!(
        b.contains("[[doc-new]]"),
        "real id reference must be retargeted: {b}"
    );
}

#[test]
fn rename_anchors_id_in_crlf_frontmatter_document() {
    // A CRLF-delimited frontmatter document must be canonicalised the
    // same way the build parses it — otherwise rename mis-reads it as
    // bare and skips id anchoring. With an explicit `id:` the move is
    // already anchored (not a bare-file warning).
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("docs").join("crlf.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "---\r\nid: doc\r\ntitle: D\r\nkind: generic\r\nstatus: active\r\n---\r\n# D\r\n",
    )
    .unwrap();
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "docs/crlf.md", "docs/renamed.md"]));
    assert_eq!(
        data.pointer("/id_stability/kind").and_then(Value::as_str),
        Some("already_anchored"),
        "CRLF frontmatter must be seen, not mis-read as bare: {data}"
    );
}

#[test]
fn rename_repoints_link_in_file_evicted_from_scope_by_the_move() {
    // `conditional_exclude` makes scope move-dependent: moving a file
    // can turn a sibling directory into an excluded one. A file that
    // drops out of post-move scope still holds a real pre-move edge to
    // the renamed file — the rewrite must visit it (the iteration set
    // is the union of pre- and post-move scope) and repoint its link.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"work/SPEC.md\"\ncondition = \"status_terminal\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         terminal = [\"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "staging/SPEC.md",
        "---\nid: spec\ntitle: S\nkind: generic\nstatus: superseded\nsuperseded_by: peer\n---\n# S\n",
    );
    write_doc(
        tmp.path(),
        "work/peer.md",
        "---\nid: peer\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n[s](../staging/SPEC.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "staging/SPEC.md", "work/SPEC.md"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(tmp.path().join("work/peer.md"))
            .unwrap()
            .contains("[s](SPEC.md)"),
        "the evicted file's pre-move edge must be repointed, not left dangling"
    );
}

#[test]
fn rename_does_not_repoint_link_bound_to_a_different_file() {
    // End-to-end resolver-disagreement regression: s.md's
    // `[x](shared.md)` binds the ROOT shared.md (literal-first).
    // Renaming docs/sub/shared.md must leave it untouched and the
    // root file's backlink intact.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "shared.md",
        "---\nid: shared-root\ntitle: Root\nkind: generic\nstatus: active\n---\n# Root\n",
    );
    write_doc(
        tmp.path(),
        "docs/sub/shared.md",
        "---\nid: shared-sub\ntitle: Sub\nkind: generic\nstatus: active\n---\n# Sub\n",
    );
    let s_content =
        "---\nid: s\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n[x](shared.md)\n";
    write_doc(tmp.path(), "docs/sub/s.md", s_content);
    nodex(tmp.path()).arg("build").assert().success();

    nodex(tmp.path())
        .args(["rename", "docs/sub/shared.md", "docs/sub/renamed.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("docs/sub/s.md")).unwrap(),
        s_content,
        "link bound to the root file must survive byte-identical"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let backlinks = run_json(nodex(tmp.path()).args(["query", "backlinks", "shared-root"]));
    assert_eq!(
        backlinks["total"].as_u64(),
        Some(1),
        "the root file's incoming edge must survive the rename: {backlinks}"
    );
}

#[test]
fn rename_does_not_repoint_wikilink_bound_to_a_bare_sibling() {
    // Extension-append resolver disagreement: `[[shared]]` binds the
    // bare extension-less `wiki/shared` (first candidate), not
    // `wiki/shared.md`. Renaming the `.md` file must leave the wikilink
    // pointing at the bare sibling — its backlink must survive.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\", \"**/wiki/*\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [parser]\nwikilink_enabled = true\nextensions = [\".md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "wiki/shared",
        "---\nid: bare-shadow\ntitle: Bare\nkind: generic\nstatus: active\n---\n# bare\n",
    );
    write_doc(
        tmp.path(),
        "wiki/shared.md",
        "---\nid: sub-shared\ntitle: SubShared\nkind: generic\nstatus: active\n---\n# sub\n",
    );
    let ref_content =
        "---\nid: linker\ntitle: L\nkind: generic\nstatus: active\n---\n# L\n[[shared]]\n";
    write_doc(tmp.path(), "wiki/ref.md", ref_content);
    nodex(tmp.path()).arg("build").assert().success();
    // Pre-condition: the wikilink binds the bare file, not the .md.
    let pre = run_json(nodex(tmp.path()).args(["query", "backlinks", "bare-shadow"]));
    assert_eq!(
        pre["total"].as_u64(),
        Some(1),
        "wikilink binds the bare file: {pre}"
    );

    nodex(tmp.path())
        .args(["rename", "wiki/shared.md", "wiki/renamed.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("wiki/ref.md")).unwrap(),
        ref_content,
        "wikilink bound to the bare sibling must survive byte-identical"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let post = run_json(nodex(tmp.path()).args(["query", "backlinks", "bare-shadow"]));
    assert_eq!(
        post["total"].as_u64(),
        Some(1),
        "the bare file's incoming edge must survive the .md rename: {post}"
    );
}

#[test]
#[cfg(unix)]
fn migrate_warns_on_bare_symlinked_file() {
    // Writer-skips / reader-follows symmetry: migrate skips symlinks
    // like rename/retarget do, but a *bare* symlinked file is exactly
    // what migrate exists to fix — the skip must surface as a warning
    // naming the file, and the external target must stay untouched.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    let external = outside.path().join("bare.md");
    fs::write(&external, "# Bare external doc\n").unwrap();
    let before = fs::read_to_string(&external).unwrap();
    unix_fs::symlink(&external, tmp.path().join("linked.md")).unwrap();

    let envelope = run_envelope(nodex(tmp.path()).args(["migrate", "--apply"]));
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("bare symlinked file must produce a warning");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("linked.md"))),
        "warning must name the skipped file: {warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(&external).unwrap(),
        before,
        "external target must not receive frontmatter through the symlink"
    );
}

#[test]
#[cfg(unix)]
fn migrate_tolerates_dangling_symlink_and_silences_anchored_ones() {
    // A dangling symlink in scope must neither abort the batch nor
    // warn (it is not a migration target); a symlinked file that
    // already carries frontmatter is equally not a target and stays
    // silent — only *bare* symlinked files warn.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    unix_fs::symlink(
        tmp.path().join("ghost-target.md"),
        tmp.path().join("ghost.md"),
    )
    .unwrap();
    let anchored = outside.path().join("anchored.md");
    fs::write(
        &anchored,
        "---\nid: anchored\ntitle: Anchored\nkind: generic\nstatus: active\n---\n# Anchored\n",
    )
    .unwrap();
    unix_fs::symlink(&anchored, tmp.path().join("anchored-link.md")).unwrap();

    let envelope = run_envelope(nodex(tmp.path()).args(["migrate", "--apply"]));
    if let Some(warnings) = envelope.get("warnings").and_then(Value::as_array) {
        assert!(
            !warnings.iter().any(|w| w
                .as_str()
                .is_some_and(|s| s.contains("ghost.md") || s.contains("anchored-link.md"))),
            "dangling / frontmatter-carrying symlinks must not warn: {warnings:?}"
        );
    }
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
fn rename_rebases_moved_file_relative_references() {
    // Cross-directory move: the moved file's OWN file-relative links
    // were written from the old directory's vantage point and must be
    // recomputed from the new one — otherwise they silently dangle.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a/b/x.md",
        "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n\
         [auth](../../t/auth.md)\n",
    );
    write_doc(
        tmp.path(),
        "t/auth.md",
        "---\nid: auth\ntitle: Auth\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "a/b/x.md", "a/x.md"]));

    let moved = fs::read_to_string(tmp.path().join("a/x.md")).unwrap();
    assert!(
        moved.contains("[auth](../t/auth.md)"),
        "outbound relative link must be rebased to the new directory: {moved}"
    );
    // The moved file reports itself among the updated references.
    let updated: Vec<&str> = data
        .get("references_updated")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        updated.contains(&"a/x.md"),
        "moved file must be listed exactly once: {updated:?}"
    );
    assert_eq!(
        updated.iter().filter(|p| **p == "a/x.md").count(),
        1,
        "moved file must not be double-counted: {updated:?}"
    );
    // The rebased link binds in the rebuilt graph (no unresolved edge).
    nodex(tmp.path()).arg("build").assert().success();
    let issues = run_json(nodex(tmp.path()).args(["query", "issues"]));
    let unresolved = issues
        .pointer("/unresolved_edges/total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert_eq!(unresolved, 0, "rebased link must resolve: {issues}");
}

#[test]
fn rename_preserves_literal_root_relative_reference_in_moved_file() {
    // A root-relative link in the moved file is move-invariant — the
    // file must come through the cross-directory move byte-identical.
    let tmp = scratch();
    init_project(tmp.path());
    let original = "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n\
                    [auth](t/auth.md)\n";
    write_doc(tmp.path(), "a/x.md", original);
    write_doc(
        tmp.path(),
        "t/auth.md",
        "---\nid: auth\ntitle: Auth\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a/x.md", "b/x.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("b/x.md")).unwrap(),
        original,
        "root-relative reference must survive a cross-directory move untouched"
    );
}

#[test]
fn rename_within_directory_leaves_moved_file_references_untouched() {
    // Same-directory rename: no vantage point moved — the moved file's
    // own outbound references must stay byte-identical.
    let tmp = scratch();
    init_project(tmp.path());
    let original = "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n\
                    [auth](../t/auth.md)\n";
    write_doc(tmp.path(), "a/x.md", original);
    write_doc(
        tmp.path(),
        "t/auth.md",
        "---\nid: auth\ntitle: Auth\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a/x.md", "a/y.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("a/y.md")).unwrap(),
        original,
        "within-directory rename must not rewrite the moved file's references"
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
    // Fixture must surface all three optional component signals so
    // we can assert their presence:
    //   - `reviewed` date on doc-active → freshness present
    //   - doc-archived links to doc-active → max_in > 0 so
    //     backlinks is present on every node (honest signal, not
    //     fabrication)
    // Drift stays absent (no `git_drift_threshold` in default
    // config) — that's the intended omission case and is asserted
    // elsewhere.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/active.md",
        "---\nid: doc-active\ntitle: Active\nkind: generic\nstatus: active\nreviewed: 2026-05-01\n---\n# Active\n",
    );
    write_doc(
        tmp.path(),
        "docs/archived.md",
        "---\nid: doc-archived\ntitle: Archived\nkind: generic\nstatus: archived\n---\n# Archived\n\nSee [active](active.md).\n",
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
    // doc-c is orthogonal on every signal — different kind AND
    // different parent directory — so its composite is dominated by
    // the title-overlap zero. Without a built-in score cutoff every
    // candidate enters the listing, so this test asserts the
    // *ranking* contract: doc-b must outrank doc-c.
    write_doc(
        tmp.path(),
        "other/c.md",
        "---\nid: doc-c\ntitle: Completely Unrelated Topic\nkind: guide\nstatus: active\n---\n# Other\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["query", "similar", "--id", "doc-a"]));
    let items = data.get("items").and_then(Value::as_array).expect("items");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    let b_pos = ids
        .iter()
        .position(|i| *i == "doc-b")
        .expect("shared title tokens must surface");
    let c_pos = ids
        .iter()
        .position(|i| *i == "doc-c")
        .expect("c appears without a cutoff");
    assert!(b_pos < c_pos, "related must outrank unrelated; got {ids:?}");
}

#[test]
fn query_similar_limit_caps_results() {
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
        "---\nid: doc-b\ntitle: Auth Retry Plan\nkind: generic\nstatus: active\n---\n# Plan\n",
    );
    write_doc(
        tmp.path(),
        "docs/c.md",
        "---\nid: doc-c\ntitle: Auth Retry Backoff\nkind: generic\nstatus: active\n---\n# Backoff\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data =
        run_json(nodex(tmp.path()).args(["query", "similar", "--id", "doc-a", "--limit", "1"]));
    let total = data.get("total").and_then(Value::as_u64).unwrap_or(0);
    assert_eq!(total, 1, "--limit 1 must truncate to a single candidate");
}

#[test]
fn query_similar_min_score_filters_low_matches() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: Auth Retry Policy\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    write_doc(
        tmp.path(),
        "other/far.md",
        "---\nid: doc-far\ntitle: Completely Different Topic\nkind: guide\nstatus: active\n---\n# Far\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args([
        "query",
        "similar",
        "--id",
        "doc-a",
        "--min-score",
        "0.99",
    ]));
    let items = data.get("items").and_then(Value::as_array).expect("items");
    assert!(
        items.is_empty(),
        "no candidate should clear --min-score 0.99 in this fixture; got {items:?}"
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
fn meta_version_pin_warns_reads_but_blocks_mutations() {
    // A project pinned below the running binary must not lose read
    // access — reads succeed and merely carry the binary-compat
    // advisory — while mutations are refused outright.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[meta]\nnodex_version = \"<0.0.1\"\n",
    )
    .unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    let envelope = run_envelope(nodex(tmp.path()).args(["query", "nodes"]));
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("out-of-pin read must carry warnings");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("meta.nodex_version"))),
        "read advisory must name the pin: {warnings:?}"
    );

    // A dry-run writes nothing, so it stays a read: it succeeds and
    // carries the advisory rather than being blocked by the pin.
    let dry = run_envelope(nodex(tmp.path()).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "X",
        "--path",
        "docs/x.md",
        "--dry-run",
    ]));
    assert!(
        dry.get("warnings")
            .and_then(Value::as_array)
            .is_some_and(|w| w
                .iter()
                .any(|x| x.as_str().is_some_and(|s| s.contains("meta.nodex_version")))),
        "dry-run scaffold must carry the advisory, not fail: {dry}"
    );

    // The actual write is refused on an incompatible binary.
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "X"])
        .output()
        .expect("ran");
    assert_eq!(
        output.status.code(),
        Some(2),
        "writing mutation on an out-of-pin binary must exit non-zero"
    );
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
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
fn check_envelope_lists_skipped_rules_for_registered_but_inapplicable_rules() {
    // The "no silent rule skips" doctrine: a rule whose `is_applicable`
    // returns false must surface in `skipped_rules` with its reason,
    // never as a silent pass.
    //
    // The witness is `frontmatter_immutable` configured *but* invoked
    // without `--since`: the rule is registered (config block present)
    // and applicable in principle, but its prerequisite (diff context)
    // is absent. Unconfigured rules are not "skipped" — they don't
    // exist for the project — and that distinction is what makes the
    // manifest + skipped_rules pair self-describing.
    let tmp = scratch();
    init_project(tmp.path());
    let cfg = tmp.path().join("nodex.toml");
    let mut content = fs::read_to_string(&cfg).expect("nodex.toml");
    content
        .push_str("\n[[rules.frontmatter_immutable]]\nname = \"identity\"\nfields = [\"kind\"]\n");
    fs::write(&cfg, content).expect("nodex.toml writable");
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
        rule_ids.contains(&"frontmatter_immutable/identity"),
        "configured-but-no-since must surface in skipped_rules: {skipped:?}"
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
            v.get("rule_id").and_then(Value::as_str) == Some("unknown_field")
                && v.get("message")
                    .and_then(Value::as_str)
                    .map(|m| m.contains("\"relatd\""))
                    .unwrap_or(false)
        }),
        "strict mode must flag `relatd:` typo as unknown_field: {violations:?}"
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

[[rules.frontmatter_immutable]]
name = "successor-chain"
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
        skipped.contains(&"frontmatter_immutable/successor-chain"),
        "plain check must list frontmatter_immutable/successor-chain as skipped: {plain}"
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
            v.get("rule_id").and_then(Value::as_str)
                == Some("frontmatter_immutable/successor-chain")
                && v.get("node_id").and_then(Value::as_str) == Some("doc-old")
        }),
        "check --since must surface frontmatter_immutable/successor-chain on doc-old: {violations:?}"
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
        !skipped_under_since.contains(&"frontmatter_immutable/successor-chain"),
        "frontmatter_immutable/successor-chain must not be skipped when --since is supplied: {env}"
    );
}

#[test]
fn immutable_baseline_enforces_without_explicit_since() {
    // With `rules.immutable_baseline = "HEAD"`, a plain `nodex check`
    // (no `--since`) must enforce the diff-aware immutability rules
    // against the last commit — closing the gap where they were inert
    // unless a ref was passed by hand.
    let tmp = scratch();
    let root = tmp.path();

    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[rules]
immutable_baseline = "HEAD"

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"

[[rules.frontmatter_immutable]]
name = "successor-chain"
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

    // Tamper the locked field in the work tree — no new commit.
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-tampered\n---\n# Old\n",
    );

    nodex(root).arg("build").assert().success();

    // Plain check (no --since) now fires the rule via the baseline.
    let output = nodex(root).args(["check"]).output().expect("ran");
    assert_eq!(output.status.code(), Some(1));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str)
                == Some("frontmatter_immutable/successor-chain")
        }),
        "baseline must enforce immutability without --since: {violations:?}"
    );
    let skipped: Vec<&str> = env
        .pointer("/data/skipped_rules")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !skipped.contains(&"frontmatter_immutable/successor-chain"),
        "rule must be active (not skipped) under an immutable_baseline: {env}"
    );
}

#[test]
fn check_since_fires_body_immutable_on_body_only_edit() {
    // Regression: a body edit on a terminal-status doc that touches NO
    // frontmatter field must still surface as a `body_immutable/<name>`
    // violation under `check --since`. The CLI's changed-id set is
    // built from every `GraphDiff` variant that names a node id; if
    // `body_changes` is omitted from that set, the post-filter strips
    // the rule's legitimate violations and we get a silent skip —
    // exactly the failure mode `.claude/rules/config-driven.md`
    // forbids. This test exercises the body-only path end-to-end.
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

[[rules.body_immutable]]
name = "frozen-once-terminal"
mode = "frozen"
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

    // Successor (so the cross_field constraint passes on the superseded doc)
    // and a predecessor at terminal status with an initial body.
    write_doc(
        root,
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n\nOriginal body.\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);

    // Body-only edit on the terminal doc — no frontmatter touched.
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n\nTampered body.\n",
    );

    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["check", "--since", "HEAD"])
        .output()
        .expect("ran");
    // The rule is severity=error, so the violation must drive exit 1.
    assert_eq!(
        output.status.code(),
        Some(1),
        "body-only edit on terminal doc must drive exit 1; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let env: Value = serde_json::from_slice(&output.stdout).expect("json envelope");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations array");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/frozen-once-terminal")
                && v.get("node_id").and_then(Value::as_str) == Some("doc-old")
        }),
        "body-only edit must surface body_immutable/frozen-once-terminal on doc-old: {violations:?}"
    );
}

#[test]
fn check_since_creation_trigger_locks_active_doc_but_not_creating_commit() {
    // trigger = "creation": the body freezes as soon as a prior
    // committed snapshot exists, regardless of status. Two contracts
    // end-to-end: (a) the commit that *creates* the doc passes check
    // (the diff carries it as added, not body-changed); (b) a later
    // body edit on the still-`active` doc fires — the case the
    // terminal trigger structurally cannot express.
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

[[rules.body_immutable]]
name = "record"
mode = "frozen"
trigger = "creation"
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

    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "config"]);

    // (a) Creating commit: the new doc is `added`, never `body_changed`
    // — check --since HEAD must pass.
    write_doc(
        root,
        "docs/rec.md",
        "---\nid: doc-rec\ntitle: Rec\nkind: generic\nstatus: active\n---\n# Rec\n\nDecided.\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["check", "--since", "HEAD"])
        .assert()
        .success();

    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "create record"]);

    // (b) Body edit on the committed, still-active doc must fire.
    write_doc(
        root,
        "docs/rec.md",
        "---\nid: doc-rec\ntitle: Rec\nkind: generic\nstatus: active\n---\n# Rec\n\nRe-decided.\n",
    );
    nodex(root).arg("build").assert().success();
    let output = nodex(root)
        .args(["check", "--since", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(
        output.status.code(),
        Some(1),
        "creation-trigger lock must fire on an active doc's body edit; stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let env: Value = serde_json::from_slice(&output.stdout).expect("json envelope");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations array");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/record")
                && v.get("node_id").and_then(Value::as_str) == Some("doc-rec")
        }),
        "expected body_immutable/record on doc-rec: {violations:?}"
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

// ─── query nodes ────────────────────────────────────────────────────

/// Seed a tiny multi-kind, multi-status corpus that exercises every
/// `query nodes` predicate (kind / status / tag).
fn seed_listing_corpus(tmp: &std::path::Path) {
    init_project(tmp);
    // Allow extra kinds and the `draft` status so the corpus tests
    // every filter category against real graph state.
    let cfg_path = tmp.join("nodex.toml");
    let mut content = fs::read_to_string(&cfg_path).expect("nodex.toml");
    content = content.replace(
        "allowed = [\"generic\", \"guide\", \"readme\"]",
        "allowed = [\"generic\", \"guide\", \"readme\", \"spec\", \"adr\"]",
    );
    content = content.replace(
        "allowed = [\"active\", \"superseded\", \"archived\", \"deprecated\", \"abandoned\"]",
        "allowed = [\"draft\", \"active\", \"superseded\", \"archived\", \"deprecated\", \"abandoned\"]",
    );
    fs::write(&cfg_path, content).expect("nodex.toml writable");
    write_doc(
        tmp,
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: spec\nstatus: active\ntags: [auth, policy]\n---\n",
    );
    write_doc(
        tmp,
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: spec\nstatus: draft\ntags: [auth]\n---\n",
    );
    write_doc(
        tmp,
        "docs/c.md",
        "---\nid: doc-c\ntitle: C\nkind: adr\nstatus: active\ntags: [policy]\n---\n",
    );
    write_doc(
        tmp,
        "docs/d.md",
        "---\nid: doc-d\ntitle: D\nkind: generic\nstatus: active\n---\n",
    );
}

#[test]
fn query_nodes_empty_filter_returns_every_node_sorted_by_id() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "nodes"]));
    let items = data["items"].as_array().expect("items array");
    let ids: Vec<&str> = items.iter().filter_map(|i| i["id"].as_str()).collect();
    assert_eq!(ids, ["doc-a", "doc-b", "doc-c", "doc-d"]);
}

#[test]
fn query_nodes_kind_filter_is_or_within_category() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "nodes", "--kind", "spec,adr"]));
    let ids: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, ["doc-a", "doc-b", "doc-c"]);
}

#[test]
fn query_nodes_kind_and_status_intersect_across_categories() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(
        nodex(tmp.path()).args(["query", "nodes", "--kind", "spec", "--status", "active"]),
    );
    let ids: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, ["doc-a"]);
}

#[test]
fn query_nodes_tag_or_by_default() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "nodes", "--tag", "auth,policy"]));
    let ids: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    // a (auth+policy), b (auth), c (policy) — d has no tags
    assert_eq!(ids, ["doc-a", "doc-b", "doc-c"]);
}

#[test]
fn query_nodes_all_tags_switches_or_to_and() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data =
        run_json(nodex(tmp.path()).args(["query", "nodes", "--tag", "auth,policy", "--all-tags"]));
    let ids: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, ["doc-a"]);
}

#[test]
fn query_nodes_limit_caps_after_sort() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "nodes", "--limit", "2"]));
    let ids: Vec<&str> = data["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, ["doc-a", "doc-b"]);
}

#[test]
fn query_nodes_limit_reports_honest_counts() {
    // `total` is the matching count, `returned` appears only when the
    // cap dropped entries — a capped response can never read as "this
    // is everything".
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();

    let capped = run_json(nodex(tmp.path()).args(["query", "nodes", "--limit", "2"]));
    assert_eq!(capped["total"].as_u64(), Some(4), "total = every match");
    assert_eq!(capped["returned"].as_u64(), Some(2), "returned = shipped");

    let uncapped = run_json(nodex(tmp.path()).args(["query", "nodes"]));
    assert_eq!(uncapped["total"].as_u64(), Some(4));
    assert!(
        uncapped.get("returned").is_none(),
        "returned is omitted when nothing was dropped: {uncapped}"
    );

    // A limit larger than the population drops nothing → no `returned`.
    let roomy = run_json(nodex(tmp.path()).args(["query", "nodes", "--limit", "99"]));
    assert!(roomy.get("returned").is_none());
}

#[test]
fn query_nodes_fields_projects_each_item() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "nodes", "--fields", "id,kind"]));
    for item in data["items"].as_array().expect("items array") {
        let keys: Vec<&str> = item
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["id", "kind"], "exactly the named fields: {item}");
    }
    // Omitting --fields keeps all five — the empty list can never
    // produce an empty object.
    let full = run_json(nodex(tmp.path()).args(["query", "nodes"]));
    let first = &full["items"][0];
    for key in ["id", "title", "kind", "status", "path"] {
        assert!(first.get(key).is_some(), "{key} present on full item");
    }
}

#[test]
fn query_nodes_rejects_unknown_field() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "nodes", "--fields", "id,bogus"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "unknown field must fail loud");
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("bogus")
    );
}

#[test]
fn query_search_and_orphans_limit_report_honest_counts() {
    // The uniform `--limit` contract holds across the unbounded list
    // surfaces, not just `nodes`.
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();

    let orphans = run_json(nodex(tmp.path()).args(["query", "orphans", "--limit", "1"]));
    let total = orphans["total"].as_u64().expect("total");
    assert!(total > 1, "corpus has multiple orphans: {orphans}");
    assert_eq!(orphans["returned"].as_u64(), Some(1));
    assert_eq!(orphans["items"].as_array().unwrap().len(), 1);

    let search = run_json(nodex(tmp.path()).args(["query", "search", "doc", "--limit", "1"]));
    assert!(search["total"].as_u64().unwrap_or(0) > 1);
    assert_eq!(search["returned"].as_u64(), Some(1));
}

#[test]
fn query_backlinks_stale_components_limit_report_honest_counts() {
    // The uniform `--limit` contract on the remaining plain-listing
    // surfaces: truncate after each query's deterministic order,
    // `total` = every match, `returned` only when capped, zero
    // rejected.
    let tmp = scratch();
    init_project(tmp.path());
    // Two linkers → target (backlinks ≥ 2); both linkers stale-free is
    // irrelevant here. Stale corpus: two active docs with old reviewed
    // dates under a stale_days threshold.
    let config_path = tmp.path().join("nodex.toml");
    let config = fs::read_to_string(&config_path)
        .unwrap()
        .replace("stale_days = 180", "stale_days = 30");
    fs::write(&config_path, config).unwrap();
    write_doc(
        tmp.path(),
        "target.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\nreviewed: 2020-01-01\n---\n# T\n",
    );
    write_doc(
        tmp.path(),
        "l1.md",
        "---\nid: l1\ntitle: L1\nkind: generic\nstatus: active\nreviewed: 2020-01-01\n---\n[t](target.md)\n",
    );
    write_doc(
        tmp.path(),
        "l2.md",
        "---\nid: l2\ntitle: L2\nkind: generic\nstatus: active\nreviewed: 2020-01-01\n---\n[t](target.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    for args in [
        vec!["query", "backlinks", "target", "--limit", "1"],
        vec!["query", "stale", "--limit", "1"],
        vec!["query", "components", "--limit", "1"],
    ] {
        let data = run_json(nodex(tmp.path()).args(&args));
        let total = data["total"].as_u64().unwrap_or(0);
        let shipped = data["items"].as_array().map(Vec::len).unwrap_or(0);
        if total > 1 {
            assert_eq!(shipped, 1, "{args:?} capped to 1: {data}");
            assert_eq!(data["returned"].as_u64(), Some(1), "{args:?}: {data}");
        } else {
            assert!(data.get("returned").is_none(), "{args:?}: {data}");
        }
    }
    // backlinks specifically has 2 matches — the cap must be announced.
    let bl = run_json(nodex(tmp.path()).args(["query", "backlinks", "target", "--limit", "1"]));
    assert_eq!(bl["total"].as_u64(), Some(2));
    assert_eq!(bl["returned"].as_u64(), Some(1));

    // Zero caps rejected uniformly.
    for args in [
        vec!["query", "backlinks", "target", "--limit", "0"],
        vec!["query", "stale", "--limit", "0"],
        vec!["query", "components", "--limit", "0"],
        vec!["query", "orphans", "--limit", "0"],
        vec!["query", "search", "t", "--limit", "0"],
    ] {
        let output = nodex(tmp.path()).args(&args).output().expect("ran");
        assert!(!output.status.success(), "{args:?} must reject zero");
        let env: Value = serde_json::from_slice(&output.stdout).expect("JSON");
        assert_eq!(env["error"]["code"], "CONFIG_ERROR", "{args:?}");
    }
}

#[test]
fn query_search_rejects_unknown_status() {
    // An unknown status would silently match nothing and return a
    // successful empty result — the silent-skip failure mode every
    // vocabulary-taking flag refuses.
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "search", "doc", "--status", "all"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "unknown status must fail loud");
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("all")
    );
}

#[test]
fn query_node_with_body_attaches_canonical_body() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nBody line.\n",
    );
    // Body-less doc: "asked and empty" must be `""`, distinct from
    // "not asked" (key absent).
    write_doc(
        tmp.path(),
        "docs/empty.md",
        "---\nid: doc-empty\ntitle: E\nkind: generic\nstatus: active\n---\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let with = run_json(nodex(tmp.path()).args(["query", "node", "doc-a", "--with-body"]));
    assert_eq!(
        with["body"].as_str(),
        Some("# A\n\nBody line.\n"),
        "body attached verbatim: {with}"
    );

    let without = run_json(nodex(tmp.path()).args(["query", "node", "doc-a"]));
    assert!(
        without.get("body").is_none(),
        "body omitted when not asked: {without}"
    );

    let empty = run_json(nodex(tmp.path()).args(["query", "node", "doc-empty", "--with-body"]));
    assert_eq!(empty["body"].as_str(), Some(""), "asked-and-empty is \"\"");
}

#[test]
fn query_node_with_body_on_stale_graph_emits_io_error() {
    // The node resolves in the graph but the file is gone — a silent
    // body drop would hide the staleness; a typed IO_ERROR names it.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    fs::remove_file(tmp.path().join("docs/a.md")).unwrap();

    let output = nodex(tmp.path())
        .args(["query", "node", "doc-a", "--with-body"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "IO_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("nodex build"),
        "stale-graph error must point at the fix: {env}"
    );
}

#[test]
fn query_nodes_rejects_unknown_kind() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "nodes", "--kind", "doesnotexist"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "unknown kind must fail loud");
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("doesnotexist")
    );
}

#[test]
fn query_nodes_rejects_unknown_status() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "nodes", "--status", "bogusstatus"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
}

#[test]
fn query_nodes_rejects_empty_csv_entry() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "nodes", "--kind", ""])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
}

#[test]
fn query_nodes_rejects_zero_limit() {
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "nodes", "--limit", "0"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
}

#[test]
fn query_tags_subcommand_is_removed() {
    // `query tags` was replaced by `query nodes --tag` in 0.8.
    // Verify the legacy form no longer parses.
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "tags", "anything"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "`query tags` must be gone");
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

#[test]
fn query_annotations_with_frontmatter_enriches_sources() {
    let tmp = scratch();
    init_project(tmp.path());
    append_annotations_block(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\ncreated: 2026-01-02\ntags: [auth, policy]\n---\n\n[PROMOTES: spec-x]\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args([
        "query",
        "annotations",
        "--with-frontmatter",
        "created,tags,kind",
    ]));
    let sources = data["items"][0]["entries"][0]["sources"]
        .as_array()
        .expect("sources");
    let fm = &sources[0]["frontmatter"];
    assert_eq!(fm["created"], "2026-01-02");
    assert_eq!(fm["kind"], "generic");
    let tags = fm["tags"].as_array().expect("tags array");
    assert_eq!(tags.len(), 2);
}

#[test]
fn query_annotations_with_frontmatter_omitted_when_flag_absent() {
    let tmp = scratch();
    init_project(tmp.path());
    append_annotations_block(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\ncreated: 2026-01-02\n---\n\n[PROMOTES: spec-x]\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["query", "annotations"]));
    let source = &data["items"][0]["entries"][0]["sources"][0];
    assert!(
        source.get("frontmatter").is_none(),
        "frontmatter key must be omitted when --with-frontmatter is absent: {source}"
    );
}

#[test]
fn query_annotations_with_frontmatter_rejects_unknown_field() {
    let tmp = scratch();
    init_project(tmp.path());
    append_annotations_block(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args([
            "query",
            "annotations",
            "--with-frontmatter",
            "creatd", // typo
        ])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "unknown field must fail");
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("creatd"),
        "error must echo the offending field name"
    );
}

// ─── query dependents ───────────────────────────────────────────────

#[test]
fn query_dependents_returns_transitive_reverse_chain() {
    let tmp = scratch();
    init_project(tmp.path());
    // c → b → a via `implements` frontmatter. From a's perspective:
    // b is a hop-1 dependent, c is hop-2.
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\nimplements: [doc-a]\n---\n",
    );
    write_doc(
        tmp.path(),
        "docs/c.md",
        "---\nid: doc-c\ntitle: C\nkind: generic\nstatus: active\nimplements: [doc-b]\n---\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["query", "dependents", "doc-a"]));
    let deps = data["dependents"].as_array().expect("dependents array");
    let by_id: std::collections::HashMap<&str, &Value> = deps
        .iter()
        .map(|d| (d["id"].as_str().unwrap_or(""), d))
        .collect();
    assert_eq!(by_id["doc-b"]["hops"].as_u64(), Some(1));
    assert_eq!(by_id["doc-c"]["hops"].as_u64(), Some(2));
    let via_c = by_id["doc-c"]["via"].as_array().expect("via array");
    assert_eq!(via_c.len(), 2);
    assert_eq!(via_c[0]["source"], "doc-c");
    assert_eq!(via_c[1]["source"], "doc-b");
}

#[test]
fn query_dependents_depth_bound_stops_expansion() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\nimplements: [doc-a]\n---\n",
    );
    write_doc(
        tmp.path(),
        "docs/c.md",
        "---\nid: doc-c\ntitle: C\nkind: generic\nstatus: active\nimplements: [doc-b]\n---\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["query", "dependents", "doc-a", "--depth", "1"]));
    let deps = data["dependents"].as_array().expect("dependents array");
    let ids: Vec<&str> = deps.iter().filter_map(|d| d["id"].as_str()).collect();
    assert_eq!(ids, vec!["doc-b"], "depth=1 must exclude doc-c");
}

#[test]
fn query_dependents_unknown_relation_emits_typed_config_error() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["query", "dependents", "doc-a", "--relations", "implments"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("implments"),
        "error must echo the unknown relation"
    );
}

// ─── export rules ───────────────────────────────────────────────────

#[test]
fn export_rules_emits_active_only_manifest_with_version_and_body_line_entries() {
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("nodex.toml");
    let mut content = fs::read_to_string(&path).expect("nodex.toml");
    content.push_str(
        "\n[[rules.body_line]]\nname = \"decision-log\"\npattern = '^- \\*\\*(?P<gate>[a-z-]+)\\*\\*'\nenums.gate = [\"scope\", \"design\"]\n",
    );
    fs::write(&path, content).expect("nodex.toml writable");

    let data = run_json(nodex(tmp.path()).args(["export", "rules"]));
    assert_eq!(data["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    let ids: Vec<&str> = data["rules"]
        .as_array()
        .expect("rules array")
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert!(ids.contains(&"required_field"));
    assert!(ids.contains(&"stale_review"));
    assert!(
        ids.contains(&"body_line/decision-log"),
        "config block must surface in manifest: {ids:?}"
    );
    assert!(
        !ids.iter()
            .any(|id| id.starts_with("frontmatter_immutable/")),
        "unconfigured rule must not appear: {ids:?}"
    );
    assert!(!ids.contains(&"git_drift"));
}

#[test]
fn export_rules_cycle_detection_surfaces_configured_relations() {
    // The manifest's cycle-detection entry must carry the live
    // relation set in `params` and a relation-agnostic description —
    // a hardcoded "implements" description would be false for this
    // project, which declares `depends_on` acyclic instead.
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("nodex.toml");
    let mut content = fs::read_to_string(&path).expect("nodex.toml");
    content.push_str(
        "\n[[parser.link_patterns]]\npattern = '@depends\\s+(\\S+)'\nrelation = \"depends_on\"\n",
    );
    // The key lives inside the [rules] table the init template opens.
    let content = content.replace(
        "immutable_baseline = \"HEAD\"",
        "immutable_baseline = \"HEAD\"\nacyclic_relations = [\"depends_on\"]",
    );
    fs::write(&path, content).expect("nodex.toml writable");

    let data = run_json(nodex(tmp.path()).args(["export", "rules"]));
    let rules = data["rules"].as_array().expect("rules array");
    let cycle = rules
        .iter()
        .find(|r| r["id"].as_str() == Some("graph_invariants/cycle-detection"))
        .expect("cycle-detection entry present");
    assert_eq!(
        cycle.pointer("/params/relations"),
        Some(&serde_json::json!(["depends_on"])),
        "params must carry the live relation set: {cycle}"
    );
    assert!(
        !cycle["description"]
            .as_str()
            .unwrap_or_default()
            .contains("implements"),
        "description must not name a relation this project does not check: {cycle}"
    );
}

#[test]
fn check_detects_cycle_in_configured_custom_relation() {
    // End-to-end: a project declaring `depends_on` acyclic gets exit 1
    // when two docs cycle through @depends references.
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("nodex.toml");
    let mut content = fs::read_to_string(&path).expect("nodex.toml");
    content.push_str(
        "\n[[parser.link_patterns]]\npattern = '@depends\\s+(\\S+)'\nrelation = \"depends_on\"\n",
    );
    let content = content.replace(
        "immutable_baseline = \"HEAD\"",
        "immutable_baseline = \"HEAD\"\nacyclic_relations = [\"depends_on\"]",
    );
    fs::write(&path, content).expect("nodex.toml writable");
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n@depends b\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n@depends a\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let assertion = nodex(tmp.path()).arg("check").assert().failure();
    let code = assertion.get_output().status.code().unwrap_or(-1);
    assert_eq!(code, 1, "cycle violation exits 1");
    let stdout = String::from_utf8_lossy(assertion.get_output().stdout.as_slice()).to_string();
    assert!(
        stdout.contains("depends_on"),
        "violation names the configured relation: {stdout}"
    );
}

#[test]
fn empty_acyclic_relations_rejected_at_load() {
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("nodex.toml");
    let content = fs::read_to_string(&path).expect("nodex.toml").replace(
        "immutable_baseline = \"HEAD\"",
        "immutable_baseline = \"HEAD\"\nacyclic_relations = []",
    );
    fs::write(&path, content).expect("nodex.toml writable");
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "config error exits 2");
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

// ─── export envelope-schema ─────────────────────────────────────────

#[test]
fn export_envelope_schema_runs_without_project_and_lists_per_command_entries() {
    // Envelope shape is project-independent; the command must run
    // even from a directory with no `nodex.toml`.
    let tmp = scratch();
    let data = run_json(nodex(tmp.path()).args(["export", "envelope-schema"]));
    assert_eq!(data["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    assert!(data["envelope"].is_object(), "envelope schema present");
    let per_command = data["per_command"].as_object().expect("per_command object");
    // Spot-check a handful of canonical entries — exhaustive coverage
    // lives in the core unit-test that validates each schema against
    // draft 2020-12.
    for key in ["query.issues", "query.annotations", "check", "build"] {
        assert!(
            per_command.contains_key(key),
            "per_command missing {key}: keys = {:?}",
            per_command.keys().collect::<Vec<_>>()
        );
    }
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
