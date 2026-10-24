//! `frontmatter_immutable` and `body_immutable`: what a lock refuses, in
//! `check` and at every write seam.

use crate::support::{
    git_runner, judged, nodex, run_envelope, run_json, scratch, snapshot_lock_project, warning_msg,
    write_doc,
};
use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

/// A lock holds what the baseline holds. A committed ADR frozen from creation
/// keeps the citation it was written with; a new one is still being written,
/// and this is the edit where its citation of a superseded decision can change.
#[test]
fn a_creation_lock_holds_the_committed_citation_and_not_the_new_one() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**\"]\n[kinds]\nallowed = [\"generic\", \"adr\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"adr\"\nmode = \"frozen\"\n\
         trigger = \"creation\"\nkinds = [\"adr\"]\n",
    )
    .unwrap();
    let adr = |id: &str, status: &str, extra: &str, body: &str| {
        format!("---\nid: {id}\ntitle: {id}\nkind: adr\nstatus: {status}\n{extra}---\n{body}\n")
    };
    write_doc(
        root,
        "docs/0001.md",
        &adr(
            "adr-0001",
            "superseded",
            "superseded_by: adr-0002\n",
            "# One",
        ),
    );
    write_doc(
        root,
        "docs/0002.md",
        &adr("adr-0002", "active", "", "# Two"),
    );
    write_doc(
        root,
        "docs/0003.md",
        &adr("adr-0003", "active", "", "Builds on [ADR-0001](0001.md)."),
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    write_doc(
        root,
        "docs/0004.md",
        &adr("adr-0004", "active", "", "Builds on [ADR-0001](0001.md)."),
    );

    let check = run_json(nodex(root).arg("check"));
    let citers: Vec<&str> = check["violations"]
        .as_array()
        .expect("violations")
        .iter()
        .filter(|v| v["rule_id"] == "superseded_reference")
        .map(|v| v["node_id"].as_str().unwrap())
        .collect();
    assert_eq!(citers, ["adr-0004"], "{check}");
}

#[test]
fn lifecycle_review_refuses_a_frontmatter_immutable_locked_field_on_a_terminal_doc() {
    // `review` is the only lifecycle action that reaches an already-
    // terminal doc (the terminal guard blocks set/supersede), and it
    // writes `reviewed`. A `frontmatter_immutable` rule that freezes
    // `reviewed` once terminal must refuse the write — otherwise lifecycle
    // writes a doc its own `check --since baseline` then flags (the
    // symmetric-guards / self-consistency rule).
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[rules.frontmatter_immutable]]\nname = \"freeze-meta\"\nfields = [\"reviewed\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nstatus: superseded\nsuperseded_by: b\nreviewed: 2026-01-01\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\nstatus: active\n---\n# B\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();

    // review on the terminal+locked doc → refused, doc untouched.
    let out = nodex(root)
        .args(["lifecycle", "review", "a"])
        .output()
        .expect("ran");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("reviewed: 2026-01-01"),
        "refused review leaves reviewed untouched"
    );

    // review on the active (non-terminal) doc → the lock is inert.
    nodex(root)
        .args(["lifecycle", "review", "b"])
        .assert()
        .success();
}

/// A lock is about the record standing at a path, never about where the
/// bytes arriving there came from.
///
/// `rename` asked the baseline only when the graph carried the source, so a
/// document authored outside the scope could be moved onto a frozen record's
/// path and replace it — the seam reporting success while `check` went red on
/// the lock it had never consulted.
#[test]
fn a_move_onto_a_frozen_record_is_refused_however_the_source_arrived() {
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
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: X\ntitle: X\nkind: generic\nstatus: archived\n---\nORIGINAL\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    // The frozen document is gone from the working tree, and a replacement
    // for its path is authored where the graph does not reach.
    fs::remove_file(root.join("docs/a.md")).unwrap();
    write_doc(
        root,
        "drafts/x.md",
        "---\nid: X\ntitle: X\nkind: generic\nstatus: archived\n---\nREPLACED\n",
    );
    let output = nodex(root)
        .args(["rename", "drafts/x.md", "docs/a.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "the move is refused");
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        msg.contains("body_immutable/frozen"),
        "names the lock: {msg}"
    );
    assert!(root.join("drafts/x.md").exists() && !root.join("docs/a.md").exists());
}

/// A record travels under its id, so a frozen one that merely moved has left
/// its path free.
///
/// The destination guard asked the baseline by path alone while its own
/// message asserted the record was gone — so a path a legal rename had
/// already vacated stayed refused, on a mutation `check` reads as nothing at
/// all.
#[test]
fn a_frozen_record_that_moved_leaves_its_path_free() {
    fn fixture() -> TempDir {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
             [statuses]\nallowed = [\"active\", \"archived\"]\n\
             terminal = [\"archived\"]\ninitial = \"active\"\n\
             [rules]\nimmutable_baseline = \"HEAD\"\n\
             [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
        )
        .unwrap();
        write_doc(
            root,
            "docs/b.md",
            "---\nid: X\ntitle: X\nkind: generic\nstatus: archived\n---\n# B\n",
        );
        write_doc(
            root,
            "docs/a.md",
            "---\nid: A\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
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

    // The frozen record moves under its own id, which is legal; its old path
    // now holds nothing frozen.
    let moved = fixture();
    let root = moved.path();
    nodex(root)
        .args(["rename", "docs/b.md", "docs/c.md"])
        .assert()
        .success();
    nodex(root)
        .args(["rename", "docs/a.md", "docs/b.md"])
        .assert()
        .success();
    nodex(root).args(["build", "--full"]).assert().success();
    nodex(root).arg("check").assert().success();

    // The record genuinely gone is still refused.
    let lost = fixture();
    let root = lost.path();
    fs::remove_file(root.join("docs/b.md")).unwrap();
    let output = nodex(root)
        .args(["rename", "docs/a.md", "docs/b.md"])
        .output()
        .expect("ran");
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(msg.contains("body_immutable/frozen"), "{msg}");
    assert!(
        !msg.contains("   "),
        "the message is one sentence, not padding: {msg}"
    );
}

/// A record carried back onto its own path loses nothing, so nothing refuses
/// it.
///
/// The destination guard asked whether the frozen id survives the move — of
/// the project as it stands, where the record is missing whichever document
/// lands there. Restoring the record itself was refused for destroying it,
/// on a move that left the project byte-identical to the baseline.
#[test]
fn a_frozen_record_returning_to_its_own_path_is_not_a_record_lost() {
    fn fixture() -> TempDir {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
             [statuses]\nallowed = [\"active\", \"archived\"]\n\
             terminal = [\"archived\"]\ninitial = \"active\"\n\
             [rules]\nimmutable_baseline = \"HEAD\"\n\
             [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
        )
        .unwrap();
        write_doc(
            root,
            "docs/a.md",
            "---\nid: X\ntitle: X\nkind: generic\nstatus: archived\n---\n# A\n",
        );
        {
            let git = git_runner(root);
            git(&["init", "-q"]);
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "base"]);
        }
        // The record leaves the graph entirely — parked outside scope, where
        // no rule can see it and the path it came from reads as free.
        fs::create_dir_all(root.join("drafts")).unwrap();
        fs::rename(root.join("docs/a.md"), root.join("drafts/x.md")).unwrap();
        tmp
    }

    let restored = fixture();
    let root = restored.path();
    nodex(root)
        .args(["rename", "drafts/x.md", "docs/a.md"])
        .assert()
        .success();
    nodex(root).args(["build", "--full"]).assert().success();
    nodex(root).arg("check").assert().success();

    // A different document landing on the same freed path still replaces
    // frozen history, and is still refused.
    let replaced = fixture();
    let root = replaced.path();
    write_doc(
        root,
        "drafts/y.md",
        "---\nid: Y\ntitle: Y\nkind: generic\nstatus: active\n---\n# Y\n",
    );
    let output = nodex(root)
        .args(["rename", "drafts/y.md", "docs/a.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(msg.contains("body_immutable/frozen"), "{msg}");
}

#[test]
fn rewrite_lock_applies_append_only_mode_like_check() {
    // The probe must mirror `check`'s mode handling: `append_only`
    // permits a body change that keeps the baseline lines as a prefix.
    // A reference rewrite inside an APPENDED region preserves that
    // prefix, so check allows it and the probe must too — while a
    // `frozen` doc's baseline-region rewrite stays locked.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"log\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"append-log\"\nmode = \"append_only\"\nkinds = [\"log\"]\n\
         [[rules.body_immutable]]\nname = \"frozen-spec\"\nmode = \"frozen\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: log-a\ntitle: A\nkind: log\nstatus: archived\n---\n# A\noriginal entry\n",
    );
    write_doc(
        root,
        "docs/f.md",
        "---\nid: generic-f\ntitle: F\nkind: generic\nstatus: archived\n---\n# F\nSee [t](t.md).\n",
    );
    write_doc(
        root,
        "docs/t.md",
        "---\nid: generic-t\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Append a referencing line to the append_only doc (a legitimate
    // append that keeps the baseline body as a prefix).
    write_doc(
        root,
        "docs/a.md",
        "---\nid: log-a\ntitle: A\nkind: log\nstatus: archived\n---\n# A\noriginal entry\nlater: see [t](t.md)\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["rename", "docs/t.md", "docs/t2.md"]));
    let updated: Vec<&str> = env
        .pointer("/data/references_updated")
        .and_then(Value::as_array)
        .expect("updated")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // append_only doc's appended-region link IS rewritten (check allows it).
    assert!(updated.contains(&"docs/a.md"), "{updated:?}");
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("(t2.md)"),
        "append_only appended-region rewrite proceeds"
    );
    // frozen doc's baseline-region link is skipped.
    assert!(!updated.contains(&"docs/f.md"), "{updated:?}");
    assert!(
        fs::read_to_string(root.join("docs/f.md"))
            .unwrap()
            .contains("(t.md)"),
        "frozen baseline-region rewrite is locked"
    );
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["check", "--since", "HEAD"])
        .assert()
        .success();
}

#[test]
fn rewrite_lock_gates_on_baseline_status_not_working_tree_status() {
    // The probe must mirror `check`: a doc terminal at the baseline whose
    // status was changed to non-terminal in the working tree is STILL
    // body-locked (check gates on the before-snapshot status). The rewrite
    // must be skipped, not silently performed — otherwise rename defaces a
    // body that `check --since` would flag.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"adr\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\nkinds = [\"adr\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: ADR-a\ntitle: A\nkind: adr\nstatus: archived\n---\n# A\nSee [b](b.md).\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Un-terminalize A in the working tree only (uncommitted).
    write_doc(
        root,
        "docs/a.md",
        "---\nid: ADR-a\ntitle: A\nkind: adr\nstatus: active\n---\n# A\nSee [b](b.md).\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["rename", "docs/b.md", "docs/c.md"]));
    assert!(
        env.pointer("/data/references_updated")
            .and_then(Value::as_array)
            .is_some_and(|a| a.is_empty()),
        "A's body rewrite is skipped: {env}"
    );
    assert!(
        env.get("warnings")
            .and_then(Value::as_array)
            .is_some_and(|w| w
                .iter()
                .filter_map(warning_msg)
                .any(|s| s.contains("locked"))),
        "skip warning surfaced: {env}"
    );
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("(b.md)"),
        "frozen body keeps its baseline spelling"
    );
}

#[test]
fn retarget_proceeds_when_only_the_frontmatter_changes_on_a_body_locked_doc() {
    // body_immutable protects the body only. A frontmatter-relation
    // retarget that leaves the body untouched must NOT be blocked by a
    // body lock — `check` would not flag it (no body change), so the probe
    // must not over-protect.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"adr\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\nkinds = [\"adr\"]\n",
    )
    .unwrap();
    // Archived adr, body-locked, with an id relation in frontmatter and
    // NO id reference in the body.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: ADR-a\ntitle: A\nkind: adr\nstatus: archived\nrelated: generic-old\n---\n# A\nno id reference in the body.\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: generic-old\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: generic-new\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["retarget", "generic-old", "generic-new"]));
    assert!(
        env.pointer("/data/references_updated")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().filter_map(Value::as_str).any(|s| s == "docs/a.md")),
        "the frontmatter-only retarget proceeds: {env}"
    );
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("generic-new"),
        "the locked-body doc's relation was repointed"
    );
}

