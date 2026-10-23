//! `retarget`.

use crate::support::{
    init_project, nodex, run_envelope, run_json, scratch, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;

/// Superseding a document is the write that makes its live citations stale,
/// and the write lands: the finding belongs to each citer, never to the
/// supersession. `retarget` then repoints the id reference, and the path link
/// — which resolves to the superseded file as well as ever — is what stays
/// reported until it is repointed by hand. The successor's own citation of
/// what it replaced is never reported.
#[test]
fn superseding_a_cited_document_reports_each_live_citation() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    write_doc(
        root,
        "docs/old.md",
        "---\nid: old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n\n\
         Replaces [the old decision](old.md).\n",
    );
    write_doc(
        root,
        "docs/guide.md",
        "---\nid: guide\ntitle: Guide\nkind: generic\nstatus: active\nrelated: [old]\n---\n\
         # Guide\n\nFollows [the old decision](old.md).\n",
    );
    nodex(root).arg("build").assert().success();
    run_envelope(nodex(root).args(["lifecycle", "supersede", "old", "--to", "new"]));

    let stale_citations = || {
        nodex(root).arg("build").assert().success();
        let issues = run_json(nodex(root).args(["query", "issues"]));
        issues["violations"]
            .as_array()
            .expect("violations")
            .iter()
            .filter(|v| v["rule_id"] == "superseded_reference")
            .map(|v| {
                assert_eq!(v["node_id"], "guide", "{v}");
                assert_eq!(v["severity"], "warning", "{v}");
                assert_eq!(v["details"]["target"], "old", "{v}");
                assert_eq!(v["details"]["current"], serde_json::json!(["new"]), "{v}");
                v["details"]["relation"].as_str().unwrap().to_string()
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        stale_citations(),
        ["references", "related"].map(String::from).into(),
        "the field and the path link, and nothing from the successor"
    );

    run_envelope(nodex(root).args(["retarget", "old", "new"]));
    assert_eq!(
        stale_citations(),
        std::collections::BTreeSet::new(),
        "retarget carries both id and path references to the successor"
    );
}

#[test]
fn retarget_leaves_wikilink_that_binds_a_file_by_path() {
    // `[[old]]` next to a file `old.md` is a path edge to that file
    // (resolver path-first), not an id reference. `retarget` repoints
    // ids only, so it must leave the path-bound wikilink alone even
    // when its text equals the retargeted id.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [parser]\nwikilink_enabled = true\nextensions = [\".md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"doc-{stem}\"\n",
    )
    .unwrap();
    // File old.md → node id doc-old; a separate node carries the bare id `old`.
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: doc-old\ntitle: F\nkind: generic\nstatus: active\n---\n# F\n",
    );
    write_doc(
        tmp.path(),
        "node.md",
        "---\nid: old\ntitle: Node\nkind: generic\nstatus: active\n---\n# Node\n",
    );
    write_doc(
        tmp.path(),
        "succ.md",
        "---\nid: succ\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    let b = "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n[[old]] here\n";
    write_doc(tmp.path(), "b.md", b);
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["retarget", "old", "succ"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("b.md")).unwrap(),
        b,
        "a path-bound wikilink must survive id retargeting byte-identical"
    );
    let env = run_envelope(nodex(tmp.path()).args(["retarget", "old", "succ"]));
    assert!(
        env.get("warnings").is_none(),
        "a reference nothing asked to repoint is not a refusal: {env:?}"
    );
}

#[test]
fn retarget_says_which_references_it_could_not_repoint() {
    // A retarget moves no file, so a reference it could not respell goes
    // on naming exactly what it named and the project it leaves is in
    // order — every reader downstream sees a graph with nothing to say.
    // The command asked for something and got nothing, and it is the only
    // place that can be said.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [parser]\nwikilink_enabled = true\n",
    )
    .unwrap();
    // `[[succ]]` read from docs/ binds the file docs/succ.md, whose
    // document is somebody else — so no spelling of the successor id
    // reads back as the successor, and the repoint is given up.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\nsee [[target]]\n",
    );
    write_doc(
        root,
        "docs/t.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\nt\n",
    );
    write_doc(
        root,
        "docs/succ.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "other/s.md",
        "---\nid: succ\ntitle: U\nkind: generic\nstatus: active\n---\nu\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["retarget", "target", "succ"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(0)
    );
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("docs/a.md") && w.contains("still names") && w.contains("target")),
        "the retarget names the reference it left: {warnings:?}"
    );
    // Nothing else reports it: the project the retarget leaves is valid.
    nodex(root).arg("check").assert().success();
}

