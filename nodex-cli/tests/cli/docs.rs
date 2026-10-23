//! Documented examples behave as documented: the READMEs, configuration
//! examples and the init template.

use crate::support::{
    envelope_of, init_project, nodex, run_envelope, run_json, scratch, write_doc,
};
use serde_json::Value;
use std::fs;

#[test]
fn every_commented_init_template_example_loads_when_enabled() {
    // The tool's own documented config can never be one its own loader
    // rejects. Enumerating the families from the template itself means an
    // example added later is covered by construction rather than by someone
    // remembering to widen this test.
    let tmp = scratch();
    let root = tmp.path();
    nodex(root).arg("init").assert().success();
    let cfg = fs::read_to_string(root.join("nodex.toml")).unwrap();
    let families = commented_families(&cfg);
    assert!(
        families.contains("rules.frontmatter_immutable")
            && families.contains("rules.body_immutable")
            && families.len() >= 8,
        "the template must document the families this test exists to prove: {families:?}"
    );
    assert_each_commented_family_loads(root, &cfg, "nodex init");
}

/// Every configuration the repository documents for readers to copy — each
/// README's example and the skill's minimal config — loads as written and
/// with each commented example enabled.
#[test]
fn every_documented_configuration_loads_as_written_and_uncommented() {
    // Line endings belong to the checkout rather than to the document: a
    // Windows checkout carries CRLF, which no heading or fence below spells.
    let read = |path: &str| {
        let text = fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(path),
        )
        .unwrap();
        nodex_core::parser::frontmatter::canonicalize(&text).into_owned()
    };
    let mut configs = vec![(
        ".claude/skills/nodex/reference/minimal-config.toml",
        read(".claude/skills/nodex/reference/minimal-config.toml"),
    )];
    for (readme, heading) in [
        ("README.md", "## Configuration"),
        ("README.ko.md", "## 설정"),
    ] {
        let text = read(readme);
        let section = text
            .split_once(&format!("\n{heading}\n"))
            .unwrap_or_else(|| panic!("{readme} has a {heading} section"))
            .1;
        let cfg = section
            .split_once("```toml\n")
            .and_then(|(_, rest)| rest.split_once("\n```"))
            .unwrap_or_else(|| panic!("{readme}'s {heading} opens with a toml block"))
            .0;
        assert!(
            !commented_families(cfg).is_empty(),
            "{readme}: the example documents commented families"
        );
        configs.push((readme, format!("{cfg}\n")));
    }
    for (source, cfg) in configs {
        let tmp = scratch();
        assert_each_commented_family_loads(tmp.path(), &cfg, source);
    }
}

/// Every `[[family]]` a config documents only as a commented example.
fn commented_families(cfg: &str) -> std::collections::BTreeSet<&str> {
    cfg.lines()
        .filter_map(|line| line.strip_prefix("# [["))
        .filter_map(|rest| rest.strip_suffix("]]"))
        .collect()
}

