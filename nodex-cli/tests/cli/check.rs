//! `check`: verdicts, severity, `--since` report scope, and the findings of
//! individual rules.

use crate::support::{git_runner, init_project, nodex, run_json, scratch, warning_msg, write_doc};
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

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
fn check_severity_narrows_the_report_and_never_the_verdict() {
    // `--severity` is presentation. A gate whose verdict follows what is
    // displayed answers for something other than what it checked, so
    // `--severity warning` over a project holding an error must still exit
    // 1 — and say what it is not showing.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [detection]\nstale_days = 1\norphan_ok_kinds = [\"generic\"]\n",
    )
    .unwrap();
    // kind out of vocab → Error-severity field_enum.
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: bogus\nstatus: active\norphan_ok: true\n---\n# A\n",
    );
    // a review far past the horizon → Warning-severity stale_review.
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\nreviewed: 2001-01-01\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let run = |args: &[&str]| -> (i32, Value) {
        let out = nodex(tmp.path()).args(args).output().expect("command ran");
        let stdout = String::from_utf8_lossy(&out.stdout);
        (
            out.status.code().unwrap_or(-1),
            serde_json::from_str(stdout.trim()).expect("stdout is parseable JSON"),
        )
    };
    let suppression = |env: &Value| -> Option<String> {
        env.get("warnings")
            .and_then(Value::as_array)?
            .iter()
            .find(|w| w.get("code").and_then(Value::as_str) == Some("gate_suppression"))
            .and_then(warning_msg)
            .map(str::to_string)
    };
    let severities = |env: &Value| -> Vec<String> {
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.get("severity").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };

    let (code, env) = run(&["check"]);
    assert_eq!(code, 1, "the unfiltered verdict");
    assert_eq!(env.pointer("/data/has_errors"), Some(&Value::Bool(true)));
    assert!(suppression(&env).is_none(), "nothing was hidden");

    let (code, env) = run(&["check", "--severity", "warning"]);
    assert_eq!(code, 1, "hiding the errors must not clear the verdict");
    assert_eq!(env.pointer("/data/has_errors"), Some(&Value::Bool(true)));
    assert_eq!(severities(&env), vec!["warning".to_string()]);
    assert!(
        suppression(&env).is_some_and(|m| m.contains("--severity warning hid 1 violation")),
        "the shown set must name what it is not showing: {env}"
    );

    let (code, env) = run(&["check", "--severity", "error"]);
    assert_eq!(code, 1, "the verdict is the same one either way");
    assert_eq!(severities(&env), vec!["error".to_string()]);
    assert!(
        suppression(&env).is_some_and(|m| m.contains("--severity error hid 1 violation")),
        "suppression is symmetric — hiding warnings is hiding too: {env}"
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
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nSee [gone](missing.md) and [excluded](../specs/x/sub.md).\n",
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
        by_target.get("missing.md").copied(),
        Some(("missing", "warning", None)),
        "truly absent target must be `missing` — the unattributed warning fallthrough; \
         got {by_target:?}"
    );
    assert_eq!(
        by_target.get("../specs/x/sub.md").copied(),
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
        by_category
            .get("violation_unresolved_reference")
            .and_then(Value::as_u64),
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

/// A successor that does not exist and a citation of the same purged record
/// fail for the same reason and name the same target. Only the relation tells
/// them apart, so only a relation-narrowed row can gate the first while the
/// second stays the point-in-time record it is.
#[test]
fn a_relation_narrowed_policy_row_gates_only_the_structural_edge() {
    let tmp = scratch();
    let project = tmp.path();
    fs::write(
        project.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
[statuses]
allowed = ["active", "superseded"]
terminal = ["superseded"]
[detection]
orphan_ok_kinds = ["generic"]
[parser]
wikilink_enabled = true
[[detection.unresolved_policy]]
name = "dead-successor"
cause = "id_not_found"
relations = ["superseded_by"]
severity = "error"
[[detection.unresolved_policy]]
name = "point-in-time"
cause = "missing"
severity = "info"
"#,
    )
    .unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: superseded\n\
         superseded_by: doc-purged\n---\n# A\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n\
         # B\n\nsuperseded by [[doc-purged]] at the time\n",
    );
    nodex(project).arg("build").assert().success();

    let data = run_json(nodex(project).args(["query", "issues"]));
    let severities: std::collections::BTreeMap<&str, &str> = data
        .get("unresolved_edges")
        .and_then(Value::as_array)
        .expect("unresolved_edges")
        .iter()
        .filter_map(|e| {
            Some((
                e.get("relation").and_then(Value::as_str)?,
                e.get("severity").and_then(Value::as_str)?,
            ))
        })
        .collect();
    assert_eq!(severities.get("superseded_by").copied(), Some("error"));
    assert_eq!(severities.get("references").copied(), Some("info"));

    nodex(project).arg("check").assert().code(1);
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
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n\nSee [spec](../specs/x.md) and [gone](missing.md).\n",
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
        by_target.get("../specs/x.md").copied(),
        Some(("info", Some("ephemeral-specs"))),
        "the declared info row classifies the specs link: {by_target:?}"
    );
    assert_eq!(
        by_target.get("missing.md").copied(),
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
        by_category
            .get("violation_unresolved_reference")
            .and_then(Value::as_u64),
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
fn schema_forbidden_reds_a_kind_carrying_the_field_across_check_and_the_write_gate() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
[kinds]
allowed = ["generic", "learning", "runbook"]
[[identity.kind_rules]]
glob = "docs/learnings/**"
kind = "learning"
[[identity.kind_rules]]
glob = "docs/runbooks/**"
kind = "runbook"
[[schema.overrides]]
kinds = ["learning"]
forbidden = ["covers"]
"#,
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "").unwrap();
    let runbook = "---\ntitle: Deploy\nstatus: active\ncovers: [src/lib.rs]\n---\n# Deploy\n";
    let learning = "---\ntitle: Cache\nstatus: active\n---\n# Cache\n";
    write_doc(root, "docs/runbooks/deploy.md", runbook);
    write_doc(root, "docs/learnings/cache.md", learning);
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();

    let proposed = "---\ntitle: Cache\nstatus: active\ncovers: [src/lib.rs]\n---\n# Cache\n";
    let forbidden_on = |out: &assert_cmd::assert::Assert| -> Vec<String> {
        let env: Value =
            serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter(|v| v.get("rule_id").and_then(Value::as_str) == Some("forbidden_field"))
            .filter_map(|v| v.get("path").and_then(Value::as_str).map(str::to_string))
            .collect()
    };

    let gate = nodex(root)
        .args(["check", "--content", "docs/learnings/cache.md=-"])
        .write_stdin(proposed)
        .assert()
        .failure()
        .code(1);
    assert_eq!(forbidden_on(&gate), vec!["docs/learnings/cache.md"]);

    write_doc(root, "docs/learnings/cache.md", proposed);
    nodex(root).arg("build").assert().success();
    let check = nodex(root).arg("check").assert().failure().code(1);
    assert_eq!(forbidden_on(&check), vec!["docs/learnings/cache.md"]);

    let data = run_json(nodex(root).args(["export", "rules"]));
    let rule = data["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "forbidden_field")
        .expect("forbidden_field is registered");
    assert_eq!(
        rule["params"]["per_kind"],
        serde_json::json!({ "learning": ["covers"] })
    );
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

/// `--since` reports the orphan a diff made and not the ones it did not
/// touch. Orphanhood is decided by other documents' edges, so the edit
/// that strands `leaf` is on `hub`; the diff answers for both — the
/// record that moved and the document its removed edge pointed at — and
/// for neither of the standing orphans the pull request never reached.
#[test]
fn since_reports_the_orphan_a_diff_made_and_not_the_standing_ones() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/hub.md",
        "---\nid: generic-hub\ntitle: Hub\nkind: generic\nstatus: active\n---\n[leaf](leaf.md)\n",
    );
    write_doc(
        root,
        "docs/leaf.md",
        "---\nid: generic-leaf\ntitle: Leaf\nkind: generic\nstatus: active\n---\n# Leaf\n",
    );
    write_doc(
        root,
        "docs/standing.md",
        "---\nid: generic-standing\ntitle: Standing\nkind: generic\nstatus: active\n---\n# Standing\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    write_doc(
        root,
        "docs/hub.md",
        "---\nid: generic-hub\ntitle: Hub\nkind: generic\nstatus: active\n---\nno link\n",
    );
    nodex(root).arg("build").assert().success();

    let orphans = |data: &Value| -> Vec<String> {
        data["violations"]
            .as_array()
            .expect("violations")
            .iter()
            .filter(|v| v["rule_id"] == "orphan")
            .map(|v| v["node_id"].as_str().expect("attributed").to_string())
            .collect()
    };
    let whole = run_json(nodex(root).arg("check"));
    assert_eq!(
        orphans(&whole),
        ["generic-hub", "generic-leaf", "generic-standing"],
        "the whole project: every orphan, {whole}"
    );
    let since = run_json(nodex(root).args(["check", "--since", "HEAD"]));
    assert_eq!(
        orphans(&since),
        ["generic-hub", "generic-leaf"],
        "the diff's: the record that moved and the document it stranded, {since}"
    );
    let coverage = |data: &Value| {
        data["rule_coverage"]
            .as_array()
            .expect("coverage")
            .iter()
            .find(|c| c["rule_id"] == "orphan")
            .map(|c| c["subjects"].as_u64().expect("subjects"))
            .expect("orphan ran")
    };
    assert_eq!(
        coverage(&since),
        coverage(&whole),
        "the reach is never narrowed"
    );
}

