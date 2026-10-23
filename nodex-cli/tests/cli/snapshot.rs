//! Snapshot freshness: `status`, and what queries answer from a graph that
//! no longer matches the working tree.

use crate::support::{
    error_code, init_project, nodex, run_envelope, run_json, scratch, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;

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
        .args(["query", "--allow-stale", "node", "no-such-id"])
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
    let hit = run_envelope(nodex(project).args(["query", "--allow-stale", "node", "generic-a"]));

    restore(&sub);
    let repaired = nodex(project)
        .args(["query", "--allow-stale", "node", "no-such-id"])
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

#[test]
#[cfg(unix)]
fn a_missed_lookup_preserves_file_read_errors_before_and_after_rebuilding() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    write_doc(root, "docs/a.md", "---\nid: a\n---\nBody\n");
    nodex(root).arg("build").assert().success();
    let path = root.join("docs/a.md");
    let permissions = fs::metadata(&path).unwrap().permissions();
    let miss = || {
        let output = nodex(root)
            .args(["query", "--allow-stale", "node", "missing"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let before = miss();
    let hit = run_envelope(nodex(root).args(["query", "--allow-stale", "node", "a"]));
    let status = run_json(nodex(root).arg("status"));
    nodex(root).arg("build").assert().success();
    let after = miss();
    fs::set_permissions(&path, permissions).unwrap();
    let repaired = miss();
    nodex(root).arg("build").assert().success();
    let current = miss();
    for error in [before, after] {
        assert_eq!(error["ok"], false);
        assert_eq!(error["error"]["code"], "IO_ERROR");
        assert!(error["error"]["message"].as_str().unwrap().contains("a.md"));
    }
    assert_eq!(hit["data"]["node"]["id"], "a");
    assert_eq!(status["state"], "outdated");
    assert_eq!(repaired["error"]["code"], "GRAPH_OUTDATED");
    assert_eq!(current["error"]["code"], "NOT_FOUND");
}

#[test]
fn query_node_with_body_rejects_changed_document_revision() {
    let tmp = scratch();
    init_project(tmp.path());
    let original = "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();

    for changed in [
        original.replace("# A", "# Changed"),
        original.replace("title: A", "title: Changed"),
        original.replace("id: doc-a", "id: doc-new"),
        original.replace('\n', "\r\n"),
    ] {
        write_doc(tmp.path(), "docs/a.md", &changed);
        let output = nodex(tmp.path())
            .args(["query", "node", "doc-a", "--with-body"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["code"], "GRAPH_OUTDATED");
    }

    write_doc(tmp.path(), "docs/a.md", original);
    let data = run_json(nodex(tmp.path()).args(["query", "node", "doc-a", "--with-body"]));
    assert_eq!(data["body"], "# A\n");
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
        .args(["query", "--allow-stale", "node", "doc-a", "--with-body"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let env: Value = serde_json::from_slice(&output.stdout).expect("JSON envelope");
    assert_eq!(env["error"]["code"], "IO_ERROR");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&tmp.path().join("docs/a.md").display().to_string()),
        "read failure must identify the affected file: {env}"
    );
}

#[test]
fn queries_reject_incompatible_snapshots_without_writes_and_build_recovers() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/a.md", "---\nid: doc-a\n---\n[B](b.md)\n");
    write_doc(tmp.path(), "docs/b.md", "---\nid: doc-b\n---\n# B\n");
    nodex(tmp.path()).arg("build").assert().success();

    let graph_path = tmp.path().join("_index/graph.json");
    let original: Value = serde_json::from_str(&fs::read_to_string(&graph_path).unwrap()).unwrap();
    let cache_path = tmp.path().join("_index/cache.json");
    let cache = fs::read(&cache_path).unwrap();
    let version = nodex_core::model::graph::SCHEMA_VERSION;

    for foreign_version in [version - 1, version + 1] {
        let mut foreign = original.clone();
        foreign["schema_version"] = foreign_version.into();
        for edge in foreign["edges"].as_array_mut().unwrap() {
            edge.as_object_mut().unwrap().remove("references");
        }
        let content = serde_json::to_vec(&foreign).unwrap();
        fs::write(&graph_path, &content).unwrap();

        let status = run_json(nodex(tmp.path()).arg("status"));
        assert_eq!(status["state"], "schema_mismatch");
        assert_eq!(status["snapshot_schema_version"], foreign_version);

        for args in [
            vec!["query", "nodes"],
            vec!["query", "--allow-stale", "nodes"],
            vec!["query", "issues"],
            vec!["query", "chain", "doc-a"],
        ] {
            let output = nodex(tmp.path()).args(&args).assert().code(2);
            let envelope: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
            assert_eq!(envelope["ok"], false, "{args:?}: {envelope}");
            assert_eq!(envelope["error"]["code"], "GRAPH_OUTDATED");
            assert_eq!(fs::read(&graph_path).unwrap(), content);
            assert_eq!(fs::read(&cache_path).unwrap(), cache);
        }

        nodex(tmp.path()).arg("build").assert().success();
        let status = run_json(nodex(tmp.path()).arg("status"));
        assert_eq!(status["state"], "current");
        assert_eq!(status["snapshot_schema_version"], version);
        let backlinks = run_json(nodex(tmp.path()).args(["query", "backlinks", "doc-b"]));
        assert_eq!(backlinks["total"], 1);
    }
}

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
    let envelope = run_envelope(nodex(tmp.path()).args(["query", "--allow-stale", "nodes"]));
    assert!(envelope.get("warnings").is_none(), "{envelope}");

    // Unbuilt new doc → the query still succeeds, with one advisory.
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\n---\n# B\n",
    );
    let envelope = run_envelope(nodex(tmp.path()).args(["query", "--allow-stale", "nodes"]));
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
fn queries_can_require_current_content_and_reject_drift_without_writing() {
    let tmp = scratch();
    init_project(tmp.path());
    let original = "---\nid: a\ntitle: Storage\nstatus: active\n---\nOriginal\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();
    let snapshot_path = tmp.path().join("_index/graph.json");
    let snapshot = fs::read(&snapshot_path).unwrap();
    let current = run_envelope(nodex(tmp.path()).args(["query", "nodes"]));
    assert_eq!(current["snapshot"]["verification"], "content");
    assert_eq!(current["snapshot"]["unbuildable"], 0);
    write_doc(
        tmp.path(),
        "docs/a.md",
        &original.replace("status: active", "status: archived"),
    );
    let held = run_envelope(nodex(tmp.path()).args([
        "query",
        "--allow-stale",
        "nodes",
        "--status",
        "active",
    ]));
    assert_eq!(held["data"]["total"], 1);
    assert_eq!(held["snapshot"]["verification"], "membership");
    for args in [
        vec!["query", "nodes"],
        vec!["query", "backlinks", "a"],
        vec!["query", "search", "Storage"],
    ] {
        let output = nodex(tmp.path()).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["error"]["code"], "GRAPH_OUTDATED");
    }
    assert_eq!(fs::read(snapshot_path).unwrap(), snapshot);
}

#[test]
fn nodes_body_contains_reads_the_governed_bodies_at_their_indexed_revision() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/live.md",
        "---\nid: live\nstatus: active\n---\nTraces go to LANGFUSE.\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: old\nstatus: superseded\nsuperseded_by: live\n---\nlangfuse, first cut\n",
    );
    write_doc(
        root,
        "docs/tagged.md",
        "---\nid: tagged\nstatus: active\ntags: [langfuse]\n---\nNo mention here.\n",
    );
    write_doc(root, "notes/outside.md", "langfuse outside the scope\n");
    nodex(root).arg("build").assert().success();

    let ids = |extra: &[&str]| -> Vec<String> {
        run_json(
            nodex(root)
                .args(["query", "nodes", "--body-contains", "Langfuse"])
                .args(extra),
        )["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["id"].as_str().expect("id").to_owned())
            .collect()
    };
    assert_eq!(
        ids(&[]),
        ["live", "old"],
        "body text of governed documents only"
    );
    assert_eq!(ids(&["--status", "active"]), ["live"]);

    assert_eq!(
        error_code(nodex(root).args(["query", "nodes", "--body-contains", ""])),
        "CONFIG_ERROR"
    );

    write_doc(
        root,
        "docs/live.md",
        "---\nid: live\nstatus: active\n---\nTraces moved elsewhere.\n",
    );
    assert_eq!(
        error_code(nodex(root).args(["query", "nodes", "--allow-stale", "--body-contains", "x"])),
        "GRAPH_OUTDATED",
        "a document changed since the build is not searched as it now reads"
    );
}

#[test]
fn strict_query_discloses_unbuildable_documents_separately_from_freshness() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/a.md", "---\nid: a\n---\nBody\n");
    write_doc(tmp.path(), "docs/bad.md", "---\nid: [\n---\nBody\n");
    nodex(tmp.path()).arg("build").assert().success();
    let answer = run_envelope(nodex(tmp.path()).args(["query", "nodes"]));
    assert_eq!(answer["snapshot"]["verification"], "content");
    assert_eq!(answer["snapshot"]["unbuildable"], 1);
    assert_eq!(answer["data"]["total"], 1);
}