/// `cfg` loads as written, and again with each commented family enabled on
/// its own — one at a time rather than all at once, so a failure names the
/// example that caused it.
fn assert_each_commented_family_loads(root: &std::path::Path, cfg: &str, source: &str) {
    let loads = |text: &str, what: &str| {
        fs::write(root.join("nodex.toml"), text).unwrap();
        let output = nodex(root).arg("build").output().expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert_eq!(
            envelope["ok"],
            true,
            "{source}: {what} must load: {}",
            envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
    };
    loads(cfg, "the example as written");
    for family in commented_families(cfg) {
        let opening = format!("[[{family}]]");
        let mut enabled = String::new();
        let mut inside = false;
        for line in cfg.lines() {
            let bare = line.strip_prefix("# ").unwrap_or(line);
            if bare.starts_with(&opening) {
                inside = true;
            } else if !bare.starts_with(|c: char| c.is_ascii_lowercase()) {
                inside = false;
            }
            enabled.push_str(if inside { bare } else { line });
            enabled.push('\n');
        }
        assert_ne!(enabled, cfg, "{source} {family}: nothing was enabled");
        loads(&enabled, &format!("the documented {family} example"));
    }
}

/// The corpus of the README's *Walkthrough: From Files to Answers* — the
/// same in README.ko.md but for body prose — and every value the two READMEs
/// print for it but the build's duration: an object they print whole is
/// compared whole, down to its key set — the envelope and each step's `data`
/// included, so a field they leave out is a difference — and one they print
/// in part on the fields they show. The
/// READMEs are not parsed, so what they print is mirrored here by hand; a
/// failure is the binary no longer answering what they print, and both need
/// the new output.
#[test]
fn the_readme_walkthrough_answers_as_the_readmes_show() {
    use serde_json::json;
    const CONFIG: &str = r#"[scope]
include = ["docs/**/*.md"]

[kinds]
allowed = ["generic", "adr", "guide"]

[statuses]
allowed = ["active", "superseded"]
terminal = ["superseded"]

[[identity.kind_rules]]
glob = "docs/decisions/**"
kind = "adr"
[[identity.kind_rules]]
glob = "docs/guides/**"
kind = "guide"
[[identity.id_rules]]
kind = "adr"
template = "adr-{stem}"

[schema]
required = ["created"]
cross_field = [{ when = "status=superseded", require = "superseded_by" }]
"#;
    const GUIDE: &str = "---\ntitle: API Setup\nstatus: active\ncreated: 2025-02-01\n---\n\
                         # API Setup\nStart from the [GraphQL API decision](../decisions/0002-graphql-api.md).\n";
    const ORPHAN: &str = "no document references this one; link it, set `orphan_ok: true`, \
                          or add its kind to [detection].orphan_ok_kinds";
    fn keys(object: &Value) -> std::collections::BTreeSet<&str> {
        object
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect()
    }
    /// `data`, once the envelope is found to carry nothing beside it.
    fn data(envelope: &Value) -> &Value {
        assert_eq!(keys(envelope), ["ok", "data"].into());
        &envelope["data"]
    }
    fn snapshot_data(envelope: &Value) -> &Value {
        assert_eq!(keys(envelope), ["ok", "data", "snapshot"].into());
        assert_eq!(
            envelope["snapshot"],
            json!({"verification": "content", "unbuildable": 0, "unfollowed": 0})
        );
        &envelope["data"]
    }
    /// The named fields of `value`, for comparing some of an object at once.
    fn shown(value: &Value, fields: &[&str]) -> Value {
        fields
            .iter()
            .map(|k| (k.to_string(), value[*k].clone()))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }
    fn each(list: &Value, fields: &[&str]) -> Vec<Value> {
        list.as_array()
            .expect("a list")
            .iter()
            .map(|item| shown(item, fields))
            .collect()
    }
    fn coverage<'a>(data: &'a Value, rule: &str) -> &'a Value {
        data["rule_coverage"]
            .as_array()
            .expect("rule_coverage")
            .iter()
            .find(|c| c["rule_id"] == rule)
            .expect("the rule ran")
    }

    let tmp = scratch();
    let root = tmp.path();
    write_doc(root, "nodex.toml", CONFIG);
    write_doc(
        root,
        "docs/decisions/0001-rest-api.md",
        "---\ntitle: REST API\nstatus: superseded\nsuperseded_by: adr-0002-graphql-api\n\
         created: 2025-01-10\n---\n# REST API\nOur original API design.\n",
    );
    write_doc(
        root,
        "docs/decisions/0002-graphql-api.md",
        "---\ntitle: GraphQL API\nstatus: active\ncreated: 2025-01-20\n---\n\
         # GraphQL API\nWe replaced REST with GraphQL.\n",
    );
    write_doc(root, "docs/guides/api-setup.md", GUIDE);

    let built = run_envelope(nodex(root).arg("build"));
    let built = data(&built);
    assert_eq!(
        keys(built),
        [
            "nodes",
            "edges",
            "annotations",
            "body_line_matches",
            "cached",
            "parsed",
            "duration_ms"
        ]
        .into()
    );
    assert_eq!(
        shown(
            built,
            &[
                "nodes",
                "edges",
                "annotations",
                "body_line_matches",
                "cached",
                "parsed"
            ]
        ),
        json!({ "nodes": 3, "edges": 2, "annotations": 0, "body_line_matches": 0, "cached": 0, "parsed": 3 })
    );

    let chain = run_envelope(nodex(root).args(["query", "chain", "adr-0001-rest-api"]));
    let chain = snapshot_data(&chain);
    assert_eq!(keys(chain), ["items", "total"].into());
    assert_eq!(
        each(&chain["items"], &["id", "title", "status"]),
        [
            json!({ "id": "adr-0001-rest-api", "title": "REST API", "status": "superseded" }),
            json!({ "id": "adr-0002-graphql-api", "title": "GraphQL API", "status": "active" })
        ]
    );
    assert_eq!(chain["total"], 2);

    let backlinks = run_envelope(nodex(root).args(["query", "backlinks", "adr-0002-graphql-api"]));
    let backlinks = snapshot_data(&backlinks);
    assert_eq!(keys(backlinks), ["items", "total"].into());
    assert_eq!(
        each(&backlinks["items"], &["id", "relation", "location"]),
        [
            json!({ "id": "guide-docs-guides-api-setup", "relation": "references", "location": "L7" })
        ]
    );
    assert_eq!(backlinks["total"], 1);

    let check = run_envelope(nodex(root).arg("check"));
    let check = data(&check);
    assert_eq!(
        keys(check),
        [
            "violations",
            "skipped_rules",
            "rule_coverage",
            "total",
            "has_errors"
        ]
        .into()
    );
    assert_eq!(
        check["violations"],
        json!([{
            "rule_id": "orphan", "severity": "warning",
            "node_id": "guide-docs-guides-api-setup", "path": "docs/guides/api-setup.md",
            "message": ORPHAN,
            "details": { "type": "orphan" }
        }])
    );
    assert_eq!(check["skipped_rules"], json!([]));
    assert_eq!(
        coverage(check, "acyclic_relation"),
        &json!({ "rule_id": "acyclic_relation", "unit": "edges", "subjects": 0, "unjudged": 0 })
    );
    assert_eq!(
        coverage(check, "required_field"),
        &json!({ "rule_id": "required_field", "unit": "nodes", "subjects": 3, "unjudged": 0 })
    );
    assert_eq!(
        shown(check, &["total", "has_errors"]),
        json!({ "total": 1, "has_errors": false })
    );

    write_doc(
        root,
        "draft.md",
        "---\ntitle: gRPC API\nstatus: active\n---\n# gRPC API\n",
    );
    let output = nodex(root)
        .current_dir(root)
        .args([
            "check",
            "--content",
            "docs/decisions/0003-grpc-api.md=draft.md",
        ])
        .output()
        .expect("command ran");
    assert_eq!(output.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(envelope["ok"], true);
    let gate = data(&envelope);
    assert_eq!(
        keys(gate),
        [
            "violations",
            "skipped_rules",
            "rule_coverage",
            "total",
            "has_errors",
            "proposals",
            "standing"
        ]
        .into()
    );
    let proposed =
        json!({ "rule_id": "orphan", "severity": "warning", "node_id": "adr-0003-grpc-api" });
    assert_eq!(gate["violations"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        shown(&gate["violations"][0], &["rule_id", "severity", "node_id"]),
        proposed
    );
    assert_eq!(
        gate["violations"][1],
        json!({
            "rule_id": "required_field", "severity": "error",
            "node_id": "adr-0003-grpc-api", "path": "docs/decisions/0003-grpc-api.md",
            "message": "missing required field: created",
            "details": { "type": "required_field", "field": "created" }
        })
    );
    assert_eq!(gate["skipped_rules"], json!([]));
    assert_eq!(
        coverage(gate, "required_field"),
        &json!({ "rule_id": "required_field", "unit": "nodes", "subjects": 4, "unjudged": 0 })
    );
    assert_eq!(
        shown(gate, &["total", "has_errors", "proposals"]),
        json!({
            "total": 2,
            "has_errors": true,
            "proposals": [{
                "path": "docs/decisions/0003-grpc-api.md",
                "in_scope": true,
                "has_path_errors": true
            }]
        })
    );
    assert_eq!(
        each(&gate["standing"], &["rule_id", "severity", "node_id"]),
        [proposed]
    );

    write_doc(
        root,
        "docs/guides/api-setup.md",
        &format!("{GUIDE}See [the missing decision](../decisions/0009-missing.md).\n"),
    );
    nodex(root).arg("build").assert().success();
    let unresolved = run_json(nodex(root).arg("check"));
    assert_eq!(
        unresolved["violations"].as_array().unwrap().len(),
        check["violations"].as_array().unwrap().len() + 1
    );
    let issues = run_json(nodex(root).args(["query", "issues"]));
    assert_eq!(
        issues["summary"]["by_category"]["violation_unresolved_reference"],
        1
    );
    write_doc(
        root,
        "nodex.toml",
        &format!(
            "{CONFIG}\n[[detection.unresolved_policy]]\nname = \"broken\"\ncause = \"missing\"\n\
             severity = \"error\"\n"
        ),
    );
    let output = nodex(root).arg("check").output().expect("command ran");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a policy row makes it an error"
    );
    let policed: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        each(
            &policed["data"]["violations"],
            &["rule_id", "severity", "node_id"]
        ),
        [
            json!({ "rule_id": "orphan", "severity": "warning", "node_id": "guide-docs-guides-api-setup" }),
            json!({ "rule_id": "unresolved_reference/broken", "severity": "error", "node_id": "guide-docs-guides-api-setup" })
        ]
    );

    write_doc(
        root,
        "docs/decisions/0002-graphql-api.md",
        "---\ntitle: GraphQL API\nstatus: superseded\nsuperseded_by: adr-0001-rest-api\n\
         created: 2025-01-20\n---\n# GraphQL API\n",
    );
    let refused = envelope_of(nodex(root).arg("build"));
    assert_eq!(refused["error"]["code"], "CYCLE_DETECTED");
}

