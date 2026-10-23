//! What the scan reads: scope patterns, links, and the disclosure of what
//! it did not read.

use crate::support::{
    envelope_of, init_project, nodex, run_envelope, run_json, scratch, warning_msg, write_doc,
};
use serde_json::Value;
use std::fs;
#[cfg(unix)]
use tempfile::TempDir;

#[test]
fn output_dir_is_auto_excluded_from_scope() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/real.md",
        "---\nid: real\ntitle: Real\nkind: generic\nstatus: active\n---\n# Real\n",
    );
    // First build creates _index/GRAPH.md via report.
    nodex(tmp.path()).arg("build").assert().success();
    nodex(tmp.path()).arg("report").assert().success();
    // Rebuild and verify _index/GRAPH.md wasn't indexed as a user doc.
    let data = run_json(nodex(tmp.path()).arg("build").arg("--full"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "_index/GRAPH.md must not be indexed"
    );
    // migrate must not offer to touch the generated GRAPH.md either.
    let migrate = run_json(nodex(tmp.path()).arg("migrate"));
    let changes = migrate
        .get("changes")
        .and_then(Value::as_array)
        .expect("changes array");
    for change in changes {
        let path = change.get("path").and_then(Value::as_str).unwrap_or("");
        assert!(
            !path.starts_with("_index/"),
            "migrate should not target _index/* but saw {path}"
        );
    }
}

#[test]
fn no_declaration_can_claim_away_an_empty_scan() {
    // `may_be_empty` bounds what it can cost: the pattern that carries it
    // goes quiet, and a project whose every pattern carries it and whose
    // paths are simply wrong still hears that nothing was read. Without that
    // floor the attribute would be a way to make a mis-scoped project
    // validate green over a corpus nobody looked at, which is the failure the
    // disclosure exists for.
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [{ glob = \"docs/**/*.md\", may_be_empty = true }, \
         { glob = \"specs/**/*.md\", may_be_empty = true }]\n\
         [kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    // The corpus is real; the declarations name the wrong place.
    write_doc(
        root,
        "documentation/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );

    for args in [vec!["build"], vec!["migrate"]] {
        let env = run_envelope(nodex(root).args(&args));
        let messages: Vec<&str> = env
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(warning_msg).collect())
            .unwrap_or_default();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("scope matched no files")),
            "{args:?} must say the scan read nothing: {env}"
        );
    }
}

/// The scan and the graph describe the same document from the same bytes.
///
/// A `conditional_exclude` rule asks whether a parent is terminal. The graph
/// gives a document that declares no status the project's initial one, so a
/// scan reading "declares none" as "not terminal" disagrees with it — and
/// under a config whose initial status *is* terminal they disagree about
/// every bare document. `migrate` then writes the status the document
/// already had and flips the scan's verdict, turning a green `check` red on
/// a command that reported success.
#[test]
fn the_scan_reads_a_documents_status_the_way_the_graph_assigns_it() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\n\
         parent_glob = \"docs/spec/index.md\"\n\
         child_glob = \"docs/spec/notes/**/*.md\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"done\"]\nterminal = [\"done\"]\ninitial = \"done\"\n\
         [[detection.unresolved_policy]]\n\
         name = \"gone\"\ncause = \"excluded_from_scope\"\nseverity = \"error\"\n",
    )
    .unwrap();
    // The parent declares no status, so the graph builds it as `done` — the
    // project's initial status, which is terminal here.
    write_doc(root, "docs/spec/index.md", "# Bare Parent\n");
    write_doc(
        root,
        "docs/spec/notes/n.md",
        "---\nid: note\ntitle: N\nkind: generic\nstatus: done\n---\n# N\n",
    );
    write_doc(
        root,
        "docs/other.md",
        "---\nid: other\ntitle: O\nkind: generic\nstatus: done\n---\nsee [n](spec/notes/n.md)\n",
    );

    let rules = |root: &std::path::Path| -> Vec<String> {
        envelope_of(nodex(root).arg("check"))
            .pointer("/data/violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter_map(|v| v["rule_id"].as_str().map(str::to_string))
            .collect()
    };
    // The child is excluded from the start, because the parent already is
    // terminal — the reference into it is red before anything is written.
    let before = rules(root);
    assert_eq!(before, ["unresolved_reference/gone"], "{before:?}");

    // Writing the status the document already had changes nothing.
    nodex(root).args(["migrate", "--apply"]).assert().success();
    assert_eq!(rules(root), before, "migrate introduced a violation");
}

