//! `rename`: the move, its destination guards and the identity it anchors.

use crate::support::{
    LOCKED_PROJECT_CONFIG, git_runner, init_project, nodex, run_envelope, run_json, scratch,
    walk_entries, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

/// Reference staging failures precede the file move and leave documents untouched.
#[cfg(unix)]
#[test]
fn a_rename_that_cannot_write_every_reference_writes_none() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [[detection.unresolved_policy]]\nname = \"missing\"\n\
         cause = \"missing\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/target.md",
        "---\nid: t\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    write_doc(
        root,
        "refs/ref.md",
        "---\nid: r\ntitle: R\nkind: generic\nstatus: active\n---\nsee [t](../docs/target.md)\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();

    let refs = root.join("refs");
    let original = fs::metadata(&refs).unwrap().permissions();
    fs::set_permissions(&refs, fs::Permissions::from_mode(0o555)).unwrap();
    let output = nodex(root)
        .args(["rename", "docs/target.md", "docs/moved.md"])
        .output()
        .expect("ran");
    fs::set_permissions(&refs, original).unwrap();

    assert_eq!(output.status.code(), Some(2), "the batch refuses");
    assert!(
        root.join("docs/target.md").exists() && !root.join("docs/moved.md").exists(),
        "nothing moved"
    );
    assert!(
        fs::read_to_string(root.join("refs/ref.md"))
            .unwrap()
            .contains("../docs/target.md"),
        "nothing was rewritten"
    );
    nodex(root).arg("check").assert().success();
}

#[cfg(unix)]
#[test]
fn a_rename_that_cannot_prepare_its_destination_does_not_anchor_the_source() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch();
    let root = tmp.path();
    let source = "---\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    let reference = "---\nid: ref\n---\n[A](a.md)\n";
    write_doc(root, "docs/a.md", source);
    write_doc(root, "docs/ref.md", reference);
    let destination = root.join("docs/destination");
    fs::create_dir(&destination).unwrap();
    let permissions = fs::metadata(&destination).unwrap().permissions();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o555)).unwrap();
    let output = nodex(root)
        .args(["rename", "docs/a.md", "docs/destination/b.md"])
        .output()
        .unwrap();
    fs::set_permissions(&destination, permissions).unwrap();
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "IO_ERROR");
    assert_eq!(fs::read_to_string(root.join("docs/a.md")).unwrap(), source);
    assert_eq!(
        fs::read_to_string(root.join("docs/ref.md")).unwrap(),
        reference
    );
    assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
}

#[test]
fn rename_refuses_a_destination_its_own_naming_rule_would_reject() {
    // Self-consistency on the move seam: a destination filename the
    // project's naming rule rejects would land and then fail the project's
    // own filename_pattern check. Refuse it.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^\\\\d{4}-[a-z0-9-]+\\\\.md$\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/0001-doc.md",
        "---\nid: a\ntitle: A\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["rename", "docs/0001-doc.md", "docs/BADNAME.md"])
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
        msg.contains("filename_pattern") && msg.contains("BADNAME.md"),
        "names the rule that would fire and the filename: {msg}"
    );
    assert!(
        root.join("docs/0001-doc.md").exists(),
        "refused rename keeps the source"
    );
    assert!(
        !root.join("docs/BADNAME.md").exists(),
        "refused rename writes no destination"
    );

    // A conforming destination succeeds.
    nodex(root)
        .args(["rename", "docs/0001-doc.md", "docs/0002-doc.md"])
        .assert()
        .success();
}

#[test]
fn rename_refuses_a_destination_outside_the_project_scope() {
    // Renaming a doc out of scope would silently drop it from the graph
    // and leave every rewritten reference dangling — refused before any
    // mutation (including the id anchor).
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["rename", "docs/a.md", "attic/a.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(root.join("docs/a.md").exists(), "source not moved");
    assert!(!root.join("attic").exists(), "no destination dirs created");
    assert_eq!(
        fs::read_to_string(root.join("docs/a.md")).unwrap(),
        original,
        "refused rename leaves the source byte-identical (no id anchor written)"
    );
}

/// A preview answers with the result the write would report and leaves
/// every file as it was, so an agent can read the plan before committing to it.
#[test]
fn rename_and_retarget_previews_report_the_write_and_make_none() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    write_doc(
        root,
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/c.md",
        "---\nid: doc-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\nrelated: [doc-a]\n---\n\
         # B\nSee [A](a.md).\n",
    );
    let tree = |root: &std::path::Path| {
        let mut files: Vec<(PathBuf, Vec<u8>)> = walk_entries(&root.join("docs"))
            .into_iter()
            .filter(|path| path.is_file())
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        files.sort();
        files
    };

    for (preview, apply) in [
        (
            vec!["rename", "docs/a.md", "docs/moved/a.md", "--dry-run"],
            vec!["rename", "docs/a.md", "docs/moved/a.md"],
        ),
        (
            vec!["retarget", "doc-a", "doc-c", "--dry-run"],
            vec!["retarget", "doc-a", "doc-c"],
        ),
    ] {
        let before = tree(root);
        let planned = run_json(nodex(root).args(&preview));
        assert_eq!(planned["completion"], "planned", "{preview:?}: {planned}");
        assert_eq!(tree(root), before, "{preview:?} wrote nothing");

        let applied = run_json(nodex(root).args(&apply));
        assert_eq!(applied["completion"], "complete", "{apply:?}: {applied}");
        assert_eq!(
            planned["references_updated"], applied["references_updated"],
            "{preview:?} names the files the write rewrote"
        );
        assert_eq!(
            applied["references_updated"],
            serde_json::json!(["docs/b.md"])
        );
        assert_eq!(planned["id_stability"], applied["id_stability"]);
    }
}

