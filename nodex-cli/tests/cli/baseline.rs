//! The immutability baseline: how a ref's project is located and read, the
//! same way by `check` and by the write seams.

#[cfg(unix)]
use crate::support::envelope_of;
use crate::support::{
    LOCKED_PROJECT_CONFIG, git_runner, init_project, nodex, run_envelope, run_json, scratch,
    snapshot_lock_project, walk_entries, warning_msg, write_doc,
};
use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

#[test]
fn check_since_surfaces_a_baseline_parse_warning() {
    // A document unparseable AT the baseline vanishes from the before
    // graph, so it looks "added" and its diff-aware immutability rules
    // silently never fire — `check --since` would pass on a lock it never
    // enforced. The rule pass only sees the CURRENT graph's parse
    // failures, so the baseline's recorded drop must reach the envelope
    // as a ref-tagged warning for the operator to see the gap.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    // Malformed frontmatter at the baseline commit.
    write_doc(
        root,
        "docs/a.md",
        "---\ntitle: [unclosed\nbad: : :\n---\n# A\n",
    );
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Repair it in the working tree.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nstatus: archived\n---\n# A body\n",
    );
    nodex(root).arg("build").assert().success();

    let env = run_envelope(nodex(root).args(["check", "--since", "HEAD"]));
    let warnings: Vec<&str> = env
        .get("warnings")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(warning_msg).collect())
        .unwrap_or_default();
    assert!(
        warnings.iter().any(|w| w.contains("baseline HEAD")
            && w.contains("docs/a.md")
            && w.contains("diff-aware rules are inert")),
        "baseline parse warning must surface: {warnings:?}"
    );
}

