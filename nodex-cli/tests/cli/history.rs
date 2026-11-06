use super::support::*;
use serde_json::{Value, json};
use std::fs;

/// Each document — an id at a path — the project no longer holds is named as
/// a revision that held it reads, with how it left: a commit removed it, the
/// work tree removed it, or no commit did because its id changed in place. A
/// path another document took again still names the one that left it. One
/// that moved is still present under its id, so the listing of what is gone
/// leaves it out; an id no document held has no history.
#[test]
fn history_names_what_the_project_no_longer_holds_and_how_it_left() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n",
    )
    .unwrap();
    let doc = |id: &str| format!("---\nid: {id}\ntitle: T\n---\n# T\n");
    let gone = doc("gone");
    for (path, id) in [
        ("gone", "gone"),
        ("kept", "kept"),
        ("leaving", "leaving"),
        ("plan", "first-plan"),
        ("named", "old-name"),
    ] {
        write_doc(root, &format!("docs/{path}.md"), &doc(id));
    }
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "add all"]);
    git(&["rm", "-q", "docs/gone.md", "docs/plan.md"]);
    git(&["mv", "docs/kept.md", "docs/moved.md"]);
    write_doc(root, "docs/named.md", &doc("new-name"));
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "retire"]);
    write_doc(root, "docs/plan.md", &doc("second-plan"));
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "reuse plan"]);
    git(&["rm", "-q", "docs/leaving.md"]);

    let left = |data: &Value| -> Vec<Value> {
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
                    "removal": entry["removal"]["type"],
                    "by": entry["removal"]["commit"]["subject"],
                })
            })
            .collect()
    };
    let entry = |id: &str, path: &str, removal: &str, by: Option<&str>| json!({"id": id, "path": path, "added": "add all", "removal": removal, "by": by});

    let departed = run_json(nodex(root).arg("history"));
    assert_eq!(departed.get("present"), None);
    assert_eq!(
        left(&departed),
        [
            entry("leaving", "docs/leaving.md", "uncommitted", None),
            entry("gone", "docs/gone.md", "committed", Some("retire")),
            entry("first-plan", "docs/plan.md", "committed", Some("retire")),
            entry("old-name", "docs/named.md", "unattributed", None),
        ]
    );

    let by_id = run_json(nodex(root).args(["history", "gone"]));
    assert_eq!(by_id["present"], false);
    assert_eq!(left(&by_id), left(&departed)[1..2]);
    let blob = by_id["items"][0]["blob"].as_str().expect("blob");
    let printed = git(&["cat-file", "-p", blob]);
    assert_eq!(String::from_utf8_lossy(&printed.stdout), gone);

    let reused = run_json(nodex(root).args(["history", "--path", "docs/plan.md"]));
    assert_eq!(reused["present"], true);
    assert_eq!(left(&reused), left(&departed)[2..3]);
    let renamed = run_json(nodex(root).args(["history", "old-name"]));
    assert_eq!(left(&renamed), left(&departed)[3..]);

    let moved = run_json(nodex(root).args(["history", "kept"]));
    assert_eq!(moved["present"], true);
    assert_eq!(
        left(&moved),
        [entry("kept", "docs/kept.md", "committed", Some("retire"))]
    );
    let by_path = run_json(nodex(root).args(["history", "--path", "./docs/kept.md"]));
    assert_eq!(by_path["present"], false);
    assert_eq!(left(&by_path), left(&moved));

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
