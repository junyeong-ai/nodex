//! Behavioural snapshot sweep.
//!
//! The unit tests beside this one are an allowlist: each names an
//! expectation and checks it. A regression lands in the complement —
//! behaviour nobody named, changed by a fix aimed somewhere else. This
//! sweep is that complement. It runs every read command against every
//! fixture and pins the whole envelope, so any change to any byte of
//! any answer shows up as a reviewable diff instead of as silence.
//!
//! The output of a run is a diff, never a verdict. `cargo insta review`
//! is where a diff is judged intended or not; nothing here decides that,
//! and nothing accepts a snapshot on the author's behalf.
//!
//! Two properties make the sweep honest:
//!
//! - **Closed over the command surface.** The covered set is checked
//!   against `nodex export commands` — the binary's own list — so a new
//!   subcommand fails [`every_command_is_swept_or_exempt`] until someone
//!   sweeps it or writes down why not. A hand-maintained list would be
//!   the same allowlist this sweep exists to complement.
//! - **Closed over time.** Every invocation pins `--today`, so a verdict
//!   that moves with the calendar is a defect rather than a flake.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The date every swept command measures against. Fixture dates are
/// authored around it, so staleness, orphan grace, and trust freshness
/// all land on their interesting side.
const TODAY: &str = "2026-06-15";

/// A fixture project, copied to a tempdir before each run so a command
/// that writes cannot leak into the next observation.
const FIXTURES: &[&str] = &["minimal", "graph"];

/// Invocations whose argv names nothing fixture-specific, run against every
/// fixture in [`FIXTURES`].
const SHARED: &[&[&str]] = &[
    &["status"],
    &["check"],
    &["report"],
    &["query", "nodes"],
    &["query", "orphans"],
    &["query", "stale"],
    &["query", "issues"],
    &["query", "components"],
    &["query", "annotations"],
    &["query", "recent", "--days", "365"],
    &["query", "trust", "--bottom", "5"],
    &["query", "node", "no-such-id"],
    &["export", "schema"],
    &["export", "enums"],
    &["export", "rules"],
    &["export", "envelope-schema"],
    &["export", "config"],
    &["export", "commands"],
    &["scaffold", "--kind", "adr", "--title", "Swept", "--dry-run"],
    &["migrate"],
];

/// Invocations whose argv names an id, a path, or a term only one fixture
/// carries. Sharing one argv list across fixtures made these answer
/// `NOT_FOUND` on the fixture that does not hold the subject, so the pin
/// recorded a lookup miss rather than the command.
const PER_FIXTURE: &[(&str, &[&str])] = &[
    ("minimal", &["query", "node", "guide-overview"]),
    ("minimal", &["query", "backlinks", "adr-storage"]),
    ("minimal", &["query", "neighborhood", "guide-overview"]),
    ("minimal", &["query", "dependents", "adr-storage"]),
    ("minimal", &["query", "similar", "--id", "guide-overview"]),
    ("minimal", &["query", "search", "storage"]),
    ("minimal", &["query", "covered-by", "nodex.toml"]),
    ("graph", &["query", "node", "adr-alpha"]),
    ("graph", &["query", "backlinks", "adr-beta"]),
    ("graph", &["query", "chain", "adr-alpha"]),
    ("graph", &["query", "neighborhood", "guide-index"]),
    ("graph", &["query", "dependents", "adr-beta"]),
    ("graph", &["query", "similar", "--id", "adr-alpha"]),
    ("graph", &["query", "search", "beta"]),
    ("graph", &["query", "covered-by", "nodex.toml"]),
];

/// Every invocation the sweep runs against `fixture`.
fn invocations_for(fixture: &str) -> Vec<&'static [&'static str]> {
    SHARED
        .iter()
        .copied()
        .chain(
            PER_FIXTURE
                .iter()
                .filter(|(owner, _)| *owner == fixture)
                .map(|(_, argv)| *argv),
        )
        .collect()
}

/// Leaf commands the sweep does not cover, each with the reason it
/// cannot be a pure function of (fixture, date) the way the rest are.
/// [`every_command_is_swept_or_exempt`] holds this list against the
/// binary's own command surface, so an exemption is a written decision
/// rather than an omission.
const EXEMPT: &[(&[&str], &str)] = &[
    (
        &["init"],
        "writes a nodex.toml the fixtures already carry; the file it emits is pinned by the unit tests",
    ),
    (
        &["build"],
        "the sweep's own precondition — every observation runs it first, and its envelope carries a duration",
    ),
    (
        &["diff"],
        "takes two git refs; a git-history fixture is its own corpus, not this one",
    ),
    (&["impact"], "takes two git refs, as `diff` does"),
    (
        &["rename"],
        "mutates the tree; the post-state belongs to a mutation corpus that snapshots files, not envelopes",
    ),
    (&["retarget"], "mutates the tree, as `rename` does"),
    (
        &["lifecycle", "supersede"],
        "mutates frontmatter, as `rename` does",
    ),
    (
        &["lifecycle", "set"],
        "mutates frontmatter, as `rename` does",
    ),
    (
        &["lifecycle", "review"],
        "mutates frontmatter, as `rename` does",
    ),
    (
        &["export", "diagnostics"],
        "reports the host toolchain, so its answer is a property of the machine rather than of the graph",
    ),
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/behaviour/corpus")
}

