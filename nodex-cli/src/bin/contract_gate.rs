//! Release contract gate: diff the published contract of two releases —
//! `nodex export envelope-schema` (what each command emits) and `nodex
//! export commands` (how each is invoked) — and enforce the CODEGEN.md
//! semver promise: any classified change requires a minor-or-major
//! version bump (pre-1.0: the 0.x component must increase; ≥1.0:
//! additive → at least minor, breaking → major).
//!
//! Usage: `contract-gate <baseline-envelope.json> <head-envelope.json>
//! <baseline-commands.json> <head-commands.json>` — each file is raw
//! `nodex export` stdout, and each release's two files must name the same
//! version. Prints one JSON verdict to stdout (the classified diff doubles
//! as the contract changelog) and exits 0 on pass, 1 on a verdict
//! violation, 2 on an operational failure. Operational failures emit the standard
//! error envelope wherever stdout takes one, classified like every other
//! command: `IO_ERROR` for a file that cannot be read, `INVALID_ARGUMENT`
//! for a malformed invocation or input — a CI gate stays JSON-first and
//! machine-dispatchable either way.

use serde::Serialize;
use serde_json::Value;

use nodex_core::export::{ContractChange, compute_commands_diff, compute_envelope_schema_diff};

// The crate's single envelope encoder, shared with the `nodex` binary
// (`format` re-exports the same module) — one stable contract, one
// encoder.
#[path = "../envelope.rs"]
mod envelope;
use envelope::{ErrorEnvelope, print_json};

#[derive(Serialize)]
struct Verdict {
    baseline_version: String,
    head_version: String,
    breaking: Vec<ContractChange>,
    additive: Vec<ContractChange>,
    verdict: &'static str,
}

/// An operational failure plus the envelope code it classifies as.
/// `Io` is a file the gate could not read; `Usage` is a malformed
/// invocation or input payload.
enum GateError {
    Io(String),
    Usage(String),
}

impl GateError {
    fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "IO_ERROR",
            Self::Usage(_) => "INVALID_ARGUMENT",
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Io(message) | Self::Usage(message) => message,
        }
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            print_json(&ErrorEnvelope::new(err.code(), err.message()), false);
            std::process::exit(2);
        }
    }
}

fn run() -> Result<i32, GateError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [
        baseline_envelope_path,
        head_envelope_path,
        baseline_commands_path,
        head_commands_path,
    ] = args.as_slice()
    else {
        return Err(GateError::Usage(
            "usage: contract-gate <baseline-envelope.json> <head-envelope.json> \
             <baseline-commands.json> <head-commands.json>"
                .into(),
        ));
    };
    let baseline_envelope = load_payload(baseline_envelope_path)?;
    let head_envelope = load_payload(head_envelope_path)?;
    let baseline_commands = load_payload(baseline_commands_path)?;
    let head_commands = load_payload(head_commands_path)?;
    let baseline_version = release_version(
        (&baseline_envelope, baseline_envelope_path),
        (&baseline_commands, baseline_commands_path),
    )?;
    let head_version = release_version(
        (&head_envelope, head_envelope_path),
        (&head_commands, head_commands_path),
    )?;

    let mut diff = compute_envelope_schema_diff(&baseline_envelope, &head_envelope);
    let commands = compute_commands_diff(&baseline_commands, &head_commands);
    diff.breaking.extend(commands.breaking);
    diff.additive.extend(commands.additive);
    let pass = bump_satisfies(
        &baseline_version,
        &head_version,
        !diff.breaking.is_empty(),
        !diff.additive.is_empty(),
    );
    let verdict = Verdict {
        baseline_version: baseline_version.to_string(),
        head_version: head_version.to_string(),
        breaking: diff.breaking,
        additive: diff.additive,
        verdict: if pass { "pass" } else { "fail" },
    };
    print_json(&verdict, false);
    Ok(if pass { 0 } else { 1 })
}

/// Unwrap the `.data` payload of a raw `nodex export` envelope read
/// from `path`. Only a *successful* envelope (`ok: true`,
/// no error branch) is admitted: diffing an error envelope's body —
/// or any payload that merely happens to carry `.data` — would let the
/// gate pass vacuously on garbage.
fn load_payload(path: &str) -> Result<Value, GateError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| GateError::Io(format!("cannot read {path}: {e}")))?;
    let envelope: Value = serde_json::from_str(&raw)
        .map_err(|e| GateError::Usage(format!("{path} is not JSON: {e}")))?;
    if let Some(error) = envelope.get("error") {
        return Err(GateError::Usage(format!(
            "{path} carries an error envelope ({error}) — expected a successful \
             `nodex export` run"
        )));
    }
    if envelope.get("ok").and_then(Value::as_bool) != Some(true) {
        let found = match envelope.get("ok") {
            None => "no `ok` field".to_string(),
            Some(value) => format!("`ok` is {value}"),
        };
        return Err(GateError::Usage(format!(
            "{path} is not a successful envelope ({found}) — expected raw \
             `nodex export` stdout"
        )));
    }
    envelope.get("data").cloned().ok_or_else(|| {
        GateError::Usage(format!(
            "{path} carries no `.data` — expected raw `nodex export` stdout"
        ))
    })
}

/// The version one release's two manifests share; manifests of two
/// different releases would classify a change neither release made.
fn release_version(
    (envelope, envelope_path): (&Value, &str),
    (commands, commands_path): (&Value, &str),
) -> Result<semver::Version, GateError> {
    let version = version_of(envelope, envelope_path)?;
    let commands_version = version_of(commands, commands_path)?;
    if commands_version != version {
        return Err(GateError::Usage(format!(
            "{envelope_path} is version {version} but {commands_path} is version \
             {commands_version}; pass the two manifests of one release"
        )));
    }
    Ok(version)
}

fn version_of(payload: &Value, path: &str) -> Result<semver::Version, GateError> {
    let version = payload
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| GateError::Usage(format!("{path} carries no `.data.version` string")))?;
    semver::Version::parse(version).map_err(|e| {
        GateError::Usage(format!(
            "{path} version {version:?} is not valid SemVer: {e}"
        ))
    })
}

/// The CODEGEN.md promise: a release whose envelope schema changed in
/// any classified way must bump beyond patch. Pre-1.0, the 0.x minor
/// component is the breaking component, so every classified change
/// (breaking or additive) requires it — or the major — to increase.
/// From 1.0 on, additive needs at least a minor bump and breaking a
/// major one. No classified change passes unconditionally.
fn bump_satisfies(
    baseline: &semver::Version,
    head: &semver::Version,
    breaking: bool,
    additive: bool,
) -> bool {
    if !breaking && !additive {
        return true;
    }
    let major_bumped = head.major > baseline.major;
    let minor_bumped =
        major_bumped || (head.major == baseline.major && head.minor > baseline.minor);
    if baseline.major == 0 {
        return minor_bumped;
    }
    if breaking {
        return major_bumped;
    }
    minor_bumped
}
