//! `statuses.flow` judged per git step: the index, ranges, merges, and
//! history the walk cannot read.

use crate::support::{
    adr, envelope_of, error_code, flow_project, git_runner, judged, nodex, reported, run_envelope,
    run_json, scratch, staged, warning_msg, write_doc,
};
use assert_cmd::Command;
use serde_json::Value;
use std::fs;

#[test]
fn lifecycle_refuses_a_move_the_declared_flow_does_not_name() {
    // The seam guard stands on its own: no `rules.immutable_baseline` is
    // declared, so the baseline gate below it refuses nothing, and without
    // this guard `lifecycle` would write exactly what the same project's
    // `check --since` reds. `supersede` is guarded too — it writes a status
    // like any other action — while `review`, which writes none, is not.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"active\"\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n\
         [scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: adr-a\ntitle: A\nkind: adr\nstatus: proposed\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: adr-b\ntitle: B\nkind: adr\nstatus: proposed\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // `proposed → superseded` is not declared; the flow allows only `active`.
    nodex(tmp.path())
        .args(["lifecycle", "set", "adr-a", "--status", "superseded"])
        .assert()
        .failure();
    let content = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    assert!(
        content.contains("status: proposed"),
        "the refused write must leave the document untouched: {content}"
    );

    // `supersede` writes `superseded` and is refused on the same ground.
    nodex(tmp.path())
        .args(["lifecycle", "supersede", "adr-b", "--to", "adr-a"])
        .assert()
        .failure();

    // `review` writes no status, so the flow has nothing to say about it.
    nodex(tmp.path())
        .args(["lifecycle", "review", "adr-a"])
        .assert()
        .success();

    // And the declared move goes through.
    nodex(tmp.path())
        .args(["lifecycle", "set", "adr-a", "--status", "active"])
        .assert()
        .success();
}

/// A project whose one lifecycle names no `kinds`, which is every kind's: the
/// write seam refuses a note's undeclared move, and `check` reds an ADR's
/// hand-edited move and a note authored past the entry status.
#[test]
fn a_flow_naming_no_kinds_governs_every_kind() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\n\
         [[statuses.flow]]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n\
         [scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/decisions/**\"\nkind = \"adr\"\n\
         [detection]\norphan_ok_kinds = [\"adr\", \"generic\"]\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
    write_doc(
        root,
        "docs/decisions/a.md",
        "---\nid: adr-a\ntitle: A\nstatus: proposed\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/notes/n.md",
        "---\nid: note-n\ntitle: N\nstatus: proposed\n---\n# N\n",
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author them"]);

    let refused =
        envelope_of(nodex(root).args(["lifecycle", "set", "note-n", "--status", "superseded"]));
    assert_eq!(refused["error"]["code"], "INVALID_TRANSITION", "{refused}");
    run_envelope(nodex(root).args(["lifecycle", "set", "note-n", "--status", "active"]));

    write_doc(
        root,
        "docs/decisions/a.md",
        "---\nid: adr-a\ntitle: A\nstatus: superseded\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/notes/m.md",
        "---\nid: note-m\ntitle: M\nstatus: active\n---\n# M\n",
    );
    let data = judged(nodex(root).arg("check"));
    let mut findings: Vec<(&str, &str)> = data["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|v| {
            (
                v["rule_id"].as_str().unwrap(),
                v["node_id"].as_str().unwrap(),
            )
        })
        .collect();
    findings.sort_unstable();
    assert_eq!(
        findings,
        [("status_entry", "note-m"), ("status_transition", "adr-a")],
        "{data}"
    );
}

/// A note promoted to a decision enters the flow at the entry status: the
/// status it held as a note, here `active`, is no prior the flow reads.
#[test]
fn a_record_taking_a_governed_kind_enters_the_flow_through_lifecycle() {
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    write_doc(
        root,
        "docs/n.md",
        "---\nid: note-n\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a note"]);

    write_doc(
        root,
        "docs/n.md",
        "---\nid: note-n\ntitle: N\nkind: adr\nstatus: active\n---\n# N\n",
    );
    run_envelope(nodex(root).args(["lifecycle", "set", "note-n", "--status", "proposed"]));
    nodex(root).arg("build").assert().success();
    let node = &run_json(nodex(root).args(["query", "node", "note-n"]))["node"];
    assert_eq!(
        (&node["kind"], &node["status"]),
        (&serde_json::json!("adr"), &serde_json::json!("proposed"))
    );
}

#[test]
fn scaffold_force_refuses_a_status_reset_the_declared_flow_does_not_name() {
    // `--force` rewrites a document from config defaults, which resets its
    // status — for a governed kind that is a move like any other, and one no
    // `--field` can satisfy, since the value comes from the config rather
    // than the caller. Without this guard the placeholder path demotes the
    // finding to an advisory and writes it, with no baseline to catch it.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"active\"\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n\
         [scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: adr-a\ntitle: A\nkind: adr\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    nodex(tmp.path())
        .args([
            "scaffold",
            "--kind",
            "adr",
            "--title",
            "A",
            "--path",
            "docs/a.md",
            "--force",
        ])
        .assert()
        .failure();
    let content = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    assert!(
        content.contains("status: active"),
        "the refused overwrite must leave the document untouched: {content}"
    );

    // A fresh document of the same governed kind still scaffolds, at the
    // flow's entry point.
    nodex(tmp.path())
        .args([
            "scaffold",
            "--kind",
            "adr",
            "--title",
            "B",
            "--path",
            "docs/b.md",
        ])
        .assert()
        .success();
    let fresh = fs::read_to_string(tmp.path().join("docs/b.md")).unwrap();
    assert!(fresh.contains(r#"status: "proposed""#), "{fresh}");
}

#[test]
fn a_write_seam_answers_for_what_it_introduces_not_for_a_drift_it_found() {
    // A lock's refusal is absolute — a record already off its frozen
    // baseline stays refused whatever the write touches. Every other rule
    // that reads history judges a change, so it has no claim on a drift the
    // proposal did not cause: `review` writes only `reviewed`, and a status
    // somebody edited by hand is not this write's to answer for. The move a
    // write does make is the step it would commit, from `HEAD`.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\n\
         [scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: adr-a\ntitle: A\nkind: adr\nstatus: active\n---\n# A\n",
    );
    let git = git_runner(tmp.path());
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    // Drift the status by hand, behind nodex's back.
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: adr-a\ntitle: A\nkind: adr\nstatus: proposed\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    nodex(tmp.path())
        .args(["lifecycle", "review", "adr-a"])
        .assert()
        .success();

    // `active → superseded` is the step this write commits, and it is
    // declared, whatever the document says uncommitted.
    nodex(tmp.path())
        .args(["lifecycle", "set", "adr-a", "--status", "superseded"])
        .assert()
        .success();
}

fn flow_findings(root: &std::path::Path, since: &str) -> Vec<(String, String, Option<String>)> {
    let output = nodex(root)
        .args(["check", "--since", since])
        .output()
        .expect("check ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope["data"]["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|v| {
            (
                v["rule_id"].as_str().unwrap().to_string(),
                v["node_id"].as_str().unwrap().to_string(),
                v["details"]["commit"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn head(git: &impl Fn(&[&str]) -> std::process::Output) -> String {
    String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string()
}

/// `(rule, node)` for every violation `check --staged` reports.
fn staged_findings(root: &std::path::Path) -> Vec<(String, String)> {
    findings(&mut staged(root))
}

fn findings(cmd: &mut Command) -> Vec<(String, String)> {
    reported(cmd)["data"]["violations"]
        .as_array()
        .expect("violations")
        .iter()
        .map(|v| {
            (
                v["rule_id"].as_str().unwrap().to_string(),
                v["node_id"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The step a commit makes is what is staged: a move staged and then undone
/// in the work tree alone is still the move `git commit` records.
#[test]
fn a_staged_move_the_work_tree_undoes_is_judged_under_staged() {
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    adr(root, "adr-a", "proposed", "a");
    git(&["add", "-A"]);
    adr(root, "adr-a", "active", "a");

    assert_eq!(findings(nodex(root).arg("check")), vec![]);
    assert_eq!(
        staged_findings(root),
        vec![("status_transition".to_string(), "adr-a".to_string())]
    );
}

/// What is not staged is not committed: an undeclared move left in the work
/// tree does not refuse the commit that leaves it out.
#[test]
fn an_unstaged_move_is_no_part_of_a_staged_check() {
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    adr(root, "adr-b", "proposed", "b");
    git(&["add", "-A"]);
    adr(root, "adr-a", "proposed", "a");

    assert_eq!(
        findings(nodex(root).arg("check")),
        vec![("status_transition".to_string(), "adr-a".to_string())]
    );
    assert_eq!(staged_findings(root), vec![]);
}

/// A document that was never added is not in the commit, so a staged link to
/// it is the dangling reference the commit will carry.
#[test]
fn an_unadded_document_is_no_part_of_a_staged_check() {
    let tmp = scratch();
    let root = tmp.path();
    flow_project(
        root,
        "[parser]\nwikilink_enabled = true\n\
         [[detection.unresolved_policy]]\nname = \"dangling\"\ncause = \"missing\"\n\
         severity = \"error\"\n",
    );
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    adr(root, "adr-z", "proposed", "Builds on [[adr-w]].");
    git(&["add", "-A"]);
    adr(root, "adr-w", "proposed", "w");

    assert_eq!(findings(nodex(root).arg("check")), vec![]);
    assert_eq!(
        staged_findings(root),
        vec![(
            "unresolved_reference/dangling".to_string(),
            "adr-z".to_string()
        )]
    );
}

/// `git commit -a` and `git commit <path>` commit an index of their own and
/// name it to their hooks in `GIT_INDEX_FILE`, absolute or relative to the
/// directory the hook runs in.
#[test]
fn check_staged_reads_the_index_git_names_for_the_commit_under_way() {
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    adr(root, "adr-a", "proposed", "a");
    git(&["add", "-A"]);
    let named = git(&["read-tree", "--index-output=.git/next-index", "HEAD"]);
    assert!(named.status.success(), "{named:?}");
    assert_eq!(
        staged_findings(root),
        vec![("status_transition".to_string(), "adr-a".to_string())],
        "the work tree's own index stages the move"
    );

    let absolute = findings(staged(root).env("GIT_INDEX_FILE", root.join(".git/next-index")));
    assert_eq!(
        absolute,
        vec![],
        "the index the commit under way makes carries no move"
    );
    let relative = findings(
        staged(root)
            .current_dir(root)
            .env("GIT_INDEX_FILE", ".git/next-index"),
    );
    assert_eq!(relative, vec![]);
}

/// git reads a relative `GIT_INDEX_FILE` against the work tree's top level,
/// wherever it is started. A hook for a subdirectory project that changes
/// into it first is handed `.git/index` by a plain `git commit`.
#[test]
fn check_staged_reads_a_relative_index_against_the_work_tree_top() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    let project = root.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    flow_project(&project, "");
    adr(&project, "adr-a", "active", "a");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    adr(&project, "adr-a", "proposed", "a");
    git(&["add", "-A"]);

    let mut hook = Command::cargo_bin("nodex").expect("nodex binary in cargo target");
    hook.current_dir(&project)
        .args(["check", "--staged"])
        .env("GIT_INDEX_FILE", ".git/index");
    assert_eq!(
        findings(&mut hook),
        vec![("status_transition".to_string(), "adr-a".to_string())]
    );
}

#[test]
fn check_staged_refuses_an_index_it_cannot_read_the_project_from() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q", "-b", "main"]);
    write_doc(root, "README.md", "root\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root"]);
    let project = root.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    flow_project(&project, "");
    adr(&project, "adr-a", "active", "a");
    assert_eq!(
        error_code(&mut staged(&project)),
        "GIT_ERROR",
        "a project never staged is not in the index"
    );
    assert_eq!(
        error_code(staged(root).env("GIT_INDEX_FILE", root.join(".git/no-such-index"))),
        "GIT_ERROR",
        "an index git did not name is not read as an empty one"
    );

    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "project"]);
    git(&["checkout", "-q", "-b", "side"]);
    adr(&project, "adr-a", "active", "side");
    git(&["commit", "-qam", "side"]);
    git(&["checkout", "-q", "main"]);
    adr(&project, "adr-a", "active", "main");
    git(&["commit", "-qam", "main"]);
    let merge = git(&["merge", "-q", "side"]);
    assert!(!merge.status.success(), "the two lines conflict");
    assert_eq!(
        error_code(&mut staged(&project)),
        "GIT_ERROR",
        "an index with unmerged entries records no tree"
    );
}

/// The index a commit is made from belongs to the operator, or to the `git
/// commit` running the hook, so a staged check reads it and writes none of
/// it — not even while another git process holds it locked.
#[test]
fn check_staged_writes_nothing_into_the_index_it_reads() {
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    adr(root, "adr-b", "proposed", "b");
    git(&["add", "-A"]);
    let index = root.join(".git/index");
    let before = fs::read(&index).unwrap();
    fs::write(root.join(".git/index.lock"), "").unwrap();

    staged(root).assert().success();
    assert_eq!(fs::read(&index).unwrap(), before);
}

/// The commit carries the staged config, and whatever judges it later reads
/// that one.
#[test]
fn check_staged_takes_its_config_from_the_index() {
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    fs::write(root.join("nodex.toml"), "[scope\n").unwrap();

    assert_eq!(error_code(nodex(root).arg("check")), "CONFIG_ERROR");
    assert_eq!(staged_findings(root), vec![]);
}

/// A staged link is graphed as the tree records it: one resolving outside the
/// repository is not read, and the report says so.
#[cfg(unix)]
#[test]
fn check_staged_names_a_staged_link_that_leaves_the_repository() {
    let tmp = scratch();
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    flow_project(&root, "");
    adr(&root, "adr-a", "active", "a");
    write_doc(
        tmp.path(),
        "outside.md",
        "---\nid: adr-out\ntitle: out\nstatus: proposed\n---\nout\n",
    );
    std::os::unix::fs::symlink(tmp.path().join("outside.md"), root.join("docs/out.md")).unwrap();
    let git = git_runner(&root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);

    let envelope = reported(&mut staged(&root));
    let named = envelope["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "scope_coverage")
        .filter_map(warning_msg)
        .any(|m| m.contains("docs/out.md"));
    assert!(named, "{envelope}");
}

#[test]
fn check_since_judges_each_commit_against_its_parents() {
    // An endpoint diff reads a record authored at `proposed` and accepted a
    // commit later as one that arrived accepted. Judged a commit at a time,
    // both steps are declared, and a record that did arrive accepted is
    // reported at the commit that brought it.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["tag", "base"]);

    adr(root, "adr-b", "proposed", "b");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author adr-b"]);
    adr(root, "adr-b", "active", "b");
    git(&["commit", "-qam", "accept adr-b"]);
    assert_eq!(flow_findings(root, "base"), vec![]);

    adr(root, "adr-c", "active", "c");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author adr-c accepted"]);
    let arrived = head(&git);
    adr(root, "adr-c", "active", "c, edited");
    git(&["commit", "-qam", "edit adr-c"]);
    assert_eq!(
        flow_findings(root, "base"),
        vec![(
            "status_entry".to_string(),
            "adr-c".to_string(),
            Some(arrived)
        )]
    );
}

#[test]
fn a_move_the_range_undoes_is_still_judged_where_it_was_made() {
    // `active → proposed → active` leaves the endpoints equal, so the diff
    // touches nothing; the demotion is still a step the flow does not name.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["tag", "base"]);
    adr(root, "adr-a", "proposed", "a");
    git(&["commit", "-qam", "demote"]);
    let demoted = head(&git);
    adr(root, "adr-a", "active", "a");
    git(&["commit", "-qam", "promote again"]);

    assert_eq!(
        flow_findings(root, "base"),
        vec![(
            "status_transition".to_string(),
            "adr-a".to_string(),
            Some(demoted)
        )]
    );
}

#[test]
fn a_merge_is_judged_only_where_it_differs_from_every_parent() {
    // The mainline accepted and then superseded a record the feature branch
    // still holds at `proposed`. Merging the mainline in carries both moves,
    // which are the mainline commits' steps: read against the branch alone,
    // the merge would move the record `proposed → superseded`.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "proposed", "a");
    let git = git_runner(root);
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["tag", "base"]);
    git(&["checkout", "-q", "-b", "feat"]);
    adr(root, "adr-f", "proposed", "f");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "feat: author adr-f"]);
    git(&["checkout", "-q", "main"]);
    adr(root, "adr-a", "active", "a");
    git(&["commit", "-qam", "main: accept adr-a"]);
    adr(root, "adr-a", "superseded", "a");
    git(&["commit", "-qam", "main: supersede adr-a"]);
    git(&["checkout", "-q", "feat"]);
    git(&["merge", "-q", "--no-edit", "main"]);

    assert_eq!(flow_findings(root, "base"), vec![]);
    assert_eq!(flow_findings(root, "main"), vec![]);
}

#[test]
fn the_uncommitted_change_is_judged_against_every_head_of_a_merge_under_way() {
    // The side branch accepted and superseded the record; the mainline
    // edited its body at `proposed`. Resolving the conflict to `superseded`
    // takes the side's status whole, which only reads as legal against
    // both of the parents the merge commit will record.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-x", "proposed", "x");
    let git = git_runner(root);
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["checkout", "-q", "-b", "side"]);
    adr(root, "adr-x", "active", "x");
    git(&["commit", "-qam", "side: accept"]);
    adr(root, "adr-x", "superseded", "side");
    git(&["commit", "-qam", "side: supersede"]);
    git(&["checkout", "-q", "main"]);
    adr(root, "adr-x", "proposed", "main");
    git(&["commit", "-qam", "main: edit"]);
    assert!(!git(&["merge", "-q", "--no-edit", "side"]).status.success());
    adr(root, "adr-x", "superseded", "resolved");
    git(&["add", "-A"]);

    assert_eq!(flow_findings(root, "HEAD"), vec![]);
}

#[test]
fn a_write_seam_judges_the_step_its_write_would_commit() {
    // The baseline is older than the record: judged against it, accepting a
    // record authored since reads as a record arriving accepted. The step the
    // write takes is from `HEAD`, where the record already stands at its
    // entry — and a record that is not committed yet would arrive accepted.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "[rules]\nimmutable_baseline = \"base\"\n");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["tag", "base"]);
    adr(root, "adr-b", "proposed", "b");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author adr-b"]);
    adr(root, "adr-c", "proposed", "c");
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args(["lifecycle", "set", "adr-b", "--status", "active"])
        .assert()
        .success();
    nodex(root)
        .args(["lifecycle", "set", "adr-c", "--status", "active"])
        .assert()
        .failure();
    assert!(
        fs::read_to_string(root.join("docs/adr-c.md"))
            .unwrap()
            .contains("status: proposed")
    );
}

#[test]
fn a_range_a_shallow_clone_cuts_is_refused_rather_than_read_as_new() {
    // A commit whose parents the clone does not hold would present every
    // record it carries as arriving there.
    let origin = scratch();
    let root = origin.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["tag", "base"]);
    for n in 0..3 {
        adr(root, "adr-a", "active", &format!("edit {n}"));
        git(&["commit", "-qam", "edit"]);
    }

    let parent = scratch();
    let clone = parent.path().join("clone");
    let git_parent = git_runner(parent.path());
    let url = format!("file://{}", root.display());
    assert!(
        git_parent(&["clone", "-q", "--depth", "2", &url, clone.to_str().unwrap()])
            .status
            .success()
    );
    let git_clone = git_runner(&clone);
    assert!(
        git_clone(&["fetch", "-q", "--depth", "2", "origin", "tag", "base"])
            .status
            .success()
    );

    let output = nodex(&clone)
        .args(["check", "--since", "base"])
        .output()
        .expect("check ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(envelope["error"]["code"], "GIT_ERROR", "{envelope}");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("shallow")
    );
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_shallow_clone_judges_what_it_can_read_and_counts_the_rest_unjudged() {
    // `actions/checkout` clones one commit deep. A document that commit could
    // not parse has no earlier state to read back there, so the records that
    // may have stood in it are counted rather than judged — and the run goes
    // on, over every record the clone can read.
    let origin = scratch();
    let root = origin.path();
    flow_project(root, "");
    adr(root, "adr-a", "proposed", "a");
    adr(root, "adr-x", "active", "x");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    write_doc(
        root,
        "docs/adr-x.md",
        "---\nid: adr-x\n  title: [broken\nkind: adr\n---\nx\n",
    );
    git(&["commit", "-qam", "break x"]);

    let parent = scratch();
    let clone = parent.path().join("clone");
    let git_parent = git_runner(parent.path());
    let url = format!("file://{}", root.display());
    assert!(
        git_parent(&["clone", "-q", "--depth", "1", &url, clone.to_str().unwrap()])
            .status
            .success()
    );
    // The repair the clone cannot judge: `adr-x` was accepted before the cut.
    write_doc(
        &clone,
        "docs/adr-x.md",
        "---\nid: adr-x\ntitle: x\nkind: adr\nstatus: active\n---\nx\n",
    );
    let envelope = reported(nodex(&clone).arg("check"));
    let data = &envelope["data"];
    let flow: Vec<(&str, u64, u64)> = data["rule_coverage"]
        .as_array()
        .expect("coverage")
        .iter()
        .filter(|c| c["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|c| {
            (
                c["rule_id"].as_str().unwrap(),
                c["subjects"].as_u64().unwrap(),
                c["unjudged"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        data["violations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
            .count(),
        0,
        "a record the clone cannot read back is not an arrival: {data}"
    );
    assert_eq!(
        flow,
        [("status_entry", 1, 1), ("status_transition", 1, 1)],
        "adr-a judged, adr-x counted: {data}"
    );
    // A count does not say why, and the cut is not the operator's to read
    // off the number: the run names the path and the remedy.
    let said = unread_said(&envelope);
    assert!(
        said.len() == 1
            && said[0].contains("docs/adr-x.md")
            && said[0].contains("shallow clone's cut"),
        "{said:?}"
    );

    // The same repair where the history is there: judged, and clean.
    let full = parent.path().join("full");
    assert!(
        git_parent(&["clone", "-q", &url, full.to_str().unwrap()])
            .status
            .success()
    );
    write_doc(
        &full,
        "docs/adr-x.md",
        "---\nid: adr-x\ntitle: x\nkind: adr\nstatus: active\n---\nx\n",
    );
    assert_eq!(flow_findings(&full, "HEAD"), vec![]);
}

#[test]
fn lines_that_share_no_commit_leave_the_record_they_disagree_about_unjudged() {
    // `--allow-unrelated-histories` merges two projects. With no commit
    // behind both lines, nothing says which of them moved a record they
    // disagree about — so neither line's position is one the merge may move
    // from, and the record is counted rather than judged.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    adr(root, "adr-b", "active", "b");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author them"]);
    let base = head(&git);
    write_doc(
        root,
        "docs/adr-a.md",
        "---\nid: adr-a\ntitle: adr-a\nstatus: superseded\nsuperseded_by: adr-b\n---\na\n",
    );
    git(&["commit", "-qam", "supersede it"]);
    let real = head(&git);

    git(&["checkout", "-q", "--orphan", "elsewhere"]);
    git(&["rm", "-rq", "--cached", "."]);
    fs::remove_file(root.join("docs/adr-b.md")).unwrap();
    adr(root, "adr-a", "proposed", "another project's document");
    git(&["add", "nodex.toml", ".gitignore", "docs/adr-a.md"]);
    git(&["commit", "-q", "-m", "an unrelated history"]);
    let elsewhere = head(&git);

    git(&["checkout", "-qf", &real]);
    git(&[
        "merge",
        "--no-commit",
        "--allow-unrelated-histories",
        &elsewhere,
    ]);
    adr(root, "adr-a", "active", "the merge resolves it");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "merge the unrelated line"]);

    let data = run_json(nodex(root).args(["check", "--since", &base]));
    let flow: Vec<&str> = data["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|v| v["rule_id"].as_str().unwrap())
        .collect();
    let unjudged: Vec<u64> = data["rule_coverage"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|c| c["unjudged"].as_u64().unwrap())
        .collect();
    assert!(
        flow.is_empty(),
        "no line's position is one the merge may move from: {data}"
    );
    assert!(
        unjudged.iter().all(|counted| *counted >= 1),
        "and the reach says the record went unjudged: {data}"
    );
}

#[test]
fn lines_that_last_agreed_in_more_than_one_place_are_read_against_every_one() {
    // Two lines that have merged each other before share two commits, each a
    // place they last agreed and neither above the other. Reading one and
    // dropping the other decides by whichever git lists first what the lines
    // agreed on, and refuses whichever resolution the dropped one allows — so
    // the merge is resolved each way in turn, and neither is refused.
    let resolved = |to: &str| -> (Value, String) {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        flow_project(&root, "");
        adr(&root, "adr-a", "proposed", "a");
        adr(&root, "adr-b", "active", "b");
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "author them"]);
        let start = head(&git);
        adr(&root, "adr-a", "proposed", "one line edits the body only");
        git(&["commit", "-qam", "one"]);
        let one = head(&git);
        git(&["checkout", "-q", "-b", "other", &start]);
        adr(&root, "adr-a", "active", "the other line accepts it");
        git(&["commit", "-qam", "other"]);
        let other = head(&git);
        // Each line merges the other as it was and keeps its own reading, so
        // both commits are places they last agreed and neither is behind the
        // other.
        git(&["checkout", "-q", &one]);
        git(&["merge", "--no-commit", "-X", "ours", &other]);
        adr(&root, "adr-a", "proposed", "one line edits the body only");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one merges other"]);
        let one_merged = head(&git);
        git(&["checkout", "-q", &other]);
        git(&["merge", "--no-commit", "-X", "ours", &one]);
        adr(&root, "adr-a", "active", "the other line accepts it");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "other merges one"]);
        let other_merged = head(&git);

        git(&["checkout", "-q", &one_merged]);
        git(&["merge", "--no-commit", &other_merged]);
        match to {
            "superseded" => write_doc(
                &root,
                "docs/adr-a.md",
                "---\nid: adr-a\ntitle: adr-a\nstatus: superseded\nsuperseded_by: adr-b\n---\nthe merge resolves it\n",
            ),
            _ => adr(&root, "adr-a", to, "the merge resolves it"),
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "the criss-cross merge"]);
        let merge = head(&git);
        (
            judged(nodex(&root).args(["check", "--since", &start])),
            merge,
        )
    };

    // From `proposed` the record may stay where it is; from `active` it may
    // be superseded. Each resolution is therefore allowed by exactly one of
    // the two places the lines agreed, and a reading that keeps one base
    // refuses whichever resolution the base it dropped allows.
    for to in ["proposed", "superseded"] {
        let (data, merge) = resolved(to);
        let refused: Vec<&Value> = data["violations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["details"]["commit"] == merge)
            .collect();
        assert!(
            refused.is_empty(),
            "one of the places the lines agreed allows the merge to resolve it \
             to {to}: {data}"
        );
    }
}

/// A flow with two terminal statuses, so a record's own line decides whether
/// the move it makes is declared.
fn two_terminals(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"note\", \"generic\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"archived\", \"superseded\"]\n\
         terminal = [\"archived\", \"superseded\"]\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\", \"archived\"] }\n\
         [scope]\ninclude = [\"docs/**/*.md\"]\n\
         [detection]\norphan_ok_kinds = [\"adr\", \"note\", \"generic\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
}

fn record(root: &std::path::Path, status: &str) {
    write_doc(
        root,
        "docs/p.md",
        &format!("---\nid: adr-b\ntitle: b\nstatus: {status}\n---\nb\n"),
    );
}

fn broken_record(root: &std::path::Path, mark: &str) {
    write_doc(
        root,
        "docs/p.md",
        &format!("---\nid: adr-b\n  broken: [{mark}\n---\nb\n"),
    );
}

#[test]
fn a_read_back_one_line_could_not_finish_is_no_answer_however_much_came_back() {
    // A document unparseable at a merge, read back along both lines: one
    // answers, the other ends at a shallow clone's cut. Taking the answer
    // that came back as the record's position judges the step against half a
    // reading — and reddens a gate the same working tree passes where the
    // history is whole.
    let origin = scratch();
    let root = origin.path();
    two_terminals(root);
    record(root, "proposed");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author it"]);
    record(root, "active");
    git(&["commit", "-qam", "accept it"]);
    git(&["branch", "retiring"]);
    broken_record(root, "one");
    git(&["commit", "-qam", "break it on this line"]);
    for filler in ["one", "two"] {
        fs::write(root.join(filler), filler).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", filler]);
    }
    let accepting = head(&git);
    git(&["checkout", "-q", "retiring"]);
    record(root, "archived");
    git(&["commit", "-qam", "retire it on the other line"]);
    broken_record(root, "two");
    git(&["commit", "-qam", "break it there too"]);
    let retiring = head(&git);
    git(&["checkout", "-q", &accepting]);
    git(&["merge", "--no-commit", &retiring]);
    broken_record(root, "one");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "merge, still broken"]);
    git(&["branch", "-f", "main", "HEAD"]);

    let parent = scratch();
    let url = format!("file://{}", root.display());
    let judged = |name: &str, depth: &[&str]| -> Value {
        let into = parent.path().join(name);
        let mut args = vec!["clone", "-q"];
        args.extend_from_slice(depth);
        args.extend_from_slice(&["--branch", "main", &url, into.to_str().unwrap()]);
        assert!(git_runner(parent.path())(&args).status.success(), "{name}");
        // `active` declares this move and `archived` declares nothing.
        record(&into, "superseded");
        reported(nodex(&into).arg("check"))
    };

    let whole = judged("whole", &[]);
    let (findings, unjudged, _) = flow_reach(&whole);
    assert!(
        findings.is_empty(),
        "one line left the record where this move is declared: {findings:?}"
    );
    assert_eq!(unjudged, [0, 0], "and both lines answered: {whole}");

    // Deep enough to hold the line that retired it, not the one that accepted
    // it: the reading that came back is the one that refuses the move.
    let cut = judged("cut", &["--depth", "3"]);
    let (findings, unjudged, _) = flow_reach(&cut);
    assert!(
        findings.is_empty(),
        "what the other line held is what this clone cannot read: {findings:?}"
    );
    assert!(
        unjudged.iter().all(|counted| *counted >= 1),
        "and the record is counted rather than judged: {unjudged:?}"
    );
    assert!(
        unread_said(&cut)
            .iter()
            .any(|said| said.contains("docs/p.md")),
        "naming the document it could not read back: {:?}",
        unread_said(&cut)
    );
}

