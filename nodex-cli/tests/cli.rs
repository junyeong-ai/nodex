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

/// The `message` of a typed `{ code, message }` envelope warning.
/// Plain string arrays (paths, ids, schema field names) are read
/// directly with `Value::as_str` — this accessor is for the `warnings`
/// plane only, so a call site's choice of reader documents which shape
/// it expects.
fn warning_msg(v: &Value) -> Option<&str> {
    v.get("message").and_then(Value::as_str)
}

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

/// A `git` runner pinned to `root` with a deterministic identity and no
/// gpg signing — the substrate for tests that need an `immutable_baseline`.
///
/// Built through the same seam production code uses, so a fixture cannot
/// be redirected by the environment the suite inherits: under an ambient
/// `GIT_DIR` a raw `git init` initialises *that* repository and the
/// fixture's commits land in it, which mutates a repository the test
/// never named.
fn git_runner(root: &std::path::Path) -> impl Fn(&[&str]) -> std::process::Output + '_ {
    move |args: &[&str]| {
        let git = || {
            let mut git = nodex_core::git::command(root).expect("git on PATH");
            git.env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null");
            git
        };
        let out = git().args(args).output().expect("git ran");
        if args.first() == Some(&"init") {
            // commit.gpgsign off so signing isn't required in CI.
            git()
                .args(["config", "commit.gpgsign", "false"])
                .output()
                .expect("git config ran");
        }
        out
    }
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
            .get("message")
            .and_then(Value::as_str)
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
fn check_flags_dropped_doc_as_parse_failure_violation() {
    // A doc that fails to parse never enters the graph; `check` reports
    // it as an Error-severity node-less `parse_failure` violation and
    // exits 1 — a dropped document can never pass a CI gate as a
    // warning the gate ignores.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/ok.md",
        "---\nid: generic-ok\ntitle: OK\nkind: generic\nstatus: active\n---\n# OK\n",
    );
    write_doc(root, "docs/bad.md", "---\nid: [unclosed yaml\n---\n# bad\n");
    nodex(root).arg("build").assert().success();

    let out = nodex(root).arg("check").assert().failure().code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    let violation = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations array")
        .iter()
        .find(|v| v.get("rule_id").and_then(Value::as_str) == Some("parse_failure"))
        .cloned()
        .unwrap_or_else(|| panic!("parse_failure violation expected: {env}"));
    assert_eq!(
        violation.get("node_id"),
        Some(&Value::Null),
        "no node exists to attribute the drop to: {violation}"
    );
    assert_eq!(
        violation.get("path").and_then(Value::as_str),
        Some("docs/bad.md"),
        "the violation names the dropped file: {violation}"
    );
}

#[test]
fn check_flags_field_broken_doc_as_field_parse_violation() {
    // A wrong-typed built-in (a bad date) is a `field_parse` violation
    // on a present node: the node stays in the graph, the field reads
    // as absent, and `check` exits 1 with the Error-severity finding.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\ncreated: yesterday\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // The node is present despite the broken field.
    let node = run_json(nodex(root).args(["query", "node", "generic-a"]));
    assert_eq!(
        node.pointer("/node/id").and_then(Value::as_str),
        Some("generic-a"),
        "field-broken doc keeps its node: {node}"
    );

    let out = nodex(root).arg("check").assert().failure().code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| {
                v.get("rule_id").and_then(Value::as_str) == Some("field_parse")
                    && v.get("node_id").and_then(Value::as_str) == Some("generic-a")
                    && v.get("message")
                        .and_then(Value::as_str)
                        .is_some_and(|m| m.contains("\"created\""))
            }),
        "field_parse violation on the present node expected: {env}"
    );
}

#[test]
fn subcommand_groups_without_a_subcommand_emit_json_error() {
    // `nodex query` / `lifecycle` / `export` with no subcommand is a
    // parse failure, not a `--help` request: it must emit the JSON error
    // envelope on stdout (exit 2), never bare help text on stderr — the
    // JSON-first contract holds for every invocation.
    let tmp = scratch();
    for group in ["query", "lifecycle", "export"] {
        let output = nodex(tmp.path()).arg(group).output().expect("ran");
        assert_eq!(output.status.code(), Some(2), "{group} exits 2");
        let parsed: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
        assert_eq!(
            parsed.pointer("/error/code").and_then(Value::as_str),
            Some("INVALID_ARGUMENT"),
            "{group} emits a JSON error envelope"
        );
    }
}

#[test]
fn init_template_frontmatter_immutable_example_loads_when_enabled() {
    // The commented `frontmatter_immutable` example in the `init`
    // template must load when uncommented verbatim — the tool's own
    // documented config can never be one its own loader rejects.
    let tmp = scratch();
    let root = tmp.path();
    nodex(root).arg("init").assert().success();
    let cfg = fs::read_to_string(root.join("nodex.toml")).unwrap();
    let enabled = cfg.replace(
        "# [[rules.frontmatter_immutable]]\n# name = \"identity\"\n# fields = [\"kind\", \"superseded_by\"]",
        "[[rules.frontmatter_immutable]]\nname = \"identity\"\nfields = [\"kind\", \"superseded_by\"]",
    );
    assert!(
        enabled != cfg,
        "the documented immutable example must be present to enable"
    );
    fs::write(root.join("nodex.toml"), enabled).unwrap();
    nodex(root).arg("build").assert().success();
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
fn query_trust_bottom_status_restricts_to_review_queue() {
    // The review-queue read: terminal docs legitimately score near zero
    // and dominate an unfiltered bottom-K — `--status active` keeps the
    // listing to nodes a review can act on.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/stale.md",
        "---\nid: doc-stale\ntitle: Stale\nkind: generic\nstatus: active\nreviewed: 2020-01-01\n---\n# Stale\n",
    );
    write_doc(
        tmp.path(),
        "docs/dead.md",
        "---\nid: doc-dead\ntitle: Dead\nkind: generic\nstatus: archived\n---\n# Dead\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data =
        run_json(nodex(tmp.path()).args(["query", "trust", "--bottom", "5", "--status", "active"]));
    let ids: Vec<&str> = data
        .get("items")
        .and_then(serde_json::Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(serde_json::Value::as_str))
        .collect();
    assert_eq!(
        ids,
        vec!["doc-stale"],
        "--status active must exclude the terminal doc"
    );
}

#[test]
fn query_trust_status_rejects_unknown_vocabulary() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let out = nodex(tmp.path())
        .args(["query", "trust", "--bottom", "5", "--status", "nonsense"])
        .assert()
        .failure()
        .code(2);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR"),
        "an unknown status must be a loud config error, never an empty listing"
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
required = ["priority"]
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
fn chain_reports_every_branch_of_a_consolidation_end_to_end() {
    // `supersedes` is a DAG: one document may supersede several. `chain`
    // must return the WHOLE lineage from any anchor — a regression guard
    // for the lexicographic-min walk that silently dropped every branch
    // but one (e.g. `[doc-a, doc-merged]`, hiding `doc-b`).
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: superseded\n---\n# B\n",
    );
    write_doc(
        tmp.path(),
        "docs/merged.md",
        "---\nid: doc-merged\ntitle: Merged\nkind: generic\nstatus: active\nsupersedes: [doc-a, doc-b]\n---\n# Merged\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let ids = |anchor: &str| -> Vec<String> {
        let data = run_json(nodex(tmp.path()).args(["query", "chain", anchor]));
        data.get("items")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|v| v.get("id").and_then(Value::as_str).map(String::from))
            .collect()
    };
    // Oldest → newest, both roots preserved, identical from every anchor.
    let expected = vec![
        "doc-a".to_string(),
        "doc-b".to_string(),
        "doc-merged".to_string(),
    ];
    assert_eq!(ids("doc-merged"), expected, "from the consolidation tip");
    assert_eq!(
        ids("doc-a"),
        expected,
        "from an older branch (anchor-agnostic)"
    );
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
fn scaffold_satisfies_cross_field_keyed_on_its_own_defaults() {
    // The self-consistency invariant at its hardest: a `cross_field`
    // whose `when` fires on a value scaffold ITSELF defaults (a required
    // enum field) must still get its `require` field written. The
    // renderer reparses the frontmatter-as-written and iterates to a
    // fixpoint, and validation runs over the full overlay graph, so
    // scaffold and `check` agree by construction.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
[kinds]
allowed = ["generic", "adr"]
[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
[schema]
required = ["component"]
enums = { component = ["auth", "billing"], severity = ["low", "high"] }
cross_field = [{ when = "component exists", require = "severity" }]
"#,
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args(["scaffold", "--kind", "adr", "--title", "X"])
        .args(["--path", "docs/x.md"])
        .assert()
        .success();
    let written = fs::read_to_string(root.join("docs/x.md")).unwrap();
    assert!(
        written.contains("component:") && written.contains("severity:"),
        "the cross_field require keyed on a defaulted field is written:\n{written}"
    );
    // The whole point: the tool's own check passes its own output.
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();
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
required = ["decision_date"]
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
fn lifecycle_refuses_action_whose_target_status_is_not_allowed() {
    // A project that only models draft/active/archived must load and
    // operate cleanly — lifecycle vocabulary is no longer forced into
    // every project's status set. A `set` whose target status the
    // project does not allow (here: "deprecated") is refused at the
    // write seam, leaving the document untouched, so the tool never
    // produces a doc its own `check` would reject.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"note\", \"generic\"]\n\
         [statuses]\nallowed = [\"draft\", \"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"draft\"\n\
         [[identity.kind_rules]]\nglob = \"**\"\nkind = \"note\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: note-a\ntitle: A\nkind: note\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // archived IS allowed → succeeds.
    nodex(tmp.path())
        .args(["lifecycle", "set", "note-a", "--status", "archived"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(tmp.path().join("a.md"))
            .unwrap()
            .contains(r#"status: "archived""#)
    );

    // deprecated is NOT allowed → refused, document untouched.
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: note-b\ntitle: B\nkind: note\nstatus: active\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let out = nodex(tmp.path())
        .args(["lifecycle", "set", "note-b", "--status", "deprecated"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("deprecated"),
        "names the refused status: {stdout}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("b.md"))
            .unwrap()
            .contains("status: active"),
        "refused transition must not touch the document"
    );
}

#[test]
fn lifecycle_set_refuses_status_with_unsatisfied_cross_field() {
    // The generic `set` writes only `status` (+ `updated`), so it must
    // refuse a status a `cross_field` rule governs while the required
    // field is absent — otherwise it would write a document its own
    // `check` rejects. `superseded` requires `superseded_by`; supplying
    // that is `supersede`'s job, not `set`'s. Config-driven: a project
    // that places no requirement on the status sets it freely.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\", \"archived\"]\n\
         terminal = [\"superseded\", \"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\n\
         cross_field = [{ when = \"status=superseded\", require = \"superseded_by\" }]\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // `set --status superseded` would dangle without a successor → refused.
    let out = nodex(tmp.path())
        .args(["lifecycle", "set", "generic-a", "--status", "superseded"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("superseded_by"),
        "names the required field: {stdout}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("a.md"))
            .unwrap()
            .contains("status: active"),
        "refused set must not touch the document"
    );

    // `archived` carries no cross-field requirement → set succeeds.
    nodex(tmp.path())
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(tmp.path().join("a.md"))
            .unwrap()
            .contains(r#"status: "archived""#)
    );
}

#[test]
fn lifecycle_does_not_launder_a_broken_field_and_does_not_refuse_over_one() {
    // The guard that used to refuse here was protecting against a laundering
    // that cannot happen: the editor rewrites the fields the action names and
    // leaves every other line exactly as it found it, so a malformed
    // `created:` is still malformed afterwards and `check` still flags it.
    // What refusing did instead was block a transition over a violation the
    // document already carried.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\ncreated: yesterday\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let flagged = |root: &std::path::Path| -> Vec<String> {
        envelope_of(nodex(root).arg("check"))
            .pointer("/data/violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter_map(|v| v["details"]["field"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(flagged(tmp.path()), ["created"]);

    nodex(tmp.path())
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .success();

    let after = fs::read_to_string(tmp.path().join("a.md")).unwrap();
    assert!(
        after.contains("created: yesterday"),
        "the broken line is left exactly as it was: {after}"
    );
    assert!(after.contains("status: \"archived\""), "{after}");
    assert_eq!(
        flagged(tmp.path()),
        ["created"],
        "still flagged, so nothing was laundered"
    );
}

#[test]
fn lifecycle_supersede_refuses_when_a_non_superseded_by_cross_field_is_unmet() {
    // `supersede` supplies `superseded_by`, but if the project requires
    // ANOTHER field once a doc is superseded, supersede must refuse just
    // as `set` does — otherwise it writes a document its own `check`
    // rejects (the self-consistency invariant). Same target status, same
    // missing field → same outcome across both write seams.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema.enums]\ndeprecation_note = [\"pending\", \"done\"]\n\
         [[schema.cross_field]]\nwhen = \"status=superseded\"\nrequire = \"deprecation_note\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: generic-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        tmp.path(),
        "new.md",
        "---\nid: generic-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // supersede would write status=superseded without deprecation_note →
    // refused, naming the field, leaving the doc untouched.
    let out = nodex(tmp.path())
        .args([
            "lifecycle",
            "supersede",
            "generic-old",
            "--to",
            "generic-new",
        ])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("deprecation_note"),
        "names the field: {stdout}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("old.md"))
            .unwrap()
            .contains("status: active"),
        "refused supersede must not touch the document"
    );
}

#[test]
fn lifecycle_review_refuses_a_frontmatter_immutable_locked_field_on_a_terminal_doc() {
    // `review` is the only lifecycle action that reaches an already-
    // terminal doc (the terminal guard blocks set/supersede), and it
    // writes `reviewed`. A `frontmatter_immutable` rule that freezes
    // `reviewed` once terminal must refuse the write — otherwise lifecycle
    // writes a doc its own `check --since baseline` then flags (the
    // symmetric-guards / self-consistency rule).
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[rules.frontmatter_immutable]]\nname = \"freeze-meta\"\nfields = [\"reviewed\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nstatus: superseded\nsuperseded_by: b\nreviewed: 2026-01-01\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\nstatus: active\n---\n# B\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();

    // review on the terminal+locked doc → refused, doc untouched.
    let out = nodex(root)
        .args(["lifecycle", "review", "a"])
        .output()
        .expect("ran");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("reviewed: 2026-01-01"),
        "refused review leaves reviewed untouched"
    );

    // review on the active (non-terminal) doc → the lock is inert.
    nodex(root)
        .args(["lifecycle", "review", "b"])
        .assert()
        .success();
}

#[test]
fn lifecycle_supersede_proceeds_when_only_superseded_by_is_required() {
    // The common case: the cross_field requires exactly the field
    // supersede supplies (`superseded_by`), so it proceeds and the
    // written doc passes check.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [[schema.cross_field]]\nwhen = \"status=superseded\"\nrequire = \"superseded_by\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: generic-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        tmp.path(),
        "new.md",
        "---\nid: generic-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args([
            "lifecycle",
            "supersede",
            "generic-old",
            "--to",
            "generic-new",
        ])
        .assert()
        .success();
    nodex(tmp.path()).arg("build").assert().success();
    let env = run_envelope(nodex(tmp.path()).arg("check"));
    assert_eq!(env.pointer("/data/total").and_then(Value::as_i64), Some(0));
}

#[test]
fn check_severity_warning_announces_hidden_errors() {
    // `--severity warning` is a display filter that hides Error-severity
    // violations and exits 0 — a silent-ish false-pass for a gate. A
    // warning must announce how many errors it suppressed.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    // kind out of vocab → an Error-severity field_enum violation.
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: bogus\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let env = run_envelope(nodex(tmp.path()).args(["check", "--severity", "warning"]));
    let warnings: Vec<&str> = env
        .get("warnings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(warning_msg).collect())
        .unwrap_or_default();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("hid 1 error-severity violation")),
        "must announce hidden errors: {warnings:?}"
    );
}

#[test]
fn check_content_out_of_scope_path_warns_instead_of_silent_green() {
    // `check --content <path>=-` on a path the scope does not admit
    // validates nothing and exits 0 — a write gate would pass on a
    // misaimed path. Surface it as a warning.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let env = run_envelope(
        nodex(tmp.path())
            .args(["check", "--content", "other/x.md=-"])
            .write_stdin("---\nid: x\ntitle: X\nstatus: active\n---\n# X\n"),
    );
    let warnings: Vec<&str> = env
        .get("warnings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(warning_msg).collect())
        .unwrap_or_default();
    assert!(
        warnings.iter().any(|w| w.contains("out of scope")),
        "must warn on out-of-scope content path: {warnings:?}"
    );
}

#[test]
fn query_in_missing_project_dir_emits_graph_missing_code() {
    // -C into a path that doesn't exist has no snapshot to read: the
    // query classifies through the typed chain as GRAPH_MISSING — never
    // the catch-all INTERNAL_ERROR.
    let nonexistent = "/nonexistent-nodex-dir-abc-xyz";
    // Spawned directly rather than through `nodex()`: the binary under
    // test needs no working directory, and `assert_cmd` would supply the
    // suite's own.
    #[expect(
        clippy::disallowed_methods,
        reason = "the binary under test, not a git invocation"
    )]
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nodex"))
        .args(["-C", nonexistent, "query", "orphans"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("GRAPH_MISSING"),
        "no snapshot must surface as GRAPH_MISSING, not INTERNAL_ERROR"
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
child_glob = "**/*"
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
    let by_target: std::collections::BTreeMap<&str, (&str, &str, Option<&str>)> = unresolved
        .iter()
        .filter_map(|e| {
            let target = e.get("raw_target").and_then(Value::as_str)?;
            let cause = e.get("cause").and_then(Value::as_str)?;
            let severity = e.get("severity").and_then(Value::as_str)?;
            let policy_name = e.get("policy_name").and_then(Value::as_str);
            Some((target, (cause, severity, policy_name)))
        })
        .collect();
    assert_eq!(
        by_target.get("docs/missing.md").copied(),
        Some(("missing", "warning", None)),
        "truly absent target must be `missing` — the unattributed warning fallthrough; \
         got {by_target:?}"
    );
    assert_eq!(
        by_target.get("specs/x/sub.md").copied(),
        Some(("excluded_from_scope", "info", Some("excluded_target"))),
        "on-disk-but-excluded target must be `excluded_from_scope`, classified info by the \
         default excluded_target policy row; got {by_target:?}"
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
fn query_issues_applies_unresolved_policy_info_downgrade() {
    // Two dangling links; the declared policy routes the specs/** one
    // to `info` (expected-by-design ephemera) while the docs/** one
    // falls through to the counted `warning` plane. Declaring the table
    // replaced the default — and every edge stays visible with its
    // per-edge attribution.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
[detection]
orphan_ok_kinds = ["generic"]
[[detection.unresolved_policy]]
name = "ephemeral-specs"
cause = "missing"
glob = "specs/**"
severity = "info"
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nSee [spec](specs/x.md) and [gone](docs/missing.md).\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["query", "issues"]));
    let unresolved = data
        .get("unresolved_edges")
        .and_then(Value::as_array)
        .expect("unresolved_edges array");
    let by_target: std::collections::BTreeMap<&str, (&str, Option<&str>)> = unresolved
        .iter()
        .filter_map(|e| {
            let target = e.get("raw_target").and_then(Value::as_str)?;
            let severity = e.get("severity").and_then(Value::as_str)?;
            let policy_name = e.get("policy_name").and_then(Value::as_str);
            Some((target, (severity, policy_name)))
        })
        .collect();
    assert_eq!(
        by_target.get("specs/x.md").copied(),
        Some(("info", Some("ephemeral-specs"))),
        "the declared info row classifies the specs link: {by_target:?}"
    );
    assert_eq!(
        by_target.get("docs/missing.md").copied(),
        Some(("warning", None)),
        "the unmatched docs link takes the warning fallthrough: {by_target:?}"
    );

    let summary = data.get("summary").expect("summary");
    let by_category = summary.get("by_category").expect("by_category");
    assert_eq!(
        by_category.get("ephemeral-specs").and_then(Value::as_u64),
        Some(1),
        "info edges count under their row's name: {summary}"
    );
    assert_eq!(
        by_category.get("unresolved_edge").and_then(Value::as_u64),
        Some(1),
        "only the fallthrough edge is a counted broken edge: {summary}"
    );
    assert_eq!(
        summary.get("total").and_then(Value::as_u64),
        Some(1),
        "the info edge must stay out of total: {summary}"
    );
}

#[test]
fn check_exits_1_on_error_policy_row() {
    // An error-severity policy row turns a matching dangling reference
    // into a gate failure: `unresolved_reference/<name>` at exit 1.
    // Without the row, the same link is a triage item — check passes.
    let tmp = scratch();
    let gating = r#"
[scope]
include = ["docs/**/*.md"]
[[detection.unresolved_policy]]
name = "broken-docs-link"
cause = "missing"
glob = "docs/**"
severity = "error"
"#;
    fs::write(tmp.path().join("nodex.toml"), gating).unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nSee [gone](docs/missing.md).\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let out = nodex(tmp.path()).arg("check").assert().failure().code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| v.get("rule_id").and_then(Value::as_str)
                == Some("unresolved_reference/broken-docs-link")),
        "the error row's rule must fire: {env}"
    );

    // Same project, no policy table: the dangling link is back on the
    // warning plane and check passes.
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    nodex(tmp.path()).arg("check").assert().success();
}

#[test]
fn check_content_gates_a_proposed_dangling_reference() {
    // Write-gate symmetry: the same error row that reds a project-wide
    // `check` also refuses a *proposal* that would introduce a matching
    // dangling reference — before the write lands.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
[[detection.unresolved_policy]]
name = "broken-docs-link"
cause = "missing"
glob = "docs/**"
severity = "error"
"#,
    )
    .unwrap();
    let clean = "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    write_doc(root, "docs/a.md", clean);
    nodex(root).arg("build").assert().success();

    // The clean on-disk state passes the gate.
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(clean)
        .assert()
        .success();

    // Proposing an edit that adds a dangling docs/** link is refused.
    let proposed = "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nSee [gone](docs/missing.md).\n";
    let out = nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(proposed)
        .assert()
        .failure()
        .code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| v.get("rule_id").and_then(Value::as_str)
                == Some("unresolved_reference/broken-docs-link")),
        "the proposal-introduced dangling link must red the gate: {env}"
    );
}

#[test]
fn check_content_reports_standing_warnings_a_body_edit_leaves_unchanged() {
    // The dominant maintenance edit: a body-only change to a doc whose
    // committed state already carries a housekeeping warning. The
    // introduced delta (`violations`) is rightly empty — the edit adds
    // nothing — but the node's absolute warning view must still reach an
    // advisory consumer through `standing`, without a second
    // project-wide check.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[detection]\nstale_days = 180\n",
    )
    .unwrap();
    let stale = "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nreviewed: 2020-01-01\n---\n# A\n\nBody.\n";
    write_doc(root, "docs/a.md", stale);
    nodex(root).arg("build").assert().success();

    let proposed = stale.replace("Body.", "Body, edited.");
    let env = run_envelope(
        nodex(root)
            .args(["check", "--content", "docs/a.md=-"])
            .write_stdin(proposed),
    );
    assert_eq!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "a body-only edit introduces nothing: {env}"
    );
    let standing = env
        .pointer("/data/standing")
        .and_then(Value::as_array)
        .expect("content mode must carry the standing view");
    assert!(
        standing.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str) == Some("stale_review")
                && v.get("path").and_then(Value::as_str) == Some("docs/a.md")
        }),
        "the node's pre-existing stale_review must ride `standing`: {env}"
    );
}

#[test]
fn check_content_standing_is_a_superset_of_introduced_warnings() {
    // `standing` is the absolute view — a warning the proposal itself
    // introduces (here: a backdated `reviewed:` the on-disk baseline
    // lacks) appears in BOTH lists by contract: `violations` answers
    // "what did this write add", `standing` answers "what does this
    // doc carry in the proposed state".
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[detection]\nstale_days = 180\n",
    )
    .unwrap();
    let clean = "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nBody.\n";
    write_doc(root, "docs/a.md", clean);
    nodex(root).arg("build").assert().success();

    let proposed = clean.replace("status: active\n", "status: active\nreviewed: 2020-01-01\n");
    let env = run_envelope(
        nodex(root)
            .args(["check", "--content", "docs/a.md=-"])
            .write_stdin(proposed),
    );
    let in_list = |ptr: &str| {
        env.pointer(ptr)
            .and_then(Value::as_array)
            .is_some_and(|vs| {
                vs.iter()
                    .any(|v| v.get("rule_id").and_then(Value::as_str) == Some("stale_review"))
            })
    };
    assert!(
        in_list("/data/violations"),
        "the introduced stale_review must gate-report in violations: {env}"
    );
    assert!(
        in_list("/data/standing"),
        "the same warning must also ride the absolute standing view: {env}"
    );
}

#[test]
fn check_content_batch_resolves_a_cross_proposal_reference() {
    // The reason batch validation exists: a `supersede`-shaped edit that
    // proposes a new document AND the referrer pointing at it must gate as
    // ONE build, so the reference resolves against the sibling proposal
    // instead of reporting a still-dangling link a one-at-a-time gate
    // would. The same error row reds a single proposal but passes the
    // batch.
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
[[detection.unresolved_policy]]
name = "broken-docs-link"
cause = "missing"
glob = "docs/**"
severity = "error"
"#,
    )
    .unwrap();
    write_doc(root, "docs/a.md", "---\ntitle: A\n---\n# A\n");
    nodex(root).arg("build").assert().success();

    let a_links_c = "---\ntitle: A\n---\n# A\n\nSee [C](c.md).\n";
    let c_new = "---\ntitle: C\n---\n# C\n";
    let a_src = root.join("a_new.md");
    let c_src = root.join("c_new.md");
    fs::write(&a_src, a_links_c).unwrap();
    fs::write(&c_src, c_new).unwrap();
    let a_pair = format!("docs/a.md={}", a_src.display());
    let c_pair = format!("docs/c.md={}", c_src.display());

    // One proposal: the link to the not-yet-existing c.md is dangling.
    nodex(root)
        .args(["check", "--content"])
        .arg(&a_pair)
        .assert()
        .failure()
        .code(1);

    // Both proposals in one batch: c.md is in the same overlay, so the
    // reference resolves and the batch is clean.
    let out = nodex(root)
        .args(["check", "--content"])
        .arg(&a_pair)
        .arg("--content")
        .arg(&c_pair)
        .assert()
        .success();
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .is_empty(),
        "the cross-proposal reference must resolve within the batch: {env}"
    );
}

#[test]
fn check_content_reports_per_proposal_verdicts() {
    // Every `--content` pair yields one verdict — including a clean or
    // out-of-scope proposal — so a per-proposal reader never sees a silent
    // green. The introduced violations live once in the flat list, keyed
    // by path; `proposals` only enumerates path / in_scope / has_path_errors.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\"]\nterminal = []\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\ntitle: A\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // a.md introduces a bad status (error); b.md is clean; out/x.md is
    // out of scope.
    let bad = "---\ntitle: A\nstatus: rogue\n---\n# A\n";
    let clean = "---\ntitle: B\nstatus: active\n---\n# B\n";
    let bad_src = root.join("bad.md");
    let clean_src = root.join("clean.md");
    fs::write(&bad_src, bad).unwrap();
    fs::write(&clean_src, clean).unwrap();
    let out = nodex(root)
        .args(["check", "--content"])
        .arg(format!("docs/a.md={}", bad_src.display()))
        .arg("--content")
        .arg(format!("docs/b.md={}", clean_src.display()))
        .arg("--content")
        .arg(format!("out/x.md={}", clean_src.display()))
        .assert()
        .failure()
        .code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    let proposals = env
        .pointer("/data/proposals")
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(proposals.len(), 3, "one verdict per proposal: {env}");
    let verdict = |path: &str| {
        proposals
            .iter()
            .find(|p| p.get("path").and_then(Value::as_str) == Some(path))
            .unwrap_or_else(|| panic!("missing verdict for {path}: {env}"))
    };
    let a = verdict("docs/a.md");
    assert_eq!(a.get("in_scope"), Some(&Value::Bool(true)));
    assert_eq!(a.get("has_path_errors"), Some(&Value::Bool(true)));
    let b = verdict("docs/b.md");
    assert_eq!(b.get("in_scope"), Some(&Value::Bool(true)));
    assert_eq!(b.get("has_path_errors"), Some(&Value::Bool(false)));
    let x = verdict("out/x.md");
    assert_eq!(
        x.get("in_scope"),
        Some(&Value::Bool(false)),
        "out-of-scope proposal is reported, not silently dropped: {env}"
    );
    assert_eq!(x.get("has_path_errors"), Some(&Value::Bool(false)));
}

