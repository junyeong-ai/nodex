//! Configuration refused at load.

use crate::support::{init_project, nodex, scratch, write_doc};
use serde_json::Value;
use std::fs;

#[test]
fn detection_orphan_ok_kinds_typo_rejected_at_load() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[kinds]
allowed = ["generic", "skill"]

[detection]
orphan_ok_kinds = ["skll"]
"#,
    )
    .unwrap();
    let assert = nodex(tmp.path())
        .args(["query", "orphans"])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout).expect("error envelope");
    let msg = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("orphan_ok_kinds") && msg.contains("\"skll\""),
        "expected typo to surface at load, got: {msg}"
    );
}

/// A field name a document cannot spell is a config no document could ever
/// satisfy, so it is refused where the operator can fix it.
#[test]
fn a_declared_field_must_be_a_key_a_document_can_spell() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [schema]\nrequired = [\"bad: key\"]\n",
    )
    .unwrap();
    write_doc(root, "docs/bare.md", "# Bare\n");
    let output = nodex(root).arg("check").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        env.pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("bad: key")
    );
}

#[test]
fn link_pattern_naming_covers_is_rejected_at_load() {
    // `covers` is the built-in path-only coverage relation, fed
    // exclusively by the frontmatter `covers:` field. A body link
    // pattern naming it would silently attach path-only resolution to
    // a user-chosen relation name, so the config is refused before any
    // command runs — with the remediation naming the frontmatter field.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[kinds]\nallowed = [\"generic\"]\n\
         [parser]\nwikilink_enabled = true\n\
         [[parser.link_patterns]]\npattern = '@covers (\\S+)'\nrelation = \"covers\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("JSON");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(false)));
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    let msg = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        msg.contains("parser.link_patterns[0]"),
        "error names the offending block: {msg}"
    );
    assert!(
        msg.contains("frontmatter covers: field"),
        "remediation names the frontmatter field: {msg}"
    );
}

#[test]
fn kinds_allowed_empty_rejected_at_load() {
    let tmp = scratch();
    fs::write(tmp.path().join("nodex.toml"), "[kinds]\nallowed = []\n").unwrap();
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn huge_stale_days_does_not_panic() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[detection]
stale_days = 4294967295
orphan_grace_days = 4294967295
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\nreviewed: 2020-01-01\n---\n# A\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    // Pathological `stale_days` values must not panic in date arithmetic.
    nodex(tmp.path()).arg("check").assert().success();
    nodex(tmp.path())
        .args(["query", "stale"])
        .assert()
        .success();
    nodex(tmp.path())
        .args(["query", "orphans"])
        .assert()
        .success();
}

#[test]
fn invalid_naming_rule_rejected_at_config_load() {
    let tmp = scratch();
    // Invalid regex — should fail fast at Config::validate.
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[[rules.naming]]
glob = "docs/**/*.md"
pattern = "[invalid("
"#,
    )
    .unwrap();
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("rules.naming"),
        "error message should identify which rule failed"
    );
}

#[test]
fn empty_acyclic_relations_rejected_at_load() {
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("nodex.toml");
    let content = fs::read_to_string(&path).expect("nodex.toml").replace(
        "immutable_baseline = \"HEAD\"",
        "immutable_baseline = \"HEAD\"\nacyclic_relations = []",
    );
    fs::write(&path, content).expect("nodex.toml writable");
    let output = nodex(tmp.path()).arg("build").output().expect("ran");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2), "config error exits 2");
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}