#[test]
fn a_tree_the_build_refused_behind_a_broken_document_stands_for_nothing_knowably() {
    // The read-back reaches a commit whose tree this config refuses. It holds
    // no document at that path the way a commit that never had one holds
    // none — it holds a record the walk cannot name — so reading it as the
    // second makes a repaired document arrive out of nowhere.
    let tmp = scratch();
    let root = tmp.path();
    two_terminals(root);
    record(root, "active");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author it"]);
    // Moves the record *and* refuses the tree, so the read-back stops here
    // rather than stepping over a commit that left the path alone.
    record(root, "archived");
    write_doc(
        root,
        "docs/dup.md",
        "---\nid: adr-b\ntitle: dup\nstatus: active\n---\ndup\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "move it, and refuse the tree"]);
    git(&["rm", "-q", "docs/dup.md"]);
    broken_record(root, "x");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "break it"]);
    record(root, "superseded");

    let envelope = reported(nodex(root).arg("check"));
    let (findings, unjudged, unread) = flow_reach(&envelope);
    assert!(
        findings.is_empty(),
        "nothing says the record arrived here: {findings:?}"
    );
    assert!(
        unjudged.iter().all(|counted| *counted >= 1),
        "it is counted instead: {unjudged:?}"
    );
    assert_eq!(unread, 1, "and the run names the commit: {envelope}");
}

