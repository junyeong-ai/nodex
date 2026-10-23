//! `export` manifests.

use crate::support::{init_project, nodex, run_envelope, run_json, scratch};
use serde_json::Value;
use std::fs;

#[test]
fn export_emits_schema_and_enums_manifests() {
    let tmp = scratch();
    init_project(tmp.path());

    let schema = run_json(nodex(tmp.path()).args(["export", "schema"]));
    assert_eq!(
        schema.get("$schema").and_then(Value::as_str),
        Some("https://json-schema.org/draft/2020-12/schema")
    );

    let enums = run_json(nodex(tmp.path()).args(["export", "enums"]));
    let kinds: Vec<&str> = enums
        .get("kinds")
        .and_then(Value::as_array)
        .expect("kinds")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(kinds.contains(&"generic"));
}

#[test]
fn export_schema_constrains_require_explicit_fields_non_empty() {
    // `require_explicit` forces a built-in to be authored; `check`'s
    // `explicit_field` reds an empty value, so the exported JSON Schema
    // must reject empty too (`minLength: 1`) or a codegen consumer would
    // accept `title: ""` that `check` refuses. The two must agree.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\"]\nterminal = []\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{stem}\"\n\
         [schema]\nrequire_explicit = [\"title\"]\n",
    )
    .unwrap();
    let schema = run_json(nodex(tmp.path()).args(["export", "schema"]));
    // Find the `title` property's minLength wherever the branch sits
    // (single-object schema, or a `oneOf` branch).
    fn title_min_length(v: &Value) -> Option<u64> {
        if let Some(props) = v.get("properties").and_then(Value::as_object)
            && let Some(min) = props
                .get("title")
                .and_then(|t| t.get("minLength"))
                .and_then(Value::as_u64)
        {
            return Some(min);
        }
        for (_k, child) in v.as_object().into_iter().flatten() {
            if let Some(m) = title_min_length(child) {
                return Some(m);
            }
        }
        for child in v.as_array().into_iter().flatten() {
            if let Some(m) = title_min_length(child) {
                return Some(m);
            }
        }
        None
    }
    assert_eq!(
        title_min_length(&schema),
        Some(1),
        "require_explicit title must carry minLength:1 in the exported schema: {schema}"
    );
}

