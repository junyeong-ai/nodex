//! `scaffold`.

use crate::support::{
    envelope_of, init_project, nodex, run_envelope, run_json, scratch, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;

#[test]
fn scaffold_uses_explicit_creation_templates_and_refuses_glob_inversion() {
    let tmp = scratch();
    let root = tmp.path();
    let config = "[scope]\ninclude = [\"specs/**/*.md\"]\n[kinds]\nallowed = [\"generic\", \"spec\"]\n[[identity.kind_rules]]\nglob = \"specs/*/spec.md\"\nkind = \"spec\"\n[[identity.id_rules]]\nkind = \"spec\"\ntemplate = \"spec-{parent}\"\n";
    fs::write(root.join("nodex.toml"), config).unwrap();
    let result = nodex(root)
        .args(["scaffold", "--kind", "spec", "--title", "Payment policy"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(error["error"]["code"], "CONFIG_ERROR");
    assert!(!root.join("specs/payment-policy.md").exists());
    fs::write(
        root.join("nodex.toml"),
        format!("{config}\n[scaffold.paths]\nspec = \"specs/{{slug}}/spec.md\"\n"),
    )
    .unwrap();
    let result =
        run_json(nodex(root).args(["scaffold", "--kind", "spec", "--title", "Payment policy"]));
    assert_eq!(result["path"], "specs/payment-policy/spec.md");
    assert_eq!(result["id"], "spec-payment-policy");
    nodex(root).arg("check").assert().success();
}

#[test]
fn scaffold_dry_run_does_not_write_and_returns_plan() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Hello"])
            .args(["--path", "misc/hello.md", "--dry-run"]),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(false));
    assert!(data.get("id").and_then(Value::as_str).is_some());
    assert!(!tmp.path().join("misc/hello.md").exists());
}

#[test]
fn scaffold_writes_file_on_non_dry_run() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Written"])
            .args(["--path", "docs/written.md"]),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    assert!(tmp.path().join("docs/written.md").exists());
    // Frontmatter round-trips through YAML parser (no Debug-escape drift).
    let content = fs::read_to_string(tmp.path().join("docs/written.md")).unwrap();
    assert!(content.contains("title: \"Written\""));
}

#[test]
fn scaffold_respects_global_schema_type_default() {
    // A top-level `[schema] types = { priority = "integer" }` (no
    // per-kind override) flows through scaffold's defaults so the
    // generated frontmatter passes the same `FieldTypeRule` it would
    // be checked against.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]

[[identity.kind_rules]]
glob = "docs/**"
kind = "guide"

[[identity.id_rules]]
kind = "guide"
template = "guide-{stem}"

[schema]
required = ["priority"]
types = { priority = "integer" }
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "guide", "--title", "Test"])
            .args(["--path", "docs/test.md"]),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    let content = fs::read_to_string(tmp.path().join("docs/test.md")).unwrap();
    assert!(
        content.contains("priority: 0"),
        "expected integer default, got:\n{content}"
    );

    // Subsequent build + check should not flag the scaffolded file.
    nodex(tmp.path()).arg("build").assert().success();
    let check_data = run_json(nodex(tmp.path()).arg("check"));
    assert_eq!(
        check_data.get("has_errors").and_then(Value::as_bool),
        Some(false),
        "check should find no errors; got: {check_data}"
    );
}

#[test]
fn scaffold_rejects_existing_without_force() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/exists.md", "existing content");
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Clash"])
        .args(["--path", "docs/exists.md"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let parsed: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON");
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("ALREADY_EXISTS"),
        "existing scaffold target classified as ALREADY_EXISTS, not CONFIG_ERROR"
    );
}

#[test]
fn scaffold_with_force_overwrites() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(tmp.path(), "docs/ow.md", "existing content");
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Overwritten"])
        .args(["--path", "docs/ow.md", "--force"])
        .assert()
        .success();
    let content = fs::read_to_string(tmp.path().join("docs/ow.md")).unwrap();
    assert!(content.contains("title: \"Overwritten\""));
}