#[test]
fn a_prior_the_rule_reads_only_part_of_leaves_the_step_unanswerable() {
    // A step read a record two ways, one of them under a kind the flow does
    // not govern. Narrowing the priors to the governed reading answers a
    // question the walk could not settle — and it answers it the same way
    // whichever way the answer falls, so it is neither a strict reading nor a
    // lenient one.
    let judged = |aside: &str, repaired: &str| -> Value {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        two_terminals(&root);
        write_doc(
            &root,
            "docs/p.md",
            "---\nid: adr-b\ntitle: b\nkind: adr\nstatus: proposed\n---\nb\n",
        );
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "author it"]);
        git(&["checkout", "-q", "-b", "aside"]);
        write_doc(
            &root,
            "docs/p.md",
            &format!("---\nid: adr-b\ntitle: b\nkind: note\nstatus: {aside}\n---\nb\n"),
        );
        git(&["commit", "-qam", "out of the governed kinds"]);
        let elsewhere = head(&git);
        git(&["checkout", "-q", "-"]);
        write_doc(
            &root,
            "docs/p.md",
            "---\nid: adr-b\ntitle: b\nkind: adr\nstatus: active\n---\nb\n",
        );
        git(&["commit", "-qam", "accept it here"]);
        git(&["merge", "--no-commit", &elsewhere]);
        broken_record(&root, "conflict");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "merge, unparseable"]);
        write_doc(
            &root,
            "docs/p.md",
            &format!("---\nid: adr-b\ntitle: b\nkind: adr\nstatus: {repaired}\n---\nb\n"),
        );
        reported(nodex(&root).arg("check"))
    };

    // Read as `adr`/`active` the repair is an undeclared move; read as a
    // `note` the flow governed nothing there and the repair is an entry at
    // the entry status.
    let refusing = judged("superseded", "proposed");
    // Read as `adr`/`active` the repair is declared; read as a `note` it is
    // an entry at a status the flow does not start at.
    let permitting = judged("proposed", "superseded");
    for envelope in [&refusing, &permitting] {
        let (findings, unjudged, _) = flow_reach(envelope);
        assert!(
            findings.is_empty(),
            "the walk could not settle which reading stood there: {findings:?}"
        );
        assert_eq!(
            unjudged,
            [1, 1],
            "so the record is counted rather than judged: {envelope}"
        );
    }
}