/// An include pattern globset matches scans the documents it matches — and
/// every plane says the same thing about them.
///
/// The hidden-path guard read the pattern's text where it needed globset's
/// answer, so a pattern naming a dotted segment without spelling it as a
/// leading dot matched in globset and scanned nothing. That is a whole
/// corpus missing with the gate green over it: `check` passes on documents
/// it never read, `rename` reports a completed rewrite while leaving
/// references dangling, and `scaffold` refuses a path that is squarely
/// inside the scope it names. The class is spelled here as a character
/// class rather than a backslash escape, which globset reads as a path
/// separator wherever `\` is one.
#[test]
fn an_include_pattern_globset_matches_is_one_the_scan_reads() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = ['[.]dotted/**/*.md']\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    write_doc(
        root,
        ".dotted/target.md",
        "---\nid: target\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n",
    );
    write_doc(
        root,
        ".dotted/referrer.md",
        "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[T](target.md)\n",
    );

    let build = run_envelope(nodex(root).arg("build"));
    assert_eq!(
        build.pointer("/data/nodes").and_then(Value::as_i64),
        Some(2),
        "the class spelling names the same directory: {build}"
    );

    // The write plane reads the same scope: the move repoints the reference
    // instead of reporting success over a corpus it never saw.
    let env = run_envelope(nodex(root).args(["rename", ".dotted/target.md", ".dotted/renamed.md"]));
    assert_eq!(
        env.pointer("/data/total_updated").and_then(Value::as_i64),
        Some(1),
        "{env}"
    );
    assert!(
        fs::read_to_string(root.join(".dotted/referrer.md"))
            .unwrap()
            .contains("(renamed.md)")
    );
    nodex(root)
        .args([
            "scaffold",
            "--kind",
            "generic",
            "--title",
            "New",
            "--path",
            ".dotted/new.md",
            "--dry-run",
        ])
        .assert()
        .success();

    // And a pattern that merely *matches* a hidden path still does not opt
    // it in — the default the guard exists to keep.
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n",
    )
    .unwrap();
    let build = run_envelope(nodex(root).args(["build", "--full"]));
    assert_eq!(
        build.pointer("/data/nodes").and_then(Value::as_i64),
        Some(0),
        "a greedy pattern opts no hidden path in: {build}"
    );
}

/// The scan reads a status the way the graph assigns it, whichever way the
/// document declines to declare one — absent, empty, or in frontmatter that
/// is not a mapping at all. The last is not a document: the build makes no
/// node for it, so nothing stands there to be a terminal parent.
#[test]
fn a_document_that_declares_no_status_reads_the_same_to_both() {
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[scope.conditional_exclude]]\n\
         parent_glob = \"docs/spec/index.md\"\n\
         child_glob = \"docs/spec/notes/**/*.md\"\n\
         [kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"done\"]\nterminal = [\"done\"]\ninitial = \"done\"\n\
         [[detection.unresolved_policy]]\n\
         name = \"gone\"\ncause = \"excluded_from_scope\"\nseverity = \"error\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/spec/notes/n.md",
        "---\nid: note\ntitle: N\nkind: generic\nstatus: done\n---\n# N\n",
    );
    write_doc(
        root,
        "docs/other.md",
        "---\nid: other\ntitle: O\nkind: generic\nstatus: done\n---\nsee [n](spec/notes/n.md)\n",
    );

    let rules = || -> Vec<String> {
        envelope_of(nodex(root).arg("check"))
            .pointer("/data/violations")
            .and_then(Value::as_array)
            .expect("violations")
            .iter()
            .filter_map(|v| v["rule_id"].as_str().map(str::to_string))
            .collect()
    };
    // Absent and empty are the same fact: the graph fills the initial status,
    // which is terminal here, so the children are excluded and the reference
    // into them is reported.
    for parent in ["# Bare Parent\n", "---\nstatus: \"\"\n---\n# Parent\n"] {
        write_doc(root, "docs/spec/index.md", parent);
        assert_eq!(rules(), ["unresolved_reference/gone"], "parent: {parent:?}");
    }
    // Frontmatter that is not a mapping produces no node at all, so no
    // parent stands there and its siblings stay in scope.
    write_doc(root, "docs/spec/index.md", "---\n- a\n---\n# Parent\n");
    assert_eq!(rules(), ["parse_failure"]);
}