#[test]
fn check_content_batch_invocation_guards_are_typed_config_errors() {
    // The three write-seam guards return CONFIG_ERROR, never a panic or a
    // silent first-wins: a pair without `=`, a repeated target path, and a
    // second stdin source (one stream).
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(root, "docs/a.md", "---\ntitle: A\n---\n# A\n");
    nodex(root).arg("build").assert().success();

    let code_of = |out: &std::process::Output| -> String {
        let v: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
        v.pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };

    // No '=' in the pair.
    let out = nodex(root)
        .args(["check", "--content", "docs/a.md"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(code_of(&out), "CONFIG_ERROR");

    // Duplicate target path.
    let out = nodex(root)
        .args([
            "check",
            "--content",
            "docs/a.md=-",
            "--content",
            "docs/a.md=-",
        ])
        .output()
        .unwrap();
    assert_eq!(code_of(&out), "CONFIG_ERROR");

    // Two stdin sources.
    let out = nodex(root)
        .args([
            "check",
            "--content",
            "docs/a.md=-",
            "--content",
            "docs/b.md=-",
        ])
        .write_stdin("x")
        .output()
        .unwrap();
    assert_eq!(code_of(&out), "CONFIG_ERROR");
}

#[test]
fn schema_require_explicit_reds_an_inferred_status_end_to_end() {
    // Full wiring: the parser records `inferred_fields`, the conditionally
    // registered `explicit_field` rule reds a document that left `status`
    // to inference, and an authored status passes — the config-driven
    // replacement for a consumer-side "missing status" lint.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\"]\nterminal = []\n\
         [schema]\nrequire_explicit = [\"status\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(root, "docs/inferred.md", "---\ntitle: A\n---\n# A\n");
    nodex(root).arg("build").assert().success();
    let out = nodex(root).arg("check").assert().failure().code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    let v = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .unwrap();
    assert!(
        v.iter().any(|x| {
            x.get("rule_id").and_then(Value::as_str) == Some("explicit_field")
                && x.pointer("/details/field").and_then(Value::as_str) == Some("status")
        }),
        "inferred status must red explicit_field: {env}"
    );

    // Author the status → clean.
    write_doc(
        root,
        "docs/inferred.md",
        "---\ntitle: A\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();
}

#[test]
fn scaffold_refuses_a_target_outside_the_project_scope() {
    // A scaffolded file the scan would never admit is a document the
    // graph can never see — written once, invisible forever. The write
    // seam probes the same scope authority the build uses and refuses.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Loose"])
        .args(["--path", "notes/loose.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        !root.join("notes/loose.md").exists(),
        "refused scaffold must write nothing"
    );

    // The same doc inside scope scaffolds fine.
    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Kept"])
        .args(["--path", "docs/kept.md"])
        .assert()
        .success();
}

#[test]
fn scaffold_refuses_a_filename_its_own_naming_rule_would_reject() {
    // Self-consistency: scaffold derives the filename from the title, but
    // a non-sequential `rules.naming` pattern the slug can't satisfy would
    // be written and then flagged by the project's own filename_pattern
    // check. Refuse instead — the caller supplies a conforming --path.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**\"\nkind = \"generic\"\n\
         [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^\\\\d{4}-[a-z0-9-]+\\\\.md$\"\n",
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "My New Doc"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(msg.contains("rules.naming"), "names the rule: {msg}");
    assert!(
        !root.join("docs/my-new-doc.md").exists(),
        "refused scaffold writes nothing"
    );

    // A conforming --path scaffolds, and the result passes check.
    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "My New Doc"])
        .args(["--path", "docs/0042-my-doc.md"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    let check = run_envelope(nodex(root).arg("check"));
    assert_eq!(
        check.pointer("/data/total").and_then(Value::as_i64),
        Some(0)
    );
}

/// The project a rename produces has to pass the project's own `check`,
/// and *only* what the project's own config makes an error may refuse it.
///
/// A locked referrer cannot be repointed, so its reference goes stale. Under
/// `[[detection.unresolved_policy]]` mapping `missing` to `error` that is an
/// Error-severity violation the rename introduces — refuse, while the tree is
/// still untouched. Under the default policy the same stale reference is a
/// warning-plane edge `check` passes, so the same rename must succeed: both
/// halves run against one fixture shape, because a gate that only ever
/// refuses is as wrong as one that never does.
#[test]
fn rename_refuses_exactly_the_moves_the_projects_own_check_would_red() {
    fn fixture(policy: &str) -> TempDir {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            format!(
                "[scope]\ninclude = [\"docs/**/*.md\"]\n\
                 [kinds]\nallowed = [\"generic\"]\n\
                 [statuses]\nallowed = [\"active\", \"archived\"]\n\
                 terminal = [\"archived\"]\ninitial = \"active\"\n\
                 [rules]\nimmutable_baseline = \"HEAD\"\n\
                 [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n{policy}"
            ),
        )
        .unwrap();
        write_doc(
            root,
            "docs/target.md",
            "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
        );
        write_doc(
            root,
            "docs/ref.md",
            "---\nid: ref\ntitle: R\nkind: generic\nstatus: archived\n---\nsee [t](target.md)\n",
        );
        {
            let git = git_runner(root);
            git(&["init", "-q"]);
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "base"]);
        }
        nodex(root).arg("build").assert().success();
        tmp
    }

    // The referrer is frozen at the baseline, so the rewrite is refused
    // either way — what differs is only what the project calls the stale
    // reference it leaves.
    let erroring = fixture(
        "[[detection.unresolved_policy]]\nname = \"broken_link\"\n\
         cause = \"missing\"\nseverity = \"error\"\n",
    );
    let root = erroring.path();
    let output = nodex(root)
        .args(["rename", "docs/target.md", "docs/moved.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "the move is refused");
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("unresolved_reference/broken_link"),
        "names the rule the project's own check would fire: {msg}"
    );
    assert!(
        root.join("docs/target.md").exists() && !root.join("docs/moved.md").exists(),
        "a refused rename moves nothing"
    );
    nodex(root).arg("check").assert().success();

    // Same shape, default policy: the stale reference is a warning-plane
    // edge, so the rename lands and says what it could not repoint.
    let permitting = fixture("");
    let root = permitting.path();
    let env = run_envelope(nodex(root).args(["rename", "docs/target.md", "docs/moved.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("docs/ref.md") && w.contains("body_immutable/frozen")),
        "the skipped rewrite is named: {warnings:?}"
    );
    assert!(root.join("docs/moved.md").exists(), "the move landed");
    nodex(root).arg("check").assert().success();
}

/// A path the graph does not carry today can land somewhere it does, and the
/// document that arrives is one the rules govern. The move answers for it.
#[test]
fn rename_answers_for_a_document_it_moves_into_scope() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [schema]\nrequired = [\"ticket\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/seed.md",
        "---\nid: seed\ntitle: S\nkind: generic\nstatus: active\nticket: T-1\n---\n# S\n",
    );
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/x.md"), "bare untracked note\n").unwrap();

    let output = nodex(root)
        .args(["rename", "notes/x.md", "docs/x.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(msg.contains("required_field"), "names the rule: {msg}");
    assert!(root.join("notes/x.md").exists(), "the source stays put");

    // The same file moved where the graph does not reach is not the rules'
    // business, and the seam must not invent one for it.
    nodex(root)
        .args(["rename", "notes/x.md", "notes/y.md"])
        .assert()
        .success();
}

/// A rule can only refuse a move it would actually fire on. `rules.naming`
/// judges documents in the graph, so a file the scan never admits — before
/// the move or after — is none of its business.
#[test]
fn rename_does_not_apply_a_rule_to_a_file_the_graph_never_sees() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [[rules.naming]]\nglob = \"**/*.md\"\npattern = \"^[a-z]+\\\\.md$\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/x.md"), "untracked\n").unwrap();

    nodex(root)
        .args(["rename", "notes/x.md", "notes/Y_Z.md"])
        .assert()
        .success();
    nodex(root).arg("check").assert().success();

    // The same rule on a document the graph does carry still refuses, and
    // for the reason the gate found rather than any refusal at all.
    let output = nodex(root)
        .args(["rename", "docs/a.md", "docs/A_B.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        env.pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("filename_pattern")
    );
    assert!(root.join("docs/a.md").exists());
}

/// A violation the project already carried never refuses a mutation — the
/// limit that keeps a completeness gate from becoming a wall.
///
/// The delta pairs findings by what a document *is*, not by where it sits: a
/// move relocates the document and `check` says the same sentence about it
/// afterwards, so pairing on the path would make every move of an
/// already-flagged document look like a fresh offence and lock a red project
/// out of the very commands that fix it.
#[test]
fn a_violation_the_project_already_carried_never_refuses_a_mutation() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [schema]\nrequired = [\"owner\"]\n",
    )
    .unwrap();
    // Every document is missing `owner`, so the whole project is red before
    // anything is asked of it.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\nrelated: [b]\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();
    assert_eq!(
        nodex(root)
            .arg("check")
            .output()
            .expect("ran")
            .status
            .code(),
        Some(1),
        "the fixture is red before any mutation"
    );

    nodex(root)
        .args(["rename", "docs/a.md", "docs/moved.md"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    nodex(root).args(["retarget", "b", "a"]).assert().success();
    nodex(root).arg("build").assert().success();
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["lifecycle", "review", "a"])
        .assert()
        .success();

    // Still red, in the same way: the mutations neither fixed nor worsened it.
    let env = envelope_of(nodex(root).arg("check"));
    let rules: Vec<&str> = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter_map(|v| v["rule_id"].as_str())
        .collect();
    assert_eq!(rules, ["required_field", "required_field"], "{env}");
}

/// The scan and the graph describe the same document from the same bytes.
///
/// A `conditional_exclude` rule asks whether a parent is terminal. The graph
/// gives a document that declares no status the project's initial one, so a
/// scan reading "declares none" as "not terminal" disagrees with it — and
/// under a config whose initial status *is* terminal they disagree about
/// every bare document. `migrate` then writes the status the document
/// already had and flips the scan's verdict, turning a green `check` red on
/// a command that reported success.
#[test]
fn the_scan_reads_a_documents_status_the_way_the_graph_assigns_it() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\n\
         parent_glob = \"docs/spec/index.md\"\n\
         child_glob = \"docs/spec/notes/**/*.md\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"done\"]\nterminal = [\"done\"]\ninitial = \"done\"\n\
         [[detection.unresolved_policy]]\n\
         name = \"gone\"\ncause = \"excluded_from_scope\"\nseverity = \"error\"\n",
    )
    .unwrap();
    // The parent declares no status, so the graph builds it as `done` — the
    // project's initial status, which is terminal here.
    write_doc(root, "docs/spec/index.md", "# Bare Parent\n");
    write_doc(
        root,
        "docs/spec/notes/n.md",
        "---\nid: note\ntitle: N\nkind: generic\nstatus: done\n---\n# N\n",
    );
    write_doc(
        root,
        "docs/other.md",
        "---\nid: other\ntitle: O\nkind: generic\nstatus: done\n---\nsee [n](spec/notes/n.md)\n",
    );

    let rules = |root: &std::path::Path| -> Vec<String> {
        envelope_of(nodex(root).arg("check"))
            .pointer("/data/violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter_map(|v| v["rule_id"].as_str().map(str::to_string))
            .collect()
    };
    // The child is excluded from the start, because the parent already is
    // terminal — the reference into it is red before anything is written.
    let before = rules(root);
    assert_eq!(before, ["unresolved_reference/gone"], "{before:?}");

    // Writing the status the document already had changes nothing.
    nodex(root).args(["migrate", "--apply"]).assert().success();
    assert_eq!(rules(root), before, "migrate introduced a violation");
}

/// An include pattern globset matches scans the documents it matches — and
/// every plane says the same thing about them.
///
/// The hidden-path guard read the pattern's text where it needed globset's
/// answer, so a pattern naming a dotted segment without spelling it as a
/// leading dot matched in globset and scanned nothing. That is a whole
/// corpus missing with the gate green over it: `check` passes on documents
/// it never read, `rename` reports a completed rewrite while leaving
/// references dangling, and `scaffold` refuses a path that is squarely
/// inside the scope it names. The class is spelled here as a character
/// class rather than a backslash escape, which globset reads as a path
/// separator wherever `\` is one.
#[test]
fn an_include_pattern_globset_matches_is_one_the_scan_reads() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = ['[.]dotted/**/*.md']\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        ".dotted/target.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    write_doc(
        root,
        ".dotted/referrer.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[T](target.md)\n",
    );

    let build = run_envelope(nodex(root).arg("build"));
    assert_eq!(
        build.pointer("/data/nodes").and_then(Value::as_i64),
        Some(2),
        "the class spelling names the same directory: {build}"
    );

    // The write plane reads the same scope: the move repoints the reference
    // instead of reporting success over a corpus it never saw.
    let env = run_envelope(nodex(root).args(["rename", ".dotted/target.md", ".dotted/renamed.md"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_i64),
        Some(1),
        "{env}"
    );
    assert!(
        fs::read_to_string(root.join(".dotted/referrer.md"))
            .unwrap()
            .contains("(renamed.md)")
    );
    nodex(root)
        .args([
            "scaffold",
            "--kind",
            "generic",
            "--title",
            "New",
            "--path",
            ".dotted/new.md",
            "--dry-run",
        ])
        .assert()
        .success();

    // And a pattern that merely *matches* a hidden path still does not opt
    // it in — the default the guard exists to keep.
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    let build = run_envelope(nodex(root).args(["build", "--full"]));
    assert_eq!(
        build.pointer("/data/nodes").and_then(Value::as_i64),
        Some(0),
        "a greedy pattern opts no hidden path in: {build}"
    );
}

/// A refusal names the document to go and fix.
///
/// A batch refusal is where that matters: two referrers the seam could not
/// repoint yield two findings whose rule and message are identical, and only
/// the document tells them apart.
#[test]
fn a_refusal_names_the_document_each_finding_is_about() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         [[detection.unresolved_policy]]\nname = \"broken_link\"\n\
         cause = \"missing\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/target.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    for id in ["one", "two"] {
        write_doc(
            root,
            &format!("docs/ref-{id}.md"),
            &format!(
                "---\nid: {id}\ntitle: R\nkind: generic\nstatus: archived\n---\nsee [t](target.md)\n"
            ),
        );
    }
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["rename", "docs/target.md", "docs/moved.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    for referrer in ["docs/ref-one.md", "docs/ref-two.md"] {
        assert!(msg.contains(referrer), "names {referrer}: {msg}");
    }
}

/// A field the action overwrites is repaired by the write, and one it does
/// not touch is left alone rather than made a reason to refuse.
#[test]
fn lifecycle_repairs_the_field_it_writes_and_leaves_the_others() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\nreviewed: not-a-date\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\ncreated: not-a-date\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args(["lifecycle", "review", "a"])
        .assert()
        .success();
    assert!(
        !fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("not-a-date"),
        "review rewrote the field it writes"
    );

    nodex(root)
        .args(["lifecycle", "review", "b"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(root.join("docs/b.md"))
            .unwrap()
            .contains("created: not-a-date"),
        "a field the action does not write is untouched, not a refusal"
    );
}

/// A lock is about the record standing at a path, never about where the
/// bytes arriving there came from.
///
/// `rename` asked the baseline only when the graph carried the source, so a
/// document authored outside the scope could be moved onto a frozen record's
/// path and replace it — the seam reporting success while `check` went red on
/// the lock it had never consulted.
#[test]
fn a_move_onto_a_frozen_record_is_refused_however_the_source_arrived() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: X\ntitle: X\nkind: generic\nstatus: archived\n---\nORIGINAL\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    // The frozen document is gone from the working tree, and a replacement
    // for its path is authored where the graph does not reach.
    fs::remove_file(root.join("docs/a.md")).unwrap();
    write_doc(
        root,
        "drafts/x.md",
        "---\nid: X\ntitle: X\nkind: generic\nstatus: archived\n---\nREPLACED\n",
    );
    let output = nodex(root)
        .args(["rename", "drafts/x.md", "docs/a.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "the move is refused");
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("body_immutable/frozen"),
        "names the lock: {msg}"
    );
    assert!(root.join("drafts/x.md").exists() && !root.join("docs/a.md").exists());
}

/// A guard in front of the gate refuses a strict subset of it, or it is the
/// gate's contradiction rather than its shortcut.
///
/// Every `set` writes `updated`, so a `cross_field` predicate keyed on it
/// governed every transition — and the guard, reading only "is the predicate
/// keyed on a field this action writes", refused a document that already
/// failed the same rule. `check --content` on the resulting bytes reported
/// nothing wrong, because the violation was not introduced.
#[test]
fn a_transition_answers_for_what_it_introduces_not_for_what_it_inherits() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [schema.enums]\nchangelog = [\"yes\", \"no\"]\n\
         [[schema.cross_field]]\nwhen = \"updated exists\"\nrequire = \"changelog\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\nupdated: 2020-01-01\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();
    assert_eq!(
        nodex(root)
            .arg("check")
            .output()
            .expect("ran")
            .status
            .code(),
        Some(1),
        "the document already fails the rule the transition is keyed on"
    );

    nodex(root)
        .args(["lifecycle", "set", "a", "--status", "active"])
        .assert()
        .success();

    // A transition that *introduces* the same rule's violation still refuses.
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        root,
        "docs/c.md",
        "---\nid: c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    nodex(root).arg("build").assert().success();
    let output = nodex(root)
        .args(["lifecycle", "supersede", "b", "--to", "c"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("cross_field") && msg.contains("docs/b.md"),
        "names the rule and the document: {msg}"
    );
}

/// The scan reads a status the way the graph assigns it, whichever way the
/// document declines to declare one — absent, empty, or in frontmatter that
/// is not a mapping at all. The last is not a document: the build makes no
/// node for it, so nothing stands there to be a terminal parent.
#[test]
fn a_document_that_declares_no_status_reads_the_same_to_both() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\n\
         parent_glob = \"docs/spec/index.md\"\n\
         child_glob = \"docs/spec/notes/**/*.md\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"done\"]\nterminal = [\"done\"]\ninitial = \"done\"\n\
         [[detection.unresolved_policy]]\n\
         name = \"gone\"\ncause = \"excluded_from_scope\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/spec/notes/n.md",
        "---\nid: note\ntitle: N\nkind: generic\nstatus: done\n---\n# N\n",
    );
    write_doc(
        root,
        "docs/other.md",
        "---\nid: other\ntitle: O\nkind: generic\nstatus: done\n---\nsee [n](spec/notes/n.md)\n",
    );

    let rules = || -> Vec<String> {
        envelope_of(nodex(root).arg("check"))
            .pointer("/data/violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter_map(|v| v["rule_id"].as_str().map(str::to_string))
            .collect()
    };
    // Absent and empty are the same fact: the graph fills the initial status,
    // which is terminal here, so the children are excluded and the reference
    // into them is reported.
    for parent in ["# Bare Parent\n", "---\nstatus: \"\"\n---\n# Parent\n"] {
        write_doc(root, "docs/spec/index.md", parent);
        assert_eq!(rules(), ["unresolved_reference/gone"], "parent: {parent:?}");
    }
    // Frontmatter that is not a mapping produces no node at all, so no
    // parent stands there and its siblings stay in scope.
    write_doc(root, "docs/spec/index.md", "---\n- a\n---\n# Parent\n");
    assert_eq!(rules(), ["parse_failure"]);
}

/// A finding no document owns is identified by its cause, not by wherever it
/// happens to point.
///
/// A cycle names its ring's first member so the operator has somewhere to
/// look, and that pointer moves with a rename the cycle is indifferent to —
/// so pairing on it made moving any member of a pre-existing cycle look like
/// closing a fresh one. The ring itself is the identity, and it is rendered
/// from a fixed starting point so two passes over one project agree.
#[test]
fn a_move_inside_a_pre_existing_cycle_is_not_a_new_cycle() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\nimplements: [b]\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\nimplements: [a]\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();
    assert_eq!(
        nodex(root)
            .arg("check")
            .output()
            .expect("ran")
            .status
            .code(),
        Some(1),
        "the cycle is there before anything is moved"
    );

    nodex(root)
        .args(["rename", "docs/a.md", "docs/c.md"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    let env = envelope_of(nodex(root).arg("check"));
    assert_eq!(
        cycled_documents(&env),
        ["a", "b"],
        "the same two documents, one of them under a new filename"
    );
}

/// A ring has no first member, and the repeat that closes it belongs to the
/// rendering rather than to the walk.
///
/// The traversal closed the ring before it was rotated, so a walk entering
/// the same cycle at a different member left the stale repeat stranded
/// mid-sequence (`a → b → b → c`). Two renderings of one untouched cycle then
/// compared unequal, and every write seam read the difference as a cycle the
/// mutation had just closed.
#[test]
fn a_cycle_entered_from_a_new_member_is_not_a_new_cycle() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    // `aaa-entry` is walked first and decides where the ring is entered; the
    // ring needs three members for a rotation to move anything.
    write_doc(
        root,
        "docs/aaa-entry.md",
        "---\nid: aaa-entry\ntitle: Entry\nkind: generic\nstatus: active\nimplements: [spur]\n---\n# Entry\n",
    );
    write_doc(
        root,
        "docs/spur.md",
        "---\nid: spur\ntitle: Spur\nkind: generic\nstatus: active\n---\n# Spur\n",
    );
    for (id, next) in [
        ("ring-a", "ring-b"),
        ("ring-b", "ring-c"),
        ("ring-c", "ring-a"),
    ] {
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!(
                "---\nid: {id}\ntitle: {id}\nkind: generic\nstatus: active\nimplements: [{next}]\n---\n# {id}\n"
            ),
        );
    }
    nodex(root).arg("build").assert().success();
    let ring_of = cycle_edges_of;
    let before = ring_of(&envelope_of(nodex(root).arg("check")));
    assert_eq!(
        before,
        ["ring-a → ring-b", "ring-b → ring-c", "ring-c → ring-a"],
        "each member names the edge of its own that stays in the ring"
    );

    // Repointing the spur moves where the walk enters the ring, and nothing
    // else: the cycle is the same three edges before and after.
    nodex(root)
        .args(["retarget", "spur", "ring-b"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    assert_eq!(
        ring_of(&envelope_of(nodex(root).arg("check"))),
        before,
        "one cycle renders one way, whichever member the walk reached first"
    );
}

/// A project holding a tangle of mutually-implementing documents: `ring-a`
/// reaches `ring-c` directly and the long way round through `ring-b`, so two
/// rings run over the same three documents. `000-entry` sorts first, so it
/// is walked first and decides which member the walk reaches before any
/// other.
fn tangle_with_a_chord(root: &std::path::Path, entry_implements: &str) {
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/000-entry.md",
        &format!(
            "---\nid: 000-entry\ntitle: Entry\nkind: generic\nstatus: active\nimplements: [{entry_implements}]\n---\n# Entry\n"
        ),
    );
    write_doc(
        root,
        "docs/spur.md",
        "---\nid: spur\ntitle: Spur\nkind: generic\nstatus: active\n---\n# Spur\n",
    );
    for (id, targets) in [
        ("ring-a", "ring-b, ring-c"),
        ("ring-b", "ring-c"),
        ("ring-c", "ring-a"),
    ] {
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!(
                "---\nid: {id}\ntitle: {id}\nkind: generic\nstatus: active\nimplements: [{targets}]\n---\n# {id}\n"
            ),
        );
    }
    nodex(root).arg("build").assert().success();
}

/// Every document a `check` reports as caught in a cycle, in order.
fn cycled_documents(env: &Value) -> Vec<String> {
    cycle_findings(env)
        .filter_map(|v| v["details"]["member"].as_str())
        .map(str::to_string)
        .collect()
}

/// Each caught document with the in-region edge the finding names, so a test
/// can state the whole shape: who is caught, and the route out of each.
fn cycle_edges_of(env: &Value) -> Vec<String> {
    cycle_findings(env)
        .map(|v| {
            format!(
                "{} → {}",
                v["details"]["member"].as_str().expect("member"),
                v["details"]["via"].as_str().expect("via")
            )
        })
        .collect()
}

fn cycle_findings(env: &Value) -> impl Iterator<Item = &Value> {
    env.pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter(|v| v["rule_id"] == "acyclic_relation")
}

/// Every document in a region of mutually-reachable documents is flagged, and
/// each is shown one edge of its own that stays inside the region — follow
/// them and you have walked a ring.
#[test]
fn a_tangle_flags_every_document_in_it_with_an_edge_to_cut() {
    let tmp = scratch();
    let root = tmp.path();
    tangle_with_a_chord(root, "spur");
    let env = envelope_of(nodex(root).arg("check"));
    assert_eq!(
        cycle_edges_of(&env),
        ["ring-a → ring-b", "ring-b → ring-c", "ring-c → ring-a"]
    );
}

/// How many cycles a tangle holds is a fact about its edges, not about where
/// a walk came into it.
///
/// A walk retires a node the first time any root reaches it, so the chord
/// `ring-a → ring-c` was reported or skipped according to which member the
/// walk entered on. Repointing a spur that touches no edge of the tangle
/// moved that entry point, a second ring appeared, and every write seam read
/// it as a cycle the mutation had just closed — refusing a mutation that
/// closed nothing.
#[test]
fn a_chord_inside_a_tangle_is_not_a_cycle_the_walk_arrived_with() {
    let tmp = scratch();
    let root = tmp.path();
    tangle_with_a_chord(root, "spur");
    let before = cycle_edges_of(&envelope_of(nodex(root).arg("check")));

    // Nothing here is an edge of the tangle: the spur is a leaf, and the
    // document repointed at it is reached by nobody.
    nodex(root)
        .args(["retarget", "spur", "ring-b"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    assert_eq!(
        cycle_edges_of(&envelope_of(nodex(root).arg("check"))),
        before,
        "the tangle is the edges it is made of, whatever reached it first"
    );
}

/// Where a finding sits in a body is not which finding it is.
///
/// The pairing key carried the body line number, so inserting a paragraph
/// renumbered every finding below it and the gate answered for offences that
/// were already there — a write seam refused an edit that only added prose.
#[test]
fn text_added_above_a_flagged_line_is_not_a_flag_the_edit_added() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[rules.body_line]]\nname = \"step\"\n\
         pattern = '^- (?<state>[a-z]+): .+$'\n\
         [rules.body_line.enums]\nstate = [\"todo\", \"done\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n\n- todo: fine\n- bogus: bad\n",
    );
    nodex(root).arg("build").assert().success();

    let proposal = root.join("with-a-paragraph.md");
    fs::write(
        &proposal,
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n\nAn added sentence.\n\n- todo: fine\n- bogus: bad\n",
    )
    .unwrap();
    let env = envelope_of(nodex(root).args([
        "check",
        "--content",
        &format!("docs/a.md={}", proposal.display()),
    ]));
    assert_eq!(
        env["data"]["total"], 0,
        "the offending line is the one it always was, two lines lower: {env}"
    );
}

/// Where an unresolved reference sits in a body is not which reference it is.
///
/// `Edge::location` is `L<n>`, and it was part of the pairing key, so adding
/// prose above a link that already dangled made it read as a reference the
/// edit had just stranded.
#[test]
fn text_added_above_a_dangling_link_is_not_a_link_the_edit_stranded() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[detection.unresolved_policy]]\nname = \"dangling\"\n\
         cause = \"missing\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n\nSee [gone](./nowhere.md).\n",
    );
    nodex(root).arg("build").assert().success();

    let proposal = root.join("with-a-paragraph.md");
    fs::write(
        &proposal,
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n\nAn added sentence.\n\nSee [gone](./nowhere.md).\n",
    )
    .unwrap();
    let env = envelope_of(nodex(root).args([
        "check",
        "--content",
        &format!("docs/a.md={}", proposal.display()),
    ]));
    assert_eq!(
        env["data"]["total"], 0,
        "the same link to the same missing file, two lines lower: {env}"
    );
}

/// A cycle is the documents caught in it, so a document the edit drags in is
/// a cycle the edit made bigger.
///
/// The finding was paired on the ring it is shown as, and the shortest ring
/// through a region does not have to pass through a document the region just
/// gained: `a → b → a` stayed the shortest route after `c` joined them, the
/// two findings compared equal, and the gate reported nothing to answer for
/// while `check` went on naming two documents out of three.
#[test]
fn a_document_dragged_into_a_cycle_is_a_cycle_that_gained_one() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    // `a` and `b` reach each other; `c` reaches them and is not reached back;
    // `spur` is the leaf `b` also names, and repointing it is the whole edit.
    for (id, targets) in [("a", "b"), ("b", "a, spur"), ("c", "a"), ("spur", "")] {
        let implements = if targets.is_empty() {
            String::new()
        } else {
            format!("implements: [{targets}]\n")
        };
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!(
                "---\nid: {id}\ntitle: {id}\nkind: generic\nstatus: active\n{implements}---\n# {id}\n"
            ),
        );
    }
    nodex(root).arg("build").assert().success();
    assert_eq!(
        cycled_documents(&envelope_of(nodex(root).arg("check"))),
        ["a", "b"],
        "`c` reaches the pair and is not reached back, so it is outside"
    );

    // Repointing the leaf onto `c` gives `b → c`, and `c → a` was already
    // there: `c` now reaches the pair and is reached back.
    let env = envelope_of(nodex(root).args(["retarget", "spur", "c"]));
    assert_eq!(env["error"]["code"], "CONTENT_VIOLATIONS");
    let message = env["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("\"c\""),
        "names the document the edit dragged in: {message}"
    );
}

/// A name held by a link pointing at nothing is a name that is taken.
///
/// `scaffold` asked whether its destination *resolved*, so a dangling link
/// fell through to the write, where the root guard refused the link's target
/// and reported a path escaping the project — of a path plainly inside it.
#[cfg(unix)]
#[test]
fn scaffold_does_not_take_a_name_a_dangling_link_already_holds() {
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    unix_fs::symlink("../nowhere/target.md", root.join("docs/taken.md")).unwrap();

    let env = envelope_of(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Taken",
        "--path",
        "docs/taken.md",
    ]));
    assert_eq!(env["error"]["code"], "ALREADY_EXISTS");
    assert!(
        fs::symlink_metadata(root.join("docs/taken.md"))
            .expect("the entry survives")
            .is_symlink(),
        "the link the operator made is still theirs"
    );
}

/// A document that did not match its filename pattern and still does not has
/// not newly broken anything, whatever it is called now.
///
/// The offending filename was the finding's identity, so renaming one
/// unmatched name to another minted a finding and `rename` refused a move
/// whose before/after reports say the same thing.
#[test]
fn a_rename_between_two_unmatched_names_introduces_no_finding() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[rules.naming]]\n\
         glob = \"docs/**/*.md\"\npattern = '^[0-9]{4}-[a-z-]+\\.md$'\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/BadName.md",
        "---\nid: bad\ntitle: Bad\nkind: generic\nstatus: active\n---\n# Bad\n",
    );
    nodex(root).arg("build").assert().success();
    let flagged = |env: &Value| -> usize {
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter(|v| {
                v["rule_id"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("filename_pattern"))
            })
            .count()
    };
    assert_eq!(flagged(&envelope_of(nodex(root).arg("check"))), 1);

    nodex(root)
        .args(["rename", "docs/BadName.md", "docs/StillBad.md"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    assert_eq!(
        flagged(&envelope_of(nodex(root).arg("check"))),
        1,
        "still one document not matching, under a different name"
    );
}

/// Freeing a document from a tangle is not a cycle, and neither is cutting
/// one tangle into two.
///
/// The finding used to be the region, so a repair that made a region smaller
/// produced a region the project had never carried: it paired against
/// nothing and every write seam read it as a cycle the repair had closed.
/// Shrinking and splitting are the two edits a tangled graph most needs, and
/// both were refused.
#[test]
fn a_repair_that_makes_a_tangle_smaller_closes_no_cycle() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    // aa ↔ bb, and cc hangs off bb and reaches back: all three are caught.
    for (id, targets) in [("aa", "bb"), ("bb", "aa, cc"), ("cc", "aa")] {
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!(
                "---\nid: {id}\ntitle: {id}\nkind: generic\nstatus: active\nimplements: [{targets}]\n---\n# {id}\n"
            ),
        );
    }
    nodex(root).arg("build").assert().success();
    assert_eq!(
        cycled_documents(&envelope_of(nodex(root).arg("check"))),
        ["aa", "bb", "cc"]
    );

    // `cc` gives up its only edge. It is free; aa and bb are as tangled as
    // they were.
    let freed = root.join("cc-freed.md");
    fs::write(
        &freed,
        "---\nid: cc\ntitle: cc\nkind: generic\nstatus: active\n---\n# cc\n",
    )
    .unwrap();
    let env = envelope_of(nodex(root).args([
        "check",
        "--content",
        &format!("docs/cc.md={}", freed.display()),
    ]));
    assert_eq!(
        env["data"]["total"], 0,
        "aa and bb were already caught, and cc no longer is: {env}"
    );
}

/// Taking an edge out of a tangle leaves the same documents tangled, so it is
/// not a cycle the edit closed.
///
/// Pairing on the rendered ring made the shortest route the finding's
/// identity, and dropping a chord lengthens that route without freeing
/// anybody: the gate read the longer ring as a cycle that had just appeared
/// and refused an edit that removed an edge.
#[test]
fn dropping_a_chord_is_not_a_cycle_the_edit_closed() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    for (id, targets) in [("a", "b, c"), ("b", "c"), ("c", "a")] {
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!(
                "---\nid: {id}\ntitle: {id}\nkind: generic\nstatus: active\nimplements: [{targets}]\n---\n# {id}\n"
            ),
        );
    }
    nodex(root).arg("build").assert().success();
    assert_eq!(
        cycled_documents(&envelope_of(nodex(root).arg("check"))),
        ["a", "b", "c"],
        "all three reach each other"
    );

    // `a` keeps `b` and drops `c`. The three still reach each other, by the
    // long way round.
    let proposal = root.join("a-without-the-chord.md");
    fs::write(
        &proposal,
        "---\nid: a\ntitle: a\nkind: generic\nstatus: active\nimplements: [b]\n---\n# a\n",
    )
    .unwrap();
    let env = envelope_of(nodex(root).args([
        "check",
        "--content",
        &format!("docs/a.md={}", proposal.display()),
    ]));
    assert_eq!(
        env["data"]["total"], 0,
        "the same documents are tangled, by a longer route: {env}"
    );
}

/// Every default the renderer emits is a YAML scalar it produced, except the
/// one whose text comes from the project — so that is the one that has to be
/// quoted.
///
/// An enum value carrying `: ` rendered a line YAML cannot read, and the
/// document the tool had just written lost its node entirely: a command whose
/// whole claim is that it derives from config turned a missing field into a
/// destroyed document.
#[test]
fn a_config_derived_default_is_written_as_the_value_the_config_declared() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [schema]\nrequired = [\"stage\"]\n\
         [schema.enums]\nstage = [\"draft: early\", \"final\"]\n",
    )
    .unwrap();
    write_doc(root, "docs/bare.md", "# Bare\n");
    let before = envelope_of(nodex(root).arg("check"));
    let rules: Vec<&str> = before
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter_map(|v| v["rule_id"].as_str())
        .collect();
    assert_eq!(rules, ["required_field"]);

    nodex(root).args(["migrate", "--apply"]).assert().success();
    let after = envelope_of(nodex(root).arg("check"));
    assert_eq!(
        after.pointer("/data/violations").and_then(Value::as_array),
        Some(&vec![]),
        "the injection filled the field rather than destroying the document: {after}"
    );

    // And `scaffold`, which shares the renderer, writes the same value.
    let content = run_envelope(
        nodex(root)
            .args(["scaffold", "--kind", "generic", "--title", "New"])
            .args(["--path", "docs/new.md", "--dry-run"]),
    );
    let content = content
        .pointer("/data/content")
        .and_then(Value::as_str)
        .expect("content");
    assert!(content.contains("stage: \"draft: early\""), "{content}");
}

/// `check --content` judges the project the proposal produces, and a rule
/// that probes the filesystem has to see it too — the overlay build's graph
/// describes a project the disk does not hold.
///
/// The proposal here creates a file the disk has no trace of, at a path the
/// scan does not admit. Probing the disk classifies the reference into it as
/// "nothing is there"; probing the proposal classifies it as "something is
/// there the graph excludes" — and this project makes only one of those an
/// error, so the two readings differ in the verdict, not only in the prose.
#[test]
fn the_content_gate_probes_the_project_the_proposal_produces() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[detection.unresolved_policy]]\nname = \"outside\"\n\
         cause = \"excluded_from_scope\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/seed.md",
        "---\nid: seed\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    fs::write(
        root.join("proposed-note.md"),
        "outside the graph, and only in the proposal\n",
    )
    .unwrap();
    assert!(
        !root.join("notes/t.md").exists(),
        "the disk has no trace of it"
    );

    let output = nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .args([
            "--content",
            &format!("notes/t.md={}", root.join("proposed-note.md").display()),
        ])
        .write_stdin(
            "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\nsee [t](../notes/t.md)\n",
        )
        .output()
        .expect("ran");
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let rules: Vec<&str> = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter_map(|v| v["rule_id"].as_str())
        .collect();
    assert_eq!(
        rules,
        ["unresolved_reference/outside"],
        "the probe answered about the project the proposal produces: {env}"
    );
}

/// A hidden segment the pattern names deeper than any lead can line up is
/// still an opt-in — and a greedy sibling never answers for it.
///
/// `**` consumes as many segments as it likes, so reading the pattern
/// position by position cannot identify the component that governs a deeper
/// one. Answering "not opted in" there scanned an empty corpus while globset
/// matched the documents, with `check` green over nothing.
#[test]
fn an_include_naming_a_hidden_segment_deeper_than_its_lead_still_opts_in() {
    for (include, doc, expected) in [
        ("['foo/**/.hidden/**/*.md']", "foo/a/b/.hidden/doc.md", 1),
        ("['**/.obsidian/**/*.md']", "x/y/.obsidian/doc.md", 1),
        ("['*/.dotted/**/*.md']", "a/.dotted/doc.md", 1),
        // One pattern's opt-in is not another's: a greedy sibling neither
        // grants nor withdraws it.
        ("['.claude/**/*.md', '**/*.md']", ".claude/doc.md", 1),
        ("['.claude/**/*.md', '**/*.md']", "sub/.claude/doc.md", 0),
        // And the default the guard keeps: a pattern that merely matches a
        // hidden path does not opt it in, at any depth.
        ("['docs/**/*.md']", "docs/a/.hidden/doc.md", 0),
        ("['**/*.md']", ".dotted/doc.md", 0),
    ] {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            format!("[scope]\ninclude = {include}\n[kinds]\nallowed = [\"generic\"]\n"),
        )
        .unwrap();
        write_doc(
            root,
            doc,
            "---\nid: d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
        );
        let env = run_envelope(nodex(root).arg("build"));
        assert_eq!(
            env.pointer("/data/nodes").and_then(Value::as_i64),
            Some(expected),
            "{include} / {doc}: {env}"
        );
    }
}

/// A proposal that puts a document somewhere puts every directory on the way
/// there too — the write makes them before it makes the file.
///
/// A `covers:` target naming such a directory classified against the tree,
/// where it does not exist yet, so the gate judged a different project from
/// the one the move produces and let a red one through.
#[test]
fn the_gate_sees_the_directories_the_proposal_creates() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[detection.unresolved_policy]]\nname = \"outside\"\n\
         cause = \"excluded_from_scope\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/target.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\ncovers: [newdir]\n---\n# R\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();

    // The move would create `newdir/`, turning a reference that resolves to
    // nothing into one that names a directory the graph excludes.
    let output = nodex(root)
        .args(["rename", "docs/target.md", "newdir/target.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        !root.join("newdir").exists(),
        "a refused move creates nothing"
    );
    nodex(root).arg("check").assert().success();
}

