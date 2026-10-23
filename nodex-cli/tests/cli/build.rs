//! `build`: indexing, the build cache, and parse failures on the build
//! result.

use crate::support::{init_project, nodex, run_envelope, run_json, scratch, write_doc};
use serde_json::Value;
use std::fs;

#[test]
fn build_empty_scope_returns_zero_counts() {
    let tmp = scratch();
    init_project(tmp.path());
    let data = run_json(nodex(tmp.path()).arg("build"));
    assert_eq!(data.get("nodes").and_then(Value::as_u64), Some(0));
    assert_eq!(data.get("edges").and_then(Value::as_u64), Some(0));
    assert!(data.get("duration_ms").is_some());
}

#[test]
fn build_indexes_markdown_files() {
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/one.md",
        "---\nid: note-one\ntitle: One\nkind: generic\nstatus: active\n---\n# One\n",
    );
    write_doc(
        tmp.path(),
        "docs/two.md",
        "---\nid: note-two\ntitle: Two\nkind: generic\nstatus: active\n---\n[one](one.md)\n",
    );
    let data = run_json(nodex(tmp.path()).arg("build"));
    assert_eq!(data.get("nodes").and_then(Value::as_u64), Some(2));
    // Exactly one resolved edge (two → one).
    assert_eq!(data.get("edges").and_then(Value::as_u64), Some(1));
}

#[test]
fn build_warns_on_dead_scope_declarations() {
    // A populated project whose include glob points at a directory with
    // no docs gets a non-fatal coverage warning — the silent-config-drift
    // class that would otherwise leave a whole area unindexed unnoticed.
    let tmp = scratch();
    fs::write(
        tmp.path().join("nodex.toml"),
        "[scope]\ninclude = [\"docs/**/*.md\", \"specs/**/*.md\"]\n",
    )
    .unwrap();
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );

    let envelope = run_envelope(nodex(tmp.path()).arg("build"));
    let warnings = envelope
        .get("warnings")
        .and_then(Value::as_array)
        .expect("warnings present for dead include glob");
    assert!(
        warnings.iter().any(|w| w
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|s| s.contains("specs/**/*.md") && s.contains("no files"))),
        "dead include glob must be flagged: {warnings:?}"
    );
}

#[test]
fn build_cache_prunes_entries_for_deleted_files() {
    // Lock the cache-pruning invariant: when a previously-tracked
    // file is deleted (or moved out of scope), its cache entry must
    // be dropped on the next build. Otherwise `_index/cache.json`
    // would accumulate orphaned entries forever, eventually bloating
    // load time. The `builder::build` pipeline calls
    // `BuildCache::retain_paths` against the current scan results;
    // this test is the regression gate that catches a refactor
    // accidentally removing that call.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    let cache_path = tmp.path().join("_index").join("cache.json");
    let before: Value = serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    let entries_before = before
        .get("entries")
        .and_then(Value::as_object)
        .expect("cache.entries object");
    assert!(
        entries_before.keys().any(|k| k.ends_with("a.md"))
            && entries_before.keys().any(|k| k.ends_with("b.md")),
        "initial cache must hold both docs: {entries_before:?}"
    );

    fs::remove_file(tmp.path().join("docs/b.md")).unwrap();
    nodex(tmp.path()).arg("build").assert().success();

    let after: Value = serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    let entries_after = after
        .get("entries")
        .and_then(Value::as_object)
        .expect("cache.entries object");
    assert!(
        entries_after.keys().any(|k| k.ends_with("a.md")),
        "surviving doc must remain cached: {entries_after:?}"
    );
    assert!(
        !entries_after.keys().any(|k| k.ends_with("b.md")),
        "deleted doc's cache entry must be pruned: {entries_after:?}"
    );
}

#[test]
fn build_full_purges_cache_entries_for_files_no_longer_in_scope() {
    // `--full` starts from an empty cache, so a file deleted between
    // builds must not survive in `cache.json`. Companion lock-in to
    // the incremental-build prune test — covers the rebuild path.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\n",
    );
    nodex(tmp.path()).arg("build").assert().success();

    fs::remove_file(tmp.path().join("docs/b.md")).unwrap();
    nodex(tmp.path())
        .args(["build", "--full"])
        .assert()
        .success();

    let cache_path = tmp.path().join("_index").join("cache.json");
    let cache: Value = serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    let entries = cache
        .get("entries")
        .and_then(Value::as_object)
        .expect("cache.entries object");
    assert!(
        !entries.keys().any(|k| k.ends_with("b.md")),
        "deleted doc must be absent from --full rebuild cache: {entries:?}"
    );
}