/// A hidden segment the pattern names deeper than any lead can line up is
/// still an opt-in — and a greedy sibling never answers for it.
///
/// `**` consumes as many segments as it likes, so reading the pattern
/// position by position cannot identify the component that governs a deeper
/// one. Answering "not opted in" there scanned an empty corpus while globset
/// matched the documents, with `check` green over nothing.
#[test]
fn an_include_naming_a_hidden_segment_deeper_than_its_lead_still_opts_in() {
    for (include, doc, expected) in [
        ("['foo/**/.hidden/**/*.md']", "foo/a/b/.hidden/doc.md", 1),
        ("['**/.obsidian/**/*.md']", "x/y/.obsidian/doc.md", 1),
        ("['*/.dotted/**/*.md']", "a/.dotted/doc.md", 1),
        // One pattern's opt-in is not another's: a greedy sibling neither
        // grants nor withdraws it.
        ("['.claude/**/*.md', '**/*.md']", ".claude/doc.md", 1),
        ("['.claude/**/*.md', '**/*.md']", "sub/.claude/doc.md", 0),
        // And the default the guard keeps: a pattern that merely matches a
        // hidden path does not opt it in, at any depth.
        ("['docs/**/*.md']", "docs/a/.hidden/doc.md", 0),
        ("['**/*.md']", ".dotted/doc.md", 0),
    ] {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            format!("[scope]\ninclude = {include}\n[kinds]\nallowed = [\"generic\"]\n"),
        )
        .unwrap();
        write_doc(
            root,
            doc,
            "---\nid: d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
        );
        let env = run_envelope(nodex(root).arg("build"));
        assert_eq!(
            env.pointer("/data/nodes").and_then(Value::as_i64),
            Some(expected),
            "{include} / {doc}: {env}"
        );
    }
}

/// A component that starts with a wildcard can still insist on the dot.
///
/// `*` matches nothing, so `*.hidden` matches the segment `.hidden` and stops
/// matching once the dot is replaced. A rule reading the pattern's *text*
/// declared every wildcard-leading component unable to require a dot and
/// pruned a corpus the include matched — with `check` green over it and
/// `scaffold` writing into a directory the next build never reads.
#[test]
fn a_wildcard_leading_component_can_still_require_the_dot() {
    for (include, doc, expected) in [
        ("['**/*.hidden/**/*.md']", "x/.hidden/doc.md", 1),
        ("['*/*.d/**/*.md']", "a/.d/x.md", 1),
        ("['docs/**/*.vault/**/*.md']", "docs/a/.vault/n.md", 1),
        // And the default the guard keeps: a wildcard that does not turn on
        // the dot still opts nothing in.
        ("['**/*.md']", ".dotted/doc.md", 0),
        ("['docs/**/*.md']", "docs/a/.hidden/doc.md", 0),
    ] {
        let tmp = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            format!("[scope]\ninclude = {include}\n[kinds]\nallowed = [\"generic\"]\n"),
        )
        .unwrap();
        write_doc(
            root,
            doc,
            "---\nid: d\ntitle: D\nkind: generic\nstatus: active\n---\n# D\n",
        );
        let env = run_envelope(nodex(root).arg("build"));
        assert_eq!(
            env.pointer("/data/nodes").and_then(Value::as_i64),
            Some(expected),
            "{include} / {doc}: {env}"
        );
    }
}