#[test]
fn retarget_skips_a_relation_field_locked_by_frontmatter_immutable() {
    // The frontmatter twin of the body lock: a doc committed terminal at
    // the immutable_baseline whose `related:` is locked by a
    // frontmatter_immutable block must not be repointed — the rewrite
    // would change a locked field that `check` flags, so the probe's
    // relation-field arm engages and the doc stays byte-identical.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.frontmatter_immutable]]\nname = \"sealed-relations\"\nfields = [\"related\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/sealed.md",
        "---\nid: generic-sealed\ntitle: S\nkind: generic\nstatus: archived\nrelated: generic-old\n---\n# S\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: generic-old\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: generic-new\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let sealed_before = fs::read_to_string(root.join("docs/sealed.md")).unwrap();

    let env = run_envelope(nodex(root).args(["retarget", "generic-old", "generic-new"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings.iter().filter_map(warning_msg).any(
            |w| w.contains("sealed.md") && w.contains("frontmatter_immutable/sealed-relations")
        ),
        "{warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/sealed.md")).unwrap(),
        sealed_before,
        "sealed relations untouched"
    );
}

#[test]
fn moved_file_lock_probe_judges_kind_at_the_before_path() {
    // A kind-scoped body lock gates on the *before* kind. A cross-kind
    // move of a doc committed terminal at the baseline must not slip its
    // own link rebase past the lock via the destination path's kind
    // inference.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"adrs/**/*.md\", \"notes/**/*.md\", \"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"adr\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.kind_rules]]\nglob = \"adrs/**\"\nkind = \"adr\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"adr-frozen\"\nmode = \"frozen\"\nkinds = [\"adr\"]\n",
    )
    .unwrap();
    // No frontmatter `kind:` — the kind is path-inferred (adr at the old
    // path, generic at the new one). The body link needs rebasing after
    // a depth-changing move, so the rewrite would fire without the lock.
    write_doc(
        root,
        "adrs/x.md",
        "---\nid: adr-x\ntitle: X\nstatus: archived\n---\n# X\n\nSee [d](../docs/d.md).\n",
    );
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["rename", "adrs/x.md", "notes/sub/x.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("body_immutable/adr-frozen")),
        "the before-kind lock engages: {warnings:?}"
    );
    let moved = fs::read_to_string(root.join("notes/sub/x.md")).unwrap();
    assert!(
        moved.contains("(../docs/d.md)"),
        "frozen body keeps its original link spelling: {moved}"
    );
}

#[test]
fn rename_and_retarget_skip_locked_bodies_with_a_warning() {
    // Writer-skips for immutability locks, mirroring the symlink
    // discipline: a rewrite check would flag as a body_immutable (or a
    // relation-field frontmatter_immutable) violation is not performed —
    // frozen history keeps its original spelling, surfaced as a warning.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen-when-archived\"\nmode = \"frozen\"\n\
         [[rules.frontmatter_immutable]]\nname = \"seal-related\"\nfields = [\"related\"]\n",
    )
    .unwrap();
    // The archived doc references the soon-to-move target both in body
    // (a path link the rename rebases — body_immutable territory) and
    // frontmatter (an id relation retarget repoints — frontmatter_immutable
    // territory); the active doc references it in body only.
    write_doc(
        root,
        "docs/frozen.md",
        "---\nid: generic-frozen\ntitle: F\nkind: generic\nstatus: archived\nrelated: generic-target\n---\n# F\n\nSee [t](target.md).\n",
    );
    write_doc(
        root,
        "docs/live.md",
        "---\nid: generic-live\ntitle: L\nkind: generic\nstatus: active\n---\n# L\n\nSee [t](target.md).\n",
    );
    write_doc(
        root,
        "docs/target.md",
        "---\nid: generic-target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    write_doc(
        root,
        "docs/successor.md",
        "---\nid: generic-successor\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let frozen_before = fs::read_to_string(root.join("docs/frozen.md")).unwrap();

    // rename: the live doc is rewritten; the frozen doc is skipped with
    // a lock warning and left byte-identical.
    let env = run_envelope(nodex(root).args(["rename", "docs/target.md", "docs/target-v2.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(env["data"]["completion"], "partial");
    let updated: Vec<&str> = env
        .pointer("/data/references_updated")
        .and_then(Value::as_array)
        .expect("updated list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(updated.contains(&"docs/live.md"), "{updated:?}");
    assert!(!updated.contains(&"docs/frozen.md"), "{updated:?}");
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("frozen.md") && w.contains("body_immutable/frozen-when-archived")),
        "{warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/frozen.md")).unwrap(),
        frozen_before,
        "frozen history untouched"
    );

    // retarget: the frozen doc's locked `related` relation is left
    // untouched — the rewrite would change a frontmatter_immutable field,
    // so it is skipped with that lock's warning.
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["retarget", "generic-target", "generic-successor"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(env["data"]["completion"], "partial");
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("frozen.md") && w.contains("frontmatter_immutable/seal-related")),
        "{warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/frozen.md")).unwrap(),
        frozen_before,
        "frozen history untouched by retarget"
    );
}