#[test]
fn places_the_lines_agreed_that_disagree_leave_the_record_counted_rather_than_judged() {
    // A criss-cross whose two agreement points hold the record at different
    // statuses. A line carrying what one of them holds may have inherited it
    // there or moved it there from the other, so which line moved the record
    // cannot be told — and read as "only what matches no agreement point
    // moved", the line that did move it is dropped and the move it declares
    // is refused.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"review\", \"superseded\"]\n\
         terminal = [\"superseded\"]\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"review\"], \
         review = [\"active\", \"superseded\"] }\n\
         [scope]\ninclude = [\"docs/**/*.md\"]\n\
         [detection]\norphan_ok_kinds = [\"adr\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
    adr(root, "adr-a", "proposed", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author it"]);
    let start = head(&git);
    let step = |status: &str, message: &str| {
        adr(root, "adr-a", status, "a");
        git(&["commit", "-qam", message]);
        head(&git)
    };
    step("active", "one line accepts it");
    let reviewing = step("review", "and sends it to review");
    git(&["checkout", "-q", "-b", "other", &start]);
    let accepted = step("active", "the other line accepts it and stops there");
    // Each line merges the other and keeps its own reading, so both commits
    // are places they last agreed and they disagree about the record.
    git(&["checkout", "-q", &reviewing]);
    git(&["merge", "--no-commit", "-X", "ours", &accepted]);
    adr(root, "adr-a", "review", "a");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "one merges the other"]);
    let retired = step("superseded", "and retires it");
    git(&["checkout", "-q", &accepted]);
    git(&["merge", "--no-commit", "-X", "ours", &reviewing]);
    adr(root, "adr-a", "active", "a");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the other merges one"]);
    let still_active = head(&git);

    git(&["checkout", "-q", &retired]);
    git(&["merge", "--no-commit", &still_active]);
    // `active` is where one line stands and `review` is declared from it.
    adr(root, "adr-a", "review", "the merge resolves it");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the criss-cross merge"]);

    let envelope = reported(nodex(root).args(["check", "--since", &start]));
    let (findings, unjudged, _) = flow_reach(&envelope);
    assert!(
        findings.is_empty(),
        "one line stands at `active`, which declares the move: {findings:?}"
    );
    assert!(
        unjudged.iter().all(|counted| *counted >= 1),
        "and the record is counted rather than judged: {unjudged:?}"
    );
}

/// The flow rules' `(rule, subjects, unjudged)` and the whole report of
/// `check --since` over a merge whose own document is unparseable, where the
/// lines behind it hold `adr-a` at `main_kind` and at `zzz`, which the flow
/// does not govern.
fn two_readings(main_kind: &str) -> (Vec<(String, u64, u64)>, Value) {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"generic\", \"zzz\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"proposed\"\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n\
         [scope]\ninclude = [\"docs/**/*.md\"]\n\
         [detection]\norphan_ok_kinds = [\"adr\", \"generic\", \"zzz\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
    adr(root, "adr-a", "active", "a");
    adr(root, "adr-b", "active", "b");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author them"]);
    let start = head(&git);
    write_doc(
        root,
        "docs/adr-a.md",
        &format!(
            "---\nid: adr-a\ntitle: adr-a\nkind: {main_kind}\nstatus: superseded\nsuperseded_by: adr-b\n---\nmain supersedes it\n"
        ),
    );
    git(&["commit", "-qam", "supersede it"]);
    let governed = head(&git);
    git(&["checkout", "-q", "-b", "side", &start]);
    write_doc(
        root,
        "docs/adr-a.md",
        "---\nid: adr-a\ntitle: adr-a\nkind: zzz\nstatus: proposed\n---\nthe side declares another kind\n",
    );
    git(&[
        "commit",
        "-qam",
        "the same id at a kind the flow does not govern",
    ]);
    let side = head(&git);
    git(&["checkout", "-q", &governed]);
    git(&["merge", "--no-commit", &side]);
    write_doc(
        root,
        "docs/adr-a.md",
        "---\nid: adr-a\n  title: [broken merge\nkind: adr\n---\nbroken\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "merge, unparseable"]);

    let data = judged(nodex(root).args(["check", "--since", &governed]));
    let reach = data["rule_coverage"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|c| {
            (
                c["rule_id"].as_str().unwrap().to_string(),
                c["subjects"].as_u64().unwrap(),
                c["unjudged"].as_u64().unwrap(),
            )
        })
        .collect();
    (reach, data)
}

#[test]
fn a_record_a_step_holds_two_readings_of_is_counted_rather_than_read_as_one() {
    // The merge's own document is unparseable, and the lines behind it hold
    // that id at two kinds — one the flow governs, one it does not. Reading
    // whichever sorts first as the record's own kind would judge a position
    // it may never have held, or skip the record entirely.
    let (reach, data) = two_readings("adr");
    assert_eq!(
        reach,
        [
            ("status_entry".to_string(), 1, 1),
            ("status_transition".to_string(), 1, 1)
        ],
        "adr-b judged, adr-a counted: {data}"
    );
}

#[test]
fn a_record_no_reading_of_which_the_flow_governs_is_not_counted() {
    // Neither reading is a position the flow governs, so whichever the step
    // stood at, the record is outside the flow there and nothing is owed.
    let (reach, data) = two_readings("generic");
    assert_eq!(
        reach,
        [
            ("status_entry".to_string(), 1, 0),
            ("status_transition".to_string(), 1, 0)
        ],
        "adr-b judged, adr-a outside: {data}"
    );
}

#[test]
fn a_record_every_line_dropped_enters_again_where_a_merge_restores_it() {
    // Where no line moved the record, the step stands on what the lines
    // carry — not on the places they last agreed, which still hold a record
    // both lines have since deleted. Reading those would make a restore look
    // like a move from the status it was deleted at.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    adr(root, "adr-b", "proposed", "b");
    adr(root, "adr-c", "active", "c");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author them"]);
    let start = head(&git);
    // The lines disagree about `adr-b`, which is what makes the step read
    // where they last agreed at all.
    git(&["rm", "-q", "docs/adr-a.md"]);
    adr(root, "adr-b", "active", "b");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "one line drops a and accepts b"]);
    let left = head(&git);
    git(&["checkout", "-q", "-b", "right", &start]);
    git(&["rm", "-q", "docs/adr-a.md"]);
    git(&["commit", "-qam", "the other line drops a"]);
    let right = head(&git);
    git(&["checkout", "-q", &left]);
    git(&["merge", "--no-commit", &right]);
    write_doc(
        root,
        "docs/adr-a.md",
        "---\nid: adr-a\ntitle: adr-a\nstatus: superseded\nsuperseded_by: adr-c\n---\nrestored by the merge\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the merge restores a"]);
    let merge = head(&git);

    let restored: Vec<(String, String, Option<String>)> = flow_findings(root, &start)
        .into_iter()
        .filter(|(_, node, _)| node == "adr-a")
        .collect();
    assert_eq!(
        restored,
        vec![("status_entry".to_string(), "adr-a".to_string(), Some(merge))],
        "a record no line carries enters where it comes back"
    );
}