#[test]
fn rename_refuses_a_directory_source() {
    // rename moves a single document; a directory source would slide a
    // whole tree past the per-document guarantees (destination gate, id
    // anchoring, reference rewriting) and dangle every reference into
    // it — tracked or not, it is refused loudly.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/sub/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    fs::create_dir_all(root.join("loose")).unwrap();
    nodex(root).arg("build").assert().success();

    for dir in ["docs/sub", "loose"] {
        let output = nodex(root)
            .args(["rename", dir, "elsewhere"])
            .output()
            .expect("ran");
        assert_eq!(output.status.code(), Some(2), "{dir} refused");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert!(
            envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("directory"),
            "names the cause: {envelope}"
        );
        assert!(root.join(dir).exists(), "{dir} untouched");
    }
    assert!(root.join("docs/sub/a.md").exists(), "tree intact");
}

#[test]
fn rename_of_an_untracked_file_is_a_plain_guarded_move() {
    // A source the scan never admitted has no node and no edges —
    // nothing can dangle, so the rename is a plain guarded move: no
    // destination gate, no id anchor, no reference rewriting. The gate
    // fires exactly when it protects something (a tracked source).
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/loose.md"), "# untracked\n").unwrap();
    nodex(root).arg("build").assert().success();

    // Untracked → untracked: plain move succeeds.
    let env = run_envelope(nodex(root).args(["rename", "notes/loose.md", "notes/moved.md"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(0)
    );
    assert!(root.join("notes/moved.md").exists());
    assert!(!root.join("notes/loose.md").exists());

    // Tracked → out-of-scope: still refused (the protected case).
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root)
        .args(["rename", "docs/a.md", "notes/a.md"])
        .assert()
        .failure()
        .code(2);
    assert!(root.join("docs/a.md").exists(), "tracked source untouched");
}

#[test]
fn rename_refuses_to_anchor_into_a_doc_with_non_scalar_kind() {
    // A non-scalar `kind:` means the build cannot parse the document —
    // no node exists, so anchoring a path-inferred id into it would be
    // phantom work; rename refuses loudly instead.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/broken.md",
        "---\nkind:\n  - weird\ntitle: Broken\nstatus: active\n---\n# B\n",
    );
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["rename", "docs/broken.md", "docs/moved.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("kind"),
        "names the broken field: {envelope}"
    );
    assert!(root.join("docs/broken.md").exists(), "source untouched");
}

#[test]
fn rename_anchors_the_id_of_the_effective_frontmatter_kind() {
    // The build derives a doc's id from its frontmatter `kind:` when
    // declared (path inference is only the fallback). The rename anchor
    // must pin exactly that id — anchoring the path-inferred kind's id
    // would write a wrong id and dangle every cross-reference.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\", \"guide\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/noid.md",
        "---\nkind: guide\ntitle: NoId\nstatus: active\n---\n# N\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["rename", "docs/noid.md", "docs/renamed.md"]));
    assert_eq!(
        env.pointer("/data/id_stability/id").and_then(Value::as_str),
        Some("guide-noid"),
        "anchors the build's effective id, not the path-inferred one: {env}"
    );
    assert!(
        fs::read_to_string(root.join("docs/renamed.md"))
            .unwrap()
            .contains("id: \"guide-noid\"")
    );
    nodex(root).arg("build").assert().success();
    let data = run_json(nodex(root).args(["query", "node", "--path", "docs/renamed.md"]));
    assert_eq!(
        data.pointer("/node/id").and_then(Value::as_str),
        Some("guide-noid")
    );
}

/// The destination's parent does not exist when the gate runs — `rename`
/// creates it afterwards — so resolving the moved link through the filesystem
/// cannot fold `..` and every relative target reads as unreachable. A move to a
/// new directory at the same depth resolves to exactly the same document.
#[test]
#[cfg(unix)]
fn a_symlink_moved_to_a_new_directory_at_the_same_depth_still_resolves() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    fs::create_dir_all(project.join("store")).unwrap();
    fs::create_dir_all(project.join("docs/x")).unwrap();
    fs::write(
        project.join("store/alpha.md"),
        "---\nid: generic-alpha\ntitle: Alpha\nkind: generic\nstatus: archived\n---\n# Alpha\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../../store/alpha.md", project.join("docs/x/a.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a linked document"]);

    // `docs/y` does not exist yet; rename creates it after the gate.
    nodex(project)
        .args(["rename", "docs/x/a.md", "docs/y/a.md"])
        .assert()
        .success();
    let data = run_json(nodex(project).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "the link still reaches the same document from its new directory: {data}"
    );
}

/// Folding the *spelled* destination is not enough: an existing destination
/// directory can itself be a symlink, and then the moved link's `..` climbs
/// from somewhere else entirely. The existing part of the destination has to be
/// canonicalised before anything is folded, or the gate approves a move that
/// leaves the document — and its frozen record — unreachable.
#[test]
#[cfg(unix)]
fn rename_resolves_a_symlinked_destination_directory_before_folding() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    for dir in ["store", "store2", "docs/x"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(
        project.join("store/alpha.md"),
        "---\nid: generic-alpha\ntitle: Alpha\nkind: generic\nstatus: archived\n---\n\
         # Alpha\n\nFrozen.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../../store/alpha.md", project.join("docs/x/a.md")).unwrap();
    // Spelled, `docs/y/../../store/alpha.md` folds to a file that exists.
    // Really, `docs/y` is `store2`, so the link climbs out of the project.
    std::os::unix::fs::symlink("../store2", project.join("docs/y")).unwrap();
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "a linked doc and a symlinked directory",
    ]);

    let output = nodex(project)
        .args(["rename", "docs/x/a.md", "docs/y/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the move leaves the document unreachable, so it is refused: {envelope}"
    );
    let data = run_json(nodex(project).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "and the frozen record still stands: {data}"
    );
}

/// A name is taken by whatever entry stands there, resolving or not.
///
/// The destination guard asked `exists`, which follows the link and answers
/// for its target — so a symlink pointing at nothing read as a free name, and
/// the raw `fs::rename` below replaced the link without a word. The source
/// side of the same guard has always used `symlink_metadata`.
#[test]
#[cfg(unix)]
fn rename_does_not_take_a_name_a_dangling_link_already_holds() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/old.md",
        "---\nid: old\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    std::os::unix::fs::symlink("../nowhere/target.md", root.join("docs/taken.md")).unwrap();

    let output = nodex(root)
        .args(["rename", "docs/old.md", "docs/taken.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("ALREADY_EXISTS"),
        "{env}"
    );
    assert_eq!(
        fs::read_link(root.join("docs/taken.md")).unwrap(),
        std::path::Path::new("../nowhere/target.md"),
        "the link is still the entry standing there"
    );
    assert!(root.join("docs/old.md").exists(), "and nothing moved");

    // A free name is still free.
    nodex(root)
        .args(["rename", "docs/old.md", "docs/new.md"])
        .assert()
        .success();
}

