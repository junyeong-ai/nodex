//! Path arguments: spellings that name one document, and spellings refused.

use crate::support::{
    envelope_of, git_runner, init_project, nodex, run_envelope, run_json, scratch, write_doc,
};
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

#[test]
fn backslash_spellings_normalize_to_the_same_document() {
    // nodex's path language is forward-slashed on every platform; the
    // one normalization seam folds `\` before anything keys on the
    // path, so `docs\a.md` gates, scaffolds, and renames the same
    // document as `docs/a.md` — never a phantom second node or a stray
    // literal-backslash file.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"bogus-free\"]\nterminal = []\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    // check --content: the backslash spelling gates the same node — a
    // proposal that violates under `docs/a.md` violates under
    // `docs\a.md` too (no DUPLICATE_ID, no vacuous pass).
    let bad = "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: rogue\n---\n# A\n";
    for spelling in ["docs/a.md", "docs\\a.md", "docs\\./a.md"] {
        nodex(root)
            .args(["check", "--content"])
            .arg(format!("{spelling}=-"))
            .write_stdin(bad)
            .assert()
            .failure()
            .code(1);
    }

    // scaffold: the backslash spelling writes into docs/, exactly where
    // the envelope says, with the plain spelling's id.
    let env = run_envelope(
        nodex(root)
            .args(["scaffold", "--kind", "generic", "--title", "New"])
            .args(["--path", "docs\\new.md"]),
    );
    assert_eq!(
        env.pointer("/data/path").and_then(Value::as_str),
        Some("docs/new.md")
    );
    assert_eq!(
        env.pointer("/data/id").and_then(Value::as_str),
        Some("generic-new")
    );
    assert!(root.join("docs/new.md").exists(), "written where reported");
    assert!(
        !root.join("docs\\new.md").exists() || cfg!(windows),
        "no stray literal-backslash file"
    );

    // rename: backslash destination lands at the folded path and the
    // envelope reports it forward-slashed.
    let env = run_envelope(nodex(root).args(["rename", "docs\\new.md", "docs\\moved.md"]));
    assert_eq!(
        env.pointer("/data/new_path").and_then(Value::as_str),
        Some("docs/moved.md")
    );
    assert!(root.join("docs/moved.md").exists());
    nodex(root).arg("build").assert().success();
}

/// A project whose `docs/a.md` is reachable under folded spellings, and
/// whether this volume folds them at all. On a case-sensitive filesystem
/// every such spelling is a genuinely new path, so each seam gates it
/// normally — the assertions below say so rather than skipping.
fn folding_project() -> (TempDir, bool) {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();
    let folds = root.join("NODEX.TOML").exists();
    (tmp, folds)
}

fn spelling_refusal(envelope: &Value) -> bool {
    envelope.get("ok").and_then(Value::as_bool) == Some(false)
        && envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("spelled differently from the filesystem's own")
}

#[test]
fn check_content_refuses_a_folded_spelling() {
    // Every comparison downstream of the gate is exact, so bytes proposed
    // under a spelling the scan never produces are judged as a document
    // nothing else can find — while the write they clear lands on the real
    // file.
    let (tmp, folds) = folding_project();
    let root = tmp.path();
    let proposal = "---\nid: generic-other\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n";
    let output = nodex(root)
        .args(["check", "--content", "docs/A.MD=-"])
        .write_stdin(proposal)
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    if folds {
        assert!(spelling_refusal(&envelope), "{envelope}");
    } else {
        assert_eq!(output.status.code(), Some(0), "{envelope}");
    }
}

#[test]
fn a_folded_spelling_does_not_carry_a_write_past_the_baseline_lock() {
    // The sharpest form: `scaffold --force` at the document's own spelling
    // is refused by the immutability lock, so the same write at a folded
    // spelling must be refused too. The lock reads the baseline by exact
    // path — an unrefused folded spelling looks up nothing, finds no frozen
    // record, and overwrites the file the lock exists to protect.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         trigger = \"creation\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/sub/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n# frozen history\n",
    );
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let folds = root.join("NODEX.TOML").exists();

    let exact = envelope_of(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Impostor",
        "--path",
        "docs/sub/a.md",
        "--force",
    ]));
    assert_eq!(
        exact.get("ok").and_then(Value::as_bool),
        Some(false),
        "the lock refuses the write at the document's own spelling: {exact}"
    );

    let folded = envelope_of(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Impostor",
        "--path",
        "docs/SUB/a.md",
        "--force",
    ]));
    if folds {
        assert!(spelling_refusal(&folded), "{folded}");
    }
    // The property under test holds on either volume: the frozen record is
    // still there. Where spellings fold, because the write was refused as one;
    // where they do not, because `docs/SUB/a.md` is a different file — one the
    // project refuses for its own reasons, since the id it infers from the
    // stem is already taken.
    assert!(
        fs::read_to_string(root.join("docs/sub/a.md"))
            .unwrap()
            .contains("frozen history"),
        "the frozen record survives: {folded}"
    );
}