#[test]
fn append_section_takes_a_correction_and_refuses_any_other_append() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"active\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"record\"\nmode = \"append_only\"\n\
         trigger = \"creation\"\nappend_section = \"## Corrections\"\n",
    )
    .unwrap();
    let record = "---\nid: adr-1\ntitle: One\nkind: generic\nstatus: active\n---\n\
                  # One\n\n## Decision\n\nWe do X.\n";
    write_doc(root, "docs/adr.md", record);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    write_doc(
        root,
        "docs/adr.md",
        &format!("{record}\n## Corrections\n\n- 2026-09-15 — the bound is 32, not 64\n"),
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();

    write_doc(
        root,
        "docs/adr.md",
        &format!("{record}Also Y.\n\n## Corrections\n\n- 2026-09-15 — fixed\n"),
    );
    nodex(root).arg("build").assert().success();
    let out = nodex(root).arg("check").assert().failure().code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    let details = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations array")
        .iter()
        .find(|v| v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/record"))
        .and_then(|v| v.get("details"))
        .cloned()
        .unwrap_or_else(|| panic!("body_immutable/record violation expected: {env}"));
    assert_eq!(
        details.get("append_section").and_then(Value::as_str),
        Some("## Corrections"),
        "{details}"
    );
    assert_eq!(
        details.get("refusal").and_then(Value::as_str),
        Some("outside_section"),
        "{details}"
    );
}

/// The move is the half that cannot be undone, so every refusal has to happen
/// before it. Asking the rules for a verdict means rebuilding the project with
/// the rewrites overlaid, and a project that does not build has no verdict to
/// give — if that refusal arrived after `fs::rename`, the rename would move a
/// file and then decline to rebase its references, leaving the tree worse than
/// it found it. `retarget` establishes the same precondition by building first.
/// A move writes no bytes, so nothing about it is expressible as a rewrite —
/// and a gate that only sees rewrites cannot see it. What it does change is the
/// document's *path*, and every field config derives from a path moves with it:
/// `kind` through `identity.kind_rules`. A lock on such a field fires at check
/// time on a terminal document that crossed a rule boundary, so the seam has to
/// refuse the move — before it happens, because afterwards a refusal cannot be
/// honoured.
#[test]
fn rename_refuses_a_move_that_would_change_a_locked_path_derived_field() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"adr\", \"note\", \"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/adr/**\"\nkind = \"adr\"\n\
         [[identity.kind_rules]]\nglob = \"docs/notes/**\"\nkind = \"note\"\n\
         [[rules.frontmatter_immutable]]\nname = \"kind-locked\"\nfields = [\"kind\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n",
    )
    .unwrap();
    // No frontmatter `kind:` — it is path-derived, so the move changes it.
    write_doc(
        project,
        "docs/adr/a.md",
        "---\nid: adr-a\ntitle: A\nstatus: superseded\n---\n# A\n",
    );
    write_doc(
        project,
        "docs/notes/k.md",
        "---\nid: note-k\ntitle: K\nkind: note\nstatus: active\n---\n# K\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a terminal adr"]);

    let output = nodex(project)
        .args(["rename", "docs/adr/a.md", "docs/notes/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("LOCKED"),
        "the seam refuses what `check` would red after the move: {envelope}"
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("frontmatter_immutable/kind-locked"),
        "and names the rule that governs it: {envelope}"
    );
    assert!(
        project.join("docs/adr/a.md").exists() && !project.join("docs/notes/a.md").exists(),
        "and nothing moved, so the refusal is honourable"
    );

    // The same move within the kind's own directory changes nothing locked.
    nodex(project)
        .args(["rename", "docs/adr/a.md", "docs/adr/renamed.md"])
        .assert()
        .success();
    assert!(project.join("docs/adr/renamed.md").exists());
}

/// The gate judges the document the move *produces*, and anchoring is part of
/// what the move writes. A frozen document whose id is path-derived keeps that
/// id across the move — rename pins it into the frontmatter first — so the
/// baseline record survives at the new path and nothing was destroyed. Judging
/// the pre-anchor bytes instead would watch the id change under it, read the
/// record as gone, and refuse a move `check` has no complaint about.
#[test]
fn rename_gates_the_anchored_document_the_move_produces() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [[rules.frontmatter_immutable]]\nname = \"title-locked\"\nfields = [\"title\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n",
    )
    .unwrap();
    // No `id:` — it is stem-derived, so the move would change it and rename
    // anchors the old one. Terminal, so the frontmatter lock holds the record.
    write_doc(
        project,
        "docs/a.md",
        "---\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a frozen doc with a derived id"]);

    let data = run_json(nodex(project).args(["rename", "docs/a.md", "docs/a-renamed.md"]));
    assert_eq!(
        data.pointer("/id_stability/id").and_then(Value::as_str),
        Some("generic-a"),
        "the record travels under its own id, so the baseline sees a move: {data}"
    );
    let moved = fs::read_to_string(project.join("docs/a-renamed.md")).unwrap();
    assert!(
        moved.contains("id: \"generic-a\"") || moved.contains("id: generic-a"),
        "and the moved document carries it: {moved}"
    );
}

/// A `conditional_exclude` eviction is not a destruction. Making a parent
/// terminal drops its sub-artifacts from *scope* by design — their files stay
/// on disk, unmodified — and `check` reports nothing for it on either plane.
/// Refusing the transition would be a refusal with no reading to be consistent
/// with, and once the parent is terminal no command could clear it.
#[test]
fn archiving_a_parent_that_evicts_a_frozen_sub_artifact_is_allowed() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"docs/**/SPEC.md\"\n\
         child_glob = \"docs/**/note-*.md\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         trigger = \"creation\"\n",
    )
    .unwrap();
    write_doc(
        project,
        "docs/spec/SPEC.md",
        "---\nid: generic-SPEC\ntitle: Spec\nkind: generic\nstatus: active\n---\n# Spec\n",
    );
    write_doc(
        project,
        "docs/spec/note-a.md",
        "---\nid: generic-note-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a frozen note under an active spec"]);

    nodex(project)
        .args(["lifecycle", "set", "generic-SPEC", "--status", "archived"])
        .assert()
        .success();
    let data = run_json(nodex(project).arg("check"));
    assert_eq!(
        data.get("total").and_then(Value::as_u64),
        Some(0),
        "the read plane agrees there is nothing to report: {data}"
    );
    assert!(
        project.join("docs/spec/note-a.md").exists(),
        "and the evicted note is still on disk — it left scope, not the project"
    );
}

#[test]
fn check_since_activates_frontmatter_immutable_rule() {
    // End-to-end witness for the diff-aware rule path:
    // 1. commit a terminal-status node with `superseded_by: old`
    // 2. edit the locked field in the work tree (no new commit)
    // 3. `check --since HEAD` must surface the violation
    //
    // The rule must self-report as APPLIED (not skipped) under
    // `--since`, and must STAY skipped under plain `check`.
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

[[rules.frontmatter_immutable]]
name = "successor-chain"
fields = ["superseded_by"]
"#,
    )
    .unwrap();

    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);

    // Two docs: a successor (active) and a predecessor (terminal, locked
    // `superseded_by`).
    write_doc(
        root,
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);

    // Edit the locked field in the work tree — no new commit; this is
    // exactly the surface `check --since` guards.
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-tampered\n---\n# Old\n",
    );

    nodex(root).arg("build").assert().success();

    // Without `--since` the rule is inert; it self-reports as skipped.
    let plain = run_json(nodex(root).args(["check"]));
    let skipped: Vec<&str> = plain
        .get("skipped_rules")
        .and_then(Value::as_array)
        .expect("skipped_rules")
        .iter()
        .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
        .collect();
    assert!(
        skipped.contains(&"frontmatter_immutable/successor-chain"),
        "plain check must list frontmatter_immutable/successor-chain as skipped: {plain}"
    );

    // With `--since HEAD` the rule activates and surfaces the violation
    // (exit 1 because the rule's severity is error).
    let output = nodex(root)
        .args(["check", "--since", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let env: Value = serde_json::from_str(stdout.trim()).expect("json envelope");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str)
                == Some("frontmatter_immutable/successor-chain")
                && v.get("node_id").and_then(Value::as_str) == Some("doc-old")
        }),
        "check --since must surface frontmatter_immutable/successor-chain on doc-old: {violations:?}"
    );

    // And the rule must NOT also appear in skipped under `--since`.
    let skipped_under_since: Vec<&str> = env
        .pointer("/data/skipped_rules")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !skipped_under_since.contains(&"frontmatter_immutable/successor-chain"),
        "frontmatter_immutable/successor-chain must not be skipped when --since is supplied: {env}"
    );
}

