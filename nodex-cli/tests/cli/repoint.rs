//! References a move or retarget rewrites: what is repointed, declined and
//! announced.

use crate::support::{
    init_project, nodex, run_envelope, run_json, scratch, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;

#[test]
fn markdown_uri_paths_resolve_and_rename_with_query_and_fragment_preserved() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    write_doc(
        root,
        "docs/space name.md",
        "---\nid: target\n---\n# Target\n",
    );
    write_doc(
        root,
        "docs/source.md",
        "---\nid: source\n---\n[encoded](space%20name.md?plain=1#part)\n[external](S3://bucket/missing.md)\n[remote](//host/missing.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).args(["query", "issues"]));
    assert!(issues["unresolved_edges"].as_array().unwrap().is_empty());
    let target = run_json(nodex(root).args(["query", "backlinks", "target"]));
    assert_eq!(target["total"], 1);
    nodex(root)
        .args(["rename", "docs/space name.md", "docs/new#name.md"])
        .assert()
        .success();
    let source = fs::read_to_string(root.join("docs/source.md")).unwrap();
    assert!(source.contains("new%23name.md?plain=1#part"), "{source}");
    assert!(source.contains("S3://bucket/missing.md"));
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).args(["query", "issues"]));
    assert!(issues["unresolved_edges"].as_array().unwrap().is_empty());
    let target = run_json(nodex(root).args(["query", "backlinks", "target"]));
    assert_eq!(target["total"], 1);
}