#[test]
fn query_issues_runs_the_same_baseline_as_check() {
    // `query issues` resolves rules.immutable_baseline through the same
    // substrate as a default `check`, so the two can never disagree
    // about immutability violations.
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "test"]);
    git(&["config", "commit.gpgsign", "false"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\noriginal\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    // Tamper with the locked body in the working tree.
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: archived\n---\n# D\n\nTAMPERED\n",
    );
    nodex(root).arg("build").assert().success();

    // check (default baseline) flags it…
    nodex(root).arg("check").assert().failure().code(1);
    // …and query issues reports the SAME violation instead of a skip.
    let data = run_json(nodex(root).args(["query", "issues"]));
    let violations = data
        .get("violations")
        .and_then(Value::as_array)
        .expect("violations");
    assert!(
        violations
            .iter()
            .any(|v| v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/frozen")),
        "issues carries the baseline violation: {data}"
    );
}

#[test]
fn git_backed_rules_measure_the_project_under_any_inherited_environment() {
    // Git selects a repository from the environment before it looks at a
    // working directory, and reinterprets path arguments from it too, so
    // an inherited variable is enough to decide what gets measured:
    // drift counted against a foreign repository (where the project's
    // paths carry no history, so the finding disappears instead of
    // erring), a baseline read from foreign bytes, a work-tree probe
    // answering `true` for a directory holding no repository. The
    // exporters are ordinary — server-side hooks export GIT_DIR with an
    // absolute GIT_OBJECT_DIRECTORY, `git submodule foreach` exports
    // GIT_DIR, and every shell-based git subcommand sourcing
    // git-sh-setup exports it.
    //
    // The property is one verdict: the project decides what is measured,
    // so every environment below produces the identical finding. It is
    // asserted as a property rather than per-mechanism, because the
    // failure mode is a finding that quietly stops appearing.
    let tmp = scratch();
    let root = tmp.path();
    let elsewhere = scratch();
    let git = git_runner(root);
    git(&["init", "-q"]);
    // A real second repository: a GIT_DIR naming nothing would make git
    // fail loudly, which is not the case under test.
    git_runner(elsewhere.path())(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [detection]\ngit_drift_threshold = 1\n",
    )
    .unwrap();
    // The covered path carries a glob metacharacter and a sibling the
    // pattern would match: an inherited GIT_GLOB_PATHSPECS turns the path
    // into a pattern and folds the sibling's history into the count, and
    // the `--literal-pathspecs` that pins interpretation is itself
    // rejected by git while such a variable is set. So the assertion is
    // the measured number, not merely the presence of a finding — a
    // verdict that survives by coincidence is not a verdict.
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n\
         reviewed: 2020-01-01\ncovers:\n  - \"src/a[1].rs\"\n---\n# D\n",
    );
    fs::create_dir_all(root.join("src")).unwrap();
    for change in 0..2 {
        fs::write(root.join("src/a[1].rs"), format!("// {change}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "covered"]);
    }
    for change in 0..3 {
        fs::write(root.join("src/a1.rs"), format!("// {change}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "not covered"]);
    }
    nodex(root).arg("build").assert().success();

    let drift = |cmd: &mut Command| -> Vec<u64> {
        run_json(cmd)
            .get("violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter(|v| v.get("rule_id").and_then(Value::as_str) == Some("git_drift"))
            .map(|v| {
                v.pointer("/details/total_commits")
                    .and_then(Value::as_u64)
                    .expect("git_drift carries its commit count")
            })
            .collect()
    };
    assert_eq!(
        drift(nodex(root).arg("check")),
        [2],
        "the project's own commits on the covered path, and no others"
    );

    let foreign_git_dir = elsewhere.path().join(".git");
    let hostile: Vec<(&str, PathBuf)> = vec![
        ("GIT_DIR", foreign_git_dir.clone()),
        ("GIT_COMMON_DIR", foreign_git_dir.clone()),
        ("GIT_WORK_TREE", elsewhere.path().to_path_buf()),
        ("GIT_OBJECT_DIRECTORY", foreign_git_dir.join("objects")),
        ("GIT_INDEX_FILE", foreign_git_dir.join("index")),
        // Reinterpret the paths the probe passes.
        ("GIT_ICASE_PATHSPECS", PathBuf::from("1")),
        ("GIT_GLOB_PATHSPECS", PathBuf::from("1")),
        ("GIT_NOGLOB_PATHSPECS", PathBuf::from("1")),
        ("GIT_LITERAL_PATHSPECS", PathBuf::from("1")),
    ];
    for (var, value) in hostile {
        assert_eq!(
            drift(nodex(root).arg("check").env(var, &value)),
            [2],
            "an inherited {var} must not change what the project measures"
        );
    }
}

#[test]
fn check_and_query_issues_surface_the_same_inert_baseline_advisory() {
    // A configured baseline that cannot engage (immutability rules
    // declared, root not a git work tree) leaves the diff-aware rules
    // inert. Both consumers of the shared baseline resolution surface
    // the identical advisory — neither goes silently green about it,
    // and the wording is constructed once in the substrate.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\n\
         terminal = [\"archived\"]\ninitial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
    );
    nodex(root).arg("build").assert().success();

    let advisory = |env: &Value| -> Option<String> {
        env.get("warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(warning_msg)
            .find(|w| w.contains("immutability rules are inert this run"))
            .map(str::to_string)
    };

    let check_env = run_envelope(nodex(root).arg("check"));
    let check_warning = advisory(&check_env)
        .unwrap_or_else(|| panic!("check surfaces the inert advisory: {check_env}"));

    let issues_env = run_envelope(nodex(root).args(["query", "issues"]));
    let issues_warning = advisory(&issues_env)
        .unwrap_or_else(|| panic!("query issues surfaces the inert advisory: {issues_env}"));

    assert_eq!(
        check_warning, issues_warning,
        "one advisory wording across both commands"
    );
}

/// A repository whose nodex project sits in `docs-site/` instead of at
/// the repository root — a monorepo documentation subproject, as ordinary
/// as a project that owns its repository.
///
/// The repository root carries a decoy at the same *relative* path whose
/// bytes differ from the project's. Git resolves `<ref>:docs/a.md` and a
/// checkout against the repository root, not against a working directory,
/// so anything that forgets where the project sits reads the decoy.
///
/// `decoy_status` is what makes the difference observable, and it differs
/// by plane. A body lock keys on the *baseline* status, so a decoy read
/// in place of the project's own file must be non-terminal for the write
/// plane (the lock would then not engage, and a prefix-less read shows up
/// as a performed write) and terminal for the read plane (the lock would
/// then engage on a document nobody touched, and a checkout graphed at
/// the wrong root shows up as a manufactured violation). A decoy that
/// trips the same lock either way makes the test agree with itself and
/// prove nothing. Returns the project root.
fn subdirectory_project(repo: &std::path::Path, decoy_status: &str) -> PathBuf {
    let git = git_runner(repo);
    git(&["init", "-q"]);
    let project = repo.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        &project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        &project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        &project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    write_doc(
        repo,
        "docs/a.md",
        &format!(
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: {decoy_status}\n---\n\
             # A\n\nthe repository root's own document, not the project's\n"
        ),
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    project
}

#[test]
fn immutability_locks_engage_for_a_subdirectory_project() {
    // The lock is judged against the document's committed bytes, which
    // the baseline reads by asking git for a path. A path written without
    // the project's prefix names the repository root's file — a different
    // document, or none — so the lock read no baseline and the write went
    // through: a frozen body silently rewritten, no warning, and a
    // `check` that stays green because it mismeasures the same way. The
    // verdict must be the one a project owning its repository gets.
    let tmp = scratch();
    // Non-terminal decoy: reading it in place of the project's own
    // baseline would leave the lock disengaged, so a refused write can
    // only come from the project's own frozen document.
    let project = subdirectory_project(tmp.path(), "active");
    nodex(&project).arg("build").assert().success();

    let frozen_before = fs::read_to_string(project.join("docs/a.md")).unwrap();
    let envelope = run_envelope(nodex(&project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope
            .pointer("/data/total_updated")
            .and_then(Value::as_u64),
        Some(0),
        "a frozen body must not be repointed: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("body_immutable/frozen")),
        "the refusal names the lock that held: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(project.join("docs/a.md")).unwrap(),
        frozen_before,
        "nothing was written"
    );
}

#[test]
fn a_repository_root_document_never_stands_in_for_a_subdirectory_project() {
    // The mirror image of the lock case: with the baseline built from the
    // repository root, the decoy's body differed from the project's
    // committed body under the same id, so the frozen-body rule fired on
    // a document the operator never touched. A manufactured violation is
    // as wrong as a missing one, and here nothing warns.
    let tmp = scratch();
    // Terminal decoy with a different body: a baseline graphed at the
    // repository root would fire the frozen lock on it, so a clean report
    // can only come from the project's own snapshot.
    let project = subdirectory_project(tmp.path(), "archived");
    nodex(&project).arg("build").assert().success();

    let envelope = run_envelope(nodex(&project).args(["check", "--since", "HEAD"]));
    let data = envelope.get("data").expect("data");
    assert_eq!(
        data.get("violations").and_then(Value::as_array),
        Some(&vec![]),
        "an untouched project has nothing to report: {envelope}"
    );
    let skipped: Vec<&str> = data
        .get("skipped_rules")
        .and_then(Value::as_array)
        .expect("skipped_rules")
        .iter()
        .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
        .collect();
    assert!(
        !skipped.iter().any(|id| id.contains("immutable")),
        "the diff-aware rules ran — the baseline engaged: {envelope}"
    );
    assert_eq!(
        envelope.get("warnings").and_then(Value::as_array),
        None,
        "a baseline that engaged has nothing to advise about: {envelope}"
    );
}

#[test]
fn a_baseline_predating_the_project_directory_is_inert_not_an_error() {
    // A subdirectory project introduced after the baseline ref has no
    // snapshot there — ordinary for a branch that adds the project. The
    // run must neither fail nor go quietly green: the ref carries no
    // project, so the diff-aware rules report themselves skipped and the
    // advisory names the directory the ref does not have.
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);
    let base = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let project = repo.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            &format!("immutable_baseline = \"{base}\""),
        ),
    )
    .unwrap();
    write_doc(
        &project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "introduce the project"]);
    nodex(&project).arg("build").assert().success();

    let envelope = run_envelope(nodex(&project).arg("check"));
    let skipped: Vec<&str> = envelope
        .pointer("/data/skipped_rules")
        .and_then(Value::as_array)
        .expect("skipped_rules")
        .iter()
        .filter_map(|r| r.get("rule_id").and_then(Value::as_str))
        .collect();
    assert_eq!(
        skipped,
        ["body_immutable/frozen"],
        "a rule that cannot fire says so: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("docs-site") && m.contains("does not carry")),
        "the advisory names the directory the ref does not carry: {envelope}"
    );
}

#[test]
fn diff_refuses_a_ref_that_does_not_carry_the_project() {
    // `diff` and `impact` compare two snapshots of the *project*, so a
    // ref without it has nothing to compare — a typed GIT_ERROR naming
    // the ref, never a graph built from whatever else the checkout held.
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);

    // The project arrives in a second commit, so `HEAD~1` is a ref the
    // project does not exist at.
    let project = subdirectory_project(repo, "archived");
    nodex(&project).arg("build").assert().success();

    let output = nodex(&project)
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(
        envelope.pointer("/error/code").and_then(Value::as_str),
        Some("GIT_ERROR"),
        "{envelope}"
    );
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(
        message.contains("docs-site") && message.contains("does not carry this project"),
        "the error names what the ref lacks: {message}"
    );
}

/// `check` builds its baseline by checking the ref out, and a checkout
/// applies whatever `.gitattributes` declares. Reading the stored bytes
/// instead gives the write side a different document for the same commit —
/// git's own `ident` stamps the blob's sha into the body — so a
/// frontmatter-only rewrite trips the *body* lock on a document whose body
/// nobody touched.
#[test]
fn a_checkout_filter_does_not_make_the_two_planes_read_different_documents() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    fs::write(project.join(".gitattributes"), "*.md ident\n").unwrap();
    // The reference lives in frontmatter, so repointing it leaves the body
    // alone — and the body carries the stamp the checkout expands.
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n\
         related: generic-b\n---\n# A\n\nstamp: $Id$\n",
    );
    write_doc(
        project,
        "docs/wired.md",
        "---\nid: generic-wired\ntitle: W\nkind: generic\nstatus: archived\n---\n\
         # W\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "documents under an ident attribute"]);
    // Force the smudge, so the working tree holds the expanded stamp.
    fs::remove_file(project.join("docs/a.md")).unwrap();
    git(&["checkout", "-q", "--", "docs/a.md"]);
    let smudged = fs::read_to_string(project.join("docs/a.md")).unwrap();
    assert!(
        smudged.contains("$Id: "),
        "the checkout expanded the stamp: {smudged}"
    );
    nodex(project).arg("build").assert().success();

    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    // `docs/a.md` is a frontmatter-only rewrite and must land; `docs/wired.md`
    // is a body rewrite on a terminal document and must not.
    assert_eq!(
        envelope["data"]["references_updated"],
        serde_json::json!(["docs/a.md"]),
        "only the frontmatter rewrite lands: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("docs/wired.md") && m.contains("body_immutable/frozen")),
        "the body rewrite is still refused: {envelope}"
    );
}

/// `core.precomposeUnicode` (git's default on macOS) matches a decomposed
/// pathspec and reports the composed spelling, so a baseline read that
/// recognised records by comparing the path it asked for against the path
/// git reported found nothing — and nothing is what permits a write. The
/// answer has to come from asking one path per invocation, where the record
/// that returns *is* the answer.
#[test]
#[cfg(target_os = "macos")]
fn a_decomposed_document_name_is_locked_like_any_other() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    // An explicit combining acute: a literal precomposed character here
    // would make the test prove nothing.
    let decomposed = "docs/cafe\u{301}.md";
    assert_eq!(decomposed.as_bytes(), b"docs/cafe\xcc\x81.md");
    for (rel, id) in [
        (decomposed, "generic-nfd"),
        ("docs/ascii.md", "generic-ascii"),
    ] {
        write_doc(
            project,
            rel,
            &format!(
                "---\nid: {id}\ntitle: T\nkind: generic\nstatus: archived\n---\n\
                 # T\n\nsee [[generic-b]]\n"
            ),
        );
    }
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "a decomposed name and an ascii control",
    ]);
    // Git records the composed spelling of the decomposed name; the
    // filesystem keeps the decomposed one. That divergence is the fixture.
    let recorded = git(&["ls-tree", "--name-only", "-z", "-r", "HEAD"]).stdout;
    let composed = "caf\u{e9}.md".as_bytes();
    assert!(
        recorded.windows(composed.len()).any(|w| w == composed),
        "git recorded the composed spelling: {}",
        String::from_utf8_lossy(&recorded)
    );

    let sealed: Vec<_> = [decomposed, "docs/ascii.md"]
        .iter()
        .map(|rel| project.join(rel))
        .collect();
    for path in &sealed {
        let edited = format!("{}\nedited\n", fs::read_to_string(path).unwrap());
        fs::write(path, edited).unwrap();
    }
    nodex(project).arg("build").assert().success();

    let before: Vec<_> = sealed
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the decomposed name is locked exactly as the ascii control is: {envelope}"
    );
    let after: Vec<_> = sealed
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    assert_eq!(after, before, "both frozen bodies are untouched");
}

/// The crate folds `\` to `/` for a stable JSON path contract, but on unix
/// a `\` is an ordinary byte in a document's name. Folding it on the way to
/// git asks about a document that does not exist, and a lock that asks
/// about a path no ref records reads as "nothing to freeze" — so `check`
/// reported the frozen body and the write rewrote it anyway.
#[test]
#[cfg(unix)]
fn a_document_whose_name_holds_a_backslash_is_still_locked() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        project,
        "docs/back\\slash.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "a document whose name holds a backslash",
    ]);
    let sealed = project.join("docs/back\\slash.md");
    fs::write(
        &sealed,
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n\nedited\n",
    )
    .unwrap();
    nodex(project).arg("build").assert().success();

    let before = fs::read_to_string(&sealed).unwrap();
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the lock reaches a document whose name holds a separator-looking byte: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("body_immutable/frozen")),
        "and it is the real rule, not an unevaluated lock: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(&sealed).unwrap(),
        before,
        "the frozen body is untouched"
    );
}