#[test]
fn a_write_seam_judges_the_step_its_write_commits_even_where_no_byte_changes() {
    // A status hand-written into the working tree and then confirmed through
    // the seam commits the move `HEAD` records. Answering for the file's own
    // bytes would let the seam sanction exactly the move it exists to refuse.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "proposed", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author it"]);
    adr(root, "adr-a", "active", "a");
    git(&["commit", "-qam", "accept it"]);

    // Hand-written back to `proposed`, which the flow declares no move to.
    adr(root, "adr-a", "proposed", "a");
    nodex(root).arg("build").assert().success();
    let output = nodex(root)
        .args(["lifecycle", "set", "adr-a", "--status", "proposed"])
        .output()
        .expect("lifecycle ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["error"]["code"], "INVALID_TRANSITION",
        "the step is `active` → `proposed` whatever the file says: {envelope}"
    );

    // A write that lands where `HEAD` already has it is no move at all.
    adr(root, "adr-a", "active", "a");
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["lifecycle", "set", "adr-a", "--status", "active"])
        .assert()
        .success();
}

#[test]
fn a_branch_forked_before_a_move_cannot_carry_the_old_status_back_through_a_merge() {
    // The branch never touched the record; the main line accepted and
    // superseded it. Merging the branch and resolving the record to what the
    // branch still carries — or to anything the old status declares — would
    // walk a terminal record back out, and nothing else in the flow refuses
    // it: the branch's own commits took no step.
    let merged = |resolution: &str| -> Vec<(String, String, String)> {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        flow_project(&root, "");
        adr(&root, "adr-a", "proposed", "a");
        adr(&root, "adr-b", "active", "b");
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "author it"]);
        let base = head(&git);
        git(&["branch", "stale"]);
        adr(&root, "adr-a", "active", "a");
        git(&["commit", "-qam", "accept it"]);
        write_doc(
            &root,
            "docs/adr-a.md",
            "---\nid: adr-a\ntitle: adr-a\nstatus: superseded\nsuperseded_by: adr-b\n---\na\n",
        );
        git(&["commit", "-qam", "supersede it"]);
        let line = head(&git);
        git(&["checkout", "-q", "stale"]);
        adr(&root, "adr-a", "proposed", "the branch edits the body only");
        git(&["commit", "-qam", "a branch that never moved the record"]);
        let stale = head(&git);
        git(&["checkout", "-q", &line]);
        git(&["merge", "--no-commit", &stale]);
        adr(&root, "adr-a", resolution, "merged");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "merge the branch"]);
        flow_findings(&root, &base)
            .into_iter()
            .map(|(rule, node, commit)| (rule, node, commit.unwrap_or_default()))
            .collect()
    };

    for resolution in ["active", "proposed"] {
        let findings = merged(resolution);
        assert_eq!(
            findings.len(),
            1,
            "merging a line that made no claim does not move a superseded \
             record to {resolution}: {findings:?}"
        );
        assert_eq!(findings[0].0, "status_transition");
        assert_eq!(findings[0].1, "adr-a");
    }
}

#[test]
fn a_record_read_back_through_lines_that_disagree_keeps_every_line_s_answer() {
    // The document is unparseable at a merge, and the two lines behind it
    // stood the record at different statuses. Reading one of them back and
    // dropping the other makes the verdict turn on which branch was merged
    // into which, so the repair is judged against every position the record
    // may have held — and the same, whichever line the merge recorded first.
    let judged = |feature_first: bool| {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        // Its own vocabulary, so the write below is a move the flow declares
        // from one of the two positions and from the other nowhere at all.
        fs::write(
            root.join("nodex.toml"),
            "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
             [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\", \"archived\"]\n\
             terminal = [\"superseded\", \"archived\"]\n\
             [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
             transitions = { proposed = [\"active\"], active = [\"superseded\", \"archived\"] }\n\
             [scope]\ninclude = [\"docs/**/*.md\"]\n\
             [detection]\norphan_ok_kinds = [\"adr\"]\n\
             [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n",
        )
        .unwrap();
        fs::write(root.join(".gitignore"), "_index/\n").unwrap();
        adr(&root, "adr-a", "active", "a");
        adr(&root, "adr-b", "active", "b");
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["branch", "feature"]);
        for broken in ["one", "two"] {
            write_doc(
                &root,
                "docs/adr-a.md",
                &format!("---\nid: adr-a\n  title: [broken {broken}\nkind: adr\n---\na\n"),
            );
            git(&["commit", "-qam", "break it on the line that kept it active"]);
        }
        let broken_line = head(&git);
        git(&["checkout", "-q", "feature"]);
        write_doc(
            &root,
            "docs/adr-a.md",
            "---\nid: adr-a\ntitle: adr-a\nstatus: superseded\nsuperseded_by: adr-b\n---\na\n",
        );
        git(&["commit", "-qam", "supersede it on the branch"]);
        let superseding_line = head(&git);
        let (into, from) = match feature_first {
            true => (&superseding_line, &broken_line),
            false => (&broken_line, &superseding_line),
        };
        git(&["checkout", "-q", into]);
        git(&["merge", "--no-commit", from]);
        write_doc(
            &root,
            "docs/adr-a.md",
            "---\nid: adr-a\n  title: [broken resolution\nkind: adr\n---\na\n",
        );
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "merge, still broken"]);
        adr(&root, "adr-a", "active", "repaired");

        let data = run_json(nodex(&root).arg("check"));
        let flow: Vec<String> = data["violations"]
            .as_array()
            .expect("violations")
            .iter()
            .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
            .map(|v| v["message"].as_str().unwrap().to_string())
            .collect();
        // The write plane reads the same positions: `archived` leads from
        // `active`, and from `superseded` leads nowhere.
        nodex(&root).arg("build").assert().success();
        let wrote = nodex(&root)
            .args(["lifecycle", "set", "adr-a", "--status", "archived"])
            .output()
            .expect("lifecycle ran")
            .status
            .success();
        (flow, wrote)
    };

    for feature_first in [true, false] {
        let (findings, wrote) = judged(feature_first);
        assert!(
            findings.is_empty(),
            "one line held the record at active, which the repair keeps \
             (feature merged first: {feature_first}): {findings:?}"
        );
        assert!(
            wrote,
            "the move is declared from a position the record may have held \
             (feature merged first: {feature_first})"
        );
    }
}

#[test]
fn a_merge_that_took_one_side_s_bytes_reads_the_record_every_line_behind_it_held() {
    // Two lines each break the same document, one of them after moving the
    // record, and the merge resolves the conflict by keeping one side's
    // bytes. git reads a merge that matches a parent as no change at all and
    // offers that parent alone, so the side whose bytes won the resolution
    // would decide which record the repair is judged against — a conflict
    // resolution deciding a verdict about history it did not change.
    let judged = |resolution: &str| {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        fs::write(
            root.join("nodex.toml"),
            "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
             [statuses]\nallowed = [\"proposed\", \"active\", \"archived\"]\n\
             terminal = [\"archived\"]\n\
             [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
             transitions = { proposed = [\"active\"], active = [\"archived\"] }\n\
             [scope]\ninclude = [\"docs/**/*.md\"]\n\
             [detection]\norphan_ok_kinds = [\"adr\"]\n\
             [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n",
        )
        .unwrap();
        fs::write(root.join(".gitignore"), "_index/\n").unwrap();
        adr(&root, "adr-a", "proposed", "a");
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);
        let base = head(&git);
        git(&["branch", "still"]);
        adr(&root, "adr-a", "active", "a");
        git(&["commit", "-qam", "accept it on this line"]);
        write_doc(&root, "docs/adr-a.md", MOVED_BREAK);
        git(&["commit", "-qam", "break it after the move"]);
        let moved = head(&git);
        git(&["checkout", "-q", "still"]);
        write_doc(&root, "docs/adr-a.md", STILL_BREAK);
        git(&["commit", "-qam", "break it where it still stands proposed"]);
        git(&["merge", "--no-commit", &moved]);
        write_doc(&root, "docs/adr-a.md", resolution);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "resolve by keeping one side"]);
        adr(&root, "adr-a", "archived", "repaired");

        let findings = flow_findings(&root, &base);
        let unjudged: Vec<i64> =
            judged(nodex(&root).args(["check", "--since", &base]))["rule_coverage"]
                .as_array()
                .expect("coverage")
                .iter()
                .filter(|entry| entry["rule_id"].as_str().unwrap().starts_with("status_"))
                .map(|entry| entry["unjudged"].as_i64().unwrap())
                .collect();
        (findings, unjudged)
    };

    let kept_the_move = judged(MOVED_BREAK);
    let kept_the_other = judged(STILL_BREAK);
    assert!(
        kept_the_move.0.is_empty(),
        "one line accepted the record and the repair leads from there: {:?}",
        kept_the_move.0
    );
    assert_eq!(
        kept_the_move, kept_the_other,
        "both lines stand behind the merge, so which side's bytes resolved it \
         says nothing about where the record stood"
    );
    assert!(
        kept_the_move.1.iter().all(|count| *count > 0),
        "the merge itself reads the record two ways and is counted: {:?}",
        kept_the_move.1
    );
}