#[test]
fn a_rename_that_leaves_a_referrer_binding_something_else_says_so() {
    // The referrer stands still and the rename takes the rung its
    // reference stood on out from under it. `[x](a.md)` bound the root
    // document by the literal frame; once that path is gone the
    // source-relative frame answers with the neighbour. The graph that
    // leaves is valid, so the rename is the only place it can be said —
    // and this pins the saying, not the computing.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        r#"[scope]
include = ["**/*.md"]
[[parser.link_patterns]]
pattern = '@ref\(([a-z]+)\)'
relation = "references"
"#,
    )
    .unwrap();
    write_doc(
        root,
        "a.md",
        "---\nid: root-a\ntitle: RA\nkind: generic\nstatus: active\n---\nx\n",
    );
    write_doc(
        root,
        "docs/a.md",
        "---\nid: shadow\ntitle: SH\nkind: generic\nstatus: active\n---\nx\n",
    );
    write_doc(
        root,
        "docs/ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n@ref(a)\n",
    );
    // The configured capture admits only letters, so it cannot carry the new name.
    let env = run_envelope(nodex(root).args(["rename", "a.md", "a#1.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("root-a") && w.contains("shadow")),
        "the rename names both documents the reference stood between: {warnings:?}"
    );
    nodex(root).arg("check").assert().success();
}

#[test]
fn a_move_that_repoints_a_reference_it_could_not_re_render_says_so() {
    // A relative reference means whatever it means from where it sits, so
    // a move can leave one naming a different document. The graph that
    // results is valid and `check` has nothing to say about it, so the
    // rename is the only place it can be said — and saying it is what
    // this pins, not merely computing it.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '@ref\\(([a-z]+)\\)'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    // The capture takes only letters, so `../a/x` is a spelling the
    // pattern cannot read back: the reference is left exactly as it is.
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n@ref(x)\n",
    );
    write_doc(
        root,
        "a/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "b/x.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    let env = run_envelope(nodex(root).args(["rename", "a/mover.md", "b/mover.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("desired") && w.contains("shadow")),
        "the move names both documents the reference stood between: {warnings:?}"
    );
    // Nothing else reports it: the project the move leaves is valid.
    nodex(root).arg("check").assert().success();
}

#[test]
fn a_move_says_nothing_about_a_self_reference_it_leaves_correctly_spelled() {
    // A document that refers to itself by path carries the one reference a
    // move must not talk about: it names the moving document, so it names
    // the same document wherever the file lands. Rewritten in two passes,
    // the second read the first's output as the author's own text and
    // reported a reference the author never wrote as one the move had
    // repointed — every claim in it false.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n\
         self [me](mover.md) and [x](other.md)\n",
    );
    write_doc(
        root,
        "a/other.md",
        "---\nid: other\ntitle: O\nkind: generic\nstatus: active\n---\no\n",
    );
    let env = run_envelope(nodex(root).args(["rename", "a/mover.md", "b/mover.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        env.get("warnings").is_none(),
        "nothing was repointed behind the author's back: {env:?}"
    );
    let moved = fs::read_to_string(root.join("b/mover.md")).unwrap();
    assert!(
        moved.contains("[me](mover.md)"),
        "the self-reference names the same document from either directory: {moved}"
    );
    assert!(
        moved.contains("[x](../a/other.md)"),
        "the reference to a document that stood still is rebased: {moved}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "mover"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|edge| edge.get("target").and_then(Value::as_str) == Some("mover")),
        "the self-edge survives the move: {outgoing:?}"
    );
}

#[test]
fn a_rename_carries_the_moved_document_own_reference_to_itself() {
    // A reference a document makes to itself names itself wherever it
    // lands, whatever the project comes to call it — the bytes it lives
    // in are the bytes at the destination, so there is no other document
    // the write could reach. Refusing it over an id the move shifted buys
    // nothing and costs the document its own reference.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("a")).unwrap();
    fs::write(root.join("a/r.md"), "[self](r.md)\n").unwrap();
    let env = run_envelope(nodex(root).args(["rename", "a/r.md", "a/r2.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("a/r2.md"))
            .unwrap()
            .contains("[self](r2.md)"),
        "the self-reference follows the document"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "issues"]));
    assert_eq!(
        env.pointer("/data/unresolved_edges")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "nothing was left dangling: {env:?}"
    );
}

#[test]
fn a_rename_leaves_references_to_a_document_whose_identity_it_cannot_carry() {
    // A document carrying no frontmatter has nowhere to anchor its id, so
    // a rename that changes the stem gives it a different one, and the
    // document the references named is not in the project the rename
    // leaves. A repoint would be a repoint at whatever stands there, and
    // what stands there is the thing in doubt — so the references are left
    // as they are, and the edge they carried comes to dangle where every
    // reader can see it.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    fs::write(root.join("bare.md"), "just body\n").unwrap();
    write_doc(
        root,
        "r.md",
        "---\nid: r\ntitle: R\nkind: generic\nstatus: active\n---\n[x](bare.md)\n",
    );
    let env = run_envelope(nodex(root).args(["rename", "bare.md", "renamed.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(0)
    );
    assert!(
        fs::read_to_string(root.join("r.md"))
            .unwrap()
            .contains("[x](bare.md)"),
        "no repoint was written over an identity the rename could not carry"
    );
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("generic-bare") && w.contains("generic-renamed")),
        "the rename names the identity it could not carry: {warnings:?}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "issues"]));
    let unresolved = env
        .pointer("/data/unresolved_edges")
        .and_then(Value::as_array)
        .expect("unresolved");
    assert!(
        unresolved
            .iter()
            .any(|edge| edge.get("source").and_then(Value::as_str) == Some("r")),
        "the reference the rename left is read as the dangling one it is: {unresolved:?}"
    );
}

#[test]
fn a_move_writes_no_repoint_at_a_path_the_move_gives_to_somebody_else() {
    // The target does not move; the move evicts it from scope, and the
    // spelling that would name it from the new directory is shadowed by a
    // document at the root. A repoint judged against the project the move
    // leaves would ask what stands at the destination and then check that
    // the write names what stands at the destination — which is no check
    // at all, and the edge changes document under an envelope reporting
    // success.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[scope.conditional_exclude]]\n\
         parent_glob = \"b/SPEC.md\"\nchild_glob = \"b/desired.md\"\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/deep/SPEC.md",
        "---\nid: spec\ntitle: S\nkind: generic\nstatus: archived\n---\n\
         [target](../../b/desired.md)\n",
    );
    write_doc(
        root,
        "b/desired.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "desired.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/deep/SPEC.md", "b/SPEC.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("b/SPEC.md"))
            .unwrap()
            .contains("[target](../../b/desired.md)"),
        "the reference is left as it is rather than pointed at the shadow"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "spec"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("target").and_then(Value::as_str) != Some("shadow")),
        "no edge was moved onto a document nobody named: {outgoing:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_move_writes_no_repoint_where_the_moved_file_changes_which_document_it_is() {
    // A relative symlink carried across directories resolves to different
    // bytes, so the document standing at the destination is not the one
    // that stood at the source. The reference named the one that stood at
    // the source; nothing the move leaves is it, so nothing is written.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         exclude = [\"a/target.md\", \"b/target.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/target.md",
        "---\nid: document-a\ntitle: A\nkind: generic\nstatus: active\n---\na\n",
    );
    write_doc(
        root,
        "b/target.md",
        "---\nid: document-b\ntitle: B\nkind: generic\nstatus: active\n---\nb\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: referrer\ntitle: R\nkind: generic\nstatus: active\n---\n[t](a/link.md)\n",
    );
    std::os::unix::fs::symlink("target.md", root.join("a/link.md")).unwrap();
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/link.md", "b/link.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("[t](a/link.md)"),
        "the reference is left as it is rather than pointed at another document"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "referrer"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("target").and_then(Value::as_str) != Some("document-b")),
        "no edge was moved onto the document the link came to resolve to: {outgoing:?}"
    );
}