/// "This ref names nothing" and "this ref does not hold the project" are
/// different facts. Collapsing them lets `check --since <typo>` report
/// every node as in scope and exit 0 — the CI-green-on-a-typo failure the
/// configured baseline already refuses — and makes `diff` blame the
/// project's location for a ref that does not exist.
#[test]
fn a_ref_that_names_nothing_is_refused_rather_than_read_as_a_missing_project() {
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);
    let project = subdirectory_project(repo, "archived");
    nodex(&project).arg("build").assert().success();

    for args in [
        vec!["diff", "no-such-ref", "HEAD"],
        vec!["impact", "no-such-ref", "HEAD"],
        vec!["check", "--since", "no-such-ref"],
    ] {
        let output = nodex(&project).args(&args).output().expect("ran");
        assert_eq!(
            output.status.code(),
            Some(2),
            "nodex {args:?} must refuse a ref git does not resolve"
        );
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        let message = envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .expect("message");
        assert!(
            message.contains("no-such-ref") && message.contains("no such ref"),
            "the refusal names the ref, not the project's location: {message}"
        );
    }

    // The ref that *does* resolve but lacks the project keeps its own
    // verdict — the distinction is the point.
    let output = nodex(&project)
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
        .expect("ran");
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert!(
        envelope
            .pointer("/error/message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains("does not carry this project")),
        "a resolvable ref without the project still says so: {envelope}"
    );
}

/// A ref may carry the project's *name* without carrying the project:
/// a gitlink is what a directory looks like before it stops being a
/// submodule, and `git worktree add` materialises an empty directory for
/// one it does not populate. Reading that directory as the project
/// graphs an empty baseline — every current document reported as newly
/// added, and `check --since` blaming `scope.include`.
#[test]
fn a_ref_recording_the_project_as_a_gitlink_carries_no_project() {
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);
    let vendored = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{vendored},docs-site"),
    ]);
    git(&["commit", "-q", "-m", "docs-site is a vendored submodule"]);

    let project = subdirectory_project(repo, "archived");
    nodex(&project).arg("build").assert().success();

    let output = nodex(&project)
        .args(["diff", "HEAD~1", "HEAD"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let message = envelope
        .pointer("/error/message")
        .and_then(Value::as_str)
        .expect("message");
    assert!(
        message.contains("docs-site") && message.contains("does not carry this project"),
        "a gitlink at the prefix is not the project: {message}"
    );

    let checked = run_envelope(nodex(&project).args(["check", "--since", "HEAD~1"]));
    assert!(
        checked
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("records no project directory at \"docs-site\"")),
        "the advisory names what the ref records, not the project's scope: {checked}"
    );
}

#[test]
fn diff_graphs_a_subdirectory_project_at_its_own_location() {
    // Both refs carry the project, so the comparison is between the
    // project's own two snapshots — the repository root's same-path
    // decoy takes part in neither.
    let tmp = scratch();
    let project = subdirectory_project(tmp.path(), "archived");
    let git = git_runner(tmp.path());
    write_doc(
        &project,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "add d"]);

    let data = run_json(nodex(&project).args(["diff", "HEAD~1", "HEAD"]));
    let added: Vec<&str> = data
        .get("added_nodes")
        .and_then(Value::as_array)
        .expect("added_nodes")
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(added, ["generic-d"], "{data}");
}

/// A repository whose path contains a newline — legal on a POSIX
/// filesystem. `git rev-parse` reports paths unquoted and has no
/// NUL-delimited mode, so answers cannot be told apart when one of them
/// spans lines; a binding read wrongly still looks bound, and then every
/// invocation built from it fails to spawn. Read as "nothing there", that
/// leaves a frozen body rewritable and a drift finding absent — the exact
/// two silent failures, so both are asserted here.
#[cfg(unix)]
#[test]
fn a_repository_path_that_spans_lines_measures_the_project_the_same() {
    let tmp = scratch();
    let repo = tmp.path().join("we\nird");
    fs::create_dir(&repo).unwrap();
    let git = git_runner(&repo);
    git(&["init", "-q"]);
    fs::write(
        repo.join("nodex.toml"),
        format!("{LOCKED_PROJECT_CONFIG}[detection]\ngit_drift_threshold = 1\n"),
    )
    .unwrap();
    write_doc(
        &repo,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        &repo,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        &repo,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    write_doc(
        &repo,
        "docs/d.md",
        "---\nid: generic-d\ntitle: D\nkind: generic\nstatus: active\n\
         reviewed: 2020-01-01\ncovers:\n  - src/code.rs\n---\n# D\n",
    );
    fs::create_dir_all(repo.join("src")).unwrap();
    for change in 0..2 {
        fs::write(repo.join("src/code.rs"), format!("// {change}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "churn"]);
    }
    nodex(&repo).arg("build").assert().success();

    let frozen_before = fs::read_to_string(repo.join("docs/a.md")).unwrap();
    let envelope = run_envelope(nodex(&repo).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope
            .pointer("/data/total_updated")
            .and_then(Value::as_u64),
        Some(0),
        "the lock reads the project's baseline, so a frozen body is refused: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(repo.join("docs/a.md")).unwrap(),
        frozen_before,
        "nothing was written"
    );

    let drift: Vec<u64> = run_json(nodex(&repo).arg("check"))
        .get("violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter(|v| v.get("rule_id").and_then(Value::as_str) == Some("git_drift"))
        .map(|v| {
            v.pointer("/details/total_commits")
                .and_then(Value::as_u64)
                .expect("git_drift carries its commit count")
        })
        .collect();
    assert_eq!(
        drift,
        [2],
        "the drift probe measures the project's own file"
    );
}

/// A subdirectory project introduced after its baseline ref has no
/// snapshot to lock against. `check` says so — and so must a write: the
/// per-document question ("no bytes at the baseline") looks identical to
/// a brand-new document, so a write plane that only ever asks it proceeds
/// on a frozen body believing there was nothing to freeze. Both planes
/// read one resolution, so both disclose the same fact.
#[test]
fn a_baseline_that_carries_nothing_for_the_project_is_disclosed_on_writes_too() {
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    write_doc(repo, "README.md", "before the project existed\n");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "root only"]);
    let base = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let project = repo.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            &format!("immutable_baseline = \"{base}\""),
        ),
    )
    .unwrap();
    write_doc(
        &project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        &project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        &project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "introduce the project"]);
    nodex(&project).arg("build").assert().success();

    let envelope = run_envelope(nodex(&project).args(["retarget", "generic-b", "generic-c"]));
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("docs-site") && m.contains("does not carry")),
        "a write against an empty baseline names what the ref lacks: {envelope}"
    );
}

/// A baseline ref git cannot resolve at all — a typo, or a ref never
/// fetched — leaves the immutability rules unable to fire *and* unable to
/// be enforced. Neither plane may go green on it: `check` would be
/// reporting on rules that can never run, and a write would be permitting
/// edits no lock could refuse.
#[test]
fn an_unreadable_baseline_ref_refuses_both_planes() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            "immutable_baseline = \"origin/main\"",
        ),
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        root,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        root,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let frozen_before = fs::read_to_string(root.join("docs/a.md")).unwrap();

    for args in [
        vec!["check"],
        vec!["retarget", "generic-b", "generic-c"],
        vec!["lifecycle", "set", "generic-b", "--status", "archived"],
    ] {
        let output = nodex(root).args(&args).output().expect("ran");
        assert_eq!(
            output.status.code(),
            Some(2),
            "nodex {args:?} must refuse an unreadable baseline"
        );
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        assert_eq!(
            envelope.pointer("/error/code").and_then(Value::as_str),
            Some("CONFIG_ERROR"),
            "{envelope}"
        );
        assert!(
            envelope
                .pointer("/error/message")
                .and_then(Value::as_str)
                .is_some_and(|m| m.contains("origin/main")),
            "the refusal names the ref: {envelope}"
        );
    }
    assert_eq!(
        fs::read_to_string(root.join("docs/a.md")).unwrap(),
        frozen_before,
        "nothing was written"
    );

    // The verdict must hold when `HEAD` is what names nothing. A
    // repository whose refs still reach this history has a baseline the
    // operator can fix, so an unknown ref stays a refusal rather than
    // becoming the advisory an empty repository earns.
    git(&["symbolic-ref", "HEAD", "refs/heads/ghost"]);
    let output = nodex(root).arg("check").output().expect("ran");
    assert_eq!(
        output.status.code(),
        Some(2),
        "an unborn HEAD over real history does not make an unknown ref inert"
    );
}