/// A move that keeps a document's id changes nothing authored and
/// changes what its filename must match, so the diff names it and
/// `--since` keeps the `filename_pattern` finding the move created —
/// an Error the whole-project gate reds, which a narrowed gate must
/// not pass.
#[test]
fn since_keeps_the_filename_finding_a_move_created() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^[a-z0-9-]+\\\\.md$\"\n\
         [detection]\norphan_ok_kinds = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/good-name.md",
        "---\nid: generic-good\ntitle: Good\nkind: generic\nstatus: active\n---\n# Good\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["mv", "docs/good-name.md", "docs/BAD_Name.md"]);
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["check", "--since", "HEAD"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let rules: Vec<&str> = envelope["data"]["violations"]
        .as_array()
        .expect("violations")
        .iter()
        .map(|v| v["rule_id"].as_str().expect("rule id"))
        .collect();
    assert_eq!(
        rules,
        ["filename_pattern"],
        "the move is the diff's, so its finding is: {envelope}"
    );
    assert_eq!(envelope["data"]["has_errors"], true);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a narrowed gate reds what the whole one reds"
    );
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
fn gate_suppression_counts_what_the_envelope_stops_carrying() {
    // The code means "there is a finding you cannot see", so it must fire for
    // a finding that left the envelope and stay quiet for one still in it.
    // `--content` reports the proposed documents' Warning-severity findings in
    // `standing` whatever `--severity` says, so both cases have to be asked of
    // one `--content` run: a proposal numbered into a gap introduces a
    // sequential_numbering warning on its own path, which `standing` keeps,
    // and another on the document above it, which nothing else carries. One
    // count discriminates both errors — 0 means the disclosure was switched
    // off, 2 means `standing` was not consulted.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [detection]\norphan_ok_kinds = [\"generic\"]\n\
         [[rules.naming]]\nglob = \"docs/*.md\"\npattern = \"^[0-9]{4}\\\\.md$\"\n\
         sequential = true\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/0001.md",
        "---\nid: d-1\ntitle: One\nkind: generic\nstatus: active\n---\n# 1\n",
    );
    write_doc(
        root,
        "docs/0009.md",
        "---\nid: d-9\ntitle: Nine\nkind: generic\nstatus: active\n---\n# 9\n",
    );
    nodex(root).arg("build").assert().success();

    let out = nodex(root)
        .args([
            "check",
            "--content",
            "docs/0005.md=-",
            "--severity",
            "error",
        ])
        .write_stdin("---\nid: d-5\ntitle: Five\nkind: generic\nstatus: active\n---\n# 5\n")
        .output()
        .expect("command ran");
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("JSON");

    let standing: Vec<&str> = env
        .pointer("/data/standing")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.get("path").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        standing,
        vec!["docs/0005.md"],
        "the proposal's own warning stays in the envelope: {env}"
    );

    let suppression = env
        .get("warnings")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find(|w| w.get("code").and_then(Value::as_str) == Some("gate_suppression"))
        })
        .and_then(warning_msg)
        .unwrap_or_default();
    assert!(
        suppression.contains("hid 1 violation"),
        "exactly the one finding nothing else carries: {env}"
    );

    // The other direction, where value membership and path membership come
    // apart: an Error hidden by `--severity warning` sits on a path `standing`
    // also names, and `standing` carries no Errors. Asked only of
    // `--severity error`, the two predicates agree and a path-keyed lookup
    // would go on passing while silencing this.
    write_doc(
        root,
        "docs/0007.md",
        "---\nid: d-7\ntitle: Seven\nkind: generic\nstatus: active\nreviewed: 2001-01-01\n---\n# 7\n",
    );
    nodex(root).arg("build").assert().success();
    let out = nodex(root)
        .args([
            "check",
            "--content",
            "docs/0007.md=-",
            "--severity",
            "warning",
        ])
        .write_stdin("---\nid: d-7\ntitle: Seven\nkind: bogus\nstatus: active\nreviewed: 2001-01-01\n---\n# 7\n")
        .output()
        .expect("command ran");
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("JSON");
    assert_eq!(
        env.pointer("/data/standing")
            .and_then(Value::as_array)
            .map(|a| a
                .iter()
                .filter_map(|v| v.get("path").and_then(Value::as_str))
                .any(|p| p == "docs/0007.md")),
        Some(true),
        "standing names the same path the hidden Error sits on: {env}"
    );
    let suppression = env
        .get("warnings")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find(|w| w.get("code").and_then(Value::as_str) == Some("gate_suppression"))
        })
        .and_then(warning_msg)
        .unwrap_or_default();
    assert!(
        suppression.contains("hid 1 violation"),
        "an Error is not in standing however the path matches: {env}"
    );
}

