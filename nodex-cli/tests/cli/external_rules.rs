//! `[[rules.external]]`: a program the project declares judges documents
//! through the protocol `nodex_core::rules::external` states. The programs
//! here are shell scripts that keep the request they were handed and answer
//! from files the test wrote, so a test reads both sides of the exchange.

use crate::support::{envelope_of, git_runner, judged, nodex, scratch, write_doc};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const CONFIG: &str = "[scope]\ninclude = [\"docs/**/*.md\"]\n\
    [[rules.external]]\nname = \"words\"\ncommand = [\"tools/words.sh\"]\n";

/// Keeps the request as `request.json` in the directory it runs in and
/// answers `forbidden.json` when a document it was handed says FORBIDDEN,
/// `standing.json` otherwise.
const WORDS: &str = "#!/bin/sh\ncat > request.json\n\
    if grep -q FORBIDDEN request.json; then cat forbidden.json; else cat standing.json; fi\n";

const A: &str = "---\nid: a\ntitle: A\norphan_ok: true\n---\n# A\n\nTODO\n";
const B: &str = "---\nid: b\ntitle: B\norphan_ok: true\n---\n# B\n";

fn project(root: &Path, program: &str, standing: &Value) {
    fs::write(root.join("nodex.toml"), CONFIG).unwrap();
    write_doc(root, "docs/a.md", A);
    write_doc(root, "docs/b.md", B);
    write_doc(root, "tools/words.sh", program);
    fs::set_permissions(
        root.join("tools/words.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::write(root.join("standing.json"), standing.to_string()).unwrap();
    fs::write(
        root.join("forbidden.json"),
        json!({"protocol": 1, "findings": [
            {"message": "a document says FORBIDDEN", "data": {"word": "FORBIDDEN"}}
        ]})
        .to_string(),
    )
    .unwrap();
}

fn request(root: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(root.join("request.json")).unwrap()).unwrap()
}

fn content_of<'a>(request: &'a Value, id: &str) -> &'a str {
    request["documents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|document| document["id"] == id)
        .and_then(|document| document["content"].as_str())
        .unwrap_or_else(|| panic!("{id} was not handed over: {request}"))
}

#[test]
fn an_external_rule_judges_the_bytes_each_pass_reads_and_gates_what_it_introduces() {
    let tmp = scratch();
    let root = tmp.path();
    project(
        root,
        WORDS,
        &json!({"protocol": 1, "findings": [
            {"id": "a", "message": "a stands", "line": 8, "data": {"word": "TODO"}}
        ]}),
    );

    let report = judged(nodex(root).arg("check"));
    let found: Vec<&Value> = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["rule_id"] == "external/words")
        .collect();
    assert_eq!(
        found,
        [&json!({
            "rule_id": "external/words",
            "severity": "error",
            "node_id": "a",
            "path": "docs/a.md",
            "message": "line 8: a stands",
            "details": {"type": "external", "message": "a stands", "line": 8,
                        "data": {"word": "TODO"}}
        })],
        "{report}"
    );
    assert!(
        report["rule_coverage"]
            .as_array()
            .unwrap()
            .contains(&json!({"rule_id": "external/words", "unit": "nodes",
                              "subjects": 2, "unjudged": 0})),
        "{report}"
    );
    let asked = request(root);
    assert_eq!(asked["protocol"], 1);
    assert_eq!(asked["rule"], "external/words");
    assert_eq!(content_of(&asked, "a"), A);

    // A proposal is handed over as the bytes it proposes, and a finding the
    // tree already carries is not one it introduces.
    let edited = B.replace("# B", "# B\n\nedited");
    nodex(root)
        .args(["check", "--content", "docs/b.md=-"])
        .write_stdin(edited.clone())
        .assert()
        .success();
    assert_eq!(content_of(&request(root), "b"), edited);

    let forbidden = B.replace("# B", "# B\n\nFORBIDDEN");
    let report = judged(
        nodex(root)
            .args(["check", "--content", "docs/b.md=-"])
            .write_stdin(forbidden.clone()),
    );
    assert_eq!(report["has_errors"], true, "{report}");

    // The write gates ask the same program, so the write is refused.
    let envelope = envelope_of(
        nodex(root)
            .args([
                "scaffold",
                "--kind",
                "generic",
                "--title",
                "C",
                "--path",
                "docs/c.md",
                "--body",
                "-",
            ])
            .write_stdin("FORBIDDEN\n"),
    );
    assert_eq!(
        envelope["error"]["code"], "CONTENT_VIOLATIONS",
        "{envelope}"
    );
    assert!(!root.join("docs/c.md").exists());

    // A snapshot the tree has moved past holds no bytes to hand over.
    nodex(root).arg("build").assert().success();
    write_doc(root, "docs/a.md", &A.replace("TODO", "edited"));
    let envelope = envelope_of(nodex(root).args(["query", "issues", "--allow-stale"]));
    assert_eq!(envelope["error"]["code"], "GRAPH_OUTDATED", "{envelope}");
}