/// A finding's identity is its cause, and a payload that merely locates the
/// cause is not part of it.
///
/// `unique_numbering` carries the files sharing a number so the operator can
/// find them. Renaming one changes that list while the conflict it evidences
/// is untouched — `check` reports the same duplication before and after — so
/// pairing on it refused a move that changed nothing.
#[test]
fn a_move_inside_a_pre_existing_numbering_conflict_is_not_a_new_conflict() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^\\\\d{4}-[a-z]+\\\\.md$\"\n\
         unique = true\n",
    )
    .unwrap();
    for name in ["0001-a.md", "0001-b.md"] {
        write_doc(
            root,
            &format!("docs/{name}"),
            "---\nid: d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
        );
    }
    // Distinct ids, same number.
    write_doc(
        root,
        "docs/0001-b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();
    assert_eq!(
        nodex(root)
            .arg("check")
            .output()
            .expect("ran")
            .status
            .code(),
        Some(1),
        "the conflict is there before anything is moved"
    );

    nodex(root)
        .args(["rename", "docs/0001-a.md", "docs/0001-c.md"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    let env = envelope_of(nodex(root).arg("check"));
    let rules: Vec<&str> = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter_map(|v| v["rule_id"].as_str())
        .collect();
    assert_eq!(rules, ["unique_numbering"], "the same conflict, unmoved");
}

/// Bytes the rules cannot read are refused where the graph reaches, and moved
/// where it does not.
///
/// A proposal models a document as text, so a source that is not text has
/// nothing to propose — and the gate judged a destination holding nothing
/// while `fs::rename` put the bytes there. Inside the scan's reach that is a
/// `parse_failure` the move introduced; outside it, there was never anything
/// for the rules to say.
#[test]
fn a_source_that_holds_no_document_is_refused_only_where_the_graph_reaches() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/seed.md",
        "---\nid: s\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::create_dir_all(root.join("other")).unwrap();
    fs::write(root.join("notes/raw.md"), [0xff, 0xfe, 0x00, b'x']).unwrap();

    let output = nodex(root)
        .args(["rename", "notes/raw.md", "docs/raw.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "into scope, refused");
    assert!(root.join("notes/raw.md").exists());
    nodex(root).arg("check").assert().success();

    // The same file where the graph does not reach is a plain guarded move,
    // and the envelope says the gate had nothing to judge.
    let env = run_envelope(nodex(root).args(["rename", "notes/raw.md", "other/raw.md"]));
    assert!(
        env.get("warnings")
            .and_then(Value::as_array)
            .is_some_and(|ws| ws
                .iter()
                .filter_map(warning_msg)
                .any(|w| w.contains("nothing for the gate to judge"))),
        "{env}"
    );
    assert!(root.join("other/raw.md").exists());
}

/// A record travels under its id, so a frozen one that merely moved has left
/// its path free.
///
/// The destination guard asked the baseline by path alone while its own
/// message asserted the record was gone — so a path a legal rename had
/// already vacated stayed refused, on a mutation `check` reads as nothing at
/// all.
#[test]
fn a_frozen_record_that_moved_leaves_its_path_free() {
    fn fixture() -> TempDir {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
             [statuses]\nallowed = [\"active\", \"archived\"]\n\
             terminal = [\"archived\"]\ninitial = \"active\"\n\
             [rules]\nimmutable_baseline = \"HEAD\"\n\
             [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
        )
        .unwrap();
        write_doc(
            root,
            "docs/b.md",
            "---\nid: X\ntitle: X\nkind: generic\nstatus: archived\n---\n# B\n",
        );
        write_doc(
            root,
            "docs/a.md",
            "---\nid: A\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
        );
        {
            let git = git_runner(root);
            git(&["init", "-q"]);
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "base"]);
        }
        nodex(root).arg("build").assert().success();
        tmp
    }

    // The frozen record moves under its own id, which is legal; its old path
    // now holds nothing frozen.
    let moved = fixture();
    let root = moved.path();
    nodex(root)
        .args(["rename", "docs/b.md", "docs/c.md"])
        .assert()
        .success();
    nodex(root)
        .args(["rename", "docs/a.md", "docs/b.md"])
        .assert()
        .success();
    nodex(root).args(["build", "--full"]).assert().success();
    nodex(root).arg("check").assert().success();

    // The record genuinely gone is still refused.
    let lost = fixture();
    let root = lost.path();
    fs::remove_file(root.join("docs/b.md")).unwrap();
    let output = nodex(root)
        .args(["rename", "docs/a.md", "docs/b.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(msg.contains("body_immutable/frozen"), "{msg}");
    assert!(
        !msg.contains("   "),
        "the message is one sentence, not padding: {msg}"
    );
}

/// A record carried back onto its own path loses nothing, so nothing refuses
/// it.
///
/// The destination guard asked whether the frozen id survives the move — of
/// the project as it stands, where the record is missing whichever document
/// lands there. Restoring the record itself was refused for destroying it,
/// on a move that left the project byte-identical to the baseline.
#[test]
fn a_frozen_record_returning_to_its_own_path_is_not_a_record_lost() {
    fn fixture() -> TempDir {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
             [statuses]\nallowed = [\"active\", \"archived\"]\n\
             terminal = [\"archived\"]\ninitial = \"active\"\n\
             [rules]\nimmutable_baseline = \"HEAD\"\n\
             [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
        )
        .unwrap();
        write_doc(
            root,
            "docs/a.md",
            "---\nid: X\ntitle: X\nkind: generic\nstatus: archived\n---\n# A\n",
        );
        {
            let git = git_runner(root);
            git(&["init", "-q"]);
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "base"]);
        }
        // The record leaves the graph entirely — parked outside scope, where
        // no rule can see it and the path it came from reads as free.
        fs::create_dir_all(root.join("drafts")).unwrap();
        fs::rename(root.join("docs/a.md"), root.join("drafts/x.md")).unwrap();
        tmp
    }

    let restored = fixture();
    let root = restored.path();
    nodex(root)
        .args(["rename", "drafts/x.md", "docs/a.md"])
        .assert()
        .success();
    nodex(root).args(["build", "--full"]).assert().success();
    nodex(root).arg("check").assert().success();

    // A different document landing on the same freed path still replaces
    // frozen history, and is still refused.
    let replaced = fixture();
    let root = replaced.path();
    write_doc(
        root,
        "drafts/y.md",
        "---\nid: Y\ntitle: Y\nkind: generic\nstatus: active\n---\n# Y\n",
    );
    let output = nodex(root)
        .args(["rename", "drafts/y.md", "docs/a.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(msg.contains("body_immutable/frozen"), "{msg}");
}

/// The seam reads a document the way the graph does, or it guards a document
/// nobody else has.
///
/// `FrontmatterEditor` is a line reader: `status: ~` is the text `"~"` to it
/// and YAML null to the parser, which fills `statuses.initial`. Under a
/// terminal initial status the graph reported the document terminal while the
/// seam let a transition out of it.
#[test]
fn lifecycle_reads_the_document_the_graph_holds() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"done\", \"active\"]\nterminal = [\"done\"]\ninitial = \"done\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: ~\n---\n# A\n",
    );
    nodex(root).args(["build", "--full"]).assert().success();
    let env = run_envelope(nodex(root).args(["query", "nodes"]));
    assert_eq!(
        env.pointer("/data/items/0/status").and_then(Value::as_str),
        Some("done"),
        "the graph resolves the null to the initial status: {env}"
    );

    let output = nodex(root)
        .args(["lifecycle", "set", "a", "--status", "active"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_TRANSITION")
    );
}

/// A placeholder's findings are its own to-do list. What it writes over
/// somebody else's document is not.
///
/// `scaffold` advises rather than refuses when every value came from config —
/// a fill-me-in document is the point of it. But the branch keyed on the
/// shape of the *input*, so a `--force` overwrite that rewrote a document
/// under a different id stranded every reference to the old one and reported
/// those Errors as advisories with `ok: true`.
#[test]
fn a_placeholder_scaffold_advises_about_itself_and_refuses_the_rest() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [schema]\nrequired = [\"ticket\"]\n\
         [[detection.unresolved_policy]]\nname = \"gone\"\n\
         cause = \"id_not_found\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/target.md",
        "---\nid: authored\ntitle: T\nkind: generic\nstatus: active\nticket: T-1\n---\n# T\n",
    );
    write_doc(
        root,
        "docs/ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\nticket: T-2\n\
         related: [authored]\n---\n# R\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();

    // Overwriting under the inferred id would strand `docs/ref.md`.
    let output = nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Target"])
        .args(["--path", "docs/target.md", "--force"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        env.pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("docs/ref.md"),
        "names the document it would damage: {env}"
    );
    nodex(root).arg("check").assert().success();

    // And the placeholder's own finding still rides as the advisory it is.
    let env = run_envelope(
        nodex(root)
            .args(["scaffold", "--kind", "generic", "--title", "New"])
            .args(["--path", "docs/new.md"]),
    );
    assert!(
        env.get("warnings")
            .and_then(Value::as_array)
            .is_some_and(|ws| ws
                .iter()
                .filter_map(warning_msg)
                .any(|w| w.contains("docs/new.md") && w.contains("required_field"))),
        "{env}"
    );
}

/// A numbering conflict is between *documents*, and a document keeps its id
/// wherever it sits. Two conflicts that merely share a number are two.
#[test]
fn a_numbering_conflict_is_identified_by_the_documents_in_it() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[rules.naming]]\nglob = \"a/**\"\npattern = \"^\\\\d{4}-[a-z]+\\\\.md$\"\nunique = true\n\
         [[rules.naming]]\nglob = \"b/**\"\npattern = \"^\\\\d{4}-[a-z]+\\\\.md$\"\nunique = true\n",
    )
    .unwrap();
    for (path, id) in [
        ("a/0001-x.md", "ax"),
        ("a/0001-move.md", "am"),
        ("b/0001-y.md", "by"),
        ("b/0001-z.md", "bz"),
    ] {
        write_doc(
            root,
            path,
            &format!("---\nid: {id}\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n"),
        );
    }
    nodex(root).arg("build").assert().success();

    // Moving a member out of one conflict and into the other changes which
    // documents each is between — a different conflict, so it is refused.
    let output = nodex(root)
        .args(["rename", "a/0001-move.md", "b/0001-move.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "the b/ conflict grew");

    // Renaming inside one conflict, keeping its number, changes nothing about
    // which documents are in it.
    nodex(root)
        .args(["rename", "a/0001-move.md", "a/0001-other.md"])
        .assert()
        .success();
}

/// A document that does not parse has no id, so a move is a document
/// breaking at a path where nothing was broken before.
///
/// `rename`'s work is to move a document and repoint what refers to it, and
/// it can do neither for a document it cannot read a name out of. Nothing
/// refuses this separately: the gate every write asks answers it, because
/// the failure the move lands at the destination is one the project did not
/// carry.
#[test]
fn a_document_that_does_not_parse_has_no_name_to_be_moved_under() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    // Frontmatter that is not a mapping: the document has no node.
    write_doc(root, "docs/a.md", "---\n- a\n---\n# A\n");
    nodex(root).arg("build").assert().success();

    let env = envelope_of(nodex(root).args(["rename", "docs/a.md", "docs/sub/a.md"]));
    assert_eq!(env["error"]["code"], "CONTENT_VIOLATIONS");
    let message = env["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("docs/sub/a.md") && message.contains("parse_failure"),
        "names the document the move would break and why: {message}"
    );
    assert!(
        !root.join("docs/sub/a.md").exists(),
        "a refused move leaves the tree alone"
    );
}

/// A proposal that repairs one document and breaks another is answerable for
/// the one it broke, whatever bytes it broke it with.
///
/// A parse failure was paired by its content digest alone, so the same
/// malformed bytes arriving at a second document cancelled against the first
/// document's repair. `check --content` reported a clean proposal, and the
/// `check` that followed applying it named a document the gate never
/// mentioned.
#[test]
fn a_repair_does_not_pay_for_breaking_a_different_document() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    let malformed = "---\n- a\n---\n# A\n";
    write_doc(root, "docs/a.md", malformed);
    write_doc(
        root,
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();

    let repaired = root.join("repaired.md");
    fs::write(
        &repaired,
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    )
    .unwrap();
    // The very bytes `docs/a.md` is broken with, now proposed for `docs/b.md`.
    let broken = root.join("broken.md");
    fs::write(&broken, malformed).unwrap();

    let env = envelope_of(nodex(root).args([
        "check",
        "--content",
        &format!("docs/a.md={}", repaired.display()),
        "--content",
        &format!("docs/b.md={}", broken.display()),
    ]));
    let broken_paths: Vec<&str> = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter(|v| v["rule_id"] == "parse_failure")
        .filter_map(|v| v["path"].as_str())
        .collect();
    assert_eq!(
        broken_paths,
        ["docs/b.md"],
        "the document the proposal broke, not the one it repaired"
    );
}

/// A file that could not be read has no bytes to be identified by, so it is
/// identified by which file it is.
///
/// The builder records the empty digest for a read that failed, and the
/// pairing key kept the digest while dropping everything else — giving every
/// unreadable file in the project one identity. A mutation that left a
/// *different* file unreadable than the one already was paired with it and
/// passed, onto a `check` naming a document it had never named.
#[cfg(unix)]
#[test]
fn an_unreadable_file_is_not_every_other_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"done\"]\nterminal = [\"done\"]\n\
         [[scope.conditional_exclude]]\n\
         parent_glob = \"docs/*/SPEC.md\"\nchild_glob = \"docs/*/note.md\"\n",
    )
    .unwrap();
    // `docs/a/note.md` is out of scope behind its terminal parent; only
    // `docs/b/note.md` is read, and only it fails.
    write_doc(
        root,
        "docs/a/SPEC.md",
        "---\nid: sa\ntitle: SA\nkind: generic\nstatus: done\n---\n# SA\n",
    );
    for name in ["a", "b"] {
        let note = root.join(format!("docs/{name}/note.md"));
        write_doc(root, &format!("docs/{name}/note.md"), "unreadable\n");
        fs::set_permissions(&note, fs::Permissions::from_mode(0o000)).unwrap();
    }
    let broken = |root: &std::path::Path| -> Vec<String> {
        envelope_of(nodex(root).arg("check"))
            .pointer("/data/violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter(|v| v["rule_id"] == "parse_failure")
            .filter_map(|v| v["path"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(broken(root), ["docs/b/note.md"], "one file is unreadable");

    // Moving the terminal parent carries the exclusion with it: `a`'s note
    // enters scope and `b`'s leaves. The count is unchanged; the document
    // the project is broken on is not.
    let output = nodex(root)
        .args(["rename", "docs/a/SPEC.md", "docs/b/SPEC.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "a newly broken document");
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(msg.contains("docs/a/note.md"), "{msg}");
    assert_eq!(broken(root), ["docs/b/note.md"], "and nothing moved");
}

/// A field name a document cannot spell is a config no document could ever
/// satisfy, so it is refused where the operator can fix it.
#[test]
fn a_declared_field_must_be_a_key_a_document_can_spell() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [schema]\nrequired = [\"bad: key\"]\n",
    )
    .unwrap();
    write_doc(root, "docs/bare.md", "# Bare\n");
    let output = nodex(root).arg("check").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        env.pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("bad: key")
    );
}

/// A batch the gate judged whole lands whole.
///
/// The gate answers for the project a rename produces, and that answer is
/// worth only as much as the batch's all-or-nothing-ness: a rewrite that
/// failed after `fs::rename` left a project nothing had judged — here, a
/// reference this project's own policy calls an error, reported as a warning
/// on a command that exited 0.
///
/// Every write is staged before the move, so the failures that actually
/// happen — an unwritable directory, a full disk — happen while the tree is
/// still untouched.
#[cfg(unix)]
#[test]
fn a_rename_that_cannot_write_every_reference_writes_none() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[detection.unresolved_policy]]\nname = \"missing\"\n\
         cause = \"missing\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/target.md",
        "---\nid: t\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    write_doc(
        root,
        "refs/ref.md",
        "---\nid: r\ntitle: R\nkind: generic\nstatus: active\n---\nsee [t](../docs/target.md)\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();

    let refs = root.join("refs");
    let original = fs::metadata(&refs).unwrap().permissions();
    fs::set_permissions(&refs, fs::Permissions::from_mode(0o555)).unwrap();
    let output = nodex(root)
        .args(["rename", "docs/target.md", "docs/moved.md"])
        .output()
        .expect("ran");
    fs::set_permissions(&refs, original).unwrap();

    assert_eq!(output.status.code(), Some(2), "the batch refuses");
    assert!(
        root.join("docs/target.md").exists() && !root.join("docs/moved.md").exists(),
        "nothing moved"
    );
    assert!(
        fs::read_to_string(root.join("refs/ref.md"))
            .unwrap()
            .contains("../docs/target.md"),
        "nothing was rewritten"
    );
    nodex(root).arg("check").assert().success();
}

/// A component that starts with a wildcard can still insist on the dot.
///
/// `*` matches nothing, so `*.hidden` matches the segment `.hidden` and stops
/// matching once the dot is replaced. A rule reading the pattern's *text*
/// declared every wildcard-leading component unable to require a dot and
/// pruned a corpus the include matched — with `check` green over it and
/// `scaffold` writing into a directory the next build never reads.
#[test]
fn a_wildcard_leading_component_can_still_require_the_dot() {
    for (include, doc, expected) in [
        ("['**/*.hidden/**/*.md']", "x/.hidden/doc.md", 1),
        ("['*/*.d/**/*.md']", "a/.d/x.md", 1),
        ("['docs/**/*.vault/**/*.md']", "docs/a/.vault/n.md", 1),
        // And the default the guard keeps: a wildcard that does not turn on
        // the dot still opts nothing in.
        ("['**/*.md']", ".dotted/doc.md", 0),
        ("['docs/**/*.md']", "docs/a/.hidden/doc.md", 0),
    ] {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            format!("[scope]\ninclude = {include}\n[kinds]\nallowed = [\"generic\"]\n"),
        )
        .unwrap();
        write_doc(
            root,
            doc,
            "---\nid: d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
        );
        let env = run_envelope(nodex(root).arg("build"));
        assert_eq!(
            env.pointer("/data/nodes").and_then(Value::as_i64),
            Some(expected),
            "{include} / {doc}: {env}"
        );
    }
}

/// A placeholder owns the findings about *itself*, and a finding no node owns
/// belongs to no document.
///
/// A duplicated number is a conflict *between* documents, and the path the
/// finding carries is whichever member sorted first — so filtering the advise
/// licence by path made the verdict depend on a filename's alphabetical luck:
/// scaffolding `0003-aaa.md` beside `0003-zzz.md` was advised and written,
/// while `0003-zzz2.md` was refused.
#[test]
fn a_placeholder_does_not_own_a_conflict_it_creates_with_another_document() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^\\\\d{4}-.*\\\\.md$\"\nunique = true\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/0003-zzz.md",
        "---\nid: z\ntitle: Z\nkind: generic\nstatus: active\n---\n# Z\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();

    // Sorts before the partner, so the finding would have carried the
    // scaffold's own path.
    let output = nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Aa"])
        .args(["--path", "docs/0003-aaa.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(!root.join("docs/0003-aaa.md").exists());
    nodex(root).arg("check").assert().success();

    // A placeholder that conflicts with nobody still writes and advises.
    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Solo"])
        .args(["--path", "docs/0009-solo.md"])
        .assert()
        .success();
}

/// A repoint moves edges, and edges are what several rules are about. The
/// seam answers for the graph it produces, not only for the locks it holds.
#[test]
fn retarget_refuses_a_repoint_that_closes_a_cycle() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    // a implements b, b implements c. Repointing c onto a closes a → b → a.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\nimplements: [b]\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\nimplements: [c]\n---\n# B\n",
    );
    write_doc(
        root,
        "docs/c.md",
        "---\nid: c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["retarget", "c", "a"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(msg.contains("acyclic_relation"), "names the rule: {msg}");
    assert!(
        fs::read_to_string(root.join("docs/b.md"))
            .unwrap()
            .contains("implements: [c]"),
        "a refused repoint rewrites nothing"
    );
    nodex(root).arg("check").assert().success();
}

/// A status change reaches past the document it is written to: a
/// `conditional_exclude` parent going terminal drops its sub-artifacts, and
/// every reference into them is a violation the transition introduced.
#[test]
fn lifecycle_refuses_a_status_change_that_strands_references() {
    fn fixture(policy: &str) -> TempDir {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            format!(
                "[scope]\ninclude = [\"docs/**/*.md\"]\n\
                 [[scope.conditional_exclude]]\n\
                 parent_glob = \"docs/proj/index.md\"\n\
                 child_glob = \"docs/proj/notes/**/*.md\"\n\
                 [kinds]\nallowed = [\"generic\"]\n\
                 [statuses]\nallowed = [\"active\", \"archived\"]\n\
                 terminal = [\"archived\"]\ninitial = \"active\"\n{policy}"
            ),
        )
        .unwrap();
        write_doc(
            root,
            "docs/proj/index.md",
            "---\nid: index\ntitle: I\nkind: generic\nstatus: active\n---\n# I\n",
        );
        write_doc(
            root,
            "docs/proj/notes/n.md",
            "---\nid: note\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
        );
        write_doc(
            root,
            "docs/other.md",
            "---\nid: other\ntitle: O\nkind: generic\nstatus: active\n---\nsee [n](proj/notes/n.md)\n",
        );
        nodex(root).arg("build").assert().success();
        tmp
    }

    let erroring = fixture(
        "[[detection.unresolved_policy]]\nname = \"gone\"\n\
         cause = \"excluded_from_scope\"\nseverity = \"error\"\n",
    );
    let root = erroring.path();
    let output = nodex(root)
        .args(["lifecycle", "set", "index", "--status", "archived"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        msg.contains("unresolved_reference/gone"),
        "names the rule: {msg}"
    );
    assert!(
        fs::read_to_string(root.join("docs/proj/index.md"))
            .unwrap()
            .contains("status: active"),
        "a refused transition writes nothing"
    );
    nodex(root).arg("check").assert().success();

    // The same eviction under the default policy is a reported edge `check`
    // passes, so the same transition must land.
    let permitting = fixture("");
    let root = permitting.path();
    nodex(root)
        .args(["lifecycle", "set", "index", "--status", "archived"])
        .assert()
        .success();
    nodex(root).arg("check").assert().success();
}

#[test]
fn rename_refuses_a_destination_its_own_naming_rule_would_reject() {
    // Self-consistency on the move seam: a destination filename the
    // project's naming rule rejects would land and then fail the project's
    // own filename_pattern check. Refuse it.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^\\\\d{4}-[a-z0-9-]+\\\\.md$\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/0001-doc.md",
        "---\nid: a\ntitle: A\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["rename", "docs/0001-doc.md", "docs/BADNAME.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("filename_pattern") && msg.contains("BADNAME.md"),
        "names the rule that would fire and the filename: {msg}"
    );
    assert!(
        root.join("docs/0001-doc.md").exists(),
        "refused rename keeps the source"
    );
    assert!(
        !root.join("docs/BADNAME.md").exists(),
        "refused rename writes no destination"
    );

    // A conforming destination succeeds.
    nodex(root)
        .args(["rename", "docs/0001-doc.md", "docs/0002-doc.md"])
        .assert()
        .success();
}

#[test]
fn rename_refuses_a_destination_outside_the_project_scope() {
    // Renaming a doc out of scope would silently drop it from the graph
    // and leave every rewritten reference dangling — refused before any
    // mutation (including the id anchor).
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["rename", "docs/a.md", "attic/a.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(root.join("docs/a.md").exists(), "source not moved");
    assert!(!root.join("attic").exists(), "no destination dirs created");
    assert_eq!(
        fs::read_to_string(root.join("docs/a.md")).unwrap(),
        original,
        "refused rename leaves the source byte-identical (no id anchor written)"
    );
}

#[test]
fn backslash_spellings_normalize_to_the_same_document() {
    // nodex's path language is forward-slashed on every platform; the
    // one normalization seam folds `\` before anything keys on the
    // path, so `docs\a.md` gates, scaffolds, and renames the same
    // document as `docs/a.md` — never a phantom second node or a stray
    // literal-backslash file.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"bogus-free\"]\nterminal = []\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // check --content: the backslash spelling gates the same node — a
    // proposal that violates under `docs/a.md` violates under
    // `docs\a.md` too (no DUPLICATE_ID, no vacuous pass).
    let bad = "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: rogue\n---\n# A\n";
    for spelling in ["docs/a.md", "docs\\a.md", "docs\\./a.md"] {
        nodex(root)
            .args(["check", "--content"])
            .arg(format!("{spelling}=-"))
            .write_stdin(bad)
            .assert()
            .failure()
            .code(1);
    }

    // scaffold: the backslash spelling writes into docs/, exactly where
    // the envelope says, with the plain spelling's id.
    let env = run_envelope(
        nodex(root)
            .args(["scaffold", "--kind", "generic", "--title", "New"])
            .args(["--path", "docs\\new.md"]),
    );
    assert_eq!(
        env.pointer("/data/path").and_then(Value::as_str),
        Some("docs/new.md")
    );
    assert_eq!(
        env.pointer("/data/id").and_then(Value::as_str),
        Some("generic-new")
    );
    assert!(root.join("docs/new.md").exists(), "written where reported");
    assert!(
        !root.join("docs\\new.md").exists() || cfg!(windows),
        "no stray literal-backslash file"
    );

    // rename: backslash destination lands at the folded path and the
    // envelope reports it forward-slashed.
    let env = run_envelope(nodex(root).args(["rename", "docs\\new.md", "docs\\moved.md"]));
    assert_eq!(
        env.pointer("/data/new_path").and_then(Value::as_str),
        Some("docs/moved.md")
    );
    assert!(root.join("docs/moved.md").exists());
    nodex(root).arg("build").assert().success();
}

#[test]
fn rewrite_lock_applies_append_only_mode_like_check() {
    // The probe must mirror `check`'s mode handling: `append_only`
    // permits a body change that keeps the baseline lines as a prefix.
    // A reference rewrite inside an APPENDED region preserves that
    // prefix, so check allows it and the probe must too — while a
    // `frozen` doc's baseline-region rewrite stays locked.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"log\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"append-log\"\nmode = \"append_only\"\nkinds = [\"log\"]\n\
         [[rules.body_immutable]]\nname = \"frozen-spec\"\nmode = \"frozen\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: log-a\ntitle: A\nkind: log\nstatus: archived\n---\n# A\noriginal entry\n",
    );
    write_doc(
        root,
        "docs/f.md",
        "---\nid: generic-f\ntitle: F\nkind: generic\nstatus: archived\n---\n# F\nSee [t](t.md).\n",
    );
    write_doc(
        root,
        "docs/t.md",
        "---\nid: generic-t\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Append a referencing line to the append_only doc (a legitimate
    // append that keeps the baseline body as a prefix).
    write_doc(
        root,
        "docs/a.md",
        "---\nid: log-a\ntitle: A\nkind: log\nstatus: archived\n---\n# A\noriginal entry\nlater: see [t](t.md)\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["rename", "docs/t.md", "docs/t2.md"]));
    let updated: Vec<&str> = env
        .pointer("/data/references_updated")
        .and_then(Value::as_array)
        .expect("updated")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // append_only doc's appended-region link IS rewritten (check allows it).
    assert!(updated.contains(&"docs/a.md"), "{updated:?}");
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("(t2.md)"),
        "append_only appended-region rewrite proceeds"
    );
    // frozen doc's baseline-region link is skipped.
    assert!(!updated.contains(&"docs/f.md"), "{updated:?}");
    assert!(
        fs::read_to_string(root.join("docs/f.md"))
            .unwrap()
            .contains("(t.md)"),
        "frozen baseline-region rewrite is locked"
    );
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["check", "--since", "HEAD"])
        .assert()
        .success();
}

#[test]
fn rewrite_lock_gates_on_baseline_status_not_working_tree_status() {
    // The probe must mirror `check`: a doc terminal at the baseline whose
    // status was changed to non-terminal in the working tree is STILL
    // body-locked (check gates on the before-snapshot status). The rewrite
    // must be skipped, not silently performed — otherwise rename defaces a
    // body that `check --since` would flag.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"adr\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\nkinds = [\"adr\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: ADR-a\ntitle: A\nkind: adr\nstatus: archived\n---\n# A\nSee [b](b.md).\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Un-terminalize A in the working tree only (uncommitted).
    write_doc(
        root,
        "docs/a.md",
        "---\nid: ADR-a\ntitle: A\nkind: adr\nstatus: active\n---\n# A\nSee [b](b.md).\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["rename", "docs/b.md", "docs/c.md"]));
    assert!(
        env.pointer("/data/references_updated")
            .and_then(Value::as_array)
            .is_some_and(|a| a.is_empty()),
        "A's body rewrite is skipped: {env}"
    );
    assert!(
        env.get("warnings")
            .and_then(Value::as_array)
            .is_some_and(|w| w
                .iter()
                .filter_map(warning_msg)
                .any(|s| s.contains("locked"))),
        "skip warning surfaced: {env}"
    );
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("(b.md)"),
        "frozen body keeps its baseline spelling"
    );
}

#[test]
fn check_since_surfaces_a_baseline_parse_warning() {
    // A document unparseable AT the baseline vanishes from the before
    // graph, so it looks "added" and its diff-aware immutability rules
    // silently never fire — `check --since` would pass on a lock it never
    // enforced. The rule pass only sees the CURRENT graph's parse
    // failures, so the baseline's recorded drop must reach the envelope
    // as a ref-tagged warning for the operator to see the gap.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    // Malformed frontmatter at the baseline commit.
    write_doc(
        root,
        "docs/a.md",
        "---\ntitle: [unclosed\nbad: : :\n---\n# A\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Repair it in the working tree.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nstatus: archived\n---\n# A body\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["check", "--since", "HEAD"]));
    let warnings: Vec<&str> = env
        .get("warnings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(warning_msg).collect())
        .unwrap_or_default();
    assert!(
        warnings.iter().any(|w| w.contains("baseline HEAD")
            && w.contains("docs/a.md")
            && w.contains("diff-aware rules are inert")),
        "baseline parse warning must surface: {warnings:?}"
    );
}

#[test]
fn retarget_proceeds_when_only_the_frontmatter_changes_on_a_body_locked_doc() {
    // body_immutable protects the body only. A frontmatter-relation
    // retarget that leaves the body untouched must NOT be blocked by a
    // body lock — `check` would not flag it (no body change), so the probe
    // must not over-protect.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"adr\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\nkinds = [\"adr\"]\n",
    )
    .unwrap();
    // Archived adr, body-locked, with an id relation in frontmatter and
    // NO id reference in the body.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: ADR-a\ntitle: A\nkind: adr\nstatus: archived\nrelated: generic-old\n---\n# A\nno id reference in the body.\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: generic-old\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: generic-new\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["retarget", "generic-old", "generic-new"]));
    assert!(
        env.pointer("/data/references_updated")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().filter_map(Value::as_str).any(|s| s == "docs/a.md")),
        "the frontmatter-only retarget proceeds: {env}"
    );
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("generic-new"),
        "the locked-body doc's relation was repointed"
    );
}

#[test]
fn retarget_skips_a_relation_field_locked_by_frontmatter_immutable() {
    // The frontmatter twin of the body lock: a doc committed terminal at
    // the immutable_baseline whose `related:` is locked by a
    // frontmatter_immutable block must not be repointed — the rewrite
    // would change a locked field that `check` flags, so the probe's
    // relation-field arm engages and the doc stays byte-identical.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.frontmatter_immutable]]\nname = \"sealed-relations\"\nfields = [\"related\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/sealed.md",
        "---\nid: generic-sealed\ntitle: S\nkind: generic\nstatus: archived\nrelated: generic-old\n---\n# S\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: generic-old\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: generic-new\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let sealed_before = fs::read_to_string(root.join("docs/sealed.md")).unwrap();

    let env = run_envelope(nodex(root).args(["retarget", "generic-old", "generic-new"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings.iter().filter_map(warning_msg).any(
            |w| w.contains("sealed.md") && w.contains("frontmatter_immutable/sealed-relations")
        ),
        "{warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/sealed.md")).unwrap(),
        sealed_before,
        "sealed relations untouched"
    );
}

#[test]
fn moved_file_lock_probe_judges_kind_at_the_before_path() {
    // A kind-scoped body lock gates on the *before* kind. A cross-kind
    // move of a doc committed terminal at the baseline must not slip its
    // own link rebase past the lock via the destination path's kind
    // inference.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"adrs/**/*.md\", \"notes/**/*.md\", \"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"adr\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.kind_rules]]\nglob = \"adrs/**\"\nkind = \"adr\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"adr-frozen\"\nmode = \"frozen\"\nkinds = [\"adr\"]\n",
    )
    .unwrap();
    // No frontmatter `kind:` — the kind is path-inferred (adr at the old
    // path, generic at the new one). The body link needs rebasing after
    // a depth-changing move, so the rewrite would fire without the lock.
    write_doc(
        root,
        "adrs/x.md",
        "---\nid: adr-x\ntitle: X\nstatus: archived\n---\n# X\n\nSee [d](../docs/d.md).\n",
    );
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["rename", "adrs/x.md", "notes/sub/x.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("body_immutable/adr-frozen")),
        "the before-kind lock engages: {warnings:?}"
    );
    let moved = fs::read_to_string(root.join("notes/sub/x.md")).unwrap();
    assert!(
        moved.contains("(../docs/d.md)"),
        "frozen body keeps its original link spelling: {moved}"
    );
}

#[test]
fn a_rename_that_leaves_a_referrer_binding_something_else_says_so() {
    // The referrer stands still and the rename takes the rung its
    // reference stood on out from under it. `[x](a.md)` bound the root
    // document by the literal frame; once that path is gone the
    // source-relative frame answers with the neighbour. The graph that
    // leaves is valid, so the rename is the only place it can be said —
    // and this pins the saying, not the computing.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a.md",
        "---\nid: root-a\ntitle: RA\nkind: generic\nstatus: active\n---\nx\n",
    );
    write_doc(
        root,
        "docs/a.md",
        "---\nid: shadow\ntitle: SH\nkind: generic\nstatus: active\n---\nx\n",
    );
    write_doc(
        root,
        "docs/ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[x](a.md)\n",
    );
    // No destination spelling can carry `#`, so the reference is left.
    let env = run_envelope(nodex(root).args(["rename", "a.md", "a#1.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("root-a") && w.contains("shadow")),
        "the rename names both documents the reference stood between: {warnings:?}"
    );
    nodex(root).arg("check").assert().success();
}

#[test]
fn a_move_that_repoints_a_reference_it_could_not_re_render_says_so() {
    // A relative reference means whatever it means from where it sits, so
    // a move can leave one naming a different document. The graph that
    // results is valid and `check` has nothing to say about it, so the
    // rename is the only place it can be said — and saying it is what
    // this pins, not merely computing it.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '@ref\\(([a-z]+)\\)'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    // The capture takes only letters, so `../a/x` is a spelling the
    // pattern cannot read back: the reference is left exactly as it is.
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n@ref(x)\n",
    );
    write_doc(
        root,
        "a/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "b/x.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    let env = run_envelope(nodex(root).args(["rename", "a/mover.md", "b/mover.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("desired") && w.contains("shadow")),
        "the move names both documents the reference stood between: {warnings:?}"
    );
    // Nothing else reports it: the project the move leaves is valid.
    nodex(root).arg("check").assert().success();
}

#[test]
fn a_move_says_nothing_about_a_self_reference_it_leaves_correctly_spelled() {
    // A document that refers to itself by path carries the one reference a
    // move must not talk about: it names the moving document, so it names
    // the same document wherever the file lands. Rewritten in two passes,
    // the second read the first's output as the author's own text and
    // reported a reference the author never wrote as one the move had
    // repointed — every claim in it false.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n\
         self [me](mover.md) and [x](other.md)\n",
    );
    write_doc(
        root,
        "a/other.md",
        "---\nid: other\ntitle: O\nkind: generic\nstatus: active\n---\no\n",
    );
    let env = run_envelope(nodex(root).args(["rename", "a/mover.md", "b/mover.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        env.get("warnings").is_none(),
        "nothing was repointed behind the author's back: {env:?}"
    );
    let moved = fs::read_to_string(root.join("b/mover.md")).unwrap();
    assert!(
        moved.contains("[me](mover.md)"),
        "the self-reference names the same document from either directory: {moved}"
    );
    assert!(
        moved.contains("[x](../a/other.md)"),
        "the reference to a document that stood still is rebased: {moved}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "mover"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|edge| edge.get("target").and_then(Value::as_str) == Some("mover")),
        "the self-edge survives the move: {outgoing:?}"
    );
}

#[test]
fn a_rename_leaves_references_to_a_document_whose_identity_it_cannot_carry() {
    // A document carrying no frontmatter has nowhere to anchor its id, so
    // a rename that changes the stem gives it a different one, and the
    // document the references named is not in the project the rename
    // leaves. A repoint would be a repoint at whatever stands there, and
    // what stands there is the thing in doubt — so the references are left
    // as they are, and the edge they carried comes to dangle where every
    // reader can see it.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    fs::write(root.join("bare.md"), "just body\n").unwrap();
    write_doc(
        root,
        "r.md",
        "---\nid: r\ntitle: R\nkind: generic\nstatus: active\n---\n[x](bare.md)\n",
    );
    let env = run_envelope(nodex(root).args(["rename", "bare.md", "renamed.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(0)
    );
    assert!(
        fs::read_to_string(root.join("r.md"))
            .unwrap()
            .contains("[x](bare.md)"),
        "no repoint was written over an identity the rename could not carry"
    );
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("generic-bare") && w.contains("generic-renamed")),
        "the rename names the identity it could not carry: {warnings:?}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "issues"]));
    let unresolved = env
        .pointer("/data/unresolved_edges")
        .and_then(Value::as_array)
        .expect("unresolved");
    assert!(
        unresolved
            .iter()
            .any(|edge| edge.get("source").and_then(Value::as_str) == Some("r")),
        "the reference the rename left is read as the dangling one it is: {unresolved:?}"
    );
}

#[test]
fn a_move_writes_no_repoint_at_a_path_the_move_gives_to_somebody_else() {
    // The target does not move; the move evicts it from scope, and the
    // spelling that would name it from the new directory is shadowed by a
    // document at the root. A repoint judged against the project the move
    // leaves would ask what stands at the destination and then check that
    // the write names what stands at the destination — which is no check
    // at all, and the edge changes document under an envelope reporting
    // success.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[scope.conditional_exclude]]\n\
         parent_glob = \"b/SPEC.md\"\nchild_glob = \"b/desired.md\"\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/deep/SPEC.md",
        "---\nid: spec\ntitle: S\nkind: generic\nstatus: archived\n---\n\
         [target](../../b/desired.md)\n",
    );
    write_doc(
        root,
        "b/desired.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "desired.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/deep/SPEC.md", "b/SPEC.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("b/SPEC.md"))
            .unwrap()
            .contains("[target](../../b/desired.md)"),
        "the reference is left as it is rather than pointed at the shadow"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "spec"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("target").and_then(Value::as_str) != Some("shadow")),
        "no edge was moved onto a document nobody named: {outgoing:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_move_writes_no_repoint_where_the_moved_file_changes_which_document_it_is() {
    // A relative symlink carried across directories resolves to different
    // bytes, so the document standing at the destination is not the one
    // that stood at the source. The reference named the one that stood at
    // the source; nothing the move leaves is it, so nothing is written.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         exclude = [\"a/target.md\", \"b/target.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/target.md",
        "---\nid: document-a\ntitle: A\nkind: generic\nstatus: active\n---\na\n",
    );
    write_doc(
        root,
        "b/target.md",
        "---\nid: document-b\ntitle: B\nkind: generic\nstatus: active\n---\nb\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: referrer\ntitle: R\nkind: generic\nstatus: active\n---\n[t](a/link.md)\n",
    );
    std::os::unix::fs::symlink("target.md", root.join("a/link.md")).unwrap();
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/link.md", "b/link.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("[t](a/link.md)"),
        "the reference is left as it is rather than pointed at another document"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "referrer"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("target").and_then(Value::as_str) != Some("document-b")),
        "no edge was moved onto the document the link came to resolve to: {outgoing:?}"
    );
}

#[test]
fn a_rename_repoints_a_reference_the_graph_bound_past_a_file_carrying_no_document() {
    // The ladder's first rung holds a file whose parse failed, so it
    // carries no document and the build bound the reference one rung
    // lower. Read against the scanned paths instead of against the graph,
    // the rewrite called that file the reference's target, left the
    // reference alone, and reported success over the edge it stranded.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    fs::write(root.join("x.md"), "---\nid: [\n---\nbroken\n").unwrap();
    write_doc(
        root,
        "a/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n[target](x.md)\n",
    );
    let env = run_envelope(nodex(root).args(["rename", "a/x.md", "a/z.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "the reference the graph bound is repointed: {env:?}"
    );
    let referrer = fs::read_to_string(root.join("a/mover.md")).unwrap();
    assert!(
        referrer.contains("[target](z.md)"),
        "repointed in its own frame: {referrer}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "mover"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|edge| edge.get("target").and_then(Value::as_str) == Some("desired")),
        "the edge the move was supposed to carry: {outgoing:?}"
    );
}

#[test]
fn a_retarget_says_nothing_about_a_reference_whose_bytes_it_rewrote() {
    // One span, two readers binding it under one relation: the wikilink
    // and a pattern spelling the same brackets. The repoint lands for the
    // first and covers the second, whose bytes are then not its own — so
    // there is nothing left standing to report, and reporting it would
    // name a reference the document plainly no longer holds.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = '\\[\\[(old)\\]\\]'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\nsee [[old]]\n",
    );
    write_doc(
        root,
        "old.txt.md",
        "---\nid: old\ntitle: O\nkind: generic\nstatus: active\n---\no\n",
    );
    write_doc(
        root,
        "new.txt.md",
        "---\nid: new\ntitle: N\nkind: generic\nstatus: active\n---\nn\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["retarget", "old", "new"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("see [[new]]"),
        "the repoint landed"
    );
    assert!(
        env.get("warnings").is_none(),
        "nothing was left standing to report: {env:?}"
    );
}

#[test]
fn rename_and_retarget_skip_locked_bodies_with_a_warning() {
    // Writer-skips for immutability locks, mirroring the symlink
    // discipline: a rewrite check would flag as a body_immutable (or a
    // relation-field frontmatter_immutable) violation is not performed —
    // frozen history keeps its original spelling, surfaced as a warning.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen-when-archived\"\nmode = \"frozen\"\n\
         [[rules.frontmatter_immutable]]\nname = \"seal-related\"\nfields = [\"related\"]\n",
    )
    .unwrap();
    // The archived doc references the soon-to-move target both in body
    // (a path link the rename rebases — body_immutable territory) and
    // frontmatter (an id relation retarget repoints — frontmatter_immutable
    // territory); the active doc references it in body only.
    write_doc(
        root,
        "docs/frozen.md",
        "---\nid: generic-frozen\ntitle: F\nkind: generic\nstatus: archived\nrelated: generic-target\n---\n# F\n\nSee [t](target.md).\n",
    );
    write_doc(
        root,
        "docs/live.md",
        "---\nid: generic-live\ntitle: L\nkind: generic\nstatus: active\n---\n# L\n\nSee [t](target.md).\n",
    );
    write_doc(
        root,
        "docs/target.md",
        "---\nid: generic-target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    write_doc(
        root,
        "docs/successor.md",
        "---\nid: generic-successor\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let frozen_before = fs::read_to_string(root.join("docs/frozen.md")).unwrap();

    // rename: the live doc is rewritten; the frozen doc is skipped with
    // a lock warning and left byte-identical.
    let env = run_envelope(nodex(root).args(["rename", "docs/target.md", "docs/target-v2.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let updated: Vec<&str> = env
        .pointer("/data/references_updated")
        .and_then(Value::as_array)
        .expect("updated list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(updated.contains(&"docs/live.md"), "{updated:?}");
    assert!(!updated.contains(&"docs/frozen.md"), "{updated:?}");
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("frozen.md") && w.contains("body_immutable/frozen-when-archived")),
        "{warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/frozen.md")).unwrap(),
        frozen_before,
        "frozen history untouched"
    );

    // retarget: the frozen doc's locked `related` relation is left
    // untouched — the rewrite would change a frontmatter_immutable field,
    // so it is skipped with that lock's warning.
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["retarget", "generic-target", "generic-successor"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("frozen.md") && w.contains("frontmatter_immutable/seal-related")),
        "{warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/frozen.md")).unwrap(),
        frozen_before,
        "frozen history untouched by retarget"
    );
}