#[test]
fn scaffold_rejects_non_md_extension() {
    let tmp = scratch();
    init_project(tmp.path());
    nodex(tmp.path()).arg("build").assert().success();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/wrong.txt"])
        .output()
        .expect("ran");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("JSON");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(false)));
    assert_eq!(
        parsed.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
}

#[test]
fn scaffold_satisfies_cross_field_keyed_on_its_own_defaults() {
    // The self-consistency invariant at its hardest: a `cross_field`
    // whose `when` fires on a value scaffold ITSELF defaults (a required
    // enum field) must still get its `require` field written. The
    // renderer reparses the frontmatter-as-written and iterates to a
    // fixpoint, and validation runs over the full overlay graph, so
    // scaffold and `check` agree by construction.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
[kinds]
allowed = ["generic", "adr"]
[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
[schema]
required = ["component"]
enums = { component = ["auth", "billing"], severity = ["low", "high"] }
cross_field = [{ when = "component exists", require = "severity" }]
"#,
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    nodex(root)
        .args(["scaffold", "--kind", "adr", "--title", "X"])
        .args(["--path", "docs/x.md"])
        .assert()
        .success();
    let written = fs::read_to_string(root.join("docs/x.md")).unwrap();
    assert!(
        written.contains("component:") && written.contains("severity:"),
        "the cross_field require keyed on a defaulted field is written:\n{written}"
    );
    // The whole point: the tool's own check passes its own output.
    nodex(root).arg("build").assert().success();
    nodex(root).arg("check").assert().success();
}

#[test]
fn scaffold_refuses_a_target_outside_the_project_scope() {
    // A scaffolded file the scan would never admit is a document the
    // graph can never see — written once, invisible forever. The write
    // seam probes the same scope authority the build uses and refuses.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Loose"])
        .args(["--path", "notes/loose.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        !root.join("notes/loose.md").exists(),
        "refused scaffold must write nothing"
    );

    // The same doc inside scope scaffolds fine.
    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "Kept"])
        .args(["--path", "docs/kept.md"])
        .assert()
        .success();
}

#[test]
fn scaffold_refuses_a_filename_its_own_naming_rule_would_reject() {
    // Self-consistency: scaffold derives the filename from the title, but
    // a non-sequential `rules.naming` pattern the slug can't satisfy would
    // be written and then flagged by the project's own filename_pattern
    // check. Refuse instead — the caller supplies a conforming --path.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.kind_rules]]\nglob = \"docs/**\"\nkind = \"generic\"\n\
         [[rules.naming]]\nglob = \"docs/**\"\npattern = \"^\\\\d{4}-[a-z0-9-]+\\\\.md$\"\n\
         [scaffold.paths]\ngeneric = \"docs/{stem}.md\"\n\
         [detection]\norphan_ok_kinds = [\"generic\"]\n",
    )
    .unwrap();
    nodex(root).arg("build").assert().success();

    let output = nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "My New Doc"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let msg = env
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(msg.contains("rules.naming"), "names the rule: {msg}");
    assert!(
        !root.join("docs/my-new-doc.md").exists(),
        "refused scaffold writes nothing"
    );

    // A conforming --path scaffolds, and the result passes check.
    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "My New Doc"])
        .args(["--path", "docs/0042-my-doc.md"])
        .assert()
        .success();
    nodex(root).arg("build").assert().success();
    let check = run_envelope(nodex(root).arg("check"));
    assert_eq!(
        check.pointer("/data/total").and_then(Value::as_i64),
        Some(0)
    );
}

/// A name held by a link pointing at nothing is a name that is taken.
///
/// `scaffold` asked whether its destination *resolved*, so a dangling link
/// fell through to the write, where the root guard refused the link's target
/// and reported a path escaping the project — of a path plainly inside it.
#[cfg(unix)]
#[test]
fn scaffold_does_not_take_a_name_a_dangling_link_already_holds() {
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    unix_fs::symlink("../nowhere/target.md", root.join("docs/taken.md")).unwrap();

    let env = envelope_of(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Taken",
        "--path",
        "docs/taken.md",
    ]));
    assert_eq!(env["error"]["code"], "ALREADY_EXISTS");
    assert!(
        fs::symlink_metadata(root.join("docs/taken.md"))
            .expect("the entry survives")
            .is_symlink(),
        "the link the operator made is still theirs"
    );
}