#[test]
fn only_a_working_tree_check_keeps_the_drift_history() {
    // `check --content` is the read-only gate: it counts drift from the
    // history a working-tree `check` kept plus the commits since, and writes
    // nothing — a commit later, the kept file is byte-for-byte what it was
    // until the next working-tree `check` keeps the new head.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    assert!(git(&["init", "-q"]).status.success());
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [detection]\ngit_drift_threshold = 1\n",
    )
    .unwrap();
    let doc = "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n\
               reviewed: 2020-01-01\ncovers:\n  - \"src/a.rs\"\n---\n# D\n";
    write_doc(root, "docs/d.md", doc);
    write_doc(root, "src/a.rs", "// 0\n");
    assert!(git(&["add", "-A"]).status.success());
    assert!(git(&["commit", "-q", "-m", "base"]).status.success());
    let head = || {
        String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_owned()
    };
    let kept = root.join("_index/history.json");
    let kept_head = || {
        serde_json::from_str::<Value>(&fs::read_to_string(&kept).unwrap()).unwrap()["head"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    nodex(root).arg("check").assert().success();
    assert_eq!(kept_head(), head());
    let before = fs::read(&kept).unwrap();

    write_doc(root, "src/a.rs", "// 1\n");
    assert!(git(&["commit", "-qam", "covered"]).status.success());
    let gate = run_json(
        nodex(root)
            .args(["check", "--content", "docs/d.md=-"])
            .write_stdin(doc),
    );
    let drift: Vec<u64> = gate["standing"]
        .as_array()
        .expect("standing")
        .iter()
        .filter(|v| v["rule_id"] == "git_drift")
        .filter_map(|v| v.pointer("/details/total_commits").and_then(Value::as_u64))
        .collect();
    assert_eq!(drift, [2], "the kept commit and the one since");
    assert_eq!(
        fs::read(&kept).unwrap(),
        before,
        "check --content must not write the kept history"
    );

    nodex(root).arg("check").assert().success();
    assert_eq!(
        kept_head(),
        head(),
        "a working-tree check keeps the new head"
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
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\norphan_ok: true\n---\n\n- **scope**: ok\n- **bogus**: typo\n",
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

#[test]
fn ai_unresolved_grammars_cannot_hide_each_others_policy_findings() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_doc(
        root,
        "nodex.toml",
        r#"[scope]
include=["docs/**/*.md"]
[parser]
wikilink_enabled=true
[[detection.unresolved_policy]]
name="root"
cause="missing"
glob="missing.md"
severity="error"
[[detection.unresolved_policy]]
name="local"
cause="missing"
glob="docs/missing.md"
severity="info"
"#,
    );
    for body in [
        "[local](missing.md)\n[[missing.md]]",
        "[[missing.md]]\n[local](missing.md)",
    ] {
        write_doc(
            root,
            "docs/a.md",
            &format!("---\nid: a\ntitle: A\n---\n{body}\n"),
        );
        let output = nodex(root).arg("check").output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let check: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            check["data"]["violations"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["rule_id"] == "unresolved_reference/root")
                .count(),
            1
        );
        nodex(root).arg("build").assert().success();
        let issues = run_json(nodex(root).args(["query", "issues"]));
        assert_eq!(issues["unresolved_edges"].as_array().unwrap().len(), 2);
    }
}
