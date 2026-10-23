//! The checkouts history reads use: persistence, locking, cleanup, and the
//! operator's git state they leave alone.

use crate::support::{
    adr, flow_project, git_runner, nodex, reported, run_json, scratch, staged, warning_msg,
    write_doc,
};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

/// A committed project whose plain `check` reads `HEAD` as its baseline, with
/// the kind of `docs/a.md` locked from creation and changed since, so the
/// verdict holds a finding only reading the baseline produces.
fn baselined_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**\"]\n[kinds]\nallowed = [\"generic\", \"note\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.frontmatter_immutable]]\nname = \"identity\"\nfields = [\"kind\"]\n\
         trigger = \"creation\"\n",
    )
    .unwrap();
    let doc = |kind: &str| {
        format!("---\nid: generic-a\ntitle: A\nkind: {kind}\nstatus: active\n---\n# A\n")
    };
    write_doc(root, "docs/a.md", &doc("generic"));
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    write_doc(root, "docs/a.md", &doc("note"));
}

/// Where the repository's trees are checked out for reading.
fn checkouts(root: &std::path::Path) -> PathBuf {
    root.join(".git/nodex/checkout")
}

/// A `check` whose verdict holds errors: exit 1, and the envelope that
/// reports them.
fn failing_check(root: &std::path::Path) -> Value {
    let output = nodex(root).arg("check").output().expect("check ran");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let envelope: Value = serde_json::from_str(stdout.trim()).expect("stdout is parseable JSON");
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope
}

fn locked_by_baseline(check: &Value) -> bool {
    check["violations"]
        .as_array()
        .expect("violations")
        .iter()
        .any(|v| v["rule_id"] == "frontmatter_immutable/identity" && v["node_id"] == "generic-a")
}

/// A ref is read in a directory under the git directory, which the next run
/// switches rather than writes afresh; it is no work tree of the repository.
#[test]
fn a_checkout_persists_under_the_git_directory_and_the_next_run_reuses_it() {
    let tmp = scratch();
    let root = tmp.path();
    baselined_project(root);

    for _ in 0..2 {
        let check = failing_check(root);
        assert!(locked_by_baseline(&check["data"]), "{check}");
    }
    assert!(
        checkouts(root).join("0/docs/a.md").is_file(),
        "the baseline stays checked out for the next read"
    );
    assert!(
        !checkouts(root).join("1").exists(),
        "a run with the directory to itself takes the first one"
    );
    let git = git_runner(root);
    let worktrees = String::from_utf8_lossy(&git(&["worktree", "list", "--porcelain"]).stdout)
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count();
    assert_eq!(worktrees, 1, "nothing is registered as a work tree");
}

#[test]
fn a_run_finding_the_checkout_held_takes_another_and_reads_the_same() {
    let tmp = scratch();
    let root = tmp.path();
    baselined_project(root);
    let alone = failing_check(root);
    assert!(locked_by_baseline(&alone["data"]), "{alone}");

    let held = fs::File::options()
        .read(true)
        .write(true)
        .open(checkouts(root).join("0.lock"))
        .expect("the first run left its lock file");
    held.try_lock()
        .expect("nothing else holds the first checkout");
    let beside = failing_check(root);
    assert_eq!(beside, alone, "a second checkout reads the same baseline");
    assert!(
        checkouts(root).join("1/docs/a.md").is_file(),
        "the held directory is left to its holder"
    );
}

/// A run that stops mid-read leaves its checkout as it was: files the index
/// does not list, and the lock git takes on the index. The next run takes it
/// back to exactly the tree it asks for.
#[test]
fn a_checkout_a_stopped_run_left_behind_holds_only_the_tree_it_is_given() {
    let tmp = scratch();
    let root = tmp.path();
    baselined_project(root);
    failing_check(root);
    write_doc(
        &checkouts(root).join("0"),
        "docs/ghost.md",
        "---\nid: [unclosed\n---\n",
    );
    fs::write(checkouts(root).join("0.index.lock"), "").unwrap();

    let envelope = failing_check(root);
    assert!(locked_by_baseline(&envelope["data"]), "{envelope}");
    let mentions_ghost = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(warning_msg)
        .any(|m| m.contains("ghost"));
    assert!(
        !mentions_ghost,
        "the baseline holds what HEAD records and nothing else: {envelope}"
    );
}