/// Copy a fixture into a fresh tempdir. Each observation gets its own,
/// so a command that writes — `build`'s snapshot, `migrate`'s plan —
/// cannot carry state into the next.
fn stage(fixture: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    copy_tree(&corpus_root().join(fixture), dir.path());
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create staged dir");
    for entry in std::fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// Stand-in for the staging directory in snapshot bodies. The path is a
/// fresh tempdir per observation, so it is the one part of an envelope
/// that is genuinely different every run; pinning it would pin the
/// filesystem rather than the behaviour. Everything else is pinned
/// verbatim — this is the only normalisation the sweep performs.
const STAGED: &str = "<staged>";

/// Run one command against a staged fixture and return its envelope as
/// the snapshot body: exit code, then pretty JSON so a diff lands on the
/// field that changed rather than on one long line.
fn observe(dir: &Path, argv: &[&str]) -> String {
    let mut cmd = Command::cargo_bin("nodex").expect("nodex binary");
    cmd.arg("-C").arg(dir).arg("--today").arg(TODAY).args(argv);
    let out = cmd.output().expect("command ran");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let body = match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(v) => serde_json::to_string_pretty(&v).expect("re-encode"),
        // Not JSON: the envelope contract is itself the thing that broke,
        // so pin the raw bytes rather than hiding them behind a parse error.
        Err(_) => stdout.into_owned(),
    };
    let body = normalise(&body, dir);
    format!("exit: {}\n{body}\n", out.status.code().unwrap_or(-1))
}

/// Replace the staging path — including the symlink-resolved spelling
/// macOS hands back for `/var` — with [`STAGED`].
fn normalise(body: &str, dir: &Path) -> String {
    let mut out = body.to_string();
    let mut locations = vec![dir.to_string_lossy().into_owned()];
    if let Ok(resolved) = dir.canonicalize() {
        locations.push(resolved.to_string_lossy().into_owned());
    }
    // Each location can appear in three renderings, and a snapshot has to be
    // free of all of them: as the operating system spells it, forward-slashed
    // the way nodex's own path language writes it, and backslash-escaped the
    // way a JSON string carries a Windows path. Without the last one a
    // Windows snapshot keeps the absolute temp directory it ran in.
    let mut spellings: Vec<String> = locations
        .iter()
        .flat_map(|l| [l.clone(), l.replace('\\', "/"), l.replace('\\', "\\\\")])
        .collect();
    // Longest first: `/private/var/…` contains `/var/…` as a suffix, and
    // replacing the shorter one first would leave a half-rewritten path.
    spellings.sort_by_key(|s| std::cmp::Reverse(s.len()));
    spellings.dedup();
    for s in spellings {
        out = out.replace(&s, STAGED);
    }
    out
}

#[test]
fn behaviour_sweep() {
    for fixture in FIXTURES {
        let dir = stage(fixture);
        // `build` is the precondition for every graph-reading command;
        // its own envelope carries a duration and is exempt from the pins.
        Command::cargo_bin("nodex")
            .expect("nodex binary")
            .arg("-C")
            .arg(dir.path())
            .arg("--today")
            .arg(TODAY)
            .arg("build")
            .output()
            .expect("build ran");

        for argv in invocations_for(fixture) {
            let name = format!("{}__{}", fixture, argv.join("_").replace(['-', '/'], "_"));
            let observed = observe(dir.path(), argv);
            assert!(
                !observed.contains("\"INVALID_ARGUMENT\""),
                "{name} pins a clap usage error — the argv is malformed, which is a \
                 property of this file rather than of the fixture: {observed}"
            );
            assert!(
                !observed.contains("\"NOT_FOUND\"") || argv.contains(&"no-such-id"),
                "{name} pins a lookup miss — the subject it names is not in this \
                 fixture, so the pin records the miss instead of the command: {observed}"
            );
            insta::assert_snapshot!(name, observed);
        }
    }
}

/// The sweep's coverage is closed against the binary's own command list.
/// A new subcommand is a failure here until it is swept or written into
/// [`EXEMPT`] with its reason — the property that keeps this file from
/// decaying into the partial allowlist it exists to complement.
#[test]
fn every_command_is_swept_or_exempt() {
    let out = Command::cargo_bin("nodex")
        .expect("nodex binary")
        .args(["export", "commands"])
        .output()
        .expect("export commands ran");
    let doc: Value = serde_json::from_slice(&out.stdout).expect("commands envelope is JSON");
    let declared: Vec<Vec<String>> = doc["data"]["commands"]
        .as_array()
        .expect("commands array")
        .iter()
        .map(|c| {
            c["path"]
                .as_array()
                .expect("command path")
                .iter()
                .map(|s| s.as_str().expect("path segment").to_string())
                .collect()
        })
        .collect();

    // A swept argv starts with its command path and continues into flags
    // and positionals, so a declared path is covered when it is a prefix
    // of some swept argv.
    let covered = |path: &[String]| {
        SHARED
            .iter()
            .copied()
            .chain(PER_FIXTURE.iter().map(|(_, argv)| *argv))
            .any(|argv| argv.len() >= path.len() && argv[..path.len()] == path[..])
    };
    let exempt = |path: &[String]| EXEMPT.iter().any(|(p, _)| *p == path);

    let uncovered: Vec<String> = declared
        .iter()
        .filter(|path| !covered(path) && !exempt(path))
        .map(|path| path.join(" "))
        .collect();
    assert!(
        uncovered.is_empty(),
        "these commands are neither swept nor exempt — add them to SHARED or PER_FIXTURE, \
         or to EXEMPT with the reason they cannot be swept: {uncovered:?}"
    );

    // The reverse direction: an exemption for a command that no longer
    // exists reads as coverage of a surface that is gone.
    let stale: Vec<String> = EXEMPT
        .iter()
        .filter(|(p, _)| !declared.iter().any(|d| d.as_slice() == *p))
        .map(|(p, _)| p.join(" "))
        .collect();
    assert!(
        stale.is_empty(),
        "these EXEMPT entries name commands the binary no longer has: {stale:?}"
    );
}