#[test]
fn immutable_baseline_enforces_without_explicit_since() {
    // With `rules.immutable_baseline = "HEAD"`, a plain `nodex check`
    // (no `--since`) must enforce the diff-aware immutability rules
    // against the last commit — closing the gap where they were inert
    // unless a ref was passed by hand.
    let tmp = scratch();
    let root = tmp.path();

    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[rules]
immutable_baseline = "HEAD"

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"

[[rules.frontmatter_immutable]]
name = "successor-chain"
fields = ["superseded_by"]
"#,
    )
    .unwrap();

    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);

    write_doc(
        root,
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);

    // Tamper the locked field in the work tree — no new commit.
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-tampered\n---\n# Old\n",
    );

    nodex(root).arg("build").assert().success();

    // Plain check (no --since) now fires the rule via the baseline.
    let output = nodex(root).args(["check"]).output().expect("ran");
    assert_eq!(output.status.code(), Some(1));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str)
                == Some("frontmatter_immutable/successor-chain")
        }),
        "baseline must enforce immutability without --since: {violations:?}"
    );
    let skipped: Vec<&str> = env
        .pointer("/data/skipped_rules")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !skipped.contains(&"frontmatter_immutable/successor-chain"),
        "rule must be active (not skipped) under an immutable_baseline: {env}"
    );
}

#[test]
fn check_since_fires_body_immutable_on_body_only_edit() {
    // Regression: a body edit on a terminal-status doc that touches NO
    // frontmatter field must still surface as a `body_immutable/<name>`
    // violation under `check --since`. The CLI's changed-id set is
    // built from every `GraphDiff` variant that names a node id; if
    // `body_changes` is omitted from that set, the post-filter strips
    // the rule's legitimate violations and we get a silent skip —
    // exactly the failure mode `.claude/rules/config-driven.md`
    // forbids. This test exercises the body-only path end-to-end.
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

[[rules.body_immutable]]
name = "frozen-once-terminal"
mode = "frozen"
"#,
    )
    .unwrap();

    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);

    // Successor (so the cross_field constraint passes on the superseded doc)
    // and a predecessor at terminal status with an initial body.
    write_doc(
        root,
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n\nOriginal body.\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);

    // Body-only edit on the terminal doc — no frontmatter touched.
    write_doc(
        root,
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: superseded\nsuperseded_by: doc-new\n---\n# Old\n\nTampered body.\n",
    );

    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["check", "--since", "HEAD"])
        .output()
        .expect("ran");
    // The rule is severity=error, so the violation must drive exit 1.
    assert_eq!(
        output.status.code(),
        Some(1),
        "body-only edit on terminal doc must drive exit 1; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let env: Value = serde_json::from_slice(&output.stdout).expect("json envelope");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations array");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/frozen-once-terminal")
                && v.get("node_id").and_then(Value::as_str) == Some("doc-old")
        }),
        "body-only edit must surface body_immutable/frozen-once-terminal on doc-old: {violations:?}"
    );
}

#[test]
fn check_since_creation_trigger_locks_active_doc_but_not_creating_commit() {
    // trigger = "creation": the body freezes as soon as a prior
    // committed snapshot exists, regardless of status. Two contracts
    // end-to-end: (a) the commit that *creates* the doc passes check
    // (the diff carries it as added, not body-changed); (b) a later
    // body edit on the still-`active` doc fires — the case the
    // terminal trigger structurally cannot express.
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

[[rules.body_immutable]]
name = "record"
mode = "frozen"
trigger = "creation"
"#,
    )
    .unwrap();

    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);

    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "config"]);

    // (a) Creating commit: the new doc is `added`, never `body_changed`
    // — check --since HEAD must pass.
    write_doc(
        root,
        "docs/rec.md",
        "---\nid: doc-rec\ntitle: Rec\nkind: generic\nstatus: active\n---\n# Rec\n\nDecided.\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["check", "--since", "HEAD"])
        .assert()
        .success();

    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "create record"]);

    // (b) Body edit on the committed, still-active doc must fire.
    write_doc(
        root,
        "docs/rec.md",
        "---\nid: doc-rec\ntitle: Rec\nkind: generic\nstatus: active\n---\n# Rec\n\nRe-decided.\n",
    );
    nodex(root).arg("build").assert().success();
    let output = nodex(root)
        .args(["check", "--since", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(
        output.status.code(),
        Some(1),
        "creation-trigger lock must fire on an active doc's body edit; stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let env: Value = serde_json::from_slice(&output.stdout).expect("json envelope");
    let violations = env
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations array");
    assert!(
        violations.iter().any(|v| {
            v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/record")
                && v.get("node_id").and_then(Value::as_str) == Some("doc-rec")
        }),
        "expected body_immutable/record on doc-rec: {violations:?}"
    );
}

/// A committed project whose `immutable_baseline` freezes terminal
/// bodies — the fixture for the recreate/--force lock tests.
/// A committed project whose `immutable_baseline` freezes a frontmatter field
/// and declares no body lock at all — the shape whose path-address protection
/// was lost when the destruction predicate asked only about bodies.
fn frontmatter_frozen_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.frontmatter_immutable]]\nname = \"locked-owner\"\nfields = [\"owner\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: superseded\nowner: alice\n---\n\
         # A\nFrozen record.\n",
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "baseline"]);
}

/// A record frozen only by its frontmatter is still frozen history, and the
/// path address is the only thing that can guard it from destruction — an
/// overwrite landing a different id shares no join key, so no rule fires.
#[test]
fn scaffold_force_refuses_overwriting_a_frontmatter_frozen_record() {
    let tmp = scratch();
    let project = tmp.path();
    frontmatter_frozen_project(project);
    let before = fs::read_to_string(project.join("docs/a.md")).unwrap();

    let output = nodex(project)
        .args(["scaffold", "--kind", "generic", "--title", "Replacement"])
        .args(["--id", "doc-new", "--path", "docs/a.md", "--force"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("LOCKED"),
        "a frontmatter-only project has frozen records too: {envelope}"
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("frontmatter_immutable/locked-owner"),
        "and the refusal names the family that froze it: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(project.join("docs/a.md")).unwrap(),
        before,
        "the frozen bytes survive"
    );
}

/// `migrate` injects a whole frontmatter block, which changes every field in
/// it. A document born terminal (`statuses.initial` is itself terminal) has its
/// locks armed from the baseline, so one of those fields is a write the
/// project's own `check` rejects — and exactly that one is what the seam keeps
/// back. Injecting the rest introduces nothing: the field the lock froze is
/// still absent, so the finding the document already carried is the same
/// finding it carries afterwards.
#[test]
fn migrate_injects_around_a_field_the_baseline_locks() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"sealed\", \"active\"]\nterminal = [\"sealed\"]\n\
         initial = \"sealed\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\nrequired = [\"owner\"]\n\
         [[rules.frontmatter_immutable]]\nname = \"locked-owner\"\nfields = [\"owner\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n",
    )
    .unwrap();
    let bare = "# Legacy\n\nno frontmatter at all\n";
    write_doc(project, "docs/legacy.md", bare);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a bare legacy document"]);

    let envelope = run_envelope(nodex(project).args(["migrate", "--apply"]));
    assert_eq!(
        envelope["data"]["total"], 1,
        "what the lock does not freeze is still migrated: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("frontmatter `owner`")
                && m.contains("frontmatter_immutable/locked-owner")),
        "and the field kept back is named with the rule that froze it: {envelope}"
    );
    let migrated = fs::read_to_string(project.join("docs/legacy.md")).unwrap();
    assert!(
        migrated.contains("id:") && !migrated.contains("owner:"),
        "the block lands without the locked field: {migrated:?}"
    );
    assert!(
        migrated.ends_with(bare),
        "and the body is untouched: {migrated:?}"
    );
}

/// The config the part-granular lock fixtures share: a frozen body for one
/// kind and a locked frontmatter field for every kind, so a document can be
/// governed by one without being governed by the other.
const PART_LOCKED_CONFIG: &str = r#"
[scope]
include = ["docs/**/*.md"]
[kinds]
allowed = ["generic", "note"]
[statuses]
allowed = ["active", "archived"]
terminal = ["archived"]
initial = "active"
[[identity.kind_rules]]
glob = "docs/notes/*.md"
kind = "note"
[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
[parser]
wikilink_enabled = true
[rules]
immutable_baseline = "HEAD"
[[rules.body_immutable]]
name = "frozen"
mode = "frozen"
trigger = "terminal"
kinds = ["generic"]
[[rules.frontmatter_immutable]]
name = "owner-locked"
fields = ["owner"]
"#;

fn part_locked_project(project: &std::path::Path) {
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), PART_LOCKED_CONFIG).unwrap();
    write_doc(
        project,
        "docs/old.md",
        "---\nid: generic-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        project,
        "docs/new.md",
        "---\nid: generic-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
}