/// A committed project whose `docs/a.md` git writes through the `retitle`
/// filter, with its title locked from creation against `HEAD`. Whether the
/// filter has a driver is each test's to say.
fn filtered_project(root: &std::path::Path) {
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**\"]\n[rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.frontmatter_immutable]]\nname = \"title\"\nfields = [\"title\"]\n\
         trigger = \"creation\"\n",
    )
    .unwrap();
    fs::write(root.join(".gitattributes"), "docs/*.md filter=retitle\n").unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
}

/// Give the `retitle` filter a driver, so a checkout of `HEAD` now reads a
/// title other than the one the working tree was written with.
fn retitle(root: &std::path::Path) {
    git_runner(root)(&[
        "config",
        "filter.retitle.smudge",
        "sed -e 's/title: A/title: B/'",
    ]);
}

fn title_locked(check: &Value) -> bool {
    check["violations"]
        .as_array()
        .expect("violations")
        .iter()
        .any(|v| v["rule_id"] == "frontmatter_immutable/title" && v["node_id"] == "generic-a")
}

/// What a filter writes follows the configuration it runs under, which the
/// checkout's index does not record. A checkout keeps no such file past the
/// run that read it, so the next run reads it as the filter writes it now.
#[test]
fn a_checkout_keeps_no_filtered_file_past_the_run_that_read_it() {
    let tmp = scratch();
    let root = tmp.path();
    filtered_project(root);
    nodex(root).arg("check").assert().success();
    assert!(checkouts(root).join("0/nodex.toml").is_file());
    assert!(!checkouts(root).join("0/docs/a.md").exists());

    retitle(root);
    assert!(title_locked(&failing_check(root)["data"]));
}

/// A run that ends before it can remove its filtered files — its output
/// refused, here — leaves them to the next run, which removes them before
/// it reads anything.
#[test]
fn a_run_that_stopped_leaves_its_filtered_files_to_the_next_to_remove() {
    let tmp = scratch();
    let root = tmp.path();
    filtered_project(root);
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    #[expect(
        clippy::disallowed_methods,
        reason = "the binary under test, not a git invocation"
    )]
    let stopped = std::process::Command::new(env!("CARGO_BIN_EXE_nodex"))
        .arg("-C")
        .arg(root)
        .args(["diff", "HEAD", "HEAD"])
        .stdout(writer)
        .output()
        .expect("ran");
    assert_eq!(stopped.status.code(), Some(2));
    assert!(checkouts(root).join("0/docs/a.md").is_file());

    retitle(root);
    assert!(title_locked(&failing_check(root)["data"]));
}

/// Killing a nodex leaves the git it started writing into its checkout, and
/// the directory stays held until that git is done: the next run takes
/// another rather than writing beside it.
#[cfg(unix)]
#[test]
fn a_checkout_stays_held_while_the_git_a_killed_run_started_writes_it() {
    let tmp = scratch();
    let root = tmp.path();
    filtered_project(root);
    // Only the first filter run holds, and it holds until the test releases
    // it or ends, so the second check meets a held directory however slowly
    // it starts.
    let signals = tempfile::TempDir::new().unwrap();
    let held = signals.path().join("held");
    let started = signals.path().join("started");
    let released = signals.path().join("released");
    git_runner(root)(&[
        "config",
        "filter.retitle.smudge",
        &format!(
            "if mkdir '{held}' 2>/dev/null; then touch '{started}'; \
             while [ -d '{held}' ] && [ ! -e '{released}' ]; do sleep 0.05; done; fi; cat",
            held = held.display(),
            started = started.display(),
            released = released.display(),
        ),
    ]);
    #[expect(
        clippy::disallowed_methods,
        reason = "the binary under test, not a git invocation"
    )]
    let mut killed = std::process::Command::new(env!("CARGO_BIN_EXE_nodex"))
        .arg("-C")
        .arg(root)
        .arg("check")
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("ran");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !started.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "git never reached the filter"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    killed.kill().unwrap();
    killed.wait().unwrap();

    let next = nodex(root).arg("check").assert();
    fs::write(&released, "").unwrap();
    next.success();
    assert!(checkouts(root).join("1").is_dir());
}