#[test]
fn every_working_tree_reader_says_when_the_scan_reached_nothing() {
    // The line is the working tree: a command that reads it to answer says
    // what it read, so an empty answer is never mistaken for a complete one.
    // Pinned at the envelope, because the disclosure travels three different
    // ways — the scan's own call (`migrate`), the build's warnings a command
    // surfaces (`rename`, `scaffold`, `report`), and `build` / `check`
    // themselves — and deleting any one of them leaves the other two green.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"typo_never_matches/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n\
         [statuses]\nallowed = [\"active\"]\nterminal = []\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/loose.md"), "# Loose\n").unwrap();

    let readers: [&[&str]; 6] = [
        &["build"],
        &["check"],
        &["report"],
        &["migrate"],
        &["rename", "notes/loose.md", "notes/moved.md"],
        &[
            "scaffold",
            "--kind",
            "generic",
            "--title",
            "T",
            "--path",
            "typo_never_matches/t.md",
            "--dry-run",
        ],
    ];
    for args in readers {
        let env = run_envelope(nodex(root).args(args));
        assert_eq!(
            env.get("ok").and_then(Value::as_bool),
            Some(true),
            "{args:?}: {env}"
        );
        let codes: Vec<&str> = env
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|w| w.get("code").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            codes.contains(&"scope_coverage"),
            "{args:?} answered over a corpus it never read without saying so: {env}"
        );
    }
}

#[test]
fn migrate_says_so_when_the_scan_reached_nothing_to_migrate() {
    // `{"changes": [], "total": 0}` is what a finished migration looks like
    // and what a mis-scoped one looks like, and only the scan can tell them
    // apart. `--apply` reports it too: that is the invocation whose "nothing
    // to do" an operator acts on.
    let tmp = scratch();
    let root = tmp.path();
    init_project(root);
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n[kinds]\nallowed = [\"generic\"]\n[statuses]\n\
         allowed = [\"active\"]\nterminal = []\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("content")).unwrap();
    fs::write(root.join("content/bare.md"), "# Out of scope\n").unwrap();

    for args in [vec!["migrate"], vec!["migrate", "--apply"]] {
        let env = run_envelope(nodex(root).args(&args));
        assert_eq!(env.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(env.pointer("/data/total").and_then(Value::as_u64), Some(0));
        let warnings: Vec<&str> = env
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(warning_msg).collect())
            .unwrap_or_default();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("scope matched no files") && w.contains("nothing to migrate")),
            "{args:?} must name the empty scan in its own terms: {warnings:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_document_under_a_followed_link_can_be_renamed_at_the_name_the_graph_gives_it() {
    // A proposal names a document, not a spelling of one. Where a directory is
    // reached under several names the file is admitted under each, so a move
    // whose source is removed by exact string leaves the document behind under
    // another name — present and gone at once, and the destination collides
    // with it. The one path a caller has is the one the graph shows them.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/real/keep.md",
        "---\nid: keep\ntitle: K\nkind: generic\nstatus: active\n---\n# K\n",
    );
    write_doc(
        root,
        "docs/real/index.md",
        "---\nid: idx\ntitle: I\nkind: generic\nstatus: active\n---\n[k](keep.md)\n",
    );
    unix_fs::symlink("real", root.join("docs/alias")).unwrap();
    nodex(root).arg("build").assert().success();

    let named = run_json(nodex(root).args(["query", "node", "keep"]))
        .pointer("/node/path")
        .and_then(Value::as_str)
        .expect("the graph names the document")
        .to_string();
    let moved = named.replace("keep.md", "renamed.md");

    let env = run_envelope(nodex(root).args(["rename", &named, &moved]));
    assert_eq!(
        env.pointer("/data/new_path").and_then(Value::as_str),
        Some(moved.as_str()),
        "{env}"
    );

    // The reference the move claimed to rewrite has to resolve afterwards —
    // a rename that reports success and dangles a link is the same defect
    // wearing a green envelope.
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).arg("query").arg("issues"));
    assert_eq!(
        issues.get("unresolved_edges").and_then(Value::as_array),
        Some(&vec![]),
        "{issues}"
    );
}