#[test]
fn scaffold_and_retarget_refuse_reference_unsafe_ids() {
    // An explicit id must round-trip through every reference syntax
    // nodex writes — trim-unstable or metacharacter-bearing ids would
    // be written / repointed into forms the next build cannot resolve.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    nodex(root).arg("build").assert().success();

    for bad in [" padded", "padded ", "with]bracket", "pipe|sep"] {
        let output = nodex(root)
            .args(["scaffold", "--kind", "generic", "--title", "T"])
            .args(["--id", bad, "--path", "docs/t.md"])
            .output()
            .expect("ran");
        assert_eq!(output.status.code(), Some(2), "{bad:?} refused");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert_eq!(
            envelope.pointer("/error/code").and_then(Value::as_str),
            Some("CONFIG_ERROR")
        );
        assert!(!root.join("docs/t.md").exists(), "nothing written");
    }

    // A unicode id remains fully legal.
    nodex(root)
        .args(["scaffold", "--kind", "generic", "--title", "U"])
        .args(["--id", "유니코드-아이디", "--path", "docs/u.md"])
        .assert()
        .success();

    // retarget refuses to repoint ONTO a reference-unsafe id (a
    // hand-written doc can carry one; rewriting references to it would
    // produce forms the build trims into a different id).
    write_doc(
        root,
        "docs/padded.md",
        "---\nid: \" my id \"\ntitle: Padded\nkind: generic\nstatus: active\n---\n# P\n",
    );
    write_doc(
        root,
        "docs/ref.md",
        "---\nid: generic-ref\ntitle: R\nkind: generic\nstatus: active\nrelated: 유니코드-아이디\n---\n# R\n",
    );
    nodex(root).arg("build").assert().success();
    let output = nodex(root)
        .args(["retarget", "유니코드-아이디", " my id "])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("reference-safe"),
        "names the invariant: {envelope}"
    );
}

#[test]
fn scaffold_returns_comparison_candidates_without_a_replacement_warning() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/existing.md",
        "---\nid: doc-existing\ntitle: Storage design\nkind: generic\nstatus: active\ntags: [knowledge]\n---\n# Existing\n",
    );
    let envelope = run_envelope(nodex(tmp.path()).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Payroll holiday policy",
        "--id",
        "doc-new",
        "--path",
        "docs/new.md",
        "--field",
        "tags=[knowledge]",
        "--dry-run",
    ]));
    assert_eq!(envelope["data"]["candidates"][0]["id"], "doc-existing");
    assert_eq!(
        envelope["data"]["candidates"][0]["components"]["title"],
        0.0
    );
    assert_eq!(envelope["data"]["candidates"][0]["components"]["tags"], 1.0);
    assert!(envelope.get("warnings").is_none());
    assert!(!tmp.path().join("docs/new.md").exists());
    let overwrite = run_envelope(nodex(tmp.path()).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Storage design",
        "--id",
        "doc-existing",
        "--path",
        "docs/existing.md",
        "--force",
        "--dry-run",
    ]));
    assert!(overwrite["data"].get("candidates").is_none());
    let manifest = nodex_core::export::export_envelope_schema(false).unwrap();
    let schema = &manifest.per_command["scaffold"];
    let validator = jsonschema::draft202012::new(schema).unwrap();
    for result in [envelope, overwrite] {
        assert!(validator.is_valid(&result["data"]));
    }
}