/// A staged check that fails ends the process on its verdict, and leaves no
/// filtered file behind in either checkout it read.
#[test]
fn a_failing_staged_check_leaves_no_filtered_file_behind() {
    let tmp = scratch();
    let root = tmp.path();
    filtered_project(root);
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: C\nkind: generic\nstatus: active\n---\n# A\n",
    );
    git_runner(root)(&["add", "-A"]);
    staged(root).assert().code(1);
    for slot in ["0", "1"] {
        assert!(checkouts(root).join(slot).join("nodex.toml").is_file());
        assert!(!checkouts(root).join(slot).join("docs/a.md").exists());
    }
}

/// Submodule recursion is the operator's setting for their own work tree. A
/// checkout nodex reads through must leave the submodules they initialised
/// where they are, staged work included.
#[test]
fn a_checkout_leaves_the_operators_submodules_where_they_are() {
    let tmp = scratch();
    let upstream = tmp.path().join("upstream");
    fs::create_dir_all(&upstream).unwrap();
    let publish = git_runner(&upstream);
    publish(&["init", "-q"]);
    write_doc(&upstream, "s.txt", "s1\n");
    publish(&["add", "-A"]);
    publish(&["commit", "-q", "-m", "s1"]);

    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    flow_project(&root, "");
    adr(&root, "adr-a", "active", "a");
    let git = git_runner(&root);
    git(&["init", "-q"]);
    let added = git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        upstream.to_str().expect("UTF-8 temp path"),
        "vendor/sub",
    ]);
    assert!(added.status.success(), "{added:?}");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    git(&["config", "submodule.recurse", "true"]);
    let submodule = root.join("vendor/sub");
    write_doc(&submodule, "s.txt", "work in progress\n");
    git_runner(&submodule)(&["add", "s.txt"]);
    let config = root.join(".git/modules/vendor/sub/config");
    let before = fs::read_to_string(&config).unwrap();

    reported(nodex(&root).arg("check"));
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
    let staged = git_runner(&submodule)(&["diff", "--cached", "--name-only"]);
    assert_eq!(String::from_utf8_lossy(&staged.stdout).trim(), "s.txt");
}

/// A split index keeps its shared half in the git directory, and git expires
/// what it finds there. Nothing of a checkout's index may live where the
/// operator's git sweeps, nor sweep what the operator's index still needs.
#[test]
fn a_checkout_keeps_no_part_of_its_index_where_the_operators_git_expires_it() {
    let tmp = scratch();
    let root = tmp.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    flow_project(&root, "");
    adr(&root, "adr-a", "active", "a");
    let git = git_runner(&root);
    git(&["init", "-q"]);
    git(&["config", "core.splitIndex", "true"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    let linked = tmp.path().join("linked");
    git(&[
        "worktree",
        "add",
        "-q",
        linked.to_str().expect("UTF-8 temp path"),
    ]);
    reported(nodex(&linked).arg("check"));
    reported(nodex(&root).arg("check"));

    adr(&root, "adr-b", "proposed", "b");
    git(&["add", "-A"]);
    let weeks_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(21 * 24 * 3600);
    for entry in fs::read_dir(root.join(".git")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("sharedindex."))
        {
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(weeks_ago)
                .unwrap();
        }
    }
    reported(nodex(&root).arg("check"));
    let status = git(&["status", "--porcelain"]);
    assert!(status.status.success(), "{status:?}");
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("A  docs/adr-b.md"),
        "the operator's staged work is still in their index: {status:?}"
    );
}

/// Sparse-checkout patterns are the operator's view of their own work tree,
/// not a statement about what a ref records.
#[test]
fn a_sparse_work_tree_does_not_narrow_what_a_ref_records() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**\"]\n",
    )
    .unwrap();
    let doc = |status: &str| {
        format!("---\nid: generic-a\ntitle: A\nkind: generic\nstatus: {status}\n---\n# A\n")
    };
    write_doc(root, "docs/a.md", &doc("draft"));
    write_doc(root, "elsewhere/readme.txt", "not a document\n");
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "draft"]);
    write_doc(root, "docs/a.md", &doc("active"));
    git(&["commit", "-q", "-am", "active"]);
    let sparse = git(&["sparse-checkout", "set", "--cone", "elsewhere"]);
    assert!(sparse.status.success(), "{sparse:?}");
    assert!(
        !root.join("docs").exists(),
        "the work tree leaves docs/ out"
    );

    let diff = run_json(nodex(root).args(["diff", "HEAD~1", "HEAD"]));
    assert_eq!(
        diff["status_transitions"],
        serde_json::json!([{"id": "generic-a", "from": "draft", "to": "active"}]),
        "{diff}"
    );
}