/// A lock names a part of a document, so a part is what it may cost. A frozen
/// body and a structural frontmatter edge are two different claims about the
/// same file: the citation is a record of what was true when it was written
/// and must stand, while the relation is an edge the graph reads and must
/// move. Refusing the file for the body's sake used to hold back both.
#[test]
fn retarget_repoints_a_relation_a_frozen_body_shares_a_file_with() {
    let tmp = scratch();
    let project = tmp.path();
    part_locked_project(project);
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n\
         related: [generic-old]\n---\n# A\n\nat the time of writing, [[generic-old]] governed\n",
    );
    let git = git_runner(project);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    let envelope = run_envelope(nodex(project).args(["retarget", "generic-old", "generic-new"]));
    assert_eq!(
        envelope["data"]["total_updated"], 1,
        "the relation is repointed: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("its body") && m.contains("body_immutable/frozen")),
        "and the part kept back is named with the rule that froze it: {envelope}"
    );
    let written = fs::read_to_string(project.join("docs/a.md")).unwrap();
    assert!(
        written.contains("generic-new"),
        "the relation names the successor: {written:?}"
    );
    assert!(
        written.contains("[[generic-old]]"),
        "the frozen body keeps its point-in-time citation: {written:?}"
    );
    nodex(project)
        .args(["check", "--since", "HEAD"])
        .assert()
        .success();
}

/// The verdict is absolute, not this write's delta: a document that already
/// drifted from a frozen baseline is frozen history someone edited, and the
/// remedy is to fix the drift. Narrowing cannot reach it — the part is not one
/// this write touches — so the whole write is held back, exactly as before
/// parts were named at all.
#[test]
fn retarget_holds_back_a_whole_write_to_an_already_drifted_record() {
    let tmp = scratch();
    let project = tmp.path();
    part_locked_project(project);
    write_doc(
        project,
        "docs/notes/n.md",
        "---\nid: note-n\ntitle: N\nkind: note\nstatus: archived\nowner: alice\n---\n\
         # N\n\nsee [[generic-old]]\n",
    );
    let git = git_runner(project);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // The locked field moves in the working tree only. The repoint would write
    // the body, which no lock names.
    write_doc(
        project,
        "docs/notes/n.md",
        "---\nid: note-n\ntitle: N\nkind: note\nstatus: archived\nowner: bob\n---\n\
         # N\n\nsee [[generic-old]]\n",
    );

    let envelope = run_envelope(nodex(project).args(["retarget", "generic-old", "generic-new"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "a drifted frozen record is not written to at all: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("frontmatter_immutable/owner-locked")),
        "and the lock that holds it is named: {envelope}"
    );
    assert!(
        fs::read_to_string(project.join("docs/notes/n.md"))
            .unwrap()
            .contains("[[generic-old]]"),
        "the body the repoint would have written is untouched"
    );
}

fn frozen_baseline_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"adr-frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\nFrozen record.\n",
    );
    let git = git_runner(root);
    git(&["init"]);
    git(&["add", "."]);
    git(&["commit", "-m", "baseline"]);
}

#[test]
fn scaffold_force_refuses_overwriting_baseline_locked_doc() {
    let tmp = scratch();
    frozen_baseline_project(tmp.path());
    let before = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();

    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Rewrite"])
        .args(["--path", "docs/a.md", "--force"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("LOCKED")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("body_immutable/adr-frozen"),
        "the refusal names the lock: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("docs/a.md")).unwrap(),
        before,
        "the frozen bytes survive"
    );
}

#[test]
fn scaffold_refuses_recreating_deleted_locked_doc() {
    // Deleting a frozen record and re-scaffolding its path is the same
    // rewrite `check` against the baseline would flag — refused without
    // `--force`, since the path no longer exists on disk.
    let tmp = scratch();
    frozen_baseline_project(tmp.path());
    fs::remove_file(tmp.path().join("docs/a.md")).unwrap();

    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Recreate"])
        .args(["--path", "docs/a.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("LOCKED")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("body_immutable/adr-frozen"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/a.md").exists());
}

/// The path a creation lands on is only one of the two ways it reaches the
/// baseline. `check` pairs by id, so a scaffold that gives a frozen record's
/// id a different body is a `body_change` in the report — and a lock that
/// asks only "does a frozen record stand at this path" answers `None` for it,
/// because the document need not land where it stood. No `--force` is
/// involved: the new path holds nothing.
#[test]
fn scaffold_refuses_relocating_a_frozen_record_under_its_own_id() {
    let tmp = scratch();
    let project = tmp.path();
    frozen_baseline_project(project);
    fs::remove_file(project.join("docs/a.md")).unwrap();
    let body = project.join("body.md");
    fs::write(&body, "# A\nA different body entirely.\n").unwrap();

    let output = nodex(project)
        .args(["scaffold", "--kind", "generic", "--title", "A"])
        .args(["--id", "doc-a", "--path", "docs/elsewhere.md"])
        .args(["--body", body.to_str().unwrap()])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("LOCKED"),
        "the write plane refuses what `check` reds: {envelope}"
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("body_immutable/adr-frozen"),
        "and names the rule that governs it: {envelope}"
    );
    assert!(
        !project.join("docs/elsewhere.md").exists(),
        "nothing was written"
    );

    // The same seam still creates a record the baseline does not hold.
    let fresh = run_envelope(
        nodex(project)
            .args(["scaffold", "--kind", "generic", "--title", "Fresh"])
            .args(["--id", "doc-fresh", "--path", "docs/fresh.md"])
            .args(["--body", body.to_str().unwrap()]),
    );
    assert_eq!(
        fresh["data"]["written"],
        Value::Bool(true),
        "an unheld id at an empty path is not locked: {fresh}"
    );
}

/// A `kind` lock armed at acceptance. Which lifecycle a record answers to is
/// the field every kind-scoped rule reads first, and `terminal` settles it
/// only once the record is finished with — after every one of those rules has
/// already been asked. Armed at the statuses the project accepts a record at,
/// the same block settles it while the answer still matters.
#[test]
fn a_kind_locked_at_acceptance_is_frozen_from_the_commit_that_accepts_it() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"adr\", \"generic\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"proposed\"\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n\
         [[rules.frontmatter_immutable]]\nname = \"adr-kind\"\nfields = [\"kind\"]\n\
         kinds = [\"adr\"]\ntrigger = \"status\"\n\
         statuses = [\"active\", \"superseded\"]\n\
         [detection]\norphan_ok_kinds = [\"adr\", \"generic\"]\n",
    )
    .unwrap();
    let record = |kind: &str, status: &str| {
        write_doc(
            project,
            "docs/a.md",
            &format!("---\nid: adr-a\ntitle: A\nkind: {kind}\nstatus: {status}\n---\n# A\n"),
        );
    };

    record("adr", "proposed");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "propose"]);
    let proposed = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    // While it is a draft the kind is still the author's to pick.
    record("generic", "proposed");
    let drafting = judged(nodex(project).args(["check", "--since", &proposed]));
    assert_eq!(
        drafting["total"], 0,
        "a draft's kind is not yet settled: {drafting}"
    );

    record("adr", "active");
    git(&["commit", "-qam", "accept"]);
    let accepted = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    // The write that accepts it arms the lock and is not refused by it.
    let accepting = judged(nodex(project).args(["check", "--since", &proposed]));
    assert_eq!(
        accepting["total"], 0,
        "the accepting write may set what it arms: {accepting}"
    );

    // From here the kind is the record's, and the flip the flow cannot see
    // is refused by the block that held it.
    record("generic", "active");
    let laundered = judged(nodex(project).args(["check", "--since", &accepted]));
    let fired: Vec<&str> = laundered["violations"]
        .as_array()
        .expect("violations")
        .iter()
        .filter_map(|v| v["rule_id"].as_str())
        .collect();
    assert_eq!(
        fired,
        vec!["frontmatter_immutable/adr-kind"],
        "the block the record was under judges the write that leaves it: {laundered}"
    );
}