#[test]
fn a_rename_repoints_a_reference_the_graph_bound_past_a_file_carrying_no_document() {
    // The ladder's first rung holds a file whose parse failed, so it
    // carries no document and the build bound the reference one rung
    // lower. Read against the scanned paths instead of against the graph,
    // the rewrite called that file the reference's target, left the
    // reference alone, and reported success over the edge it stranded.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    fs::write(root.join("x.md"), "---\nid: [\n---\nbroken\n").unwrap();
    write_doc(
        root,
        "a/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n[target](x.md)\n",
    );
    let env = run_envelope(nodex(root).args(["rename", "a/x.md", "a/z.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "the reference the graph bound is repointed: {env:?}"
    );
    let referrer = fs::read_to_string(root.join("a/mover.md")).unwrap();
    assert!(
        referrer.contains("[target](z.md)"),
        "repointed in its own frame: {referrer}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "mover"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|edge| edge.get("target").and_then(Value::as_str) == Some("desired")),
        "the edge the move was supposed to carry: {outgoing:?}"
    );
}

#[test]
fn a_retarget_says_nothing_about_a_reference_whose_bytes_it_rewrote() {
    // One span, two readers binding it under one relation: the wikilink
    // and a pattern spelling the same brackets. The repoint lands for the
    // first and covers the second, whose bytes are then not its own — so
    // there is nothing left standing to report, and reporting it would
    // name a reference the document plainly no longer holds.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = '\\[\\[(old)\\]\\]'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\nsee [[old]]\n",
    );
    write_doc(
        root,
        "old.txt.md",
        "---\nid: old\ntitle: O\nkind: generic\nstatus: active\n---\no\n",
    );
    write_doc(
        root,
        "new.txt.md",
        "---\nid: new\ntitle: N\nkind: generic\nstatus: active\n---\nn\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["retarget", "old", "new"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("see [[new]]"),
        "the repoint landed"
    );
    assert!(
        env.get("warnings").is_none(),
        "nothing was left standing to report: {env:?}"
    );
}

#[test]
fn a_repoint_says_the_frame_out_loud_where_the_plain_spelling_is_taken() {
    // The plain rendering of the reference's own frame is one a document
    // at the root takes: it differs from what the author wrote, so it is
    // a rewrite, and it names somebody else, so the gate refuses it.
    // Which of the two spellings *names* the document is the gate's to
    // answer, so both are offered and it takes the one that does.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/t.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "u.md",
        "---\nid: rootu\ntitle: U\nkind: generic\nstatus: active\n---\nu\n",
    );
    write_doc(
        root,
        "a/ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[x](t.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/t.md", "a/u.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("a/ref.md"))
            .unwrap()
            .contains("[x](u.md)"),
        "spelled so nothing can take it"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|edge| edge.get("target").and_then(Value::as_str) == Some("desired")),
        "it names what it named: {outgoing:?}"
    );
}