/// A rename is a move plus a reference rewrite. The move cannot be
/// undone, so a refusal must arrive while the tree is still untouched:
/// refusing between the two halves leaves the file moved and every
/// reference to it dangling, which is worse than either outcome the
/// operator asked for.
#[test]
fn a_refused_rename_leaves_the_tree_untouched() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            "immutable_baseline = \"origin/main\"",
        ),
    )
    .unwrap();
    write_doc(
        root,
        "docs/r.md",
        "---\nid: generic-r\ntitle: R\nkind: generic\nstatus: active\n---\n\
         # R\n\nsee [c](c.md)\n",
    );
    write_doc(
        root,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    nodex(root).arg("build").assert().success();
    let before: Vec<String> = walk_entries(root)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let referrer_before = fs::read_to_string(root.join("docs/r.md")).unwrap();

    let output = nodex(root)
        .args(["rename", "docs/c.md", "docs/c2.md"])
        .output()
        .expect("ran");
    assert_eq!(output.status.code(), Some(2), "the rename is refused");

    assert!(
        root.join("docs/c.md").exists() && !root.join("docs/c2.md").exists(),
        "the document did not move"
    );
    assert_eq!(
        fs::read_to_string(root.join("docs/r.md")).unwrap(),
        referrer_before,
        "no reference was rewritten"
    );
    let mut after: Vec<String> = walk_entries(root)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(after, before, "nothing was created or removed");
}

/// A project set up before its first commit has recorded nothing to
/// compare against — the same "no snapshot" state as a ref that predates
/// the project, not a ref the operator spelled wrong. Refusing here would
/// block `git init` → `nodex init` → `nodex scaffold` on a baseline that
/// is perfectly correct.
#[test]
fn a_repository_with_no_commits_yet_is_inert_not_refused() {
    let tmp = scratch();
    let root = tmp.path();
    git_runner(root)(&["init", "-q"]);
    fs::write(root.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();

    let envelope = run_envelope(nodex(root).arg("check"));
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("no ref in the repository names a commit")),
        "the advisory names the condition it measured: {envelope}"
    );
    let scaffolded = run_envelope(
        nodex(root)
            .args(["scaffold", "--kind", "generic", "--title", "New"])
            .args(["--path", "docs/new.md"]),
    );
    assert!(
        scaffolded.pointer("/data/written").and_then(Value::as_bool) == Some(true),
        "a write proceeds against a repository with no history: {scaffolded}"
    );
}

/// The project's directory has to be a *directory* at the baseline. A ref
/// that records that name as a file resolves just as happily, and binding
/// to it leaves every document lookup empty — a baseline that reads as
/// "nothing is frozen" for the whole project, silently.
#[test]
fn a_baseline_recording_the_project_path_as_a_file_is_not_a_baseline() {
    let tmp = scratch();
    let repo = tmp.path();
    let git = git_runner(repo);
    git(&["init", "-q"]);
    // `docs-site` is a plain file at this commit.
    fs::write(repo.join("docs-site"), "not a directory yet\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "file at the project's path"]);
    let base = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    fs::remove_file(repo.join("docs-site")).unwrap();
    let project = repo.join("docs-site");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace(
            "immutable_baseline = \"HEAD\"",
            &format!("immutable_baseline = \"{base}\""),
        ),
    )
    .unwrap();
    write_doc(
        &project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        &project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        &project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the project replaces the file"]);
    nodex(&project).arg("build").assert().success();

    let envelope = run_envelope(nodex(&project).args(["retarget", "generic-b", "generic-c"]));
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("does not carry")),
        "a name recorded as a file carries no project: {envelope}"
    );
}

/// A document's baseline is the *file* the ref records at its path.
/// Consolidating a directory of notes into one document of the same name
/// is an ordinary refactor, and it leaves the document with no baseline:
/// `check` reads it as new and locks nothing. A type-agnostic read hands
/// the write seam the directory's listing instead — a prior snapshot where
/// there is none — and a creation-triggered lock freezes a document on its
/// first appearance, refusing a write `check` does not flag.
#[test]
fn a_document_whose_name_held_a_directory_at_the_baseline_is_new_on_both_planes() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace("trigger = \"terminal\"", "trigger = \"creation\""),
    )
    .unwrap();
    write_doc(project, "docs/a.md/note.md", "a note, once its own file\n");
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "docs/a.md is a directory of notes"]);

    fs::remove_dir_all(project.join("docs/a.md")).unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: active\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    nodex(project).arg("build").assert().success();

    let checked = run_json(nodex(project).arg("check"));
    assert!(
        !checked
            .get("violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .any(|v| v
                .get("rule_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("body_immutable/"))),
        "the read plane reads a first appearance as new: {checked}"
    );

    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 1,
        "the write plane refuses only what `check` flags: {envelope}"
    );
    assert!(
        fs::read_to_string(project.join("docs/a.md"))
            .unwrap()
            .contains("[[generic-c]]"),
        "the rewrite landed"
    );
}

/// Graphing the baseline runs the same build `check` runs, so it fails the
/// same typed ways — a duplicate id at the baseline is a duplicate id on
/// either plane. Reporting one condition under two codes is what a consumer
/// dispatching on the code cannot recover from; the prose naming the real
/// cause does not help it.
#[test]
fn both_planes_name_a_failed_baseline_build_with_the_same_code() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-dup\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/x.md",
        "---\nid: generic-dup\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the baseline carries a duplicate id"]);

    // The working tree is clean of the duplicate, so only the baseline build
    // fails and the failure is reached the same way from both planes.
    write_doc(
        project,
        "docs/x.md",
        "---\nid: generic-x\ntitle: X\nkind: generic\nstatus: active\n---\n# X\n",
    );
    nodex(project).arg("build").assert().success();

    let code = |args: &[&str]| {
        let output = nodex(project).args(args).output().expect("ran");
        let envelope: Value =
            serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
        envelope
            .pointer("/error/code")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    assert_eq!(
        code(&["check"]).as_deref(),
        Some("DUPLICATE_ID"),
        "the read plane names the cause"
    );
    assert_eq!(
        code(&["retarget", "generic-b", "generic-c"]).as_deref(),
        Some("DUPLICATE_ID"),
        "and the write plane names the same one"
    );
}

/// A baseline pairs documents by node id, so a document that moved since it
/// is the same document — the write seams decline an edit to it exactly as
/// `check` reports one. Addressing the baseline by path instead read a moved
/// document as new, and new is the answer that permits the write.
#[test]
fn a_document_moved_since_the_baseline_is_still_the_same_document() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    write_doc(
        project,
        "docs/old.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "generic-a lives at docs/old.md"]);

    // The document moves and keeps its id, which is what makes it the same
    // document to a baseline that pairs by id.
    git(&["mv", "docs/old.md", "docs/new.md"]);
    let moved = project.join("docs/new.md");
    fs::write(
        &moved,
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n\nedited after the move\n",
    )
    .unwrap();
    nodex(project).arg("build").assert().success();

    let output = nodex(project).arg("check").output().expect("ran");
    assert_eq!(
        output.status.code(),
        Some(1),
        "check reds the moved document"
    );

    let before = fs::read_to_string(&moved).unwrap();
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the write plane declines what `check` reds: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("body_immutable/frozen")),
        "and by the rule that governs it: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(&moved).unwrap(),
        before,
        "the frozen body is untouched"
    );
}

/// `identity.id_rules` are how a project declares ids once instead of in
/// every document, so a document that writes no `id:` is the ordinary case,
/// not a degenerate one. A probe that pairs on the id has to complete a
/// proposed document the way the build completes a stored one, or it pairs
/// on an id the document never had and finds no baseline — and no baseline
/// is the answer that permits the write.
#[test]
fn a_document_whose_id_its_config_supplies_is_locked_like_any_other() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    // No `id:` anywhere — every id comes from `identity.id_rules`.
    write_doc(
        project,
        "docs/old.md",
        "---\ntitle: A\nkind: generic\nstatus: archived\n---\n# A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "ids come from config"]);

    let frozen = project.join("docs/old.md");
    fs::write(
        &frozen,
        "---\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n\nedited in place\n",
    )
    .unwrap();
    nodex(project).arg("build").assert().success();

    let output = nodex(project).arg("check").output().expect("ran");
    assert_eq!(
        output.status.code(),
        Some(1),
        "check reds the frozen body it read through the same rules"
    );

    let before = fs::read_to_string(&frozen).unwrap();
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the write plane declines what `check` reds: {envelope}"
    );
    assert!(
        envelope
            .get("warnings")
            .and_then(Value::as_array)
            .expect("warnings")
            .iter()
            .filter_map(warning_msg)
            .any(|m| m.contains("body_immutable/frozen")),
        "and by the rule that governs it: {envelope}"
    );
    assert_eq!(
        fs::read_to_string(&frozen).unwrap(),
        before,
        "the frozen body is untouched"
    );
}