/// A lock judges the state a write would leave, which is two different facts
/// wearing one verdict: the record already differs from its baseline, or this
/// write is what would make it differ. The remedies are opposite — revert the
/// drift, or abandon the write — so a seam that assumed the first sent an
/// operator to `nodex check` for a finding it does not report.
#[test]
fn a_lock_that_refuses_only_the_write_does_not_send_the_operator_to_revert() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"adr\", \"note\", \"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/adr/**\"\nkind = \"adr\"\n\
         [[identity.kind_rules]]\nglob = \"docs/notes/**\"\nkind = \"note\"\n\
         [[rules.frontmatter_immutable]]\nname = \"kind-locked\"\nfields = [\"kind\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [detection]\norphan_ok_kinds = [\"adr\", \"note\", \"generic\"]\n",
    )
    .unwrap();
    // No frontmatter `kind:` — it is path-derived, so the move alone changes it.
    write_doc(
        project,
        "docs/adr/a.md",
        "---\nid: adr-a\ntitle: A\nstatus: superseded\n---\n# A\n",
    );
    fs::create_dir_all(project.join("docs/notes")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a terminal adr"]);

    let refusal = |cmd: &mut Command| -> String {
        let output = cmd.output().expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let moving = || {
        let mut cmd = nodex(project);
        cmd.args(["rename", "docs/adr/a.md", "docs/notes/a.md"]);
        cmd
    };

    // The tree is clean, so `check` reports nothing; only the move would move
    // the frozen field.
    let clean = judged(nodex(project).arg("check"));
    assert_eq!(
        clean["total"], 0,
        "the record as it stands is fine: {clean}"
    );
    let message = refusal(&mut moving());
    assert!(
        message.contains("frontmatter_immutable/kind-locked")
            && message.contains("nothing to revert"),
        "the refusal says the move is the cause, not a drift: {message}"
    );
    assert!(
        !message.contains("`nodex check` reports the difference"),
        "and never claims a `check` finding that does not exist: {message}"
    );

    // Now the record really has drifted, and `check` really does name it.
    write_doc(
        project,
        "docs/adr/a.md",
        "---\nid: adr-a\ntitle: A\nkind: note\nstatus: superseded\n---\n# A\n",
    );
    let drifted = judged(nodex(project).arg("check"));
    assert_eq!(
        drifted["violations"][0]["rule_id"], "frontmatter_immutable/kind-locked",
        "the drift is what `check` names: {drifted}"
    );
    let message = refusal(&mut moving());
    assert!(
        message.contains("`nodex check` reports the difference"),
        "so the refusal may send the operator there: {message}"
    );
}

/// The same distinction at the `lifecycle` seam, which reaches it through
/// `review` — the one action the terminal guard exempts.
#[test]
fn a_lifecycle_write_a_lock_refuses_names_which_of_the_two_refused_it() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"adr\", \"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[rules.frontmatter_immutable]]\nname = \"sealed\"\nfields = [\"reviewed\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [detection]\norphan_ok_kinds = [\"adr\", \"generic\"]\n",
    )
    .unwrap();
    let record = |reviewed: &str| {
        write_doc(
            project,
            "docs/a.md",
            &format!(
                "---\nid: adr-a\ntitle: A\nkind: adr\nstatus: superseded\n{reviewed}---\n# A\n"
            ),
        );
    };
    record("");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a terminal adr"]);

    let refusal = || -> String {
        let output = nodex(project)
            .args(["lifecycle", "review", "adr-a"])
            .output()
            .expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    assert_eq!(judged(nodex(project).arg("check"))["total"], 0);
    let message = refusal();
    assert!(
        message.contains("frontmatter_immutable/sealed") && message.contains("nothing to revert"),
        "review is what would write the frozen field: {message}"
    );

    record("reviewed: 2020-01-01\n");
    assert_eq!(judged(nodex(project).arg("check"))["total"], 1);
    let message = refusal();
    assert!(
        message.contains("`nodex check` reports the difference"),
        "now the drift is the remedy: {message}"
    );
}

/// And at the `scaffold` seam, where a lock refusing only the proposal is the
/// ordinary case: overwriting a frozen record that has not itself moved.
#[test]
fn a_scaffold_over_a_frozen_record_says_the_scaffold_is_what_would_move_it() {
    let tmp = scratch();
    frozen_baseline_project(tmp.path());

    let refusal = || -> String {
        let output = nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Rewrite"])
            // The same id, so the record survives the write and the question
            // is the lock rather than the record's destruction.
            .args(["--path", "docs/a.md", "--id", "doc-a", "--force"])
            .output()
            .expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    assert_eq!(judged(nodex(tmp.path()).arg("check"))["total"], 0);
    let message = refusal();
    assert!(
        message.contains("body_immutable/adr-frozen") && message.contains("nothing to revert"),
        "the scaffold is the cause: {message}"
    );

    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: superseded\n---\n# A\nEdited.\n",
    );
    assert_eq!(judged(nodex(tmp.path()).arg("check"))["total"], 1);
    let message = refusal();
    assert!(
        message.contains("`nodex check` reports the difference")
            && !message.contains("nothing to revert"),
        "now the drift is what the operator must clear: {message}"
    );
}

/// The guard against destroying a frozen record and the rule against editing
/// one must arm on the same word. A destroy-and-replace at a path is a removal
/// plus an addition to the diff, which no diff-aware rule can see, so this
/// guard is the only thing standing there — a trigger it does not read is a
/// lock with no guarantee at all, and nothing afterwards would report it.
#[test]
fn a_record_frozen_by_any_trigger_survives_a_write_that_would_replace_it() {
    // The record's status differs per arm so that each trigger is the only
    // reason the lock is armed: at `active` the terminal trigger does not
    // arm at all, which is what makes the other two arms load-bearing.
    for (trigger, statuses, status) in [
        ("terminal", "", "archived"),
        ("creation", "", "active"),
        ("status", "\nstatuses = [\"active\"]", "active"),
    ] {
        let tmp = scratch();
        let project = tmp.path();
        let git = git_runner(project);
        git(&["init", "-q"]);
        fs::write(
            project.join("nodex.toml"),
            format!(
                "[scope]\ninclude = [\"docs/**/*.md\"]\n\
                 [kinds]\nallowed = [\"generic\"]\n\
                 [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
                 [[rules.frontmatter_immutable]]\nname = \"sealed\"\nfields = [\"owner\"]\n\
                 trigger = \"{trigger}\"{statuses}\n\
                 [rules]\nimmutable_baseline = \"HEAD\"\n\
                 [detection]\norphan_ok_kinds = [\"generic\"]\n"
            ),
        )
        .unwrap();
        write_doc(
            project,
            "docs/a.md",
            &format!(
                "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: {status}\n\
                 owner: alice\n---\n# A\n"
            ),
        );
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "a frozen record"]);

        let output = nodex(project)
            .args(["scaffold", "--kind", "generic", "--title", "Replacement"])
            .args(["--path", "docs/a.md", "--id", "generic-b", "--force"])
            .args(["--field", "owner=mallory"])
            .output()
            .expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert_eq!(
            envelope["ok"], false,
            "trigger = {trigger:?} must refuse the replacement: {envelope}"
        );
        assert!(
            envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("frontmatter_immutable/sealed"),
            "and name the lock that refused it: {envelope}"
        );
        assert!(
            fs::read_to_string(project.join("docs/a.md"))
                .unwrap()
                .contains("id: generic-a"),
            "trigger = {trigger:?}: the frozen record is still the one at the path"
        );
    }
}

/// The same guard at `rename`'s other call site. A move onto a path whose file
/// the working tree no longer has is still a write over the record the
/// baseline holds there, and the baseline is the only thing that still knows.
#[test]
fn a_move_onto_a_frozen_record_s_path_is_refused_by_any_trigger() {
    for (trigger, statuses, status) in [
        ("terminal", "", "archived"),
        ("creation", "", "active"),
        ("status", "\nstatuses = [\"active\"]", "active"),
    ] {
        let tmp = scratch();
        let project = tmp.path();
        let git = git_runner(project);
        git(&["init", "-q"]);
        fs::write(
            project.join("nodex.toml"),
            format!(
                "[scope]\ninclude = [\"docs/**/*.md\"]\n\
                 [kinds]\nallowed = [\"generic\"]\n\
                 [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
                 [[rules.frontmatter_immutable]]\nname = \"sealed\"\nfields = [\"owner\"]\n\
                 trigger = \"{trigger}\"{statuses}\n\
                 [rules]\nimmutable_baseline = \"HEAD\"\n\
                 [detection]\norphan_ok_kinds = [\"generic\"]\n"
            ),
        )
        .unwrap();
        for (name, held) in [("a", status), ("b", "active")] {
            write_doc(
                project,
                &format!("docs/{name}.md"),
                &format!(
                    "---\nid: generic-{name}\ntitle: {name}\nkind: generic\nstatus: {held}\n\
                     owner: alice\n---\n# {name}\n"
                ),
            );
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "a frozen record and a mover"]);
        fs::remove_file(project.join("docs/a.md")).unwrap();

        let output = nodex(project)
            .args(["rename", "docs/b.md", "docs/a.md"])
            .output()
            .expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert_eq!(
            envelope["ok"], false,
            "trigger = {trigger:?} must refuse the move: {envelope}"
        );
        assert!(
            envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("frontmatter_immutable/sealed"),
            "and name the lock that refused it: {envelope}"
        );
        assert!(
            project.join("docs/b.md").exists() && !project.join("docs/a.md").exists(),
            "trigger = {trigger:?}: nothing moved, so the refusal is honourable"
        );
    }
}

#[test]
fn an_unbound_lock_does_not_judge_an_unreadable_historical_graph() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    let content = "---\nid: same\ntitle: A\nkind: generic\nstatus: active\n---\nBody\n";
    write_doc(root, "docs/a.md", content);
    write_doc(root, "docs/b.md", content);
    let git = git_runner(root);
    git(&["init"]);
    git(&["add", "."]);
    assert!(
        git(&["commit", "-qm", "duplicate records"])
            .status
            .success()
    );
    fs::remove_file(root.join("docs/b.md")).unwrap();
    let envelope = run_envelope(nodex(root).arg("check"));
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["has_errors"], false);
    assert!(
        envelope["warnings"]
            .as_array()
            .is_none_or(|warnings| warnings
                .iter()
                .all(|warning| warning["code"] != "history_unread")),
        "{envelope}"
    );
    assert!(
        envelope["data"]["skipped_rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["rule_id"] == "body_immutable/frozen"),
        "{envelope}"
    );
}

