//! `report` and the `GRAPH.md` it writes.

use crate::support::{init_project, nodex, run_envelope, run_json, scratch, write_doc};
use serde_json::Value;
use std::fs;

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
    let env = run_envelope(nodex(root).args(["report"]));
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

    let env = run_envelope(nodex(root).args(["report"]));
    let out_dir = env
        .pointer("/data/output_dir")
        .and_then(Value::as_str)
        .expect("output_dir");
    let md = fs::read_to_string(std::path::Path::new(out_dir).join("GRAPH.md")).expect("GRAPH.md");
    let god = md
        .split("## Most Referenced Documents")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("Most Referenced Documents section");
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
fn report_supersession_preserves_branch_links() {
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

    let env = run_envelope(nodex(root).args(["report"]));
    let out_dir = env
        .pointer("/data/output_dir")
        .and_then(Value::as_str)
        .expect("output_dir");
    let md = fs::read_to_string(std::path::Path::new(out_dir).join("GRAPH.md")).expect("GRAPH.md");
    let section = md
        .split("## Supersession Links")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .expect("Supersession Links section");
    let bullets: Vec<&str> = section.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(bullets, ["- ~~a~~ → **x**", "- ~~b~~ → **x**"]);
}

#[test]
fn report_generates_markdown_without_replacing_query_snapshots() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    write_doc(root, "docs/a.md", "---\nid: a\ntitle: First\n---\n");
    let report = run_json(nodex(root).arg("report"));
    assert_eq!(report["generated"], serde_json::json!(["GRAPH.md"]));
    assert!(!root.join("_index/graph.json").exists());
    nodex(root).arg("build").assert().success();
    let snapshot = fs::read(root.join("_index/graph.json")).unwrap();
    write_doc(root, "docs/b.md", "---\nid: b\ntitle: Second\n---\n");
    run_json(nodex(root).arg("report"));
    assert!(
        fs::read_to_string(root.join("_index/GRAPH.md"))
            .unwrap()
            .contains("**2 nodes**")
    );
    assert_eq!(fs::read(root.join("_index/graph.json")).unwrap(), snapshot);
    let query = nodex(root).args(["query", "nodes"]).output().unwrap();
    assert_eq!(query.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&query.stdout).unwrap()["error"]["code"],
        "GRAPH_OUTDATED"
    );
}