/// A `..` inside the link's own target is resolved by the kernel against what
/// precedes it, so one that traverses a dangling symlink finds nothing — and
/// neither may the gate. Falling back to the spelling there names whatever file
/// happens to sit at the folded path, which is how a byte-identical decoy gets
/// a frozen record replaced with the gate reporting success.
#[test]
#[cfg(unix)]
fn rename_does_not_fall_back_to_spelling_through_a_dangling_link() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nexclude = [\"docs/**/store/**\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    for dir in ["store", "docs/x", "docs/deep/store", "other"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    let frozen = "---\nid: alpha\ntitle: Alpha\nkind: generic\nstatus: archived\n---\n# Alpha\n";
    fs::write(project.join("store/alpha.md"), frozen).unwrap();
    // Byte-identical decoy sitting exactly where the spelling would land.
    fs::write(project.join("docs/deep/store/alpha.md"), frozen).unwrap();
    std::os::unix::fs::symlink("../other", project.join("docs/sub")).unwrap();
    // Dangling: nothing at ../../elsewhere/nested, so the kernel cannot
    // traverse `docs/deep/sub/..` at all.
    std::os::unix::fs::symlink("../../elsewhere/nested", project.join("docs/deep/sub")).unwrap();
    std::os::unix::fs::symlink("../sub/../store/alpha.md", project.join("docs/x/a.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a frozen record behind two links"]);

    let output = nodex(project)
        .args(["rename", "docs/x/a.md", "docs/deep/y/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the moved link reaches nothing, so the move is refused: {envelope}"
    );
    nodex(project).arg("build").assert().success();
    let nodes = run_json(nodex(project).args(["query", "nodes"]));
    let ids: Vec<&str> = nodes
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ids,
        ["alpha"],
        "and the frozen record still stands: {nodes}"
    );
}

/// `x/..` is `ENOTDIR` when `x` is a regular file, but `canonicalize` answers
/// happily and popping steps into that file's parent — so a target whose `..`
/// crosses a file resolves for the gate and not for the kernel, landing on a
/// real unrelated document. With a byte-identical one there, no rule fires
/// either and the frozen record is replaced in silence.
#[test]
#[cfg(unix)]
fn rename_refuses_a_target_stepping_out_of_a_regular_file() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nexclude = [\"docs/**/store/**\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    let frozen = "---\nid: rec\ntitle: Rec\nkind: generic\nstatus: archived\n---\n# Rec\n";
    for dir in ["docs/a/q", "docs/a/store", "docs/b/store"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(project.join("docs/a/store/rec.md"), frozen).unwrap();
    // Byte-identical, exactly where popping out of the file would land.
    fs::write(project.join("docs/b/store/rec.md"), frozen).unwrap();
    // `q` is a directory beside the source and a regular file beside the
    // destination, so the same target string is traversable from one and
    // ENOTDIR from the other.
    fs::write(project.join("docs/b/q"), "not a directory\n").unwrap();
    std::os::unix::fs::symlink("q/../store/rec.md", project.join("docs/a/doc.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a frozen record behind a q/.. hop"]);

    let output = nodex(project)
        .args(["rename", "docs/a/doc.md", "docs/b/doc.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the kernel cannot step out of a file, so neither may the gate: {envelope}"
    );
    nodex(project).arg("build").assert().success();
    let nodes = run_json(nodex(project).args(["query", "nodes"]));
    let paths: Vec<&str> = nodes
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("path").and_then(Value::as_str))
        .collect();
    assert_eq!(
        paths,
        ["docs/a/doc.md"],
        "and the frozen record still stands where it was: {nodes}"
    );
}

/// A target may leave the chain of directories the move is about to create and
/// re-enter it by name. Those segments are still to-be-created, so `..` over
/// them is the spelling — tracking membership by a countdown of `..` seen
/// forgets that and refuses a move the kernel performs happily.
#[test]
#[cfg(unix)]
fn rename_allows_a_target_that_re_enters_a_created_segment() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\nexclude = [\"**/store/**\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    for dir in ["other/x", "other/y", "other/store", "docs/store"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    // The same document either side of the move: `../y/../store/rec.md` means
    // `other/store/rec.md` from `other/x`, and `docs/store/rec.md` from the
    // `docs/y` the move creates. Out of scope itself, so the two are not a
    // duplicate id — only what the link reaches.
    let rec = "---\nid: generic-rec\ntitle: Rec\nkind: generic\nstatus: archived\n---\n# Rec\n";
    fs::write(project.join("other/store/rec.md"), rec).unwrap();
    fs::write(project.join("docs/store/rec.md"), rec).unwrap();
    std::os::unix::fs::symlink("../y/../store/rec.md", project.join("other/x/doc.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a target that leaves and re-enters"]);

    nodex(project)
        .args(["rename", "other/x/doc.md", "docs/y/doc.md"])
        .assert()
        .success();
    let data = run_json(nodex(project).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "the link reaches the same document from the created directory: {data}"
    );
}

/// A relative target can land back on the source link, which the move is about
/// to take away. It reads fine right now — that is the trap — so the gate must
/// judge it as the post-move world will: gone.
#[test]
#[cfg(unix)]
fn rename_refuses_a_target_that_lands_on_the_source_itself() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"x/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    for dir in ["x/y", "x/z/w", "y"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(
        project.join("y/link.md"),
        "---\nid: rec\ntitle: R\nkind: generic\nstatus: archived\n---\n# R\n\nFrozen.\n",
    )
    .unwrap();
    // From `x/y` this is `y/link.md`; from `x/z/w` it is `x/y/link.md` — the
    // source, which will not be there once the move lands.
    std::os::unix::fs::symlink("../../y/link.md", project.join("x/y/link.md")).unwrap();
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "a link that would point at its own old home",
    ]);

    let output = nodex(project)
        .args(["rename", "x/y/link.md", "x/z/w/link.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the moved link would point at the path the move emptied: {envelope}"
    );
    nodex(project).arg("build").assert().success();
    let nodes = run_json(nodex(project).args(["query", "nodes"]));
    assert_eq!(
        nodes.get("total").and_then(Value::as_u64),
        Some(1),
        "and the frozen record still stands: {nodes}"
    );
}