#[test]
fn query_issues_runs_the_same_baseline_as_check() {
    // `query issues` resolves rules.immutable_baseline through the same
    // substrate as a default `check`, so the two can never disagree
    // about immutability violations.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\noriginal\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Tamper with the locked body in the working tree.
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\nTAMPERED\n",
    );
    nodex(root).arg("build").assert().success();

    // check (default baseline) flags it…
    nodex(root).arg("check").assert().failure().code(1);
    // …and query issues reports the SAME violation instead of a skip.
    let data = run_json(nodex(root).args(["query", "issues"]));
    let violations = data
        .get("violations")
        .and_then(Value::as_array)
        .expect("violations");
    assert!(
        violations
            .iter()
            .any(|v| v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/frozen")),
        "issues carries the baseline violation: {data}"
    );
}

#[test]
fn git_backed_rules_measure_the_project_under_any_inherited_environment() {
    // Git selects a repository from the environment before it looks at a
    // working directory, and reinterprets path arguments from it too, so
    // an inherited variable is enough to decide what gets measured:
    // drift counted against a foreign repository (where the project's
    // paths carry no history, so the finding disappears instead of
    // erring), a baseline read from foreign bytes, a work-tree probe
    // answering `true` for a directory holding no repository. The
    // exporters are ordinary — server-side hooks export GIT_DIR with an
    // absolute GIT_OBJECT_DIRECTORY, `git submodule foreach` exports
    // GIT_DIR, and every shell-based git subcommand sourcing
    // git-sh-setup exports it.
    //
    // The property is one verdict: the project decides what is measured,
    // so every environment below produces the identical finding. It is
    // asserted as a property rather than per-mechanism, because the
    // failure mode is a finding that quietly stops appearing.
    let tmp = scratch();
    let root = tmp.path();
    let elsewhere = scratch();
    let git = git_runner(root);
    git(&["init", "-q"]);
    // A real second repository: a GIT_DIR naming nothing would make git
    // fail loudly, which is not the case under test.
    git_runner(elsewhere.path())(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [detection]\ngit_drift_threshold = 1\n",
    )
    .unwrap();
    // The covered path carries a glob metacharacter and a sibling the
    // pattern would match: an inherited GIT_GLOB_PATHSPECS turns the path
    // into a pattern and folds the sibling's history into the count, and
    // the `--literal-pathspecs` that pins interpretation is itself
    // rejected by git while such a variable is set. So the assertion is
    // the measured number, not merely the presence of a finding — a
    // verdict that survives by coincidence is not a verdict.
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n\
         reviewed: 2020-01-01\ncovers:\n  - \"src/a[1].rs\"\n---\n# D\n",
    );
    fs::create_dir_all(root.join("src")).unwrap();
    for change in 0..2 {
        fs::write(root.join("src/a[1].rs"), format!("// {change}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "covered"]);
    }
    for change in 0..3 {
        fs::write(root.join("src/a1.rs"), format!("// {change}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "not covered"]);
    }
    nodex(root).arg("build").assert().success();

    let drift = |cmd: &mut Command| -> Vec<u64> {
        run_json(cmd)
            .get("violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter(|v| v.get("rule_id").and_then(Value::as_str) == Some("git_drift"))
            .map(|v| {
                v.pointer("/details/total_commits")
                    .and_then(Value::as_u64)
                    .expect("git_drift carries its commit count")
            })
            .collect()
    };
    assert_eq!(
        drift(nodex(root).arg("check")),
        [2],
        "the project's own commits on the covered path, and no others"
    );

    let foreign_git_dir = elsewhere.path().join(".git");
    let hostile: Vec<(&str, PathBuf)> = vec![
        ("GIT_DIR", foreign_git_dir.clone()),
        ("GIT_COMMON_DIR", foreign_git_dir.clone()),
        ("GIT_WORK_TREE", elsewhere.path().to_path_buf()),
        ("GIT_OBJECT_DIRECTORY", foreign_git_dir.join("objects")),
        ("GIT_INDEX_FILE", foreign_git_dir.join("index")),
        // Reinterpret the paths the probe passes.
        ("GIT_ICASE_PATHSPECS", PathBuf::from("1")),
        ("GIT_GLOB_PATHSPECS", PathBuf::from("1")),
        ("GIT_NOGLOB_PATHSPECS", PathBuf::from("1")),
        ("GIT_LITERAL_PATHSPECS", PathBuf::from("1")),
    ];
    for (var, value) in hostile {
        assert_eq!(
            drift(nodex(root).arg("check").env(var, &value)),
            [2],
            "an inherited {var} must not change what the project measures"
        );
    }
}

#[test]
fn check_and_query_issues_surface_the_same_inert_baseline_advisory() {
    // A configured baseline that cannot engage (immutability rules
    // declared, root not a git work tree) leaves the diff-aware rules
    // inert. Both consumers of the shared baseline resolution surface
    // the identical advisory — neither goes silently green about it,
    // and the wording is constructed once in the substrate.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
    );
    nodex(root).arg("build").assert().success();

    let advisory = |env: &Value| -> Option<String> {
        env.get("warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(warning_msg)
            .find(|w| w.contains("immutability rules are inert this run"))
            .map(str::to_string)
    };

    let check_env = run_envelope(nodex(root).arg("check"));
    let check_warning = advisory(&check_env)
        .unwrap_or_else(|| panic!("check surfaces the inert advisory: {check_env}"));

    let issues_env = run_envelope(nodex(root).args(["query", "issues"]));
    let issues_warning = advisory(&issues_env)
        .unwrap_or_else(|| panic!("query issues surfaces the inert advisory: {issues_env}"));

    assert_eq!(
        check_warning, issues_warning,
        "one advisory wording across both commands"
    );
}

// ─── a project's location inside its repository ─────────────────────

/// The config a subdirectory-project fixture writes: an immutability
/// baseline plus a frozen-body lock, so both planes the baseline governs
/// (the `check` diff and the write-seam locks) are live.
const LOCKED_PROJECT_CONFIG: &str = r#"
[scope]
include = ["docs/**/*.md"]
[statuses]
allowed = ["active", "archived"]
terminal = ["archived"]
initial = "active"
[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
[parser]
wikilink_enabled = true
[rules]
immutable_baseline = "HEAD"
[[rules.body_immutable]]
name = "frozen"
mode = "frozen"
trigger = "terminal"
kinds = ["generic"]
"#;

/// A repository whose nodex project sits in `docs-site/` instead of at
/// the repository root — a monorepo documentation subproject, as ordinary
/// as a project that owns its repository.
///
/// The repository root carries a decoy at the same *relative* path whose
/// bytes differ from the project's. Git resolves `<ref>:docs/a.md` and a
/// checkout against the repository root, not against a working directory,
/// so anything that forgets where the project sits reads the decoy.
///
/// `decoy_status` is what makes the difference observable, and it differs
/// by plane. A body lock keys on the *baseline* status, so a decoy read
/// in place of the project's own file must be non-terminal for the write
/// plane (the lock would then not engage, and a prefix-less read shows up
/// as a performed write) and terminal for the read plane (the lock would
/// then engage on a document nobody touched, and a checkout graphed at
/// the wrong root shows up as a manufactured violation). A decoy that
/// trips the same lock either way makes the test agree with itself and
/// prove nothing. Returns the project root.
fn subdirectory_project(repo: &std::path::Path, decoy_status: &str) -> PathBuf {
    let git = git_runner(repo);
    git(&["init", "-q"]);
    let project = repo.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        &project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        &project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        &project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    write_doc(
        repo,
        "docs/a.md",
        &format!(
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: {decoy_status}\n---\n\
             # A\n\nthe repository root's own document, not the project's\n"
        ),
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    project
}

#[test]
fn immutability_locks_engage_for_a_subdirectory_project() {
    // The lock is judged against the document's committed bytes, which
    // the baseline reads by asking git for a path. A path written without
    // the project's prefix names the repository root's file — a different
    // document, or none — so the lock read no baseline and the write went
    // through: a frozen body silently rewritten, no warning, and a
    // `check` that stays green because it mismeasures the same way. The
    // verdict must be the one a project owning its repository gets.
    let tmp = scratch();
    // Non-terminal decoy: reading it in place of the project's own
    // baseline would leave the lock disengaged, so a refused write can
    // only come from the project's own frozen document.
    let project = subdirectory_project(tmp.path(), "active");
    nodex(&project).arg("build").assert().success();

    let frozen_before = fs::read_to_string(project.join("docs/a.md")).unwrap();
    let envelope = run_envelope(nodex(&project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope
            .pointer("/data/total_updated")
            .and_then(Value::as_u64),
        Some(0),
        "a frozen body must not be repointed: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("body_immutable/frozen")),
        "the refusal names the lock that held: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(project.join("docs/a.md")).unwrap(),
        frozen_before,
        "nothing was written"
    );
}

#[test]
fn a_repository_root_document_never_stands_in_for_a_subdirectory_project() {
    // The mirror image of the lock case: with the baseline built from the
    // repository root, the decoy's body differed from the project's
    // committed body under the same id, so the frozen-body rule fired on
    // a document the operator never touched. A manufactured violation is
    // as wrong as a missing one, and here nothing warns.
    let tmp = scratch();
    // Terminal decoy with a different body: a baseline graphed at the
    // repository root would fire the frozen lock on it, so a clean report
    // can only come from the project's own snapshot.
    let project = subdirectory_project(tmp.path(), "archived");
    nodex(&project).arg("build").assert().success();

    let envelope = run_envelope(nodex(&project).args(["check", "--since", "HEAD"]));
    let data = envelope.get("data").expect("data");
    assert_eq!(
        data.get("violations").and_then(Value::as_array),
        Some(&vec![]),
        "an untouched project has nothing to report: {envelope}"
    );
    assert_eq!(
        data.get("skipped_rules").and_then(Value::as_array),
        Some(&vec![]),
        "the diff-aware rules ran — the baseline engaged: {envelope}"
    );
    assert_eq!(
        envelope.get("warnings").and_then(Value::as_array),
        None,
        "a baseline that engaged has nothing to advise about: {envelope}"
    );
}

#[test]
fn a_baseline_predating_the_project_directory_is_inert_not_an_error() {
    // A subdirectory project introduced after the baseline ref has no
    // snapshot there — ordinary for a branch that adds the project. The
    // run must neither fail nor go quietly green: the ref carries no
    // project, so the diff-aware rules report themselves skipped and the
    // advisory names the directory the ref does not have.
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);
    let base = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let project = repo.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            &format!("immutable_baseline = \"{base}\""),
        ),
    )
    .unwrap();
    write_doc(
        &project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "introduce the project"]);
    nodex(&project).arg("build").assert().success();

    let envelope = run_envelope(nodex(&project).arg("check"));
    let skipped: Vec<&str> = envelope
        .pointer("/data/skipped_rules")
        .and_then(Value::as_array)
        .expect("skipped_rules")
        .iter()
        .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
        .collect();
    assert_eq!(
        skipped,
        ["body_immutable/frozen"],
        "a rule that cannot fire says so: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("docs-site") && m.contains("does not carry")),
        "the advisory names the directory the ref does not carry: {envelope}"
    );
}

#[test]
fn diff_refuses_a_ref_that_does_not_carry_the_project() {
    // `diff` and `impact` compare two snapshots of the *project*, so a
    // ref without it has nothing to compare — a typed GIT_ERROR naming
    // the ref, never a graph built from whatever else the checkout held.
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);

    // The project arrives in a second commit, so `HEAD~1` is a ref the
    // project does not exist at.
    let project = subdirectory_project(repo, "archived");
    nodex(&project).arg("build").assert().success();

    let output = nodex(&project)
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("GIT_ERROR"),
        "{envelope}"
    );
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(
        message.contains("docs-site") && message.contains("does not carry this project"),
        "the error names what the ref lacks: {message}"
    );
}

/// `check` builds its baseline by checking the ref out, and a checkout
/// applies whatever `.gitattributes` declares. Reading the stored bytes
/// instead gives the write side a different document for the same commit —
/// git's own `ident` stamps the blob's sha into the body — so a
/// frontmatter-only rewrite trips the *body* lock on a document whose body
/// nobody touched.
#[test]
fn a_checkout_filter_does_not_make_the_two_planes_read_different_documents() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    fs::write(project.join(".gitattributes"), "*.md ident\n").unwrap();
    // The reference lives in frontmatter, so repointing it leaves the body
    // alone — and the body carries the stamp the checkout expands.
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n\
         related: generic-b\n---\n# A\n\nstamp: $Id$\n",
    );
    write_doc(
        project,
        "docs/wired.md",
        "---\nid: generic-wired\ntitle: W\nkind: generic\nstatus: archived\n---\n\
         # W\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "documents under an ident attribute"]);
    // Force the smudge, so the working tree holds the expanded stamp.
    fs::remove_file(project.join("docs/a.md")).unwrap();
    git(&["checkout", "-q", "--", "docs/a.md"]);
    let smudged = fs::read_to_string(project.join("docs/a.md")).unwrap();
    assert!(
        smudged.contains("$Id: "),
        "the checkout expanded the stamp: {smudged}"
    );
    nodex(project).arg("build").assert().success();

    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    // `docs/a.md` is a frontmatter-only rewrite and must land; `docs/wired.md`
    // is a body rewrite on a terminal document and must not.
    assert_eq!(
        envelope["data"]["references_updated"],
        serde_json::json!(["docs/a.md"]),
        "only the frontmatter rewrite lands: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("docs/wired.md") && m.contains("body_immutable/frozen")),
        "the body rewrite is still refused: {envelope}"
    );
}

/// `core.precomposeUnicode` (git's default on macOS) matches a decomposed
/// pathspec and reports the composed spelling, so a baseline read that
/// recognised records by comparing the path it asked for against the path
/// git reported found nothing — and nothing is what permits a write. The
/// answer has to come from asking one path per invocation, where the record
/// that returns *is* the answer.
#[test]
#[cfg(target_os = "macos")]
fn a_decomposed_document_name_is_locked_like_any_other() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    // An explicit combining acute: a literal precomposed character here
    // would make the test prove nothing.
    let decomposed = "docs/cafe\u{301}.md";
    assert_eq!(decomposed.as_bytes(), b"docs/cafe\xcc\x81.md");
    for (rel, id) in [
        (decomposed, "generic-nfd"),
        ("docs/ascii.md", "generic-ascii"),
    ] {
        write_doc(
            project,
            rel,
            &format!(
                "---\nid: {id}\ntitle: T\nkind: generic\nstatus: archived\n---\n\
                 # T\n\nsee [[generic-b]]\n"
            ),
        );
    }
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "a decomposed name and an ascii control",
    ]);
    // Git records the composed spelling of the decomposed name; the
    // filesystem keeps the decomposed one. That divergence is the fixture.
    let recorded = git(&["ls-tree", "--name-only", "-z", "-r", "HEAD"]).stdout;
    let composed = "caf\u{e9}.md".as_bytes();
    assert!(
        recorded.windows(composed.len()).any(|w| w == composed),
        "git recorded the composed spelling: {}",
        String::from_utf8_lossy(&recorded)
    );

    let sealed: Vec<_> = [decomposed, "docs/ascii.md"]
        .iter()
        .map(|rel| project.join(rel))
        .collect();
    for path in &sealed {
        let edited = format!("{}\nedited\n", fs::read_to_string(path).unwrap());
        fs::write(path, edited).unwrap();
    }
    nodex(project).arg("build").assert().success();

    let before: Vec<_> = sealed
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the decomposed name is locked exactly as the ascii control is: {envelope}"
    );
    let after: Vec<_> = sealed
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    assert_eq!(after, before, "both frozen bodies are untouched");
}

/// The crate folds `\` to `/` for a stable JSON path contract, but on unix
/// a `\` is an ordinary byte in a document's name. Folding it on the way to
/// git asks about a document that does not exist, and a lock that asks
/// about a path no ref records reads as "nothing to freeze" — so `check`
/// reported the frozen body and the write rewrote it anyway.
#[test]
#[cfg(unix)]
fn a_document_whose_name_holds_a_backslash_is_still_locked() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        project,
        "docs/back\\slash.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "a document whose name holds a backslash",
    ]);
    let sealed = project.join("docs/back\\slash.md");
    fs::write(
        &sealed,
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n\nedited\n",
    )
    .unwrap();
    nodex(project).arg("build").assert().success();

    let before = fs::read_to_string(&sealed).unwrap();
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the lock reaches a document whose name holds a separator-looking byte: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("body_immutable/frozen")),
        "and it is the real rule, not an unevaluated lock: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(&sealed).unwrap(),
        before,
        "the frozen body is untouched"
    );
}

/// "This ref names nothing" and "this ref does not hold the project" are
/// different facts. Collapsing them lets `check --since <typo>` report
/// every node as in scope and exit 0 — the CI-green-on-a-typo failure the
/// configured baseline already refuses — and makes `diff` blame the
/// project's location for a ref that does not exist.
#[test]
fn a_ref_that_names_nothing_is_refused_rather_than_read_as_a_missing_project() {
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);
    let project = subdirectory_project(repo, "archived");
    nodex(&project).arg("build").assert().success();

    for args in [
        vec!["diff", "no-such-ref", "HEAD"],
        vec!["impact", "no-such-ref", "HEAD"],
        vec!["check", "--since", "no-such-ref"],
    ] {
        let output = nodex(&project).args(&args).output().expect("ran");
        assert_eq!(
            output.status.code(),
            Some(2),
            "nodex {args:?} must refuse a ref git does not resolve"
        );
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        let message = envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .expect("message");
        assert!(
            message.contains("no-such-ref") && message.contains("no such ref"),
            "the refusal names the ref, not the project's location: {message}"
        );
    }

    // The ref that *does* resolve but lacks the project keeps its own
    // verdict — the distinction is the point.
    let output = nodex(&project)
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains("does not carry this project")),
        "a resolvable ref without the project still says so: {envelope}"
    );
}

/// A ref may carry the project's *name* without carrying the project:
/// a gitlink is what a directory looks like before it stops being a
/// submodule, and `git worktree add` materialises an empty directory for
/// one it does not populate. Reading that directory as the project
/// graphs an empty baseline — every current document reported as newly
/// added, and `check --since` blaming `scope.include`.
#[test]
fn a_ref_recording_the_project_as_a_gitlink_carries_no_project() {
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);
    let vendored = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{vendored},docs-site"),
    ]);
    git(&["commit", "-q", "-m", "docs-site is a vendored submodule"]);

    let project = subdirectory_project(repo, "archived");
    nodex(&project).arg("build").assert().success();

    let output = nodex(&project)
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(
        message.contains("docs-site") && message.contains("does not carry this project"),
        "a gitlink at the prefix is not the project: {message}"
    );

    let checked = run_envelope(nodex(&project).args(["check", "--since", "HEAD~1"]));
    assert!(
        checked
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("records no project directory at \"docs-site\"")),
        "the advisory names what the ref records, not the project's scope: {checked}"
    );
}

#[test]
fn diff_graphs_a_subdirectory_project_at_its_own_location() {
    // Both refs carry the project, so the comparison is between the
    // project's own two snapshots — the repository root's same-path
    // decoy takes part in neither.
    let tmp = scratch();
    let project = subdirectory_project(tmp.path(), "archived");
    let git = git_runner(tmp.path());
    write_doc(
        &project,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "add d"]);

    let data = run_json(nodex(&project).args(["diff", "HEAD~1", "HEAD"]));
    let added: Vec<&str> = data
        .get("added_nodes")
        .and_then(Value::as_array)
        .expect("added_nodes")
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(added, ["generic-d"], "{data}");
}

/// A repository whose path contains a newline — legal on a POSIX
/// filesystem. `git rev-parse` reports paths unquoted and has no
/// NUL-delimited mode, so answers cannot be told apart when one of them
/// spans lines; a binding read wrongly still looks bound, and then every
/// invocation built from it fails to spawn. Read as "nothing there", that
/// leaves a frozen body rewritable and a drift finding absent — the exact
/// two silent failures, so both are asserted here.
#[cfg(unix)]
#[test]
fn a_repository_path_that_spans_lines_measures_the_project_the_same() {
    let tmp = scratch();
    let repo = tmp.path().join("we\nird");
    fs::create_dir(&repo).unwrap();
    let git = git_runner(&repo);
    git(&["init", "-q"]);
    fs::write(
        repo.join("nodex.toml"),
        format!("{LOCKED_PROJECT_CONFIG}[detection]\ngit_drift_threshold = 1\n"),
    )
    .unwrap();
    write_doc(
        &repo,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        &repo,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        &repo,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    write_doc(
        &repo,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n\
         reviewed: 2020-01-01\ncovers:\n  - src/code.rs\n---\n# D\n",
    );
    fs::create_dir_all(repo.join("src")).unwrap();
    for change in 0..2 {
        fs::write(repo.join("src/code.rs"), format!("// {change}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "churn"]);
    }
    nodex(&repo).arg("build").assert().success();

    let frozen_before = fs::read_to_string(repo.join("docs/a.md")).unwrap();
    let envelope = run_envelope(nodex(&repo).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope
            .pointer("/data/total_updated")
            .and_then(Value::as_u64),
        Some(0),
        "the lock reads the project's baseline, so a frozen body is refused: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(repo.join("docs/a.md")).unwrap(),
        frozen_before,
        "nothing was written"
    );

    let drift: Vec<u64> = run_json(nodex(&repo).arg("check"))
        .get("violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter(|v| v.get("rule_id").and_then(Value::as_str) == Some("git_drift"))
        .map(|v| {
            v.pointer("/details/total_commits")
                .and_then(Value::as_u64)
                .expect("git_drift carries its commit count")
        })
        .collect();
    assert_eq!(
        drift,
        [2],
        "the drift probe measures the project's own file"
    );
}

/// A subdirectory project introduced after its baseline ref has no
/// snapshot to lock against. `check` says so — and so must a write: the
/// per-document question ("no bytes at the baseline") looks identical to
/// a brand-new document, so a write plane that only ever asks it proceeds
/// on a frozen body believing there was nothing to freeze. Both planes
/// read one resolution, so both disclose the same fact.
#[test]
fn a_baseline_that_carries_nothing_for_the_project_is_disclosed_on_writes_too() {
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);
    let base = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let project = repo.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            &format!("immutable_baseline = \"{base}\""),
        ),
    )
    .unwrap();
    write_doc(
        &project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        &project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        &project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "introduce the project"]);
    nodex(&project).arg("build").assert().success();

    let envelope = run_envelope(nodex(&project).args(["retarget", "generic-b", "generic-c"]));
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("docs-site") && m.contains("does not carry")),
        "a write against an empty baseline names what the ref lacks: {envelope}"
    );
}

/// A baseline ref git cannot resolve at all — a typo, or a ref never
/// fetched — leaves the immutability rules unable to fire *and* unable to
/// be enforced. Neither plane may go green on it: `check` would be
/// reporting on rules that can never run, and a write would be permitting
/// edits no lock could refuse.
#[test]
fn an_unreadable_baseline_ref_refuses_both_planes() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            "immutable_baseline = \"origin/main\"",
        ),
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        root,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let frozen_before = fs::read_to_string(root.join("docs/a.md")).unwrap();

    for args in [
        vec!["check"],
        vec!["retarget", "generic-b", "generic-c"],
        vec!["lifecycle", "set", "generic-b", "--status", "archived"],
    ] {
        let output = nodex(root).args(&args).output().expect("ran");
        assert_eq!(
            output.status.code(),
            Some(2),
            "nodex {args:?} must refuse an unreadable baseline"
        );
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert_eq!(
            envelope.pointer("/error/code").and_then(Value::as_str),
            Some("CONFIG_ERROR"),
            "{envelope}"
        );
        assert!(
            envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("origin/main")),
            "the refusal names the ref: {envelope}"
        );
    }
    assert_eq!(
        fs::read_to_string(root.join("docs/a.md")).unwrap(),
        frozen_before,
        "nothing was written"
    );

    // The verdict must hold when `HEAD` is what names nothing. A
    // repository whose refs still reach this history has a baseline the
    // operator can fix, so an unknown ref stays a refusal rather than
    // becoming the advisory an empty repository earns.
    git(&["symbolic-ref", "HEAD", "refs/heads/ghost"]);
    let output = nodex(root).arg("check").output().expect("ran");
    assert_eq!(
        output.status.code(),
        Some(2),
        "an unborn HEAD over real history does not make an unknown ref inert"
    );
}

/// A rename is a move plus a reference rewrite. The move cannot be
/// undone, so a refusal must arrive while the tree is still untouched:
/// refusing between the two halves leaves the file moved and every
/// reference to it dangling, which is worse than either outcome the
/// operator asked for.
#[test]
fn a_refused_rename_leaves_the_tree_untouched() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            "immutable_baseline = \"origin/main\"",
        ),
    )
    .unwrap();
    write_doc(
        root,
        "docs/r.md",
        "---\nid: generic-r\ntitle: R\nkind: generic\nstatus: active\n---\n\
         # R\n\nsee [c](c.md)\n",
    );
    write_doc(
        root,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let before: Vec<String> = walk_entries(root)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let referrer_before = fs::read_to_string(root.join("docs/r.md")).unwrap();

    let output = nodex(root)
        .args(["rename", "docs/c.md", "docs/c2.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "the rename is refused");

    assert!(
        root.join("docs/c.md").exists() && !root.join("docs/c2.md").exists(),
        "the document did not move"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/r.md")).unwrap(),
        referrer_before,
        "no reference was rewritten"
    );
    let mut after: Vec<String> = walk_entries(root)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(after, before, "nothing was created or removed");
}

/// A project set up before its first commit has recorded nothing to
/// compare against — the same "no snapshot" state as a ref that predates
/// the project, not a ref the operator spelled wrong. Refusing here would
/// block `git init` → `nodex init` → `nodex scaffold` on a baseline that
/// is perfectly correct.
#[test]
fn a_repository_with_no_commits_yet_is_inert_not_refused() {
    let tmp = scratch();
    let root = tmp.path();
    git_runner(root)(&["init", "-q"]);
    fs::write(root.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    let envelope = run_envelope(nodex(root).arg("check"));
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("no ref in the repository names a commit")),
        "the advisory names the condition it measured: {envelope}"
    );
    let scaffolded = run_envelope(
        nodex(root)
            .args(["scaffold", "--kind", "generic", "--title", "New"])
            .args(["--path", "docs/new.md"]),
    );
    assert!(
        scaffolded.pointer("/data/written").and_then(Value::as_bool) == Some(true),
        "a write proceeds against a repository with no history: {scaffolded}"
    );
}

/// The project's directory has to be a *directory* at the baseline. A ref
/// that records that name as a file resolves just as happily, and binding
/// to it leaves every document lookup empty — a baseline that reads as
/// "nothing is frozen" for the whole project, silently.
#[test]
fn a_baseline_recording_the_project_path_as_a_file_is_not_a_baseline() {
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    // `docs-site` is a plain file at this commit.
    fs::write(repo.join("docs-site"), "not a directory yet\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "file at the project's path"]);
    let base = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    fs::remove_file(repo.join("docs-site")).unwrap();
    let project = repo.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            &format!("immutable_baseline = \"{base}\""),
        ),
    )
    .unwrap();
    write_doc(
        &project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        &project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        &project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the project replaces the file"]);
    nodex(&project).arg("build").assert().success();

    let envelope = run_envelope(nodex(&project).args(["retarget", "generic-b", "generic-c"]));
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("does not carry")),
        "a name recorded as a file carries no project: {envelope}"
    );
}

/// A document's baseline is the *file* the ref records at its path.
/// Consolidating a directory of notes into one document of the same name
/// is an ordinary refactor, and it leaves the document with no baseline:
/// `check` reads it as new and locks nothing. A type-agnostic read hands
/// the write seam the directory's listing instead — a prior snapshot where
/// there is none — and a creation-triggered lock freezes a document on its
/// first appearance, refusing a write `check` does not flag.
#[test]
fn a_document_whose_name_held_a_directory_at_the_baseline_is_new_on_both_planes() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace("trigger = \"terminal\"", "trigger = \"creation\""),
    )
    .unwrap();
    write_doc(project, "docs/a.md/note.md", "a note, once its own file\n");
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "docs/a.md is a directory of notes"]);

    fs::remove_dir_all(project.join("docs/a.md")).unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    nodex(project).arg("build").assert().success();

    let checked = run_json(nodex(project).arg("check"));
    assert!(
        !checked
            .get("violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .any(|v| v
                .get("rule_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("body_immutable/"))),
        "the read plane reads a first appearance as new: {checked}"
    );

    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 1,
        "the write plane refuses only what `check` flags: {envelope}"
    );
    assert!(
        fs::read_to_string(project.join("docs/a.md"))
            .unwrap()
            .contains("[[generic-c]]"),
        "the rewrite landed"
    );
}

/// A `query` leaf answers from `graph.json`. Absence from a snapshot that no
/// longer matches the working tree is not absence from the project, and a
/// consumer dispatching on the code cannot tell the two apart unless they
/// carry different codes — the remedy differs too: a rebuild, not a
/// corrected id.
#[test]
fn a_lookup_missed_against_a_stale_snapshot_is_not_absence() {
    let tmp = scratch();
    let project = tmp.path();
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(project).arg("build").assert().success();

    // On disk, and absent from the snapshot that predates it.
    write_doc(
        project,
        "docs/new.md",
        "---\nid: generic-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    for args in [
        vec!["query", "node", "generic-new"],
        vec!["query", "trust", "generic-new"],
        vec!["query", "dependents", "generic-new"],
        vec!["query", "node", "--path", "docs/new.md"],
    ] {
        let output = nodex(project).args(&args).output().expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert_eq!(
            envelope.pointer("/error/code").and_then(Value::as_str),
            Some("GRAPH_OUTDATED"),
            "nodex {args:?} must not report a document on disk as absent: {envelope}"
        );
        assert!(
            envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("nodex build")),
            "the remedy rides the message: {envelope}"
        );
    }

    // Rebuilt, the same snapshot answers a genuine typo as absence.
    nodex(project).arg("build").assert().success();
    let output = nodex(project)
        .args(["query", "node", "no-such-id"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("NOT_FOUND"),
        "a current snapshot answers absence as absence: {envelope}"
    );
}

/// The membership probe a graph read pays for sees which paths exist, not
/// what they hold. An in-place edit that gives a document a new id moves no
/// path and changes no config, so that probe agrees while the id the caller
/// asked for is sitting on disk — and absence asserted on that evidence is
/// exactly the confident `NOT_FOUND` the code distinction exists to prevent.
/// A miss, which ends the command anyway, is what pays for the content probe.
#[test]
fn a_snapshot_that_moved_no_path_can_still_be_blind_to_the_id_asked_for() {
    let tmp = scratch();
    let project = tmp.path();
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(project).arg("build").assert().success();

    // Same path, same config, different id — nothing the membership probe
    // measures has moved.
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-renamed\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    let code = |id: &str| {
        let output = nodex(project)
            .args(["query", "node", id])
            .output()
            .expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        envelope
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    assert_eq!(
        code("generic-renamed").as_deref(),
        Some("GRAPH_OUTDATED"),
        "the id is on disk; the snapshot merely never read it"
    );
    assert_eq!(
        code("no-such-id").as_deref(),
        Some("GRAPH_OUTDATED"),
        "and a snapshot that cannot be trusted cannot deny an id either"
    );

    nodex(project).arg("build").assert().success();
    assert_eq!(
        code("no-such-id").as_deref(),
        Some("NOT_FOUND"),
        "only a snapshot proven faithful asserts absence"
    );
}

/// Every code a missed lookup can carry names a condition whose remedy can
/// actually succeed. A working tree the process cannot read establishes
/// nothing about the project, so it is neither absence nor staleness: reported
/// as `GRAPH_OUTDATED` it would prescribe a rebuild, and the rebuild fails on
/// the same path — a consumer dispatching on the code retries forever.
#[test]
#[cfg(unix)]
fn a_working_tree_that_cannot_be_read_is_not_a_stale_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch();
    let project = tmp.path();
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        project,
        "docs/sub/s.md",
        "---\nid: generic-s\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    nodex(project).arg("build").assert().success();

    let sub = project.join("docs/sub");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();
    let restore =
        |p: &std::path::Path| fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();

    let build = nodex(project).arg("build").output().expect("ran");
    let build_envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&build.stdout).trim()).expect("json");
    let build_code = build_envelope
        .pointer("/error/code")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let miss = nodex(project)
        .args(["query", "node", "no-such-id"])
        .output()
        .expect("ran");
    let miss_envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&miss.stdout).trim()).expect("json");
    let miss_code = miss_envelope
        .pointer("/error/code")
        .and_then(Value::as_str)
        .map(str::to_owned);

    // A read the snapshot can answer is still answered — the unreadable path
    // is an advisory there, never a gate.
    let hit = run_envelope(nodex(project).args(["query", "node", "generic-a"]));

    restore(&sub);
    let repaired = nodex(project)
        .args(["query", "node", "no-such-id"])
        .output()
        .expect("ran");
    let repaired_envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&repaired.stdout).trim()).expect("json");

    assert_eq!(
        miss_code, build_code,
        "one broken environment, one code on every command that hits it: {miss_envelope}"
    );
    assert_eq!(
        miss_code.as_deref(),
        Some("IO_ERROR"),
        "and it names the unreadable path, not a staleness the probe never found: {miss_envelope}"
    );
    assert_eq!(hit["ok"], Value::Bool(true), "a hit still answers: {hit}");
    assert_eq!(
        repaired_envelope
            .pointer("/error/code")
            .and_then(Value::as_str),
        Some("NOT_FOUND"),
        "and once the tree is readable the same id is plain absence: {repaired_envelope}"
    );
}

/// Graphing the baseline runs the same build `check` runs, so it fails the
/// same typed ways — a duplicate id at the baseline is a duplicate id on
/// either plane. Reporting one condition under two codes is what a consumer
/// dispatching on the code cannot recover from; the prose naming the real
/// cause does not help it.
#[test]
fn both_planes_name_a_failed_baseline_build_with_the_same_code() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-dup\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/x.md",
        "---\nid: generic-dup\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the baseline carries a duplicate id"]);

    // The working tree is clean of the duplicate, so only the baseline build
    // fails and the failure is reached the same way from both planes.
    write_doc(
        project,
        "docs/x.md",
        "---\nid: generic-x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n",
    );
    nodex(project).arg("build").assert().success();

    let code = |args: &[&str]| {
        let output = nodex(project).args(args).output().expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        envelope
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    assert_eq!(
        code(&["check"]).as_deref(),
        Some("DUPLICATE_ID"),
        "the read plane names the cause"
    );
    assert_eq!(
        code(&["retarget", "generic-b", "generic-c"]).as_deref(),
        Some("DUPLICATE_ID"),
        "and the write plane names the same one"
    );
}

/// A baseline pairs documents by node id, so a document that moved since it
/// is the same document — the write seams decline an edit to it exactly as
/// `check` reports one. Addressing the baseline by path instead read a moved
/// document as new, and new is the answer that permits the write.
#[test]
fn a_document_moved_since_the_baseline_is_still_the_same_document() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        project,
        "docs/old.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "generic-a lives at docs/old.md"]);

    // The document moves and keeps its id, which is what makes it the same
    // document to a baseline that pairs by id.
    git(&["mv", "docs/old.md", "docs/new.md"]);
    let moved = project.join("docs/new.md");
    fs::write(
        &moved,
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n\nedited after the move\n",
    )
    .unwrap();
    nodex(project).arg("build").assert().success();

    let output = nodex(project).arg("check").output().expect("ran");
    assert_eq!(
        output.status.code(),
        Some(1),
        "check reds the moved document"
    );

    let before = fs::read_to_string(&moved).unwrap();
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the write plane declines what `check` reds: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("body_immutable/frozen")),
        "and by the rule that governs it: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(&moved).unwrap(),
        before,
        "the frozen body is untouched"
    );
}

/// `identity.id_rules` are how a project declares ids once instead of in
/// every document, so a document that writes no `id:` is the ordinary case,
/// not a degenerate one. A probe that pairs on the id has to complete a
/// proposed document the way the build completes a stored one, or it pairs
/// on an id the document never had and finds no baseline — and no baseline
/// is the answer that permits the write.
#[test]
fn a_document_whose_id_its_config_supplies_is_locked_like_any_other() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    // No `id:` anywhere — every id comes from `identity.id_rules`.
    write_doc(
        project,
        "docs/old.md",
        "---\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "ids come from config"]);

    let frozen = project.join("docs/old.md");
    fs::write(
        &frozen,
        "---\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n\nedited in place\n",
    )
    .unwrap();
    nodex(project).arg("build").assert().success();

    let output = nodex(project).arg("check").output().expect("ran");
    assert_eq!(
        output.status.code(),
        Some(1),
        "check reds the frozen body it read through the same rules"
    );

    let before = fs::read_to_string(&frozen).unwrap();
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the write plane declines what `check` reds: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("body_immutable/frozen")),
        "and by the rule that governs it: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(&frozen).unwrap(),
        before,
        "the frozen body is untouched"
    );
}

