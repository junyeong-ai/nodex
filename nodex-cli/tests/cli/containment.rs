//! Writes never leave the project through a symlink or the output
//! directory.

use crate::support::{init_project, nodex, run_envelope, scratch, warning_msg, write_doc};
use serde_json::Value;
use std::fs;

/// Opening a FIFO blocks until a writer appears, so a target that resolves to
/// one at the destination hung the command with no envelope at all. The
/// scanner admits a document by `is_file()`, and so does the gate — decided
/// from metadata, which never opens anything.
#[test]
#[cfg(unix)]
fn rename_does_not_open_a_non_regular_destination_target() {
    let tmp = scratch();
    let project = tmp.path();
    fs::write(
        project.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nexclude = [\"**/store/**\"]\n\
         [kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    for dir in ["docs/x", "docs/z/w", "store", "docs/store"] {
        fs::create_dir_all(project.join(dir)).unwrap();
    }
    fs::write(
        project.join("store/f.md"),
        "---\nid: rec\ntitle: R\nkind: generic\nstatus: active\n---\n# R\n",
    )
    .unwrap();
    // Not git, and std has no FIFO constructor — the one way to build the
    // fixture this test is about.
    #[expect(
        clippy::disallowed_methods,
        reason = "mkfifo is the only way to create the non-regular file under test"
    )]
    let made = std::process::Command::new("mkfifo")
        .arg(project.join("docs/store/f.md"))
        .status()
        .expect("mkfifo");
    assert!(made.success());
    std::os::unix::fs::symlink("../../store/f.md", project.join("docs/x/a.md")).unwrap();
    nodex(project).arg("build").assert().success();

    // Spawned rather than run to completion: a regression here blocks forever,
    // and a test that hangs tells CI nothing. `assert_cmd` waits, so this is
    // the one place the binary is driven directly.
    #[expect(
        clippy::disallowed_methods,
        reason = "the binary must be killable, which running it to completion cannot be"
    )]
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_nodex"))
        .args(["-C", project.to_str().unwrap()])
        .args(["rename", "docs/x/a.md", "docs/z/w/a.md"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawned");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let finished = loop {
        match child.try_wait().expect("wait") {
            Some(_) => break true,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                break false;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };
    assert!(
        finished,
        "rename must decide from metadata instead of opening the target"
    );
}

#[test]
#[cfg(unix)]
fn rename_rewriter_does_not_follow_symlinks() {
    // The link rewriter touches every in-scope file. Symlinks are
    // skipped so a link pointing outside the project root cannot have
    // its target mutated through the symlink.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    let config = fs::read_to_string(tmp.path().join("nodex.toml"))
        .unwrap()
        .replace(
            "unresolved_reference = \"error\"",
            "unresolved_reference = \"warning\"",
        );
    fs::write(tmp.path().join("nodex.toml"), config).unwrap();
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    let external = outside.path().join("external.md");
    fs::write(
        &external,
        "---\nid: external\ntitle: External\nkind: generic\nstatus: active\n---\n# External\n\
         See [a](a.md) for details.\n",
    )
    .unwrap();
    let before = fs::read_to_string(&external).unwrap();
    unix_fs::symlink(&external, tmp.path().join("linked.md")).unwrap();

    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["rename", "a.md", "renamed.md"])
        .assert()
        .success();

    // External file byte-identical even though it contained a link
    // to `a.md` that the rewriter would otherwise have updated.
    let after = fs::read_to_string(&external).unwrap();
    assert_eq!(
        before, after,
        "rename must not mutate external files reached through a symlink"
    );
    // The in-project file still got renamed.
    assert!(tmp.path().join("renamed.md").exists());
}

#[test]
#[cfg(unix)]
fn lifecycle_refuses_to_mutate_through_symlink() {
    // `lifecycle::transition` refuses to write through a symlink, so
    // `nodex lifecycle set <id> --status` on a symlinked doc cannot reach
    // a target outside the project root.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    let external = outside.path().join("external.md");
    fs::write(
        &external,
        "---\nid: ext\ntitle: Ext\nkind: generic\nstatus: active\n---\n# Ext\n",
    )
    .unwrap();
    let before = fs::read_to_string(&external).unwrap();
    unix_fs::symlink(&external, tmp.path().join("linked.md")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["lifecycle", "set", "ext", "--status", "archived"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "must reject symlink mutation");
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("SYMLINK_TARGET")
    );

    // The external file must be byte-identical to before.
    let after = fs::read_to_string(&external).unwrap();
    assert_eq!(
        before, after,
        "external file through symlink must not be mutated"
    );
}

