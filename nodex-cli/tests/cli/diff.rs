//! `diff` and `impact`.

use crate::support::{
    error_code, git_runner, init_project, nodex, run_envelope, run_json, scratch, walk_entries,
    write_doc,
};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

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
fn a_diff_refused_for_its_before_ref_leaves_the_project_untouched() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    let git = git_runner(root);
    git(&["init", "-q"]);
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    let entries = || -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = walk_entries(root)
            .into_iter()
            .filter(|p| !p.starts_with(root.join(".git")))
            .collect();
        entries.sort();
        entries
    };
    let before = entries();

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
    assert_eq!(
        entries(),
        before,
        "a refused diff writes nothing into the project"
    );
}

/// `impact` graphs both refs under the after ref's config, so that config is
/// the vocabulary `--relations` names, whatever the working tree declares.
#[test]
fn impact_relations_are_the_after_refs_vocabulary() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    let plain = "[scope]\ninclude = [\"docs/**/*.md\"]\n";
    let declaring = format!(
        "{plain}[[parser.link_patterns]]\npattern = \"DEP:([a-z]+)\"\nrelation = \"depends_on\"\n"
    );
    fs::write(root.join("nodex.toml"), plain).unwrap();
    write_doc(root, "docs/a.md", "---\nid: a\ntitle: A\n---\n# A\n");
    write_doc(root, "docs/b.md", "---\nid: b\ntitle: B\n---\n# B\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "plain"]);
    fs::write(root.join("nodex.toml"), &declaring).unwrap();
    write_doc(root, "docs/b.md", "---\nid: b\ntitle: B\n---\n# B\nDEP:a\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "declared"]);

    fs::write(root.join("nodex.toml"), plain).unwrap();
    let env =
        run_envelope(nodex(root).args(["impact", "HEAD~1", "HEAD", "--relations", "depends_on"]));
    let added = env
        .pointer("/data/diff/added_edges")
        .and_then(Value::as_array)
        .expect("added_edges");
    assert!(
        added.iter().any(|e| e["relation"] == "depends_on"),
        "the relation the after ref declares is walked: {added:?}"
    );

    fs::write(root.join("nodex.toml"), &declaring).unwrap();
    assert_eq!(
        error_code(nodex(root).args(["impact", "HEAD", "HEAD~1", "--relations", "depends_on"])),
        "INVALID_ARGUMENT",
        "a relation only the working tree declares names no edge of either graph"
    );
}

/// A review list names what references the changed record; how far beyond
/// that to walk is asked for, because a hub's closure over citations reaches
/// most of a corpus.
#[test]
fn impact_names_direct_dependents_unless_asked_for_more_hops() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    write_doc(root, "docs/a.md", "---\nid: a\ntitle: A\n---\n# A\n");
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\n---\n# B\n[a](a.md)\n",
    );
    write_doc(
        root,
        "docs/c.md",
        "---\nid: c\ntitle: C\n---\n# C\n[b](b.md)\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A revised\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "revise a"]);

    let dependents_of_a = |extra: &[&str]| -> Vec<String> {
        let data = run_json(nodex(root).args(["impact", "HEAD~1", "HEAD"]).args(extra));
        data["impacted"]
            .as_array()
            .expect("impacted")
            .iter()
            .find(|entry| entry["id"] == "a")
            .expect("a is impacted")["dependents"]
            .as_array()
            .expect("dependents")
            .iter()
            .map(|d| d["id"].as_str().expect("id").to_owned())
            .collect()
    };
    assert_eq!(dependents_of_a(&[]), ["b"]);
    assert_eq!(dependents_of_a(&["--depth", "2"]), ["b", "c"]);
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