/// Landing *on* the source is only the simplest way to depend on it. The
/// target may reach a link that reaches another that reaches the source, and
/// every hop in that chain stops working when the source moves — so the whole
/// chain is followed, not just the path the walk computed.
#[test]
#[cfg(unix)]
fn rename_refuses_a_target_reaching_the_source_through_another_link() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"x/**/*.md\"]\nexclude = [\"x/y/b.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    for dir in ["x/y", "x/z/w", "y"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(
        project.join("y/b.md"),
        "---\nid: rec\ntitle: R\nkind: generic\nstatus: archived\n---\n# R\n\nFrozen.\n",
    )
    .unwrap();
    // From `x/y` the target is `y/b.md`, the real document. From the
    // destination `x/z/w` the same spelling is `x/y/b.md` — a link that
    // reaches the document only by way of `a.md`, the one being moved.
    std::os::unix::fs::symlink("../../y/b.md", project.join("x/y/a.md")).unwrap();
    std::os::unix::fs::symlink("a.md", project.join("x/y/b.md")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a chain that runs through the source"]);

    let output = nodex(project)
        .args(["rename", "x/y/a.md", "x/z/w/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "the chain runs through the path the move empties: {envelope}"
    );
    nodex(project).arg("build").assert().success();
    let nodes = run_json(nodex(project).args(["query", "nodes"]));
    assert_eq!(
        nodes.get("total").and_then(Value::as_u64),
        Some(1),
        "and the frozen record still stands: {nodes}"
    );
}

/// `rename` moves a file symlink as the link itself, so the destination holds
/// what that link resolves to *from there*. A relative target that changes
/// directory depth resolves somewhere else — here, nowhere — so judging the
/// source's bytes would let every pre-move gate approve a move that destroys
/// the record.
#[test]
#[cfg(unix)]
fn rename_judges_where_a_moved_symlink_will_point() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    fs::create_dir_all(project.join("store")).unwrap();
    fs::create_dir_all(project.join("docs")).unwrap();
    fs::write(
        project.join("store/a.md"),
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nFrozen decision.\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../store/a.md", project.join("docs/a.md")).unwrap();
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "the document is reached through a link",
    ]);

    let output = nodex(project)
        .args(["rename", "docs/a.md", "docs/sub/a.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.get("ok").and_then(Value::as_bool),
        Some(false),
        "a move that leaves the document unreachable is refused: {envelope}"
    );
    let data = run_json(nodex(project).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "and the record still stands: {data}"
    );
}

#[test]
fn rename_refuses_an_unbuildable_project_with_no_baseline_configured() {
    let tmp = scratch();
    let project = tmp.path();
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: dup\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: dup\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );

    let output = nodex(project)
        .args(["rename", "docs/a.md", "docs/moved.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("DUPLICATE_ID"),
        "the move is refused for the same reason `build` is: {envelope}"
    );
    assert!(
        project.join("docs/a.md").exists() && !project.join("docs/moved.md").exists(),
        "and nothing moved"
    );
}

#[test]
fn rename_refuses_a_project_that_cannot_build_before_it_moves_anything() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n\
         # A\n\nsee [b](b.md)\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a project that builds"]);

    // The duplicate exists only in the working tree, so the baseline still
    // builds and only the overlay build would fail.
    write_doc(
        project,
        "docs/dup.md",
        "---\nid: generic-b\ntitle: Dup\nkind: generic\nstatus: active\n---\n# Dup\n",
    );

    let output = nodex(project)
        .args(["rename", "docs/b.md", "docs/moved.md"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("DUPLICATE_ID"),
        "the refusal names the project's own problem: {envelope}"
    );
    assert!(
        !project.join("docs/moved.md").exists(),
        "nothing moved, so the reference is still correct and a re-run can succeed"
    );
    assert!(
        fs::read_to_string(project.join("docs/a.md"))
            .unwrap()
            .contains("(b.md)"),
        "the reference was left intact rather than stranded"
    );
}

#[test]
fn rename_of_a_terminal_parent_is_not_vetoed_by_its_own_pre_move_presence() {
    // The destination probe models the POST-move world: the source path
    // is overlaid empty, so the still-on-disk source cannot act as its
    // own terminal conditional-exclude parent and veto a move the build
    // would admit once the source is gone.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"specs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"specs/*/spec.md\"\n\
         child_glob = \"specs/*/*.md\"\n\
         [statuses]\nallowed = [\"active\", \"done\"]\nterminal = [\"done\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "specs/auth/spec.md",
        "---\nid: spec-auth\ntitle: Auth\nkind: generic\nstatus: done\n---\n# Auth\n",
    );
    nodex(root).arg("build").assert().success();

    // Renaming the terminal parent itself: post-move no terminal parent
    // remains in the directory, so the destination IS graphed.
    nodex(root)
        .args([
            "rename",
            "specs/auth/spec.md",
            "specs/auth/spec-archived.md",
        ])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    let data =
        run_json(nodex(root).args(["query", "node", "--path", "specs/auth/spec-archived.md"]));
    assert_eq!(
        data.pointer("/node/id").and_then(Value::as_str),
        Some("spec-auth"),
        "moved doc is graphed: {data}"
    );

    // Inverse protection still holds: moving a doc INTO a directory
    // governed by a different, still-present terminal parent is refused
    // — post-move it would be conditionally excluded — and the message
    // names the actual cause.
    write_doc(
        root,
        "specs/billing/spec.md",
        "---\nid: spec-billing\ntitle: Billing\nkind: generic\nstatus: done\n---\n# B\n",
    );
    write_doc(
        root,
        "specs/other/notes.md",
        "---\nid: generic-notes\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n",
    );
    nodex(root).arg("build").assert().success();
    let output = nodex(root)
        .args(["rename", "specs/other/notes.md", "specs/billing/notes.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("conditional_exclude"),
        "names the actual cause: {envelope}"
    );
    assert!(
        root.join("specs/other/notes.md").exists(),
        "source untouched"
    );
}

