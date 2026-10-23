//! `migrate`.

use crate::support::{
    init_project, nodex, run_envelope, run_json, scratch, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;

#[test]
fn migrate_fills_required_fields_under_strict_schema() {
    // `migrate --apply` walks `required_for(kind)` + `cross_field_for(kind)`
    // through scaffold's shared frontmatter generator, so the injected
    // frontmatter passes the same schema rules the document will be
    // checked against.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[schema]
required = ["decision_date"]
types = { decision_date = "date" }
"#,
    )
    .unwrap();
    write_doc(tmp.path(), "bare.md", "# Bare Doc\nBody.\n");

    nodex(tmp.path())
        .args(["migrate", "--apply"])
        .assert()
        .success();

    let content = fs::read_to_string(tmp.path().join("bare.md")).unwrap();
    assert!(
        content.contains("decision_date:"),
        "required field must be written; got:\n{content}"
    );

    // Build + check should not flag any violation on the migrated doc.
    nodex(tmp.path()).arg("build").assert().success();
    let check = run_json(nodex(tmp.path()).arg("check"));
    assert_eq!(
        check.get("has_errors").and_then(Value::as_bool),
        Some(false),
        "migrated doc should pass check; got: {check}"
    );
}

#[test]
fn migrate_does_not_double_inject_a_bom_crlf_document() {
    // A BOM+CRLF file authored outside nodex already has frontmatter; the
    // build parser canonicalizes before splitting, and migrate must too —
    // otherwise it misreads the file as bare and injects a duplicate
    // frontmatter block.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    fs::create_dir_all(root.join("docs")).unwrap();
    let original = "\u{FEFF}---\r\nid: doc-crlf\r\ntitle: CRLF\r\nkind: generic\r\nstatus: active\r\n---\r\n# Body\r\n";
    fs::write(root.join("docs/crlf.md"), original).unwrap();

    let env = run_envelope(nodex(root).args(["migrate", "--apply"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    // The doc already had frontmatter → nothing migrated.
    assert_eq!(
        env.pointer("/data/total").and_then(Value::as_u64),
        Some(0),
        "a doc with frontmatter must not be migrated: {env}"
    );
    // A skipped doc is left byte-identical — no duplicate frontmatter
    // block injected on top of the (BOM+CRLF) original.
    assert_eq!(
        fs::read_to_string(root.join("docs/crlf.md")).unwrap(),
        original,
        "skipped doc must be left untouched"
    );

    nodex(root).arg("build").assert().success();
}

#[test]
fn migrate_skips_unclosed_fence_file_with_a_warning() {
    // A file that opens a frontmatter fence and never closes it is
    // neither bare nor parseable — injecting frontmatter would bury the
    // malformed block in the body. Migrate skips it with a per-file
    // warning and leaves the bytes untouched; the file itself reds
    // `check` via the `parse_failure` rule.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    fs::create_dir_all(root.join("docs")).unwrap();
    let original = "---\nid: half-open\n# never closed\n";
    fs::write(root.join("docs/half.md"), original).unwrap();
    write_doc(root, "docs/bare.md", "# Bare Doc\nBody.\n");

    let env = run_envelope(nodex(root).args(["migrate", "--apply"]));
    assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        env.pointer("/data/total").and_then(Value::as_u64),
        Some(1),
        "only the genuinely bare doc is migrated: {env}"
    );
    let warnings: Vec<&str> = env
        .get("warnings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(warning_msg).collect())
        .unwrap_or_default();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("docs/half.md") && w.contains("unclosed frontmatter fence")),
        "the skip names the file and the cause: {warnings:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/half.md")).unwrap(),
        original,
        "no frontmatter is injected into the malformed file"
    );
}

