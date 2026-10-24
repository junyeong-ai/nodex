//! Helpers shared by more than one module.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// The `message` of a typed `{ code, message }` envelope warning.
/// Plain string arrays (paths, ids, schema field names) are read
/// directly with `Value::as_str` — this accessor is for the `warnings`
/// plane only, so a call site's choice of reader documents which shape
/// it expects.
pub(crate) fn warning_msg(v: &Value) -> Option<&str> {
    v.get("message").and_then(Value::as_str)
}

pub(crate) fn nodex(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("nodex").expect("nodex binary in cargo target");
    cmd.arg("-C").arg(dir);
    cmd
}

/// Run the command and parse stdout as JSON, asserting the envelope
/// wrapper invariants. Returns the parsed `data` field on success.
pub(crate) fn run_json(cmd: &mut Command) -> Value {
    run_envelope(cmd)
        .get("data")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Same as [`run_json`] but returns the full envelope so callers can
/// inspect `ok` / `warnings` / `error` directly. Use this whenever the
/// assertion is about the envelope itself, not the `data` payload.
pub(crate) fn run_envelope(cmd: &mut Command) -> Value {
    let output = cmd.output().expect("command ran");
    assert!(
        output.status.success(),
        "command failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("stdout is parseable JSON");
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(true)));
    parsed
}

pub(crate) fn scratch() -> TempDir {
    tempfile::tempdir().expect("create tempdir")
}

pub(crate) fn write_doc(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, body).unwrap();
}

/// A `git` runner pinned to `root` with a deterministic identity and no
/// gpg signing — the substrate for tests that need an `immutable_baseline`.
///
/// Built through the same seam production code uses, so a fixture cannot
/// be redirected by the environment the suite inherits: under an ambient
/// `GIT_DIR` a raw `git init` initialises *that* repository and the
/// fixture's commits land in it, which mutates a repository the test
/// never named.
pub(crate) fn git_runner(root: &std::path::Path) -> impl Fn(&[&str]) -> std::process::Output + '_ {
    move |args: &[&str]| {
        let git = || {
            let mut git = nodex_core::git::command(root).expect("git on PATH");
            git.env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_CONFIG_GLOBAL", "/dev/null");
            git
        };
        let out = git().args(args).output().expect("git ran");
        if args.first() == Some(&"init") {
            // commit.gpgsign off so signing isn't required in CI.
            git()
                .args(["config", "commit.gpgsign", "false"])
                .output()
                .expect("git config ran");
        }
        out
    }
}

pub(crate) fn init_project(root: &std::path::Path) {
    nodex(root).arg("init").assert().success();
}

/// The config a subdirectory-project fixture writes: an immutability
/// baseline plus a frozen-body lock, so both planes the baseline governs
/// (the `check` diff and the write-seam locks) are live.
pub(crate) const LOCKED_PROJECT_CONFIG: &str = r#"
[scope]
include = ["docs/**/*.md"]
[statuses]
allowed = ["active", "archived"]
terminal = ["archived"]
initial = "active"
[[identity.id_rules]]
kind = "*"
template = "{kind}-{stem}"
[parser]
wikilink_enabled = true
[rules]
immutable_baseline = "HEAD"
[[rules.body_immutable]]
name = "frozen"
mode = "frozen"
trigger = "terminal"
kinds = ["generic"]
"#;

/// Every path under `dir`, recursively — for assertions about what a run
/// left behind.
pub(crate) fn walk_entries(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            found.push(path);
        }
    }
    found
}

/// The envelope a command emitted, whether it succeeded or not — the
/// spelling guard's tests assert on both outcomes, because a
/// case-sensitive volume gates the same spelling as an ordinary new path.
pub(crate) fn envelope_of(cmd: &mut Command) -> Value {
    let output = cmd.output().expect("command ran");
    serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim())
        .expect("stdout is parseable JSON")
}

pub(crate) fn flow_project(root: &std::path::Path, rules: &str) {
    fs::write(
        root.join("nodex.toml"),
        format!(
            "[kinds]\nallowed = [\"adr\", \"generic\"]\n\
             [statuses]\nallowed = [\"proposed\", \"active\", \"superseded\"]\n\
             terminal = [\"superseded\"]\n\
             [[statuses.flow]]\nkinds = [\"adr\"]\ninitial = \"proposed\"\n\
             transitions = {{ proposed = [\"active\"], active = [\"superseded\"] }}\n\
             [scope]\ninclude = [\"docs/**/*.md\"]\n\
             [[identity.kind_rules]]\nglob = \"docs/**/*.md\"\nkind = \"adr\"\n\
             [detection]\norphan_ok_kinds = [\"adr\"]\n{rules}"
        ),
    )
    .unwrap();
    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
}

pub(crate) fn adr(root: &std::path::Path, id: &str, status: &str, body: &str) {
    write_doc(
        root,
        &format!("docs/{id}.md"),
        &format!("---\nid: {id}\ntitle: {id}\nstatus: {status}\n---\n{body}\n"),
    );
}

/// The whole envelope, whatever the exit code: a `check` that found
/// something exits non-zero and still answers.
pub(crate) fn reported(cmd: &mut Command) -> Value {
    let output = cmd.output().expect("check ran");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope
}

/// A check's report, whatever its verdict: a run expected to red still has a
/// reach and a violation list to read.
pub(crate) fn judged(cmd: &mut Command) -> Value {
    reported(cmd)["data"].clone()
}

pub(crate) fn staged(root: &std::path::Path) -> Command {
    let mut cmd = nodex(root);
    cmd.args(["check", "--staged"]);
    cmd
}

pub(crate) fn error_code(cmd: &mut Command) -> Value {
    let output = cmd.output().expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    envelope["error"]["code"].clone()
}

pub(crate) fn snapshot_lock_project(root: &std::path::Path, mode: &str) {
    fs::write(root.join("nodex.toml"), format!(
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"open\", \"accepted\", \"superseded\"]\nterminal = [\"superseded\"]\n\
         [rules]\nimmutable_baseline = \"main\"\nimmutable_baseline_mode = {mode:?}\nimmutable_history = \"snapshot\"\n\
         [[rules.body_immutable]]\nname = \"sealed\"\nmode = \"append_only\"\nappend_section = \"## Corrections\"\ntrigger = \"status\"\nstatuses = [\"accepted\", \"superseded\"]\n\
         [[rules.frontmatter_immutable]]\nname = \"sealed\"\nfields = [\"*\"]\nexcept = [\"status\", \"superseded_by\", \"covers\"]\ntrigger = \"status\"\nstatuses = [\"accepted\", \"superseded\"]\n"
    )).unwrap();
    fs::write(root.join(".gitignore"), "_index/\n").unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nstatus: accepted\n---\nOriginal.\n",
    );
    let git = git_runner(root);
    assert!(git(&["init", "-b", "main"]).status.success());
    assert!(git(&["add", "."]).status.success());
    assert!(git(&["commit", "-qm", "baseline"]).status.success());
    assert!(git(&["checkout", "-qb", "topic"]).status.success());
}