#[test]
fn document_identity_cannot_bypass_an_armed_lock() {
    let tmp = scratch();
    let root = tmp.path();
    frozen_baseline_project(root);
    let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
    write_doc(
        root,
        "docs/a.md",
        &original
            .replace("id: doc-a", "id: replacement")
            .replace("Frozen record.", "Changed record."),
    );
    for args in [vec!["check"], vec!["check", "--staged"]] {
        if args.contains(&"--staged") {
            git_runner(root)(&["add", "docs/a.md"]);
        }
        let output = nodex(root).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            envelope["data"]["violations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["rule_id"] == "body_immutable/adr-frozen"
                    && v["details"]["field"] == "id"),
            "{envelope}"
        );
    }
    write_doc(root, "docs/a.md", &original);
    fs::rename(root.join("docs/a.md"), root.join("docs/moved.md")).unwrap();
    nodex(root).arg("check").assert().success();
    fs::remove_file(root.join("docs/moved.md")).unwrap();
    nodex(root).arg("check").assert().success();
}

#[test]
fn range_lock_detects_acceptance_followed_by_edit_and_revert() {
    let tmp = scratch();
    let root = tmp.path();
    frozen_baseline_project(root);
    let config_path = root.join("nodex.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        config_path,
        format!("{config}\n[[rules.frontmatter_immutable]]\nname = \"sealed-title\"\nfields = [\"title\"]\n"),
    )
    .unwrap();
    let git = git_runner(root);
    let accepted = fs::read_to_string(root.join("docs/a.md")).unwrap();
    write_doc(
        root,
        "docs/a.md",
        &accepted.replace("status: superseded", "status: active"),
    );
    git(&["commit", "-qam", "draft"]);
    git(&["tag", "draft"]);
    write_doc(root, "docs/a.md", &accepted);
    git(&["commit", "-qam", "accepted"]);
    git(&["tag", "accepted"]);
    nodex(root)
        .args(["check", "--since", "draft"])
        .assert()
        .success();
    write_doc(
        root,
        "docs/a.md",
        &accepted
            .replace("Frozen record.", "Changed record.")
            .replace("title: A", "title: Changed"),
    );
    git(&["commit", "-qam", "edit"]);
    for baseline in ["draft", "accepted"] {
        nodex(root)
            .args(["check", "--since", baseline])
            .assert()
            .code(1);
    }
    write_doc(root, "docs/a.md", &accepted);
    git(&["commit", "-qam", "revert"]);
    let output = nodex(root)
        .args(["check", "--since", "draft"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    let violations = envelope["data"]["violations"].as_array().unwrap();
    for rule in [
        "body_immutable/adr-frozen",
        "frontmatter_immutable/sealed-title",
    ] {
        assert!(
            violations
                .iter()
                .any(|finding| finding["rule_id"] == rule
                    && finding["details"]["commit"].is_string()),
            "{envelope}"
        );
    }
}

#[test]
fn append_only_merge_preserves_growth_despite_unlocked_edits_and_moves() {
    for unrelated in ["source", "title", "path"] {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            "[scope]\ninclude = [\"docs/**/*.md\"]\n\
             [[rules.body_immutable]]\nname = \"log\"\nmode = \"append_only\"\ntrigger = \"creation\"\n",
        )
        .unwrap();
        let base = "---\nid: record\ntitle: Record\nkind: generic\nstatus: active\n---\nBase\n";
        write_doc(root, "docs/a.md", base);
        let git = git_runner(root);
        git(&["init"]);
        git(&["add", "."]);
        assert!(git(&["commit", "-qm", "base"]).status.success());
        git(&["tag", "base"]);
        git(&["checkout", "-qb", "left"]);
        let left = if unrelated == "title" {
            base.replace("title: Record", "title: Other")
        } else {
            base.to_string()
        };
        let path = if unrelated == "path" {
            "docs/b.md"
        } else {
            "docs/a.md"
        };
        if unrelated == "path" {
            fs::rename(root.join("docs/a.md"), root.join(path)).unwrap();
        }
        write_doc(root, path, &left);
        fs::write(root.join("source.txt"), "left").unwrap();
        git(&["add", "."]);
        assert!(git(&["commit", "-qm", "unlocked edit"]).status.success());
        nodex(root)
            .args(["check", "--since", "base"])
            .assert()
            .success();
        git(&["checkout", "-qb", "right", "base"]);
        write_doc(root, "docs/a.md", &format!("{base}Added\n"));
        assert!(git(&["commit", "-qam", "append"]).status.success());
        nodex(root)
            .args(["check", "--since", "base"])
            .assert()
            .success();
        assert!(
            git(&["merge", "--no-ff", "--no-commit", "left"])
                .status
                .success()
        );
        write_doc(root, path, &left);
        git(&["add", "."]);
        assert!(git(&["commit", "-qm", "discard append"]).status.success());
        let output = nodex(root)
            .args(["check", "--since", "base"])
            .assert()
            .code(1)
            .get_output()
            .clone();
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            envelope["data"]["violations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|finding| finding["rule_id"] == "body_immutable/log"
                    && finding["details"]["commit"].is_string()),
            "{unrelated}: {envelope}"
        );
    }
}

#[test]
fn a_merge_cannot_replace_a_locked_revision_with_an_unaccepted_revision() {
    for family in ["body", "frontmatter"] {
        let tmp = scratch();
        let root = tmp.path();
        let rule = if family == "body" {
            "[[rules.body_immutable]]\nname = \"accepted\"\nmode = \"frozen\"\n"
        } else {
            "[[rules.frontmatter_immutable]]\nname = \"accepted\"\nfields = [\"title\"]\n"
        };
        fs::write(root.join("nodex.toml"), format!(
            "[scope]\ninclude = [\"docs/**/*.md\"]\n\
             [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n{rule}"
        )).unwrap();
        let draft =
            "---\nid: record\ntitle: Original\nkind: generic\nstatus: active\n---\nOriginal\n";
        write_doc(root, "docs/a.md", draft);
        let git = git_runner(root);
        git(&["init"]);
        git(&["add", "."]);
        assert!(git(&["commit", "-qm", "draft"]).status.success());
        git(&["tag", "draft"]);
        git(&["checkout", "-qb", "accepted"]);
        let accepted = draft.replace("status: active", "status: superseded");
        write_doc(root, "docs/a.md", &accepted);
        assert!(git(&["commit", "-qam", "accepted"]).status.success());
        git(&["checkout", "-qb", "editing", "draft"]);
        let edited = draft.replace("Original", "Unaccepted");
        write_doc(root, "docs/a.md", &edited);
        assert!(git(&["commit", "-qam", "draft edit"]).status.success());
        nodex(root)
            .args(["check", "--since", "draft"])
            .assert()
            .success();
        assert!(
            git(&["merge", "--no-ff", "--no-commit", "accepted"])
                .status
                .success()
        );
        write_doc(
            root,
            "docs/a.md",
            &edited.replace("status: active", "status: superseded"),
        );
        git(&["add", "."]);
        assert!(
            git(&[
                "commit",
                "-qm",
                "combine unaccepted content with accepted status"
            ])
            .status
            .success()
        );
        nodex(root)
            .args(["check", "--since", "draft"])
            .assert()
            .code(1);
    }
}

#[test]
fn append_only_merge_preserves_every_armed_parent() {
    for divergent in [false, true] {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(root.join("nodex.toml"),
            "[scope]\ninclude = [\"docs/**/*.md\"]\n\
             [[rules.body_immutable]]\nname = \"log\"\nmode = \"append_only\"\ntrigger = \"creation\"\n"
        ).unwrap();
        let base = "---\nid: record\ntitle: Record\nkind: generic\nstatus: active\n---\nBase\n";
        write_doc(root, "docs/a.md", base);
        let git = git_runner(root);
        git(&["init"]);
        git(&["add", "."]);
        assert!(git(&["commit", "-qm", "base"]).status.success());
        git(&["tag", "base"]);
        git(&["checkout", "-qb", "left"]);
        let left = format!("{base}Left\n");
        write_doc(root, "docs/a.md", &left);
        fs::write(root.join("left.txt"), "left").unwrap();
        git(&["add", "."]);
        assert!(git(&["commit", "-qm", "left append"]).status.success());
        nodex(root)
            .args(["check", "--since", "base"])
            .assert()
            .success();
        git(&["checkout", "-qb", "right", "base"]);
        let right = if divergent {
            format!("{base}Right\n")
        } else {
            left.clone()
        };
        write_doc(root, "docs/a.md", &right);
        fs::write(root.join("right.txt"), "right").unwrap();
        git(&["add", "."]);
        assert!(git(&["commit", "-qm", "right append"]).status.success());
        nodex(root)
            .args(["check", "--since", "base"])
            .assert()
            .success();
        let merge = git(&["merge", "--no-commit", "--no-ff", "left"]);
        assert_eq!(merge.status.success(), !divergent);
        let result = if divergent {
            left
        } else {
            format!("{left}Merged\n")
        };
        write_doc(root, "docs/a.md", &result);
        git(&["add", "."]);
        for staged in [true, false] {
            if !staged {
                assert!(git(&["commit", "-qm", "merge"]).status.success());
            }
            let mut check = nodex(root);
            check.args(["check", "--since", "base"]);
            if staged {
                check.arg("--staged");
            }
            check.assert().code(if divergent { 1 } else { 0 });
        }
    }
}