#[test]
fn retarget_names_the_successors_own_references() {
    // A repoint never turns a reference on the document holding it, so the
    // successor's own references to the predecessor stand. Unlike every
    // other reference a repoint leaves they leave nothing wrong behind —
    // the predecessor still exists, the edge still binds, `check` is green
    // — so no reader downstream has a reason to mention them, and
    // `total_updated: 0` reads the same whether the successor held them or
    // the project referenced the predecessor nowhere at all.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[parser]\nwikilink_enabled = true\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/old.md",
        "---\nid: spec-s1\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: learning-s1\ntitle: L\nkind: generic\nstatus: active\n\
         supersedes: [spec-s1]\nrelated: [spec-s1]\n---\nsee [[spec-s1]]\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["retarget", "spec-s1", "learning-s1"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(0)
    );
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    let named = warnings
        .iter()
        .filter_map(warning_msg)
        .find(|w| w.contains("docs/new.md"))
        .unwrap_or_else(|| panic!("the successor's kept references are named: {warnings:?}"));
    assert!(named.contains("frontmatter `related`"), "{named}");
    assert!(named.contains("1 body reference"), "{named}");
    assert!(
        !named.contains("supersedes"),
        "the succession record is not a reference the repoint declined: {named}"
    );
    // Named, never touched — and the project the retarget leaves is valid.
    let after = fs::read_to_string(root.join("docs/new.md")).unwrap();
    assert!(after.contains("related: [spec-s1]"), "{after}");
    assert!(after.contains("[[spec-s1]]"), "{after}");
    nodex(root).arg("check").assert().success();
}

#[test]
fn retarget_says_nothing_about_the_succession_record_alone() {
    // `supersedes: [old]` on the successor is what *makes* it the
    // successor, so a repoint has nothing to say about it: naming it would
    // put a line on every supersede-then-retarget there is, and a signal
    // that is always on carries nothing and buries the ones that do.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/old.md",
        "---\nid: spec-s1\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: learning-s1\ntitle: L\nkind: generic\nstatus: active\n\
         supersedes: [spec-s1]\n---\nl\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["retarget", "spec-s1", "learning-s1"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(0)
    );
    assert!(
        env.get("warnings").is_none(),
        "the succession record is not a finding: {env}"
    );
}

#[test]
fn retarget_counts_every_body_form_the_successor_keeps() {
    // The count is the body's whole account, so it has to reach every form
    // an id reference takes — a wikilink, a link-pattern capture, and the
    // same capture inside a code span. A destination is not one of them: it
    // spells a path, so no repoint was ever proposed for it.
    //
    // The successor's `supersedes` is excluded by name, and the id it holds
    // beside the predecessor is what proves the exclusion cannot bury
    // anything: a value that is not this repoint's predecessor never enters
    // the report to begin with.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = \"@cite\\\\(([^)]+)\\\\)\"\n\
         relation = \"references\"\ncode_spans = true\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/old.md",
        "---\nid: spec-s1\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "docs/leaf.md",
        "---\nid: generic-leaf\ntitle: U\nkind: generic\nstatus: active\n---\nu\n",
    );
    write_doc(
        root,
        "docs/new.md",
        "---\nid: learning-s1\ntitle: L\nkind: generic\nstatus: active\n\
         supersedes: [spec-s1, generic-leaf]\nrelated: [spec-s1]\n---\n\
         # L\n\n[[spec-s1]], `@cite(spec-s1)`, @cite(spec-s1), [t](old.md)\n",
    );
    // Not the successor, so its own `supersedes` is an ordinary reference
    // and moves like any other.
    write_doc(
        root,
        "docs/third.md",
        "---\nid: generic-third\ntitle: T\nkind: generic\nstatus: active\n\
         supersedes: [spec-s1]\n---\nt\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["retarget", "spec-s1", "learning-s1"]));
    assert_eq!(
        env.pointer("/data/references_updated/0")
            .and_then(Value::as_str),
        Some("docs/third.md"),
        "a non-successor `supersedes` is repointed: {env}"
    );
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    let named = warnings
        .iter()
        .filter_map(warning_msg)
        .find(|w| w.contains("docs/new.md"))
        .unwrap_or_else(|| panic!("the successor's kept references are named: {warnings:?}"));
    assert!(named.contains("frontmatter `related`"), "{named}");
    assert!(
        named.contains("4 body references"),
        "all authored reference forms count: {named}"
    );
    assert!(!named.contains("supersedes"), "{named}");
    assert!(
        fs::read_to_string(root.join("docs/third.md"))
            .unwrap()
            .contains("learning-s1")
    );
    nodex(root).arg("check").assert().success();
}