#[test]
fn a_newcomer_at_the_root_does_not_take_a_reference_that_says_its_frame() {
    // A document arriving at the root takes over the literal rung, so a
    // source-relative reference below it comes to name the newcomer. Its
    // frame rendered plainly is the spelling it already has, which says
    // nothing; said out loud it is the same frame, the same document, and
    // out of the newcomer's reach — and it is a link every other reader
    // follows to the same place, which a root-relative rewrite would not
    // have been.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "other.md",
        "---\nid: newcomer\ntitle: N\nkind: generic\nstatus: active\n---\nn\n",
    );
    write_doc(
        root,
        "a/ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[target](x.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "other.md", "x.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("a/ref.md"))
            .unwrap()
            .contains("[target](x.md)"),
        "spelled so nothing can take it"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|edge| edge.get("target").and_then(Value::as_str) == Some("desired")),
        "it names what it named: {outgoing:?}"
    );
}

#[test]
fn a_move_says_which_references_it_declined_to_repoint() {
    // Every other reason a reference is left — a lock, a symlink, a fence
    // that does not parse — the rename names. The one it did not name is
    // its own: that no rewrite it could accept was available. The
    // reference names nothing afterwards, so the next build reports an
    // unresolved edge — a fact about the project, arriving later. That
    // the command declined is knowable only while it is running.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '@r\\((.+)\\)'\n\
         relation = \"primary\"\n\
         [[parser.link_patterns]]\npattern = '(shadow)'\n\
         relation = \"secondary\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "nodes/old.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "nodes/shadow-doc.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n@r(nodes/old.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "nodes/old.md", "nodes/new-shadow.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("ref.md") && w.contains("nodes/old.md")),
        "the rename names the reference it left: {warnings:?}"
    );
}

#[test]
fn a_move_counts_only_the_references_it_leaves_a_place_for() {
    // A rewrite may take a reference whose bytes it replaced entirely, so
    // one leaves and, under a total that never moved, one may arrive: an
    // edge traded for another that nobody wrote. What the original held
    // is the places it holds references in, minus the ones taken.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '(ld)'\nrelation = \"lost\"\n\
         [[parser.link_patterns]]\npattern = '(shadow)'\nrelation = \"minted\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "old.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "s.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[t](old.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "old.md", "shadowy.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("relation").and_then(Value::as_str) != Some("minted")),
        "the taken reference left no room for a minted one: {outgoing:?}"
    );
}

#[test]
fn a_move_writes_no_repoint_that_frees_a_place_for_another_relation() {
    // A capture covered by a repointed destination can come to spell
    // exactly what the destination spells, and the two are then one place
    // — rightly, they are one edge. The place that frees belongs to their
    // relation and to no other, so a reader of a relation the document
    // never had may not take it: counted over the document as a whole it
    // does, and the edge it brings arrives under a total that never moved.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '(a\\.md|\\.\\./b\\.md)'\n\
         relation = \"references\"\n\
         [[parser.link_patterns]]\npattern = '\\.\\./(b\\.md)'\n\
         relation = \"minted\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "a.md",
        "---\nid: alpha\ntitle: A\nkind: generic\nstatus: active\n---\na\n",
    );
    write_doc(
        root,
        "docs/r.md",
        "---\nid: r\ntitle: R\nkind: generic\nstatus: active\n---\n[t](../a.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a.md", "b.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "r"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("relation").and_then(Value::as_str) != Some("minted")),
        "no edge took the place another relation gave up: {outgoing:?}"
    );
}

#[test]
fn a_move_writes_no_repoint_a_second_reader_of_one_span_turns_into_an_edge() {
    // The same bytes read twice are one edge only where the readers agree
    // on relation and target — which is what `carried` means by it. Read
    // as one place regardless, a reader of a relation the document never
    // had reads as no arrival, and the edge it brings arrives unremarked.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '(new\\.md)'\n\
         relation = \"minted\"\n",
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
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[target](old.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "old.md", "new.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("relation").and_then(Value::as_str) != Some("minted")),
        "no edge of a relation the document never held: {outgoing:?}"
    );
}