#[test]
fn export_rules_emits_active_only_manifest_with_version_and_body_line_entries() {
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("nodex.toml");
    let mut content = fs::read_to_string(&path).expect("nodex.toml");
    content.push_str(
        "\n[[rules.body_line]]\nname = \"decision-log\"\npattern = '^- \\*\\*(?P<gate>[a-z-]+)\\*\\*'\nenums.gate = [\"scope\", \"design\"]\n",
    );
    fs::write(&path, content).expect("nodex.toml writable");

    let data = run_json(nodex(tmp.path()).args(["export", "rules"]));
    assert_eq!(data["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    let ids: Vec<&str> = data["rules"]
        .as_array()
        .expect("rules array")
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert!(ids.contains(&"required_field"));
    assert!(ids.contains(&"stale_review"));
    assert!(
        ids.contains(&"body_line/decision-log"),
        "config block must surface in manifest: {ids:?}"
    );
    assert!(
        !ids.iter()
            .any(|id| id.starts_with("frontmatter_immutable/")),
        "unconfigured rule must not appear: {ids:?}"
    );
    assert!(!ids.contains(&"git_drift"));
}

#[test]
fn export_rules_cycle_detection_surfaces_configured_relations() {
    // The manifest's cycle-detection entry must carry the live
    // relation set in `params` and a relation-agnostic description —
    // a hardcoded "implements" description would be false for this
    // project, which declares `depends_on` acyclic instead.
    let tmp = scratch();
    init_project(tmp.path());
    let path = tmp.path().join("nodex.toml");
    let mut content = fs::read_to_string(&path).expect("nodex.toml");
    content.push_str(
        "\n[[parser.link_patterns]]\npattern = '@depends\\s+(\\S+)'\nrelation = \"depends_on\"\n",
    );
    // The key lives inside the [rules] table the init template opens.
    let content = content.replace(
        "immutable_baseline = \"HEAD\"",
        "immutable_baseline = \"HEAD\"\nacyclic_relations = [\"depends_on\"]",
    );
    fs::write(&path, content).expect("nodex.toml writable");

    let data = run_json(nodex(tmp.path()).args(["export", "rules"]));
    let rules = data["rules"].as_array().expect("rules array");
    let cycle = rules
        .iter()
        .find(|r| r["id"].as_str() == Some("acyclic_relation"))
        .expect("cycle-detection entry present");
    assert_eq!(
        cycle.pointer("/params/relations"),
        Some(&serde_json::json!(["depends_on"])),
        "params must carry the live relation set: {cycle}"
    );
    assert!(
        !cycle["description"]
            .as_str()
            .unwrap_or_default()
            .contains("implements"),
        "description must not name a relation this project does not check: {cycle}"
    );
}

#[test]
fn export_envelope_schema_runs_without_project_and_lists_per_command_entries() {
    // Envelope shape is project-independent; the command must run
    // even from a directory with no `nodex.toml`.
    let tmp = scratch();
    let data = run_json(nodex(tmp.path()).args(["export", "envelope-schema"]));
    assert_eq!(data["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    assert!(data["envelope"].is_object(), "envelope schema present");
    let per_command = data["per_command"].as_object().expect("per_command object");
    // Spot-check a handful of canonical entries — exhaustive coverage
    // lives in the core unit-test that validates each schema against
    // draft 2020-12.
    for key in ["query.issues", "query.annotations", "check", "build"] {
        assert!(
            per_command.contains_key(key),
            "per_command missing {key}: keys = {:?}",
            per_command.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn export_config_emits_resolved_surface() {
    let tmp = scratch();
    init_project(tmp.path());
    let data = run_json(nodex(tmp.path()).args(["export", "config"]));
    assert!(data["scope"]["include"].is_array());
    assert!(data["output"]["dir"].is_string());
    assert_eq!(data["identity"]["fallback_kind"], "generic");
    assert_eq!(
        data["identity"]["fallback_id_template"],
        "{kind}-{path_slug}"
    );
    assert!(data["initial_status"].is_string());
}

#[test]
fn export_commands_emits_grammar_without_a_project() {
    // Config-independent: runs in a directory with no nodex.toml.
    let tmp = scratch();
    let data = run_json(nodex(tmp.path()).args(["export", "commands"]));
    let commands = data["commands"].as_array().expect("commands array");
    assert!(!commands.is_empty());
    let backlinks = commands
        .iter()
        .find(|c| c["schema"] == "query.backlinks")
        .expect("query.backlinks leaf");
    assert_eq!(backlinks["positionals"][0]["name"], "id");
    assert_eq!(backlinks["positionals"][0]["required"], true);
}

#[test]
fn export_diagnostics_emits_code_vocabularies_without_a_project() {
    // Config-independent: the error/exit-code vocabulary is the same in
    // every project, so it runs with no nodex.toml.
    let tmp = scratch();
    let data = run_json(nodex(tmp.path()).args(["export", "diagnostics"]));

    let codes: Vec<&str> = data["error_codes"]
        .as_array()
        .expect("error_codes array")
        .iter()
        .filter_map(|e| e["code"].as_str())
        .collect();
    // A core code and both CLI-classifier codes are published.
    assert!(codes.contains(&"CONFIG_ERROR"), "{codes:?}");
    assert!(codes.contains(&"INVALID_ARGUMENT"), "{codes:?}");
    assert!(codes.contains(&"INTERNAL_ERROR"), "{codes:?}");
    // The CLI-owned codes are tagged `cli`, core codes `core`.
    let origin = |code: &str| -> Option<String> {
        data["error_codes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["code"] == code)
            .and_then(|e| e["origin"].as_str())
            .map(String::from)
    };
    assert_eq!(origin("CONFIG_ERROR").as_deref(), Some("core"));
    assert_eq!(origin("INVALID_ARGUMENT").as_deref(), Some("cli"));

    let exit: Vec<u64> = data["exit_codes"]
        .as_array()
        .expect("exit_codes array")
        .iter()
        .filter_map(|e| e["code"].as_u64())
        .collect();
    assert_eq!(exit, [0, 1, 2], "documented exit-code contract");

    // The advisory-warning vocabulary is published too (the success-plane
    // counterpart to error_codes).
    let warn_codes: Vec<&str> = data["warning_codes"]
        .as_array()
        .expect("warning_codes array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        warn_codes.contains(&"gate_suppression") && warn_codes.contains(&"binary_compat"),
        "{warn_codes:?}"
    );
}

#[test]
fn export_envelope_schema_inline_refs_is_self_contained() {
    let tmp = scratch();
    let envelope =
        run_envelope(nodex(tmp.path()).args(["export", "envelope-schema", "--inline-refs"]));
    let raw = serde_json::to_string(&envelope["data"]["per_command"]).unwrap();
    assert!(!raw.contains("\"$ref\""), "inlined form must carry no $ref");
    assert!(
        !raw.contains("\"$defs\""),
        "inlined form must carry no $defs"
    );
}