#[test]
#[cfg(unix)]
fn rename_rejects_destination_through_symlinked_directory() {
    // A lexically clean destination (`linked-dir/evil.md`) whose
    // ancestor is a symlink pointing outside the project root must be
    // refused before the move — `reject_traversal` cannot see this,
    // `reject_outside_root` must.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    unix_fs::symlink(outside.path(), tmp.path().join("linked-dir")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    let output = nodex(tmp.path())
        .args(["rename", "a.md", "linked-dir/evil.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success(), "must reject escaping destination");
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
    // Source untouched, nothing materialised outside the root.
    assert!(tmp.path().join("a.md").exists(), "source must remain");
    assert!(
        !outside.path().join("evil.md").exists(),
        "no file may appear outside the project root"
    );
}

#[test]
#[cfg(unix)]
fn rename_rejects_source_through_symlinked_directory() {
    // The mirror of the destination guard: a SOURCE reached through a
    // symlinked ancestor would let `fs::rename` pull an out-of-root
    // file into the project (exfiltration). Both the bare and the
    // pinned-id source shapes must be refused — neither passes through
    // a write that would incidentally guard them.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    fs::write(
        outside.path().join("secret.md"),
        "TOP SECRET out-of-root content\n",
    )
    .unwrap();
    fs::write(
        outside.path().join("pinned.md"),
        "---\nid: pinned\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    unix_fs::symlink(outside.path(), tmp.path().join("docs").join("ext")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    for source in ["docs/ext/secret.md", "docs/ext/pinned.md"] {
        let output = nodex(tmp.path())
            .args(["rename", source, "docs/pulled-in.md"])
            .output()
            .expect("ran");
        assert!(!output.status.success(), "escaping source must be refused");
        let parsed: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
        assert_eq!(
            parsed.pointer("/error/code").and_then(Value::as_str),
            Some("PATH_ESCAPES_ROOT"),
            "source {source}"
        );
        assert!(
            !tmp.path().join("docs/pulled-in.md").exists(),
            "out-of-root file must not be pulled into the project"
        );
    }
    assert!(
        outside.path().join("secret.md").exists() && outside.path().join("pinned.md").exists(),
        "out-of-root files must remain where they are"
    );
}

#[test]
#[cfg(unix)]
fn retarget_skips_file_under_symlinked_directory_with_warning() {
    // With `scope.follow_symlinks` on, a file outside the root can be a graph
    // node. Retargeting must give it the reader-follows / writer-skips
    // treatment: complete the batch (exit 0), rewrite the real in-root
    // referrer, warn about the symlinked one, and never touch the external
    // target.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    let outside = scratch();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "old.md",
        "---\nid: doc-old\ntitle: Old\nkind: generic\nstatus: active\n---\n# Old\n",
    );
    write_doc(
        root,
        "new.md",
        "---\nid: doc-new\ntitle: New\nkind: generic\nstatus: active\n---\n# New\n",
    );
    write_doc(
        root,
        "ref.md",
        "---\nid: doc-ref\ntitle: Ref\nkind: generic\nstatus: active\nrelated: doc-old\n---\n# Ref\n",
    );
    let external = "---\nid: doc-ext\ntitle: Ext\nkind: generic\nstatus: active\nrelated: doc-old\n---\n# Ext\n";
    fs::write(outside.path().join("ext.md"), external).unwrap();
    unix_fs::symlink(outside.path(), root.join("linked")).unwrap();
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["retarget", "doc-old", "doc-new"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    let updated: Vec<&str> = env
        .pointer("/data/references_updated")
        .and_then(Value::as_array)
        .expect("references_updated")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        updated.contains(&"ref.md"),
        "in-root referrer rewritten: {updated:?}"
    );
    assert!(
        !updated.iter().any(|p| p.contains("linked")),
        "symlinked referrer must not be rewritten: {updated:?}"
    );
    let warnings = env
        .get("warnings")
        .and_then(Value::as_array)
        .expect("skip warning present");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("linked/ext.md") && w.contains("symlink")),
        "warning names the skipped path: {warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("ext.md")).unwrap(),
        external,
        "external target byte-identical"
    );
}

