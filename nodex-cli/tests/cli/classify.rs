use super::support::*;
use serde_json::{Value, json};
use std::fs;

/// Each path is read from the configuration alone: the scope's verdict, and
/// for an admitted path the kind, id and status a document declaring none
/// would take, a flow's entry status included. A document's own frontmatter
/// is not read, and no graph is built or required.
#[test]
fn classify_reads_each_path_from_the_configuration_without_a_graph() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nexclude = [\"docs/drafts/**\"]\n\
         [kinds]\nallowed = [\"generic\", \"adr\"]\n\
         [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
         terminal = [\"superseded\"]\ninitial = \"active\"\n\
         [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
         transitions = { proposed = [\"active\"], active = [\"superseded\"] }\n\
         [[identity.kind_rules]]\nglob = \"docs/decisions/**\"\nkind = \"adr\"\n\
         [[identity.id_rules]]\nkind = \"adr\"\ntemplate = \"adr-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/decisions/0001-kept.md",
        "---\nid: declared\nkind: adr\nstatus: active\n---\n# Kept\n",
    );

    let data = run_json(nodex(root).args([
        "classify",
        "docs/decisions/0001-kept.md",
        "./docs/decisions/0002-new.md",
        "docs/guide.md",
        "docs/drafts/wip.md",
        "src/main.rs",
    ]));

    assert_eq!(
        data,
        json!({
            "items": [
                {
                    "path": "docs/decisions/0001-kept.md",
                    "in_scope": true,
                    "inferred": {"kind": "adr", "id": "adr-0001-kept", "status": "proposed"}
                },
                {
                    "path": "docs/decisions/0002-new.md",
                    "in_scope": true,
                    "inferred": {"kind": "adr", "id": "adr-0002-new", "status": "proposed"}
                },
                {
                    "path": "docs/guide.md",
                    "in_scope": true,
                    "inferred": {"kind": "generic", "id": "generic-docs-guide", "status": "active"}
                },
                {"path": "docs/drafts/wip.md", "in_scope": false},
                {"path": "src/main.rs", "in_scope": false}
            ],
            "total": 5
        })
    );
    assert!(!root.join("_index").exists(), "classify wrote output");
}

/// A `conditional_exclude` keeps out what a terminal parent governs, whether
/// the child exists yet or not.
#[test]
fn classify_keeps_out_the_paths_a_terminal_parent_excludes() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\n\
         parent_glob = \"docs/spec/index.md\"\n\
         child_glob = \"docs/spec/notes/**/*.md\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\", \"done\"]\nterminal = [\"done\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/spec/index.md",
        "---\nid: spec\nstatus: done\n---\n# Spec\n",
    );
    write_doc(root, "docs/spec/notes/old.md", "# Old\n");

    let data = run_json(nodex(root).args([
        "classify",
        "docs/spec/index.md",
        "docs/spec/notes/old.md",
        "docs/spec/notes/new.md",
    ]));
    let in_scope: Vec<_> = data["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|entry| entry["in_scope"].as_bool())
        .collect();
    assert_eq!(in_scope, [Some(true), Some(false), Some(false)], "{data}");
    assert_eq!(data["items"][1].get("inferred"), None::<&Value>);
}