/// The document left unparseable by each line, differing so the merge has a
/// conflict to resolve and the resolution can keep either side whole.
const MOVED_BREAK: &str = "---\nid: adr-a\n  title: [broken after the move\nkind: adr\n---\na\n";

const STILL_BREAK: &str = "---\nid: adr-a\n  title: [broken where it stands\nkind: adr\n---\na\n";

#[test]
fn a_commit_whose_tree_will_not_graph_is_counted_rather_than_fatal() {
    // Two branches each author a document whose id resolves the same way and
    // a merge holds both: the tree is one this project's config refuses, and
    // history keeps it. Nothing short of rewriting that commit makes it
    // readable, so a run today reports what it could not read and judges the
    // rest.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"generic\", \"memo\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"proposed\"\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n\
         [scope]\ninclude = [\"docs/**/*.md\", \"notes/**/*.md\", \"memos/**/*.md\"]\n\
         [detection]\norphan_ok_kinds = [\"adr\", \"generic\", \"memo\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n\
         [[identity.kind_rules]]\nglob = \"notes/**/*.md\"\nkind = \"generic\"\n\
         [[identity.kind_rules]]\nglob = \"memos/**/*.md\"\nkind = \"memo\"\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
    adr(root, "adr-seed", "proposed", "seed");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author the seed"]);
    let base = head(&git);
    for (branch, path) in [("one", "notes/z.md"), ("two", "memos/z.md")] {
        git(&["checkout", "-q", "-b", branch, &base]);
        write_doc(root, path, "---\nid: z\ntitle: z\nstatus: active\n---\nz\n");
        git(&["add", "-A"]);
        git(&[
            "commit",
            "-q",
            "-m",
            "a document whose id resolves the same way",
        ]);
    }
    git(&["checkout", "-q", &base]);
    git(&["merge", "--no-edit", "one", "two"]);

    let unread = |data: &Value| -> usize {
        data["warnings"]
            .as_array()
            .map(|warnings| {
                warnings
                    .iter()
                    .filter(|w| w["code"] == "history_unread")
                    .count()
            })
            .unwrap_or_default()
    };
    // The working tree is the merge's own, which the build refuses, so the
    // run that reads it is the one that reports it.
    fs::remove_file(root.join("notes/z.md")).unwrap();
    fs::remove_file(root.join("memos/z.md")).unwrap();
    let plain = run_envelope(nodex(root).arg("check"));
    assert_eq!(unread(&plain), 1, "{plain}");
    // Naming the commit is not naming the cause, and the operator's question
    // is what the tree does that this config refuses.
    let said = unread_said(&plain);
    assert!(
        said[0].contains("duplicate node id \"z\""),
        "the refusal itself, not the context the build wrapped it in: {said:?}"
    );
    let ranged = run_envelope(nodex(root).args(["check", "--since", &base]));
    assert_eq!(unread(&ranged), 1, "{ranged}");
    // A record that commit carried is counted rather than read as arriving.
    let reach: Vec<(&str, u64)> = ranged["data"]["rule_coverage"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|c| {
            (
                c["rule_id"].as_str().unwrap(),
                c["unjudged"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(reach, [("status_entry", 1), ("status_transition", 1)]);
    // And the write plane runs: the commit is not the operator's to fix.
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["lifecycle", "set", "adr-seed", "--status", "active"])
        .assert()
        .success();
}

/// What the run said about history it could not read, in its own words.
fn unread_said(envelope: &Value) -> Vec<&str> {
    envelope["warnings"]
        .as_array()
        .map(|warnings| {
            warnings
                .iter()
                .filter(|w| w["code"] == "history_unread")
                .map(|w| w["message"].as_str().expect("a message"))
                .collect()
        })
        .unwrap_or_default()
}

/// What a run of `check --since base` said about the flow: the findings, the
/// reach each rule could not judge, and how many commits it could not read.
fn flow_reach(envelope: &Value) -> (Vec<String>, Vec<u64>, usize) {
    let data = &envelope["data"];
    let findings = data["violations"]
        .as_array()
        .expect("violations")
        .iter()
        .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|v| v["message"].as_str().unwrap().to_string())
        .collect();
    let unjudged = data["rule_coverage"]
        .as_array()
        .expect("coverage")
        .iter()
        .filter(|c| c["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|c| c["unjudged"].as_u64().unwrap())
        .collect();
    let unread = envelope["warnings"]
        .as_array()
        .map(|warnings| {
            warnings
                .iter()
                .filter(|w| w["code"] == "history_unread")
                .count()
        })
        .unwrap_or_default();
    (findings, unjudged, unread)
}

/// A document holding a second copy of `adr-a`'s id: a tree this project's
/// config refuses, which is how a commit the walk cannot read is made.
const REFUSED_TREE: &str = "---\nid: adr-a\ntitle: dup\nstatus: proposed\n---\ndup\n";

#[test]
fn a_line_the_walk_could_not_read_leaves_the_records_of_its_step_unjudged() {
    // One line of a merge ends at a tree the build refuses and the other
    // carries the record. What the unread line left the record at is what the
    // merge stands on just as much, so reading the line that could be read as
    // though it were the whole answer judges the step against a position that
    // may be no prior of it — and sanctions the move the other line made.
    let judged = |readable: bool| -> Value {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        flow_project(&root, "");
        adr(&root, "adr-a", "proposed", "a");
        adr(&root, "adr-b", "proposed", "b");
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "author them"]);
        let base = head(&git);
        adr(&root, "adr-a", "active", "a");
        git(&["commit", "-qam", "accept it"]);
        adr(&root, "adr-a", "superseded", "a");
        git(&["commit", "-qam", "supersede it"]);
        if !readable {
            write_doc(&root, "docs/dup.md", REFUSED_TREE);
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "a tree this config refuses"]);
        }
        let retired = head(&git);
        git(&["checkout", "-q", "-b", "elsewhere", &base]);
        adr(&root, "adr-b", "active", "b");
        git(&["commit", "-qam", "move the other record"]);
        git(&["merge", "--no-commit", &retired]);
        if !readable {
            fs::remove_file(root.join("docs/dup.md")).unwrap();
        }
        adr(&root, "adr-a", "active", "the merge puts it back");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "merge, adr-a back at active"]);
        reported(nodex(&root).args(["check", "--since", &base]))
    };

    let (findings, unjudged, unread) = flow_reach(&judged(true));
    assert_eq!(unread, 0, "every commit graphs: {findings:?}");
    assert_eq!(
        findings.len(),
        1,
        "the line that retired the record is a prior of the merge: {findings:?}"
    );
    assert!(
        findings[0].contains("\"superseded\" → \"active\""),
        "{findings:?}"
    );
    assert!(unjudged.iter().all(|counted| *counted == 0), "{unjudged:?}");

    let (findings, unjudged, unread) = flow_reach(&judged(false));
    assert_eq!(unread, 1, "the run says which commit it could not read");
    assert!(
        findings.is_empty(),
        "the line holding the prior could not be read, so nothing is judged \
         against the one that could: {findings:?}"
    );
    assert!(
        unjudged.iter().all(|counted| *counted >= 1),
        "and the reach counts what it did not judge: {unjudged:?}"
    );
}

#[test]
fn a_place_the_lines_last_agreed_that_will_not_graph_leaves_the_record_unjudged() {
    // The commit the lines last agreed at is a tree the build refuses. Which
    // line moved the record and which only carried it is what that commit
    // settles, so without it every line's position reads as one it moved to,
    // and the union of them accepts a move no line declares.
    let judged = |readable: bool| -> Value {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        flow_project(&root, "");
        adr(&root, "adr-a", "proposed", "a");
        adr(&root, "adr-b", "proposed", "b");
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "author them"]);
        // Where the lines last agreed, behind the step the run judges: a
        // ref the build refuses is a baseline that cannot be evaluated, so
        // this commit is one the walk reads and never one it is asked from.
        write_doc(
            &root,
            "docs/dup.md",
            match readable {
                true => "---\nid: adr-c\ntitle: c\nstatus: proposed\n---\nc\n",
                false => REFUSED_TREE,
            },
        );
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one more document"]);
        let agreed = head(&git);
        let line = |branch: &str, id: &str| {
            git(&["checkout", "-q", "-b", branch, &agreed]);
            git(&["rm", "-q", "docs/dup.md"]);
            adr(&root, id, "active", id);
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "accept a record on this line"]);
            head(&git)
        };
        let accepted = line("accepted", "adr-a");
        line("still", "adr-b");
        // Left uncommitted, so the run judges one step and the count below
        // answers for that step alone.
        git(&["merge", "--no-commit", &accepted]);
        adr(&root, "adr-a", "proposed", "the merge puts it back");
        reported(nodex(&root).arg("check"))
    };

    let (findings, unjudged, unread) = flow_reach(&judged(true));
    assert_eq!(unread, 0, "every commit graphs: {findings:?}");
    assert_eq!(
        findings.len(),
        1,
        "one line moved it and the merge moves it back: {findings:?}"
    );
    assert!(
        findings[0].contains("\"active\" → \"proposed\""),
        "{findings:?}"
    );
    assert_eq!(unjudged, [0, 0], "every record was judged");

    let (findings, unjudged, unread) = flow_reach(&judged(false));
    assert_eq!(unread, 1, "the run says which commit it could not read");
    assert!(
        findings.is_empty(),
        "nothing says which line moved it: {findings:?}"
    );
    assert_eq!(
        unjudged,
        [2, 2],
        "and both records the lines carry are counted rather than judged"
    );
}