#[test]
fn rename_cross_dir_moved_file_with_self_and_outbound_links() {
    // The moved file carries BOTH a self-reference (a link to its own
    // old path) and an outbound relative link to another file. A
    // cross-directory move must, in one read+write, repoint the
    // self-reference (pass 1) AND rebase the outbound link to the new
    // vantage point (pass 2) — exercising the `.or(pass1)` compose.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a/b/x.md",
        "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n\
         self [me](x.md) and out [auth](../../t/auth.md)\n",
    );
    write_doc(
        tmp.path(),
        "t/auth.md",
        "---\nid: auth\ntitle: Auth\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a/b/x.md", "a/x.md"])
        .assert()
        .success();

    let moved = fs::read_to_string(tmp.path().join("a/x.md")).unwrap();
    assert!(
        moved.contains("[me](x.md)"),
        "self-reference stays the same stem after the move: {moved}"
    );
    assert!(
        moved.contains("[auth](../t/auth.md)"),
        "outbound relative link rebased to the new directory: {moved}"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let issues = run_json(nodex(tmp.path()).args(["query", "issues"]));
    assert_eq!(
        issues["unresolved_edges"].as_array().map(Vec::len),
        Some(0),
        "both rewritten links must resolve in the rebuilt graph: {issues}"
    );
}

#[test]
fn rename_rewrites_custom_pattern_inbound_reference() {
    // A `[[parser.link_patterns]]` reference in another file must be
    // repointed by rename, exactly like a markdown link — the rewriter
    // shares the same candidate ladder.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [[parser.link_patterns]]\npattern = '@import\\s+(\\S+)'\nrelation = \"imports\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n@import a.md here\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "a.md", "c.md"]));
    assert!(
        data["references_updated"]
            .as_array()
            .map(|a| a.iter().any(|p| p == "b.md"))
            .unwrap_or(false),
        "custom-pattern inbound reference must be repointed: {data}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("b.md"))
            .unwrap()
            .contains("@import c.md here"),
        "the custom-pattern target is rewritten to the new path"
    );
}

#[test]
fn rename_terminal_parent_under_conditional_exclude_resolves_against_real_pre_move_scope() {
    // `conditional_exclude` makes scope location-dependent: a terminal
    // parent excludes its directory siblings. Renaming that parent
    // changes which files are in scope. The inbound rewrite must
    // resolve against the *real* pre-move scope (scanned before the
    // move), not a set fabricated from the post-move scan — otherwise a
    // sibling that only re-enters scope after the move could corrupt
    // resolution.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"docs/feat/SPEC.md\"\nchild_glob = \"**/*\"\ncondition = \"status_terminal\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         terminal = [\"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    // Terminal parent → its sibling `notes.md` is excluded from scope.
    write_doc(
        tmp.path(),
        "docs/feat/SPEC.md",
        "---\nid: spec\ntitle: Spec\nkind: generic\nstatus: superseded\nsuperseded_by: ext\n---\n# Spec\n",
    );
    write_doc(
        tmp.path(),
        "docs/feat/notes.md",
        "scratch notes, out of scope\n",
    );
    // An in-scope file links the terminal parent.
    write_doc(
        tmp.path(),
        "ext.md",
        "---\nid: ext\ntitle: Ext\nkind: generic\nstatus: active\n---\n# Ext\n[s](docs/feat/SPEC.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "docs/feat/SPEC.md", "docs/archived-spec.md"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(tmp.path().join("ext.md"))
            .unwrap()
            .contains("[s](docs/archived-spec.md)"),
        "inbound link to the moved terminal parent must be repointed"
    );
}

#[test]
fn rename_rewrites_titled_and_pointy_markdown_links() {
    // The rewriter extracts markdown destinations via the same
    // pulldown parser as the builder, so titled (`(url "t")`) and
    // pointy (`(<url>)`) inline-link forms — which a regex matcher
    // misses — are repointed, preserving the title and brackets.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n\
         [t](a.md \"the title\") and [p](<a.md>) and [plain](a.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a.md", "c.md"])
        .assert()
        .success();
    let b = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(b.contains("[t](c.md \"the title\")"), "titled form: {b}");
    assert!(b.contains("[p](<c.md>)"), "pointy form: {b}");
    assert!(b.contains("[plain](c.md)"), "plain form: {b}");
}

#[test]
fn rename_repoints_reference_style_link_definitions() {
    // Reference / collapsed / shortcut markdown links carry their URL
    // in a `[label]: url` definition line. The builder binds an edge
    // for each use; the rewriter repoints the single definition so all
    // uses stay resolved. A move must leave no dangling edge.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n\
         See [full][r] and [coll][] and [short].\n\n\
         [r]: old.md\n[coll]: old.md\n[short]: old.md\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "old.md", "new.md"]));
    assert!(
        data["references_updated"]
            .as_array()
            .map(|a| a.iter().any(|p| p == "b.md"))
            .unwrap_or(false),
        "reference-style definitions must be repointed: {data}"
    );
    let b = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(
        !b.contains("old.md"),
        "no definition may still point at old: {b}"
    );
    assert_eq!(
        b.matches("new.md").count(),
        3,
        "all three definitions repointed: {b}"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let issues = run_json(nodex(tmp.path()).args(["query", "issues"]));
    assert_eq!(
        issues["unresolved_edges"].as_array().map(Vec::len),
        Some(0),
        "no edge may dangle after the move: {issues}"
    );
}