/// A baseline is one graph, built by checking the ref out, and the scanner
/// resolves symlinks by design. So a document the ref records as a link —
/// or one under a linked directory — has a baseline like any other, and its
/// locks fire by the rule that governs it rather than by a conservative
/// stand-in. Both planes read the same graph, so they cannot differ on
/// which documents have a baseline or on what it says.
#[test]
#[cfg(unix)]
fn a_document_reached_through_a_link_is_locked_by_the_rule_that_governs_it() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace("[scope]\n", "[scope]\nfollow_symlinks = true\n"),
    )
    .unwrap();
    // Outside `scope.include`, so it is only ever reached through the link.
    write_doc(
        project,
        "sealed_source.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    std::os::unix::fs::symlink("../sealed_source.md", project.join("docs/a.md")).unwrap();
    // The same divergence one level up: git records the link and nothing
    // below it, while a checkout has the whole subtree and the walk graphs
    // every document in it.
    write_doc(
        project,
        "real/v.md",
        "---\nid: generic-v\ntitle: V\nkind: generic\nstatus: archived\n---\n\
         # V\n\nsee [[generic-b]]\n",
    );
    std::os::unix::fs::symlink("../real", project.join("docs/vendor")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "docs/a.md and docs/vendor are links"]);

    // Each sealed document becomes one whose body differs from what the
    // checkout reads through the link, so `check` flags both.
    fs::remove_file(project.join("docs/a.md")).unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n\nan added line\n",
    );
    write_doc(
        project,
        "real/v.md",
        "---\nid: generic-v\ntitle: V\nkind: generic\nstatus: archived\n---\n\
         # V\n\nsee [[generic-b]]\n\nan added line\n",
    );
    nodex(project).arg("build").assert().success();

    let output = nodex(project).arg("check").output().expect("ran");
    assert_eq!(output.status.code(), Some(1), "check reds both documents");
    let checked: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let frozen: Vec<&str> = checked
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter(|v| v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/frozen"))
        .filter_map(|v| v.get("node_id").and_then(Value::as_str))
        .collect();
    assert!(
        frozen.contains(&"generic-a") && frozen.contains(&"generic-v"),
        "the read plane resolves both links and finds frozen baselines: {checked}"
    );

    let before =
        ["docs/a.md", "real/v.md"].map(|rel| fs::read_to_string(project.join(rel)).unwrap());
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the write plane does not rewrite what `check` reds: {envelope}"
    );
    let declined: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .filter(|m| m.contains("body_immutable/frozen"))
        .collect();
    assert_eq!(
        declined.len(),
        2,
        "each document is declined by the rule `check` reported, not a stand-in: {envelope}"
    );
    assert_eq!(
        ["docs/a.md", "real/v.md"].map(|rel| fs::read_to_string(project.join(rel)).unwrap()),
        before,
        "both sealed bodies are untouched"
    );
}

#[test]
fn a_relative_project_root_resolves_against_the_invoking_directory() {
    // `-C` accepts a relative path, and git invocations run in the
    // repository's work tree — so a root left relative would have every
    // path derived from it re-resolved against that work tree instead:
    // a baseline checkout materialised outside the project, and a
    // verdict that degrades to inert because the project is not where
    // the checkout was looked for. How the root was spelled must not
    // reach the verdict.
    let tmp = scratch();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("sub")).unwrap();
    let project = subdirectory_project(&repo, "archived");
    nodex(&project).arg("build").assert().success();

    let absolute = run_envelope(nodex(&project).args(["check", "--since", "HEAD"]));

    let mut relative_cmd = Command::cargo_bin("nodex").expect("nodex binary in cargo target");
    relative_cmd.current_dir(repo.join("sub")).args([
        "-C",
        "../docs-site",
        "check",
        "--since",
        "HEAD",
    ]);
    let relative = run_envelope(&mut relative_cmd);

    assert_eq!(
        relative, absolute,
        "the same project, spelled two ways, is one verdict"
    );
    let strays: Vec<PathBuf> = walk_entries(tmp.path())
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".nodex-"))
        })
        .collect();
    assert!(
        strays.is_empty(),
        "a materialised baseline is removed, wherever it was put: {strays:?}"
    );
    assert!(
        !tmp.path().join("docs-site").exists(),
        "nothing is created beside the repository"
    );
}

/// Every path under `dir`, recursively — for assertions about what a run
/// left behind.
fn walk_entries(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            found.push(path);
        }
    }
    found
}

#[test]
fn every_mutating_command_discloses_an_unenforced_baseline() {
    // A project that configures immutability locks but has no git
    // repository cannot have them enforced. `check` says so; a mutation
    // that writes under the same condition must say so too, or the
    // caller reads a clean result from a run that enforced nothing —
    // the same silent-skip failure mode in the write plane.
    let cases: [&[&str]; 5] = [
        &["retarget", "generic-b", "generic-c"],
        &["lifecycle", "set", "generic-b", "--status", "archived"],
        &["rename", "docs/c.md", "docs/c2.md"],
        &["migrate", "--apply"],
        &[
            "scaffold",
            "--kind",
            "generic",
            "--title",
            "New",
            "--path",
            "docs/new.md",
        ],
    ];
    for args in cases {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(root.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
        write_doc(
            root,
            "docs/a.md",
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
             # A\n\nsee [[generic-b]]\n",
        );
        write_doc(
            root,
            "docs/b.md",
            "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
        );
        write_doc(
            root,
            "docs/c.md",
            "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
        );
        // A frontmatter-less document, so `migrate --apply` has a real
        // write to perform rather than an empty plan.
        write_doc(root, "docs/bare.md", "# Bare\n");
        nodex(root).arg("build").assert().success();

        let envelope = run_envelope(nodex(root).args(args));
        assert!(
            envelope
                .get("warnings")
                .and_then(Value::as_array)
                .expect("warnings")
                .iter()
                .filter_map(warning_msg)
                .any(|m| m.contains("immutability rules are inert")),
            "nodex {args:?} must disclose that the configured locks did not engage: {envelope}"
        );
    }
}

#[test]
fn scaffold_and_retarget_refuse_reference_unsafe_ids() {
    // An explicit id must round-trip through every reference syntax
    // nodex writes — trim-unstable or metacharacter-bearing ids would
    // be written / repointed into forms the next build cannot resolve.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    nodex(root).arg("build").assert().success();

    for bad in [" padded", "padded ", "with]bracket", "pipe|sep"] {
        let output = nodex(root)
            .args(["scaffold", "--kind", "generic", "--title", "T"])
            .args(["--id", bad, "--path", "docs/t.md"])
            .output()
            .expect("ran");
        assert_eq!(output.status.code(), Some(2), "{bad:?} refused");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert_eq!(
            envelope.pointer("/error/code").and_then(Value::as_str),
            Some("CONFIG_ERROR")
        );
        assert!(!root.join("docs/t.md").exists(), "nothing written");
    }

    // A unicode id remains fully legal.
    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "U"])
        .args(["--id", "유니코드-아이디", "--path", "docs/u.md"])
        .assert()
        .success();

    // retarget refuses to repoint ONTO a reference-unsafe id (a
    // hand-written doc can carry one; rewriting references to it would
    // produce forms the build trims into a different id).
    write_doc(
        root,
        "docs/padded.md",
        "---\nid: \" my id \"\ntitle: Padded\nkind: generic\nstatus: active\n---\n# P\n",
    );
    write_doc(
        root,
        "docs/ref.md",
        "---\nid: generic-ref\ntitle: R\nkind: generic\nstatus: active\nrelated: 유니코드-아이디\n---\n# R\n",
    );
    nodex(root).arg("build").assert().success();
    let output = nodex(root)
        .args(["retarget", "유니코드-아이디", " my id "])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("reference-safe"),
        "names the invariant: {envelope}"
    );
}

#[test]
fn rename_refuses_a_directory_source() {
    // rename moves a single document; a directory source would slide a
    // whole tree past the per-document guarantees (destination gate, id
    // anchoring, reference rewriting) and dangle every reference into
    // it — tracked or not, it is refused loudly.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/sub/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    fs::create_dir_all(root.join("loose")).unwrap();
    nodex(root).arg("build").assert().success();

    for dir in ["docs/sub", "loose"] {
        let output = nodex(root)
            .args(["rename", dir, "elsewhere"])
            .output()
            .expect("ran");
        assert_eq!(output.status.code(), Some(2), "{dir} refused");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert!(
            envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("directory"),
            "names the cause: {envelope}"
        );
        assert!(root.join(dir).exists(), "{dir} untouched");
    }
    assert!(root.join("docs/sub/a.md").exists(), "tree intact");
}

/// A project whose `docs/a.md` is reachable under folded spellings, and
/// whether this volume folds them at all. On a case-sensitive filesystem
/// every such spelling is a genuinely new path, so each seam gates it
/// normally — the assertions below say so rather than skipping.
fn folding_project() -> (TempDir, bool) {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();
    let folds = root.join("NODEX.TOML").exists();
    (tmp, folds)
}

/// The envelope a command emitted, whether it succeeded or not — the
/// spelling guard's tests assert on both outcomes, because a
/// case-sensitive volume gates the same spelling as an ordinary new path.
fn envelope_of(cmd: &mut Command) -> Value {
    let output = cmd.output().expect("command ran");
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("stdout is parseable JSON")
}

fn spelling_refusal(envelope: &Value) -> bool {
    envelope.get("ok").and_then(Value::as_bool) == Some(false)
        && envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("spelled differently from the filesystem's own")
}

#[test]
fn check_content_refuses_a_folded_spelling() {
    // Every comparison downstream of the gate is exact, so bytes proposed
    // under a spelling the scan never produces are judged as a document
    // nothing else can find — while the write they clear lands on the real
    // file.
    let (tmp, folds) = folding_project();
    let root = tmp.path();
    let proposal = "---\nid: generic-other\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n";
    let output = nodex(root)
        .args(["check", "--content", "docs/A.MD=-"])
        .write_stdin(proposal)
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    if folds {
        assert!(spelling_refusal(&envelope), "{envelope}");
    } else {
        assert_eq!(output.status.code(), Some(0), "{envelope}");
    }
}

#[test]
fn a_folded_spelling_does_not_carry_a_write_past_the_baseline_lock() {
    // The sharpest form: `scaffold --force` at the document's own spelling
    // is refused by the immutability lock, so the same write at a folded
    // spelling must be refused too. The lock reads the baseline by exact
    // path — an unrefused folded spelling looks up nothing, finds no frozen
    // record, and overwrites the file the lock exists to protect.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         trigger = \"creation\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/sub/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# frozen history\n",
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let folds = root.join("NODEX.TOML").exists();

    let exact = envelope_of(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Impostor",
        "--path",
        "docs/sub/a.md",
        "--force",
    ]));
    assert_eq!(
        exact.get("ok").and_then(Value::as_bool),
        Some(false),
        "the lock refuses the write at the document's own spelling: {exact}"
    );

    let folded = envelope_of(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Impostor",
        "--path",
        "docs/SUB/a.md",
        "--force",
    ]));
    if folds {
        assert!(spelling_refusal(&folded), "{folded}");
    }
    // The property under test holds on either volume: the frozen record is
    // still there. Where spellings fold, because the write was refused as one;
    // where they do not, because `docs/SUB/a.md` is a different file — one the
    // project refuses for its own reasons, since the id it infers from the
    // stem is already taken.
    assert!(
        fs::read_to_string(root.join("docs/sub/a.md"))
            .unwrap()
            .contains("frozen history"),
        "the frozen record survives: {folded}"
    );
}

#[test]
fn rename_refuses_a_folded_destination_spelling() {
    // The destination is written into every rewritten reference. A folded
    // one is rewritten as authored, resolves against a path index that
    // spells it the filesystem's way, and dangles every link the command
    // reported as updated.
    let (tmp, folds) = folding_project();
    let root = tmp.path();
    write_doc(
        root,
        "docs/ref.md",
        "---\nid: generic-ref\ntitle: R\nkind: generic\nstatus: active\n---\n[a](a.md)\n",
    );
    nodex(root).arg("build").assert().success();
    fs::create_dir_all(root.join("docs/sub")).unwrap();

    let env = envelope_of(nodex(root).args(["rename", "docs/a.md", "docs/SUB/moved.md"]));
    if folds {
        assert!(spelling_refusal(&env), "{env}");
    } else {
        assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true), "{env}");
    }
}

#[test]
fn a_path_whose_components_do_not_exist_yet_is_not_a_folded_spelling() {
    // The guard asks the filesystem, and a path that exists under no
    // spelling cannot alias one that does — so authoring a document into a
    // directory tree that does not exist yet stays legal on every volume.
    let (tmp, _folds) = folding_project();
    let root = tmp.path();
    let env = run_envelope(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Fresh",
        "--path",
        "docs/Brand/New/Deep/fresh.md",
    ]));
    assert_eq!(
        env.pointer("/data/path").and_then(Value::as_str),
        Some("docs/Brand/New/Deep/fresh.md"),
        "{env}"
    );
    assert!(root.join("docs/Brand/New/Deep/fresh.md").exists());
}

#[test]
fn rename_of_an_untracked_file_is_a_plain_guarded_move() {
    // A source the scan never admitted has no node and no edges —
    // nothing can dangle, so the rename is a plain guarded move: no
    // destination gate, no id anchor, no reference rewriting. The gate
    // fires exactly when it protects something (a tracked source).
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/loose.md"), "# untracked\n").unwrap();
    nodex(root).arg("build").assert().success();

    // Untracked → untracked: plain move succeeds.
    let env = run_envelope(nodex(root).args(["rename", "notes/loose.md", "notes/moved.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(0)
    );
    assert!(root.join("notes/moved.md").exists());
    assert!(!root.join("notes/loose.md").exists());

    // Tracked → out-of-scope: still refused (the protected case).
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["rename", "docs/a.md", "notes/a.md"])
        .assert()
        .failure()
        .code(2);
    assert!(root.join("docs/a.md").exists(), "tracked source untouched");
}

#[test]
fn dot_prefixed_mutation_paths_normalize_like_check_content() {
    // `./docs/a.md` and `docs/a.md` name the same document; scaffold and
    // rename collapse the `.` segment (as `check --content` does) so the
    // admission probe and id inference key on the scanner's
    // root-relative form — the dot-prefixed spelling of a perfectly
    // scoped path must not be refused.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Dot"])
        .args(["--path", "./docs/dot.md"])
        .assert()
        .success();
    assert!(root.join("docs/dot.md").exists());

    nodex(root)
        .args(["rename", "./docs/dot.md", "./docs/dot2.md"])
        .assert()
        .success();
    assert!(root.join("docs/dot2.md").exists());
    nodex(root).arg("build").assert().success();
    let data = run_json(nodex(root).args(["query", "node", "--path", "docs/dot2.md"]));
    assert!(
        data.pointer("/node/id").and_then(Value::as_str).is_some(),
        "renamed doc graphed under the normalized key: {data}"
    );
}

#[test]
fn rename_refuses_to_anchor_into_a_doc_with_non_scalar_kind() {
    // A non-scalar `kind:` means the build cannot parse the document —
    // no node exists, so anchoring a path-inferred id into it would be
    // phantom work; rename refuses loudly instead.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/broken.md",
        "---\nkind:\n  - weird\ntitle: Broken\nstatus: active\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["rename", "docs/broken.md", "docs/moved.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("kind"),
        "names the broken field: {envelope}"
    );
    assert!(root.join("docs/broken.md").exists(), "source untouched");
}

#[test]
fn rename_anchors_the_id_of_the_effective_frontmatter_kind() {
    // The build derives a doc's id from its frontmatter `kind:` when
    // declared (path inference is only the fallback). The rename anchor
    // must pin exactly that id — anchoring the path-inferred kind's id
    // would write a wrong id and dangle every cross-reference.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"guide\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/noid.md",
        "---\nkind: guide\ntitle: NoId\nstatus: active\n---\n# N\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["rename", "docs/noid.md", "docs/renamed.md"]));
    assert_eq!(
        env.pointer("/data/id_stability/id").and_then(Value::as_str),
        Some("guide-noid"),
        "anchors the build's effective id, not the path-inferred one: {env}"
    );
    assert!(
        fs::read_to_string(root.join("docs/renamed.md"))
            .unwrap()
            .contains("id: \"guide-noid\"")
    );
    nodex(root).arg("build").assert().success();
    let data = run_json(nodex(root).args(["query", "node", "--path", "docs/renamed.md"]));
    assert_eq!(
        data.pointer("/node/id").and_then(Value::as_str),
        Some("guide-noid")
    );
}

/// Two things drop a document from the baseline graph, and both leave the
/// diff-aware locks inert for it: a parse failure there, and a
/// `conditional_exclude` rule that matched there. A parent terminal at the
/// baseline but active now takes its sub-artifacts out of the *before* graph
/// alone, so a frozen child has nothing to be compared against — the write
/// proceeds, and the envelope has to say why the lock did not.
#[test]
fn a_baseline_that_conditionally_excluded_a_document_says_so() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"docs/parent.md\"\n\
         child_glob = \"docs/parent.*.md\"\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         initial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [parser]\nwikilink_enabled = true\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         trigger = \"terminal\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    // Terminal at the baseline, so the child is excluded there.
    write_doc(
        root,
        "docs/parent.md",
        "---\nid: generic-parent\ntitle: P\nkind: generic\nstatus: archived\n---\n# P\n",
    );
    write_doc(
        root,
        "docs/parent.child.md",
        "---\nid: generic-child\ntitle: C\nkind: generic\nstatus: archived\n---\n\
         # C\n\nsee [[generic-t]]\n",
    );
    write_doc(
        root,
        "docs/t.md",
        "---\nid: generic-t\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    write_doc(
        root,
        "docs/u.md",
        "---\nid: generic-u\ntitle: U\nkind: generic\nstatus: active\n---\n# U\n",
    );
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "the parent is terminal at the baseline",
    ]);

    // Active now, so the child is in the current scope but has no baseline.
    write_doc(
        root,
        "docs/parent.md",
        "---\nid: generic-parent\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n",
    );
    nodex(root).arg("build").assert().success();

    let envelope = run_envelope(nodex(root).args(["retarget", "generic-t", "generic-u"]));
    let advisory = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .any(|m| m.contains("docs/parent.child.md") && m.contains("conditional_exclude"));
    assert!(
        advisory,
        "an inert lock names the document it did not guard: {envelope}"
    );
}

/// The third way a baseline loses a document, and the only one the walk used
/// to drop with no record at all: a tracked symlink whose target the ref does
/// not carry materialises dangling, and `is_dir` / `is_file` both answer
/// false, so the entry fell out of the classification entirely — not a parse
/// failure, not a conditional exclude, not a warning. The frozen body it
/// pointed at could then be rewritten with `check` green and an empty
/// warnings array, which is the exact failure the advisory chain exists to
/// prevent.
#[test]
#[cfg(unix)]
fn a_baseline_whose_symlink_resolves_to_nothing_says_so() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    fs::create_dir_all(project.join("outside")).unwrap();
    // The target is untracked, so the ref carries the link and nothing else.
    fs::write(project.join(".gitignore"), "outside/\n").unwrap();
    fs::write(
        project.join("outside/a.md"),
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nFrozen decision.\n",
    )
    .unwrap();
    fs::create_dir_all(project.join("docs")).unwrap();
    std::os::unix::fs::symlink("../outside/a.md", project.join("docs/a.md")).unwrap();
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "the link is tracked, its target is not",
    ]);

    // Rewrite the frozen body through the link.
    fs::write(
        project.join("outside/a.md"),
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nRewritten.\n",
    )
    .unwrap();
    nodex(project).arg("build").assert().success();

    let envelope = run_envelope(nodex(project).arg("check"));
    let named = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .any(|m| m.contains("docs/a.md") && m.contains("holds no readable document"));
    assert!(
        named,
        "the baseline names the document it could not read, so an inert lock is not silent: \
         {envelope}"
    );
}

/// A baseline is a checkout, and git records a symlink rather than what it
/// points at. Following one *out* of the checkout reads the present and
/// presents it as the ref's past: the before-body then equals the after-body,
/// the lock never fires, and `check` — the CI gate — passes on a frozen body
/// that was edited. A ref build keeps to the checkout, so the document has no
/// baseline node and the envelope says why. A symlink resolving *inside*
/// stays exactly as locked as any other document: the working tree's
/// reader-follows discipline is untouched, only a ref's is confined.
#[test]
#[cfg(unix)]
fn a_baseline_does_not_read_through_a_symlink_that_leaves_the_checkout() {
    let locked_through = |target: fn(&std::path::Path) -> std::path::PathBuf| {
        let tmp = scratch();
        let project = tmp.path().to_path_buf();
        let git = git_runner(&project);
        git(&["init", "-q"]);
        fs::write(
            project.join("nodex.toml"),
            LOCKED_PROJECT_CONFIG.replace("[scope]\n", "[scope]\nfollow_symlinks = true\n"),
        )
        .unwrap();
        fs::create_dir_all(project.join("real")).unwrap();
        fs::create_dir_all(project.join("docs")).unwrap();
        fs::write(
            project.join("real/a.md"),
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
             # A\n\nFrozen decision.\n",
        )
        .unwrap();
        write_doc(
            &project,
            "docs/b.md",
            "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
        );
        std::os::unix::fs::symlink(target(&project), project.join("docs/linked")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "the link is what git records"]);

        fs::write(
            project.join("real/a.md"),
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
             # A\n\nRewritten.\n",
        )
        .unwrap();
        nodex(&project).arg("build").assert().success();
        // `check` exits 1 when it reds something, so the envelope is read
        // directly rather than through the success-asserting helper.
        let output = nodex(&project).arg("check").output().expect("ran");
        serde_json::from_str::<Value>(String::from_utf8_lossy(&output.stdout).trim()).expect("json")
    };

    // Resolving inside the checkout: the ref carries the target, so the lock
    // has a real before-state and fires.
    let inside = locked_through(|_| std::path::PathBuf::from("../real"));
    assert_eq!(
        inside["data"]["total"], 1,
        "a contained link is locked like any other document: {inside}"
    );

    // Resolving outside: the ref recorded the link only, so there is nothing
    // to compare against — and the envelope has to say so rather than pass
    // in silence.
    let outside = locked_through(|project| project.join("real"));
    assert_eq!(
        outside["data"]["total"], 0,
        "no baseline content exists for it, so no rule can fire: {outside}"
    );
    let named = outside
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .any(|m| m.contains("docs/linked") && m.contains("outside the checkout"));
    assert!(
        named,
        "the inert lock names the path the ref could not supply: {outside}"
    );
}

/// The move is the half that cannot be undone, so every refusal has to happen
/// before it. Asking the rules for a verdict means rebuilding the project with
/// the rewrites overlaid, and a project that does not build has no verdict to
/// give — if that refusal arrived after `fs::rename`, the rename would move a
/// file and then decline to rebase its references, leaving the tree worse than
/// it found it. `retarget` establishes the same precondition by building first.
/// A move writes no bytes, so nothing about it is expressible as a rewrite —
/// and a gate that only sees rewrites cannot see it. What it does change is the
/// document's *path*, and every field config derives from a path moves with it:
/// `kind` through `identity.kind_rules`. A lock on such a field fires at check
/// time on a terminal document that crossed a rule boundary, so the seam has to
/// refuse the move — before it happens, because afterwards a refusal cannot be
/// honoured.
#[test]
fn rename_refuses_a_move_that_would_change_a_locked_path_derived_field() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"adr\", \"note\", \"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/adr/**\"\nkind = \"adr\"\n\
         [[identity.kind_rules]]\nglob = \"docs/notes/**\"\nkind = \"note\"\n\
         [[rules.frontmatter_immutable]]\nname = \"kind-locked\"\nfields = [\"kind\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n",
    )
    .unwrap();
    // No frontmatter `kind:` — it is path-derived, so the move changes it.
    write_doc(
        project,
        "docs/adr/a.md",
        "---\nid: adr-a\ntitle: A\nstatus: superseded\n---\n# A\n",
    );
    write_doc(
        project,
        "docs/notes/k.md",
        "---\nid: note-k\ntitle: K\nkind: note\nstatus: active\n---\n# K\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a terminal adr"]);

    let output = nodex(project)
        .args(["rename", "docs/adr/a.md", "docs/notes/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR"),
        "the seam refuses what `check` would red after the move: {envelope}"
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("frontmatter_immutable/kind-locked"),
        "and names the rule that governs it: {envelope}"
    );
    assert!(
        project.join("docs/adr/a.md").exists() && !project.join("docs/notes/a.md").exists(),
        "and nothing moved, so the refusal is honourable"
    );

    // The same move within the kind's own directory changes nothing locked.
    nodex(project)
        .args(["rename", "docs/adr/a.md", "docs/adr/renamed.md"])
        .assert()
        .success();
    assert!(project.join("docs/adr/renamed.md").exists());
}

/// The gate judges the document the move *produces*, and anchoring is part of
/// what the move writes. A frozen document whose id is path-derived keeps that
/// id across the move — rename pins it into the frontmatter first — so the
/// baseline record survives at the new path and nothing was destroyed. Judging
/// the pre-anchor bytes instead would watch the id change under it, read the
/// record as gone, and refuse a move `check` has no complaint about.
#[test]
fn rename_gates_the_anchored_document_the_move_produces() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [[rules.frontmatter_immutable]]\nname = \"title-locked\"\nfields = [\"title\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n",
    )
    .unwrap();
    // No `id:` — it is stem-derived, so the move would change it and rename
    // anchors the old one. Terminal, so the frontmatter lock holds the record.
    write_doc(
        project,
        "docs/a.md",
        "---\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a frozen doc with a derived id"]);

    let data = run_json(nodex(project).args(["rename", "docs/a.md", "docs/a-renamed.md"]));
    assert_eq!(
        data.pointer("/id_stability/id").and_then(Value::as_str),
        Some("generic-a"),
        "the record travels under its own id, so the baseline sees a move: {data}"
    );
    let moved = fs::read_to_string(project.join("docs/a-renamed.md")).unwrap();
    assert!(
        moved.contains("id: \"generic-a\"") || moved.contains("id: generic-a"),
        "and the moved document carries it: {moved}"
    );
}

/// The precondition is that the project the move produces graphs — which is
/// not the same question as whether a baseline refuses it, and a project with
/// no `rules.immutable_baseline` never asks the second. Downstream of
/// `fs::rename` the reference rewrite can only degrade to a warning, so a
/// project `nodex build` refuses must stop the move while refusing still costs
/// nothing.
/// A `conditional_exclude` eviction is not a destruction. Making a parent
/// terminal drops its sub-artifacts from *scope* by design — their files stay
/// on disk, unmodified — and `check` reports nothing for it on either plane.
/// Refusing the transition would be a refusal with no reading to be consistent
/// with, and once the parent is terminal no command could clear it.
#[test]
fn archiving_a_parent_that_evicts_a_frozen_sub_artifact_is_allowed() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"docs/**/SPEC.md\"\n\
         child_glob = \"docs/**/note-*.md\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         trigger = \"creation\"\n",
    )
    .unwrap();
    write_doc(
        project,
        "docs/spec/SPEC.md",
        "---\nid: generic-SPEC\ntitle: Spec\nkind: generic\nstatus: active\n---\n# Spec\n",
    );
    write_doc(
        project,
        "docs/spec/note-a.md",
        "---\nid: generic-note-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a frozen note under an active spec"]);

    nodex(project)
        .args(["lifecycle", "set", "generic-SPEC", "--status", "archived"])
        .assert()
        .success();
    let data = run_json(nodex(project).arg("check"));
    assert_eq!(
        data.get("total").and_then(Value::as_u64),
        Some(0),
        "the read plane agrees there is nothing to report: {data}"
    );
    assert!(
        project.join("docs/spec/note-a.md").exists(),
        "and the evicted note is still on disk — it left scope, not the project"
    );
}

/// The destination's parent does not exist when the gate runs — `rename`
/// creates it afterwards — so resolving the moved link through the filesystem
/// cannot fold `..` and every relative target reads as unreachable. A move to a
/// new directory at the same depth resolves to exactly the same document.
#[test]
#[cfg(unix)]
fn a_symlink_moved_to_a_new_directory_at_the_same_depth_still_resolves() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    fs::create_dir_all(project.join("store")).unwrap();
    fs::create_dir_all(project.join("docs/x")).unwrap();
    fs::write(
        project.join("store/alpha.md"),
        "---\nid: generic-alpha\ntitle: Alpha\nkind: generic\nstatus: archived\n---\n# Alpha\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../../store/alpha.md", project.join("docs/x/a.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a linked document"]);

    // `docs/y` does not exist yet; rename creates it after the gate.
    nodex(project)
        .args(["rename", "docs/x/a.md", "docs/y/a.md"])
        .assert()
        .success();
    let data = run_json(nodex(project).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "the link still reaches the same document from its new directory: {data}"
    );
}

/// Folding the *spelled* destination is not enough: an existing destination
/// directory can itself be a symlink, and then the moved link's `..` climbs
/// from somewhere else entirely. The existing part of the destination has to be
/// canonicalised before anything is folded, or the gate approves a move that
/// leaves the document — and its frozen record — unreachable.
#[test]
#[cfg(unix)]
fn rename_resolves_a_symlinked_destination_directory_before_folding() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    for dir in ["store", "store2", "docs/x"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(
        project.join("store/alpha.md"),
        "---\nid: generic-alpha\ntitle: Alpha\nkind: generic\nstatus: archived\n---\n\
         # Alpha\n\nFrozen.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../../store/alpha.md", project.join("docs/x/a.md")).unwrap();
    // Spelled, `docs/y/../../store/alpha.md` folds to a file that exists.
    // Really, `docs/y` is `store2`, so the link climbs out of the project.
    std::os::unix::fs::symlink("../store2", project.join("docs/y")).unwrap();
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "a linked doc and a symlinked directory",
    ]);

    let output = nodex(project)
        .args(["rename", "docs/x/a.md", "docs/y/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the move leaves the document unreachable, so it is refused: {envelope}"
    );
    let data = run_json(nodex(project).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "and the frozen record still stands: {data}"
    );
}

/// A name is taken by whatever entry stands there, resolving or not.
///
/// The destination guard asked `exists`, which follows the link and answers
/// for its target — so a symlink pointing at nothing read as a free name, and
/// the raw `fs::rename` below replaced the link without a word. The source
/// side of the same guard has always used `symlink_metadata`.
#[test]
#[cfg(unix)]
fn rename_does_not_take_a_name_a_dangling_link_already_holds() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/old.md",
        "---\nid: old\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    std::os::unix::fs::symlink("../nowhere/target.md", root.join("docs/taken.md")).unwrap();

    let output = nodex(root)
        .args(["rename", "docs/old.md", "docs/taken.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("ALREADY_EXISTS"),
        "{env}"
    );
    assert_eq!(
        fs::read_link(root.join("docs/taken.md")).unwrap(),
        std::path::Path::new("../nowhere/target.md"),
        "the link is still the entry standing there"
    );
    assert!(root.join("docs/old.md").exists(), "and nothing moved");

    // A free name is still free.
    nodex(root)
        .args(["rename", "docs/old.md", "docs/new.md"])
        .assert()
        .success();
}

/// A `..` inside the link's own target is resolved by the kernel against what
/// precedes it, so one that traverses a dangling symlink finds nothing — and
/// neither may the gate. Falling back to the spelling there names whatever file
/// happens to sit at the folded path, which is how a byte-identical decoy gets
/// a frozen record replaced with the gate reporting success.
#[test]
#[cfg(unix)]
fn rename_does_not_fall_back_to_spelling_through_a_dangling_link() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nexclude = [\"docs/**/store/**\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    for dir in ["store", "docs/x", "docs/deep/store", "other"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    let frozen = "---\nid: alpha\ntitle: Alpha\nkind: generic\nstatus: archived\n---\n# Alpha\n";
    fs::write(project.join("store/alpha.md"), frozen).unwrap();
    // Byte-identical decoy sitting exactly where the spelling would land.
    fs::write(project.join("docs/deep/store/alpha.md"), frozen).unwrap();
    std::os::unix::fs::symlink("../other", project.join("docs/sub")).unwrap();
    // Dangling: nothing at ../../elsewhere/nested, so the kernel cannot
    // traverse `docs/deep/sub/..` at all.
    std::os::unix::fs::symlink("../../elsewhere/nested", project.join("docs/deep/sub")).unwrap();
    std::os::unix::fs::symlink("../sub/../store/alpha.md", project.join("docs/x/a.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a frozen record behind two links"]);

    let output = nodex(project)
        .args(["rename", "docs/x/a.md", "docs/deep/y/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the moved link reaches nothing, so the move is refused: {envelope}"
    );
    nodex(project).arg("build").assert().success();
    let nodes = run_json(nodex(project).args(["query", "nodes"]));
    let ids: Vec<&str> = nodes
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ids,
        ["alpha"],
        "and the frozen record still stands: {nodes}"
    );
}

/// `x/..` is `ENOTDIR` when `x` is a regular file, but `canonicalize` answers
/// happily and popping steps into that file's parent — so a target whose `..`
/// crosses a file resolves for the gate and not for the kernel, landing on a
/// real unrelated document. With a byte-identical one there, no rule fires
/// either and the frozen record is replaced in silence.
#[test]
#[cfg(unix)]
fn rename_refuses_a_target_stepping_out_of_a_regular_file() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nexclude = [\"docs/**/store/**\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    let frozen = "---\nid: rec\ntitle: Rec\nkind: generic\nstatus: archived\n---\n# Rec\n";
    for dir in ["docs/a/q", "docs/a/store", "docs/b/store"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(project.join("docs/a/store/rec.md"), frozen).unwrap();
    // Byte-identical, exactly where popping out of the file would land.
    fs::write(project.join("docs/b/store/rec.md"), frozen).unwrap();
    // `q` is a directory beside the source and a regular file beside the
    // destination, so the same target string is traversable from one and
    // ENOTDIR from the other.
    fs::write(project.join("docs/b/q"), "not a directory\n").unwrap();
    std::os::unix::fs::symlink("q/../store/rec.md", project.join("docs/a/doc.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a frozen record behind a q/.. hop"]);

    let output = nodex(project)
        .args(["rename", "docs/a/doc.md", "docs/b/doc.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the kernel cannot step out of a file, so neither may the gate: {envelope}"
    );
    nodex(project).arg("build").assert().success();
    let nodes = run_json(nodex(project).args(["query", "nodes"]));
    let paths: Vec<&str> = nodes
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("path").and_then(Value::as_str))
        .collect();
    assert_eq!(
        paths,
        ["docs/a/doc.md"],
        "and the frozen record still stands where it was: {nodes}"
    );
}

/// A target may leave the chain of directories the move is about to create and
/// re-enter it by name. Those segments are still to-be-created, so `..` over
/// them is the spelling — tracking membership by a countdown of `..` seen
/// forgets that and refuses a move the kernel performs happily.
#[test]
#[cfg(unix)]
fn rename_allows_a_target_that_re_enters_a_created_segment() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\nexclude = [\"**/store/**\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    for dir in ["other/x", "other/y", "other/store", "docs/store"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    // The same document either side of the move: `../y/../store/rec.md` means
    // `other/store/rec.md` from `other/x`, and `docs/store/rec.md` from the
    // `docs/y` the move creates. Out of scope itself, so the two are not a
    // duplicate id — only what the link reaches.
    let rec = "---\nid: generic-rec\ntitle: Rec\nkind: generic\nstatus: archived\n---\n# Rec\n";
    fs::write(project.join("other/store/rec.md"), rec).unwrap();
    fs::write(project.join("docs/store/rec.md"), rec).unwrap();
    std::os::unix::fs::symlink("../y/../store/rec.md", project.join("other/x/doc.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a target that leaves and re-enters"]);

    nodex(project)
        .args(["rename", "other/x/doc.md", "docs/y/doc.md"])
        .assert()
        .success();
    let data = run_json(nodex(project).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "the link reaches the same document from the created directory: {data}"
    );
}

/// A relative target can land back on the source link, which the move is about
/// to take away. It reads fine right now — that is the trap — so the gate must
/// judge it as the post-move world will: gone.
#[test]
#[cfg(unix)]
fn rename_refuses_a_target_that_lands_on_the_source_itself() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"x/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    for dir in ["x/y", "x/z/w", "y"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(
        project.join("y/link.md"),
        "---\nid: rec\ntitle: R\nkind: generic\nstatus: archived\n---\n# R\n\nFrozen.\n",
    )
    .unwrap();
    // From `x/y` this is `y/link.md`; from `x/z/w` it is `x/y/link.md` — the
    // source, which will not be there once the move lands.
    std::os::unix::fs::symlink("../../y/link.md", project.join("x/y/link.md")).unwrap();
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "a link that would point at its own old home",
    ]);

    let output = nodex(project)
        .args(["rename", "x/y/link.md", "x/z/w/link.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the moved link would point at the path the move emptied: {envelope}"
    );
    nodex(project).arg("build").assert().success();
    let nodes = run_json(nodex(project).args(["query", "nodes"]));
    assert_eq!(
        nodes.get("total").and_then(Value::as_u64),
        Some(1),
        "and the frozen record still stands: {nodes}"
    );
}

/// Landing *on* the source is only the simplest way to depend on it. The
/// target may reach a link that reaches another that reaches the source, and
/// every hop in that chain stops working when the source moves — so the whole
/// chain is followed, not just the path the walk computed.
#[test]
#[cfg(unix)]
fn rename_refuses_a_target_reaching_the_source_through_another_link() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"x/**/*.md\"]\nexclude = [\"x/y/b.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    for dir in ["x/y", "x/z/w", "y"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(
        project.join("y/b.md"),
        "---\nid: rec\ntitle: R\nkind: generic\nstatus: archived\n---\n# R\n\nFrozen.\n",
    )
    .unwrap();
    // From `x/y` the target is `y/b.md`, the real document. From the
    // destination `x/z/w` the same spelling is `x/y/b.md` — a link that
    // reaches the document only by way of `a.md`, the one being moved.
    std::os::unix::fs::symlink("../../y/b.md", project.join("x/y/a.md")).unwrap();
    std::os::unix::fs::symlink("a.md", project.join("x/y/b.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a chain that runs through the source"]);

    let output = nodex(project)
        .args(["rename", "x/y/a.md", "x/z/w/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the chain runs through the path the move empties: {envelope}"
    );
    nodex(project).arg("build").assert().success();
    let nodes = run_json(nodex(project).args(["query", "nodes"]));
    assert_eq!(
        nodes.get("total").and_then(Value::as_u64),
        Some(1),
        "and the frozen record still stands: {nodes}"
    );
}

