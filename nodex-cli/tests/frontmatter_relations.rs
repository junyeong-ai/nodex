use assert_cmd::Command;
use serde_json::{Value, json};
use std::path::Path;
use tempfile::TempDir;

const CONFIG: &str = r#"
[scope]
include = ["docs/**/*.md"]
[kinds]
allowed = ["generic", "record", "guide"]
[statuses]
allowed = ["active", "retired"]
terminal = ["retired"]
[rules]
immutable_baseline = "HEAD"
acyclic_relations = ["derived_from"]
[detection]
orphan_ok_kinds = ["generic", "record", "guide"]
[[parser.frontmatter_relations]]
field = "sources"
relation = "derived_from"
retarget = "keep"
"#;

fn project(config: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("nodex.toml"), config).unwrap();
    write(dir.path(), "a.md", "a", "", "");
    write(dir.path(), "b.md", "b", "", "");
    dir
}

fn write(root: &Path, file: &str, id: &str, fields: &str, body: &str) {
    std::fs::write(
        root.join("docs").join(file),
        format!("---\nid: {id}\nkind: record\nstatus: active\n{fields}---\n{body}"),
    )
    .unwrap();
}

fn run(root: &Path, args: &[&str], code: i32) -> Value {
    let result = Command::cargo_bin("nodex")
        .unwrap()
        .arg("-C")
        .arg(root)
        .args(args)
        .assert()
        .code(code);
    let envelope: Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(envelope["ok"], code != 2, "{envelope}");
    envelope
}