/// A baseline is one graph, built by checking the ref out, and the scanner
/// resolves symlinks by design. So a document the ref records as a link —
/// or one under a linked directory — has a baseline like any other, and its
/// locks fire by the rule that governs it rather than by a conservative
/// stand-in. Both planes read the same graph, so they cannot differ on
/// which documents have a baseline or on what it says.
#[test]
#[cfg(unix)]
fn a_document_reached_through_a_link_is_locked_by_the_rule_that_governs_it() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(
        project.join("nodex.toml"),
        LOCKED_PROJECT_CONFIG.replace("[scope]\n", "[scope]\nfollow_symlinks = true\n"),
    )
    .unwrap();
    // Outside `scope.include`, so it is only ever reached through the link.
    write_doc(
        project,
        "sealed_source.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    );
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    std::os::unix::fs::symlink("../sealed_source.md", project.join("docs/a.md")).unwrap();
    // The same divergence one level up: git records the link and nothing
    // below it, while a checkout has the whole subtree and the walk graphs
    // every document in it.
    write_doc(
        project,
        "real/v.md",
        "---\nid: generic-v\ntitle: V\nkind: generic\nstatus: archived\n---\n\
         # V\n\nsee [[generic-b]]\n",
    );
    std::os::unix::fs::symlink("../real", project.join("docs/vendor")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "docs/a.md and docs/vendor are links"]);

    // Each sealed document becomes one whose body differs from what the
    // checkout reads through the link, so `check` flags both.
    fs::remove_file(project.join("docs/a.md")).unwrap();
    write_doc(
        project,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n\nan added line\n",
    );
    write_doc(
        project,
        "real/v.md",
        "---\nid: generic-v\ntitle: V\nkind: generic\nstatus: archived\n---\n\
         # V\n\nsee [[generic-b]]\n\nan added line\n",
    );
    nodex(project).arg("build").assert().success();

    let output = nodex(project).arg("check").output().expect("ran");
    assert_eq!(output.status.code(), Some(1), "check reds both documents");
    let checked: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    let frozen: Vec<&str> = checked
        .pointer("/data/violations")
        .and_then(Value::as_array)
        .expect("violations")
        .iter()
        .filter(|v| v.get("rule_id").and_then(Value::as_str) == Some("body_immutable/frozen"))
        .filter_map(|v| v.get("node_id").and_then(Value::as_str))
        .collect();
    assert!(
        frozen.contains(&"generic-a") && frozen.contains(&"generic-v"),
        "the read plane resolves both links and finds frozen baselines: {checked}"
    );

    let before =
        ["docs/a.md", "real/v.md"].map(|rel| fs::read_to_string(project.join(rel)).unwrap());
    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    assert_eq!(
        envelope["data"]["total_updated"], 0,
        "the write plane does not rewrite what `check` reds: {envelope}"
    );
    let declined: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .filter(|m| m.contains("body_immutable/frozen"))
        .collect();
    assert_eq!(
        declined.len(),
        2,
        "each document is declined by the rule `check` reported, not a stand-in: {envelope}"
    );
    assert_eq!(
        ["docs/a.md", "real/v.md"].map(|rel| fs::read_to_string(project.join(rel)).unwrap()),
        before,
        "both sealed bodies are untouched"
    );
}

#[test]
fn a_relative_project_root_resolves_against_the_invoking_directory() {
    // `-C` accepts a relative path, and git invocations run in the
    // repository's work tree — so a root left relative would have every
    // path derived from it re-resolved against that work tree instead:
    // a baseline read outside the project, and a verdict that degrades
    // to inert because the project is not where the checkout was looked
    // for. How the root was spelled must not
    // reach the verdict.
    let tmp = scratch();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("sub")).unwrap();
    let project = subdirectory_project(&repo, "archived");
    nodex(&project).arg("build").assert().success();

    let absolute = run_envelope(nodex(&project).args(["check", "--since", "HEAD"]));

    let mut relative_cmd = Command::cargo_bin("nodex").expect("nodex binary in cargo target");
    relative_cmd.current_dir(repo.join("sub")).args([
        "-C",
        "../docs-site",
        "check",
        "--since",
        "HEAD",
    ]);
    let relative = run_envelope(&mut relative_cmd);

    assert_eq!(
        relative, absolute,
        "the same project, spelled two ways, is one verdict"
    );
    let strays: Vec<PathBuf> = walk_entries(tmp.path())
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".nodex-"))
        })
        .collect();
    assert!(
        strays.is_empty(),
        "reading a baseline writes nothing into the tree, however the root was spelled: {strays:?}"
    );
    assert!(
        !tmp.path().join("docs-site").exists(),
        "nothing is created beside the repository"
    );
}

#[test]
fn every_mutating_command_discloses_an_unenforced_baseline() {
    // A project that configures immutability locks but has no git
    // repository cannot have them enforced. `check` says so; a mutation
    // that writes under the same condition must say so too, or the
    // caller reads a clean result from a run that enforced nothing —
    // the same silent-skip failure mode in the write plane.
    let cases: [&[&str]; 5] = [
        &["retarget", "generic-b", "generic-c"],
        &["lifecycle", "set", "generic-b", "--status", "archived"],
        &["rename", "docs/c.md", "docs/c2.md"],
        &["migrate", "--apply"],
        &[
            "scaffold",
            "--kind",
            "generic",
            "--title",
            "New",
            "--path",
            "docs/new.md",
        ],
    ];
    for args in cases {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(root.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
        write_doc(
            root,
            "docs/a.md",
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
             # A\n\nsee [[generic-b]]\n",
        );
        write_doc(
            root,
            "docs/b.md",
            "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
        );
        write_doc(
            root,
            "docs/c.md",
            "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
        );
        // A frontmatter-less document, so `migrate --apply` has a real
        // write to perform rather than an empty plan.
        write_doc(root, "docs/bare.md", "# Bare\n");
        nodex(root).arg("build").assert().success();

        let envelope = run_envelope(nodex(root).args(args));
        assert!(
            envelope
                .get("warnings")
                .and_then(Value::as_array)
                .expect("warnings")
                .iter()
                .filter_map(warning_msg)
                .any(|m| m.contains("immutability rules are inert")),
            "nodex {args:?} must disclose that the configured locks did not engage: {envelope}"
        );
    }
}

/// A lock reports as its reach the records it can actually fire over, and a
/// record the baseline holds no node for is not one of them.
///
/// Every per-node channel a diff carries is built over the ids both snapshots
/// hold, so such a record can appear in none of them however terminal it looks
/// now. Counting it would report a lock guarding a document it can never judge
/// — which is what every way of losing a baseline node looks like from the
/// inside, whether the ref could not read it, the project's own scope declined
/// it there, or the record has since moved. The population answers for all of
/// them at once and needs to attribute none of them.
///
/// Here the parent is terminal at the baseline and active now, so the child is
/// in scope only on this side: a frozen record with nothing to be compared
/// against.
#[test]
fn a_lock_does_not_count_a_record_the_baseline_has_no_node_for() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\nparent_glob = \"docs/parent.md\"\n\
         child_glob = \"docs/parent.*.md\"\n\
         [kinds]\nallowed = [\"generic\", \"spec\"]\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         initial = \"active\"\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         trigger = \"terminal\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    // Outside the lock's kinds, so its own status never moves the reach.
    write_doc(
        root,
        "docs/parent.md",
        "---\nid: spec-parent\ntitle: P\nkind: spec\nstatus: archived\n---\n# P\n",
    );
    write_doc(
        root,
        "docs/parent.child.md",
        "---\nid: generic-child\ntitle: C\nkind: generic\nstatus: archived\n---\n# C\n\nFrozen.\n",
    );
    write_doc(
        root,
        "docs/kept.md",
        "---\nid: generic-kept\ntitle: K\nkind: generic\nstatus: archived\n---\n# K\n\nFrozen.\n",
    );
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "the parent is terminal at the baseline",
    ]);

    let reach = |envelope: &Value| -> u64 {
        envelope
            .pointer("/data/rule_coverage")
            .and_then(Value::as_array)
            .expect("rule_coverage")
            .iter()
            .find(|c| c.get("rule_id").and_then(Value::as_str) == Some("body_immutable/frozen"))
            .and_then(|c| c.get("subjects"))
            .and_then(Value::as_u64)
            .expect("the lock reports a reach")
    };

    nodex(root).arg("build").assert().success();
    assert_eq!(
        reach(&run_envelope(nodex(root).arg("check"))),
        1,
        "the one frozen record in scope on both sides"
    );

    // Active now, so the child joins the project carrying no baseline record.
    write_doc(
        root,
        "docs/parent.md",
        "---\nid: spec-parent\ntitle: P\nkind: spec\nstatus: active\n---\n# P\n",
    );
    nodex(root).arg("build").assert().success();
    let envelope = run_envelope(nodex(root).arg("check"));
    assert_eq!(
        reach(&envelope),
        1,
        "the child joined the project but not the lock's reach: {envelope}"
    );
    // And the project's own scope is not reported as something the ref failed
    // to supply — the same rule, from the same config, decided both sides.
    let inert: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .map(|w| {
            w.iter()
                .filter(|x| x.get("code").and_then(Value::as_str) == Some("baseline_inert"))
                .filter_map(warning_msg)
                .collect()
        })
        .unwrap_or_default();
    assert!(inert.is_empty(), "{inert:?}");
}

