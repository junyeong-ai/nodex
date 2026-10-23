//! `lifecycle`: status writes and the fields they stamp.

use crate::support::{
    envelope_of, init_project, nodex, run_envelope, run_json, scratch, write_doc,
};
use serde_json::Value;
use std::fs;

#[test]
fn lifecycle_supersede_writes_minimal_diff() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: active\n# author note\n---\n# Old\n",
    );
    write_doc(
        tmp.path(),
        "docs/new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["lifecycle", "supersede", "doc-old", "--to", "doc-new"])
        .assert()
        .success();
    let content = fs::read_to_string(tmp.path().join("docs/old.md")).unwrap();
    // Touched fields use the canonical quoted form.
    assert!(content.contains(r#"status: "superseded""#));
    assert!(content.contains(r#"superseded_by: "doc-new""#));
    // Untouched lines — including the author's comment — are preserved
    // verbatim. A full YAML round-trip would have rewritten them.
    assert!(content.contains("id: doc-old"));
    assert!(content.contains("title: Old"));
    assert!(content.contains("kind: generic"));
    assert!(content.contains("# author note"));
    assert!(content.contains("# Old"));
    // Subsequent build picks up the change and materialises the
    // canonical supersedes edge.
    nodex(tmp.path())
        .arg("build")
        .arg("--full")
        .assert()
        .success();
    let data = run_json(nodex(tmp.path()).args(["query", "chain", "doc-old"]));
    assert_eq!(
        data.get("total").and_then(Value::as_u64),
        Some(2),
        "chain should walk old → new after lifecycle write"
    );
}

#[test]
fn lifecycle_refuses_action_whose_target_status_is_not_allowed() {
    // A project that only models draft/active/archived must load and
    // operate cleanly — lifecycle vocabulary is no longer forced into
    // every project's status set. A `set` whose target status the
    // project does not allow (here: "deprecated") is refused at the
    // write seam, leaving the document untouched, so the tool never
    // produces a doc its own `check` would reject.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"note\", \"generic\"]\n\
         [statuses]\nallowed = [\"draft\", \"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"draft\"\n\
         [[identity.kind_rules]]\nglob = \"**\"\nkind = \"note\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: note-a\ntitle: A\nkind: note\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // archived IS allowed → succeeds.
    nodex(tmp.path())
        .args(["lifecycle", "set", "note-a", "--status", "archived"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(tmp.path().join("a.md"))
            .unwrap()
            .contains(r#"status: "archived""#)
    );

    // deprecated is NOT allowed → refused, document untouched.
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: note-b\ntitle: B\nkind: note\nstatus: active\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let out = nodex(tmp.path())
        .args(["lifecycle", "set", "note-b", "--status", "deprecated"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("deprecated"),
        "names the refused status: {stdout}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("b.md"))
            .unwrap()
            .contains("status: active"),
        "refused transition must not touch the document"
    );
}

#[test]
fn lifecycle_set_refuses_status_with_unsatisfied_cross_field() {
    // The generic `set` writes only `status` (+ `updated`), so it must
    // refuse a status a `cross_field` rule governs while the required
    // field is absent — otherwise it would write a document its own
    // `check` rejects. `superseded` requires `superseded_by`; supplying
    // that is `supersede`'s job, not `set`'s. Config-driven: a project
    // that places no requirement on the status sets it freely.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\", \"archived\"]\n\
         terminal = [\"superseded\", \"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\n\
         cross_field = [{ when = \"status=superseded\", require = \"superseded_by\" }]\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // `set --status superseded` would dangle without a successor → refused.
    let out = nodex(tmp.path())
        .args(["lifecycle", "set", "generic-a", "--status", "superseded"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("superseded_by"),
        "names the required field: {stdout}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("a.md"))
            .unwrap()
            .contains("status: active"),
        "refused set must not touch the document"
    );

    // `archived` carries no cross-field requirement → set succeeds.
    nodex(tmp.path())
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(tmp.path().join("a.md"))
            .unwrap()
            .contains(r#"status: "archived""#)
    );
}

#[test]
fn lifecycle_does_not_launder_a_broken_field_and_does_not_refuse_over_one() {
    // The guard that used to refuse here was protecting against a laundering
    // that cannot happen: the editor rewrites the fields the action names and
    // leaves every other line exactly as it found it, so a malformed
    // `created:` is still malformed afterwards and `check` still flags it.
    // What refusing did instead was block a transition over a violation the
    // document already carried.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\ncreated: yesterday\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let flagged = |root: &std::path::Path| -> Vec<String> {
        envelope_of(nodex(root).arg("check"))
            .pointer("/data/violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter_map(|v| v["details"]["field"].as_str().map(str::to_string))
            .collect()
    };
    assert_eq!(flagged(tmp.path()), ["created"]);

    nodex(tmp.path())
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .success();

    let after = fs::read_to_string(tmp.path().join("a.md")).unwrap();
    assert!(
        after.contains("created: yesterday"),
        "the broken line is left exactly as it was: {after}"
    );
    assert!(after.contains("status: \"archived\""), "{after}");
    assert_eq!(
        flagged(tmp.path()),
        ["created"],
        "still flagged, so nothing was laundered"
    );
}

/// A document with no frontmatter has no block for `lifecycle` to edit. That
/// is not a malformed block, so the refusal says what is missing and which
/// command writes it, and the command it names makes the same move succeed.
#[test]
fn lifecycle_on_a_document_without_frontmatter_names_the_command_that_writes_one() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n",
    )
    .unwrap();
    write_doc(tmp.path(), "a.md", "# A\n");
    nodex(tmp.path()).arg("build").assert().success();

    let refused = envelope_of(nodex(tmp.path()).args([
        "lifecycle",
        "set",
        "generic-a",
        "--status",
        "archived",
    ]));
    assert_eq!(refused["error"]["code"], "PARSE_ERROR", "{refused}");
    let message = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("no frontmatter block to edit")
            && message.contains("migrate --apply")
            && !message.contains("parse error"),
        "{message}"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("a.md")).unwrap(),
        "# A\n"
    );

    nodex(tmp.path())
        .args(["migrate", "--apply"])
        .assert()
        .success();
    nodex(tmp.path())
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .success();
}

#[test]
fn lifecycle_supersede_refuses_when_a_non_superseded_by_cross_field_is_unmet() {
    // `supersede` supplies `superseded_by`, but if the project requires
    // ANOTHER field once a doc is superseded, supersede must refuse just
    // as `set` does — otherwise it writes a document its own `check`
    // rejects (the self-consistency invariant). Same target status, same
    // missing field → same outcome across both write seams.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema.enums]\ndeprecation_note = [\"pending\", \"done\"]\n\
         [[schema.cross_field]]\nwhen = \"status=superseded\"\nrequire = \"deprecation_note\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: generic-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        tmp.path(),
        "new.md",
        "---\nid: generic-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    // supersede would write status=superseded without deprecation_note →
    // refused, naming the field, leaving the doc untouched.
    let out = nodex(tmp.path())
        .args([
            "lifecycle",
            "supersede",
            "generic-old",
            "--to",
            "generic-new",
        ])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("deprecation_note"),
        "names the field: {stdout}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("old.md"))
            .unwrap()
            .contains("status: active"),
        "refused supersede must not touch the document"
    );
}

#[test]
fn lifecycle_supersede_proceeds_when_only_superseded_by_is_required() {
    // The common case: the cross_field requires exactly the field
    // supersede supplies (`superseded_by`), so it proceeds and the
    // written doc passes check.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [detection]\norphan_ok_kinds = [\"generic\"]\n\
         [[schema.cross_field]]\nwhen = \"status=superseded\"\nrequire = \"superseded_by\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: generic-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        tmp.path(),
        "new.md",
        "---\nid: generic-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args([
            "lifecycle",
            "supersede",
            "generic-old",
            "--to",
            "generic-new",
        ])
        .assert()
        .success();
    nodex(tmp.path()).arg("build").assert().success();
    let env = run_envelope(nodex(tmp.path()).arg("check"));
    assert_eq!(env.pointer("/data/total").and_then(Value::as_i64), Some(0));
}

#[test]
fn lifecycle_leaves_out_a_stamp_the_kind_forbids_and_refuses_a_move_that_writes_one() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\", \"learning\", \"adr\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\", \"superseded\"]\n\
         terminal = [\"archived\", \"superseded\"]\n\
         [[identity.kind_rules]]\nglob = \"adr/**\"\nkind = \"adr\"\n\
         [[identity.kind_rules]]\nglob = \"**\"\nkind = \"learning\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [[schema.overrides]]\nkinds = [\"learning\"]\nforbidden = [\"updated\"]\n\
         [[schema.overrides]]\nkinds = [\"adr\"]\nforbidden = [\"superseded_by\"]\n",
    )
    .unwrap();
    let old = "---\ntitle: Old\nstatus: active\n---\n# Old\n";
    let new = "---\ntitle: New\nstatus: active\n---\n# New\n";
    write_doc(root, "old.md", old);
    write_doc(root, "new.md", new);
    write_doc(root, "adr/old.md", old);
    write_doc(root, "adr/new.md", new);
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args([
            "lifecycle",
            "supersede",
            "learning-old",
            "--to",
            "learning-new",
        ])
        .assert()
        .success();
    let superseded = fs::read_to_string(root.join("old.md")).unwrap();
    assert!(superseded.contains("superseded_by"), "{superseded}");
    assert!(!superseded.contains("updated:"), "{superseded}");

    nodex(root)
        .args(["lifecycle", "set", "learning-new", "--status", "archived"])
        .assert()
        .success();
    let archived = fs::read_to_string(root.join("new.md")).unwrap();
    assert!(archived.contains(r#"status: "archived""#), "{archived}");
    assert!(!archived.contains("updated:"), "{archived}");

    let out = nodex(root)
        .args(["lifecycle", "supersede", "adr-old", "--to", "adr-new"])
        .assert()
        .failure();
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS"),
        "{env}"
    );
    assert_eq!(fs::read_to_string(root.join("adr/old.md")).unwrap(), old);

    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();
}

/// A field the action overwrites is repaired by the write, and one it does
/// not touch is left alone rather than made a reason to refuse.
#[test]
fn lifecycle_repairs_the_field_it_writes_and_leaves_the_others() {
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
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\nreviewed: not-a-date\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\ncreated: not-a-date\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args(["lifecycle", "review", "a"])
        .assert()
        .success();
    assert!(
        !fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("not-a-date"),
        "review rewrote the field it writes"
    );

    nodex(root)
        .args(["lifecycle", "review", "b"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(root.join("docs/b.md"))
            .unwrap()
            .contains("created: not-a-date"),
        "a field the action does not write is untouched, not a refusal"
    );
}

/// The seam reads a document the way the graph does, or it guards a document
/// nobody else has.
///
/// `FrontmatterEditor` is a line reader: `status: ~` is the text `"~"` to it
/// and YAML null to the parser, which fills `statuses.initial`. Under a
/// terminal initial status the graph reported the document terminal while the
/// seam let a transition out of it.
#[test]
fn lifecycle_reads_the_document_the_graph_holds() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"done\", \"active\"]\nterminal = [\"done\"]\ninitial = \"done\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: ~\n---\n# A\n",
    );
    nodex(root).args(["build", "--full"]).assert().success();
    let env = run_envelope(nodex(root).args(["query", "nodes"]));
    assert_eq!(
        env.pointer("/data/items/0/status").and_then(Value::as_str),
        Some("done"),
        "the graph resolves the null to the initial status: {env}"
    );

    let output = nodex(root)
        .args(["lifecycle", "set", "a", "--status", "active"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_TRANSITION")
    );
}

#[test]
fn lifecycle_review_refuses_to_overwrite_future_reviewed_date() {
    // Future-dated `reviewed` carries real information (clock skew or
    // intentional "approved through" marker). `Action::Review` must
    // refuse rather than silently replace it with today — that loss
    // would violate the monotonicity assumption the audit trail
    // relies on. INVALID_TRANSITION with the existing date in the
    // `from → to` payload is the contract.
    let tmp = scratch();
    init_project(tmp.path());
    let original =
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nreviewed: 2099-01-01\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["lifecycle", "review", "doc-a"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_TRANSITION")
    );
    let after = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    assert_eq!(
        after, original,
        "rejected review must leave source byte-for-byte unchanged: {after}"
    );
}

#[test]
fn lifecycle_review_updates_past_reviewed_date() {
    // Happy path: a past `reviewed` date is bumped to today. Lock in
    // so the future-date guard above doesn't accidentally reject the
    // normal case too.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nreviewed: 2000-01-01\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["lifecycle", "review", "doc-a"])
        .assert()
        .success();
    let after = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    assert!(
        !after.contains("reviewed: 2000-01-01") && !after.contains("reviewed: \"2000-01-01\""),
        "past reviewed date must be bumped: {after}"
    );
}

#[test]
fn lifecycle_supersede_rejects_unknown_successor() {
    // Self-consistency invariant: nodex must never write a frontmatter
    // value the next `build` would surface as a broken edge. A
    // supersede pointing at a non-existent id must be rejected BEFORE
    // any file mutation, with NOT_FOUND surfaced at exit 2. The
    // source file's frontmatter must be byte-for-byte unchanged.
    let tmp = scratch();
    init_project(tmp.path());
    let original = "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["lifecycle", "supersede", "a", "--to", "ghost"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("NOT_FOUND")
    );
    let after = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    assert_eq!(
        after, original,
        "rejected lifecycle must leave source untouched: {after}"
    );
}

#[test]
fn lifecycle_supersede_rejects_cycle() {
    // Pre-existing chain A -> B -> C; closing it via `supersede C --to A`
    // would silently corrupt the graph. The pre-check must surface
    // CYCLE_DETECTED at exit 2 *before* mutating C's frontmatter.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: superseded\nsuperseded_by: b\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: superseded\nsuperseded_by: c\n---\n# B\n",
    );
    let c_original = "---\nid: c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n";
    write_doc(tmp.path(), "docs/c.md", c_original);
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["lifecycle", "supersede", "c", "--to", "a"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CYCLE_DETECTED")
    );
    let after = fs::read_to_string(tmp.path().join("docs/c.md")).unwrap();
    assert_eq!(
        after, c_original,
        "rejected cycle-introducing supersede must leave source untouched: {after}"
    );
}