#[test]
fn migrate_writes_nothing_an_external_rule_refuses() {
    let tmp = scratch();
    let root = tmp.path();
    project(root, WORDS, &json!({"protocol": 1}));
    write_doc(root, "docs/c.md", "# C\n");
    fs::write(
        root.join("tools/words.sh"),
        WORDS.replace("FORBIDDEN", "kind: "),
    )
    .unwrap();

    let envelope = envelope_of(nodex(root).args(["migrate", "--apply"]));
    assert_eq!(
        envelope["error"]["code"], "CONTENT_VIOLATIONS",
        "{envelope}"
    );
    assert_eq!(fs::read_to_string(root.join("docs/c.md")).unwrap(), "# C\n");

    fs::write(root.join("tools/words.sh"), WORDS).unwrap();
    nodex(root).args(["migrate", "--apply"]).assert().success();
    assert!(
        fs::read_to_string(root.join("docs/c.md"))
            .unwrap()
            .starts_with("---\n")
    );
}

#[test]
fn an_external_rule_that_cannot_answer_refuses_the_command() {
    let tmp = scratch();
    let root = tmp.path();
    for (program, refusal) in [
        (
            "#!/bin/sh\ncat > /dev/null\necho not-json\n",
            "not protocol 1",
        ),
        (
            "#!/bin/sh\ncat > /dev/null\necho boom >&2\nexit 3\n",
            "exited with status 3; stderr: boom",
        ),
    ] {
        project(root, program, &json!({"protocol": 1}));
        nodex(root).arg("build").assert().success();
        for args in [&["check"][..], &["query", "issues"]] {
            let output = nodex(root).args(args).output().unwrap();
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(envelope["error"]["code"], "CONFIG_ERROR", "{envelope}");
            let message = envelope["error"]["message"].as_str().unwrap();
            assert!(
                message.contains("external rule external/words") && message.contains(refusal),
                "{args:?}: {message}"
            );
        }
    }
}

#[test]
fn a_staged_check_hands_the_index_bytes_to_the_program_the_work_tree_holds() {
    let tmp = scratch();
    let root = tmp.path();
    project(root, WORDS, &json!({"protocol": 1}));
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "nodex.toml", "docs"]);
    git(&["commit", "-q", "-m", "init"]);

    // The index says FORBIDDEN, the working tree does not, and the program
    // is installed only in the working tree.
    write_doc(root, "docs/b.md", &B.replace("# B", "# B\n\nFORBIDDEN"));
    git(&["add", "docs/b.md"]);
    write_doc(root, "docs/b.md", B);

    nodex(root).arg("check").assert().success();
    let report = judged(&mut crate::support::staged(root));
    assert_eq!(report["has_errors"], true, "{report}");
    let asked = request(root);
    assert!(content_of(&asked, "b").contains("FORBIDDEN"));
    assert_ne!(
        Path::new(asked["tree"].as_str().unwrap()),
        root.canonicalize().unwrap(),
        "the documents come from the index's checkout"
    );

    // A range touching no document still reports what the program found:
    // nodex cannot tell what decided it.
    fs::write(
        root.join("standing.json"),
        json!({"protocol": 1, "findings": [{"id": "a", "message": "a stands"}]}).to_string(),
    )
    .unwrap();
    let report = judged(nodex(root).args(["check", "--since", "HEAD"]));
    assert_eq!(report["has_errors"], true, "{report}");
}