/// What the ref genuinely could not read is a different thing from what the
/// project declined, and it is still reported on its own.
///
/// The ref held a document there and could not parse it, so no id was ever
/// recovered — a working tree holding nothing at that path is as consistent
/// with a record that moved as with one that went away.
#[test]
fn a_baseline_parse_failure_says_so_even_where_the_path_is_free() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    git(&["init", "-q"]);
    fs::write(root.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    // At the baseline this document cannot be read — the frontmatter fence is
    // never closed — so it has no baseline node and nothing locks it.
    write_doc(
        root,
        "docs/a.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n\
         # A\n\nFrozen decision.\n",
    );
    write_doc(
        root,
        "docs/other.md",
        "---\nid: generic-other\ntitle: O\nkind: generic\nstatus: active\n---\n# O\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "the baseline cannot read docs/a.md"]);

    // Repaired and moved: the record is current under a path the baseline
    // never named, so the pairing at `docs/a.md` finds nothing.
    fs::remove_file(root.join("docs/a.md")).unwrap();
    write_doc(
        root,
        "docs/b.md",
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nFrozen decision.\n",
    );
    nodex(root).arg("build").assert().success();

    let envelope = run_envelope(nodex(root).arg("check"));
    let advisory = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .map(|w| {
            w.iter()
                .filter_map(warning_msg)
                .any(|m| m.contains("docs/a.md") && m.contains("inert"))
        })
        .unwrap_or(false);
    assert!(
        advisory,
        "an unreadable baseline record is named whether or not its path is still in use: {envelope}"
    );
}

/// The third way a baseline loses a document, and the only one the walk used
/// to drop with no record at all: a tracked symlink whose target the ref does
/// not carry materialises dangling, and `is_dir` / `is_file` both answer
/// false, so the entry fell out of the classification entirely — not a parse
/// failure, not a conditional exclude, not a warning. The frozen body it
/// pointed at could then be rewritten with `check` green and an empty
/// warnings array, which is the exact failure the advisory chain exists to
/// prevent.
#[test]
#[cfg(unix)]
fn a_baseline_whose_symlink_resolves_to_nothing_says_so() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    fs::create_dir_all(project.join("outside")).unwrap();
    // The target is untracked, so the ref carries the link and nothing else.
    fs::write(project.join(".gitignore"), "outside/\n").unwrap();
    fs::write(
        project.join("outside/a.md"),
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nFrozen decision.\n",
    )
    .unwrap();
    fs::create_dir_all(project.join("docs")).unwrap();
    std::os::unix::fs::symlink("../outside/a.md", project.join("docs/a.md")).unwrap();
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    git(&["add", "-A"]);
    git(&[
        "commit",
        "-q",
        "-m",
        "the link is tracked, its target is not",
    ]);

    // Rewrite the frozen body through the link.
    fs::write(
        project.join("outside/a.md"),
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nRewritten.\n",
    )
    .unwrap();
    nodex(project).arg("build").assert().success();

    let envelope = run_envelope(nodex(project).arg("check"));
    let named = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .any(|m| m.contains("docs/a.md") && m.contains("holds no readable document"));
    assert!(
        named,
        "the baseline names the document it could not read, so an inert lock is not silent: \
         {envelope}"
    );
}

/// A baseline is a checkout, and git records a symlink rather than what it
/// points at. Following one *out* of the checkout reads the present and
/// presents it as the ref's past: the before-body then equals the after-body,
/// the lock never fires, and `check` — the CI gate — passes on a frozen body
/// that was edited. A ref build keeps to the checkout, so the document has no
/// baseline node and the envelope says why. A symlink resolving *inside*
/// stays exactly as locked as any other document: the working tree's
/// reader-follows discipline is untouched, only a ref's is confined.
#[test]
#[cfg(unix)]
fn a_baseline_does_not_read_through_a_symlink_that_leaves_the_checkout() {
    let locked_through = |target: fn(&std::path::Path) -> std::path::PathBuf| {
        let tmp = scratch();
        let project = tmp.path().to_path_buf();
        let git = git_runner(&project);
        git(&["init", "-q"]);
        fs::write(
            project.join("nodex.toml"),
            LOCKED_PROJECT_CONFIG.replace("[scope]\n", "[scope]\nfollow_symlinks = true\n"),
        )
        .unwrap();
        fs::create_dir_all(project.join("real")).unwrap();
        fs::create_dir_all(project.join("docs")).unwrap();
        fs::write(
            project.join("real/a.md"),
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
             # A\n\nFrozen decision.\n",
        )
        .unwrap();
        write_doc(
            &project,
            "docs/b.md",
            "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
        );
        std::os::unix::fs::symlink(target(&project), project.join("docs/linked")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "the link is what git records"]);

        fs::write(
            project.join("real/a.md"),
            "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
             # A\n\nRewritten.\n",
        )
        .unwrap();
        nodex(&project).arg("build").assert().success();
        // `check` exits 1 when it reds something, so the envelope is read
        // directly rather than through the success-asserting helper.
        let output = nodex(&project).arg("check").output().expect("ran");
        serde_json::from_str::<Value>(String::from_utf8_lossy(&output.stdout).trim()).expect("json")
    };

    let lock_fired = |envelope: &Value| {
        envelope["data"]["violations"]
            .as_array()
            .expect("violations")
            .iter()
            .filter(|v| v["rule_id"] == "body_immutable/frozen")
            .count()
    };

    // Resolving inside the checkout: the ref carries the target, so the lock
    // has a real before-state and fires.
    let inside = locked_through(|_| std::path::PathBuf::from("../real"));
    assert_eq!(
        lock_fired(&inside),
        1,
        "a contained link is locked like any other document: {inside}"
    );

    // Resolving outside: the ref recorded the link only, so there is nothing
    // to compare against — and the envelope has to say so rather than pass
    // in silence.
    let outside = locked_through(|project| project.join("real"));
    assert_eq!(
        lock_fired(&outside),
        0,
        "no baseline content exists for it, so no rule can fire: {outside}"
    );
    let named = outside
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .any(|m| m.contains("docs/linked") && m.contains("outside the checkout"));
    assert!(
        named,
        "the inert lock names the path the ref could not supply: {outside}"
    );
}

#[test]
fn a_baseline_that_carries_the_project_and_scans_nothing_says_so_on_both_planes() {
    // The baseline is graphed by the same build the working tree is, so it
    // discloses by construction rather than per call site — but "by
    // construction" is what a caller assumes and a test proves. The two are
    // told apart by the ref tag: a reader has to know which side read nothing.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"typo_never_matches/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\"]\nterminal = []\n[rules]\n\
         immutable_baseline = \"HEAD\"\n[[rules.body_immutable]]\nname = \"frozen\"\n\
         mode = \"frozen\"\ntrigger = \"creation\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();
    write_doc(root, "notes/a.md", "# A\n");
    let git = git_runner(root);
    git(&["init"]);
    git(&["add", "-A"]);
    git(&["commit", "-m", "initial"]);
    nodex(root).arg("build").assert().success();

    for args in [vec!["check"], vec!["query", "issues"]] {
        let env = run_envelope(nodex(root).args(&args));
        let messages: Vec<&str> = env
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(warning_msg).collect())
            .unwrap_or_default();
        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("baseline ") && m.contains("scope matched no files")),
            "{args:?} must name the side that read nothing: {messages:?}"
        );
    }
}

/// A file that is both symlinked and would be refused reports the symlink,
/// because the write discipline is decided before any verdict is asked for: a
/// path that must never be written through cannot be made writable by a lock
/// that happens to permit it.
#[test]
#[cfg(unix)]
fn a_symlinked_file_reports_the_symlink_rather_than_the_lock() {
    let tmp = scratch();
    let project = tmp.path();
    let git = git_runner(project);
    git(&["init", "-q"]);
    fs::write(project.join("nodex.toml"), LOCKED_PROJECT_CONFIG).unwrap();
    // The target lives *inside* the project root, so `reject_outside_root`
    // admits it and only the symlink predicate can decline the write — which
    // is what this test is named for. It sits outside `scope.include`, so the
    // link is the only path the graph reaches it by. It is a frozen document
    // referencing generic-b, so the lock is a genuine alternative reason the
    // seam would report if the symlink guard let the plan through.
    let target = project.join("store/a.md");
    fs::create_dir_all(project.join("store")).unwrap();
    fs::write(
        &target,
        "---\nid: generic-a\ntitle: A\nkind: generic\nstatus: archived\n---\n\
         # A\n\nsee [[generic-b]]\n",
    )
    .unwrap();
    fs::create_dir_all(project.join("docs")).unwrap();
    std::os::unix::fs::symlink("../store/a.md", project.join("docs/a.md")).unwrap();
    write_doc(
        project,
        "docs/b.md",
        "---\nid: generic-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    write_doc(
        project,
        "docs/c.md",
        "---\nid: generic-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\n",
    );
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "a symlinked frozen document"]);

    let envelope = run_envelope(nodex(project).args(["retarget", "generic-b", "generic-c"]));
    let reasons: Vec<&str> = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings")
        .iter()
        .filter_map(warning_msg)
        .collect();
    assert!(
        reasons.iter().any(|m| m.contains("symlink")),
        "the symlink is the reason reported: {envelope}"
    );
    assert!(
        !reasons.iter().any(|m| m.contains("body_immutable")),
        "the lock is never reached, so it is never the reason: {envelope}"
    );
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("[[generic-b]]"),
        "and the target is not written through"
    );
}