#[cfg(unix)]
#[test]
fn a_link_to_the_output_directory_does_not_make_it_a_project_document() {
    // The output-dir exclusion is unconditional, and a directory is a
    // location rather than a spelling: reached under another name, nodex's
    // own GRAPH.md would be a user document that `migrate` writes
    // frontmatter into.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    nodex(root).arg("build").assert().success();
    nodex(root).arg("report").assert().success();
    assert!(
        root.join("_index/GRAPH.md").exists(),
        "report wrote GRAPH.md"
    );
    unix_fs::symlink("_index", root.join("pub")).unwrap();

    nodex(root).args(["build", "--full"]).assert().success();
    let listed = run_json(nodex(root).args(["query", "nodes", "--fields", "id,path"]));
    let paths: Vec<&str> = listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|n| n["path"].as_str())
        .collect();
    assert_eq!(paths, vec!["docs/a.md"], "only the project's own document");

    let plan = run_json(nodex(root).arg("migrate"));
    assert_eq!(plan.get("total").and_then(Value::as_u64), Some(0), "{plan}");
}

#[cfg(unix)]
#[test]
fn a_write_seam_refuses_a_target_below_an_undescended_link() {
    // A proposal is admitted by the globs, which judge a path's spelling —
    // but membership also depends on where the path is. Below a link the
    // walk does not descend, a seam would approve the write and the next
    // build could not see the document.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/source.md",
        "---\nid: src\ntitle: S\nkind: generic\nstatus: active\n---\n# S\n",
    );
    fs::create_dir_all(root.join("real")).unwrap();
    unix_fs::symlink("../real", root.join("docs/linked")).unwrap();
    nodex(root).arg("build").assert().success();

    let proposal = "---\nid: p\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n";
    let gate = run_json(
        nodex(root)
            .args(["check", "--content", "docs/linked/exact.md=-"])
            .write_stdin(proposal),
    );
    assert_eq!(
        gate.pointer("/proposals/0/in_scope")
            .and_then(Value::as_bool),
        Some(false),
        "the gate validated nothing, and says so: {gate}"
    );

    let scaffolded = envelope_of(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Exact",
        "--path",
        "docs/linked/exact.md",
    ]));
    assert_eq!(
        scaffolded.get("ok").and_then(Value::as_bool),
        Some(false),
        "{scaffolded}"
    );
    assert!(
        scaffolded
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("directory symlink the scan does not descend"),
        "the refusal names the cause the operator can act on: {scaffolded}"
    );
    assert!(!root.join("real/exact.md").exists(), "nothing was written");

    let renamed =
        envelope_of(nodex(root).args(["rename", "docs/source.md", "docs/linked/dest.md"]));
    assert_eq!(
        renamed.get("ok").and_then(Value::as_bool),
        Some(false),
        "{renamed}"
    );
    assert!(
        root.join("docs/source.md").exists(),
        "the source stayed put"
    );
}

