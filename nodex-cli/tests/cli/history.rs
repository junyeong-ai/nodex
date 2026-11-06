use super::support::*;
use serde_json::{Value, json};
use std::fs;

/// A removed document is named as its last revision reads, beside the
/// commits that added and removed its path; a removal the work tree has not
/// committed is named before any, as `HEAD` holds it. One that moved is still
/// present under its id, so the listing of what is gone leaves it out; an id
/// no document held has no history.
#[test]
fn history_names_what_the_project_no_longer_holds_and_the_commits_around_it() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    let gone = "---\nid: gone\ntitle: Gone\n---\n# Gone\n";
    write_doc(root, "docs/gone.md", gone);
    for id in ["kept", "leaving"] {
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!("---\nid: {id}\ntitle: T\n---\n# T\n"),
        );
    }
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "add all"]);
    git(&["rm", "-q", "docs/gone.md"]);
    git(&["mv", "docs/kept.md", "docs/moved.md"]);
    git(&["commit", "-q", "-m", "remove gone, move kept"]);
    git(&["rm", "-q", "docs/leaving.md"]);

    let removed = |data: &Value| -> Vec<Value> {
        assert_eq!(
            data["total"],
            data["items"].as_array().expect("items").len()
        );
        data["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|entry| {
                json!({
                    "id": entry["id"],
                    "path": entry["path"],
                    "added": entry["first_added"]["subject"],
                    "removed": entry["last_removed"]["subject"],
                })
            })
            .collect()
    };

    let departed = run_json(nodex(root).arg("history"));
    assert_eq!(departed.get("present"), None);
    assert_eq!(
        removed(&departed),
        [
            json!({"id": "leaving", "path": "docs/leaving.md", "added": "add all", "removed": null}),
            json!({"id": "gone", "path": "docs/gone.md", "added": "add all", "removed": "remove gone, move kept"}),
        ]
    );

    let by_id = run_json(nodex(root).args(["history", "gone"]));
    assert_eq!(by_id["present"], false);
    assert_eq!(removed(&by_id), removed(&departed)[1..]);
    let blob = by_id["items"][0]["blob"].as_str().expect("blob");
    let printed = git(&["cat-file", "-p", blob]);
    assert_eq!(String::from_utf8_lossy(&printed.stdout), gone);

    let moved = run_json(nodex(root).args(["history", "kept"]));
    assert_eq!(moved["present"], true);
    assert_eq!(removed(&moved)[0]["path"], "docs/kept.md");
    let by_path = run_json(nodex(root).args(["history", "--path", "./docs/kept.md"]));
    assert_eq!(by_path["present"], false);
    assert_eq!(removed(&by_path)[0]["id"], "kept");

    let never = run_json(nodex(root).args(["history", "never"]));
    assert_eq!(never, json!({"items": [], "total": 0, "present": false}));
}

/// A clone holding part of its history cannot say a document was never in
/// it, so it is refused rather than answered.
#[test]
fn history_refuses_a_shallow_clone() {
    let source = scratch();
    let git = git_runner(source.path());
    fs::write(
        source.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    write_doc(source.path(), "docs/a.md", "# A\n");
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a"]);
    write_doc(source.path(), "docs/b.md", "# B\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "b"]);
    let clone = scratch();
    assert!(
        git(&[
            "clone",
            "-q",
            "--no-local",
            "--depth",
            "1",
            source.path().to_str().unwrap(),
            clone.path().to_str().unwrap(),
        ])
        .status
        .success()
    );

    let refused = envelope_of(nodex(clone.path()).args(["history", "generic-docs-a"]));
    assert_eq!(refused["error"]["code"], "GIT_ERROR", "{refused}");
}