#[test]
fn a_write_seam_attributes_no_move_where_the_walk_could_not_read_the_step() {
    // The write commits a step whose priors the walk cannot name, which is
    // what `status_transition` counts rather than judges — so the seam
    // refuses nothing there either. Reading the file's own bytes as the prior
    // instead refuses a move a line the walk could not narrow declares, and
    // makes the answer turn on a status somebody hand-wrote.
    let wrote = |readable: bool| -> Value {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        flow_project(&root, "");
        adr(&root, "adr-a", "proposed", "a");
        adr(&root, "adr-b", "proposed", "b");
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "author them"]);
        write_doc(
            &root,
            "docs/dup.md",
            match readable {
                true => "---\nid: adr-c\ntitle: c\nstatus: proposed\n---\nc\n",
                false => REFUSED_TREE,
            },
        );
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one more document"]);
        let agreed = head(&git);
        let line = |branch: &str, id: &str| {
            git(&["checkout", "-q", "-b", branch, &agreed]);
            git(&["rm", "-q", "docs/dup.md"]);
            adr(&root, id, "active", id);
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "accept a record on this line"]);
            head(&git)
        };
        let accepted = line("accepted", "adr-a");
        line("still", "adr-b");
        git(&["merge", "--no-commit", &accepted]);
        // Hand-written, and no prior of the step: one line accepted the
        // record and the other never moved it.
        adr(&root, "adr-a", "proposed", "hand-written back");
        nodex(&root).arg("build").assert().success();
        let output = nodex(&root)
            .args(["lifecycle", "set", "adr-a", "--status", "superseded"])
            .output()
            .expect("lifecycle ran");
        serde_json::from_slice(&output.stdout).expect("stdout is JSON")
    };

    let read = wrote(true);
    assert_eq!(
        read["ok"], true,
        "`active` is a prior of the step and the flow declares the move: {read}"
    );
    let unread = wrote(false);
    assert_eq!(
        unread["ok"], true,
        "the same step, with what it stands on unreadable: {unread}"
    );
    let warnings: Vec<&str> = unread["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(
        warnings.contains(&"history_unread"),
        "and the write says which commit it could not read: {unread}"
    );
}

#[test]
fn a_line_that_takes_a_record_out_of_the_flow_does_not_say_where_it_stood() {
    // One line of a merge gives the document a kind the flow does not govern
    // and touches nothing else; the other carries it on, governed, where the
    // lines last agreed left it. Deciding which line *moved* the record over
    // the whole position and narrowing to the governed kinds afterwards reads
    // the kind change as the move, drops the line that still holds the status
    // for having moved nothing, and then discards the one prior left — and a
    // terminal record reads as entering the flow again.
    let judged = |flip_the_kind: bool| -> Vec<String> {
        let tmp = scratch();
        let root = tmp.path().to_path_buf();
        flow_project(&root, "");
        write_doc(
            &root,
            "docs/adr-a.md",
            "---\nid: adr-a\ntitle: a\nkind: adr\nstatus: superseded\n---\na\n",
        );
        adr(&root, "adr-b", "proposed", "b");
        let git = git_runner(&root);
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "author them"]);
        let base = head(&git);
        git(&["checkout", "-q", "-b", "flipped"]);
        match flip_the_kind {
            true => write_doc(
                &root,
                "docs/adr-a.md",
                "---\nid: adr-a\ntitle: a\nkind: generic\nstatus: superseded\n---\na\n",
            ),
            false => adr(&root, "adr-c", "proposed", "c"),
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one line changes something else"]);
        git(&["checkout", "-q", "-b", "elsewhere", &base]);
        adr(&root, "adr-d", "proposed", "d");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "the other line adds a record"]);
        let elsewhere = head(&git);
        git(&["checkout", "-q", "flipped"]);
        git(&["merge", "--no-commit", &elsewhere]);
        write_doc(
            &root,
            "docs/adr-a.md",
            "---\nid: adr-a\ntitle: a\nkind: adr\nstatus: proposed\n---\na\n",
        );
        git(&["add", "-A"]);
        git(&[
            "commit",
            "-q",
            "-m",
            "merge, the retired record back at proposed",
        ]);
        reported(nodex(&root).args(["check", "--since", &base]))["data"]["violations"]
            .as_array()
            .expect("violations")
            .iter()
            .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
            // The commit is the one thing the two constructions differ in.
            .map(|v| {
                format!(
                    "{} {} {}→{} {}",
                    v["rule_id"],
                    v["node_id"],
                    v["details"]["from"],
                    v["details"]["to"],
                    v["details"]["declared"]
                )
            })
            .collect()
    };

    let flipped = judged(true);
    assert_eq!(
        flipped,
        judged(false),
        "the kind a line gave the document says nothing about where the \
         record stood on the line that kept it"
    );
    assert_eq!(
        flipped,
        [r#""status_transition" "adr-a" "superseded"→"proposed" []"#],
        "a terminal record was put back, which no transition declares"
    );
}