#[test]
fn a_move_says_a_rebound_reference_once_however_many_readers_found_it() {
    // A span the wikilink reader and a pattern of the same relation both
    // bind is one reference, and what became of it is one fact. Answered
    // per reader, a move names it once per pattern that happened to see
    // it — the same reference, in the same words, twice over.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '@ref\\(([a-z]+)\\)'\n\
         relation = \"references\"\n\
         [[parser.link_patterns]]\npattern = '@ref\\((x)\\)'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n@ref(x)\n",
    );
    write_doc(
        root,
        "a/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "b/x.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/mover.md", "b/mover.md"]));
    let empty = Vec::new();
    let said: Vec<&str> = env
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter_map(warning_msg)
        .collect();
    assert_eq!(
        said.iter().filter(|one| one.contains("shadow")).count(),
        1,
        "one reference, one answer: {said:?}"
    );
}

#[test]
fn a_repoint_says_a_wikilink_frame_in_the_vocabulary_wikilinks_have() {
    // The plain rendering `x` would be taken by the `x.md` arriving at
    // the root, so the reference has to say which frame it is in. A
    // destination says that with `./`. A wikilink has no `./` — Obsidian,
    // Foam, Dendron and Logseq all fail to follow `[[./x]]` — and says it
    // by naming from the root instead, which is where they look and which
    // rung nothing can get in front of.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[parser]\nwikilink_enabled = true\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/u.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\nm\n",
    );
    write_doc(
        root,
        "x.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "a/ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\nsee [[u]] and [x](u.md) here\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/u.md", "a/x.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let body = fs::read_to_string(root.join("a/ref.md")).unwrap();
    assert!(
        body.contains("[[a/x]]") && body.contains("[x](x.md)"),
        "each form says its frame in its own words: {body}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let reached: Vec<&str> = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing")
        .iter()
        .filter_map(|edge| edge.get("target").and_then(Value::as_str))
        .collect();
    assert_eq!(reached, ["mover"], "and both still name the mover");
}

#[test]
fn a_move_announces_a_reference_it_carried_over_and_wrote_around() {
    // The moved document's own `[t](sub/w.md)` is rebased to
    // `[t](c/sub/w.md)`, and the capture inside it still says `w.md` word
    // for word — the rewrite wrote over it and left it exactly as its
    // author had it. What it *reaches* changed all the same, because the
    // document it sits in moved and a relative reference means whatever it
    // means from where it sits. Refusing would not prevent that: the
    // unrewritten bytes rebind just the same. Saying so is the whole of
    // what the seam can do, so it has to do it.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '\\b([a-z0-9]+\\.md)\\b'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/c/u.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n[t](sub/w.md)\n",
    );
    write_doc(
        root,
        "a/c/sub/w.md",
        "---\nid: inner\ntitle: I\nkind: generic\nstatus: active\n---\ni\n",
    );
    write_doc(
        root,
        "a/w.md",
        "---\nid: outer\ntitle: O\nkind: generic\nstatus: active\n---\no\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/c/u.md", "a/v.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let empty = Vec::new();
    let said: Vec<&str> = env
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter_map(warning_msg)
        .collect();
    assert!(
        said.iter()
            .any(|one| one.contains("w.md") && one.contains("outer")),
        "the move names the document it left the reference reaching: {said:?}"
    );
}