#[cfg(unix)]
#[test]
fn a_document_the_scan_holds_under_two_names_says_which_one_it_uses() {
    // A followed link gives a document more than one name, and the scan keeps
    // one. The name it drops is a path the operator can read that the graph
    // does not carry, so it is reported — and a seam naming the dropped one is
    // told which to use, instead of writing a document the next build files
    // under a different path.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/real/keep.md",
        "---\nid: keep\ntitle: K\nkind: generic\nstatus: active\n---\n# K\n",
    );
    unix_fs::symlink("real", root.join("docs/alias")).unwrap();

    let built = run_json(nodex(root).arg("build"));
    assert_eq!(
        built.get("nodes").and_then(Value::as_u64),
        Some(1),
        "{built}"
    );
    let dropped = built
        .get("aliased_paths")
        .and_then(Value::as_array)
        .expect("the name it does not use is reported");
    assert_eq!(dropped.len(), 1, "{built}");
    let named = dropped[0]["named"].as_str().expect("the name in use");

    // The name the report says is in use is the one the graph carries.
    let listed = run_json(nodex(root).args(["query", "nodes", "--fields", "id,path"]));
    assert_eq!(listed["items"][0]["path"].as_str(), Some(named), "{listed}");

    // A seam naming the other one is refused, and told which path to use.
    let unused = dropped[0]["path"].as_str().expect("the name not used");
    let sibling = unused.replace("keep.md", "new.md");
    let refused = envelope_of(nodex(root).args([
        "scaffold", "--kind", "generic", "--title", "New", "--path", &sibling,
    ]));
    let message = refused
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains(&named.replace("keep.md", "new.md")),
        "the refusal names the path to use: {refused}"
    );

    // And at the name in use, the write plane and the read plane agree.
    let written = run_json(nodex(root).args([
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "New",
        "--path",
        &named.replace("keep.md", "new.md"),
    ]));
    let path = written["path"].as_str().expect("path");
    nodex(root).arg("build").assert().success();
    let found = run_json(nodex(root).args(["query", "node", "--path", path]));
    assert_eq!(
        found.pointer("/node/path").and_then(Value::as_str),
        Some(path)
    );
}

#[cfg(unix)]
#[test]
fn a_seam_naming_a_document_by_an_unused_name_is_told_the_one_in_use() {
    // A followed link gives one document several names and the graph carries
    // one. Named by another, the source reads as untracked — a plain move, no
    // reference rewriting — so the real file leaves and every reference to the
    // name in use dangles. Both planes ask the same question of the same
    // channel.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\nfollow_symlinks = true\ninclude = [\"docs/**/*.md\", \"real/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "real/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        root,
        "docs/ref-a.md",
        "---\nid: ref-a\ntitle: R\nkind: generic\nstatus: active\n---\n[a](link/a.md)\n",
    );
    unix_fs::symlink("../real", root.join("docs/link")).unwrap();
    nodex(root).arg("build").assert().success();

    let in_use = run_json(nodex(root).args(["query", "node", "a"]))
        .pointer("/node/path")
        .and_then(Value::as_str)
        .expect("the graph names the document")
        .to_string();
    assert_ne!(in_use, "real/a.md", "the graph carries the other name");

    for env in [
        envelope_of(nodex(root).args(["rename", "real/a.md", "docs/a-moved.md"])),
        envelope_of(
            nodex(root)
                .args(["check", "--content", "real/a.md=-"])
                .write_stdin(
                    "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# edited\n",
                ),
        ),
    ] {
        assert_eq!(env.get("ok").and_then(Value::as_bool), Some(false), "{env}");
        assert!(
            env.pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains(&in_use),
            "the refusal names the path in use: {env}"
        );
    }
    assert!(root.join("real/a.md").exists(), "the document stayed put");

    // At the name in use, the move rewrites the reference and nothing dangles.
    let moved =
        run_envelope(nodex(root).args(["rename", &in_use, &in_use.replace("a.md", "m.md")]));
    assert_eq!(
        moved.pointer("/data/total_updated").and_then(Value::as_u64),
        Some(1),
        "{moved}"
    );
    nodex(root).arg("build").assert().success();
    let issues = run_json(nodex(root).arg("query").arg("issues"));
    assert_eq!(
        issues.get("unresolved_edges").and_then(Value::as_array),
        Some(&vec![]),
        "{issues}"
    );
}