#[test]
fn rename_leaves_extensionless_markdown_link_untouched() {
    // A standard markdown link needs a configured extension to be a
    // graph edge (the builder's process_link_target guard). `[x](old)`
    // is not an edge, so rename must leave it byte-unchanged — only the
    // extension-bearing `[y](old.md)` is repointed.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n\
         bare [x](old) and full [y](old.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "old.md", "new.md"])
        .assert()
        .success();
    let b = fs::read_to_string(tmp.path().join("b.md")).unwrap();
    assert!(
        b.contains("[x](old)"),
        "extensionless link must be untouched: {b}"
    );
    assert!(
        b.contains("[y](new.md)"),
        "extension-bearing link repointed: {b}"
    );
}

#[test]
fn rename_repoints_reference_link_with_case_divergent_label() {
    // CommonMark matches reference labels case-insensitively, so
    // `[x][REF]` binds `[ref]: old.md` as a build edge. Rename must
    // repoint that definition despite the casing mismatch — otherwise
    // the edge silently dangles.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "old.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\nSee [x][REF].\n\n[ref]: old.md\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let pre = run_json(nodex(tmp.path()).args(["query", "backlinks", "a"]));
    assert_eq!(
        pre["total"].as_u64(),
        Some(1),
        "case-divergent ref is an edge: {pre}"
    );

    let data = run_json(nodex(tmp.path()).args(["rename", "old.md", "new.md"]));
    assert!(
        data["references_updated"]
            .as_array()
            .map(|a| a.iter().any(|p| p == "b.md"))
            .unwrap_or(false),
        "case-divergent reference definition must be repointed: {data}"
    );
    assert!(
        fs::read_to_string(tmp.path().join("b.md"))
            .unwrap()
            .contains("[ref]: new.md"),
        "definition repointed"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let issues = run_json(nodex(tmp.path()).args(["query", "issues"]));
    assert_eq!(
        issues["unresolved_edges"].as_array().map(Vec::len),
        Some(0),
        "no edge may dangle after the rename: {issues}"
    );
}

#[test]
fn rename_anchors_id_in_crlf_frontmatter_document() {
    // A CRLF-delimited frontmatter document must be canonicalised the
    // same way the build parses it — otherwise rename mis-reads it as
    // bare and skips id anchoring. With an explicit `id:` the move is
    // already anchored (not a bare-file warning).
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("docs").join("crlf.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "---\r\nid: doc\r\ntitle: D\r\nkind: generic\r\nstatus: active\r\n---\r\n# D\r\n",
    )
    .unwrap();
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "docs/crlf.md", "docs/renamed.md"]));
    assert_eq!(
        data.pointer("/id_stability/type").and_then(Value::as_str),
        Some("already_anchored"),
        "CRLF frontmatter must be seen, not mis-read as bare: {data}"
    );
}

#[test]
fn rename_repoints_link_in_file_evicted_from_scope_by_the_move() {
    // `conditional_exclude` makes scope move-dependent: moving a file
    // can turn a sibling directory into an excluded one. A file that
    // drops out of post-move scope still holds a real pre-move edge to
    // the renamed file — the rewrite must visit it (the iteration set
    // is the union of pre- and post-move scope) and repoint its link.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"work/SPEC.md\"\nchild_glob = \"**/*\"\ncondition = \"status_terminal\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         terminal = [\"superseded\", \"archived\", \"deprecated\", \"abandoned\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "staging/SPEC.md",
        "---\nid: spec\ntitle: S\nkind: generic\nstatus: superseded\nsuperseded_by: peer\n---\n# S\n",
    );
    write_doc(
        tmp.path(),
        "work/peer.md",
        "---\nid: peer\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n[s](../staging/SPEC.md)\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "staging/SPEC.md", "work/SPEC.md"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(tmp.path().join("work/peer.md"))
            .unwrap()
            .contains("[s](SPEC.md)"),
        "the evicted file's pre-move edge must be repointed, not left dangling"
    );
}