#[test]
#[cfg(unix)]
fn migrate_warns_on_bare_symlinked_file() {
    // Writer-skips / reader-follows symmetry: migrate skips symlinks
    // like rename/retarget do, but a *bare* symlinked file is exactly
    // what migrate exists to fix — the skip must surface as a warning
    // naming the file, and the external target must stay untouched.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    let external = outside.path().join("bare.md");
    fs::write(&external, "# Bare external doc\n").unwrap();
    let before = fs::read_to_string(&external).unwrap();
    unix_fs::symlink(&external, tmp.path().join("linked.md")).unwrap();

    let envelope = run_envelope(nodex(tmp.path()).args(["migrate", "--apply"]));
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("bare symlinked file must produce a warning");
    assert!(
        warnings
            .iter()
            .any(|w| warning_msg(w).is_some_and(|s| s.contains("linked.md"))),
        "warning must name the skipped file: {warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(&external).unwrap(),
        before,
        "external target must not receive frontmatter through the symlink"
    );
}

#[test]
#[cfg(unix)]
fn migrate_tolerates_dangling_symlink_and_silences_anchored_ones() {
    // A dangling symlink in scope must neither abort the batch nor
    // warn (it is not a migration target); a symlinked file that
    // already carries frontmatter is equally not a target and stays
    // silent — only *bare* symlinked files warn.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    unix_fs::symlink(
        tmp.path().join("ghost-target.md"),
        tmp.path().join("ghost.md"),
    )
    .unwrap();
    let anchored = outside.path().join("anchored.md");
    fs::write(
        &anchored,
        "---\nid: anchored\ntitle: Anchored\nkind: generic\nstatus: active\n---\n# Anchored\n",
    )
    .unwrap();
    unix_fs::symlink(&anchored, tmp.path().join("anchored-link.md")).unwrap();

    let envelope = run_envelope(nodex(tmp.path()).args(["migrate", "--apply"]));
    if let Some(warnings) = envelope.get("warnings").and_then(Value::as_array) {
        assert!(
            !warnings.iter().any(|w| w
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|s| s.contains("ghost.md") || s.contains("anchored-link.md"))),
            "dangling / frontmatter-carrying symlinks must not warn: {warnings:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn report_refuses_symlinked_output_dir_escape() {
    // `output.dir` passes the lexical load check, but a symlinked
    // ancestor can still resolve it outside the project. The write
    // primitive enforces containment, so report hard-fails — writing
    // the artefacts is the command's purpose.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    unix_fs::symlink(outside.path(), tmp.path().join("_index")).unwrap();

    let output = nodex(tmp.path()).arg("report").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
    assert!(
        !outside.path().join("graph.json").exists() && !outside.path().join("GRAPH.md").exists(),
        "nothing may land outside the project root"
    );
}

#[cfg(unix)]
#[test]
fn build_refuses_graph_json_write_through_escaping_output_dir() {
    // graph.json is the build's purpose, so an escaping output dir is a
    // hard PATH_ESCAPES_ROOT — never a silent write outside the root.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    unix_fs::symlink(outside.path(), tmp.path().join("_index")).unwrap();

    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
    assert!(!outside.path().join("graph.json").exists());
}

#[cfg(unix)]
#[test]
fn build_warns_and_skips_cache_persist_when_cache_json_is_a_symlink() {
    // The cache is an optimization, not the build's purpose: a
    // cache.json the primitive refuses (here: the user's symlink, which
    // the staged rename would otherwise silently replace) degrades to
    // an honest envelope warning while the graph data stays correct.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    fs::create_dir_all(tmp.path().join("_index")).unwrap();
    unix_fs::symlink(
        outside.path().join("external-cache.json"),
        tmp.path().join("_index/cache.json"),
    )
    .unwrap();

    let envelope = run_envelope(nodex(tmp.path()).arg("build"));
    assert_eq!(
        envelope.pointer("/data/nodes").and_then(Value::as_u64),
        Some(1),
        "the graph itself is correct"
    );
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings present");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("cache save failed")),
        "cache persistence failure must surface as a warning: {warnings:?}"
    );
    assert!(
        !outside.path().join("external-cache.json").exists(),
        "nothing was written through the link"
    );
    assert!(tmp.path().join("_index/graph.json").exists());
}

#[cfg(unix)]
#[test]
fn init_refuses_dangling_symlinked_nodex_toml() {
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let outside = scratch();
    let ghost = outside.path().join("ghost.toml");
    unix_fs::symlink(&ghost, tmp.path().join("nodex.toml")).unwrap();

    let output = nodex(tmp.path()).arg("init").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("SYMLINK_TARGET")
    );
    assert!(!ghost.exists(), "nothing was written through the link");
}