#[cfg(unix)]
#[test]
fn an_undescended_link_at_the_baseline_reports_the_lock_inert() {
    // A ref that records a directory symlink carries no document below it
    // once the walk declines to descend, so the baseline has no node and the
    // lock cannot fire. Silence there reads as "the lock passed".
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n\
         [statuses]\nallowed = [\"active\", \"archived\"]\nterminal = [\"archived\"]\n\
         [rules]\nimmutable_baseline = \"HEAD\"\n\
         [[rules.body_immutable]]\nname = \"frozen\"\nmode = \"frozen\"\n\
         trigger = \"creation\"\nkinds = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        "vendor/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: archived\n---\n# frozen\n",
    );
    fs::create_dir_all(root.join("docs")).unwrap();
    unix_fs::symlink("../vendor", root.join("docs/vendor")).unwrap();
    let git = git_runner(root);
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);

    fs::remove_file(root.join("docs/vendor")).unwrap();
    write_doc(
        root,
        "docs/vendor/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: archived\n---\n# tampered\n",
    );

    let env = envelope_of(nodex(root).args(["check", "--since", "HEAD"]));
    let inert = env
        .get("warnings")
        .and_then(Value::as_array)
        .map(|ws| {
            ws.iter().any(|w| {
                w["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("docs/vendor is a directory symlink there")
            })
        })
        .unwrap_or(false);
    assert!(inert, "the inert lock is named, never silent: {env}");
}

/// A ref-to-ref report reads what the ref *records* and nothing else.
///
/// An unconfined build follows a link out of the checkout into the live
/// filesystem, so a symlink whose target changed between two commits yields
/// field changes that happened entirely outside the repository — history the
/// refs do not carry, reported as history they do.
#[cfg(unix)]
#[test]
fn a_ref_to_ref_report_reads_only_what_the_refs_record() {
    use std::os::unix::fs as unix_fs;
    let outside = scratch();
    for (dir, title) in [("a", "External A"), ("b", "External B")] {
        write_doc(
            outside.path(),
            &format!("{dir}/t.md"),
            &format!("---\nid: t\ntitle: {title}\nkind: generic\nstatus: active\n---\n# T\n"),
        );
    }

    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\nfollow_symlinks = true\n\
         [kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    git(&["init", "-q"]);
    unix_fs::symlink(outside.path().join("a"), root.join("docs/linked")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    fs::remove_file(root.join("docs/linked")).unwrap();
    unix_fs::symlink(outside.path().join("b"), root.join("docs/linked")).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "second"]);

    // Neither ref records the external documents.
    for command in [
        vec!["diff", "HEAD~1", "HEAD"],
        vec!["impact", "HEAD~1", "HEAD"],
    ] {
        let env = run_envelope(nodex(root).args(&command));
        let changes = env
            .pointer("/data/field_changes")
            .or_else(|| env.pointer("/data/diff/field_changes"))
            .and_then(Value::as_array)
            .expect("field_changes");
        assert!(
            changes.is_empty(),
            "{command:?} reported a change from outside both refs: {changes:?}"
        );
        // And says what it could not read, per ref, rather than reading
        // complete.
        for git_ref in ["HEAD~1", "HEAD"] {
            assert!(
                env.get("warnings")
                    .and_then(Value::as_array)
                    .is_some_and(|ws| ws.iter().filter_map(warning_msg).any(|w| w
                        .starts_with(&format!("{git_ref}: "))
                        && w.contains("docs/linked"))),
                "{command:?} names what {git_ref} does not record: {env}"
            );
        }
    }
}

#[test]
fn snapshot_locks_use_one_baseline_across_proposals_index_issues_and_writes() {
    let tmp = scratch();
    let root = tmp.path();
    snapshot_lock_project(root, "merge_base");
    let git = git_runner(root);
    let branch_doc = "---\nid: b\ntitle: B\nstatus: accepted\n---\nBranch record.\n";
    write_doc(root, "docs/b.md", branch_doc);
    assert!(git(&["add", "."]).status.success());
    assert!(
        git(&["commit", "-qm", "accept branch record"])
            .status
            .success()
    );
    write_doc(
        root,
        "docs/b.md",
        &branch_doc.replace("Branch record.", "Reviewed record."),
    );
    assert!(git(&["commit", "-qam", "review"]).status.success());
    let proposed = branch_doc.replace("Branch record.", "Final record.");
    nodex(root)
        .args(["check", "--content", "docs/b.md=-"])
        .write_stdin(proposed.clone())
        .assert()
        .success();
    write_doc(root, "docs/b.md", &proposed);
    assert!(git(&["add", "."]).status.success());
    for args in [
        vec!["check"],
        vec!["check", "--since", "HEAD"],
        vec!["check", "--staged"],
    ] {
        nodex(root).args(args).assert().success();
    }
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).args(["query", "issues"]));
    assert!(
        !issues["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["rule_id"].as_str().unwrap().contains("immutable"))
    );
    nodex(root)
        .args(["rename", "docs/b.md", "docs/moved.md"])
        .assert()
        .success();
    let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
    let edited = original.replace("Original.", "Changed.");
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(edited.clone())
        .assert()
        .code(1);
    write_doc(root, "docs/a.md", &edited);
    assert!(git(&["add", "."]).status.success());
    for args in [
        vec!["check"],
        vec!["check", "--since", "HEAD"],
        vec!["check", "--staged"],
    ] {
        nodex(root).args(args).assert().code(1);
    }
    nodex(root)
        .args(["check", "--content", "docs/a.md=-"])
        .write_stdin(edited)
        .assert()
        .code(1);
    let error = nodex(root)
        .args(["rename", "docs/a.md", "docs/other.md"])
        .output()
        .unwrap();
    assert_eq!(error.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&error.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "CONFIG_ERROR", "{envelope}");
    assert!(root.join("docs/a.md").exists());
    write_doc(
        root,
        "docs/a.md",
        &format!("{original}\n## Corrections\nFirst correction.\n"),
    );
    nodex(root).arg("check").assert().success();
    assert!(
        git(&["commit", "-qam", "append correction"])
            .status
            .success()
    );
    write_doc(
        root,
        "docs/a.md",
        &format!("{original}\n## Corrections\nRevised correction.\n"),
    );
    nodex(root).arg("check").assert().success();
    assert!(git(&["restore", "docs/a.md"]).status.success());
    assert!(
        git(&["checkout", "-qb", "integration", "main"])
            .status
            .success()
    );
    assert!(
        git(&["merge", "--no-ff", "-m", "PR merge", "topic"])
            .status
            .success()
    );
    nodex(root).arg("check").assert().success();
    let merged = fs::read_to_string(root.join("docs/moved.md")).unwrap();
    write_doc(
        root,
        "docs/moved.md",
        &merged.replace("Final record.", "Merge review."),
    );
    nodex(root).arg("check").assert().success();
}

#[test]
fn snapshot_baseline_advances_only_with_shared_ancestry() {
    let tmp = scratch();
    let root = tmp.path();
    snapshot_lock_project(root, "merge_base");
    let git = git_runner(root);
    let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
    assert!(git(&["checkout", "main"]).status.success());
    let corrected = format!("{original}\n## Corrections\nPublished correction.\n");
    write_doc(root, "docs/a.md", &corrected);
    assert!(
        git(&["commit", "-qam", "published correction"])
            .status
            .success()
    );
    assert!(git(&["checkout", "topic"]).status.success());
    nodex(root).arg("check").assert().success();
    let config_path = root.join("nodex.toml");
    let policy = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        policy.replace(
            "immutable_baseline_mode = \"merge_base\"",
            "immutable_baseline_mode = \"ref\"",
        ),
    )
    .unwrap();
    nodex(root).arg("check").assert().code(1);
    fs::write(&config_path, policy).unwrap();
    assert!(
        git(&["merge", "--no-ff", "-m", "integrate main", "main"])
            .status
            .success()
    );
    nodex(root).arg("check").assert().success();
    write_doc(
        root,
        "docs/a.md",
        &corrected.replace("Published correction.", "Changed correction."),
    );
    nodex(root).arg("check").assert().code(1);
    assert!(git(&["restore", "docs/a.md"]).status.success());
    assert!(git(&["checkout", "main"]).status.success());
    nodex(root).arg("check").assert().success();
    write_doc(
        root,
        "docs/a.md",
        &corrected.replace("Published correction.", "Changed correction."),
    );
    nodex(root).arg("check").assert().code(1);
}

