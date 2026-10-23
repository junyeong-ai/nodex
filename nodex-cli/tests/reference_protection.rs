use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

const CONFIG: &str = r#"
[kinds]
allowed = ["generic", "record"]
[statuses]
allowed = ["draft", "sealed"]
terminal = ["sealed"]
[detection]
orphan_ok_kinds = ["generic", "record"]
[rules]
immutable_baseline = "HEAD"
[[rules.body_immutable]]
name = "record"
trigger = "creation"
kinds = ["record"]
mode = "frozen"
"#;

const POLICY: &str = r#"
[[detection.unresolved_policy]]
name = "protected"
cause = "missing"
source_protection = "protected"
severity = "info"
[[detection.unresolved_policy]]
name = "missing"
cause = "missing"
severity = "error"
"#;

fn git(root: &Path, args: &[&str]) {
    let result = nodex_core::git::command(root)
        .unwrap()
        .args(["-c", "core.hooksPath=", "-c", "commit.gpgsign=false"])
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn write(root: &Path, name: &str, content: &str) {
    std::fs::write(root.join(name), content).unwrap();
}

fn record(body: &str) -> String {
    format!("---\nid: record\nkind: record\nstatus: draft\n---\n{body}")
}

fn commit(root: &Path) {
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "fixture"]);
}

fn project(config: &str, body: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "nodex.toml", &format!("{config}{POLICY}"));
    write(dir.path(), ".gitignore", "_index/\n");
    write(dir.path(), "record.md", &record(body));
    git(dir.path(), &["init", "-q"]);
    commit(dir.path());
    dir
}

fn run(root: &Path, args: &[&str], content: Option<String>, code: i32) -> Value {
    let mut command = Command::cargo_bin("nodex").unwrap();
    command.arg("-C").arg(root).args(args);
    if let Some(content) = content {
        command.write_stdin(content);
    }
    let result = command.assert().code(code);
    let envelope: Value = serde_json::from_slice(&result.get_output().stdout).unwrap();
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope["data"].clone()
}

fn issues(root: &Path) -> Value {
    run(root, &["build"], None, 0);
    run(root, &["query", "issues"], None, 0)
}