#[test]
fn scaffold_works_without_prior_build() {
    // scaffold builds its before-graph live from the working tree —
    // no graph.json read, no `nodex build` prerequisite, and the
    // read-only overlay builds persist nothing.
    let tmp = scratch();
    init_project(tmp.path());
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Fresh"])
            .args(["--path", "docs/fresh.md"]),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    assert!(tmp.path().join("docs/fresh.md").exists());
    assert!(
        !tmp.path().join("_index/graph.json").exists()
            && !tmp.path().join("_index/cache.json").exists(),
        "scaffold's overlay builds are read-only"
    );
}

#[test]
fn scaffold_with_body_and_field_writes_exactly_validated_bytes_and_passes_check() {
    let tmp = scratch();
    init_project(tmp.path());
    let body = "# Real Content\n\nDecided: yes.\n";
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Real"])
            .args(["--path", "docs/real.md", "--body", "-"])
            .args(["--field", "tags=[\"decision\"]"])
            .write_stdin(body),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    let on_disk = fs::read_to_string(tmp.path().join("docs/real.md")).unwrap();
    assert_eq!(
        data.get("content").and_then(Value::as_str),
        Some(on_disk.as_str()),
        "the envelope's content and the written bytes are identical"
    );
    assert!(on_disk.ends_with(body), "the supplied body lands verbatim");
    assert!(on_disk.contains("tags: [\"decision\"]"));

    nodex(tmp.path()).arg("build").assert().success();
    let check = run_json(nodex(tmp.path()).arg("check"));
    assert_eq!(
        check.get("has_errors").and_then(Value::as_bool),
        Some(false),
        "the scaffolded document passes its own project's check: {check}"
    );
}

#[test]
fn scaffold_field_enum_violation_refuses() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\nenums = { severity = [\"low\", \"high\"] }\n",
    )
    .unwrap();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Bad"])
        .args(["--path", "docs/bad.md", "--field", "severity=wat"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("field_enum"),
        "the refusal names the rule: {envelope}"
    );
    assert!(!tmp.path().join("docs/bad.md").exists());
}

#[test]
fn scaffold_field_unknown_key_refused_in_strict_mode() {
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\nmode = \"strict\"\n",
    )
    .unwrap();
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Mystery"])
        .args(["--path", "docs/mystery.md", "--field", "mystery=\"x\""])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("unknown_field"),
        "the refusal names the rule: {envelope}"
    );
    assert!(!tmp.path().join("docs/mystery.md").exists());
}

#[test]
fn scaffold_field_reserved_key_refused() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/t.md", "--field", "status=active"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("reserved"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/t.md").exists());

    // `path` is a reserved structural field (set via `--path`), so it is
    // refused at the `--field` seam too — symmetric with the schema /
    // cross_field reservation, not silently written as an inert key.
    let out_path = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/t.md", "--field", "path=docs/other.md"])
        .output()
        .expect("ran");
    assert_eq!(out_path.status.code(), Some(2));
    let env_path: Value =
        serde_json::from_str(String::from_utf8_lossy(&out_path.stdout).trim()).expect("json");
    assert_eq!(
        env_path.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR"),
        "{env_path}"
    );
    assert!(!tmp.path().join("docs/t.md").exists());
}

#[test]
fn scaffold_duplicate_field_key_refused() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/t.md"])
        .args(["--field", "owner=\"a\"", "--field", "owner=\"b\""])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONFIG_ERROR")
    );
    assert!(!tmp.path().join("docs/t.md").exists());
}

#[test]
fn scaffold_malformed_field_pair_rejected_by_clap() {
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "T"])
        .args(["--path", "docs/t.md", "--field", "no-equals-sign"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT")
    );
}

#[test]
fn scaffold_field_supersedes_cycle_refused() {
    // The overlay build refuses structurally: once the scaffolded node
    // exists, doc-a → generic-docs-c resolves and generic-docs-c → doc-a closes
    // the supersession ring.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\nsupersedes: [generic-docs-c]\n---\n# A\n",
    );
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "C"])
        .args(["--path", "docs/c.md", "--field", "supersedes=[doc-a]"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CYCLE_DETECTED")
    );
    assert!(!tmp.path().join("docs/c.md").exists());
}