#[test]
fn a_move_announces_every_edge_the_document_comes_out_holding() {
    // A pattern twinning the destination reads its own old spelling back
    // out of the successor, and the name the moved file vacated uncovers a
    // shadow down the bare-id rung. The rename may leave the referrer
    // reaching that shadow — the reference does either way — but not
    // without saying so, and a reader of the report is who decides.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = '(old\\.md)'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "old.md",
        "---\nid: moved\ntitle: M\nkind: generic\nstatus: active\n---\nm\n",
    );
    write_doc(
        root,
        "z1.md",
        "---\nid: old\ntitle: Z\nkind: generic\nstatus: active\n---\nz\n",
    );
    write_doc(
        root,
        "z2.md",
        "---\nid: old.md\ntitle: Z\nkind: generic\nstatus: active\n---\nz\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[[old]][t](old.md)\n",
    );
    let reached = |root: &std::path::Path| -> Vec<String> {
        nodex(root).arg("build").assert().success();
        let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
        env.pointer("/data/outgoing")
            .and_then(Value::as_array)
            .expect("outgoing")
            .iter()
            .filter_map(|edge| edge.get("target").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    };
    let before = reached(root);
    let env = run_envelope(nodex(root).args(["rename", "old.md", "x-old.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let empty = Vec::new();
    let said: String = env
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter_map(warning_msg)
        .collect();
    for arrival in reached(root).iter().filter(|to| !before.contains(to)) {
        assert!(
            said.contains(arrival.as_str()),
            "the rename says which document it left the referrer reaching: \
             {arrival} not in {said:?}"
        );
    }
}

#[test]
fn a_move_writes_no_repoint_that_reads_one_place_as_room_for_two_edges() {
    // `[[old]]` read by the wikilink reader and by a pattern of the same
    // relation is one place and one edge, whatever the readers number.
    // Counted as two, the relation has room under it for a second
    // document — and the successor spelling brings one, down the bare-id
    // rung the vacated filename uncovers.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = '\\[\\[(old)\\]\\]'\n\
         relation = \"references\"\n\
         [[parser.link_patterns]]\npattern = '\\[\\[x-(old)\\]\\]'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "old.md",
        "---\nid: moved\ntitle: M\nkind: generic\nstatus: active\n---\nm\n",
    );
    write_doc(
        root,
        "z.md",
        "---\nid: old\ntitle: Z\nkind: generic\nstatus: active\n---\nz\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[[old]]\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "old.md", "x-old.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing.len() <= 1,
        "one place carries one edge: {outgoing:?}"
    );
}

#[test]
fn a_move_writes_no_repoint_that_takes_a_covered_reference_at_both_its_words() {
    // A covered reference may honestly say either what its repoint
    // intends or what its own bytes name — the write around it is
    // somebody else's rendering of a file, and a reader inside it gets a
    // frame of its own. It says one of them, though. Here the successor
    // spelling holds a span for each, so the second is an edge nobody
    // wrote, arriving under an answer the first had already given.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '(old\\.md)'\nrelation = \"pat\"\n\
         [[parser.link_patterns]]\npattern = '(x-old\\.md)'\nrelation = \"pat\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "old.md",
        "---\nid: moved\ntitle: M\nkind: generic\nstatus: active\n---\nm\n",
    );
    // Reachable only down the bare-id rung, and only once the file is gone.
    write_doc(
        root,
        "z.md",
        "---\nid: old.md\ntitle: Z\nkind: generic\nstatus: active\n---\nz\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[t](old.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "old.md", "x-old.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    let under_pat: Vec<&str> = outgoing
        .iter()
        .filter(|edge| edge.get("relation").and_then(Value::as_str) == Some("pat"))
        .filter_map(|edge| edge.get("target").and_then(Value::as_str))
        .collect();
    assert!(
        under_pat.len() <= 1,
        "one reference reaches one document: {under_pat:?}"
    );
}

#[test]
fn a_move_repoints_past_a_capture_that_names_no_document() {
    // The successor spelling gives a declared pattern something to match
    // and what it captures names nothing. No edge arrives, so there is
    // none to mint — and giving the repoint up would cost the live edge
    // it was carrying to prevent a reference the next build reports
    // whichever way this goes.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '(shadow)'\nrelation = \"minted\"\n",
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
    let env = run_envelope(nodex(root).args(["rename", "old.md", "shadowy.md"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "the repoint lands: {env:?}"
    );
    assert!(
        fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("[t](shadowy.md)")
    );
}

#[test]
fn a_move_repoints_past_a_capture_of_an_edge_the_document_already_carries() {
    // The graph holds one edge however many times a document spells it,
    // so a capture arriving inside a successor spelling gives it nothing
    // where it already reaches that document under that relation. A tally
    // reads the second spelling as an arrival; the edge it names is not.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '(shadow)'\nrelation = \"minted\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "old.md",
        "---\nid: moved\ntitle: M\nkind: generic\nstatus: active\n---\nm\n",
    );
    write_doc(
        root,
        "s.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\nshadow\n\n[t](old.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "old.md", "shadowy.md"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "the repoint lands: {env:?}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    let edges: Vec<(&str, &str)> = outgoing
        .iter()
        .filter_map(|edge| {
            Some((
                edge.get("relation").and_then(Value::as_str)?,
                edge.get("target").and_then(Value::as_str)?,
            ))
        })
        .collect();
    assert!(
        edges.contains(&("minted", "shadow")) && edges.contains(&("references", "moved")),
        "the edge it already carried, and the one the repoint kept: {edges:?}"
    );
}

#[test]
fn a_move_repoints_where_a_second_reader_of_one_span_is_one_edge() {
    // The successor spelling a pattern reads is the destination's own,
    // read at the same place under the same relation — one edge to the
    // graph, and counting it as a second reference would call every such
    // repoint a mint and give up the edge it was keeping.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '\\b(n[a-z]+\\.md)\\b'\n\
         relation = \"references\"\n",
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
    let env = run_envelope(nodex(root).args(["rename", "old.md", "new.md"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "the repoint lands: {env:?}"
    );
    assert!(
        fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("[t](new.md)")
    );
}

#[test]
fn a_move_says_nothing_about_a_reference_that_names_what_it_named() {
    // A repoint is proposed off the path rungs alone, and the ladder has
    // one below them: a capture the move leaves goes on naming the
    // document by its bare id, having lost nothing. There is nothing to
    // report, and saying it names nothing would be false.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '@ref\\(([a-z]+)\\)'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "x.md",
        "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\nx\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n@ref(x)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "x.md", "y2.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        env.get("warnings").is_none(),
        "the reference kept its document, so nothing happened to it: {env:?}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|edge| edge.get("target").and_then(Value::as_str) == Some("x")),
        "and it still holds the edge: {outgoing:?}"
    );
}

#[test]
fn a_document_arriving_at_the_root_does_not_take_a_reference_that_says_here() {
    // `./sub/x.md` names the directory the document is in, to CommonMark
    // and to everything that follows the link. A document arriving at the
    // root cannot take it, so the move has nothing to repoint and nothing
    // to say — and the reference goes on naming what it always named.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/sub/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "mv/y.md",
        "---\nid: newcomer\ntitle: N\nkind: generic\nstatus: active\n---\nn\n",
    );
    write_doc(
        root,
        "docs/r.md",
        "---\nid: r\ntitle: R\nkind: generic\nstatus: active\n---\n[d](./sub/x.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "mv/y.md", "sub/x.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(env.get("warnings").is_none(), "nothing to say: {env:?}");
    assert!(
        fs::read_to_string(root.join("docs/r.md"))
            .unwrap()
            .contains("[d](./sub/x.md)"),
        "and nothing to write"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "r"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .any(|edge| edge.get("target").and_then(Value::as_str) == Some("desired")),
        "the document its marker points at: {outgoing:?}"
    );
}

#[test]
fn a_move_says_which_of_its_own_references_it_could_not_rebase() {
    // The moved document's own references are answered for by the same
    // rule and reported in the same voice: one it could not rebase names
    // nothing from the new directory, and only the run that gave up on it
    // knows that it did.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '@r\\(([a-z]+)\\)'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/m.md",
        "---\nid: m\ntitle: M\nkind: generic\nstatus: active\n---\n@r(x)\n",
    );
    write_doc(
        root,
        "a/x.md",
        "---\nid: ax\ntitle: X\nkind: generic\nstatus: active\n---\nx\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/m.md", "b/m.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("b/m.md") && w.contains("not rebased")),
        "the move names its own reference it left: {warnings:?}"
    );
}

#[test]
fn a_move_says_one_thing_about_one_reference() {
    // The two answers a move owes are about the same thing — a reference
    // it left — so a reference that came to name somebody else is not
    // also reported as one it merely declined. Read apart, every rebound
    // reference would be named twice, in two voices, one of them wrong.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '@ref\\(([a-z]+)\\)'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n@ref(x)\n",
    );
    write_doc(
        root,
        "a/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "b/x.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/mover.md", "b/mover.md"]));
    let warnings = env.get("warnings").and_then(Value::as_array).expect("warn");
    assert_eq!(
        warnings.len(),
        1,
        "one reference, one thing said: {warnings:?}"
    );
    assert!(
        warning_msg(&warnings[0]).is_some_and(|w| w.contains("desired") && w.contains("shadow")),
        "and it is the one that says what it came to name: {warnings:?}"
    );
}

#[test]
fn a_move_writes_no_repoint_that_gives_the_document_an_edge_of_its_own() {
    // What a rewrite writes is text, and text is read by every reader the
    // project declares: a successor spelling can satisfy a pattern that
    // matched nothing before. The edge that arrives was written by nobody,
    // resolves cleanly, and so gives no reader downstream a reason to
    // doubt it — the twin of the lost-reference sweep, and the one the
    // rewrite has no other chance to answer for.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '@r\\((.+)\\)'\n\
         relation = \"primary\"\n\
         [[parser.link_patterns]]\npattern = '(shadow)'\n\
         relation = \"secondary\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "nodes/old.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "nodes/shadow-doc.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n@r(nodes/old.md)\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "nodes/old.md", "nodes/new-shadow.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("ref.md"))
            .unwrap()
            .contains("@r(nodes/old.md)"),
        "the spelling that would have minted an edge is not written"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "ref"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("relation").and_then(Value::as_str) != Some("secondary")),
        "no edge the document never held: {outgoing:?}"
    );
}

#[test]
fn a_move_gives_up_only_the_repoint_that_would_mint_an_edge() {
    // The moved document's own reference to itself can only be spelled
    // with the new name in it, whichever frame renders it, and the new
    // name is what the pattern matches — so that repoint mints and is
    // given up. Its neighbour rebases to a name the pattern does not
    // read, and giving up the document would cost it the edge it kept.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[parser.link_patterns]]\npattern = '(mx)'\nrelation = \"minted\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/m.md",
        "---\nid: m\ntitle: M\nkind: generic\nstatus: active\n---\n\
         [self](m.md) and [t](x.md)\n",
    );
    write_doc(
        root,
        "a/x.md",
        "---\nid: ax\ntitle: X\nkind: generic\nstatus: active\n---\nx\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/m.md", "b/mx.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let moved = fs::read_to_string(root.join("b/mx.md")).unwrap();
    assert!(
        moved.contains("[t](../a/x.md)"),
        "the rebase that mints nothing lands: {moved}"
    );
    assert!(
        moved.contains("[self](m.md)"),
        "the one that would mint is the only one given up: {moved}"
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["query", "node", "m"]));
    let outgoing = env
        .pointer("/data/outgoing")
        .and_then(Value::as_array)
        .expect("outgoing");
    assert!(
        outgoing
            .iter()
            .all(|edge| edge.get("relation").and_then(Value::as_str) != Some("minted")),
        "and no edge the document never held: {outgoing:?}"
    );
}