/// The marker the `nodex init` template documents, enabled as written: a
/// comment opening its line declares the superseded document a runbook cites
/// as history, and only its body citation of it. The syntax shown as inline
/// code, as escaped text or inside a code block declares nothing, because
/// annotations read inline code and the template's pattern is what keeps an
/// example from being read.
#[test]
fn the_documented_marker_declares_a_body_citation_history() {
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    let template = fs::read_to_string(root.join("nodex.toml")).unwrap();
    let mut config = String::new();
    let mut in_block = false;
    let mut enabled = 0;
    let lines: Vec<&str> = template.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if *line == "# [[annotations]]" && lines.get(i + 1) == Some(&"# name = \"superseded-ok\"") {
            in_block = true;
        } else if !line.starts_with("# ") || line.starts_with("# [") {
            in_block = false;
        }
        let bare = line.strip_prefix("# ");
        match bare {
            Some(bare)
                if in_block || bare == "superseded_reference_ok_annotation = \"superseded-ok\"" =>
            {
                enabled += 1;
                config.push_str(bare);
            }
            _ => config.push_str(line),
        }
        config.push('\n');
    }
    assert_eq!(enabled, 5, "the template's marker block and key:\n{config}");
    fs::write(root.join("nodex.toml"), config).unwrap();

    for (id, successor) in [("old", "new"), ("older", "newer")] {
        write_doc(
            root,
            &format!("docs/{id}.md"),
            &format!(
                "---\nid: {id}\ntitle: {id}\nkind: generic\nstatus: superseded\n\
                 superseded_by: {successor}\n---\n# {id}\n"
            ),
        );
        write_doc(
            root,
            &format!("docs/{successor}.md"),
            &format!(
                "---\nid: {successor}\ntitle: {successor}\nkind: generic\nstatus: active\n\
                 ---\n# {successor}\n"
            ),
        );
    }
    write_doc(
        root,
        "docs/runbook.md",
        "---\nid: runbook\ntitle: Runbook\nkind: generic\nstatus: active\nrelated: [old]\n\
         ---\n# Runbook\n\n\
         <!-- superseded-ok: old the figures are its own -->\n\
         Measured in [the old decision](old.md).\n\
         Replaced by [the older decision](older.md).\n\n\
         `<!-- superseded-ok: older how a marker reads -->` declares one.\n\
         \\<!-- superseded-ok: older how a marker reads --> is escaped.\n\n\
         ```html\n<!-- superseded-ok: older how a marker reads -->\n```\n",
    );

    let data = run_json(nodex(root).arg("check"));
    let stale: Vec<(&str, &str, &str)> = data["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["rule_id"] == "superseded_reference")
        .map(|v| {
            (
                v["node_id"].as_str().unwrap(),
                v["details"]["relation"].as_str().unwrap(),
                v["details"]["target"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        stale,
        [
            ("runbook", "references", "older"),
            ("runbook", "related", "old")
        ],
        "{data}"
    );
}
