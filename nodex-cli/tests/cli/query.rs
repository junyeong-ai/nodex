//! `query` leaves.

use crate::support::{init_project, nodex, run_envelope, run_json, scratch, write_doc};
use serde_json::Value;
use std::fs;

#[test]
fn similar_status_filters_candidates_without_changing_the_target() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    for (id, status) in [("live", "active"), ("past", "archived")] {
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!("---\nid: {id}\ntitle: Shared policy\nstatus: {status}\n---\n# Policy\n"),
        );
    }
    nodex(root).arg("build").assert().success();
    let all = run_json(nodex(root).args(["query", "similar", "--title", "Shared policy"]));
    assert_eq!(all["total"], 2);
    for (id, status) in [("live", "active"), ("past", "archived")] {
        let selected = run_json(nodex(root).args([
            "query",
            "similar",
            "--title",
            "Shared policy",
            "--status",
            status,
        ]));
        assert_eq!(selected["total"], 1);
        assert_eq!(selected["items"][0]["id"], id);
        assert_eq!(selected["items"][0]["score"], 1.0);
    }
    let rejected = nodex(root)
        .args([
            "query",
            "similar",
            "--title",
            "Shared policy",
            "--status",
            "unknown",
        ])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    let rejected: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(rejected["error"]["code"], "CONFIG_ERROR");
}

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
fn query_issues_returns_summary_shape() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "issues"]));
    let summary = data.get("summary").expect("summary key present");
    assert!(summary.get("total").is_some());
    assert!(summary.get("by_category").is_some());
}

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
fn query_covered_by_reads_a_covers_value_the_way_the_build_reads_it() {
    // `covers: ["./src/a.rs"]` in `docs/x.md` names `docs/src/a.rs` —
    // the marker says which directory, and the resolver honours it. The
    // reverse lookup folded the marker off the document's own value
    // before comparing, so it answered for a path that exists nowhere
    // and denied the file the reference actually names.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("docs/src")).unwrap();
    fs::write(root.join("docs/src/a.rs"), "fn main() {}\n").unwrap();
    write_doc(
        root,
        "docs/x.md",
        "---\nid: cov\ntitle: C\nkind: generic\nstatus: active\ncovers:\n  - \"./src/a.rs\"\n---\nb\n",
    );
    nodex(root).arg("build").assert().success();
    let total = |needle: &str| {
        run_json(nodex(root).args(["query", "covered-by", needle]))
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    assert_eq!(total("docs/src/a.rs"), 1, "the file the value names");
    assert_eq!(total("src/a.rs"), 0, "a path the project does not have");
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

/// Staleness asked of a project that declares no horizon is not tracked, and
/// an empty listing must not read as nothing stale: `query stale` says so on
/// its envelope and `GRAPH.md` says so in its section, while a declared
/// horizon lists the document and says nothing of the kind.
#[test]
fn staleness_asked_without_a_horizon_says_it_is_not_tracked() {
    let tmp = scratch();
    let root = tmp.path();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nstatus: active\nreviewed: 2020-01-01\n---\n# A\n",
    );
    let codes = |envelope: &Value| -> Vec<String> {
        envelope["warnings"]
            .as_array()
            .map(|warnings| {
                warnings
                    .iter()
                    .filter_map(|w| w["code"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let stale_section = || {
        let report = fs::read_to_string(root.join("_index/GRAPH.md")).expect("GRAPH.md");
        let section = report.split_once("## Stale\n").expect("a Stale section").1;
        let section = section.split("\n## ").next().unwrap_or_default();
        section
            .split("\n---")
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    for (config, listed, untracked, section) in [
        (
            "",
            0,
            true,
            "_Not tracked — `[detection].stale_days` is not set_",
        ),
        (
            "[detection]\nstale_days = 90\n",
            1,
            false,
            "- a — reviewed 2020-01-01 (2192 days ago)",
        ),
    ] {
        fs::write(root.join("nodex.toml"), config).unwrap();
        nodex(root).arg("build").assert().success();
        let envelope = run_envelope(nodex(root).args(["--today", "2026-01-01", "query", "stale"]));
        assert_eq!(envelope["data"]["total"], listed, "config {config:?}");
        assert_eq!(
            codes(&envelope).contains(&"threshold_undeclared".to_string()),
            untracked,
            "config {config:?}: {envelope}"
        );
        run_envelope(nodex(root).args(["--today", "2026-01-01", "report"]));
        assert_eq!(stale_section(), section, "config {config:?}");
    }
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
    assert_eq!(ids, ["doc-b"]);
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
fn query_components_partitions_disconnected_subgraphs() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n[b](b.md)\n",
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

    // A size bound counts and ranks against the whole partition.
    let data = run_json(nodex(tmp.path()).args(["query", "components", "--max-size", "1"]));
    assert_eq!(data["total"], 1, "{data}");
    assert_eq!(data["items"][0]["component_id"], 2, "{data}");
    assert_eq!(data["items"][0]["members"][0]["id"], "doc-c", "{data}");
}

#[test]
fn query_neighborhood_expands_by_depth() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n[b](b.md)\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n[c](c.md)\n",
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

/// Seed a tiny multi-kind, multi-status corpus that exercises every
/// `query nodes` predicate (kind / status / tag).
fn seed_listing_corpus(tmp: &std::path::Path) {
    init_project(tmp);
    // Allow an extra kind and the `draft` status so the corpus tests every
    // filter category against real graph state. Written by section rather
    // than by the vocabulary the template happens to ship, so a template
    // that gains a kind does not turn these into silent no-ops; `adr` comes
    // from the template, which `every_commented_init_template_example_loads_when_enabled`
    // holds it to.
    let cfg_path = tmp.join("nodex.toml");
    let content = fs::read_to_string(&cfg_path).expect("nodex.toml");
    let widen = |content: &str, section: &str, value: &str| {
        let opening = format!("[{section}]\nallowed = [");
        let widened = content.replace(&opening, &format!("{opening}{value}, "));
        assert_ne!(widened, *content, "the template must declare [{section}]");
        widened
    };
    let content = widen(&content, "kinds", "\"spec\"");
    let content = widen(&content, "statuses", "\"draft\"");
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
        vec!["query", "components", "--max-size", "0"],
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

#[test]
fn query_dependents_follows_the_reverse_chain_to_the_depth_asked() {
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

    let data = run_json(nodex(tmp.path()).args(["query", "dependents", "doc-a", "--depth", "2"]));
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
fn query_dependents_names_direct_dependents_unless_asked_for_more_hops() {
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

    let data = run_json(nodex(tmp.path()).args(["query", "dependents", "doc-a"]));
    assert_eq!(data["max_depth"], 1);
    let deps = data["dependents"].as_array().expect("dependents array");
    let ids: Vec<&str> = deps.iter().filter_map(|d| d["id"].as_str()).collect();
    assert_eq!(ids, vec!["doc-b"], "the default must exclude doc-c");
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

#[test]
fn similarity_filters_candidates_before_selection_without_metadata_boosts() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\", \"adr\", \"guide\"]\n",
    )
    .unwrap();
    for (path, id, kind, status, title) in [
        ("docs/target.md", "target", "adr", "active", "Shared policy"),
        ("docs/ended.md", "ended", "adr", "archived", "Shared policy"),
        (
            "guides/guide.md",
            "guide",
            "guide",
            "active",
            "Shared policy",
        ),
        (
            "docs/deep/nested.md",
            "nested",
            "adr",
            "active",
            "Shared policy",
        ),
        ("root.md", "root", "generic", "active", "Shared policy"),
        (
            "docs/unrelated.md",
            "unrelated",
            "adr",
            "active",
            "Salmon migration",
        ),
    ] {
        write_doc(
            root,
            path,
            &format!("---\nid: {id}\nkind: {kind}\nstatus: {status}\ntitle: {title}\n---\n"),
        );
    }
    nodex(root).arg("build").assert().success();
    let result = run_json(nodex(root).args([
        "query",
        "similar",
        "--title",
        "Shared policy",
        "--kind",
        "adr",
        "--status",
        "active",
        "--parent-dir",
        "./docs/deep/..",
        "--limit",
        "1",
    ]));
    assert_eq!(result["total"], 1);
    assert_eq!(result["items"][0]["id"], "target");
    assert_eq!(result["items"][0]["score"], 1.0);
    assert_eq!(
        result["items"][0]["components"],
        serde_json::json!({"title":1.0})
    );
    let cross_kind =
        run_json(nodex(root).args(["query", "similar", "--id", "target", "--kind", "guide"]));
    assert_eq!(cross_kind["total"], 1);
    assert_eq!(cross_kind["items"][0]["id"], "guide");
    assert_eq!(cross_kind["items"][0]["score"], 1.0);
    let at_root =
        run_json(nodex(root).args(["query", "similar", "--id", "target", "--parent-dir", "."]));
    assert_eq!(at_root["items"][0]["id"], "root");
    let no_overlap = run_json(nodex(root).args([
        "query",
        "similar",
        "--title",
        "Ocean currents",
        "--kind",
        "adr",
        "--parent-dir",
        "docs",
        "--min-score",
        "0",
    ]));
    assert_eq!(no_overlap["total"], 0);
    let excluded = nodex(root)
        .args([
            "query",
            "similar",
            "--id",
            "target",
            "--parent-dir",
            "../outside",
        ])
        .output()
        .unwrap();
    assert_eq!(excluded.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&excluded.stdout).unwrap()["error"]["code"],
        "PATH_ESCAPES_ROOT"
    );
}

#[test]
fn similarity_rejects_unused_target_tags_and_discloses_unrankable_queries() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\nkind: generic\ntitle: Policy\n---\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\nkind: adr\ntitle: Policy\n---\n",
    );
    nodex(root).arg("build").assert().success();
    let unused = nodex(root)
        .args(["query", "similar", "--id", "a", "--tags", "auth"])
        .output()
        .unwrap();
    assert_eq!(unused.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&unused.stdout).unwrap()["error"]["code"],
        "INVALID_ARGUMENT"
    );
    let absent =
        run_envelope(nodex(root).args(["query", "similar", "--title", "the and", "--kind", "adr"]));
    assert_eq!(absent["data"]["total"], 0);
    assert_eq!(
        absent["warnings"][0]["details"],
        serde_json::json!({"type":"ranking_unscored", "count":1, "reason":"target_without_signals"})
    );
}