#[test]
fn rename_does_not_repoint_link_bound_to_a_different_file() {
    // A root namesake never changes the meaning of a source-relative link.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "shared.md",
        "---\nid: shared-root\ntitle: Root\nkind: generic\nstatus: active\n---\n# Root\n",
    );
    write_doc(
        tmp.path(),
        "docs/sub/shared.md",
        "---\nid: shared-sub\ntitle: Sub\nkind: generic\nstatus: active\n---\n# Sub\n",
    );
    let s_content =
        "---\nid: s\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n[x](shared.md)\n";
    write_doc(tmp.path(), "docs/sub/s.md", s_content);
    nodex(tmp.path()).arg("build").assert().success();

    nodex(tmp.path())
        .args(["rename", "shared.md", "renamed.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("docs/sub/s.md")).unwrap(),
        s_content,
        "link bound to the local file must survive byte-identical"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let backlinks = run_json(nodex(tmp.path()).args(["query", "backlinks", "shared-sub"]));
    assert_eq!(
        backlinks["total"].as_u64(),
        Some(1),
        "the local file's incoming edge must survive the rename: {backlinks}"
    );
}

#[test]
fn rename_does_not_repoint_wikilink_bound_to_a_bare_sibling() {
    // Extension-append resolver disagreement: `[[shared]]` binds the
    // bare extension-less `wiki/shared` (first candidate), not
    // `wiki/shared.md`. Renaming the `.md` file must leave the wikilink
    // pointing at the bare sibling — its backlink must survive.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\", \"**/wiki/*\"]\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [parser]\nwikilink_enabled = true\nextensions = [\".md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "wiki/shared",
        "---\nid: bare-shadow\ntitle: Bare\nkind: generic\nstatus: active\n---\n# bare\n",
    );
    write_doc(
        tmp.path(),
        "wiki/shared.md",
        "---\nid: sub-shared\ntitle: SubShared\nkind: generic\nstatus: active\n---\n# sub\n",
    );
    let ref_content =
        "---\nid: linker\ntitle: L\nkind: generic\nstatus: active\n---\n# L\n[[shared]]\n";
    write_doc(tmp.path(), "wiki/ref.md", ref_content);
    nodex(tmp.path()).arg("build").assert().success();
    // Pre-condition: the wikilink binds the bare file, not the .md.
    let pre = run_json(nodex(tmp.path()).args(["query", "backlinks", "bare-shadow"]));
    assert_eq!(
        pre["total"].as_u64(),
        Some(1),
        "wikilink binds the bare file: {pre}"
    );

    nodex(tmp.path())
        .args(["rename", "wiki/shared.md", "wiki/renamed.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("wiki/ref.md")).unwrap(),
        ref_content,
        "wikilink bound to the bare sibling must survive byte-identical"
    );
    nodex(tmp.path()).arg("build").assert().success();
    let post = run_json(nodex(tmp.path()).args(["query", "backlinks", "bare-shadow"]));
    assert_eq!(
        post["total"].as_u64(),
        Some(1),
        "the bare file's incoming edge must survive the .md rename: {post}"
    );
}

#[test]
fn rename_rewrites_markdown_links_but_not_prose() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n\
         # A\n\
         Prose mention of docs/b.md must survive verbatim.\n\
         But this [link](b.md) and [anchored](b.md#section) must update.\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "docs/b.md", "docs/c.md"])
        .assert()
        .success();
    let content = fs::read_to_string(tmp.path().join("docs/a.md")).unwrap();
    // Prose occurrence must NOT be rewritten.
    assert!(
        content.contains("Prose mention of docs/b.md must survive verbatim."),
        "prose was corrupted: {content}"
    );
    // Both markdown links MUST be rewritten, preserving anchor.
    assert!(content.contains("[link](c.md)"), "link not updated");
    assert!(
        content.contains("[anchored](c.md#section)"),
        "anchored link not updated"
    );
}

#[test]
fn rename_skips_a_broken_referencing_file_without_stranding_the_batch() {
    // One referencing file whose frontmatter fence does not parse is a
    // per-file skip: the move lands, every other reference is
    // rewritten, the command exits 0 with a warning naming the broken
    // file. The file already reds `check` as a parse_failure, and its
    // stale reference surfaces as an unresolved edge — a batch abort
    // here would strand a half-applied rename (the file is already
    // moved when references are rewritten).
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/target.md",
        "---\nid: target\ntitle: Target\nkind: generic\nstatus: active\n---\n# Target\n",
    );
    write_doc(
        tmp.path(),
        "docs/healthy.md",
        "---\nid: healthy\ntitle: Healthy\nkind: generic\nstatus: active\n---\n\
         # Healthy\n\nSee [target](target.md).\n",
    );
    // Opened fence, never closed — unsplittable.
    write_doc(
        tmp.path(),
        "docs/broken.md",
        "---\nid: broken\ntitle: Broken\nSee [target](target.md).\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let envelope =
        run_envelope(nodex(tmp.path()).args(["rename", "docs/target.md", "docs/moved.md"]));
    assert!(
        !tmp.path().join("docs/target.md").exists() && tmp.path().join("docs/moved.md").exists(),
        "the move itself lands"
    );
    let healthy = fs::read_to_string(tmp.path().join("docs/healthy.md")).unwrap();
    assert!(
        healthy.contains("moved.md"),
        "the parseable referencing file is rewritten: {healthy}"
    );
    let broken = fs::read_to_string(tmp.path().join("docs/broken.md")).unwrap();
    assert!(
        broken.contains("target.md"),
        "the broken file is left untouched: {broken}"
    );
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("skip warning present");
    assert!(
        warnings.iter().any(|w| w
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("docs/broken.md") && s.contains("parse_failure"))),
        "the warning names the skipped file: {warnings:?}"
    );
}

#[test]
fn rename_rewrites_sibling_and_parent_relative_links() {
    let tmp = scratch();
    init_project(tmp.path());
    // Link written **file-relative** from sibling inside same dir.
    write_doc(
        tmp.path(),
        "docs/sibling.md",
        "---\nid: s\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n[x](first.md)\n",
    );
    // A link crosses from notes into docs through their common parent.
    write_doc(
        tmp.path(),
        "notes/n.md",
        "---\nid: n\ntitle: N\nkind: generic\nstatus: active\n---\n# N\n[y](../docs/first.md)\n",
    );
    write_doc(
        tmp.path(),
        "docs/first.md",
        "---\nid: f\ntitle: F\nkind: generic\nstatus: active\n---\n# F\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "docs/first.md", "docs/new.md"])
        .assert()
        .success();
    let sibling = fs::read_to_string(tmp.path().join("docs/sibling.md")).unwrap();
    assert!(
        sibling.contains("[x](new.md)"),
        "file-relative link should stay file-relative: {sibling}"
    );
    let note = fs::read_to_string(tmp.path().join("notes/n.md")).unwrap();
    assert!(
        note.contains("[y](../docs/new.md)"),
        "root-relative link should stay root-relative: {note}"
    );
}

#[test]
fn rename_rebases_moved_file_relative_references() {
    // Cross-directory move: the moved file's OWN file-relative links
    // were written from the old directory's vantage point and must be
    // recomputed from the new one — otherwise they silently dangle.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a/b/x.md",
        "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n\
         [auth](../../t/auth.md)\n",
    );
    write_doc(
        tmp.path(),
        "t/auth.md",
        "---\nid: auth\ntitle: Auth\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "a/b/x.md", "a/x.md"]));

    let moved = fs::read_to_string(tmp.path().join("a/x.md")).unwrap();
    assert!(
        moved.contains("[auth](../t/auth.md)"),
        "outbound relative link must be rebased to the new directory: {moved}"
    );
    // The moved file reports itself among the updated references.
    let updated: Vec<&str> = data
        .get("references_updated")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        updated.contains(&"a/x.md"),
        "moved file must be listed exactly once: {updated:?}"
    );
    assert_eq!(
        updated.iter().filter(|p| **p == "a/x.md").count(),
        1,
        "moved file must not be double-counted: {updated:?}"
    );
    // The rebased link binds in the rebuilt graph (no unresolved edge).
    nodex(tmp.path()).arg("build").assert().success();
    let issues = run_json(nodex(tmp.path()).args(["query", "issues"]));
    let unresolved = issues
        .pointer("/unresolved_edges/total")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    assert_eq!(unresolved, 0, "rebased link must resolve: {issues}");
}