#[test]
fn duplicate_id_error_attribution_is_cache_state_independent() {
    // The DUPLICATE_ID error names two colliding files; which is reported
    // `first` must NOT depend on whether one was served from the cache.
    // `all_nodes` is `[cached…] ++ [fresh…]`; the canonical sort before
    // the duplicate check makes the warm build (one cached) and the
    // `--full` rebuild (both fresh) agree on path order.
    let tmp = scratch();
    init_project(tmp.path());
    // `zzz.md` (id `collide`) builds first → cached. `mmm.md` sorts before
    // it and also claims `collide`; the cached file must sort AFTER the
    // fresh one to expose any insertion-order leak.
    write_doc(
        tmp.path(),
        "docs/zzz.md",
        "---\nid: collide\ntitle: Z\nkind: generic\nstatus: active\n---\nbody\n",
    );
    nodex(tmp.path()).arg("build").assert().success();
    write_doc(
        tmp.path(),
        "docs/mmm.md",
        "---\nid: collide\ntitle: M\nkind: generic\nstatus: active\n---\nbody\n",
    );

    let warm = nodex(tmp.path()).arg("build").output().expect("ran");
    let full = nodex(tmp.path())
        .args(["build", "--full"])
        .output()
        .expect("ran");
    let warm_msg = String::from_utf8_lossy(&warm.stdout).to_string();
    let full_msg = String::from_utf8_lossy(&full.stdout).to_string();
    // Both must report the path-lesser file (mmm.md) as `first`.
    let first_is_mmm = |m: &str| m.find("mmm.md").unwrap() < m.find("zzz.md").unwrap();
    assert!(first_is_mmm(&warm_msg), "warm build: {warm_msg}");
    assert!(first_is_mmm(&full_msg), "full rebuild: {full_msg}");
}

#[test]
fn build_produces_deterministic_node_detail_across_rebuilds() {
    // The JSON envelope from `query node` must be byte-identical
    // across two `build` cycles on the same input — adjacency
    // ordering, field serialisation, and edge collection are all
    // deterministic by construction; this lock-in catches an
    // accidental introduction of HashMap iteration or a clock
    // dependency in the rebuild path.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/a.md",
        "---\nid: doc-a\ntitle: A\nkind: generic\nstatus: active\n---\n# A\nSee [b](docs/b.md) and [c](docs/c.md).\n",
    );
    write_doc(
        tmp.path(),
        "docs/b.md",
        "---\nid: doc-b\ntitle: B\nkind: generic\nstatus: active\n---\n# B\nSee [a](docs/a.md).\n",
    );
    write_doc(
        tmp.path(),
        "docs/c.md",
        "---\nid: doc-c\ntitle: C\nkind: generic\nstatus: active\n---\n# C\nSee [a](docs/a.md).\n",
    );

    nodex(tmp.path()).arg("build").assert().success();
    let first = run_json(nodex(tmp.path()).args(["query", "node", "doc-a"]));
    nodex(tmp.path())
        .args(["build", "--full"])
        .assert()
        .success();
    let second = run_json(nodex(tmp.path()).args(["query", "node", "doc-a"]));
    assert_eq!(
        first, second,
        "node detail must be deterministic across rebuilds"
    );
}

#[test]
fn bom_prefixed_frontmatter_parses_correctly() {
    let tmp = scratch();
    init_project(tmp.path());
    // Write file prefixed with a UTF-8 BOM.
    let path = tmp.path().join("docs/bom.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    bytes.extend_from_slice(
        b"---\nid: bom-id\ntitle: BOM\nkind: generic\nstatus: active\n---\n# BOM\n",
    );
    fs::write(&path, bytes).unwrap();
    let data = run_json(nodex(tmp.path()).arg("build"));
    assert_eq!(
        data.get("nodes").and_then(Value::as_u64),
        Some(1),
        "BOM-prefixed file should still produce exactly one node"
    );
    // And its id came from frontmatter, not from inferred filename.
    let detail = run_json(nodex(tmp.path()).args(["query", "node", "bom-id"]));
    assert!(
        detail.get("node").is_some(),
        "bom-id should resolve; BOM must be stripped"
    );
}

