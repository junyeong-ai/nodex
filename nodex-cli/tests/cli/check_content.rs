//! `check --content`: proposed content judged as the write it would be.

use crate::support::{nodex, run_envelope, run_json, scratch, warning_msg, write_doc};
use serde_json::Value;
use std::fs;

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
fn check_content_batch_invocation_guards_are_typed_argument_errors() {
    // The three write-seam guards return INVALID_ARGUMENT, never a panic or a
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
    assert_eq!(code_of(&out), "INVALID_ARGUMENT");

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
    assert_eq!(code_of(&out), "INVALID_ARGUMENT");

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
    assert_eq!(code_of(&out), "INVALID_ARGUMENT");
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
        ["orphan", "unresolved_reference/outside"],
        "the probe answered about the project the proposal produces: {env}"
    );
}

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
fn check_content_severity_filter_never_clears_the_gate() {
    // `--severity` composes with `--content` exactly as with any other
    // check mode — as presentation. This is where a moved verdict costs
    // the most: an agent gating a write on the filtered view would be told
    // its edit is clean while the lock it broke sits behind the filter, so
    // both the exit code and the per-proposal `has_path_errors` answer for
    // everything checked.
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

    for filter in [None, Some("error"), Some("warning")] {
        let mut args = vec!["check", "--content", "docs/d.md=-"];
        if let Some(f) = filter {
            args.extend(["--severity", f]);
        }
        let out = nodex(root)
            .args(&args)
            .write_stdin(tampered)
            .output()
            .expect("command ran");
        assert_eq!(
            out.status.code(),
            Some(1),
            "the frozen body is broken whatever is displayed: {filter:?}"
        );
        let env: Value =
            serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("JSON");
        assert_eq!(
            env.pointer("/data/proposals/0/has_path_errors"),
            Some(&Value::Bool(true)),
            "the per-proposal verdict answers for the proposal, not the view: {filter:?}"
        );
    }
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