#[test]
fn rename_refuses_a_folded_destination_spelling() {
    // The destination is written into every rewritten reference. A folded
    // one is rewritten as authored, resolves against a path index that
    // spells it the filesystem's way, and dangles every link the command
    // reported as updated.
    let (tmp, folds) = folding_project();
    let root = tmp.path();
    write_doc(
        root,
        "docs/ref.md",
        "---\nid: generic-ref\ntitle: R\nkind: generic\nstatus: active\n---\n[a](a.md)\n",
    );
    nodex(root).arg("build").assert().success();
    fs::create_dir_all(root.join("docs/sub")).unwrap();

    let env = envelope_of(nodex(root).args(["rename", "docs/a.md", "docs/SUB/moved.md"]));
    if folds {
        assert!(spelling_refusal(&env), "{env}");
    } else {
        assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true), "{env}");
    }
}

#[test]
fn a_path_whose_components_do_not_exist_yet_is_not_a_folded_spelling() {
    // The guard asks the filesystem, and a path that exists under no
    // spelling cannot alias one that does — so authoring a document into a
    // directory tree that does not exist yet stays legal on every volume.
    let (tmp, _folds) = folding_project();
    let root = tmp.path();
    let env = run_envelope(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Fresh",
        "--path",
        "docs/Brand/New/Deep/fresh.md",
    ]));
    assert_eq!(
        env.pointer("/data/path").and_then(Value::as_str),
        Some("docs/Brand/New/Deep/fresh.md"),
        "{env}"
    );
    assert!(root.join("docs/Brand/New/Deep/fresh.md").exists());
}

#[test]
fn dot_prefixed_mutation_paths_normalize_like_check_content() {
    // `./docs/a.md` and `docs/a.md` name the same document; scaffold and
    // rename collapse the `.` segment (as `check --content` does) so the
    // admission probe and id inference key on the scanner's
    // root-relative form — the dot-prefixed spelling of a perfectly
    // scoped path must not be refused.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Dot"])
        .args(["--path", "./docs/dot.md"])
        .assert()
        .success();
    assert!(root.join("docs/dot.md").exists());

    nodex(root)
        .args(["rename", "./docs/dot.md", "./docs/dot2.md"])
        .assert()
        .success();
    assert!(root.join("docs/dot2.md").exists());
    nodex(root).arg("build").assert().success();
    let data = run_json(nodex(root).args(["query", "node", "--path", "docs/dot2.md"]));
    assert!(
        data.pointer("/node/id").and_then(Value::as_str).is_some(),
        "renamed doc graphed under the normalized key: {data}"
    );
}

#[test]
fn scaffold_rejects_path_traversal() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "x"])
        .args(["--path", "../escaped.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
}

#[test]
fn rename_rejects_path_traversal() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["rename", "docs/a.md", "../escaped.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("PATH_ESCAPES_ROOT")
    );
}

#[cfg(unix)]
#[test]
fn a_document_whose_name_holds_a_backslash_is_graphed_where_it_lives() {
    // `\` divides components where the platform says so and nowhere else.
    // A name the walk read has to render reversibly: folding a character the
    // filesystem allows in a filename puts a path in the graph that no reader
    // can open, and every seam that reads a document by its recorded path
    // skips it.
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
        "docs/target.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    let odd = "docs/literal\\ref.md";
    write_doc(
        root,
        odd,
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[t](target.md)\n",
    );
    nodex(root).arg("build").assert().success();

    let listed = run_json(nodex(root).args(["query", "nodes", "--fields", "id,path"]));
    let paths: Vec<&str> = listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|n| n["path"].as_str())
        .collect();
    assert!(
        paths.contains(&odd),
        "the graph carries the name on disk: {listed}"
    );

    // A reference from it is rewritten, which requires reading it back at the
    // path the graph recorded.
    let env = run_envelope(nodex(root).args(["rename", "docs/target.md", "docs/moved.md"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "{env}"
    );
    assert!(env.get("warnings").is_none(), "nothing was skipped: {env}");
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).arg("query").arg("issues"));
    assert_eq!(
        issues.get("unresolved_edges").and_then(Value::as_array),
        Some(&vec![]),
        "{issues}"
    );
}