#[cfg(unix)]
#[test]
fn the_validation_surface_states_what_the_walk_did_not_read() {
    // A link the walk declines is a boundary of what was read. A project whose
    // documents live behind one otherwise validates green against a corpus
    // that no longer holds them, and the gate CI runs says nothing.
    use std::os::unix::fs as unix_fs;
    let tmp = scratch();
    let root = tmp.path();
    let outside = scratch();
    fs::write(
        root.join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\"]\n\
         [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
    )
    .unwrap();
    write_doc(
        root,
        "docs/plain.md",
        "---\nid: plain\ntitle: P\nkind: generic\nstatus: active\n---\n# P\n",
    );
    write_doc(
        outside.path(),
        "tree/vend.md",
        "---\nid: vend\ntitle: V\nkind: generic\nstatus: active\n---\n# V\n",
    );
    unix_fs::symlink(outside.path().join("tree"), root.join("docs/linked")).unwrap();

    for command in [vec!["check"], vec!["build"]] {
        let env = run_envelope(nodex(root).args(&command));
        let said = env
            .get("warnings")
            .and_then(Value::as_array)
            .map(|ws| {
                ws.iter().any(|w| {
                    w["code"] == "scope_coverage"
                        && w["message"]
                            .as_str()
                            .unwrap_or("")
                            .contains("not descended")
                })
            })
            .unwrap_or(false);
        assert!(said, "{command:?} names the boundary: {env}");
    }
}

