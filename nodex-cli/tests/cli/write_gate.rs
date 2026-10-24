//! The mutation gate: a write refuses exactly the findings it introduces.

use crate::support::{
    envelope_of, git_runner, nodex, run_envelope, run_json, scratch, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

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
///
/// The precondition is that the project the move produces graphs — which is
/// not the same question as whether a baseline refuses it, and a project with
/// no `rules.immutable_baseline` never asks the second. Downstream of
/// `fs::rename` the reference rewrite can only degrade to a warning, so a
/// project `nodex build` refuses must stop the move while refusing still costs
/// nothing.
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
    assert_eq!(
        rules,
        ["orphan", "required_field", "required_field"],
        "{env}"
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
         [schema.enums]\nstage = [\"draft: early\", \"final\"]\n\
         [detection]\norphan_ok_kinds = [\"generic\"]\n",
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
    assert_eq!(
        rules,
        ["orphan", "orphan", "unique_numbering"],
        "the same conflict, unmoved"
    );
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
        .args(["--path", "docs/target.md", "--force", "--id", "replacement"])
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
fn a_project_that_reds_a_dangling_reference_refuses_the_move_that_invents_one() {
    // The successor spelling makes the pattern capture `shadow`, which
    // names no document. `mints_nothing` permits that on purpose — it is
    // no edge — and the question of what a dangling reference is worth
    // belongs to the project, which answers it in `unresolved_policy`.
    // The write seam applies that answer to the whole candidate, so the
    // guard that cannot see violations and the gate that cannot see edges
    // between them leave nothing unasked.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '(shadow)'\nrelation = \"minted\"\n\
         [[detection.unresolved_policy]]\nname = \"dangling-capture\"\n\
         cause = \"missing\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "old.md",
        "---\nid: moved\ntitle: M\nkind: generic\nstatus: active\n---\nm\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[t](old.md)\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();
    let output = nodex(root)
        .args(["rename", "old.md", "shadowy.md"])
        .output()
        .expect("rename ran");
    assert_eq!(output.status.code(), Some(2), "the write gate refuses");
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        env.pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("unresolved_reference/dangling-capture")),
        "the refusal names the rule the project declared: {env}"
    );
    assert!(
        root.join("old.md").exists() && !root.join("shadowy.md").exists(),
        "the move it refused is the move it did not make"
    );
    assert!(
        fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("[t](old.md)"),
        "nor the rewrite that went with it"
    );
    nodex(root).arg("check").assert().success();
}

/// A project whose ADRs move `proposed → active → superseded`, with no
/// `orphan` noise, written but not committed.
#[test]
fn a_rename_refused_by_a_rule_that_is_not_a_lock_says_which_rule_and_what_it_said() {
    // A move into a governed kind's territory makes the record enter the
    // flow, which no lock refuses. Naming a lock there would send the
    // operator to revert a document nothing is wrong with, and to look for a
    // finding `nodex check` does not report: the document as it stands is
    // outside the flow.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"proposed\"\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n\
         [scope]\ninclude = [\"docs/**/*.md\", \"notes/**/*.md\"]\n\
         [detection]\norphan_ok_kinds = [\"adr\", \"generic\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n\
         [[identity.kind_rules]]\nglob = \"notes/**/*.md\"\nkind = \"generic\"\n",
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
    write_doc(
        root,
        "notes/n1.md",
        "---\nid: n1\ntitle: N\nstatus: active\n---\nN\n",
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    // The document as it stands is clean, which is why the lock wording
    // would be false in both of its claims.
    assert_eq!(
        run_json(nodex(root).arg("check"))["violations"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let output = nodex(root)
        .args(["rename", "notes/n1.md", "docs/n1.md"])
        .output()
        .expect("rename ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["error"]["code"], "CONTENT_VIOLATIONS",
        "{envelope}"
    );
    let message = envelope["error"]["message"].as_str().expect("a message");
    assert!(
        message.contains("status_entry") && message.contains("enters statuses.flow"),
        "names the rule and what it said: {message}"
    );
    assert!(
        !message.contains("the lock named") && !message.contains("reports the difference"),
        "no lock refused this: {message}"
    );
}
