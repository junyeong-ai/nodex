//! The `contract-gate` release tool.

use crate::support::scratch;
use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn contract_gate() -> Command {
    Command::cargo_bin("contract-gate").expect("contract-gate binary in cargo target")
}

/// One release's two published manifests, as the release workflow downloads
/// them.
struct Release {
    envelope: PathBuf,
    commands: PathBuf,
}

fn write_release(
    dir: &std::path::Path,
    name: &str,
    version: &str,
    per_command: Value,
    commands: Value,
) -> Release {
    let write = |suffix: &str, data: Value| {
        let path = dir.join(format!("{name}-{suffix}.json"));
        fs::write(
            &path,
            serde_json::json!({ "ok": true, "data": data }).to_string(),
        )
        .unwrap();
        path
    };
    Release {
        envelope: write(
            "envelope",
            serde_json::json!({ "version": version, "envelope": {}, "per_command": per_command }),
        ),
        commands: write(
            "commands",
            serde_json::json!({ "version": version, "global_flags": [], "commands": commands }),
        ),
    }
}

/// The gate run over two releases, each manifest in the order the release
/// workflow passes it.
fn gate(baseline: &Release, head: &Release) -> std::process::Output {
    contract_gate()
        .arg(&baseline.envelope)
        .arg(&head.envelope)
        .arg(&baseline.commands)
        .arg(&head.commands)
        .output()
        .expect("ran")
}

fn gate_per_command() -> Value {
    serde_json::json!({
        "build": {
            "type": "object",
            "properties": { "nodes": { "type": "integer" } },
            "required": ["nodes"]
        }
    })
}

fn gate_commands() -> Value {
    serde_json::json!([{
        "path": ["build"],
        "schema": "build",
        "flags": [{ "long": "full", "takes_value": false, "repeatable": false, "required": false }]
    }])
}

fn verdict_of(output: &std::process::Output) -> Value {
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("JSON")
}