#[test]
fn rename_preserves_literal_root_relative_reference_in_moved_file() {
    // A root-relative link in the moved file is move-invariant — the
    // file must come through the cross-directory move byte-identical.
    let tmp = scratch();
    init_project(tmp.path());
    let original = "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n\
                    [auth](t/auth.md)\n";
    write_doc(tmp.path(), "a/x.md", original);
    write_doc(
        tmp.path(),
        "t/auth.md",
        "---\nid: auth\ntitle: Auth\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a/x.md", "b/x.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("b/x.md")).unwrap(),
        original,
        "root-relative reference must survive a cross-directory move untouched"
    );
}

#[test]
fn rename_within_directory_leaves_moved_file_references_untouched() {
    // Same-directory rename: no vantage point moved — the moved file's
    // own outbound references must stay byte-identical.
    let tmp = scratch();
    init_project(tmp.path());
    let original = "---\nid: x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n\
                    [auth](../t/auth.md)\n";
    write_doc(tmp.path(), "a/x.md", original);
    write_doc(
        tmp.path(),
        "t/auth.md",
        "---\nid: auth\ntitle: Auth\nkind: generic\nstatus: active\n---\n# Auth\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a/x.md", "a/y.md"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(tmp.path().join("a/y.md")).unwrap(),
        original,
        "within-directory rename must not rewrite the moved file's references"
    );
}

/// Project that derives every id from the file stem
/// (`{kind}-{stem}`), so renaming a file mechanically changes the
/// inferred id. This is the exact configuration where the rename
/// command has to anchor an explicit `id:` to keep cross-document
/// references valid — and the test fixture used by the suite below
/// to lock that contract in.
fn init_path_derived_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
}

#[test]
fn rename_explicit_id_doc_does_not_touch_frontmatter() {
    // When `id:` is pinned explicitly, the rename is path-only by
    // construction. The doc's frontmatter must NOT be rewritten — a
    // minimal-diff invariant the `id_stability` envelope locks in.
    let tmp = scratch();
    init_path_derived_project(tmp.path());
    let original = "---\nid: explicit-anchor\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["rename", "docs/a.md", "docs/renamed.md"]));
    assert_eq!(
        data.pointer("/id_stability/type").and_then(Value::as_str),
        Some("already_anchored")
    );
    let moved = fs::read_to_string(tmp.path().join("docs/renamed.md")).unwrap();
    assert_eq!(
        moved, original,
        "explicit-id doc must move byte-for-byte unchanged"
    );
}

#[test]
fn rename_path_derived_id_anchors_into_frontmatter() {
    // The blocker scenario: id is derived from stem, the rename
    // changes the stem, and another doc references the old id via
    // frontmatter. Without anchoring, the reference would dangle on
    // the next build. Lock in: after rename, the moved doc carries
    // the old id explicitly, and the referencing doc's `related`
    // edge still resolves end-to-end.
    let tmp = scratch();
    init_path_derived_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\nrelated:\n  - generic-a\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let data = run_json(nodex(tmp.path()).args(["rename", "docs/a.md", "docs/a-renamed.md"]));
    assert_eq!(
        data.pointer("/id_stability/type").and_then(Value::as_str),
        Some("anchored")
    );
    assert_eq!(
        data.pointer("/id_stability/id").and_then(Value::as_str),
        Some("generic-a")
    );

    // The moved file now has an explicit `id: generic-a` line.
    let moved = fs::read_to_string(tmp.path().join("docs/a-renamed.md")).unwrap();
    assert!(
        moved.contains("id: \"generic-a\"") || moved.contains("id: generic-a"),
        "anchored id must appear in moved frontmatter; got:\n{moved}"
    );

    // End-to-end witness: rebuild and confirm the cross-doc edge
    // still resolves. A backlink query on `generic-a` must surface
    // `generic-b` — the original guarantee a path-derived rename was
    // silently breaking.
    nodex(tmp.path()).arg("build").assert().success();
    let backlinks = run_json(nodex(tmp.path()).args(["query", "backlinks", "generic-a"]));
    let ids: Vec<&str> = backlinks
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str))
        .collect();
    assert!(
        ids.contains(&"generic-b"),
        "cross-doc reference must survive the rename: {ids:?}"
    );
}

#[test]
fn rename_path_derived_id_no_anchor_when_id_would_not_change() {
    // Edge case: a path-derived id project where the rename happens
    // to preserve the inferred id (e.g., the stem stays the same and
    // only the directory changes, under a glob that produces the
    // same template output). Anchoring would be unnecessary churn —
    // the contract is minimal-diff. Lock in: `id_stability == "unchanged"`.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md", "specs/**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
    let original = "---\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n";
    write_doc(tmp.path(), "docs/a.md", original);
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["rename", "docs/a.md", "specs/a.md"]));
    assert_eq!(
        data.pointer("/id_stability/type").and_then(Value::as_str),
        Some("unchanged")
    );
    let moved = fs::read_to_string(tmp.path().join("specs/a.md")).unwrap();
    assert_eq!(
        moved, original,
        "no-id-change rename must leave frontmatter byte-for-byte intact"
    );
}

#[test]
fn rename_bare_markdown_warns_about_id_shift() {
    // A bare markdown file (no frontmatter) still gets an inferred
    // id from the path. Renaming it changes that id, but the file
    // has no frontmatter for us to anchor into — silently moving on
    // would break references. Contract: `id_stability ==
    // "bare_no_frontmatter"` with an envelope-level warning so the
    // caller cannot miss it.
    let tmp = scratch();
    init_path_derived_project(tmp.path());
    write_doc(tmp.path(), "docs/bare.md", "# Bare\n\nNo frontmatter.\n");
    nodex(tmp.path()).arg("build").assert().success();

    let envelope =
        run_envelope(nodex(tmp.path()).args(["rename", "docs/bare.md", "docs/bare-renamed.md"]));
    assert_eq!(
        envelope
            .pointer("/data/id_stability/type")
            .and_then(Value::as_str),
        Some("bare_no_frontmatter")
    );
    let warnings: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("envelope-level warnings array")
        .iter()
        .filter_map(warning_msg)
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("inferred id changed")),
        "bare-file rename must surface a warning, not silently drift: {warnings:?}"
    );
}