#[test]
fn lifecycle_set_treats_explicitly_empty_typed_attr_as_missing() {
    // The guard shares the cross_field rule's own `is_field_missing`
    // semantics: a typed attr declared as an explicitly empty string is
    // missing to the rule, so it must be missing to the guard too —
    // otherwise `set` would write a document its own `check` rejects.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\n\
         enums = { reason = [\"cleanup\", \"superseded-by-plan\"] }\n\
         cross_field = [{ when = \"status=archived\", require = \"reason\" }]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/empty.md",
        "---\nid: generic-empty\ntitle: Empty\nkind: generic\nstatus: active\nreason: \"\"\n---\n# E\n",
    );
    write_doc(
        root,
        "docs/filled.md",
        "---\nid: generic-filled\ntitle: Filled\nkind: generic\nstatus: active\nreason: cleanup\n---\n# F\n",
    );
    nodex(root).arg("build").assert().success();

    // Explicitly empty → missing → refused, document untouched.
    let out = nodex(root)
        .args(["lifecycle", "set", "generic-empty", "--status", "archived"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("reason"),
        "names the missing field: {stdout}"
    );
    assert!(
        fs::read_to_string(root.join("docs/empty.md"))
            .unwrap()
            .contains("status: active")
    );

    // A real value satisfies the requirement → set succeeds.
    nodex(root)
        .args(["lifecycle", "set", "generic-filled", "--status", "archived"])
        .assert()
        .success();
}

#[test]
fn lifecycle_set_guards_a_predicate_keyed_on_the_updated_field_it_writes() {
    // `set` writes `updated` as well as `status`, so a cross_field
    // predicate keyed on `updated` is one the action can activate — the
    // guard must answer for it, not only status-keyed predicates.
    // `when updated exists require reason`: the set writes `updated`, so
    // the predicate fires, and a missing `reason` would make the written
    // doc fail check — the guard refuses up front.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\n\
         enums = { reason = [\"cleanup\"] }\n\
         cross_field = [{ when = \"updated exists\", require = \"reason\" }]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // No `reason` → the write would make `updated` exist while `reason`
    // is missing → refused, document untouched.
    let out = nodex(root)
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .failure()
        .code(2);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("reason"),
        "names the missing field: {stdout}"
    );
    assert!(
        fs::read_to_string(root.join("docs/a.md"))
            .unwrap()
            .contains("status: active"),
        "refused set leaves the document untouched"
    );

    // With `reason` already present, the same set is clean.
    write_doc(
        root,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\nreason: cleanup\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["lifecycle", "set", "generic-b", "--status", "archived"])
        .assert()
        .success();
}

#[test]
fn lifecycle_set_allows_status_whose_required_field_set_itself_writes() {
    // The guard evaluates cross_field against the POST-set document, so
    // a requirement `set` itself satisfies (`updated`, written on every
    // set) must never false-reject a valid transition — the document
    // the guard judges is exactly the one `check` would see.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [schema]\n\
         cross_field = [{ when = \"status=archived\", require = \"updated\" }]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // `set --status archived` writes `updated: <today>`, satisfying the
    // requirement → it must succeed, not false-reject.
    nodex(root)
        .args(["lifecycle", "set", "generic-a", "--status", "archived"])
        .assert()
        .success();
    let written = fs::read_to_string(root.join("docs/a.md")).unwrap();
    assert!(written.contains(r#"status: "archived""#));
    assert!(written.contains("updated:"));

    // And the written document passes the project's own check (the
    // self-consistency invariant the guard exists to protect).
    nodex(root).arg("build").assert().success();
    let data = run_json(nodex(root).args(["check"]));
    assert_eq!(data.pointer("/total").and_then(Value::as_u64), Some(0));
}