#[test]
fn scaffold_with_content_refuses_unfilled_required_placeholder_then_passes_with_field() {
    // Strategy 3: supplying content engages the strict gate — the
    // placeholder a default-only scaffold writes as an advisory now
    // refuses, and --field is the documented remedy.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\nrequired = [\"component\"]\n",
    )
    .unwrap();

    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Gated"])
        .args(["--path", "docs/gated.md", "--body", "-"])
        .write_stdin("# Gated\n")
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("required_field"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/gated.md").exists());

    // --field satisfies the same finding.
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Gated"])
            .args(["--path", "docs/gated.md", "--body", "-"])
            .args(["--field", "component=\"auth\""])
            .write_stdin("# Gated\n"),
    );
    assert_eq!(data.get("written").and_then(Value::as_bool), Some(true));
    assert!(
        fs::read_to_string(tmp.path().join("docs/gated.md"))
            .unwrap()
            .contains("component: \"auth\"")
    );
}

#[test]
fn scaffold_default_path_still_writes_with_placeholder_warnings() {
    // Strategy 2: a default-only scaffold keeps the write-and-advise
    // contract — the placeholder required field rides the warnings
    // array, never a refusal.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\nrequired = [\"component\"]\n",
    )
    .unwrap();
    let envelope = run_envelope(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Advised"])
            .args(["--path", "docs/advised.md"]),
    );
    assert_eq!(
        envelope.pointer("/data/written").and_then(Value::as_bool),
        Some(true)
    );
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings present");
    assert!(
        warnings
            .iter()
            .filter_map(warning_msg)
            .any(|w| w.contains("required_field")),
        "the unfilled placeholder is an advisory: {warnings:?}"
    );
    assert!(tmp.path().join("docs/advised.md").exists());
}

#[test]
fn scaffold_field_with_unparseable_yaml_value_refused_via_parse_failure_delta() {
    // A field value that breaks the whole YAML block drops the document
    // from the overlay graph as a typed parse failure; the delta refuses
    // on the new node-less parse_failure violation.
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "Broken"])
        .args(["--path", "docs/broken.md", "--field", "note=[unclosed"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("parse_failure"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/broken.md").exists());
}

#[test]
fn scaffold_field_bad_builtin_value_refused_via_field_parse() {
    // A bad value for a built-in typed field parses leniently into a
    // FieldParseIssue; the node still builds, and the attributable
    // field_parse Error refuses under strategy 3.
    let tmp = scratch();
    init_project(tmp.path());
    let output = nodex(tmp.path())
        .args(["scaffold", "--kind", "generic", "--title", "BadDate"])
        .args([
            "--path",
            "docs/bad-date.md",
            "--field",
            "created=not-a-date",
        ])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("CONTENT_VIOLATIONS")
    );
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("field_parse"),
        "{envelope}"
    );
    assert!(!tmp.path().join("docs/bad-date.md").exists());
}

#[test]
fn scaffold_cross_field_when_keyed_on_supplied_field_emits_require() {
    // The cross_field fixpoint reparses the frontmatter as written, so
    // a `when` keyed on a *supplied* value fires and its `require`
    // field is emitted (here with an enum-valid default).
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [schema]\n\
         enums = { component = [\"auth\", \"billing\"], auth_review = [\"pending\", \"done\"] }\n\
         cross_field = [{ when = \"component=auth\", require = \"auth_review\" }]\n",
    )
    .unwrap();
    let data = run_json(
        nodex(tmp.path())
            .args(["scaffold", "--kind", "generic", "--title", "Auth Thing"])
            .args(["--path", "docs/auth-thing.md", "--field", "component=auth"]),
    );
    let content = data.get("content").and_then(Value::as_str).unwrap();
    assert!(content.contains("component: auth"), "{content}");
    assert!(
        content.contains("auth_review: \"pending\""),
        "the require keyed on the supplied value is emitted: {content}"
    );

    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path()).arg("check").assert().success();
}