#[test]
fn malformed_frontmatter_yaml_surfaces_on_build_result() {
    // Malformed YAML in a single document does NOT halt the build.
    // The file is dropped from the graph (no node) and the drop is
    // structural data: `data.parse_failures` names the failing path —
    // one bad file never blocks the operator from inspecting the rest
    // of the project, and never hides as a warning a gate ignores
    // (`check` reds the same record via the `parse_failure` rule).
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/broken.md",
        "---\ntags: [a, b\n---\n# Broken\n",
    );
    write_doc(
        tmp.path(),
        "docs/good.md",
        "---\nid: doc-good\ntitle: Good\nkind: generic\nstatus: active\n---\n# Good\n",
    );
    let envelope = run_envelope(nodex(tmp.path()).arg("build"));
    // Build must succeed; the good doc enters the graph, the broken
    // doc is recorded on the result.
    assert_eq!(
        envelope.pointer("/data/nodes").and_then(Value::as_u64),
        Some(1),
        "only the well-formed doc must appear in the graph: {envelope}"
    );
    let failures = envelope
        .pointer("/data/parse_failures")
        .and_then(Value::as_array)
        .expect("parse_failures array on the build result");
    assert_eq!(failures.len(), 1, "one drop recorded: {envelope}");
    assert_eq!(
        failures[0].get("path").and_then(Value::as_str),
        Some("docs/broken.md"),
        "the record names the failing file: {envelope}"
    );
    assert!(
        failures[0]
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains("docs/broken.md")),
        "the message carries the full error chain: {envelope}"
    );
    assert!(
        failures[0]
            .get("content_hash")
            .and_then(Value::as_str)
            .is_some_and(|h| h.len() == 64),
        "the record carries the content digest: {envelope}"
    );
}

#[test]
fn non_utf8_doc_is_a_parse_failure_through_build_check_and_status() {
    // An in-scope file the build cannot read as text takes the same
    // path as a YAML failure end to end: recorded on the build result
    // (exit 0), an Error-severity `parse_failure` in `check` (exit 1),
    // and covered-but-unbuildable for `status` — never `added_paths`,
    // never a staleness a rebuild could not clear.
    let tmp = scratch();
    init_project(tmp.path());
    write_doc(
        tmp.path(),
        "docs/good.md",
        "---\nid: doc-good\ntitle: Good\nkind: generic\nstatus: active\n---\n# Good\n",
    );
    let raw_path = tmp.path().join("docs/raw.md");
    fs::write(&raw_path, [0xFF, 0xFE, 0x01, 0x02]).unwrap();

    let envelope = run_envelope(nodex(tmp.path()).arg("build"));
    assert_eq!(
        envelope.pointer("/data/nodes").and_then(Value::as_u64),
        Some(1),
        "only the readable doc enters the graph: {envelope}"
    );
    let failures = envelope
        .pointer("/data/parse_failures")
        .and_then(Value::as_array)
        .expect("parse_failures array on the build result");
    assert_eq!(
        failures[0].get("path").and_then(Value::as_str),
        Some("docs/raw.md"),
        "the record names the unreadable file: {envelope}"
    );

    let out = nodex(tmp.path()).arg("check").assert().failure().code(1);
    let env: Value =
        serde_json::from_str(String::from_utf8_lossy(&out.get_output().stdout).trim()).unwrap();
    assert!(
        env.pointer("/data/violations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|v| {
                v.get("rule_id").and_then(Value::as_str) == Some("parse_failure")
                    && v.get("path").and_then(Value::as_str) == Some("docs/raw.md")
            }),
        "a non-UTF-8 in-scope file must red check via parse_failure: {env}"
    );

    let data = run_json(nodex(tmp.path()).arg("status"));
    assert_eq!(
        data["state"], "current",
        "a faithfully-built snapshot is current — the breakage signal belongs to check: {data}"
    );
    assert_eq!(
        data.pointer("/unbuildable_paths/0").and_then(Value::as_str),
        Some("docs/raw.md"),
        "status surfaces the path as unbuildable, not as added_paths: {data}"
    );
}

#[test]
fn frontmatter_handles_bom_and_crlf() {
    let tmp = scratch();
    init_project(tmp.path());
    // UTF-8 BOM followed by CRLF-terminated frontmatter — Windows
    // editors emit this combo. Both cleaning steps must happen.
    let mut content = Vec::new();
    content.extend_from_slice(b"\xEF\xBB\xBF");
    content.extend_from_slice(
        b"---\r\nid: bom-doc\r\ntitle: BOM\r\nkind: generic\r\nstatus: active\r\n---\r\n# Body\r\n",
    );
    let path = tmp.path().join("docs/bom.md");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    nodex(tmp.path()).arg("build").assert().success();
    let data = run_json(nodex(tmp.path()).args(["query", "node", "bom-doc"]));
    assert_eq!(
        data.pointer("/node/title").and_then(Value::as_str),
        Some("BOM"),
        "BOM + CRLF frontmatter must parse cleanly"
    );
}