#[test]
fn a_clone_too_shallow_to_hold_where_the_lines_agreed_says_so() {
    // git answers a merge base beyond the cut exactly as it answers lines
    // that never met, so reading the failure as "these share no commit"
    // describes the project by an accident of how it was cloned — and leaves
    // the reach saying the rules guard nothing, with nothing to say why.
    let origin = scratch();
    let root = origin.path();
    flow_project(root, "");
    adr(root, "adr-a", "proposed", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author it"]);
    for filler in ["one", "two"] {
        fs::write(root.join(filler), filler).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", filler]);
    }
    git(&["checkout", "-q", "-b", "side", "HEAD~2"]);
    fs::write(root.join("aside"), "aside").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the other line goes its own way"]);
    git(&["checkout", "-q", "-"]);
    adr(root, "adr-a", "active", "a");
    git(&["commit", "-qam", "accept it on this line"]);

    let parent = scratch();
    let url = format!("file://{}", root.display());
    let clone_into = |name: &str, depth: &[&str]| -> std::path::PathBuf {
        let into = parent.path().join(name);
        let git_parent = git_runner(parent.path());
        let mut args = vec!["clone", "-q"];
        args.extend_from_slice(depth);
        args.extend_from_slice(&[&url, into.to_str().unwrap()]);
        assert!(git_parent(&args).status.success(), "cloned {name}");
        {
            let git_clone = git_runner(&into);
            let mut fetch = vec!["fetch", "-q"];
            fetch.extend_from_slice(depth);
            fetch.extend_from_slice(&["origin", "side:side"]);
            assert!(
                git_clone(&fetch).status.success(),
                "fetched side into {name}"
            );
            let mut merge = vec!["merge", "--no-commit"];
            if !depth.is_empty() {
                merge.push("--allow-unrelated-histories");
            }
            merge.push("side");
            git_clone(&merge);
        }
        // The move the other line never made, resolved into the merge.
        adr(&into, "adr-a", "proposed", "a");
        into
    };

    let whole = clone_into("whole", &[]);
    assert_eq!(
        flow_findings(&whole, "HEAD")
            .iter()
            .map(|(rule, id, _)| format!("{rule} {id}"))
            .collect::<Vec<_>>(),
        ["status_transition adr-a"],
        "the clone that holds where the lines agreed knows which one moved it"
    );

    let shallow = clone_into("shallow", &["--depth", "1"]);
    let envelope = reported(nodex(&shallow).arg("check"));
    let (findings, unjudged, unread) = flow_reach(&envelope);
    assert!(
        findings.is_empty(),
        "nothing can be judged there: {findings:?}"
    );
    assert!(
        unjudged.iter().all(|counted| *counted >= 1),
        "and the record is counted: {unjudged:?}"
    );
    assert_eq!(unread, 1, "with one warning saying why: {envelope}");
    let said = unread_said(&envelope);
    assert!(
        said[0].contains("holds no commit behind both") && said[0].contains("fetch"),
        "which says what the clone knows, and what a fetch would settle: {said:?}"
    );
}

#[test]
fn a_cut_behind_the_commit_that_broke_a_document_is_still_unknown() {
    // The clone holds the commit that broke the document and the commits that
    // kept it broken, but not the one that could read it. Each step back is
    // answerable until the line runs into the cut, and what the cut holds is
    // no more knowable from one step behind it than from the cut itself.
    let origin = scratch();
    let root = origin.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author adr-a"]);
    for broken in ["one", "two", "three"] {
        write_doc(
            root,
            "docs/adr-a.md",
            &format!("---\nid: adr-a\n  title: [broken {broken}\nkind: adr\n---\na\n"),
        );
        git(&["commit", "-qam", "break it further"]);
    }
    adr(root, "adr-a", "active", "repaired");
    git(&["commit", "-qam", "repair adr-a"]);

    let parent = scratch();
    let git_parent = git_runner(parent.path());
    let url = format!("file://{}", root.display());
    let clone = parent.path().join("cut");
    assert!(
        git_parent(&["clone", "-q", "--depth", "3", &url, clone.to_str().unwrap()])
            .status
            .success()
    );
    let data = run_json(nodex(&clone).args(["check", "--since", "HEAD~2"]));
    let flow: Vec<(&str, u64, u64)> = data["rule_coverage"]
        .as_array()
        .expect("coverage")
        .iter()
        .filter(|c| c["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|c| {
            (
                c["rule_id"].as_str().unwrap(),
                c["subjects"].as_u64().unwrap(),
                c["unjudged"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        data["violations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
            .count(),
        0,
        "the repair is not an arrival the clone can rule out: {data}"
    );
    assert_eq!(
        flow,
        [("status_entry", 1, 1), ("status_transition", 1, 1)],
        "counted, not judged: {data}"
    );

    // The same range where the history is there: the record is the one it was.
    let full = parent.path().join("full");
    assert!(
        git_parent(&["clone", "-q", &url, full.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(flow_findings(&full, "HEAD~2"), vec![]);
}

#[test]
fn a_write_seam_judges_the_move_from_head_without_a_baseline() {
    // No `rules.immutable_baseline`: the document says `active`, uncommitted,
    // while `HEAD` holds `proposed`. `proposed → superseded` is the step the
    // write would commit, so the seam refuses it with the transition — not
    // only when a baseline happens to be configured.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "proposed", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    adr(root, "adr-a", "active", "a");
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["lifecycle", "set", "adr-a", "--status", "superseded"])
        .output()
        .expect("lifecycle ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["error"]["code"], "INVALID_TRANSITION",
        "{envelope}"
    );

    // A draft scaffolded and moved before it is ever committed would enter the
    // flow accepted.
    adr(root, "adr-b", "proposed", "b");
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["lifecycle", "set", "adr-b", "--status", "active"])
        .assert()
        .failure();
}

#[test]
fn a_plain_check_judges_only_the_change_being_made() {
    // A step finding is about a commit. Judged again on every run against a
    // baseline that does not move, it could never clear; a plain `check`
    // judges the uncommitted change, and a range is judged when asked for.
    // The body lock is what binds the release tag as a baseline at all.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(
        root,
        "[rules]\nimmutable_baseline = \"v1\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    );
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["tag", "v1"]);
    adr(root, "adr-a", "proposed", "a");
    git(&["commit", "-qam", "demote"]);
    adr(root, "adr-a", "active", "a");
    git(&["commit", "-qam", "restore"]);

    let output = nodex(root).arg("check").output().expect("check ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    let flow: Vec<&Value> = envelope["data"]["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["rule_id"].as_str().unwrap().starts_with("status_"))
        .collect();
    assert!(flow.is_empty(), "{flow:?}");
    assert_eq!(flow_findings(root, "v1").len(), 1);

    adr(root, "adr-a", "proposed", "a");
    let output = nodex(root).arg("check").output().expect("check ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert!(
        envelope["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["rule_id"] == "status_transition" && v["details"]["commit"].is_null())
    );
}

#[test]
fn a_range_includes_the_commits_a_merge_under_way_brings_in() {
    // Before the merge commit exists, no commit reaches the side branch; the
    // commit about to be made will. The same range judged before and after
    // committing the merge must agree.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-x", "active", "x");
    adr(root, "adr-y", "proposed", "y");
    let git = git_runner(root);
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["tag", "base"]);
    git(&["checkout", "-q", "-b", "side"]);
    adr(root, "adr-x", "proposed", "x");
    git(&["commit", "-qam", "side: demote"]);
    git(&["checkout", "-q", "main"]);
    adr(root, "adr-y", "active", "y");
    git(&["commit", "-qam", "main: accept"]);
    git(&["merge", "-q", "--no-commit", "side"]);

    let before = flow_findings(root, "base");
    git(&["commit", "-q", "-m", "merge side"]);
    assert_eq!(before, flow_findings(root, "base"));
    assert_eq!(before.len(), 1, "{before:?}");
}

#[test]
fn a_branch_with_no_commit_yet_judges_its_first_commit_as_a_root() {
    // No commit, or a fresh orphan branch: the next commit records no
    // parents, so every record it carries enters the flow there — and nothing
    // about that is an error.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    let git = git_runner(root);
    git(&["init", "-q", "-b", "main"]);
    nodex(root)
        .args([
            "scaffold",
            "--kind",
            "adr",
            "--title",
            "first",
            "--path",
            "docs/first.md",
        ])
        .assert()
        .success();
    assert_eq!(flow_findings(root, "HEAD").len(), 0);
    adr(root, "adr-a", "active", "a");
    let output = nodex(root).arg("check").output().expect("check ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert!(
        envelope["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["rule_id"] == "status_entry" && v["node_id"] == "adr-a")
    );

    fs::remove_file(root.join("docs/adr-a.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root"]);
    git(&["checkout", "-q", "--orphan", "fresh"]);
    let output = nodex(root)
        .args(["check", "--since", "main"])
        .output()
        .expect("check ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(envelope["ok"], true, "{envelope}");
}

#[test]
fn a_document_git_ignores_takes_no_step() {
    // Nothing git ignores can reach a commit, so a draft kept there enters no
    // flow — at `active` or anywhere else, and however it is moved — and the
    // reach says so rather than leaving it out.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    fs::write(root.join(".gitignore"), "_index/\ndocs/drafts/\n").unwrap();
    adr(root, "adr-a", "proposed", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    write_doc(
        root,
        "docs/drafts/adr-d.md",
        "---\nid: adr-d\ntitle: d\nstatus: active\n---\nd\n",
    );
    assert_eq!(flow_findings(root, "HEAD"), vec![]);
    let data = run_json(nodex(root).arg("check"));
    let reach: Vec<(&str, u64, u64)> = data["rule_coverage"]
        .as_array()
        .expect("coverage")
        .iter()
        .filter(|c| c["rule_id"].as_str().unwrap().starts_with("status_"))
        .map(|c| {
            (
                c["rule_id"].as_str().unwrap(),
                c["subjects"].as_u64().unwrap(),
                c["unjudged"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        reach,
        [("status_entry", 1, 1), ("status_transition", 1, 1)],
        "adr-a judged, adr-d selected and never judged: {data}"
    );
    nodex(root).arg("build").assert().success();
    // With no commit to step from, a write judges the move from the status
    // the document carries, as outside a git work tree.
    let output = nodex(root)
        .args(["lifecycle", "set", "adr-d", "--status", "proposed"])
        .output()
        .expect("lifecycle ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["error"]["code"], "INVALID_TRANSITION",
        "{envelope}"
    );
    nodex(root)
        .args(["lifecycle", "set", "adr-d", "--status", "superseded"])
        .assert()
        .success();

    // The same draft outside the ignored directory is a record entering the
    // flow accepted.
    write_doc(
        root,
        "docs/adr-e.md",
        "---\nid: adr-e\ntitle: e\nstatus: active\n---\ne\n",
    );
    assert_eq!(
        flow_findings(root, "HEAD"),
        vec![("status_entry".to_string(), "adr-e".to_string(), None)]
    );
}

#[test]
fn a_record_an_earlier_step_judged_is_still_unjudged_where_no_step_holds_it() {
    // The record was tracked and judged inside the range, and then dropped
    // out of git's reach. What a past step judged is no answer for where it
    // stands now, so the range's reach reads what a plain run's does.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "proposed", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "author it"]);
    let base = head(&git);
    adr(root, "adr-a", "proposed", "an edit the flow allows");
    git(&["commit", "-qam", "a step that judged it"]);
    git(&["rm", "-q", "--cached", "docs/adr-a.md"]);
    fs::write(root.join(".gitignore"), "_index/\ndocs/adr-a.md\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-q", "-m", "git stops carrying it"]);
    // An undeclared move, made where no commit can record it.
    adr(root, "adr-a", "superseded", "an edit no step judges");

    let reach = |args: &[&str]| -> Vec<(String, u64, u64)> {
        run_json(nodex(root).args(args))["rule_coverage"]
            .as_array()
            .expect("coverage")
            .iter()
            .filter(|c| c["rule_id"].as_str().unwrap().starts_with("status_"))
            .map(|c| {
                (
                    c["rule_id"].as_str().unwrap().to_string(),
                    c["subjects"].as_u64().unwrap(),
                    c["unjudged"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    assert_eq!(
        reach(&["check"]),
        [
            ("status_entry".to_string(), 0, 1),
            ("status_transition".to_string(), 0, 1)
        ]
    );
    assert_eq!(
        reach(&["check", "--since", &base]),
        [
            ("status_entry".to_string(), 1, 1),
            ("status_transition".to_string(), 1, 1)
        ],
        "judged at the commit that held it, and unjudged where it stands now"
    );
}

#[test]
fn a_document_a_commit_could_not_parse_holds_the_record_it_last_held() {
    // Broken in one commit and repaired — in place or at a new path — the
    // record is the one it was. Never readable before the repair, it enters
    // there: a broken commit is no way past the entry status.
    let tmp = scratch();
    let root = tmp.path();
    flow_project(root, "");
    adr(root, "adr-a", "active", "a");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["tag", "base"]);
    let broken = "---\nid: [\n---\nbroken\n";

    write_doc(root, "docs/adr-a.md", broken);
    git(&["commit", "-qam", "break adr-a"]);
    fs::remove_file(root.join("docs/adr-a.md")).unwrap();
    adr(root, "adr-a-moved", "active", "a");
    fs::rename(root.join("docs/adr-a-moved.md"), root.join("docs/moved.md")).unwrap();
    write_doc(
        root,
        "docs/moved.md",
        "---\nid: adr-a\ntitle: adr-a\nstatus: active\n---\na\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "repair adr-a at a new path"]);
    assert_eq!(flow_findings(root, "base"), vec![]);

    write_doc(root, "docs/adr-x.md", broken);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "add adr-x unparseable"]);
    adr(root, "adr-x", "active", "x");
    git(&["commit", "-qam", "repair adr-x"]);
    let repaired = head(&git);
    assert_eq!(
        flow_findings(root, "base"),
        vec![(
            "status_entry".to_string(),
            "adr-x".to_string(),
            Some(repaired)
        )]
    );
}