/// Opening a FIFO blocks until a writer appears, so a target that resolves to
/// one at the destination hung the command with no envelope at all. The
/// scanner admits a document by `is_file()`, and so does the gate — decided
/// from metadata, which never opens anything.
#[test]
#[cfg(unix)]
fn rename_does_not_open_a_non_regular_destination_target() {
    let tmp = scratch();
    let project = tmp.path();
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nexclude = [\"**/store/**\"]\n\
         [kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    for dir in ["docs/x", "docs/z/w", "store", "docs/store"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(
        project.join("store/f.md"),
        "---\nid: rec\ntitle: R\nkind: generic\nstatus: active\n---\n# R\n",
    )
    .unwrap();
    // Not git, and std has no FIFO constructor — the one way to build the
    // fixture this test is about.
    #[expect(
        clippy::disallowed_methods,
        reason = "mkfifo is the only way to create the non-regular file under test"
    )]
    let made = std::process::Command::new("mkfifo")
        .arg(project.join("docs/store/f.md"))
        .status()
        .expect("mkfifo");
    assert!(made.success());
    std::os::unix::fs::symlink("../../store/f.md", project.join("docs/x/a.md")).unwrap();
    nodex(project).arg("build").assert().success();

    // Spawned rather than run to completion: a regression here blocks forever,
    // and a test that hangs tells CI nothing. `assert_cmd` waits, so this is
    // the one place the binary is driven directly.
    #[expect(
        clippy::disallowed_methods,
        reason = "the binary must be killable, which running it to completion cannot be"
    )]
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_nodex"))
        .args(["-C", project.to_str().unwrap()])
        .args(["rename", "docs/x/a.md", "docs/z/w/a.md"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawned");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let finished = loop {
        match child.try_wait().expect("wait") {
            Some(_) => break true,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                break false;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    assert!(
        finished,
        "rename must decide from metadata instead of opening the target"
    );
}

/// `rename` moves a file symlink as the link itself, so the destination holds
/// what that link resolves to *from there*. A relative target that changes
/// directory depth resolves somewhere else — here, nowhere — so judging the
/// source's bytes would let every pre-move gate approve a move that destroys
/// the record.
#[test]
#[cfg(unix)]
fn rename_judges_where_a_moved_symlink_will_point() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    fs::create_dir_all(project.join("store")).unwrap();
    fs::create_dir_all(project.join("docs")).unwrap();
    fs::write(
        project.join("store/a.md"),
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nFrozen decision.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../store/a.md", project.join("docs/a.md")).unwrap();
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "the document is reached through a link",
    ]);

    let output = nodex(project)
        .args(["rename", "docs/a.md", "docs/sub/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "a move that leaves the document unreachable is refused: {envelope}"
    );
    let data = run_json(nodex(project).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "and the record still stands: {data}"
    );
}

#[test]
fn rename_refuses_an_unbuildable_project_with_no_baseline_configured() {
    let tmp = scratch();
    let project = tmp.path();
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: dup\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: dup\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );

    let output = nodex(project)
        .args(["rename", "docs/a.md", "docs/moved.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("DUPLICATE_ID"),
        "the move is refused for the same reason `build` is: {envelope}"
    );
    assert!(
        project.join("docs/a.md").exists() && !project.join("docs/moved.md").exists(),
        "and nothing moved"
    );
}

#[test]
fn rename_refuses_a_project_that_cannot_build_before_it_moves_anything() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n\
         # A\n\nsee [b](b.md)\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a project that builds"]);

    // The duplicate exists only in the working tree, so the baseline still
    // builds and only the overlay build would fail.
    write_doc(
        project,
        "docs/dup.md",
        "---\nid: generic-b\ntitle: Dup\nkind: generic\nstatus: active\n---\n# Dup\n",
    );

    let output = nodex(project)
        .args(["rename", "docs/b.md", "docs/moved.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("DUPLICATE_ID"),
        "the refusal names the project's own problem: {envelope}"
    );
    assert!(
        !project.join("docs/moved.md").exists(),
        "nothing moved, so the reference is still correct and a re-run can succeed"
    );
    assert!(
        fs::read_to_string(project.join("docs/a.md"))
            .unwrap()
            .contains("(b.md)"),
        "the reference was left intact rather than stranded"
    );
}

#[test]
fn rename_of_a_terminal_parent_is_not_vetoed_by_its_own_pre_move_presence() {
    // The destination probe models the POST-move world: the source path
    // is overlaid empty, so the still-on-disk source cannot act as its
    // own terminal conditional-exclude parent and veto a move the build
    // would admit once the source is gone.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"specs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"specs/*/spec.md\"\n\
         child_glob = \"specs/*/*.md\"\n\
         [statuses]\nallowed = [\"active\", \"done\"]\nterminal = [\"done\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "specs/auth/spec.md",
        "---\nid: spec-auth\ntitle: Auth\nkind: generic\nstatus: done\n---\n# Auth\n",
    );
    nodex(root).arg("build").assert().success();

    // Renaming the terminal parent itself: post-move no terminal parent
    // remains in the directory, so the destination IS graphed.
    nodex(root)
        .args([
            "rename",
            "specs/auth/spec.md",
            "specs/auth/spec-archived.md",
        ])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    let data =
        run_json(nodex(root).args(["query", "node", "--path", "specs/auth/spec-archived.md"]));
    assert_eq!(
        data.pointer("/node/id").and_then(Value::as_str),
        Some("spec-auth"),
        "moved doc is graphed: {data}"
    );

    // Inverse protection still holds: moving a doc INTO a directory
    // governed by a different, still-present terminal parent is refused
    // — post-move it would be conditionally excluded — and the message
    // names the actual cause.
    write_doc(
        root,
        "specs/billing/spec.md",
        "---\nid: spec-billing\ntitle: Billing\nkind: generic\nstatus: done\n---\n# B\n",
    );
    write_doc(
        root,
        "specs/other/notes.md",
        "---\nid: generic-notes\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    nodex(root).arg("build").assert().success();
    let output = nodex(root)
        .args(["rename", "specs/other/notes.md", "specs/billing/notes.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("conditional_exclude"),
        "names the actual cause: {envelope}"
    );
    assert!(
        root.join("specs/other/notes.md").exists(),
        "source untouched"
    );
}

#[test]
fn since_gate_survives_a_config_format_migration() {
    // Single-lens semantics: the working tree's config is the one lens;
    // a ref supplies content only. The PR that migrates the config
    // format itself must still pass `check --since` and `diff` — under
    // per-ref configs it deadlocks, because the base ref's config no
    // longer parses under the new binary.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);

    // The base commit carries a config shape the current binary REJECTS
    // (a conditional_exclude without the now-mandatory child_glob).
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"docs/specs/*/spec.md\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "old config shape"]);

    // The migration: working tree moves to the current shape.
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"docs/specs/*/spec.md\"\n\
         child_glob = \"docs/specs/*/*.md\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    // check --since the base ref: must run (content under today's lens),
    // not die parsing the base ref's config.
    let data = run_json(nodex(root).args(["check", "--since", "HEAD"]));
    assert_eq!(data.pointer("/total").and_then(Value::as_u64), Some(0));

    // The migration committed: diff/impact across the boundary work too.
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "migrate config shape"]);
    nodex(root)
        .args(["diff", "HEAD~1", "HEAD"])
        .assert()
        .success();
    nodex(root)
        .args(["impact", "HEAD~1", "HEAD"])
        .assert()
        .success();
}

#[test]
fn migrate_does_not_double_inject_a_bom_crlf_document() {
    // A BOM+CRLF file authored outside nodex already has frontmatter; the
    // build parser canonicalizes before splitting, and migrate must too —
    // otherwise it misreads the file as bare and injects a duplicate
    // frontmatter block.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    fs::create_dir_all(root.join("docs")).unwrap();
    let original = "\u{FEFF}---\r\nid: doc-crlf\r\ntitle: CRLF\r\nkind: generic\r\nstatus: active\r\n---\r\n# Body\r\n";
    fs::write(root.join("docs/crlf.md"), original).unwrap();

    let env = run_envelope(nodex(root).args(["migrate", "--apply"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    // The doc already had frontmatter → nothing migrated.
    assert_eq!(
        env.pointer("/data/total").and_then(Value::as_u64),
        Some(0),
        "a doc with frontmatter must not be migrated: {env}"
    );
    // A skipped doc is left byte-identical — no duplicate frontmatter
    // block injected on top of the (BOM+CRLF) original.
    assert_eq!(
        fs::read_to_string(root.join("docs/crlf.md")).unwrap(),
        original,
        "skipped doc must be left untouched"
    );

    nodex(root).arg("build").assert().success();
}

#[test]
fn migrate_skips_unclosed_fence_file_with_a_warning() {
    // A file that opens a frontmatter fence and never closes it is
    // neither bare nor parseable — injecting frontmatter would bury the
    // malformed block in the body. Migrate skips it with a per-file
    // warning and leaves the bytes untouched; the file itself reds
    // `check` via the `parse_failure` rule.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    fs::create_dir_all(root.join("docs")).unwrap();
    let original = "---\nid: half-open\n# never closed\n";
    fs::write(root.join("docs/half.md"), original).unwrap();
    write_doc(root, "docs/bare.md", "# Bare Doc\nBody.\n");

    let env = run_envelope(nodex(root).args(["migrate", "--apply"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total").and_then(Value::as_u64),
        Some(1),
        "only the genuinely bare doc is migrated: {env}"
    );
    let warnings: Vec<&str> = env
        .get("warnings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(warning_msg).collect())
        .unwrap_or_default();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("docs/half.md") && w.contains("unclosed frontmatter fence")),
        "the skip names the file and the cause: {warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/half.md")).unwrap(),
        original,
        "no frontmatter is injected into the malformed file"
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
fn every_command_real_output_conforms_to_its_per_command_schema() {
    // The bijection test (`every_cli_leaf_has_a_per_command_schema`)
    // proves each leaf HAS a schema; this proves each schema MATCHES the
    // bytes the command actually emits. That conformance gap is exactly
    // what let `export.enums` publish `per_kind` as required while serde
    // omitted it — a typed client codegen'd from the schema would reject
    // valid output. Driving every command's REAL output through its own
    // schema closes the whole drift class, present and future.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"draft\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"draft\"\n\
         [detection]\nstale_days = 30\n\
         [[annotations]]\nname = \"todo\"\npattern = '\\[\\[TODO: (?P<task>[^\\]]+)\\]\\]'\nkey = \"task\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/spec.md",
        "---\nid: spec\ntitle: Spec\nkind: generic\nstatus: active\ncovers: [\"src/x.rs\"]\n---\n# Spec\n",
    );
    write_doc(
        root,
        "docs/impl.md",
        "---\nid: impl\ntitle: Impl\nkind: generic\nstatus: active\nimplements: spec\nreviewed: 2020-01-01\n---\n# Impl\n[spec](spec.md) [[TODO: finish]]\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: new\n---\n# Old\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: new\ntitle: New\nkind: generic\nstatus: active\nsupersedes: old\n---\n# New\n",
    );
    write_doc(
        root,
        "docs/orphan.md",
        "---\nid: orphan\ntitle: Orphan\nkind: generic\nstatus: draft\n---\n# Orphan\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // A second commit so diff/impact have a non-empty HEAD~1..HEAD delta.
    write_doc(
        root,
        "docs/new.md",
        "---\nid: new\ntitle: New v2\nkind: generic\nstatus: active\nsupersedes: old\n---\n# New\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "v2"]);
    nodex(root).arg("build").assert().success();

    let schemas = run_envelope(nodex(root).args(["export", "envelope-schema"]));
    let per_command = schemas
        .pointer("/data/per_command")
        .and_then(Value::as_object)
        .expect("per_command object")
        .clone();

    // Every key validated below is recorded, then asserted equal to the
    // full per_command key set — so a future leaf that ships a schema but
    // is never driven through it (the original gap with status /
    // export.config / export.commands) is a hard failure, not a silent
    // skip.
    let validated = std::cell::RefCell::new(std::collections::BTreeSet::<String>::new());
    let validate = |key: &str, data: &Value| {
        validated.borrow_mut().insert(key.to_string());
        let schema = per_command
            .get(key)
            .unwrap_or_else(|| panic!("no per_command schema for {key}"));
        let validator =
            jsonschema::draft202012::new(schema).unwrap_or_else(|e| panic!("{key} schema: {e}"));
        assert!(
            validator.is_valid(data),
            "{key}: real output rejected by its own schema: {:?}\ndata: {data}",
            validator
                .iter_errors(data)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
    };

    // Read-only + dry-run commands against the stable fixture.
    let read_cases: Vec<(&str, Vec<&str>)> = vec![
        ("build", vec!["build"]),
        ("check", vec!["check"]),
        ("diff", vec!["diff", "HEAD~1", "HEAD"]),
        ("impact", vec!["impact", "HEAD~1", "HEAD"]),
        ("report", vec!["report", "--format", "json"]),
        ("migrate", vec!["migrate"]),
        (
            "scaffold",
            vec![
                "scaffold",
                "--kind",
                "generic",
                "--title",
                "New Doc",
                "--dry-run",
                "--path",
                "docs/newdoc.md",
            ],
        ),
        ("export.schema", vec!["export", "schema"]),
        ("export.enums", vec!["export", "enums"]),
        ("export.rules", vec!["export", "rules"]),
        ("export.envelope-schema", vec!["export", "envelope-schema"]),
        ("export.config", vec!["export", "config"]),
        ("export.commands", vec!["export", "commands"]),
        ("export.diagnostics", vec!["export", "diagnostics"]),
        ("status", vec!["status"]),
        ("query.search", vec!["query", "search", "spec"]),
        ("query.backlinks", vec!["query", "backlinks", "spec"]),
        ("query.chain", vec!["query", "chain", "old"]),
        ("query.node", vec!["query", "node", "spec"]),
        ("query.nodes", vec!["query", "nodes"]),
        ("query.orphans", vec!["query", "orphans"]),
        ("query.stale", vec!["query", "stale"]),
        ("query.components", vec!["query", "components"]),
        ("query.recent", vec!["query", "recent"]),
        ("query.annotations", vec!["query", "annotations"]),
        ("query.issues", vec!["query", "issues"]),
        ("query.covered-by", vec!["query", "covered-by", "src/x.rs"]),
        (
            "query.neighborhood",
            vec!["query", "neighborhood", "spec", "--depth", "1"],
        ),
        ("query.dependents", vec!["query", "dependents", "spec"]),
        ("query.similar", vec!["query", "similar", "--id", "spec"]),
        ("query.trust", vec!["query", "trust", "spec"]),
        ("query.trust-list", vec!["query", "trust", "--top", "3"]),
    ];
    for (key, args) in &read_cases {
        let env = run_envelope(nodex(root).args(args));
        validate(key, env.get("data").expect("data"));
    }

    // Mutating commands, each validated on its own result envelope. Run
    // after the reads; order so each succeeds against the prior state.
    let lc_set =
        run_envelope(nodex(root).args(["lifecycle", "set", "--status", "active", "orphan"]));
    validate("lifecycle.set", lc_set.get("data").unwrap());
    let lc_review = run_envelope(nodex(root).args(["lifecycle", "review", "new"]));
    validate("lifecycle.review", lc_review.get("data").unwrap());
    let lc_sup =
        run_envelope(nodex(root).args(["lifecycle", "supersede", "--to", "new", "orphan"]));
    validate("lifecycle.supersede", lc_sup.get("data").unwrap());
    let ren = run_envelope(nodex(root).args(["rename", "docs/impl.md", "docs/impl2.md"]));
    validate("rename", ren.get("data").unwrap());
    let ret = run_envelope(nodex(root).args(["retarget", "spec", "new"]));
    validate("retarget", ret.get("data").unwrap());

    // init writes nodex.toml, so run it in a fresh empty directory.
    let tmp2 = scratch();
    let init = run_envelope(nodex(tmp2.path()).arg("init"));
    validate("init", init.get("data").unwrap());

    // Bijection: every registered per_command schema has a real-output
    // conformance case here, and vice versa. A new leaf that ships a
    // schema but forgets a case (or a stale case for a removed schema)
    // fails here instead of silently escaping conformance.
    let expected: std::collections::BTreeSet<String> = per_command.keys().cloned().collect();
    assert_eq!(
        *validated.borrow(),
        expected,
        "every per_command schema must be exercised by a real-output conformance case"
    );
}

#[test]
fn report_markdown_sanitizes_newlines_in_every_field_and_section() {
    // A hand-authored double-quoted scalar carrying a literal newline +
    // `## heading` must not inject structure into GRAPH.md from ANY field
    // (id/title/status/kind) in ANY section (Summary tally, Orphans,
    // Stale, God-Nodes, Chains). The renderer runs every interpolated
    // value through `inline`; this asserts the WHOLE surface, so a future
    // unsanitized field is caught here rather than in a generated report.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[detection]\nstale_days = 1\n",
    )
    .unwrap();
    // One orphan+stale doc with a newline in id, title, status, AND kind —
    // it lands in the Summary status/kind tallies, Orphans, and Stale.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: \"evil\\n## INJ_ID\"\ntitle: \"T\\n## INJ_TITLE\"\nstatus: \"active\\n## INJ_STATUS\"\nkind: \"k\\n## INJ_KIND\"\nreviewed: 2020-01-01\n---\n# Evil\n",
    );
    nodex(root).arg("build").assert().success();
    // `report --format md` WRITES GRAPH.md to `output_dir`; the envelope
    // only carries `{generated, output_dir}`. Read the actual artifact —
    // checking the envelope would be vacuous.
    let env = run_envelope(nodex(root).args(["report", "--format", "md"]));
    let out_dir = env
        .pointer("/data/output_dir")
        .and_then(Value::as_str)
        .expect("output_dir");
    let md = fs::read_to_string(std::path::Path::new(out_dir).join("GRAPH.md")).expect("GRAPH.md");
    // A real injected heading starts at column 0 (`\n## INJ_...`). The
    // sanitized form collapses the newline to a space (`... ## INJ_...`),
    // which is inert inline text.
    for marker in ["## INJ_ID", "## INJ_TITLE", "## INJ_STATUS", "## INJ_KIND"] {
        assert!(
            !md.contains(&format!("\n{marker}")),
            "{marker} injected a column-0 heading — a field reached the report unsanitized:\n{md}"
        );
    }
    // Sanity: the markers ARE present (as inert inline text), proving the
    // fields reached the report and the assertion isn't vacuously true.
    assert!(
        md.contains("INJ_STATUS"),
        "status must appear in the tally: {md}"
    );
    assert!(md.contains("INJ_ID"), "id must appear in a section: {md}");
}

#[test]
fn report_god_nodes_exclude_self_loops_matching_query_backlinks() {
    // The God-Nodes "backlinks" count must be the same external-attention
    // measure `query backlinks` and orphan detection use — self-loops
    // excluded. Otherwise a self-referencing node is simultaneously an
    // orphan (per `query orphans`) and a "god node with 1 backlink", and
    // a node can inflate its own rank with self-references.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    // selfonly: only a self-loop → 0 external backlinks (an orphan).
    write_doc(
        root,
        "docs/selfonly.md",
        "---\nid: selfonly\ntitle: Self\nstatus: active\nrelated: [selfonly]\n---\n# Self\n",
    );
    // hub: two real external backlinks.
    write_doc(
        root,
        "docs/hub.md",
        "---\nid: hub\ntitle: Hub\nstatus: active\n---\n# Hub\n",
    );
    write_doc(
        root,
        "docs/x.md",
        "---\nid: x\ntitle: X\nstatus: active\nrelated: [hub]\n---\n# X\n",
    );
    write_doc(
        root,
        "docs/y.md",
        "---\nid: y\ntitle: Y\nstatus: active\nrelated: [hub]\n---\n# Y\n",
    );
    nodex(root).arg("build").assert().success();

    // Consistency anchor: query backlinks agrees selfonly has zero.
    let bl = run_envelope(nodex(root).args(["query", "backlinks", "selfonly"]));
    assert_eq!(bl.pointer("/data/total").and_then(Value::as_i64), Some(0));

    let env = run_envelope(nodex(root).args(["report", "--format", "md"]));
    let out_dir = env
        .pointer("/data/output_dir")
        .and_then(Value::as_str)
        .expect("output_dir");
    let md = fs::read_to_string(std::path::Path::new(out_dir).join("GRAPH.md")).expect("GRAPH.md");
    let god = md
        .split("## God Nodes")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("God Nodes section");
    assert!(
        !god.contains("selfonly"),
        "a self-loop-only node must not appear as a god node:\n{god}"
    );
    assert!(
        god.contains("**hub** (2 backlinks)"),
        "hub's real backlinks count: {god}"
    );
}

#[test]
fn report_supersession_chains_render_every_branch_once() {
    // The GRAPH.md "Supersession Chains" section must render the WHOLE
    // lineage of a consolidation (`x supersedes [a, b]`) on one line, with
    // no branch omitted and no component duplicated — the report consumes
    // the same component-wide `find_chain` as `query chain`.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\nstatus: superseded\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\nstatus: superseded\n---\n# B\n",
    );
    write_doc(
        root,
        "docs/x.md",
        "---\nid: x\nstatus: active\nsupersedes: [a, b]\n---\n# X\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["report", "--format", "md"]));
    let out_dir = env
        .pointer("/data/output_dir")
        .and_then(Value::as_str)
        .expect("output_dir");
    let md = fs::read_to_string(std::path::Path::new(out_dir).join("GRAPH.md")).expect("GRAPH.md");
    let section = md
        .split("## Supersession Chains")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("Supersession Chains section");
    let bullets: Vec<&str> = section.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(bullets.len(), 1, "exactly one component bullet: {section}");
    for id in ["a", "b", "x"] {
        assert!(bullets[0].contains(id), "member {id} missing: {section}");
    }
}

#[test]
fn duplicate_id_error_attribution_is_cache_state_independent() {
    // The DUPLICATE_ID error names two colliding files; which is reported
    // `first` must NOT depend on whether one was served from the cache.
    // `all_nodes` is `[cached…] ++ [fresh…]`; the canonical sort before
    // the duplicate check makes the warm build (one cached) and the
    // `--full` rebuild (both fresh) agree on path order.
    let tmp = scratch();
    init_project(tmp.path());
    // `zzz.md` (id `collide`) builds first → cached. `mmm.md` sorts before
    // it and also claims `collide`; the cached file must sort AFTER the
    // fresh one to expose any insertion-order leak.
    write_doc(
        tmp.path(),
        "docs/zzz.md",
        "---\nid: collide\ntitle: Z\nkind: generic\nstatus: active\n---\nbody\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    write_doc(
        tmp.path(),
        "docs/mmm.md",
        "---\nid: collide\ntitle: M\nkind: generic\nstatus: active\n---\nbody\n",
    );

    let warm = nodex(tmp.path()).arg("build").output().expect("ran");
    let full = nodex(tmp.path())
        .args(["build", "--full"])
        .output()
        .expect("ran");
    let warm_msg = String::from_utf8_lossy(&warm.stdout).to_string();
    let full_msg = String::from_utf8_lossy(&full.stdout).to_string();
    // Both must report the path-lesser file (mmm.md) as `first`.
    let first_is_mmm = |m: &str| m.find("mmm.md").unwrap() < m.find("zzz.md").unwrap();
    assert!(first_is_mmm(&warm_msg), "warm build: {warm_msg}");
    assert!(first_is_mmm(&full_msg), "full rebuild: {full_msg}");
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
    // `nodex lifecycle set <id> --status` on a symlinked doc cannot reach
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
        .args(["lifecycle", "set", "ext", "--status", "archived"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "must reject symlink mutation");
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("SYMLINK_TARGET")
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
         [[scope.conditional_exclude]]\nparent_glob = \"docs/feat/SPEC.md\"\nchild_glob = \"**/*\"\ncondition = \"status_terminal\"\n\
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
#[cfg(unix)]
fn retarget_skips_file_under_symlinked_directory_with_warning() {
    // With `scope.follow_symlinks` on, a file outside the root can be a graph
    // node. Retargeting must give it the reader-follows / writer-skips
    // treatment: complete the batch (exit 0), rewrite the real in-root
    // referrer, warn about the symlinked one, and never touch the external
    // target.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    let outside = scratch();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        root,
        "new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: doc-ref\ntitle: Ref\nkind: generic\nstatus: active\nrelated: doc-old\n---\n# Ref\n",
    );
    let external = "---\nid: doc-ext\ntitle: Ext\nkind: generic\nstatus: active\nrelated: doc-old\n---\n# Ext\n";
    fs::write(outside.path().join("ext.md"), external).unwrap();
    unix_fs::symlink(outside.path(), root.join("linked")).unwrap();
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["retarget", "doc-old", "doc-new"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let updated: Vec<&str> = env
        .pointer("/data/references_updated")
        .and_then(Value::as_array)
        .expect("references_updated")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        updated.contains(&"ref.md"),
        "in-root referrer rewritten: {updated:?}"
    );
    assert!(
        !updated.iter().any(|p| p.contains("linked")),
        "symlinked referrer must not be rewritten: {updated:?}"
    );
    let warnings = env
        .get("warnings")
        .and_then(Value::as_array)
        .expect("skip warning present");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("linked/ext.md") && w.contains("symlink")),
        "warning names the skipped path: {warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("ext.md")).unwrap(),
        external,
        "external target byte-identical"
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
    let env = run_envelope(nodex(tmp.path()).args(["retarget", "old", "succ"]));
    assert!(
        env.get("warnings").is_none(),
        "a reference nothing asked to repoint is not a refusal: {env:?}"
    );
}

#[test]
fn retarget_says_which_references_it_could_not_repoint() {
    // A retarget moves no file, so a reference it could not respell goes
    // on naming exactly what it named and the project it leaves is in
    // order — every reader downstream sees a graph with nothing to say.
    // The command asked for something and got nothing, and it is the only
    // place that can be said.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [parser]\nwikilink_enabled = true\n",
    )
    .unwrap();
    // `[[succ]]` read from docs/ binds the file docs/succ.md, whose
    // document is somebody else — so no spelling of the successor id
    // reads back as the successor, and the repoint is given up.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\nsee [[target]]\n",
    );
    write_doc(
        root,
        "docs/t.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\nt\n",
    );
    write_doc(
        root,
        "docs/succ.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "other/s.md",
        "---\nid: succ\ntitle: U\nkind: generic\nstatus: active\n---\nu\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["retarget", "target", "succ"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(0)
    );
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("docs/a.md") && w.contains("still names") && w.contains("target")),
        "the retarget names the reference it left: {warnings:?}"
    );
    // Nothing else reports it: the project the retarget leaves is valid.
    nodex(root).arg("check").assert().success();
}

#[test]
fn retarget_rewrites_a_list_interrupted_by_a_column0_comment() {
    // A column-0 comment between `related` items is interior trivia of
    // the block: the rewrite replaces the block whole, so a file the
    // envelope reports as references_updated carries zero stale
    // predecessor ids — never a stale `- old` line re-attached to the
    // new list behind the comment.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "keep.md",
        "---\nid: keep-adr\ntitle: K\nkind: generic\nstatus: active\n---\n# K\n",
    );
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: stale-adr\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    write_doc(
        tmp.path(),
        "new.md",
        "---\nid: fresh-adr\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\nrelated:\n  - keep-adr\n# stale-adr pending replacement\n  - stale-adr\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["retarget", "stale-adr", "fresh-adr"]));
    assert!(
        data["references_updated"]
            .as_array()
            .map(|a| a.iter().any(|p| p == "b.md"))
            .unwrap_or(false),
        "the comment-interrupted relation list must be rewritten: {data}"
    );
    let content = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(
        !content.contains("stale-adr"),
        "zero stale predecessor ids may remain, including behind the comment: {content}"
    );
    assert!(
        content.contains("fresh-adr"),
        "successor id written: {content}"
    );
    nodex(tmp.path()).arg("check").assert().success();
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
fn link_pattern_naming_covers_is_rejected_at_load() {
    // `covers` is the built-in path-only coverage relation, fed
    // exclusively by the frontmatter `covers:` field. A body link
    // pattern naming it would silently attach path-only resolution to
    // a user-chosen relation name, so the config is refused before any
    // command runs — with the remediation naming the frontmatter field.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = '@covers (\\S+)'\nrelation = \"covers\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("JSON");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(false)));
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    let msg = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("parser.link_patterns[0]"),
        "error names the offending block: {msg}"
    );
    assert!(
        msg.contains("frontmatter covers: field"),
        "remediation names the frontmatter field: {msg}"
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
        data.pointer("/id_stability/type").and_then(Value::as_str),
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
         [[scope.conditional_exclude]]\nparent_glob = \"work/SPEC.md\"\nchild_glob = \"**/*\"\ncondition = \"status_terminal\"\n\
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
            .any(|w| warning_msg(w).is_some_and(|s| s.contains("linked.md"))),
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
                .get("message")
                .and_then(Value::as_str)
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
fn rename_skips_a_broken_referencing_file_without_stranding_the_batch() {
    // One referencing file whose frontmatter fence does not parse is a
    // per-file skip: the move lands, every other reference is
    // rewritten, the command exits 0 with a warning naming the broken
    // file. The file already reds `check` as a parse_failure, and its
    // stale reference surfaces as an unresolved edge — a batch abort
    // here would strand a half-applied rename (the file is already
    // moved when references are rewritten).
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/target.md",
        "---\nid: target\ntitle: Target\nkind: generic\nstatus: active\n---\n# Target\n",
    );
    write_doc(
        tmp.path(),
        "docs/healthy.md",
        "---\nid: healthy\ntitle: Healthy\nkind: generic\nstatus: active\n---\n\
         # Healthy\n\nSee [target](docs/target.md).\n",
    );
    // Opened fence, never closed — unsplittable.
    write_doc(
        tmp.path(),
        "docs/broken.md",
        "---\nid: broken\ntitle: Broken\nSee [target](docs/target.md).\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let envelope =
        run_envelope(nodex(tmp.path()).args(["rename", "docs/target.md", "docs/moved.md"]));
    assert!(
        !tmp.path().join("docs/target.md").exists() && tmp.path().join("docs/moved.md").exists(),
        "the move itself lands"
    );
    let healthy = fs::read_to_string(tmp.path().join("docs/healthy.md")).unwrap();
    assert!(
        healthy.contains("docs/moved.md"),
        "the parseable referencing file is rewritten: {healthy}"
    );
    let broken = fs::read_to_string(tmp.path().join("docs/broken.md")).unwrap();
    assert!(
        broken.contains("docs/target.md"),
        "the broken file is left untouched: {broken}"
    );
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("skip warning present");
    assert!(
        warnings.iter().any(|w| w
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("docs/broken.md") && s.contains("parse_failure"))),
        "the warning names the skipped file: {warnings:?}"
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
        data.pointer("/id_stability/type").and_then(Value::as_str),
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
        data.pointer("/id_stability/type").and_then(Value::as_str),
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
        data.pointer("/id_stability/type").and_then(Value::as_str),
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
            .pointer("/data/id_stability/type")
            .and_then(Value::as_str),
        Some("bare_no_frontmatter")
    );
    let warnings: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("envelope-level warnings array")
        .iter()
        .filter_map(warning_msg)
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("inferred id changed")),
        "bare-file rename must surface a warning, not silently drift: {warnings:?}"
    );
}

#[test]
fn malformed_frontmatter_yaml_surfaces_on_build_result() {
    // Malformed YAML in a single document does NOT halt the build.
    // The file is dropped from the graph (no node) and the drop is
    // structural data: `data.parse_failures` names the failing path —
    // one bad file never blocks the operator from inspecting the rest
    // of the project, and never hides as a warning a gate ignores
    // (`check` reds the same record via the `parse_failure` rule).
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
    // doc is recorded on the result.
    assert_eq!(
        envelope.pointer("/data/nodes").and_then(Value::as_u64),
        Some(1),
        "only the well-formed doc must appear in the graph: {envelope}"
    );
    let failures = envelope
        .pointer("/data/parse_failures")
        .and_then(Value::as_array)
        .expect("parse_failures array on the build result");
    assert_eq!(failures.len(), 1, "one drop recorded: {envelope}");
    assert_eq!(
        failures[0].get("path").and_then(Value::as_str),
        Some("docs/broken.md"),
        "the record names the failing file: {envelope}"
    );
    assert!(
        failures[0]
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains("docs/broken.md")),
        "the message carries the full error chain: {envelope}"
    );
    assert!(
        failures[0]
            .get("content_hash")
            .and_then(Value::as_str)
            .is_some_and(|h| h.len() == 64),
        "the record carries the content digest: {envelope}"
    );
}

#[test]
fn non_utf8_doc_is_a_parse_failure_through_build_check_and_status() {
    // An in-scope file the build cannot read as text takes the same
    // path as a YAML failure end to end: recorded on the build result
    // (exit 0), an Error-severity `parse_failure` in `check` (exit 1),
    // and covered-but-unbuildable for `status` — never `added_paths`,
    // never a staleness a rebuild could not clear.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/good.md",
        "---\nid: doc-good\ntitle: Good\nkind: generic\nstatus: active\n---\n# Good\n",
    );
    let raw_path = tmp.path().join("docs/raw.md");
    fs::write(&raw_path, [0xFF, 0xFE, 0x01, 0x02]).unwrap();

    let envelope = run_envelope(nodex(tmp.path()).arg("build"));
    assert_eq!(
        envelope.pointer("/data/nodes").and_then(Value::as_u64),
        Some(1),
        "only the readable doc enters the graph: {envelope}"
    );
    let failures = envelope
        .pointer("/data/parse_failures")
        .and_then(Value::as_array)
        .expect("parse_failures array on the build result");
    assert_eq!(
        failures[0].get("path").and_then(Value::as_str),
        Some("docs/raw.md"),
        "the record names the unreadable file: {envelope}"
    );

    let out = nodex(tmp.path()).arg("check").assert().failure().code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| {
                v.get("rule_id").and_then(Value::as_str) == Some("parse_failure")
                    && v.get("path").and_then(Value::as_str) == Some("docs/raw.md")
            }),
        "a non-UTF-8 in-scope file must red check via parse_failure: {env}"
    );

    let data = run_json(nodex(tmp.path()).arg("status"));
    assert_eq!(
        data["state"], "current",
        "a faithfully-built snapshot is current — the breakage signal belongs to check: {data}"
    );
    assert_eq!(
        data.pointer("/unbuildable_paths/0").and_then(Value::as_str),
        Some("docs/raw.md"),
        "status surfaces the path as unbuildable, not as added_paths: {data}"
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

/// Fixture for the absent-composite contract: a `guide`-kind doc under
/// backlinks-only override weights in a graph with zero external
/// incoming edges anywhere — its single positively-weighted component
/// is absent, so no composite exists. The two `generic` siblings keep
/// the default weights (status 0.4 always present) and stay scored.
fn init_backlinks_only_trust_project(root: &std::path::Path) {
    init_project(root);
    let path = root.join("nodex.toml");
    let mut content = fs::read_to_string(&path).expect("nodex.toml");
    content.push_str(
        "\n[[trust.overrides]]\nkinds = [\"guide\"]\n\
         weights = { status = 0.0, freshness = 0.0, drift = 0.0, backlinks = 1.0 }\n",
    );
    fs::write(&path, content).expect("nodex.toml writable");
    write_doc(
        root,
        "docs/no-signal.md",
        "---\nid: doc-no-signal\ntitle: No Signal\nkind: guide\nstatus: active\n---\n# No Signal\n",
    );
    write_doc(
        root,
        "docs/dead.md",
        "---\nid: doc-dead\ntitle: Dead\nkind: generic\nstatus: archived\n---\n# Dead\n",
    );
    write_doc(
        root,
        "docs/live.md",
        "---\nid: doc-live\ntitle: Live\nkind: generic\nstatus: active\n---\n# Live\n",
    );
    nodex(root).arg("build").assert().success();
}

#[test]
fn query_trust_single_node_omits_score_key_when_no_signal() {
    // A node with no positively-weighted present component has no
    // composite: the single-node form still succeeds (exit 0) and
    // returns the components, but the `score` key is absent from the
    // wire — the same honest-absence convention the components follow,
    // never `null` or a fabricated `0.0`.
    let tmp = scratch();
    init_backlinks_only_trust_project(tmp.path());

    let data = run_json(nodex(tmp.path()).args(["query", "trust", "doc-no-signal"]));
    let obj = data.as_object().expect("trust entry object");
    assert!(
        !obj.contains_key("score"),
        "score must be omitted when no signal is present; got {data}"
    );
    assert!(
        obj.contains_key("components"),
        "components stay present so the absence is inspectable; got {data}"
    );
}

#[test]
fn query_trust_ranking_excludes_unscored_node_and_warns() {
    // An unrankable node is not in the ranking's domain: excluded from
    // `items` and `total` (it can never occupy a bottom-N slot or
    // satisfy `--below`), and the exclusion is announced through the
    // envelope warnings — never silent.
    let tmp = scratch();
    init_backlinks_only_trust_project(tmp.path());

    let env = run_envelope(nodex(tmp.path()).args(["query", "trust", "--bottom", "5"]));
    let items = env
        .pointer("/data/items")
        .and_then(Value::as_array)
        .expect("items array");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ids,
        vec!["doc-dead", "doc-live"],
        "scored entries rank ascending; the unscored node never occupies a slot"
    );
    assert_eq!(
        env.pointer("/data/total").and_then(Value::as_u64),
        Some(2),
        "total counts the ranking's domain only"
    );
    let warnings = env
        .get("warnings")
        .and_then(Value::as_array)
        .expect("exclusion must surface as an envelope warning");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("1 node(s) excluded from the ranking")),
        "warning names the excluded count: {warnings:?}"
    );
}