#[test]
fn protection_reports_constraints_without_claiming_historical_validity() {
    let dir = project(CONFIG, "[deleted](gone.md)\n[typo](typo.md)\n");
    let root = dir.path();
    write(root, "gone.md", "---\nid: target\n---\nTarget\n");
    commit(root);
    std::fs::remove_file(root.join("gone.md")).unwrap();
    commit(root);

    let check = run(root, &["check"], None, 0);
    assert!(check["violations"].as_array().unwrap().is_empty());
    let report = issues(root);
    assert_eq!(report["unresolved_edges"].as_array().unwrap().len(), 2);
    for edge in report["unresolved_edges"].as_array().unwrap() {
        assert_eq!(edge["cause"], "missing");
        assert_eq!(edge["severity"], "info");
        assert_eq!(edge["source_protection"], "protected");
        assert_eq!(edge["sources"][0]["rules"][0], "body_immutable/record");
    }
    write(root, "nodex.toml", CONFIG);
    let check = run(root, &["check"], None, 0);
    assert_eq!(check["violations"].as_array().unwrap().len(), 2);
    assert!(
        check["violations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["severity"] == "warning")
    );
}

#[test]
fn informational_references_do_not_release_body_locks() {
    let dir = project(CONFIG, "[missing](missing.md)\n");
    write(dir.path(), "target.md", "---\nid: target\n---\nTarget\n");
    let data = run(
        dir.path(),
        &["check", "--content", "record.md=-"],
        Some(record("[fixed](target.md)\n")),
        1,
    );
    assert!(
        data["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["rule_id"] == "body_immutable/record")
    );
}

#[test]
fn appended_occurrences_do_not_inherit_the_protection_of_duplicate_relations() {
    let config = CONFIG.replace("frozen", "append_only");
    let dir = project(&config, "[original](same.md)\n[other](old.md)\n");
    let root = dir.path();
    let proposed =
        record("[original](same.md)\n[other](old.md)\n\n[again](same.md)\n[new](new.md)\n");
    let data = run(
        root,
        &["check", "--content", "record.md=-"],
        Some(proposed.clone()),
        1,
    );
    assert_eq!(data["violations"].as_array().unwrap().len(), 2);
    write(root, "record.md", &proposed);
    let report = issues(root);
    let same = report["unresolved_edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["raw_target"] == "same.md")
        .unwrap();
    assert_eq!(same["severity"], "error");
    assert_eq!(same["source_protection"], "unprotected");
    assert_eq!(same["sources"].as_array().unwrap().len(), 2);
    assert_eq!(same["sources"][0]["protection"], "protected");
    assert_eq!(same["sources"][1]["protection"], "unprotected");
    let old = report["unresolved_edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["raw_target"] == "old.md")
        .unwrap();
    assert_eq!(old["severity"], "info");
    let node = run(root, &["query", "node", "record"], None, 0);
    assert_eq!(node["outgoing"].as_array().unwrap().len(), 3);
}

/// A citation is protected by its place in the committed body, so a field
/// added above the body moves it down the file without moving it out.
#[test]
fn a_committed_citation_stays_protected_below_a_grown_frontmatter() {
    let dir = project(
        &CONFIG.replace("frozen", "append_only"),
        "[original](same.md)\n",
    );
    let root = dir.path();
    write(
        root,
        "record.md",
        "---\nid: record\nkind: record\nstatus: draft\nowner: team\n---\n[original](same.md)\n[again](same.md)\n",
    );
    let report = issues(root);
    let same = report["unresolved_edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["raw_target"] == "same.md")
        .unwrap();
    let sources: Vec<(u64, &str)> = same["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            (
                source["reference"]["location"]["start_line"]
                    .as_u64()
                    .unwrap(),
                source["protection"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(sources, [(7, "protected"), (8, "unprotected")]);
}

#[test]
fn appended_markdown_dependencies_are_new_citations() {
    let config = CONFIG.replace("frozen", "append_only");
    for (before, after) in [
        ("[label][key]\n", "[label][key]\n\n[key]: missing.md\n"),
        ("[label](\n", "[label](\nmissing.md)\n"),
    ] {
        let dir = project(&config, before);
        write(dir.path(), "record.md", &record(after));
        let data = run(dir.path(), &["check"], None, 1);
        let findings = data["violations"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "{data}");
        assert_eq!(findings[0]["rule_id"], "unresolved_reference/missing");
        assert_eq!(findings[0]["details"]["source_protection"], "unprotected");
    }
}

#[test]
fn appended_code_span_delimiters_do_not_protect_new_pattern_citations() {
    let config = format!(
        r#"{}
[[parser.link_patterns]]
pattern = '(?m)^([a-z]+\.md)$'
relation = "references"
code_spans = true
"#,
        CONFIG.replace("frozen", "append_only")
    );
    let dir = project(&config, "`missing.md\n");
    write(dir.path(), "record.md", &record("`missing.md\n`\n"));
    let data = run(dir.path(), &["check"], None, 1);
    assert_eq!(
        data["violations"][0]["details"]["source_protection"],
        "unprotected"
    );
}

#[test]
fn appended_superseded_citations_are_checked_even_when_a_prior_citation_is_protected() {
    let config = CONFIG.replace("frozen", "append_only");
    let dir = project(&config, "[original](old.md)\n");
    let root = dir.path();
    write(root, "old.md", "---\nid: old\nstatus: sealed\n---\nOld\n");
    write(
        root,
        "new.md",
        "---\nid: new\nstatus: draft\nsupersedes: [old]\n---\nNew\n",
    );
    commit(root);
    let text = record("[original](old.md)\n\n[again](old.md)\n");
    write(root, "record.md", &text);
    let data = run(root, &["check"], None, 0);
    assert_eq!(data["violations"].as_array().unwrap().len(), 1);
    let finding = &data["violations"][0];
    assert_eq!(finding["rule_id"], "superseded_reference");
    assert_eq!(finding["details"]["sources"][0]["protection"], "protected");
    assert_eq!(
        finding["details"]["sources"][1]["protection"],
        "unprotected"
    );
}

#[test]
fn missing_baselines_remain_unknown_and_first_activation_is_unprotected() {
    let config = CONFIG.replace("immutable_baseline = \"HEAD\"", "");
    let dir = project(&config, "[missing](missing.md)\n");
    let data = run(dir.path(), &["check"], None, 1);
    assert_eq!(
        data["violations"][0]["details"]["source_protection"],
        "unknown"
    );

    let config = CONFIG.replace(
        "trigger = \"creation\"",
        "trigger = \"status\"\nstatuses = [\"sealed\"]",
    );
    let dir = project(&config, "[missing](missing.md)\n");
    let proposed = record("[missing](different.md)\n").replace("status: draft", "status: sealed");
    let data = run(
        dir.path(),
        &["check", "--content", "record.md=-"],
        Some(proposed),
        1,
    );
    assert_eq!(data["violations"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["violations"][0]["details"]["source_protection"],
        "unprotected"
    );
}

#[test]
fn commit_steps_protect_records_created_after_the_selected_baseline() {
    let config = CONFIG.replace(
        "immutable_baseline = \"HEAD\"",
        "immutable_baseline = \"anchor\"",
    );
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    write(root, "nodex.toml", &format!("{config}{POLICY}"));
    write(root, ".gitignore", "_index/\n");
    write(root, "index.md", "Index\n");
    commit(root);
    git(root, &["tag", "anchor"]);
    write(root, "record.md", &record("[missing](missing.md)\n"));
    commit(root);
    run(root, &["check"], None, 0);
    let snapshot = config.replace("[rules]", "[rules]\nimmutable_history = \"snapshot\"");
    write(root, "nodex.toml", &format!("{snapshot}{POLICY}"));
    let data = run(root, &["check"], None, 1);
    assert_eq!(
        data["violations"][0]["details"]["source_protection"],
        "unprotected"
    );
}

#[test]
fn staged_protection_reads_the_index_instead_of_unstaged_citations() {
    let config = CONFIG.replace("frozen", "append_only");
    let dir = project(&config, "[original](missing.md)\n");
    write(
        dir.path(),
        "record.md",
        &record("[original](missing.md)\n\n[new](new.md)\n"),
    );
    run(dir.path(), &["check", "--staged"], None, 0);
    run(dir.path(), &["check"], None, 1);
}

#[test]
fn renaming_a_target_uses_the_same_protection_policy_as_check() {
    let dir = project(CONFIG, "[target](target.md)\n");
    let root = dir.path();
    write(root, "target.md", "---\nid: target\n---\nTarget\n");
    commit(root);
    run(root, &["rename", "target.md", "moved.md"], None, 0);
    assert!(root.join("moved.md").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("record.md")).unwrap(),
        record("[target](target.md)\n")
    );
    run(root, &["check"], None, 0);
    let report = issues(root);
    assert_eq!(
        report["unresolved_edges"][0]["source_protection"],
        "protected"
    );
    assert_eq!(report["unresolved_edges"][0]["severity"], "info");
}

#[test]
fn frontmatter_protection_follows_the_authored_field() {
    let config = format!(
        r#"{CONFIG}
[[parser.frontmatter_relations]]
field = "sources"
relation = "derived_from"
retarget = "keep"
[[rules.frontmatter_immutable]]
name = "sources"
trigger = "creation"
fields = ["sources"]
[[detection.unresolved_policy]]
name = "protected-id"
cause = "id_not_found"
source_protection = "protected"
severity = "info"
[[detection.unresolved_policy]]
name = "id"
cause = "id_not_found"
severity = "error"
"#
    );
    let dir = project(&config, "[body](body.md)\n");
    let text = record("[body](body.md)\n").replace(
        "status: draft",
        "status: draft\nsources: [missing-source]\nrelated: [missing-related]",
    );
    write(dir.path(), "record.md", &text);
    commit(dir.path());
    let report = issues(dir.path());
    let edges = report["unresolved_edges"].as_array().unwrap();
    let source = edges
        .iter()
        .find(|e| e["relation"] == "derived_from")
        .unwrap();
    assert_eq!(source["severity"], "info");
    assert_eq!(
        source["sources"][0]["reference"]["location"]["field"],
        "sources"
    );
    assert_eq!(
        source["sources"][0]["rules"][0],
        "frontmatter_immutable/sources"
    );
    let related = edges.iter().find(|e| e["relation"] == "related").unwrap();
    assert_eq!(related["severity"], "error");
    assert_eq!(related["source_protection"], "unprotected");
    run(dir.path(), &["check"], None, 1);
}