#[test]
fn retarget_rewrites_a_list_interrupted_by_a_column0_comment() {
    // A column-0 comment between `related` items is interior trivia of
    // the block: the rewrite replaces the block whole, so a file the
    // envelope reports as references_updated carries zero stale
    // predecessor ids — never a stale `- old` line re-attached to the
    // new list behind the comment.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "keep.md",
        "---\nid: keep-adr\ntitle: K\nkind: generic\nstatus: active\n---\n# K\n",
    );
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: stale-adr\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    write_doc(
        tmp.path(),
        "new.md",
        "---\nid: fresh-adr\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\nrelated:\n  - keep-adr\n# stale-adr pending replacement\n  - stale-adr\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["retarget", "stale-adr", "fresh-adr"]));
    assert!(
        data["references_updated"]
            .as_array()
            .map(|a| a.iter().any(|p| p == "b.md"))
            .unwrap_or(false),
        "the comment-interrupted relation list must be rewritten: {data}"
    );
    let content = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(
        !content.contains("stale-adr"),
        "zero stale predecessor ids may remain, including behind the comment: {content}"
    );
    assert!(
        content.contains("fresh-adr"),
        "successor id written: {content}"
    );
    nodex(tmp.path()).arg("check").assert().success();
}

#[cfg(unix)]
#[test]
fn composed_replacement_refuses_an_external_predecessor_before_citations_change() {
    use std::os::unix::fs::symlink;

    let root = scratch();
    let external = scratch();
    write_doc(
        root.path(),
        "nodex.toml",
        "[scope]\ninclude=[\"docs/**/*.md\"]\nfollow_symlinks=true\n[parser]\nwikilink_enabled=true\n",
    );
    let predecessor = "---\nid: old\nstatus: active\n---\n# Old\n";
    let citation = "---\nid: use\n---\n[[old]]\n";
    write_doc(external.path(), "old.md", predecessor);
    write_doc(root.path(), "docs/new.md", "---\nid: new\n---\n# New\n");
    write_doc(root.path(), "docs/use.md", citation);
    symlink(external.path(), root.path().join("docs/external")).unwrap();

    let output = nodex(root.path())
        .args(["retarget", "old", "new", "--supersede"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "PATH_ESCAPES_ROOT");
    assert_eq!(
        fs::read_to_string(external.path().join("old.md")).unwrap(),
        predecessor
    );
    assert_eq!(
        fs::read_to_string(root.path().join("docs/use.md")).unwrap(),
        citation
    );

    let ordinary = run_json(nodex(root.path()).args(["retarget", "old", "new"]));
    assert_eq!(ordinary["completion"], "complete");
    assert_eq!(
        fs::read_to_string(external.path().join("old.md")).unwrap(),
        predecessor
    );
    assert!(
        fs::read_to_string(root.path().join("docs/use.md"))
            .unwrap()
            .contains("[[new]]")
    );
}