#[test]
fn a_move_says_nothing_about_a_reference_another_reader_rewrote_for_it() {
    // One span, two readers of it under one relation. The wikilink's
    // repoint lands and covers the pattern's span, so the pattern's
    // reference is not one the move left — reporting it would claim the
    // move repointed a reference the document plainly no longer holds,
    // over a rewrite that named exactly what it named before.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = '\\[\\[([a-z./]+)\\]\\]'\n\
         relation = \"references\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "a/mover.md",
        "---\nid: mover\ntitle: M\nkind: generic\nstatus: active\n---\n[[x]]\n",
    );
    write_doc(
        root,
        "a/x.md",
        "---\nid: desired\ntitle: D\nkind: generic\nstatus: active\n---\nd\n",
    );
    write_doc(
        root,
        "b/x.md",
        "---\nid: shadow\ntitle: S\nkind: generic\nstatus: active\n---\ns\n",
    );
    nodex(root).arg("build").assert().success();
    let env = run_envelope(nodex(root).args(["rename", "a/mover.md", "b/mover.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(root.join("b/mover.md"))
            .unwrap()
            .contains("[[../a/x]]"),
        "the repoint landed"
    );
    assert!(
        env.get("warnings").is_none(),
        "the move left nothing to report: {env:?}"
    );
}