#[test]
fn export_trust_schemas_mark_score_optional_with_gate_descriptions() {
    // The typed-codegen contract for the scoring leaves: `score` is
    // present in properties but outside the required set (absence is
    // single-node-form-only — ranking entries always carry it), and
    // the TrustComponents descriptions enumerate every verified gate
    // so typed clients read the complete absence conditions.
    let tmp = scratch();
    let data = run_json(nodex(tmp.path()).args(["export", "envelope-schema"]));
    let per_command = data["per_command"].as_object().expect("per_command object");

    let trust = &per_command["query.trust"];
    let required: Vec<&str> = trust["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        !required.contains(&"score"),
        "score must not be required on query.trust: {required:?}"
    );
    assert!(required.contains(&"components"));
    assert!(
        trust.pointer("/properties/score").is_some(),
        "score stays a declared property"
    );

    let entry_required: Vec<&str> = per_command["query.trust-list"]
        .pointer("/$defs/TrustEntry/required")
        .and_then(Value::as_array)
        .expect("TrustEntry required array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        !entry_required.contains(&"score"),
        "score must not be required on query.trust-list entries: {entry_required:?}"
    );

    let components = trust
        .pointer("/$defs/TrustComponents/properties")
        .expect("TrustComponents def");
    let freshness = components
        .pointer("/freshness/description")
        .and_then(Value::as_str)
        .expect("freshness description");
    assert!(
        freshness.contains("stale_days") && freshness.contains("reviewed"),
        "freshness description must name both gates: {freshness}"
    );
    let drift = components
        .pointer("/drift/description")
        .and_then(Value::as_str)
        .expect("drift description");
    assert!(
        drift.contains("git_drift_threshold")
            && drift.contains("reviewed")
            && drift.contains("cannot measure"),
        "drift description must name all three gates: {drift}"
    );
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
    // The composite field is `score`, identical in spine to trust/search —
    // not the old `similarity` name.
    let top = &items[0];
    assert!(
        top.get("score").and_then(Value::as_f64).is_some(),
        "similar entries carry a `score` composite: {top}"
    );
    assert!(
        top.get("similarity").is_none(),
        "the composite is named `score`, not `similarity`: {top}"
    );
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
        .filter_map(warning_msg)
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
            .any(|w| warning_msg(w).is_some_and(|s| s.contains("meta.nodex_version"))),
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
                .any(|x| warning_msg(x).is_some_and(|s| s.contains("meta.nodex_version")))),
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
fn export_schema_constrains_require_explicit_fields_non_empty() {
    // `require_explicit` forces a built-in to be authored; `check`'s
    // `explicit_field` reds an empty value, so the exported JSON Schema
    // must reject empty too (`minLength: 1`) or a codegen consumer would
    // accept `title: ""` that `check` refuses. The two must agree.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\"]\nterminal = []\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{stem}\"\n\
         [schema]\nrequire_explicit = [\"title\"]\n",
    )
    .unwrap();
    let schema = run_json(nodex(tmp.path()).args(["export", "schema"]));
    // Find the `title` property's minLength wherever the branch sits
    // (single-object schema, or a `oneOf` branch).
    fn title_min_length(v: &Value) -> Option<u64> {
        if let Some(props) = v.get("properties").and_then(Value::as_object)
            && let Some(min) = props
                .get("title")
                .and_then(|t| t.get("minLength"))
                .and_then(Value::as_u64)
        {
            return Some(min);
        }
        for (_k, child) in v.as_object().into_iter().flatten() {
            if let Some(m) = title_min_length(child) {
                return Some(m);
            }
        }
        for child in v.as_array().into_iter().flatten() {
            if let Some(m) = title_min_length(child) {
                return Some(m);
            }
        }
        None
    }
    assert_eq!(
        title_min_length(&schema),
        Some(1),
        "require_explicit title must carry minLength:1 in the exported schema: {schema}"
    );
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
fn diff_with_bad_before_ref_leaves_no_scratch_dir() {
    // When the FIRST `git worktree add` fails (the `before` ref is bad),
    // the scratch directory created beforehand must still be removed —
    // the RAII guard never owns it on that path, so `add` cleans it up.
    // Otherwise the repo root accumulates an empty `.nodex-diff-<pid>`
    // per failed run, contradicting the worktree module's own invariant.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);

    let output = nodex(root)
        .args(["diff", "no-such-ref", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("GIT_ERROR")
    );

    let leaked: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".nodex-diff"))
        })
        .collect();
    assert!(leaked.is_empty(), "scratch dir leaked: {leaked:?}");
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

    let git = git_runner(root);
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

    let git = git_runner(root);
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

    let git = git_runner(root);
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

    let git = git_runner(root);
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

// ─── check --content (proposed-content / write-time validation) ──────

#[test]
fn check_content_blocks_terminal_body_edit_and_allows_identical() {
    // The agent's write moment: an already-terminal doc whose body the
    // proposed bytes would change must be blocked before the write, and
    // proposing the identical bytes must pass — all through nodex's own
    // rule engine, so no consumer reimplements immutability.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    let on_disk =
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\noriginal\n";
    write_doc(root, "docs/d.md", on_disk);
    nodex(root).arg("build").assert().success();

    // A body edit on the terminal doc is refused (exit 1).
    let tampered =
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\nTAMPERED\n";
    let out = nodex(root)
        .args(["check", "--content", "docs/d.md=-"])
        .write_stdin(tampered)
        .assert()
        .failure()
        .code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/frozen")),
        "proposed body edit on a terminal doc must fire body_immutable/frozen: {env}"
    );

    // Proposing the identical bytes is clean (exit 0).
    nodex(root)
        .args(["check", "--content", "docs/d.md=-"])
        .write_stdin(on_disk)
        .assert()
        .success();
}

#[test]
fn check_content_allows_out_of_scope_path() {
    // A path the project never graphs has no node identity, so proposing
    // any content for it is vacuously clean — the overlay is ignored.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    let data = run_json(
        nodex(root)
            .args(["check", "--content", "README.md=-"])
            .write_stdin("# whatever, not in scope\n"),
    );
    assert_eq!(data.pointer("/total").and_then(Value::as_u64), Some(0));

    // Even malformed frontmatter is vacuously clean out of scope —
    // nodex governs no node there, so the parse gate must not block a
    // write it has no say over (e.g. a PreToolUse hook covering every
    // file in the repository).
    let data = run_json(
        nodex(root)
            .args(["check", "--content", "README.md=-"])
            .write_stdin("---\nid: [unclosed\n---\n# not in scope\n"),
    );
    assert_eq!(data.pointer("/total").and_then(Value::as_u64), Some(0));
}

#[test]
fn check_content_validates_new_in_scope_file() {
    // A proposed file not yet on disk is graphed and validated when it
    // matches scope — here its status is out of the declared vocabulary,
    // so the new node's violation surfaces before the file is written.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         terminal = [\"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // Proposed new file carries an out-of-vocabulary status → field_enum
    // on the new node.
    let proposed = "---\nid: generic-new\ntitle: New\nkind: generic\nstatus: rogue\n---\n# New\n";
    let out = nodex(root)
        .args(["check", "--content", "docs/new.md=-"])
        .write_stdin(proposed)
        .assert()
        .failure()
        .code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| {
                v.get("rule_id").and_then(Value::as_str) == Some("field_enum")
                    && v.get("node_id").and_then(Value::as_str) == Some("generic-new")
            }),
        "proposed new file with an invalid status must surface field_enum: {env}"
    );
}

#[test]
fn check_content_treats_conditionally_excluded_child_as_out_of_scope() {
    // A proposed sub-artifact under a terminal parent that a
    // `conditional_exclude` rule drops must be vacuously clean — the
    // real build would never graph it, so the overlay must not either
    // (the probe runs the same `apply_conditional_excludes` the scan
    // does). The proposed content is deliberately invalid: if the
    // overlay wrongly graphed it, `field_enum` would fire.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"specs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         terminal = [\"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"specs/*/spec.md\"\n\
         child_glob = \"specs/**/tasks/**\"\ncondition = \"status_terminal\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "specs/auth/spec.md",
        "---\nid: generic-spec\ntitle: Spec\nkind: generic\nstatus: archived\n---\n# Spec\n",
    );
    nodex(root).arg("build").assert().success();

    let data = run_json(
        nodex(root)
            .args(["check", "--content", "specs/auth/tasks/t1.md=-"])
            .write_stdin(
                "---\nid: generic-t1\ntitle: T1\nkind: generic\nstatus: rogue\n---\n# T1\n",
            ),
    );
    assert_eq!(
        data.pointer("/total").and_then(Value::as_u64),
        Some(0),
        "a conditionally-excluded proposed child must be vacuously clean: {data}"
    );
}

#[test]
fn check_content_normalizes_dot_prefixed_path() {
    // `./docs/d.md` and `docs/d.md` name the same document; the lexical
    // normalization makes the proposed path compare equal to the
    // scanner's root-relative form, so the write gate can be neither
    // sidestepped nor spuriously passed by a `./` spelling.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\noriginal\n",
    );
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args(["check", "--content", "./docs/d.md=-"])
        .write_stdin(
            "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\nTAMPERED\n",
        )
        .assert()
        .failure()
        .code(1);
}

#[test]
fn check_content_rejects_traversal_path() {
    // A `..` form could name a file outside the project; the same
    // lexical guard rename applies refuses it before any work runs.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    let output = nodex(root)
        .args(["check", "--content", "../outside.md=-"])
        .write_stdin("# x\n")
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
}

#[test]
fn check_content_rejects_unparseable_proposal() {
    // The build drops an unparseable file (one bad doc never hides the
    // rest of the graph) — but a write gate must never approve bytes
    // that would destroy the node. The proposal vanishes from the
    // overlay graph as a `Graph::parse_failures` record, the delta sees
    // the new node-less `parse_failure` violation, and the gate exits 1
    // — the same uniform rule path every other validation finding takes.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    let out = nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin("---\nid: [unclosed\ntitle: broken\n---\n# A\n")
        .assert()
        .failure()
        .code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| {
                v.get("rule_id").and_then(Value::as_str) == Some("parse_failure")
                    && v.get("path").and_then(Value::as_str) == Some("docs/a.md")
            }),
        "a proposal that destroys its own node must red the gate via parse_failure: {env}"
    );

    // The flip side — bare markdown (no frontmatter) is a legal
    // document (everything is inferred), so the gate must not reject
    // it: the rule refuses malformed YAML, never plain prose.
    nodex(root)
        .args(["check", "--content", "docs/bare.md=-"])
        .write_stdin("# Just prose, no frontmatter\n")
        .assert()
        .success();
}

#[test]
fn check_content_distinguishes_broken_byte_states_at_one_path() {
    // The target is ALREADY broken on disk. The parse_failure violation
    // carries the content digest, so proposing *different* broken bytes
    // produces a violation absent from the before-report and the gate
    // refuses — the same error class over new bytes is never laundered
    // through the delta. Proposing the byte-identical broken content (a
    // true no-op) cancels exactly and passes.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    // Both byte-states fail with the same error class (an opened fence
    // that never closes), so only the digest can tell them apart.
    let broken_on_disk = "---\nid: generic-a\ntitle: A\nno close\n";
    write_doc(root, "docs/a.md", broken_on_disk);
    nodex(root).arg("build").assert().success();

    let different_broken = "---\nid: generic-a\ntitle: A2\nstill no close\n";
    let out = nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(different_broken)
        .assert()
        .failure()
        .code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| {
                v.get("rule_id").and_then(Value::as_str) == Some("parse_failure")
                    && v.get("path").and_then(Value::as_str) == Some("docs/a.md")
            }),
        "different broken bytes must red the gate via parse_failure: {env}"
    );

    // Byte-identical broken content is a no-op: the violation cancels
    // against the before-report exactly.
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(broken_on_disk)
        .assert()
        .success();
}

#[test]
fn check_content_rejects_proposal_with_a_bad_field_via_field_parse() {
    // A proposal whose built-in field fails coercion keeps its node in
    // the overlay graph; the gate reds on the new `field_parse`
    // violation attributed to the overlaid node.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    let out = nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\ncreated: yesterday\n---\n# A\n",
        )
        .assert()
        .failure()
        .code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| {
                v.get("rule_id").and_then(Value::as_str) == Some("field_parse")
                    && v.get("node_id").and_then(Value::as_str) == Some("generic-a")
            }),
        "a bad field value must red the gate via field_parse on the overlaid node: {env}"
    );
}

#[test]
fn check_content_delta_ignores_a_pre_existing_failure_elsewhere() {
    // A pre-existing malformed doc elsewhere in the repo appears in
    // both the before and after reports and cancels out of the delta —
    // an agent gating an unrelated edit is never blocked by someone
    // else's broken document. Project-wide `nodex check` still reports
    // it (exit 1) until it is fixed.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(root, "docs/bad.md", "---\nid: [unclosed yaml\n---\n# bad\n");
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // The unrelated clean edit passes the gate.
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A edited\n",
        )
        .assert()
        .success();

    // The project-wide check still reds on the pre-existing failure.
    nodex(root).arg("check").assert().failure().code(1);
}

#[test]
fn lifecycle_set_treats_explicitly_empty_typed_attr_as_missing() {
    // The guard shares the cross_field rule's own `is_field_missing`
    // semantics: a typed attr declared as an explicitly empty string is
    // missing to the rule, so it must be missing to the guard too —
    // otherwise `set` would write a document its own `check` rejects.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\n\
         enums = { reason = [\"cleanup\", \"superseded-by-plan\"] }\n\
         cross_field = [{ when = \"status=archived\", require = \"reason\" }]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/empty.md",
        "---\nid: generic-empty\ntitle: Empty\nkind: generic\nstatus: active\nreason: \"\"\n---\n# E\n",
    );
    write_doc(
        root,
        "docs/filled.md",
        "---\nid: generic-filled\ntitle: Filled\nkind: generic\nstatus: active\nreason: cleanup\n---\n# F\n",
    );
    nodex(root).arg("build").assert().success();

    // Explicitly empty → missing → refused, document untouched.
    let out = nodex(root)
        .args(["lifecycle", "set", "generic-empty", "--status", "archived"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("reason"),
        "names the missing field: {stdout}"
    );
    assert!(
        fs::read_to_string(root.join("docs/empty.md"))
            .unwrap()
            .contains("status: active")
    );

    // A real value satisfies the requirement → set succeeds.
    nodex(root)
        .args(["lifecycle", "set", "generic-filled", "--status", "archived"])
        .assert()
        .success();
}

#[test]
fn lifecycle_set_guards_a_predicate_keyed_on_the_updated_field_it_writes() {
    // `set` writes `updated` as well as `status`, so a cross_field
    // predicate keyed on `updated` is one the action can activate — the
    // guard must answer for it, not only status-keyed predicates.
    // `when updated exists require reason`: the set writes `updated`, so
    // the predicate fires, and a missing `reason` would make the written
    // doc fail check — the guard refuses up front.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\n\
         enums = { reason = [\"cleanup\"] }\n\
         cross_field = [{ when = \"updated exists\", require = \"reason\" }]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // No `reason` → the write would make `updated` exist while `reason`
    // is missing → refused, document untouched.
    let out = nodex(root)
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("reason"),
        "names the missing field: {stdout}"
    );
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("status: active"),
        "refused set leaves the document untouched"
    );

    // With `reason` already present, the same set is clean.
    write_doc(
        root,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\nreason: cleanup\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["lifecycle", "set", "generic-b", "--status", "archived"])
        .assert()
        .success();
}

#[test]
fn lifecycle_set_allows_status_whose_required_field_set_itself_writes() {
    // The guard evaluates cross_field against the POST-set document, so
    // a requirement `set` itself satisfies (`updated`, written on every
    // set) must never false-reject a valid transition — the document
    // the guard judges is exactly the one `check` would see.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\n\
         cross_field = [{ when = \"status=archived\", require = \"updated\" }]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // `set --status archived` writes `updated: <today>`, satisfying the
    // requirement → it must succeed, not false-reject.
    nodex(root)
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .success();
    let written = fs::read_to_string(root.join("docs/a.md")).unwrap();
    assert!(written.contains(r#"status: "archived""#));
    assert!(written.contains("updated:"));

    // And the written document passes the project's own check (the
    // self-consistency invariant the guard exists to protect).
    nodex(root).arg("build").assert().success();
    let data = run_json(nodex(root).args(["check"]));
    assert_eq!(data.pointer("/total").and_then(Value::as_u64), Some(0));
}

#[test]
fn check_content_respects_severity_filter() {
    // `--severity` composes with `--content` exactly as with any other
    // check mode: the exit code follows the *reported* (filtered) set.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\noriginal\n",
    );
    nodex(root).arg("build").assert().success();
    let tampered =
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\nTAMPERED\n";

    nodex(root)
        .args(["check", "--content", "docs/d.md=-", "--severity", "error"])
        .write_stdin(tampered)
        .assert()
        .failure()
        .code(1);
    nodex(root)
        .args(["check", "--content", "docs/d.md=-", "--severity", "warning"])
        .write_stdin(tampered)
        .assert()
        .success();
}

#[test]
fn check_content_missing_file_source_is_io_error() {
    // A `--content FILE` read failure is typed through Error::Io, so
    // the envelope carries IO_ERROR — never the INTERNAL_ERROR
    // catch-all (the classifier only recognises the typed chain).
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    let output = nodex(root)
        .args([
            "check",
            "--content",
            "docs/a.md=/nonexistent-nodex-proposed-content",
        ])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("IO_ERROR")
    );
}

#[test]
fn check_content_conflicts_with_since() {
    // `--content` and `--since` are mutually exclusive — one validates an
    // unwritten proposal, the other a committed range. clap rejects the
    // combination before any work runs.
    let tmp = scratch();
    let output = nodex(tmp.path())
        .args(["check", "--content", "docs/x.md=-", "--since", "HEAD"])
        .write_stdin("# x\n")
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "clap conflict exits 2");
}

#[test]
fn check_content_does_not_mutate_cache() {
    // A write-time check is read-only: validating a proposal must not
    // touch cache.json (neither the baseline nor the overlay build
    // persists it), so the proposed bytes can never leak into a later
    // real build.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    let cache_path = root.join("_index/cache.json");
    let before = fs::read(&cache_path).expect("cache.json exists after build");
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A edited\n",
        )
        .assert()
        .success();
    let after = fs::read(&cache_path).expect("cache.json still present");
    assert_eq!(before, after, "check --content must not mutate cache.json");
}

#[test]
fn diff_reports_added_node_between_two_commits() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);

    // Initialise a real git repo so worktree add works.
    let git = git_runner(root);
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
    // Default listing carries the full five-field spine and no `attrs`.
    let first = &items[0];
    assert!(
        first.get("title").is_some(),
        "default spine includes title: {first}"
    );
    assert!(
        first.get("attrs").is_none(),
        "no --fields → no attrs: {first}"
    );
}

#[test]
fn query_nodes_fields_projects_declared_frontmatter_under_attrs() {
    // An agent pulls a document's own frontmatter (here the built-in
    // `owner`) in one listing instead of reparsing the file — the spine
    // projects in place, the declared field lands under `attrs`.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nowner: alice\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["query", "nodes", "--fields", "id,owner"]));
    let item = &data["items"].as_array().expect("items")[0];
    assert_eq!(item.get("id").and_then(Value::as_str), Some("doc-a"));
    assert!(
        item.get("title").is_none(),
        "an unrequested spine field is dropped: {item}"
    );
    assert_eq!(
        item.pointer("/attrs/owner").and_then(Value::as_str),
        Some("alice"),
        "the declared field is projected under attrs: {item}"
    );
}

#[test]
fn query_nodes_where_filters_by_field_equality() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nowner: alice\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\nowner: bob\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "nodes", "--where", "owner=alice"]));
    let ids: Vec<&str> = data["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, ["doc-a"], "only owner=alice matches");
}

#[test]
fn query_nodes_where_rejects_unknown_field_and_malformed_clause() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let config_error = |args: &[&str]| {
        let out = nodex(tmp.path()).args(args).output().expect("ran");
        assert!(!out.status.success(), "expected failure for {args:?}");
        let parsed: Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("JSON");
        assert_eq!(
            parsed.pointer("/error/code").and_then(Value::as_str),
            Some("CONFIG_ERROR"),
            "for {args:?}: {parsed}"
        );
    };
    // An undeclared field would silently match nothing — refused.
    config_error(&["query", "nodes", "--where", "nope=x"]);
    // A clause without `=` is not FIELD=VALUE — refused.
    config_error(&["query", "nodes", "--where", "owner"]);
    // A collection-valued built-in compares against a comma-joined string
    // and would silently miss multi-value docs — refused, symmetric with
    // the cross_field load-time guard. (Use `--tag` for tag membership.)
    config_error(&["query", "nodes", "--where", "tags=auth"]);
    config_error(&["query", "nodes", "--where", "supersedes=spec-1"]);
}

#[test]
fn query_nodes_where_filters_by_path() {
    // `--where path=<exact>` filters on the same canonical forward-slash
    // path that `--fields path` projects — the documented "same read"
    // promise holds for the spine `path` field, not just frontmatter.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "nodes", "--where", "path=docs/a.md"]));
    let ids: Vec<&str> = data["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert_eq!(ids, ["doc-a"], "only the node at docs/a.md matches");
}

#[test]
fn query_nodes_fields_rejects_an_undeclared_field() {
    // A field that is neither a spine field nor declared by the project
    // is a CONFIG_ERROR, never a silently dropped projection.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "nodes", "--fields", "not_a_field"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR"),
        "undeclared --fields entry rejected: {parsed}"
    );
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
fn query_search_rejects_empty_keyword() {
    // An empty keyword is a substring of every document, so it would
    // "match" the whole corpus — the opposite of a keyword search. It is
    // refused loud (CONFIG_ERROR), symmetric with the unknown-status and
    // zero-limit guards.
    let tmp = scratch();
    seed_listing_corpus(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["query", "search", ""])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "empty keyword must fail loud");
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "CONFIG_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("must not be empty")
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
        .find(|r| r["id"].as_str() == Some("acyclic_relation"))
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
fn check_since_keeps_node_less_parse_failure_violation() {
    // `--since` narrowing is set-membership over node ids; a dropped
    // document has no node, so its `parse_failure` violation must
    // survive the filter (the cycle-detection convention) even when the
    // since-window names other documents entirely.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(root, "docs/bad.md", "---\nid: [unclosed yaml\n---\n# bad\n");
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Touch only the healthy doc since the baseline.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A edited\n",
    );
    nodex(root).arg("build").assert().success();

    let out = nodex(root)
        .args(["check", "--since", "HEAD"])
        .assert()
        .failure()
        .code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| {
                v.get("rule_id").and_then(Value::as_str) == Some("parse_failure")
                    && v.get("path").and_then(Value::as_str) == Some("docs/bad.md")
            }),
        "node-less parse_failure must survive --since narrowing: {env}"
    );
}

#[test]
fn check_since_keeps_cycle_whose_anchor_is_untouched() {
    // A cycle is a project-wide structural finding, so it must survive
    // `--since` narrowing even when the ring is closed by editing a node
    // OTHER than the cycle's DFS anchor. DFS enters at the smallest id
    // (`aaa`), so the anchor is `aaa`; the committed-then-edited node is
    // `ccc`. A node-pinned violation would be filtered out (`ccc != aaa`);
    // a node-less one is kept — this is exactly that guarantee.
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

[rules]
acyclic_relations = ["implements"]
"#,
    )
    .unwrap();

    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);

    // Acyclic implements chain: aaa → bbb → ccc.
    write_doc(
        root,
        "docs/aaa.md",
        "---\nid: aaa\ntitle: A\nkind: generic\nstatus: active\nimplements: bbb\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/bbb.md",
        "---\nid: bbb\ntitle: B\nkind: generic\nstatus: active\nimplements: ccc\n---\n# B\n",
    );
    write_doc(
        root,
        "docs/ccc.md",
        "---\nid: ccc\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "acyclic chain"]);

    // Close the ring by editing ONLY ccc (touched id = ccc); the cycle's
    // DFS anchor is the untouched aaa.
    write_doc(
        root,
        "docs/ccc.md",
        "---\nid: ccc\ntitle: C\nkind: generic\nstatus: active\nimplements: aaa\n---\n# C\n",
    );

    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["check", "--since", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a newly-closed cycle must fail check --since"
    );
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str) == Some("acyclic_relation")
                && v.get("node_id").is_some_and(Value::is_null)
        }),
        "check --since must keep the node-less cycle violation: {violations:?}"
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

// ─── guarded write primitive (output.dir / nodex.toml containment) ──

#[cfg(unix)]
#[test]
fn report_refuses_symlinked_output_dir_escape() {
    // `output.dir` passes the lexical load check, but a symlinked
    // ancestor can still resolve it outside the project. The write
    // primitive enforces containment, so report hard-fails — writing
    // the artefacts is the command's purpose.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    unix_fs::symlink(outside.path(), tmp.path().join("_index")).unwrap();

    let output = nodex(tmp.path()).arg("report").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
    assert!(
        !outside.path().join("graph.json").exists() && !outside.path().join("GRAPH.md").exists(),
        "nothing may land outside the project root"
    );
}

#[cfg(unix)]
#[test]
fn build_refuses_graph_json_write_through_escaping_output_dir() {
    // graph.json is the build's purpose, so an escaping output dir is a
    // hard PATH_ESCAPES_ROOT — never a silent write outside the root.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    unix_fs::symlink(outside.path(), tmp.path().join("_index")).unwrap();

    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
    assert!(!outside.path().join("graph.json").exists());
}

#[cfg(unix)]
#[test]
fn build_warns_and_skips_cache_persist_when_cache_json_is_a_symlink() {
    // The cache is an optimization, not the build's purpose: a
    // cache.json the primitive refuses (here: the user's symlink, which
    // the staged rename would otherwise silently replace) degrades to
    // an honest envelope warning while the graph data stays correct.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    fs::create_dir_all(tmp.path().join("_index")).unwrap();
    unix_fs::symlink(
        outside.path().join("external-cache.json"),
        tmp.path().join("_index/cache.json"),
    )
    .unwrap();

    let envelope = run_envelope(nodex(tmp.path()).arg("build"));
    assert_eq!(
        envelope.pointer("/data/nodes").and_then(Value::as_u64),
        Some(1),
        "the graph itself is correct"
    );
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings present");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("cache save failed")),
        "cache persistence failure must surface as a warning: {warnings:?}"
    );
    assert!(
        !outside.path().join("external-cache.json").exists(),
        "nothing was written through the link"
    );
    assert!(tmp.path().join("_index/graph.json").exists());
}

#[cfg(unix)]
#[test]
fn init_refuses_dangling_symlinked_nodex_toml() {
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    let ghost = outside.path().join("ghost.toml");
    unix_fs::symlink(&ghost, tmp.path().join("nodex.toml")).unwrap();

    let output = nodex(tmp.path()).arg("init").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("SYMLINK_TARGET")
    );
    assert!(!ghost.exists(), "nothing was written through the link");
}

// ─── migrate batch resilience ───────────────────────────────────────

#[cfg(unix)]
#[test]
fn migrate_apply_warns_and_skips_unreadable_file_instead_of_aborting() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/good.md", "# Good\nBody.\n");
    write_doc(tmp.path(), "docs/blocked.md", "# Blocked\nBody.\n");
    fs::set_permissions(
        tmp.path().join("docs/blocked.md"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let envelope = run_envelope(nodex(tmp.path()).args(["migrate", "--apply"]));
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings present");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("could not read in-scope file docs/blocked.md")),
        "the unreadable file rides the warnings array: {warnings:?}"
    );
    let changes = envelope
        .pointer("/data/changes")
        .and_then(Value::as_array)
        .expect("changes array");
    assert_eq!(changes.len(), 1, "only the readable file was migrated");
    assert!(
        fs::read_to_string(tmp.path().join("docs/good.md"))
            .unwrap()
            .starts_with("---\n"),
        "the readable sibling was still migrated"
    );

    // Restore permissions so the tempdir can be cleaned up.
    fs::set_permissions(
        tmp.path().join("docs/blocked.md"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
}

// ─── scaffold: self-sufficiency ─────────────────────────────────────

#[test]
fn scaffold_works_without_prior_build() {
    // scaffold builds its before-graph live from the working tree —
    // no graph.json read, no `nodex build` prerequisite, and the
    // read-only overlay builds persist nothing.
    let tmp = scratch();
    init_project(tmp.path());
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Fresh"])
            .args(["--path", "docs/fresh.md"]),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    assert!(tmp.path().join("docs/fresh.md").exists());
    assert!(
        !tmp.path().join("_index/graph.json").exists()
            && !tmp.path().join("_index/cache.json").exists(),
        "scaffold's overlay builds are read-only"
    );
}

// ─── scaffold: --body / --field content gate ────────────────────────

#[test]
fn scaffold_with_body_and_field_writes_exactly_validated_bytes_and_passes_check() {
    let tmp = scratch();
    init_project(tmp.path());
    let body = "# Real Content\n\nDecided: yes.\n";
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Real"])
            .args(["--path", "docs/real.md", "--body", "-"])
            .args(["--field", "tags=[\"decision\"]"])
            .write_stdin(body),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    let on_disk = fs::read_to_string(tmp.path().join("docs/real.md")).unwrap();
    assert_eq!(
        data.get("content").and_then(Value::as_str),
        Some(on_disk.as_str()),
        "the envelope's content and the written bytes are identical"
    );
    assert!(on_disk.ends_with(body), "the supplied body lands verbatim");
    assert!(on_disk.contains("tags: [\"decision\"]"));

    nodex(tmp.path()).arg("build").assert().success();
    let check = run_json(nodex(tmp.path()).arg("check"));
    assert_eq!(
        check.get("has_errors").and_then(Value::as_bool),
        Some(false),
        "the scaffolded document passes its own project's check: {check}"
    );
}

#[test]
fn scaffold_field_enum_violation_refuses() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\nenums = { severity = [\"low\", \"high\"] }\n",
    )
    .unwrap();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Bad"])
        .args(["--path", "docs/bad.md", "--field", "severity=wat"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("field_enum"),
        "the refusal names the rule: {envelope}"
    );
    assert!(!tmp.path().join("docs/bad.md").exists());
}

#[test]
fn scaffold_field_unknown_key_refused_in_strict_mode() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\nmode = \"strict\"\n",
    )
    .unwrap();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Mystery"])
        .args(["--path", "docs/mystery.md", "--field", "mystery=\"x\""])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("unknown_field"),
        "the refusal names the rule: {envelope}"
    );
    assert!(!tmp.path().join("docs/mystery.md").exists());
}

#[test]
fn scaffold_field_reserved_key_refused() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/t.md", "--field", "status=active"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("reserved"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/t.md").exists());

    // `path` is a reserved structural field (set via `--path`), so it is
    // refused at the `--field` seam too — symmetric with the schema /
    // cross_field reservation, not silently written as an inert key.
    let out_path = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/t.md", "--field", "path=docs/other.md"])
        .output()
        .expect("ran");
    assert_eq!(out_path.status.code(), Some(2));
    let env_path: Value =
        serde_json::from_str(String::from_utf8_lossy(&out_path.stdout).trim()).expect("json");
    assert_eq!(
        env_path.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR"),
        "{env_path}"
    );
    assert!(!tmp.path().join("docs/t.md").exists());
}

#[test]
fn scaffold_duplicate_field_key_refused() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/t.md"])
        .args(["--field", "owner=\"a\"", "--field", "owner=\"b\""])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(!tmp.path().join("docs/t.md").exists());
}

#[test]
fn scaffold_malformed_field_pair_rejected_by_clap() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/t.md", "--field", "no-equals-sign"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT")
    );
}

#[test]
fn scaffold_field_supersedes_cycle_refused() {
    // The overlay build refuses structurally: once the scaffolded node
    // exists, doc-a → generic-c resolves and generic-c → doc-a closes
    // the supersession ring.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nsupersedes: [generic-c]\n---\n# A\n",
    );
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "C"])
        .args(["--path", "docs/c.md", "--field", "supersedes=[doc-a]"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CYCLE_DETECTED")
    );
    assert!(!tmp.path().join("docs/c.md").exists());
}

#[test]
fn scaffold_with_content_refuses_unfilled_required_placeholder_then_passes_with_field() {
    // Strategy 3: supplying content engages the strict gate — the
    // placeholder a default-only scaffold writes as an advisory now
    // refuses, and --field is the documented remedy.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\nrequired = [\"component\"]\n",
    )
    .unwrap();

    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Gated"])
        .args(["--path", "docs/gated.md", "--body", "-"])
        .write_stdin("# Gated\n")
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("required_field"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/gated.md").exists());

    // --field satisfies the same finding.
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Gated"])
            .args(["--path", "docs/gated.md", "--body", "-"])
            .args(["--field", "component=\"auth\""])
            .write_stdin("# Gated\n"),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(tmp.path().join("docs/gated.md"))
            .unwrap()
            .contains("component: \"auth\"")
    );
}

#[test]
fn scaffold_default_path_still_writes_with_placeholder_warnings() {
    // Strategy 2: a default-only scaffold keeps the write-and-advise
    // contract — the placeholder required field rides the warnings
    // array, never a refusal.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\nrequired = [\"component\"]\n",
    )
    .unwrap();
    let envelope = run_envelope(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Advised"])
            .args(["--path", "docs/advised.md"]),
    );
    assert_eq!(
        envelope.pointer("/data/written").and_then(Value::as_bool),
        Some(true)
    );
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings present");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("required_field")),
        "the unfilled placeholder is an advisory: {warnings:?}"
    );
    assert!(tmp.path().join("docs/advised.md").exists());
}

#[test]
fn scaffold_field_with_unparseable_yaml_value_refused_via_parse_failure_delta() {
    // A field value that breaks the whole YAML block drops the document
    // from the overlay graph as a typed parse failure; the delta refuses
    // on the new node-less parse_failure violation.
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Broken"])
        .args(["--path", "docs/broken.md", "--field", "note=[unclosed"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("parse_failure"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/broken.md").exists());
}

#[test]
fn scaffold_field_bad_builtin_value_refused_via_field_parse() {
    // A bad value for a built-in typed field parses leniently into a
    // FieldParseIssue; the node still builds, and the attributable
    // field_parse Error refuses under strategy 3.
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "BadDate"])
        .args([
            "--path",
            "docs/bad-date.md",
            "--field",
            "created=not-a-date",
        ])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("field_parse"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/bad-date.md").exists());
}

#[test]
fn scaffold_cross_field_when_keyed_on_supplied_field_emits_require() {
    // The cross_field fixpoint reparses the frontmatter as written, so
    // a `when` keyed on a *supplied* value fires and its `require`
    // field is emitted (here with an enum-valid default).
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\n\
         enums = { component = [\"auth\", \"billing\"], auth_review = [\"pending\", \"done\"] }\n\
         cross_field = [{ when = \"component=auth\", require = \"auth_review\" }]\n",
    )
    .unwrap();
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Auth Thing"])
            .args(["--path", "docs/auth-thing.md", "--field", "component=auth"]),
    );
    let content = data.get("content").and_then(Value::as_str).unwrap();
    assert!(content.contains("component: auth"), "{content}");
    assert!(
        content.contains("auth_review: \"pending\""),
        "the require keyed on the supplied value is emitted: {content}"
    );

    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path()).arg("check").assert().success();
}

// ─── scaffold: immutability lock consult ────────────────────────────

/// A committed project whose `immutable_baseline` freezes terminal
/// bodies — the fixture for the recreate/--force lock tests.
/// A committed project whose `immutable_baseline` freezes a frontmatter field
/// and declares no body lock at all — the shape whose path-address protection
/// was lost when the destruction predicate asked only about bodies.
fn frontmatter_frozen_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.frontmatter_immutable]]\nname = \"locked-owner\"\nfields = [\"owner\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: superseded\nowner: alice\n---\n\
         # A\nFrozen record.\n",
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "baseline"]);
}

/// A record frozen only by its frontmatter is still frozen history, and the
/// path address is the only thing that can guard it from destruction — an
/// overwrite landing a different id shares no join key, so no rule fires.
#[test]
fn scaffold_force_refuses_overwriting_a_frontmatter_frozen_record() {
    let tmp = scratch();
    let project = tmp.path();
    frontmatter_frozen_project(project);
    let before = fs::read_to_string(project.join("docs/a.md")).unwrap();

    let output = nodex(project)
        .args(["scaffold", "--kind", "generic", "--title", "Replacement"])
        .args(["--id", "doc-new", "--path", "docs/a.md", "--force"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR"),
        "a frontmatter-only project has frozen records too: {envelope}"
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("frontmatter_immutable/locked-owner"),
        "and the refusal names the family that froze it: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(project.join("docs/a.md")).unwrap(),
        before,
        "the frozen bytes survive"
    );
}

/// `migrate` injects a whole frontmatter block, which changes every field in
/// it. A document born terminal (`statuses.initial` is itself terminal) has its
/// locks armed from the baseline, so the injection is a write the project's own
/// `check` rejects — and the seam has to refuse it rather than write and let
/// `check` complain afterwards.
#[test]
fn migrate_refuses_injecting_a_field_the_baseline_locks() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"sealed\", \"active\"]\nterminal = [\"sealed\"]\n\
         initial = \"sealed\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\nrequired = [\"owner\"]\n\
         [[rules.frontmatter_immutable]]\nname = \"locked-owner\"\nfields = [\"owner\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n",
    )
    .unwrap();
    let bare = "# Legacy\n\nno frontmatter at all\n";
    write_doc(project, "docs/legacy.md", bare);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a bare legacy document"]);

    let envelope = run_envelope(nodex(project).args(["migrate", "--apply"]));
    assert_eq!(
        envelope["data"]["total"], 0,
        "the write plane declines what `check` would red: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("frontmatter_immutable/locked-owner")),
        "and names the rule that governs it: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(project.join("docs/legacy.md")).unwrap(),
        bare,
        "the document is untouched"
    );
}