#[cfg(unix)]
#[test]
fn every_command_built_on_the_graph_states_what_the_walk_did_not_read() {
    // A link the walk declines bounds the corpus every one of these commands
    // reasons about: a rewrite skips a reference behind it, a plan omits a
    // document, a report renders a partial graph. Each carries its own
    // warnings to the envelope, so this is where the set is kept complete.
    //
    // The set is closed against the CLI's own grammar rather than
    // hand-maintained: every leaf `nodex export commands` reports is either
    // driven here or carries a reason for standing outside, and a leaf added
    // later belongs to neither until someone decides which. A list nobody is
    // forced to update is a list that stops being true.
    //
    // Every command also gets its own tree: the mutating ones would
    // otherwise decide the state the next one is asked about.
    use std::os::unix::fs as unix_fs;

    /// `docs/` holds two documents and a link out of the project the walk
    /// does not descend — a corpus with something in it and a boundary
    /// around it. With `populated = false` the two documents live behind
    /// the link instead, so the graph is empty *because* of the boundary:
    /// the shape where "no nodes" reads as "new project" and is not.
    fn fixture(populated: bool) -> (TempDir, TempDir) {
        let tmp = scratch();
        let outside = scratch();
        let root = tmp.path();
        fs::write(
            root.join("nodex.toml"),
            "[scope]\ninclude = [\"docs/**/*.md\"]\n\
             [[identity.id_rules]]\nkind = \"*\"\ntemplate = \"{kind}-{stem}\"\n",
        )
        .unwrap();
        let home = if populated { root } else { outside.path() };
        for (path, id) in [("docs/old.md", "old"), ("docs/new.md", "new")] {
            write_doc(
                home,
                path,
                &format!("---\nid: {id}\ntitle: T\nkind: generic\nstatus: active\n---\n# T\n"),
            );
        }
        write_doc(
            outside.path(),
            "ref.md",
            "---\nid: ref\ntitle: R\nkind: generic\nstatus: active\n---\n[[old]]\n",
        );
        fs::create_dir_all(root.join("docs")).unwrap();
        unix_fs::symlink(outside.path(), root.join("docs/linked")).unwrap();
        nodex(root).arg("build").assert().success();
        (tmp, outside)
    }

    let scaffold: Vec<&str> = vec![
        "scaffold",
        "--kind",
        "generic",
        "--title",
        "Z",
        "--path",
        "docs/z.md",
        "--dry-run",
    ];
    // Every leaf that reads the working tree's corpus, paired with the
    // invocation that exercises it.
    let driven: Vec<(&str, Vec<&str>)> = vec![
        ("check", vec!["check"]),
        ("build", vec!["build"]),
        ("report", vec!["report"]),
        ("migrate", vec!["migrate"]),
        ("retarget", vec!["retarget", "old", "new"]),
        ("rename", vec!["rename", "docs/old.md", "docs/moved.md"]),
        ("lifecycle.review", vec!["lifecycle", "review", "old"]),
        (
            "lifecycle.set",
            vec!["lifecycle", "set", "old", "--status", "archived"],
        ),
        (
            "lifecycle.supersede",
            vec!["lifecycle", "supersede", "old", "--to", "new"],
        ),
        ("scaffold", scaffold.clone()),
    ];
    // Every other leaf, with why the working tree's boundary is not its
    // answer to give. A leaf that fits none of these has not been decided
    // about, which is what the assertion below refuses.
    let standing_outside: Vec<(&str, &str)> = vec![
        (
            "init",
            "writes a config into a directory that has no corpus yet",
        ),
        (
            "status",
            "probes the snapshot against the tree; divergence is its whole answer",
        ),
        (
            "diff",
            "graphs two ref checkouts, and names each ref's omissions per ref",
        ),
        (
            "impact",
            "graphs two ref checkouts, and names each ref's omissions per ref",
        ),
        (
            "export.schema",
            "renders the config's own vocabulary, never the corpus",
        ),
        (
            "export.enums",
            "renders the config's own vocabulary, never the corpus",
        ),
        (
            "export.rules",
            "renders the rule registry, never the corpus",
        ),
        (
            "export.envelope-schema",
            "renders the JSON contract, never the corpus",
        ),
        (
            "export.config",
            "renders the loaded config, never the corpus",
        ),
        (
            "export.commands",
            "renders the CLI grammar, never the corpus",
        ),
        (
            "export.diagnostics",
            "renders the code vocabularies, never the corpus",
        ),
    ];

    // Closed against the grammar, both directions.
    let manifest =
        envelope_of(nodex(std::env::current_dir().unwrap().as_path()).args(["export", "commands"]));
    let leaves: std::collections::BTreeSet<String> = manifest
        .pointer("/data/commands")
        .and_then(Value::as_array)
        .expect("commands manifest")
        .iter()
        .map(|entry| {
            entry["path"]
                .as_array()
                .expect("path")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(".")
        })
        .collect();
    let mut classified: std::collections::BTreeSet<String> =
        driven.iter().map(|(id, _)| id.to_string()).collect();
    classified.extend(standing_outside.iter().map(|(id, _)| id.to_string()));
    // `query *` reads `graph.json` and carries the membership-divergence
    // advisory instead — one decision covering the whole family.
    let unclassified: Vec<&String> = leaves
        .iter()
        .filter(|leaf| !leaf.starts_with("query.") && !classified.contains(*leaf))
        .collect();
    assert!(
        unclassified.is_empty(),
        "these leaves are neither driven here nor given a reason to stand outside: \
         {unclassified:?}"
    );
    let stale: Vec<&String> = classified
        .iter()
        .filter(|id| !leaves.contains(*id))
        .collect();
    assert!(stale.is_empty(), "these are not leaves any more: {stale:?}");

    for (id, command) in &driven {
        let (tmp, _outside) = fixture(true);
        let env = envelope_of(nodex(tmp.path()).args(command));
        assert!(
            names_the_boundary(&env),
            "{id} names the boundary it reasoned across: {env}"
        );
    }

    // The corpus is empty *because* the walk stopped at the link. A command
    // that reads "no nodes" as "nothing to report" would go silent exactly
    // where nothing else has told the operator yet.
    for command in [
        vec!["check"],
        vec!["build"],
        vec!["report"],
        vec!["migrate"],
        scaffold,
    ] {
        let (tmp, _outside) = fixture(false);
        let env = envelope_of(nodex(tmp.path()).args(&command));
        assert_eq!(
            run_envelope(nodex(tmp.path()).arg("build"))
                .pointer("/data/nodes")
                .and_then(Value::as_i64),
            Some(0),
            "the fixture's graph is empty"
        );
        assert!(
            names_the_boundary(&env),
            "{command:?} names the boundary that emptied the corpus: {env}"
        );
    }
}

/// Whether an envelope names the undescended link the walk stopped at.
#[cfg(unix)]
fn names_the_boundary(env: &Value) -> bool {
    env.get("warnings")
        .and_then(Value::as_array)
        .is_some_and(|ws| {
            ws.iter().any(|w| {
                w["code"] == "scope_coverage"
                    && w["message"]
                        .as_str()
                        .unwrap_or("")
                        .contains("not descended")
            })
        })
}