#[test]
fn contract_gate_passes_identical_inputs() {
    let tmp = scratch();
    let baseline = write_release(
        tmp.path(),
        "baseline",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let head = write_release(
        tmp.path(),
        "head",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let output = gate(&baseline, &head);
    assert_eq!(output.status.code(), Some(0));
    let verdict = verdict_of(&output);
    assert_eq!(verdict["verdict"], "pass");
    assert!(verdict["breaking"].as_array().unwrap().is_empty());
    assert!(verdict["additive"].as_array().unwrap().is_empty());
}

#[test]
fn contract_gate_fails_breaking_change_without_version_bump() {
    let tmp = scratch();
    let baseline = write_release(
        tmp.path(),
        "baseline",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let mut head_schema = gate_per_command();
    head_schema["build"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("nodes");
    head_schema["build"]["required"] = serde_json::json!([]);
    let head = write_release(tmp.path(), "head", "0.15.0", head_schema, gate_commands());
    let output = gate(&baseline, &head);
    assert_eq!(output.status.code(), Some(1));
    let verdict = verdict_of(&output);
    assert_eq!(verdict["verdict"], "fail");
    assert!(!verdict["breaking"].as_array().unwrap().is_empty());
}

#[test]
fn contract_gate_passes_breaking_change_with_minor_bump() {
    // Pre-1.0, the 0.x component is the breaking component — bumping
    // it satisfies the promise for any classified change.
    let tmp = scratch();
    let baseline = write_release(
        tmp.path(),
        "baseline",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let mut head_schema = gate_per_command();
    head_schema["build"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("nodes");
    head_schema["build"]["required"] = serde_json::json!([]);
    let head = write_release(tmp.path(), "head", "0.16.0", head_schema, gate_commands());
    assert_eq!(gate(&baseline, &head).status.code(), Some(0));
}

#[test]
fn contract_gate_fails_additive_change_within_patch_bump() {
    // Even an additive change needs the 0.x bump pre-1.0 — a patch
    // release must be envelope-identical.
    let tmp = scratch();
    let baseline = write_release(
        tmp.path(),
        "baseline",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let mut head_schema = gate_per_command();
    head_schema["build"]["properties"]["edges"] = serde_json::json!({ "type": "integer" });
    let head = write_release(tmp.path(), "head", "0.15.1", head_schema, gate_commands());
    let output = gate(&baseline, &head);
    assert_eq!(output.status.code(), Some(1));
    let verdict = verdict_of(&output);
    assert_eq!(verdict["verdict"], "fail");
    assert!(verdict["breaking"].as_array().unwrap().is_empty());
    assert!(!verdict["additive"].as_array().unwrap().is_empty());
}

/// A flag a patch release removes breaks every script that passes it, though
/// no payload changed shape.
#[test]
fn contract_gate_fails_a_removed_flag_within_patch_bump() {
    let tmp = scratch();
    let baseline = write_release(
        tmp.path(),
        "baseline",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let mut commands = gate_commands();
    commands[0]["flags"] = serde_json::json!([]);
    let patch = write_release(
        tmp.path(),
        "patch",
        "0.15.1",
        gate_per_command(),
        commands.clone(),
    );
    let output = gate(&baseline, &patch);
    assert_eq!(output.status.code(), Some(1));
    let verdict = verdict_of(&output);
    assert_eq!(verdict["breaking"][0]["command"], "build");
    assert_eq!(verdict["breaking"][0]["location"], "flags/--full");

    let minor = write_release(tmp.path(), "minor", "0.16.0", gate_per_command(), commands);
    assert_eq!(gate(&baseline, &minor).status.code(), Some(0));
}

#[test]
fn contract_gate_refuses_non_success_envelope_input() {
    // An error envelope (`ok: false`) — e.g. a failed export captured
    // into the baseline file — must be an operational failure, never a
    // baseline the gate diffs vacuously. Same for a payload that carries
    // `.data` without declaring `ok: true`.
    let tmp = scratch();
    let mut baseline = write_release(
        tmp.path(),
        "baseline",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let head = write_release(
        tmp.path(),
        "head",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );

    fs::write(
        &baseline.envelope,
        serde_json::json!({
            "ok": false,
            "error": { "code": "CONFIG_ERROR", "message": "boom" }
        })
        .to_string(),
    )
    .unwrap();
    let output = gate(&baseline, &head);
    assert_eq!(output.status.code(), Some(2));
    let envelope = verdict_of(&output);
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{envelope}"
    );
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(message.contains("CONFIG_ERROR"), "{message}");

    // `.data` present but no `ok: true` declaration: refused, naming
    // what was found.
    baseline.envelope = tmp.path().join("okless.json");
    fs::write(
        &baseline.envelope,
        serde_json::json!({ "data": { "version": "0.15.0" } }).to_string(),
    )
    .unwrap();
    let output = gate(&baseline, &head);
    assert_eq!(output.status.code(), Some(2));
    let envelope = verdict_of(&output);
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{envelope}"
    );
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(message.contains("no `ok` field"), "{message}");
}

#[test]
fn contract_gate_refuses_bare_ok_false_envelope() {
    // `{"ok": false}` with no `error` key — e.g. a truncated capture —
    // takes the not-a-successful-envelope branch, not the
    // error-envelope branch: refused naming exactly what was found.
    let tmp = scratch();
    let baseline = write_release(
        tmp.path(),
        "baseline",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let head = write_release(
        tmp.path(),
        "head",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    fs::write(
        &baseline.envelope,
        serde_json::json!({ "ok": false }).to_string(),
    )
    .unwrap();
    let output = gate(&baseline, &head);
    assert_eq!(output.status.code(), Some(2));
    let envelope = verdict_of(&output);
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("INVALID_ARGUMENT"),
        "{envelope}"
    );
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(message.contains("`ok` is false"), "{message}");
}

#[test]
fn contract_gate_classifies_operational_failures() {
    // A file the gate cannot read is IO_ERROR; a malformed invocation —
    // or two manifests of different releases passed as one — is
    // INVALID_ARGUMENT: the same dispatch vocabulary as every other
    // command, so CI tooling branches on the code, never the prose.
    let tmp = scratch();
    let mut baseline = write_release(
        tmp.path(),
        "baseline",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let head = write_release(
        tmp.path(),
        "head",
        "0.15.0",
        gate_per_command(),
        gate_commands(),
    );
    let code = |output: std::process::Output| {
        assert_eq!(output.status.code(), Some(2));
        verdict_of(&output)
            .pointer("/error/code")
            .and_then(Value::as_str)
            .expect("error code")
            .to_owned()
    };

    let other = write_release(
        tmp.path(),
        "other",
        "0.14.0",
        gate_per_command(),
        gate_commands(),
    );
    let mixed = Release {
        envelope: baseline.envelope.clone(),
        commands: other.commands,
    };
    assert_eq!(code(gate(&mixed, &head)), "INVALID_ARGUMENT");

    baseline.envelope = tmp.path().join("missing.json");
    assert_eq!(code(gate(&baseline, &head)), "IO_ERROR");

    assert_eq!(
        code(contract_gate().output().expect("ran")),
        "INVALID_ARGUMENT"
    );
}