/// A file that is both symlinked and would be refused reports the symlink,
/// because the write discipline is decided before any verdict is asked for: a
/// path that must never be written through cannot be made writable by a lock
/// that happens to permit it.
#[test]
#[cfg(unix)]
fn a_symlinked_file_reports_the_symlink_rather_than_the_lock() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    // The target lives *inside* the project root, so `reject_outside_root`
    // admits it and only the symlink predicate can decline the write — which
    // is what this test is named for. It sits outside `scope.include`, so the
    // link is the only path the graph reaches it by. It is a frozen document
    // referencing generic-b, so the lock is a genuine alternative reason the
    // seam would report if the symlink guard let the plan through.
    let target = project.join("store/a.md");
    fs::create_dir_all(project.join("store")).unwrap();
    fs::write(
        &target,
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    )
    .unwrap();
    fs::create_dir_all(project.join("docs")).unwrap();
    std::os::unix::fs::symlink("../store/a.md", project.join("docs/a.md")).unwrap();
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a symlinked frozen document"]);

    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    let reasons: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .collect();
    assert!(
        reasons.iter().any(|m| m.contains("symlink")),
        "the symlink is the reason reported: {envelope}"
    );
    assert!(
        !reasons.iter().any(|m| m.contains("body_immutable")),
        "the lock is never reached, so it is never the reason: {envelope}"
    );
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("[[generic-b]]"),
        "and the target is not written through"
    );
}

fn frozen_baseline_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"adr-frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\nFrozen record.\n",
    );
    let git = git_runner(root);
    git(&["init"]);
    git(&["add", "."]);
    git(&["commit", "-m", "baseline"]);
}

#[test]
fn scaffold_force_refuses_overwriting_baseline_locked_doc() {
    let tmp = scratch();
    frozen_baseline_project(tmp.path());
    let before = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();

    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Rewrite"])
        .args(["--path", "docs/a.md", "--force"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("body_immutable/adr-frozen"),
        "the refusal names the lock: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("docs/a.md")).unwrap(),
        before,
        "the frozen bytes survive"
    );
}

#[test]
fn scaffold_refuses_recreating_deleted_locked_doc() {
    // Deleting a frozen record and re-scaffolding its path is the same
    // rewrite `check` against the baseline would flag — refused without
    // `--force`, since the path no longer exists on disk.
    let tmp = scratch();
    frozen_baseline_project(tmp.path());
    fs::remove_file(tmp.path().join("docs/a.md")).unwrap();

    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Recreate"])
        .args(["--path", "docs/a.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("body_immutable/adr-frozen"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/a.md").exists());
}

/// The path a creation lands on is only one of the two ways it reaches the
/// baseline. `check` pairs by id, so a scaffold that gives a frozen record's
/// id a different body is a `body_change` in the report — and a lock that
/// asks only "does a frozen record stand at this path" answers `None` for it,
/// because the document need not land where it stood. No `--force` is
/// involved: the new path holds nothing.
#[test]
fn scaffold_refuses_relocating_a_frozen_record_under_its_own_id() {
    let tmp = scratch();
    let project = tmp.path();
    frozen_baseline_project(project);
    fs::remove_file(project.join("docs/a.md")).unwrap();
    let body = project.join("body.md");
    fs::write(&body, "# A\nA different body entirely.\n").unwrap();

    let output = nodex(project)
        .args(["scaffold", "--kind", "generic", "--title", "A"])
        .args(["--id", "doc-a", "--path", "docs/elsewhere.md"])
        .args(["--body", body.to_str().unwrap()])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR"),
        "the write plane refuses what `check` reds: {envelope}"
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("body_immutable/adr-frozen"),
        "and names the rule that governs it: {envelope}"
    );
    assert!(
        !project.join("docs/elsewhere.md").exists(),
        "nothing was written"
    );

    // The same seam still creates a record the baseline does not hold.
    let fresh = run_envelope(
        nodex(project)
            .args(["scaffold", "--kind", "generic", "--title", "Fresh"])
            .args(["--id", "doc-fresh", "--path", "docs/fresh.md"])
            .args(["--body", body.to_str().unwrap()]),
    );
    assert_eq!(
        fresh["data"]["written"],
        Value::Bool(true),
        "an unheld id at an empty path is not locked: {fresh}"
    );
}

// ─── status & the snapshot contract ─────────────────────────────────

#[test]
fn status_walks_the_snapshot_state_ladder() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\n---\n# A\n",
    );

    // No build yet → absent, exit 0 (probe, not gate).
    let data = run_json(nodex(tmp.path()).arg("status"));
    assert_eq!(data["state"], "absent");

    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).arg("status"));
    assert_eq!(data["state"], "current");
    assert!(data.get("divergence").is_none());
    assert!(data["snapshot_nodex_version"].is_string());

    // Append to an indexed doc → the content probe flags it.
    let doc = tmp.path().join("docs/a.md");
    let mut content = fs::read_to_string(&doc).unwrap();
    content.push_str("\nmore\n");
    fs::write(&doc, content).unwrap();
    let data = run_json(nodex(tmp.path()).arg("status"));
    assert_eq!(data["state"], "outdated");
    assert_eq!(data["divergence"]["changed_paths"][0], "docs/a.md");

    // Rebuild clears it; a new in-scope file is membership divergence.
    nodex(tmp.path()).arg("build").assert().success();
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\n---\n# B\n",
    );
    let data = run_json(nodex(tmp.path()).arg("status"));
    assert_eq!(data["state"], "outdated");
    assert_eq!(data["divergence"]["added_paths"][0], "docs/b.md");
}

#[test]
fn status_ignores_comment_edits_but_flags_graph_shaping_config_change() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // A comment-only nodex.toml edit perturbs no projected config.
    let toml_path = tmp.path().join("nodex.toml");
    let mut toml = fs::read_to_string(&toml_path).unwrap();
    toml.push_str("\n# a comment changes nothing\n");
    fs::write(&toml_path, &toml).unwrap();
    let data = run_json(nodex(tmp.path()).arg("status"));
    assert_eq!(data["state"], "current");

    // A semantic identity edit reshapes the graph → config_changed.
    toml.push_str(
        "\n[[identity.id_rules]]\nkind = \"*\"\nglob = \"docs/**\"\ntemplate = \"doc-{stem}\"\n",
    );
    fs::write(&toml_path, &toml).unwrap();
    let data = run_json(nodex(tmp.path()).arg("status"));
    assert_eq!(data["state"], "outdated");
    assert_eq!(data["divergence"]["config_changed"], true);
}

#[test]
fn query_without_snapshot_emits_graph_missing_code() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["query", "nodes"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("GRAPH_MISSING")
    );
    assert!(
        parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("nodex build"),
        "the remedy rides the message: {parsed}"
    );
}

#[test]
fn query_after_unbuilt_change_carries_divergence_warning() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // Fresh snapshot → no staleness warning.
    let envelope = run_envelope(nodex(tmp.path()).args(["query", "nodes"]));
    assert!(envelope.get("warnings").is_none(), "{envelope}");

    // Unbuilt new doc → the query still succeeds, with one advisory.
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\n---\n# B\n",
    );
    let envelope = run_envelope(nodex(tmp.path()).args(["query", "nodes"]));
    let warnings = envelope["warnings"].as_array().expect("warnings array");
    assert!(
        warnings.iter().any(|w| {
            let w = warning_msg(w).unwrap_or_default();
            w.contains("outdated") && w.contains("nodex build")
        }),
        "divergence advisory expected: {warnings:?}"
    );
    assert_eq!(
        envelope["data"]["total"], 1,
        "results come from the snapshot"
    );
}

#[test]
fn export_config_emits_resolved_surface() {
    let tmp = scratch();
    init_project(tmp.path());
    let data = run_json(nodex(tmp.path()).args(["export", "config"]));
    assert!(data["scope"]["include"].is_array());
    assert!(data["output"]["dir"].is_string());
    assert_eq!(data["identity"]["fallback_kind"], "generic");
    assert_eq!(data["identity"]["fallback_id_template"], "{kind}-{stem}");
    assert!(data["initial_status"].is_string());
}

#[test]
fn export_commands_emits_grammar_without_a_project() {
    // Config-independent: runs in a directory with no nodex.toml.
    let tmp = scratch();
    let data = run_json(nodex(tmp.path()).args(["export", "commands"]));
    let commands = data["commands"].as_array().expect("commands array");
    assert!(!commands.is_empty());
    let trust = commands
        .iter()
        .find(|c| c["schema"] == "query.trust")
        .expect("query.trust leaf");
    assert_eq!(trust["path"], serde_json::json!(["query", "trust"]));
    assert_eq!(trust["modes"][0]["schema"], "query.trust-list");
    let backlinks = commands
        .iter()
        .find(|c| c["schema"] == "query.backlinks")
        .expect("query.backlinks leaf");
    assert_eq!(backlinks["positionals"][0]["name"], "id");
    assert_eq!(backlinks["positionals"][0]["required"], true);
}

#[test]
fn export_diagnostics_emits_code_vocabularies_without_a_project() {
    // Config-independent: the error/exit-code vocabulary is the same in
    // every project, so it runs with no nodex.toml.
    let tmp = scratch();
    let data = run_json(nodex(tmp.path()).args(["export", "diagnostics"]));

    let codes: Vec<&str> = data["error_codes"]
        .as_array()
        .expect("error_codes array")
        .iter()
        .filter_map(|e| e["code"].as_str())
        .collect();
    // A core code and both CLI-classifier codes are published.
    assert!(codes.contains(&"CONFIG_ERROR"), "{codes:?}");
    assert!(codes.contains(&"INVALID_ARGUMENT"), "{codes:?}");
    assert!(codes.contains(&"INTERNAL_ERROR"), "{codes:?}");
    // The CLI-owned codes are tagged `cli`, core codes `core`.
    let origin = |code: &str| -> Option<String> {
        data["error_codes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["code"] == code)
            .and_then(|e| e["origin"].as_str())
            .map(String::from)
    };
    assert_eq!(origin("CONFIG_ERROR").as_deref(), Some("core"));
    assert_eq!(origin("INVALID_ARGUMENT").as_deref(), Some("cli"));

    let exit: Vec<u64> = data["exit_codes"]
        .as_array()
        .expect("exit_codes array")
        .iter()
        .filter_map(|e| e["code"].as_u64())
        .collect();
    assert_eq!(exit, [0, 1, 2], "documented exit-code contract");

    // The advisory-warning vocabulary is published too (the success-plane
    // counterpart to error_codes).
    let warn_codes: Vec<&str> = data["warning_codes"]
        .as_array()
        .expect("warning_codes array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        warn_codes.contains(&"gate_suppression") && warn_codes.contains(&"binary_compat"),
        "{warn_codes:?}"
    );
}

#[test]
fn export_envelope_schema_inline_refs_is_self_contained() {
    let tmp = scratch();
    let envelope =
        run_envelope(nodex(tmp.path()).args(["export", "envelope-schema", "--inline-refs"]));
    let raw = serde_json::to_string(&envelope["data"]["per_command"]).unwrap();
    assert!(!raw.contains("\"$ref\""), "inlined form must carry no $ref");
    assert!(
        !raw.contains("\"$defs\""),
        "inlined form must carry no $defs"
    );
}

// ─── contract-gate (release CI tool) ────────────────────────────────

fn contract_gate() -> Command {
    Command::cargo_bin("contract-gate").expect("contract-gate binary in cargo target")
}

fn write_schema_envelope(
    dir: &std::path::Path,
    name: &str,
    version: &str,
    per_command: Value,
) -> PathBuf {
    let path = dir.join(name);
    let envelope = serde_json::json!({
        "ok": true,
        "data": { "version": version, "envelope": {}, "per_command": per_command }
    });
    fs::write(&path, envelope.to_string()).unwrap();
    path
}

fn gate_per_command() -> Value {
    serde_json::json!({
        "build": {
            "type": "object",
            "properties": { "nodes": { "type": "integer" } },
            "required": ["nodes"]
        }
    })
}

#[test]
fn contract_gate_passes_identical_inputs() {
    let tmp = scratch();
    let baseline = write_schema_envelope(tmp.path(), "baseline.json", "0.15.0", gate_per_command());
    let head = write_schema_envelope(tmp.path(), "head.json", "0.15.0", gate_per_command());
    let output = contract_gate()
        .arg(&baseline)
        .arg(&head)
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(0));
    let verdict: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(verdict["verdict"], "pass");
    assert!(verdict["breaking"].as_array().unwrap().is_empty());
    assert!(verdict["additive"].as_array().unwrap().is_empty());
}

#[test]
fn contract_gate_fails_breaking_change_without_version_bump() {
    let tmp = scratch();
    let baseline = write_schema_envelope(tmp.path(), "baseline.json", "0.15.0", gate_per_command());
    let mut head_schema = gate_per_command();
    head_schema["build"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("nodes");
    head_schema["build"]["required"] = serde_json::json!([]);
    let head = write_schema_envelope(tmp.path(), "head.json", "0.15.0", head_schema);
    let output = contract_gate()
        .arg(&baseline)
        .arg(&head)
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(1));
    let verdict: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(verdict["verdict"], "fail");
    assert!(!verdict["breaking"].as_array().unwrap().is_empty());
}

#[test]
fn contract_gate_passes_breaking_change_with_minor_bump() {
    // Pre-1.0, the 0.x component is the breaking component — bumping
    // it satisfies the promise for any classified change.
    let tmp = scratch();
    let baseline = write_schema_envelope(tmp.path(), "baseline.json", "0.15.0", gate_per_command());
    let mut head_schema = gate_per_command();
    head_schema["build"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("nodes");
    head_schema["build"]["required"] = serde_json::json!([]);
    let head = write_schema_envelope(tmp.path(), "head.json", "0.16.0", head_schema);
    contract_gate().arg(&baseline).arg(&head).assert().success();
}

#[test]
fn contract_gate_fails_additive_change_within_patch_bump() {
    // Even an additive change needs the 0.x bump pre-1.0 — a patch
    // release must be envelope-identical.
    let tmp = scratch();
    let baseline = write_schema_envelope(tmp.path(), "baseline.json", "0.15.0", gate_per_command());
    let mut head_schema = gate_per_command();
    head_schema["build"]["properties"]["edges"] = serde_json::json!({ "type": "integer" });
    let head = write_schema_envelope(tmp.path(), "head.json", "0.15.1", head_schema);
    let output = contract_gate()
        .arg(&baseline)
        .arg(&head)
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(1));
    let verdict: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(verdict["verdict"], "fail");
    assert!(verdict["breaking"].as_array().unwrap().is_empty());
    assert!(!verdict["additive"].as_array().unwrap().is_empty());
}

#[test]
fn contract_gate_refuses_non_success_envelope_input() {
    // An error envelope (`ok: false`) — e.g. a failed export captured
    // into the baseline file — must be an operational failure, never a
    // baseline the gate diffs vacuously. Same for a payload that carries
    // `.data` without declaring `ok: true`.
    let tmp = scratch();
    let head = write_schema_envelope(tmp.path(), "head.json", "0.15.0", gate_per_command());

    let error_baseline = tmp.path().join("baseline.json");
    fs::write(
        &error_baseline,
        serde_json::json!({
            "ok": false,
            "error": { "code": "CONFIG_ERROR", "message": "boom" }
        })
        .to_string(),
    )
    .unwrap();
    let output = contract_gate()
        .arg(&error_baseline)
        .arg(&head)
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{envelope}"
    );
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(message.contains("CONFIG_ERROR"), "{message}");

    // `.data` present but no `ok: true` declaration: refused, naming
    // what was found.
    let ok_less = tmp.path().join("okless.json");
    fs::write(
        &ok_less,
        serde_json::json!({ "data": { "version": "0.15.0" } }).to_string(),
    )
    .unwrap();
    let output = contract_gate()
        .arg(&ok_less)
        .arg(&head)
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{envelope}"
    );
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(message.contains("no `ok` field"), "{message}");
}

#[test]
fn contract_gate_refuses_bare_ok_false_envelope() {
    // `{"ok": false}` with no `error` key — e.g. a truncated capture —
    // takes the not-a-successful-envelope branch, not the
    // error-envelope branch: refused naming exactly what was found.
    let tmp = scratch();
    let head = write_schema_envelope(tmp.path(), "head.json", "0.15.0", gate_per_command());
    let bare = tmp.path().join("baseline.json");
    fs::write(&bare, serde_json::json!({ "ok": false }).to_string()).unwrap();
    let output = contract_gate().arg(&bare).arg(&head).output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{envelope}"
    );
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(message.contains("`ok` is false"), "{message}");
}

#[test]
fn contract_gate_classifies_operational_failures() {
    // A file the gate cannot read is IO_ERROR; a malformed invocation
    // is INVALID_ARGUMENT — the same dispatch vocabulary as every other
    // command, so CI tooling branches on the code, never the prose.
    let tmp = scratch();
    let head = write_schema_envelope(tmp.path(), "head.json", "0.15.0", gate_per_command());

    let output = contract_gate()
        .arg(tmp.path().join("missing.json"))
        .arg(&head)
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("IO_ERROR"),
        "{envelope}"
    );

    let output = contract_gate().output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{envelope}"
    );
}

#[cfg(unix)]
#[test]
fn a_document_under_a_followed_link_can_be_renamed_at_the_name_the_graph_gives_it() {
    // A proposal names a document, not a spelling of one. Where a directory is
    // reached under several names the file is admitted under each, so a move
    // whose source is removed by exact string leaves the document behind under
    // another name — present and gone at once, and the destination collides
    // with it. The one path a caller has is the one the graph shows them.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/real/keep.md",
        "---\nid: keep\ntitle: K\nkind: generic\nstatus: active\n---\n# K\n",
    );
    write_doc(
        root,
        "docs/real/index.md",
        "---\nid: idx\ntitle: I\nkind: generic\nstatus: active\n---\n[k](keep.md)\n",
    );
    unix_fs::symlink("real", root.join("docs/alias")).unwrap();
    nodex(root).arg("build").assert().success();

    let named = run_json(nodex(root).args(["query", "node", "keep"]))
        .pointer("/node/path")
        .and_then(Value::as_str)
        .expect("the graph names the document")
        .to_string();
    let moved = named.replace("keep.md", "renamed.md");

    let env = run_envelope(nodex(root).args(["rename", &named, &moved]));
    assert_eq!(
        env.pointer("/data/new_path").and_then(Value::as_str),
        Some(moved.as_str()),
        "{env}"
    );

    // The reference the move claimed to rewrite has to resolve afterwards —
    // a rename that reports success and dangles a link is the same defect
    // wearing a green envelope.
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).arg("query").arg("issues"));
    assert_eq!(
        issues.get("unresolved_edges").and_then(Value::as_array),
        Some(&vec![]),
        "{issues}"
    );
}

#[cfg(unix)]
#[test]
fn a_link_to_the_output_directory_does_not_make_it_a_project_document() {
    // The output-dir exclusion is unconditional, and a directory is a
    // location rather than a spelling: reached under another name, nodex's
    // own GRAPH.md would be a user document that `migrate` writes
    // frontmatter into.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("report").assert().success();
    assert!(
        root.join("_index/GRAPH.md").exists(),
        "report wrote GRAPH.md"
    );
    unix_fs::symlink("_index", root.join("pub")).unwrap();

    nodex(root).args(["build", "--full"]).assert().success();
    let listed = run_json(nodex(root).args(["query", "nodes", "--fields", "id,path"]));
    let paths: Vec<&str> = listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|n| n["path"].as_str())
        .collect();
    assert_eq!(paths, vec!["docs/a.md"], "only the project's own document");

    let plan = run_json(nodex(root).arg("migrate"));
    assert_eq!(plan.get("total").and_then(Value::as_u64), Some(0), "{plan}");
}

#[cfg(unix)]
#[test]
fn a_write_seam_refuses_a_target_below_an_undescended_link() {
    // A proposal is admitted by the globs, which judge a path's spelling —
    // but membership also depends on where the path is. Below a link the
    // walk does not descend, a seam would approve the write and the next
    // build could not see the document.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/source.md",
        "---\nid: src\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    fs::create_dir_all(root.join("real")).unwrap();
    unix_fs::symlink("../real", root.join("docs/linked")).unwrap();
    nodex(root).arg("build").assert().success();

    let proposal = "---\nid: p\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n";
    let gate = run_json(
        nodex(root)
            .args(["check", "--content", "docs/linked/exact.md=-"])
            .write_stdin(proposal),
    );
    assert_eq!(
        gate.pointer("/proposals/0/in_scope")
            .and_then(Value::as_bool),
        Some(false),
        "the gate validated nothing, and says so: {gate}"
    );

    let scaffolded = envelope_of(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Exact",
        "--path",
        "docs/linked/exact.md",
    ]));
    assert_eq!(
        scaffolded.get("ok").and_then(Value::as_bool),
        Some(false),
        "{scaffolded}"
    );
    assert!(
        scaffolded
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("directory symlink the scan does not descend"),
        "the refusal names the cause the operator can act on: {scaffolded}"
    );
    assert!(!root.join("real/exact.md").exists(), "nothing was written");

    let renamed =
        envelope_of(nodex(root).args(["rename", "docs/source.md", "docs/linked/dest.md"]));
    assert_eq!(
        renamed.get("ok").and_then(Value::as_bool),
        Some(false),
        "{renamed}"
    );
    assert!(
        root.join("docs/source.md").exists(),
        "the source stayed put"
    );
}

#[cfg(unix)]
#[test]
fn an_undescended_link_at_the_baseline_reports_the_lock_inert() {
    // A ref that records a directory symlink carries no document below it
    // once the walk declines to descend, so the baseline has no node and the
    // lock cannot fire. Silence there reads as "the lock passed".
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         trigger = \"creation\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "vendor/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: archived\n---\n# frozen\n",
    );
    fs::create_dir_all(root.join("docs")).unwrap();
    unix_fs::symlink("../vendor", root.join("docs/vendor")).unwrap();
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    fs::remove_file(root.join("docs/vendor")).unwrap();
    write_doc(
        root,
        "docs/vendor/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: archived\n---\n# tampered\n",
    );

    let env = envelope_of(nodex(root).args(["check", "--since", "HEAD"]));
    let inert = env
        .get("warnings")
        .and_then(Value::as_array)
        .map(|ws| {
            ws.iter().any(|w| {
                w["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("docs/vendor is a directory symlink there")
            })
        })
        .unwrap_or(false);
    assert!(inert, "the inert lock is named, never silent: {env}");
}

#[cfg(unix)]
#[test]
fn a_document_the_scan_holds_under_two_names_says_which_one_it_uses() {
    // A followed link gives a document more than one name, and the scan keeps
    // one. The name it drops is a path the operator can read that the graph
    // does not carry, so it is reported — and a seam naming the dropped one is
    // told which to use, instead of writing a document the next build files
    // under a different path.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/real/keep.md",
        "---\nid: keep\ntitle: K\nkind: generic\nstatus: active\n---\n# K\n",
    );
    unix_fs::symlink("real", root.join("docs/alias")).unwrap();

    let built = run_json(nodex(root).arg("build"));
    assert_eq!(
        built.get("nodes").and_then(Value::as_u64),
        Some(1),
        "{built}"
    );
    let dropped = built
        .get("aliased_paths")
        .and_then(Value::as_array)
        .expect("the name it does not use is reported");
    assert_eq!(dropped.len(), 1, "{built}");
    let named = dropped[0]["named"].as_str().expect("the name in use");

    // The name the report says is in use is the one the graph carries.
    let listed = run_json(nodex(root).args(["query", "nodes", "--fields", "id,path"]));
    assert_eq!(listed["items"][0]["path"].as_str(), Some(named), "{listed}");

    // A seam naming the other one is refused, and told which path to use.
    let unused = dropped[0]["path"].as_str().expect("the name not used");
    let sibling = unused.replace("keep.md", "new.md");
    let refused = envelope_of(nodex(root).args([
        "scaffold", "--kind", "generic", "--title", "New", "--path", &sibling,
    ]));
    let message = refused
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains(&named.replace("keep.md", "new.md")),
        "the refusal names the path to use: {refused}"
    );

    // And at the name in use, the write plane and the read plane agree.
    let written = run_json(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "New",
        "--path",
        &named.replace("keep.md", "new.md"),
    ]));
    let path = written["path"].as_str().expect("path");
    nodex(root).arg("build").assert().success();
    let found = run_json(nodex(root).args(["query", "node", "--path", path]));
    assert_eq!(
        found.pointer("/node/path").and_then(Value::as_str),
        Some(path)
    );
}

#[cfg(unix)]
#[test]
fn a_seam_naming_a_document_by_an_unused_name_is_told_the_one_in_use() {
    // A followed link gives one document several names and the graph carries
    // one. Named by another, the source reads as untracked — a plain move, no
    // reference rewriting — so the real file leaves and every reference to the
    // name in use dangles. Both planes ask the same question of the same
    // channel.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"docs/**/*.md\", \"real/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "real/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/ref-a.md",
        "---\nid: ref-a\ntitle: R\nkind: generic\nstatus: active\n---\n[a](link/a.md)\n",
    );
    unix_fs::symlink("../real", root.join("docs/link")).unwrap();
    nodex(root).arg("build").assert().success();

    let in_use = run_json(nodex(root).args(["query", "node", "a"]))
        .pointer("/node/path")
        .and_then(Value::as_str)
        .expect("the graph names the document")
        .to_string();
    assert_ne!(in_use, "real/a.md", "the graph carries the other name");

    for env in [
        envelope_of(nodex(root).args(["rename", "real/a.md", "docs/a-moved.md"])),
        envelope_of(
            nodex(root)
                .args(["check", "--content", "real/a.md=-"])
                .write_stdin(
                    "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# edited\n",
                ),
        ),
    ] {
        assert_eq!(env.get("ok").and_then(Value::as_bool), Some(false), "{env}");
        assert!(
            env.pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains(&in_use),
            "the refusal names the path in use: {env}"
        );
    }
    assert!(root.join("real/a.md").exists(), "the document stayed put");

    // At the name in use, the move rewrites the reference and nothing dangles.
    let moved =
        run_envelope(nodex(root).args(["rename", &in_use, &in_use.replace("a.md", "m.md")]));
    assert_eq!(
        moved.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "{moved}"
    );
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).arg("query").arg("issues"));
    assert_eq!(
        issues.get("unresolved_edges").and_then(Value::as_array),
        Some(&vec![]),
        "{issues}"
    );
}

#[cfg(unix)]
#[test]
fn a_document_whose_name_holds_a_backslash_is_graphed_where_it_lives() {
    // `\` divides components where the platform says so and nowhere else.
    // A name the walk read has to render reversibly: folding a character the
    // filesystem allows in a filename puts a path in the graph that no reader
    // can open, and every seam that reads a document by its recorded path
    // skips it.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/target.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    let odd = "docs/literal\\ref.md";
    write_doc(
        root,
        odd,
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[t](target.md)\n",
    );
    nodex(root).arg("build").assert().success();

    let listed = run_json(nodex(root).args(["query", "nodes", "--fields", "id,path"]));
    let paths: Vec<&str> = listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|n| n["path"].as_str())
        .collect();
    assert!(
        paths.contains(&odd),
        "the graph carries the name on disk: {listed}"
    );

    // A reference from it is rewritten, which requires reading it back at the
    // path the graph recorded.
    let env = run_envelope(nodex(root).args(["rename", "docs/target.md", "docs/moved.md"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "{env}"
    );
    assert!(env.get("warnings").is_none(), "nothing was skipped: {env}");
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).arg("query").arg("issues"));
    assert_eq!(
        issues.get("unresolved_edges").and_then(Value::as_array),
        Some(&vec![]),
        "{issues}"
    );
}

#[cfg(unix)]
#[test]
fn the_validation_surface_states_what_the_walk_did_not_read() {
    // A link the walk declines is a boundary of what was read. A project whose
    // documents live behind one otherwise validates green against a corpus
    // that no longer holds them, and the gate CI runs says nothing.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    let outside = scratch();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/plain.md",
        "---\nid: plain\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n",
    );
    write_doc(
        outside.path(),
        "tree/vend.md",
        "---\nid: vend\ntitle: V\nkind: generic\nstatus: active\n---\n# V\n",
    );
    unix_fs::symlink(outside.path().join("tree"), root.join("docs/linked")).unwrap();

    for command in [vec!["check"], vec!["build"]] {
        let env = run_envelope(nodex(root).args(&command));
        let said = env
            .get("warnings")
            .and_then(Value::as_array)
            .map(|ws| {
                ws.iter().any(|w| {
                    w["code"] == "scope_coverage"
                        && w["message"]
                            .as_str()
                            .unwrap_or("")
                            .contains("not descended")
                })
            })
            .unwrap_or(false);
        assert!(said, "{command:?} names the boundary: {env}");
    }
}

/// A ref-to-ref report reads what the ref *records* and nothing else.
///
/// An unconfined build follows a link out of the checkout into the live
/// filesystem, so a symlink whose target changed between two commits yields
/// field changes that happened entirely outside the repository — history the
/// refs do not carry, reported as history they do.
#[cfg(unix)]
#[test]
fn a_ref_to_ref_report_reads_only_what_the_refs_record() {
    use std::os::unix::fs as unix_fs;
    let outside = scratch();
    for (dir, title) in [("a", "External A"), ("b", "External B")] {
        write_doc(
            outside.path(),
            &format!("{dir}/t.md"),
            &format!("---\nid: t\ntitle: {title}\nkind: generic\nstatus: active\n---\n# T\n"),
        );
    }

    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nfollow_symlinks = true\n\
         [kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    git(&["init", "-q"]);
    unix_fs::symlink(outside.path().join("a"), root.join("docs/linked")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    fs::remove_file(root.join("docs/linked")).unwrap();
    unix_fs::symlink(outside.path().join("b"), root.join("docs/linked")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "second"]);

    // Neither ref records the external documents.
    for command in [
        vec!["diff", "HEAD~1", "HEAD"],
        vec!["impact", "HEAD~1", "HEAD"],
    ] {
        let env = run_envelope(nodex(root).args(&command));
        let changes = env
            .pointer("/data/field_changes")
            .or_else(|| env.pointer("/data/diff/field_changes"))
            .and_then(Value::as_array)
            .expect("field_changes");
        assert!(
            changes.is_empty(),
            "{command:?} reported a change from outside both refs: {changes:?}"
        );
        // And says what it could not read, per ref, rather than reading
        // complete.
        for git_ref in ["HEAD~1", "HEAD"] {
            assert!(
                env.get("warnings")
                    .and_then(Value::as_array)
                    .is_some_and(|ws| ws.iter().filter_map(warning_msg).any(|w| w
                        .starts_with(&format!("{git_ref}: "))
                        && w.contains("docs/linked"))),
                "{command:?} names what {git_ref} does not record: {env}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn every_command_built_on_the_graph_states_what_the_walk_did_not_read() {
    // A link the walk declines bounds the corpus every one of these commands
    // reasons about: a rewrite skips a reference behind it, a plan omits a
    // document, a report renders a partial graph. Each carries its own
    // warnings to the envelope, so this is where the set is kept complete.
    //
    // The set is closed against the CLI's own grammar rather than
    // hand-maintained: every leaf `nodex export commands` reports is either
    // driven here or carries a reason for standing outside, and a leaf added
    // later belongs to neither until someone decides which. A list nobody is
    // forced to update is a list that stops being true.
    //
    // Every command also gets its own tree: the mutating ones would
    // otherwise decide the state the next one is asked about.
    use std::os::unix::fs as unix_fs;

    /// `docs/` holds two documents and a link out of the project the walk
    /// does not descend — a corpus with something in it and a boundary
    /// around it. With `populated = false` the two documents live behind
    /// the link instead, so the graph is empty *because* of the boundary:
    /// the shape where "no nodes" reads as "new project" and is not.
    fn fixture(populated: bool) -> (TempDir, TempDir) {
        let tmp = scratch();
        let outside = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            "[scope]\ninclude = [\"docs/**/*.md\"]\n\
             [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
        )
        .unwrap();
        let home = if populated { root } else { outside.path() };
        for (path, id) in [("docs/old.md", "old"), ("docs/new.md", "new")] {
            write_doc(
                home,
                path,
                &format!("---\nid: {id}\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n"),
            );
        }
        write_doc(
            outside.path(),
            "ref.md",
            "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[[old]]\n",
        );
        fs::create_dir_all(root.join("docs")).unwrap();
        unix_fs::symlink(outside.path(), root.join("docs/linked")).unwrap();
        nodex(root).arg("build").assert().success();
        (tmp, outside)
    }

    let scaffold: Vec<&str> = vec![
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Z",
        "--path",
        "docs/z.md",
        "--dry-run",
    ];
    // Every leaf that reads the working tree's corpus, paired with the
    // invocation that exercises it.
    let driven: Vec<(&str, Vec<&str>)> = vec![
        ("check", vec!["check"]),
        ("build", vec!["build"]),
        ("report", vec!["report"]),
        ("migrate", vec!["migrate"]),
        ("retarget", vec!["retarget", "old", "new"]),
        ("rename", vec!["rename", "docs/old.md", "docs/moved.md"]),
        ("lifecycle.review", vec!["lifecycle", "review", "old"]),
        (
            "lifecycle.set",
            vec!["lifecycle", "set", "old", "--status", "archived"],
        ),
        (
            "lifecycle.supersede",
            vec!["lifecycle", "supersede", "old", "--to", "new"],
        ),
        ("scaffold", scaffold.clone()),
    ];
    // Every other leaf, with why the working tree's boundary is not its
    // answer to give. A leaf that fits none of these has not been decided
    // about, which is what the assertion below refuses.
    let standing_outside: Vec<(&str, &str)> = vec![
        (
            "init",
            "writes a config into a directory that has no corpus yet",
        ),
        (
            "status",
            "probes the snapshot against the tree; divergence is its whole answer",
        ),
        (
            "diff",
            "graphs two ref checkouts, and names each ref's omissions per ref",
        ),
        (
            "impact",
            "graphs two ref checkouts, and names each ref's omissions per ref",
        ),
        (
            "export.schema",
            "renders the config's own vocabulary, never the corpus",
        ),
        (
            "export.enums",
            "renders the config's own vocabulary, never the corpus",
        ),
        (
            "export.rules",
            "renders the rule registry, never the corpus",
        ),
        (
            "export.envelope-schema",
            "renders the JSON contract, never the corpus",
        ),
        (
            "export.config",
            "renders the loaded config, never the corpus",
        ),
        (
            "export.commands",
            "renders the CLI grammar, never the corpus",
        ),
        (
            "export.diagnostics",
            "renders the code vocabularies, never the corpus",
        ),
    ];

    // Closed against the grammar, both directions.
    let manifest =
        envelope_of(nodex(std::env::current_dir().unwrap().as_path()).args(["export", "commands"]));
    let leaves: std::collections::BTreeSet<String> = manifest
        .pointer("/data/commands")
        .and_then(Value::as_array)
        .expect("commands manifest")
        .iter()
        .map(|entry| {
            entry["path"]
                .as_array()
                .expect("path")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(".")
        })
        .collect();
    let mut classified: std::collections::BTreeSet<String> =
        driven.iter().map(|(id, _)| id.to_string()).collect();
    classified.extend(standing_outside.iter().map(|(id, _)| id.to_string()));
    // `query *` reads `graph.json` and carries the membership-divergence
    // advisory instead — one decision covering the whole family.
    let unclassified: Vec<&String> = leaves
        .iter()
        .filter(|leaf| !leaf.starts_with("query.") && !classified.contains(*leaf))
        .collect();
    assert!(
        unclassified.is_empty(),
        "these leaves are neither driven here nor given a reason to stand outside: \
         {unclassified:?}"
    );
    let stale: Vec<&String> = classified
        .iter()
        .filter(|id| !leaves.contains(*id))
        .collect();
    assert!(stale.is_empty(), "these are not leaves any more: {stale:?}");

    for (id, command) in &driven {
        let (tmp, _outside) = fixture(true);
        let env = envelope_of(nodex(tmp.path()).args(command));
        assert!(
            names_the_boundary(&env),
            "{id} names the boundary it reasoned across: {env}"
        );
    }

    // The corpus is empty *because* the walk stopped at the link. A command
    // that reads "no nodes" as "nothing to report" would go silent exactly
    // where nothing else has told the operator yet.
    for command in [
        vec!["check"],
        vec!["build"],
        vec!["report"],
        vec!["migrate"],
        scaffold,
    ] {
        let (tmp, _outside) = fixture(false);
        let env = envelope_of(nodex(tmp.path()).args(&command));
        assert_eq!(
            run_envelope(nodex(tmp.path()).arg("build"))
                .pointer("/data/nodes")
                .and_then(Value::as_i64),
            Some(0),
            "the fixture's graph is empty"
        );
        assert!(
            names_the_boundary(&env),
            "{command:?} names the boundary that emptied the corpus: {env}"
        );
    }
}

/// Whether an envelope names the undescended link the walk stopped at.
#[cfg(unix)]
fn names_the_boundary(env: &Value) -> bool {
    env.get("warnings")
        .and_then(Value::as_array)
        .is_some_and(|ws| {
            ws.iter().any(|w| {
                w["code"] == "scope_coverage"
                    && w["message"]
                        .as_str()
                        .unwrap_or("")
                        .contains("not descended")
            })
        })
}