#[test]
fn migrate_rejects_self_collision_between_bare_files() {
    // Two bare files in distinct directories both infer the same id
    // (`{kind}-{stem}` template, both stems = "foo"). Migrating both
    // would write the same `id:` to both → next build fails with
    // DUPLICATE_ID. The atomic refuse must surface DUPLICATE_ID at
    // exit 2 and leave both files byte-for-byte unchanged.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
    let bare = "# Foo\n\nbody\n";
    write_doc(tmp.path(), "docs/foo.md", bare);
    write_doc(tmp.path(), "specs/foo.md", bare);

    let output = nodex(tmp.path())
        .args(["migrate", "--apply"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("DUPLICATE_ID")
    );
    let docs_foo = fs::read_to_string(tmp.path().join("docs/foo.md")).unwrap();
    let specs_foo = fs::read_to_string(tmp.path().join("specs/foo.md")).unwrap();
    assert_eq!(docs_foo, bare, "first bare file must remain untouched");
    assert_eq!(specs_foo, bare, "second bare file must remain untouched");
}

#[test]
fn migrate_rejects_collision_against_existing_explicit_id() {
    // A bare file's inferred id collides with an existing file's
    // explicit id. Same DUPLICATE_ID atomic refuse contract.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/existing.md",
        "---\nid: generic-bare\ntitle: Existing\nkind: generic\nstatus: active\n---\n# Existing\n",
    );
    let bare = "# Bare\n\nbody\n";
    write_doc(tmp.path(), "docs/bare.md", bare);

    let output = nodex(tmp.path())
        .args(["migrate", "--apply"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("json envelope");
    assert_eq!(
        env.pointer("/error/code").and_then(Value::as_str),
        Some("DUPLICATE_ID")
    );
    let after = fs::read_to_string(tmp.path().join("docs/bare.md")).unwrap();
    assert_eq!(
        after, bare,
        "rejected migration must leave the bare file untouched"
    );
}

#[test]
fn migrate_apply_succeeds_when_inferred_ids_are_distinct() {
    // Sanity contract for the happy path: distinct stems → distinct
    // inferred ids → migration writes frontmatter and the resulting
    // graph builds without a DUPLICATE_ID surface. Lock-in so a
    // future tightening of the collision check doesn't accidentally
    // reject valid input.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["**/*.md"]

[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
"#,
    )
    .unwrap();
    write_doc(tmp.path(), "docs/alpha.md", "# Alpha\n");
    write_doc(tmp.path(), "docs/beta.md", "# Beta\n");

    let data = run_json(nodex(tmp.path()).args(["migrate", "--apply"]));
    assert_eq!(data.get("total").and_then(Value::as_u64), Some(2));
    nodex(tmp.path()).arg("build").assert().success();
}

#[cfg(unix)]
#[test]
fn migrate_write_failure_preserves_partial_results_and_exits_nonzero() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/good.md", "# Good\n");
    write_doc(tmp.path(), "blocked/bad.md", "# Bad\n");
    let blocked = tmp.path().join("blocked");
    let permissions = fs::metadata(&blocked).unwrap().permissions();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o555)).unwrap();
    let output = nodex(tmp.path())
        .args(["migrate", "--apply"])
        .output()
        .unwrap();
    fs::set_permissions(&blocked, permissions).unwrap();

    assert_eq!(output.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    assert!(envelope.get("error").is_none());
    let data = &envelope["data"];
    assert_eq!(data["completion"], "partial");
    assert_eq!(data["total"], 1);
    assert_eq!(data["changes"][0]["path"], "docs/good.md");
    assert_eq!(data["failures"].as_array().unwrap().len(), 1);
    assert_eq!(data["failures"][0]["path"], "blocked/bad.md");
    assert_eq!(data["failures"][0]["code"], "IO_ERROR");
    assert!(
        fs::read_to_string(tmp.path().join("docs/good.md"))
            .unwrap()
            .starts_with("---\n")
    );
    assert_eq!(
        fs::read_to_string(blocked.join("bad.md")).unwrap(),
        "# Bad\n"
    );
    assert_eq!(fs::read_dir(&blocked).unwrap().count(), 1);
    nodex(tmp.path())
        .args(["migrate", "--apply"])
        .assert()
        .success();
    assert!(
        fs::read_to_string(blocked.join("bad.md"))
            .unwrap()
            .starts_with("---\n")
    );
}

#[cfg(unix)]
#[test]
fn migrate_apply_warns_and_skips_unreadable_file_instead_of_aborting() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/good.md", "# Good\nBody.\n");
    write_doc(tmp.path(), "docs/blocked.md", "# Blocked\nBody.\n");
    fs::set_permissions(
        tmp.path().join("docs/blocked.md"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let envelope = run_envelope(nodex(tmp.path()).args(["migrate", "--apply"]));
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings present");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("could not read in-scope file docs/blocked.md")),
        "the unreadable file rides the warnings array: {warnings:?}"
    );
    let changes = envelope
        .pointer("/data/changes")
        .and_then(Value::as_array)
        .expect("changes array");
    assert_eq!(changes.len(), 1, "only the readable file was migrated");
    assert!(
        fs::read_to_string(tmp.path().join("docs/good.md"))
            .unwrap()
            .starts_with("---\n"),
        "the readable sibling was still migrated"
    );

    // Restore permissions so the tempdir can be cleaned up.
    fs::set_permissions(
        tmp.path().join("docs/blocked.md"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
}
