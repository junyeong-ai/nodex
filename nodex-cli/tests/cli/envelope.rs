//! The output contract every command shares: envelope shape, error codes
//! and exit statuses.

use crate::support::{
    git_runner, init_project, nodex, run_envelope, scratch, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;

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

#[test]
fn a_closed_stdout_exits_2_silently_whatever_the_verdict() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/bad.md",
        "---\nid: bad\ntitle: Bad\nkind: generic\nstatus: superseded\n---\nbody\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let open = nodex(tmp.path()).arg("check").output().expect("ran");
    assert_eq!(
        open.status.code(),
        Some(1),
        "the fixture must carry violations"
    );

    // The reader is gone before the binary writes a byte — what `head`
    // exiting early looks like to the writer, whatever the output's size.
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    #[expect(
        clippy::disallowed_methods,
        reason = "the binary under test, not a git invocation"
    )]
    let closed = std::process::Command::new(env!("CARGO_BIN_EXE_nodex"))
        .arg("-C")
        .arg(tmp.path())
        .arg("check")
        .stdout(writer)
        .output()
        .expect("ran");
    assert_eq!(
        closed.status.code(),
        Some(2),
        "stderr={}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert!(
        closed.stderr.is_empty(),
        "a reader closing the pipe is not an error to report: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_stdout_refusing_the_write_exits_2_and_says_why_on_stderr() {
    let tmp = scratch();
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full");
    #[expect(
        clippy::disallowed_methods,
        reason = "the binary under test, not a git invocation"
    )]
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nodex"))
        .arg("-C")
        .arg(tmp.path())
        .args(["export", "envelope-schema"])
        .stdout(full)
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        !output.stderr.is_empty(),
        "the failure must be named somewhere"
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
        ("report", vec!["report"]),
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
    for (path, id) in [("docs/a.md", "doc-a"), ("docs/c.md", "doc-c")] {
        write_doc(
            tmp.path(),
            path,
            &format!("---\nid: {id}\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n"),
        );
    }
    for preview in [
        ["rename", "docs/a.md", "docs/b.md", "--dry-run"],
        ["retarget", "doc-a", "doc-c", "--dry-run"],
    ] {
        let env = run_envelope(nodex(tmp.path()).args(preview));
        assert!(
            env.get("warnings")
                .and_then(Value::as_array)
                .is_some_and(|w| w
                    .iter()
                    .any(|x| warning_msg(x).is_some_and(|s| s.contains("meta.nodex_version")))),
            "{preview:?} must carry the advisory, not fail: {env}"
        );
    }

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

#[test]
fn a_zero_worker_pool_is_a_usage_error() {
    let tmp = scratch();
    let output = nodex(tmp.path())
        .args(["--jobs", "0", "query", "nodes"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
}