#[test]
fn merge_base_locks_refuse_missing_and_ambiguous_ancestry_on_both_planes() {
    for ambiguous in [false, true] {
        let tmp = scratch();
        let root = tmp.path();
        snapshot_lock_project(root, "merge_base");
        let git = git_runner(root);
        let oid = |args: &[&str]| {
            let output = git(args);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        let tree = oid(&["rev-parse", "HEAD^{tree}"]);
        let source = if ambiguous {
            let parent = oid(&["rev-parse", "HEAD"]);
            let left = oid(&["commit-tree", &tree, "-p", &parent, "-m", "left"]);
            let right = oid(&["commit-tree", &tree, "-p", &parent, "-m", "right"]);
            let topic = oid(&[
                "commit-tree",
                &tree,
                "-p",
                &left,
                "-p",
                &right,
                "-m",
                "left merge",
            ]);
            assert!(git(&["reset", "--hard", &topic]).status.success());
            oid(&[
                "commit-tree",
                &tree,
                "-p",
                &right,
                "-p",
                &left,
                "-m",
                "right merge",
            ])
        } else {
            oid(&["commit-tree", &tree, "-m", "unrelated"])
        };
        assert!(
            git(&["update-ref", "refs/heads/main", &source])
                .status
                .success()
        );
        let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
        for args in [
            vec!["check"],
            vec!["check", "--staged"],
            vec!["check", "--content", "docs/a.md=-"],
            vec!["rename", "docs/a.md", "docs/moved.md"],
        ] {
            let mut command = nodex(root);
            command.args(&args);
            if args.contains(&"--content") {
                command.write_stdin(original.clone());
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(envelope["error"]["code"], "CONFIG_ERROR", "{envelope}");
            assert!(
                envelope["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(if ambiguous {
                        "multiple merge bases"
                    } else {
                        "no common ancestor"
                    }),
                "{envelope}"
            );
        }
        assert!(root.join("docs/a.md").exists());
    }
}

#[test]
fn snapshot_lock_proposals_still_judge_status_flow_steps() {
    let tmp = scratch();
    let root = tmp.path();
    snapshot_lock_project(root, "merge_base");
    let config_path = root.join("nodex.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str("\n[[statuses.flow]]\nkinds = [\"generic\"]\ninitial = \"open\"\ntransitions = { open = [\"accepted\"], accepted = [\"superseded\"] }\n");
    fs::write(config_path, config).unwrap();
    let git = git_runner(root);
    let open = "---\nid: b\ntitle: B\nstatus: open\n---\nDraft.\n";
    write_doc(root, "docs/b.md", open);
    assert!(git(&["add", "."]).status.success());
    assert!(git(&["commit", "-qm", "draft"]).status.success());
    let accepted = open
        .replace("status: open", "status: accepted")
        .replace("Draft.", "Reviewed.");
    nodex(root)
        .args(["check", "--content", "docs/b.md=-"])
        .write_stdin(accepted.clone())
        .assert()
        .success();
    let skipped = open.replace("status: open", "status: superseded");
    let output = nodex(root)
        .args(["check", "--content", "docs/b.md=-"])
        .write_stdin(skipped)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        envelope["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["rule_id"] == "status_transition")
    );
    write_doc(root, "docs/b.md", &accepted);
    assert!(git(&["commit", "-qam", "accept"]).status.success());
    write_doc(root, "docs/b.md", &accepted.replace("Reviewed.", "Final."));
    nodex(root)
        .args(["check", "--since", "main"])
        .assert()
        .success();
    write_doc(root, "docs/c.md", &open.replace("id: b", "id: c"));
    assert!(git(&["add", "."]).status.success());
    assert!(git(&["commit", "-qm", "another draft"]).status.success());
    let standing = open
        .replace("id: b", "id: c")
        .replace("status: open", "status: superseded");
    write_doc(root, "docs/c.md", &standing);
    nodex(root)
        .args(["check", "--content", "docs/c.md=-"])
        .write_stdin(standing)
        .assert()
        .success();
    nodex(root)
        .args(["rename", "docs/c.md", "docs/standing.md"])
        .assert()
        .success();
    nodex(root).arg("check").assert().code(1);
}

#[test]
fn merge_base_locks_require_the_ancestry_missing_from_a_shallow_clone() {
    let source = scratch();
    snapshot_lock_project(source.path(), "merge_base");
    let git = git_runner(source.path());
    write_doc(source.path(), "docs/topic.md", "# Topic\n");
    assert!(git(&["add", "."]).status.success());
    assert!(git(&["commit", "-qm", "topic"]).status.success());
    assert!(git(&["checkout", "main"]).status.success());
    write_doc(source.path(), "docs/main.md", "# Main\n");
    assert!(git(&["add", "."]).status.success());
    assert!(git(&["commit", "-qm", "main"]).status.success());
    let clone = scratch();
    assert!(
        git(&[
            "clone",
            "--no-local",
            "--depth",
            "1",
            "--no-single-branch",
            "--branch",
            "topic",
            source.path().to_str().unwrap(),
            clone.path().to_str().unwrap()
        ])
        .status
        .success()
    );
    let config_path = clone.path().join("nodex.toml");
    let config = fs::read_to_string(&config_path).unwrap().replace(
        "immutable_baseline = \"main\"",
        "immutable_baseline = \"origin/main\"",
    );
    fs::write(config_path, config).unwrap();
    let output = nodex(clone.path()).arg("check").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "CONFIG_ERROR");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("shallow"),
        "{envelope}"
    );
    assert!(
        git_runner(clone.path())(&["fetch", "--unshallow"])
            .status
            .success()
    );
    nodex(clone.path()).arg("check").assert().success();
}

#[test]
fn merge_base_locks_do_not_trust_an_older_base_exposed_by_shallow_history() {
    let source = scratch();
    let root = source.path();
    snapshot_lock_project(root, "merge_base");
    let git = git_runner(root);
    let oid = |args: &[&str]| {
        let output = git(args);
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let initial = oid(&["rev-parse", "HEAD"]);
    let tree = oid(&["rev-parse", "HEAD^{tree}"]);
    let original = fs::read_to_string(root.join("docs/a.md")).unwrap();
    write_doc(
        root,
        "docs/a.md",
        &original.replace("status: accepted", "status: open"),
    );
    assert!(git(&["add", "."]).status.success());
    let draft_tree = oid(&["write-tree"]);
    let left = oid(&["commit-tree", &draft_tree, "-p", &initial, "-m", "left"]);
    let newer = oid(&["commit-tree", &tree, "-p", &left, "-m", "newer"]);
    let tail = oid(&["commit-tree", &tree, "-p", &newer, "-m", "tail"]);
    let topic = oid(&[
        "commit-tree",
        &tree,
        "-p",
        &left,
        "-p",
        &tail,
        "-m",
        "merge",
    ]);
    assert!(
        git(&["update-ref", "refs/heads/main", &newer])
            .status
            .success()
    );
    assert!(git(&["reset", "--hard", &topic]).status.success());
    let clone = scratch();
    assert!(
        git(&[
            "clone",
            "--no-local",
            "--depth",
            "2",
            "--no-single-branch",
            "--branch",
            "topic",
            root.to_str().unwrap(),
            clone.path().to_str().unwrap()
        ])
        .status
        .success()
    );
    let git_clone = git_runner(clone.path());
    let output = git_clone(&["merge-base", "--all", "HEAD", "origin/main"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), left);
    let config_path = clone.path().join("nodex.toml");
    let policy = fs::read_to_string(&config_path).unwrap().replace(
        "immutable_baseline = \"main\"",
        "immutable_baseline = \"origin/main\"",
    );
    fs::write(config_path, policy).unwrap();
    write_doc(
        clone.path(),
        "docs/a.md",
        &original.replace("Original.", "Changed."),
    );
    let output = nodex(clone.path()).arg("check").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "CONFIG_ERROR");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("shallow"),
        "{envelope}"
    );
    assert!(git_clone(&["fetch", "--deepen", "1"]).status.success());
    let repository = nodex_core::Repository::discover(clone.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        repository
            .unique_merge_base(&[topic, newer.clone()])
            .unwrap(),
        newer
    );
    nodex(clone.path()).arg("check").assert().code(1);
    write_doc(clone.path(), "docs/a.md", &original);
    nodex(clone.path()).arg("check").assert().success();
}

#[test]
fn configured_step_locks_keep_history_when_the_report_ref_predates_the_project() {
    let tmp = scratch();
    let root = tmp.path();
    let git = git_runner(root);
    assert!(git(&["init"]).status.success());
    fs::write(root.join("source.txt"), "source\n").unwrap();
    assert!(git(&["add", "."]).status.success());
    assert!(git(&["commit", "-qm", "before project"]).status.success());
    assert!(git(&["tag", "old"]).status.success());
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("nodex.toml"), "[scope]\ninclude = [\"docs/**/*.md\"]\n[rules]\nimmutable_baseline = \"base\"\n[[rules.body_immutable]]\nname = \"sealed\"\nmode = \"frozen\"\n").unwrap();
    let draft = "---\nid: a\nstatus: active\n---\nDraft.\n";
    write_doc(&project, "docs/a.md", draft);
    assert!(git(&["add", "."]).status.success());
    assert!(git(&["commit", "-qm", "project"]).status.success());
    assert!(git(&["tag", "base"]).status.success());
    let accepted = draft.replace("status: active", "status: superseded");
    write_doc(&project, "docs/a.md", &accepted);
    assert!(git(&["commit", "-qam", "accept"]).status.success());
    write_doc(
        &project,
        "docs/a.md",
        &accepted.replace("Draft.", "Changed."),
    );
    assert!(git(&["commit", "-qam", "edit"]).status.success());
    let output = nodex(&project)
        .args(["check", "--since", "old"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        envelope["data"]["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["rule_id"] == "body_immutable/sealed" && v["details"]["commit"].is_string()),
        "{envelope}"
    );
}
