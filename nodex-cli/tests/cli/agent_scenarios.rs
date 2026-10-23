//! Agent workflows that cross several commands, each asserted as one
//! scenario.

use crate::support::{
    error_code, git_runner, init_project, nodex, run_envelope, run_json, scratch, write_doc,
};
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

#[test]
fn ai_markdown_reference_identity_survives_namesakes_and_moves() {
    let tmp = scratch();
    let root = tmp.path();
    write_doc(root, "nodex.toml", "[scope]\ninclude = [\"**/*.md\"]\n");
    write_doc(root, "guide.md", "---\nid: root-guide\n---\n# Root\n");
    write_doc(
        root,
        "docs/guide.md",
        "---\nid: local-guide\n---\n# Local\n",
    );
    let source = "---\nid: start\n---\n[implicit](guide.md#local)\n[explicit](./guide.md)\n";
    write_doc(root, "docs/start.md", source);
    nodex(root).arg("build").assert().success();
    let detail = run_json(nodex(root).args(["query", "node", "start"]));
    assert_eq!(detail["outgoing"].as_array().unwrap().len(), 1);
    assert_eq!(detail["outgoing"][0]["target"], "local-guide");
    assert_eq!(detail["outgoing"][0]["references"][0]["syntax"], "markdown");
    nodex(root)
        .args(["rename", "guide.md", "renamed.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(root.join("docs/start.md")).unwrap(),
        source
    );
    nodex(root)
        .args(["rename", "docs/guide.md", "guides/current.md"])
        .assert()
        .success();
    let changed = fs::read_to_string(root.join("docs/start.md")).unwrap();
    assert!(changed.contains("../guides/current.md#local"));
    nodex(root).arg("build").assert().success();
    let detail = run_json(nodex(root).args([
        "query",
        "node",
        "start",
        "--with-body",
        "--body-lines",
        "4:4",
    ]));
    assert_eq!(
        detail["body_range"],
        serde_json::json!({ "start": 4, "end": 4, "body_start": 4, "body_end": 5 })
    );
    assert!(!detail["body"].as_str().unwrap().contains("[explicit]"));
    assert!(detail["node"].get("body_lines_hash").is_none());
    assert_eq!(
        error_code(nodex(root).args([
            "query",
            "node",
            "start",
            "--with-body",
            "--body-lines",
            "3:4"
        ])),
        "CONFIG_ERROR",
        "a range reaching into the frontmatter is refused"
    );
}

#[test]
fn ai_replacement_judges_lifecycle_and_all_reference_syntaxes_together() {
    let tmp = scratch();
    let root = tmp.path();
    write_doc(
        root,
        "nodex.toml",
        "[scope]\ninclude = [\"**/*.md\"]\n[parser]\nwikilink_enabled = true\n[rules.severity]\nsuperseded_reference = \"error\"\n",
    );
    write_doc(
        root,
        "decisions/old.md",
        "---\nid: old\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        root,
        "decisions/new.md",
        "---\nid: new\nstatus: active\n---\n# New\n",
    );
    write_doc(
        root,
        "guides/use.md",
        "---\nid: use\nrelated: [old]\n---\n[decision](../decisions/old.md#choice)\n[[old]]\n",
    );
    nodex(root)
        .args(["lifecycle", "supersede", "old", "--to", "new"])
        .assert()
        .code(2);
    assert!(
        !fs::read_to_string(root.join("decisions/old.md"))
            .unwrap()
            .contains("superseded_by")
    );
    nodex(root)
        .args(["retarget", "old", "new", "--supersede"])
        .assert()
        .success();
    let old = fs::read_to_string(root.join("decisions/old.md")).unwrap();
    assert!(old.contains("superseded_by: new") || old.contains("superseded_by: \"new\""));
    let guide = fs::read_to_string(root.join("guides/use.md")).unwrap();
    assert!(guide.contains("../decisions/new.md#choice"), "{guide}");
    assert!(guide.contains("[[new]]"), "{guide}");
    let checked = run_json(nodex(root).arg("check"));
    assert_eq!(checked["has_errors"], false);
}

#[test]
fn ai_node_reads_are_bounded_by_requested_information() {
    let tmp = scratch();
    let root = tmp.path();
    write_doc(
        root,
        "nodex.toml",
        "[scope]\ninclude = [\"**/*.md\"]\n[detection]\nstale_days = 30\n",
    );
    write_doc(
        root,
        "long.md",
        &format!(
            "---\nid: long\ncreated: 2020-01-01\n---\n{}",
            "A body line.\n".repeat(2000)
        ),
    );
    nodex(root).arg("build").assert().success();
    let detail = run_json(nodex(root).args(["query", "node", "long"]));
    assert!(serde_json::to_vec(&detail).unwrap().len() < 2000);
    assert!(detail["node"].get("body_structure").is_none());
    let missing =
        run_json(nodex(root).args(["query", "nodes", "--missing", "reviewed", "--fields", "id"]));
    assert_eq!(missing["items"][0]["id"], "long");
    let stale = run_json(nodex(root).args(["query", "stale"]));
    assert_eq!(stale["total"], 0);
    assert_eq!(stale["subjects"], 0);
    assert_eq!(stale["unjudged"], 1);
    nodex(root)
        .args([
            "query",
            "node",
            "long",
            "--with-body",
            "--body-lines",
            "0:1",
        ])
        .assert()
        .code(2);
}

#[test]
fn ai_queries_verify_content_unless_snapshot_reads_are_explicit() {
    let tmp = scratch();
    let root = tmp.path();
    write_doc(root, "nodex.toml", "[scope]\ninclude = [\"**/*.md\"]\n");
    write_doc(root, "a.md", "---\nid: a\nrelated: [b]\n---\n# A\n");
    write_doc(root, "b.md", "---\nid: b\n---\n# B\n");
    nodex(root).arg("build").assert().success();
    write_doc(root, "a.md", "---\nid: a\n---\n# A\n");
    let output = nodex(root)
        .args(["query", "backlinks", "b"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let refused: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(refused["error"]["code"], "GRAPH_OUTDATED");
    let old = run_envelope(nodex(root).args(["query", "--allow-stale", "backlinks", "b"]));
    assert_eq!(old["data"]["total"], 1);
    assert_eq!(old["snapshot"]["verification"], "membership");
    nodex(root).arg("build").assert().success();
    assert_eq!(
        run_json(nodex(root).args(["query", "backlinks", "b"]))["total"],
        0
    );
}

#[test]
fn ai_init_supports_repeated_readmes_and_migrated_identity_survives_move() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    write_doc(root, "apps/a/README.md", "# A\n");
    write_doc(root, "apps/b/README.md", "# B\n");
    nodex(root).arg("build").assert().success();
    let nodes = run_json(nodex(root).args(["query", "nodes"]));
    let items = nodes["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_ne!(items[0]["id"], items[1]["id"]);
    let id = items
        .iter()
        .find(|node| node["path"] == "apps/a/README.md")
        .unwrap()["id"]
        .clone();
    nodex(root).args(["migrate", "--apply"]).assert().success();
    nodex(root)
        .args(["rename", "apps/a/README.md", "apps/a/overview.md"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    let moved = run_json(nodex(root).args(["query", "node", "--path", "apps/a/overview.md"]));
    assert_eq!(moved["node"]["id"], id);
}

#[test]
fn ai_similarity_compares_tag_case_consistently_with_listings() {
    let tmp = scratch();
    let root = tmp.path();
    write_doc(
        root,
        "nodex.toml",
        "[scope]\ninclude = [\"**/*.md\"]\n[similarity.weights]\ntitle=0.0\ntags=1.0\nlinked=0.0\n",
    );
    write_doc(root, "a.md", "---\nid: a\ntags: [Auth]\n---\n# Policy\n");
    nodex(root).arg("build").assert().success();
    assert_eq!(
        run_json(nodex(root).args(["query", "nodes", "--tag", "auth"]))["total"],
        1
    );
    let similar =
        run_json(nodex(root).args(["query", "similar", "--title", "Policy", "--tags", "auth"]));
    assert_eq!(similar["items"][0]["components"]["tags"], 1.0);
    assert_eq!(similar["items"][0]["score"], 1.0);
}

#[test]
fn ai_independent_kind_flows_share_history_without_sharing_transitions() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    assert!(git(&["init", "-q"]).status.success());
    let config = r#"
[scope]
include = ["docs/**/*.md"]
[kinds]
allowed = ["generic", "adr", "requirement"]
[statuses]
allowed = ["proposed", "active", "draft", "agreed", "archived"]
terminal = ["archived"]
initial = "active"
[[statuses.flow]]
kinds = ["adr"]
initial = "proposed"
transitions = { proposed = ["active"], active = ["archived"] }
[[statuses.flow]]
kinds = ["requirement"]
initial = "draft"
transitions = { draft = ["agreed"], agreed = ["archived"] }
"#;
    write_doc(root, "nodex.toml", config);
    nodex(root)
        .args([
            "scaffold",
            "--kind",
            "adr",
            "--title",
            "Decision",
            "--id",
            "decision",
            "--path",
            "docs/decision.md",
        ])
        .assert()
        .success();
    nodex(root)
        .args([
            "scaffold",
            "--kind",
            "requirement",
            "--title",
            "Request",
            "--id",
            "request",
            "--path",
            "docs/request.md",
        ])
        .assert()
        .success();
    assert!(git(&["add", "nodex.toml", "docs"]).status.success());
    assert!(git(&["commit", "-qm", "entry records"]).status.success());
    nodex(root)
        .args(["lifecycle", "set", "decision", "--status", "active"])
        .assert()
        .success();
    nodex(root)
        .args(["lifecycle", "set", "request", "--status", "agreed"])
        .assert()
        .success();
    nodex(root).arg("check").assert().success();
    let unchanged = fs::read_to_string(root.join("docs/request.md")).unwrap();
    nodex(root)
        .args(["lifecycle", "set", "request", "--status", "active"])
        .assert()
        .code(2);
    assert_eq!(
        fs::read_to_string(root.join("docs/request.md")).unwrap(),
        unchanged
    );
    write_doc(
        root,
        "nodex.toml",
        &format!(
            "{config}\n[[statuses.flow]]\nkinds=[\"adr\"]\ninitial=\"active\"\ntransitions={{active=[\"archived\"]}}\n"
        ),
    );
    let output = nodex(root).args(["export", "config"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "CONFIG_ERROR");
}

#[test]
fn ai_replacement_preserves_frozen_historical_citations_and_reports_holds() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    assert!(git(&["init", "-q"]).status.success());
    write_doc(
        root,
        "nodex.toml",
        "[scope]\ninclude=[\"**/*.md\"]\n[rules]\nimmutable_baseline=\"HEAD\"\n[rules.severity]\nunresolved_reference=\"error\"\n[[rules.body_immutable]]\nname=\"records\"\nmode=\"frozen\"\ntrigger=\"creation\"\n",
    );
    write_doc(root, "old.md", "---\nid: old\n---\n# Old\n");
    write_doc(root, "new.md", "---\nid: new\n---\n# New\n");
    write_doc(root, "use.md", "---\nid: use\n---\n[old](old.md)\n");
    assert!(
        git(&["add", "nodex.toml", "old.md", "new.md", "use.md"])
            .status
            .success()
    );
    assert!(git(&["commit", "-qm", "frozen records"]).status.success());
    let source = fs::read_to_string(root.join("old.md")).unwrap();
    let citation = fs::read_to_string(root.join("use.md")).unwrap();
    // A frozen citation is historical evidence and remains on the predecessor.
    let result = run_json(nodex(root).args(["retarget", "old", "new", "--supersede"]));
    assert_eq!(result["completion"], "partial");
    assert_eq!(fs::read_to_string(root.join("use.md")).unwrap(), citation);
    assert_ne!(fs::read_to_string(root.join("old.md")).unwrap(), source);
    nodex(root).arg("check").assert().success();
}

#[test]
fn ai_missing_fields_agree_with_schema_presence_for_collections() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_doc(
        root,
        "nodex.toml",
        "[scope]\ninclude=[\"docs/**/*.md\"]\n[schema]\nrequired=[\"refs\"]\ncross_field=[{when=\"refs exists\", require=\"owner\"}]\n",
    );
    for (id, field) in [
        ("array", "refs: [x, y]\n"),
        ("object", "refs: {key: value}\n"),
        ("empty-object", "refs: {}\n"),
        ("false", "refs: false\n"),
        ("zero", "refs: 0\n"),
        ("empty-array", "refs: []\n"),
        ("absent", ""),
    ] {
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!("---\nid: '{id}'\ntitle: '{id}'\n{field}---\n"),
        );
    }
    nodex(root).arg("build").assert().success();
    let missing =
        run_json(nodex(root).args(["query", "nodes", "--missing", "refs", "--fields", "id,refs"]));
    assert_eq!(
        missing["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["absent", "empty-array"]
    );
    let output = nodex(root).arg("check").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let check: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        check["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["rule_id"] == "required_field")
            .map(|v| v["node_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["absent", "empty-array"]
    );
    assert_eq!(
        check["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["rule_id"] == "cross_field")
            .map(|v| v["node_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["array", "empty-object", "false", "object", "zero"]
    );
    let paths = run_json(nodex(root).args(["query", "nodes", "--missing", "path"]));
    assert_eq!(paths["total"], 0);
}