#[test]
fn historical_lock_findings_survive_moves_without_counting_the_endpoint_twice() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [[rules.body_immutable]]\nname = \"body\"\nmode = \"frozen\"\ntrigger = \"creation\"\n\
         [[rules.frontmatter_immutable]]\nname = \"title\"\nfields = [\"title\"]\ntrigger = \"creation\"\n"
    ).unwrap();
    let original =
        "---\nid: record\ntitle: Original\nkind: generic\nstatus: active\n---\nOriginal\n";
    write_doc(root, "docs/a.md", original);
    let git = git_runner(root);
    git(&["init"]);
    git(&["add", "."]);
    assert!(git(&["commit", "-qm", "baseline"]).status.success());
    git(&["tag", "baseline"]);
    let wrong = original.replace("Original", "Wrong");
    write_doc(root, "docs/a.md", &wrong);
    assert!(git(&["commit", "-qam", "illegal edit"]).status.success());
    fs::rename(root.join("docs/a.md"), root.join("docs/b.md")).unwrap();
    let moved = wrong.replace("status: active", "status: superseded");
    write_doc(root, "docs/b.md", &moved);
    git(&["add", "."]);
    assert!(
        git(&["commit", "-qm", "move and update unlocked status"])
            .status
            .success()
    );
    let findings = |root: &std::path::Path| {
        let output = nodex(root)
            .args(["check", "--since", "baseline"])
            .assert()
            .code(1)
            .get_output()
            .clone();
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        envelope["data"]["violations"].as_array().unwrap().clone()
    };
    let committed = findings(root);
    for rule in ["body_immutable/body", "frontmatter_immutable/title"] {
        let held: Vec<_> = committed
            .iter()
            .filter(|finding| finding["rule_id"] == rule)
            .collect();
        assert_eq!(held.len(), 1, "{committed:?}");
        assert!(held[0]["details"]["commit"].is_string());
    }
    write_doc(root, "docs/b.md", &moved.replace("Wrong", "Another edit"));
    let uncommitted = findings(root);
    for rule in ["body_immutable/body", "frontmatter_immutable/title"] {
        let held: Vec<_> = uncommitted
            .iter()
            .filter(|finding| finding["rule_id"] == rule)
            .collect();
        assert_eq!(held.len(), 2, "{uncommitted:?}");
        assert_eq!(
            held.iter()
                .filter(|finding| finding["details"]["commit"].is_string())
                .count(),
            1
        );
    }
}

#[test]
fn locks_allow_a_merge_to_keep_either_independently_accepted_revision() {
    let tmp = scratch();
    let root = tmp.path();
    frozen_baseline_project(root);
    let config_path = root.join("nodex.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        format!(
            "{config}\n[[rules.frontmatter_immutable]]\nname = \"title\"\nfields = [\"title\"]\n"
        ),
    )
    .unwrap();
    let git = git_runner(root);
    let accepted = fs::read_to_string(root.join("docs/a.md")).unwrap();
    let draft = accepted.replace("status: superseded", "status: active");
    write_doc(root, "docs/a.md", &draft);
    assert!(git(&["commit", "-qam", "draft"]).status.success());
    git(&["tag", "draft"]);
    git(&["checkout", "-qb", "left"]);
    write_doc(
        root,
        "docs/a.md",
        &accepted
            .replace("Frozen record.", "Left decision.")
            .replace("title: A", "title: Left"),
    );
    assert!(git(&["commit", "-qam", "left acceptance"]).status.success());
    git(&["tag", "left-accepted"]);
    git(&["checkout", "-qb", "right", "draft"]);
    write_doc(
        root,
        "docs/a.md",
        &accepted
            .replace("Frozen record.", "Right decision.")
            .replace("title: A", "title: Right"),
    );
    assert!(
        git(&["commit", "-qam", "right acceptance"])
            .status
            .success()
    );
    git(&["tag", "right-accepted"]);
    assert!(!git(&["merge", "--no-commit", "left"]).status.success());
    write_doc(
        root,
        "docs/a.md",
        &accepted
            .replace("Frozen record.", "Left decision.")
            .replace("title: A", "title: Left"),
    );
    git(&["add", "docs/a.md"]);
    assert!(
        git(&["commit", "-qm", "choose left decision"])
            .status
            .success()
    );
    let policy = fs::read_to_string(&config_path).unwrap();
    for baseline in ["draft", "left-accepted", "right-accepted"] {
        fs::write(
            &config_path,
            policy.replace(
                "immutable_baseline = \"HEAD\"",
                &format!("immutable_baseline = {baseline:?}"),
            ),
        )
        .unwrap();
        let output = nodex(root)
            .args(["check", "--since", baseline])
            .assert()
            .code(if baseline == "right-accepted" { 1 } else { 0 })
            .get_output()
            .clone();
        if baseline == "right-accepted" {
            let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
            let findings = envelope["data"]["violations"].as_array().unwrap();
            for rule in ["body_immutable/adr-frozen", "frontmatter_immutable/title"] {
                assert!(
                    findings.iter().any(|finding| finding["rule_id"] == rule
                        && finding["details"].get("commit").is_none()),
                    "{envelope}"
                );
            }
        }
    }
    fs::write(&config_path, policy).unwrap();
    write_doc(
        root,
        "docs/a.md",
        &accepted.replace("Frozen record.", "A third decision."),
    );
    nodex(root)
        .args(["check", "--since", "draft"])
        .assert()
        .code(1);
}

#[test]
fn locks_recover_the_revision_before_a_document_became_unparseable() {
    let tmp = scratch();
    let root = tmp.path();
    frozen_baseline_project(root);
    let git = git_runner(root);
    let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
    git(&["tag", "baseline"]);
    write_doc(root, "docs/a.md", "---\nid: [broken\n---\nBroken\n");
    assert!(git(&["commit", "-qam", "broken record"]).status.success());
    git(&["tag", "broken"]);
    write_doc(
        root,
        "docs/a.md",
        &original.replace("Frozen record.", "Changed record."),
    );
    assert!(
        git(&["commit", "-qam", "repair and rewrite"])
            .status
            .success()
    );
    for baseline in ["baseline", "broken"] {
        let output = nodex(root)
            .args(["check", "--since", baseline])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            envelope["data"]["violations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|finding| finding["rule_id"] == "body_immutable/adr-frozen"
                    && finding["details"]["commit"].is_string()),
            "{envelope}"
        );
    }
}

#[test]
fn all_frontmatter_fields_lock_new_custom_values_and_preserve_exceptions() {
    let tmp = scratch();
    let root = tmp.path();
    snapshot_lock_project(root, "ref");
    let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
    for field in [
        "title: Changed",
        "priority: high",
        "metadata: { nested: [one, two] }",
    ] {
        let proposed = if field.starts_with("title:") {
            original.replace("title: A", field)
        } else {
            original.replace("status: accepted", &format!("status: accepted\n{field}"))
        };
        nodex(root)
            .args(["check", "--content", "docs/a.md=-"])
            .write_stdin(proposed)
            .assert()
            .code(1);
    }
    let proposed = original.replace("status: accepted", "status: superseded\ncovers: [src/a.rs]");
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(proposed)
        .assert()
        .success();
    let changed_id = original.replace("id: a", "id: replacement");
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(changed_id)
        .assert()
        .code(1);
    let with_metadata = original.replace(
        "status: accepted",
        "status: accepted\nmetadata: { nested: [one, two], flag: true }",
    );
    write_doc(root, "docs/a.md", &with_metadata);
    let git = git_runner(root);
    assert!(git(&["commit", "-qam", "metadata"]).status.success());
    assert!(git(&["branch", "-f", "main", "HEAD"]).status.success());
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(with_metadata.replace(
            "{ nested: [one, two], flag: true }",
            "{ flag: true, nested: [one, two] }",
        ))
        .assert()
        .success();
    for proposed in [original, with_metadata.replace("[one, two]", "[two]")] {
        nodex(root)
            .args(["check", "--content", "docs/a.md=-"])
            .write_stdin(proposed)
            .assert()
            .code(1);
    }
}