fn git(root: &Path, args: &[&str]) {
    let output = nodex_core::git::command(root)
        .unwrap()
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn declared_relations_are_visible_in_graph_queries_and_schema() {
    let dir = project(CONFIG);
    let root = dir.path();
    write(root, "source.md", "source", "sources: [a, b]\n", "");
    run(root, &["build"], 0);
    let node = run(root, &["query", "node", "source"], 0);
    assert_eq!(
        node["data"]["node"]["frontmatter_relations"]["sources"],
        json!(["a", "b"])
    );
    assert!(node["data"]["node"]["attrs"].get("sources").is_none());
    assert_eq!(node["data"]["outgoing"].as_array().unwrap().len(), 2);
    let backlinks = run(root, &["query", "backlinks", "a"], 0);
    assert_eq!(backlinks["data"]["items"][0]["relation"], "derived_from");
    let dependents = run(
        root,
        &["query", "dependents", "a", "--relations", "derived_from"],
        0,
    );
    assert_eq!(dependents["data"]["dependents"][0]["id"], "source");
    let references = run(
        root,
        &["query", "dependents", "a", "--relations", "references"],
        0,
    );
    assert!(
        references["data"]["dependents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let neighborhood = run(root, &["query", "neighborhood", "a"], 0);
    assert!(
        neighborhood["data"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == "source")
    );
    let rejected = run(root, &["query", "nodes", "--where", "sources=a"], 2);
    assert_eq!(rejected["error"]["code"], "CONFIG_ERROR");
    let missing = run(root, &["query", "nodes", "--missing", "sources"], 0);
    assert_eq!(missing["data"]["total"], 2);
    let projection = run(
        root,
        &[
            "query",
            "nodes",
            "--where",
            "id=source",
            "--fields",
            "id,sources",
        ],
        0,
    );
    assert_eq!(
        projection["data"]["items"][0]["attrs"]["sources"],
        json!(["a", "b"])
    );
    let manifest = run(root, &["export", "schema"], 0);
    let validator = jsonschema::validator_for(&manifest["data"]).unwrap();
    for value in [json!("a"), json!(["a", "b"])] {
        assert!(
            validator
                .is_valid(&json!({"id":"x", "kind":"record", "status":"active", "sources":value}))
        );
    }
    assert!(!validator.is_valid(&json!({"kind":"record", "status":"active", "sources":3})));
    let config = run(root, &["export", "config"], 0);
    assert!(config["data"].to_string().contains("frontmatter_relations"));
    let envelope_schema = run(root, &["export", "envelope-schema"], 0);
    let schema = &envelope_schema["data"]["per_command"]["query.node"];
    assert!(
        jsonschema::validator_for(schema)
            .unwrap()
            .is_valid(&node["data"])
    );
}

#[test]
fn id_resolution_never_uses_a_namesake_path_and_policy_classifies_the_relation() {
    let config = format!(
        "{CONFIG}\n[[detection.unresolved_policy]]\nname = 'source_missing'\ncause = 'id_not_found'\nrelations = ['derived_from']\nseverity = 'error'\n"
    );
    let dir = project(&config);
    let root = dir.path();
    write(root, "collision.md", "different-id", "", "");
    write(root, "source.md", "source", "sources: collision\n", "");
    let result = run(root, &["check"], 1);
    assert!(
        result["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["rule_id"] == "unresolved_reference/source_missing")
    );
    run(root, &["build"], 0);
    let issues = run(root, &["query", "issues"], 0);
    let edge = &issues["data"]["unresolved_edges"][0];
    assert_eq!(edge["relation"], "derived_from");
    assert_eq!(edge["cause"], "id_not_found");
    assert_eq!(edge["policy_name"], "source_missing");
}

#[test]
fn retarget_policies_preserve_fields_independently_of_body_rewrites() {
    for policy in ["keep", "repoint"] {
        let dir =
            project(&CONFIG.replace("retarget = \"keep\"", &format!("retarget = \"{policy}\"")));
        let root = dir.path();
        write(
            root,
            "source.md",
            "source",
            "sources: [a, b] # provenance\n",
            "[a](a.md)\n",
        );
        write(root, "retained.md", "retained", "sources: a\n", "");
        write(root, "b.md", "b", "sources: a\n", "");
        let retained = std::fs::read(root.join("docs/retained.md")).unwrap();
        let successor = std::fs::read(root.join("docs/b.md")).unwrap();
        let before = std::fs::read_to_string(root.join("docs/source.md")).unwrap();
        let result = run(root, &["retarget", "a", "b"], 0);
        assert_eq!(std::fs::read(root.join("docs/b.md")).unwrap(), successor);
        if policy == "keep" {
            assert_eq!(
                std::fs::read(root.join("docs/retained.md")).unwrap(),
                retained
            );
        }
        let after = std::fs::read_to_string(root.join("docs/source.md")).unwrap();
        assert!(after.contains("[a](b.md)"));
        if policy == "keep" {
            assert!(after.contains("sources: [a, b] # provenance\n"));
            assert!(
                result["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|w| w["code"] == "reference_kept")
            );
        } else {
            assert_ne!(after, before);
        }
        run(root, &["build"], 0);
        let node = run(root, &["query", "node", "source"], 0);
        assert_eq!(
            node["data"]["node"]["frontmatter_relations"]["sources"],
            if policy == "keep" {
                json!(["a", "b"])
            } else {
                json!(["b"])
            }
        );
    }
}

#[test]
fn required_forbidden_and_field_parse_checks_use_the_declared_field() {
    let config = format!(
        "{CONFIG}\n[schema]\nmode = 'strict'\n[[schema.overrides]]\nkinds = ['record']\nrequired = ['sources']\n[[schema.overrides]]\nkinds = ['guide']\nforbidden = ['sources']\n"
    );
    let dir = project(&config);
    let root = dir.path();
    write(root, "a.md", "a", "sources: b\n", "");
    write(root, "b.md", "b", "", "");
    write(root, "bad.md", "bad", "sources: [a, 3]\n", "");
    let guide = "---\nid: guide\nkind: guide\nstatus: active\nsources: a\n---\n";
    std::fs::write(root.join("docs/guide.md"), guide).unwrap();
    let result = run(root, &["check"], 1);
    let violations = result["data"]["violations"].as_array().unwrap();
    assert!(
        violations
            .iter()
            .any(|v| v["rule_id"] == "required_field" && v["node_id"] == "b")
    );
    assert!(
        violations
            .iter()
            .any(|v| v["rule_id"] == "field_parse" && v["node_id"] == "bad")
    );
    assert!(
        violations
            .iter()
            .any(|v| v["rule_id"] == "forbidden_field" && v["node_id"] == "guide")
    );
    assert!(!violations.iter().any(|v| v["rule_id"] == "unknown_field"));
    let manifest = run(root, &["export", "schema"], 0);
    assert!(
        !jsonschema::validator_for(&manifest["data"])
            .unwrap()
            .is_valid(&json!({"kind":"guide", "status":"active", "sources":"a"}))
    );
    let scaffold = run(
        root,
        &[
            "scaffold",
            "--kind",
            "record",
            "--title",
            "Created",
            "--path",
            "docs/created.md",
            "--field",
            "sources=a",
        ],
        0,
    );
    assert!(scaffold["ok"].as_bool().unwrap());
}

#[test]
fn relation_fields_participate_in_history_diff_impact_and_immutability() {
    let config = format!(
        "{CONFIG}\n[[rules.frontmatter_immutable]]\nname = 'provenance'\ntrigger = 'creation'\nfields = ['sources']\n"
    );
    let dir = project(&config);
    let root = dir.path();
    write(root, "source.md", "source", "sources: a\n", "");
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);
    write(root, "source.md", "source", "sources: b\n", "");
    let check = run(root, &["check"], 1);
    assert!(
        check["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["rule_id"] == "frontmatter_immutable/provenance")
    );
    git(root, &["add", "docs/source.md"]);
    git(root, &["commit", "-qm", "relations"]);
    let diff = run(root, &["diff", "HEAD~1", "HEAD"], 0);
    assert_eq!(diff["data"]["added_edges"][0]["relation"], "derived_from");
    assert!(
        diff["data"]["field_changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "sources"
                && f["before"] == json!(["a"])
                && f["after"] == json!(["b"]))
    );
    write(root, "b.md", "b", "", "changed\n");
    git(root, &["add", "docs/b.md"]);
    git(root, &["commit", "-qm", "body"]);
    let impact = run(
        root,
        &["impact", "HEAD~1", "HEAD", "--relations", "derived_from"],
        0,
    );
    assert_eq!(
        impact["data"]["impacted"][0]["dependents"][0]["id"],
        "source"
    );
}

#[test]
fn historical_relation_exemptions_are_independent_of_retarget_policy() {
    for exempt in [false, true] {
        let config = CONFIG.replace(
            "orphan_ok_kinds =",
            &format!(
                "superseded_reference_ok_relations = {}\norphan_ok_kinds =",
                if exempt { "['derived_from']" } else { "[]" }
            ),
        );
        let dir = project(&config);
        let root = dir.path();
        std::fs::write(
            root.join("docs/a.md"),
            "---\nid: a\nkind: record\nstatus: retired\n---\n",
        )
        .unwrap();
        write(root, "b.md", "b", "supersedes: [a]\n", "");
        write(root, "source.md", "source", "sources: a\n", "");
        let result = run(root, &["check"], 0);
        let warnings: Vec<_> = result["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["rule_id"] == "superseded_reference")
            .collect();
        assert_eq!(warnings.len(), usize::from(!exempt));
    }
}

#[test]
fn locked_custom_fields_are_distinct_from_unlocked_body_citations() {
    let config = format!(
        "{CONFIG}\n[[rules.frontmatter_immutable]]\nname = 'provenance'\ntrigger = 'creation'\nfields = ['sources']\n"
    );
    let dir = project(&config.replace("retarget = \"keep\"", "retarget = \"repoint\""));
    let root = dir.path();
    std::fs::write(
        root.join("docs/a.md"),
        "---\nid: a\nkind: record\nstatus: retired\n---\n",
    )
    .unwrap();
    write(root, "b.md", "b", "supersedes: [a]\n", "");
    write(root, "source.md", "source", "sources: a\n", "[old](a.md)\n");
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);
    let result = run(root, &["check"], 0);
    let citations: Vec<_> = result["data"]["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["rule_id"] == "superseded_reference")
        .collect();
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0]["details"]["relation"], "references");
    let result = run(root, &["retarget", "a", "b"], 0);
    assert!(
        result["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "file_skipped")
    );
    let content = std::fs::read_to_string(root.join("docs/source.md")).unwrap();
    assert!(content.contains("sources: a\n"));
    assert!(content.contains("[old](b.md)"));
}

#[test]
fn retarget_refuses_custom_relation_cycles_before_writing() {
    let dir = project(&CONFIG.replace("retarget = \"keep\"", "retarget = \"repoint\""));
    let root = dir.path();
    write(root, "source.md", "source", "sources: a\n", "");
    write(root, "b.md", "b", "sources: source\n", "");
    let before = std::fs::read(root.join("docs/source.md")).unwrap();
    let result = run(root, &["retarget", "a", "b"], 2);
    assert_eq!(result["error"]["code"], "CONTENT_VIOLATIONS");
    assert_eq!(std::fs::read(root.join("docs/source.md")).unwrap(), before);
}

#[test]
fn deleting_a_namesake_path_does_not_break_an_unresolved_id_relation() {
    let dir = project(CONFIG);
    let root = dir.path();
    write(root, "source.md", "source", "sources: a.md\n", "");
    git(root, &["init", "-q"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);
    git(root, &["rm", "docs/a.md"]);
    git(root, &["commit", "-qm", "remove"]);
    let result = run(
        root,
        &["impact", "HEAD~1", "HEAD", "--relations", "derived_from"],
        0,
    );
    assert!(
        result["data"]["likely_breaking"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(result["data"]["impacted"].as_array().unwrap().is_empty());
}
