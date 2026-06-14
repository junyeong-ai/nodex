use schemars::JsonSchema;

pub mod cache;
pub mod resolver;
pub mod scanner;
pub mod validator;

use globset::Glob;
use indexmap::IndexMap;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{
    Annotation, BodyLineMatch, Graph, GraphMeta, Node, ParseFailure, RawAnnotation,
    RawBodyLineMatch, RawEdge,
};
use crate::parser::{self, ParsedDocument};

use cache::BuildCache;
use resolver::{build_id_set, build_path_index, resolve_edges};
use validator::validate_supersedes_dag;

/// Build result.
///
/// Intermediate aggregate the builder hands back to in-process
/// callers (the CLI, benches, tests). Holds the built `Graph`, the
/// counter snapshot, and any non-fatal advisories (scope coverage
/// gaps, cache load/save problems). A document the build saw but could
/// not turn into a node — unreadable, non-UTF-8, or unparseable — is
/// typed graph data (`Graph::parse_failures`), never a warning string.
/// Not a CLI envelope — the CLI layer projects the counters + timing
/// into [`crate::command_result::BuildResult`] before serialising.
///
/// `warnings` lives on the outcome, not on `stats` — the JSON envelope
/// contract puts warnings at the envelope level, never inside the data
/// payload, and the same separation here keeps any future serializer
/// of `BuildStats` from accidentally re-nesting them (the trap that
/// `ScaffoldResult` had to be split out of).
pub struct BuildOutcome {
    pub graph: Graph,
    pub stats: BuildStats,
    pub warnings: Vec<String>,
    /// Project-root-relative paths a `conditional_exclude` rule dropped
    /// from scope, so the exclusion is reported rather than silent.
    pub conditionally_excluded: Vec<String>,
}

/// One cache hit, materialised into the per-doc tuple the build loop
/// passes around. Named so the type appears in error messages and
/// keeps clippy happy without a leading positional `Vec<(...)>` blob.
type CachedEntry = (
    Node,
    Vec<RawEdge>,
    Vec<RawAnnotation>,
    Vec<RawBodyLineMatch>,
);

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct BuildStats {
    pub nodes: usize,
    pub edges: usize,
    pub annotations: usize,
    pub body_line_matches: usize,
    pub cached: usize,
    pub parsed: usize,
}

/// How a build run interacts with the cache and the on-disk content.
/// The mode couples the two decisions that must never drift apart: only
/// a working-tree build may persist the refreshed cache, and only a
/// read-only build may substitute proposed bytes — so "proposed bytes
/// leak into `cache.json`" is unrepresentable rather than merely
/// avoided by caller discipline.
enum BuildMode<'a> {
    /// Graph the working tree as-is and persist the refreshed cache.
    WorkingTree { full_rebuild: bool },
    /// Read-only: graph the working tree with each `(rel_path, content)`
    /// substituted for that file's bytes (or injected, for an in-scope
    /// path not yet on disk). Never persists the cache. An empty overlay
    /// is the read-only working-tree build.
    Overlay(&'a [(PathBuf, String)]),
}

/// Build the full document graph from the working tree.
pub fn build(root: &Path, config: &Config, full_rebuild: bool) -> Result<BuildOutcome> {
    build_inner(root, config, BuildMode::WorkingTree { full_rebuild })
}

/// Hash of the graph-shaping config surface, recorded as
/// [`GraphMeta::config_hash`] on every built snapshot: SHA-256 over the
/// binary version plus the two compiler-enforced projections that
/// determine graph content — [`parser::ParseConfig`] (parse surface)
/// and [`scanner::ScanConfig`] (membership surface). Config that only
/// steers validation or query ranking never perturbs it, so retuning
/// `trust` / `similarity` / `detection` / `schema` can never flag a
/// snapshot stale. The version salt mirrors the cache policy: a binary
/// upgrade marks existing snapshots for one rebuild.
pub fn graph_config_hash(config: &Config) -> String {
    #[derive(serde::Serialize)]
    struct Keyed<'a> {
        nodex: &'static str,
        parse: parser::ParseConfig<'a>,
        scan: scanner::ScanConfig<'a>,
    }
    let canonical = serde_json::to_string(&Keyed {
        nodex: env!("CARGO_PKG_VERSION"),
        parse: parser::ParseConfig::new(config),
        scan: scanner::ScanConfig::new(config),
    })
    .expect("config projections are serialisable");
    crate::hash::sha256_hex(&canonical)
}

/// Build the graph as if `overlay` were the on-disk content: each
/// `(rel_path, content)` replaces that file's bytes, or injects an
/// in-scope path not yet on disk. Out-of-scope overlay paths are
/// ignored — a path the project never graphs has no identity to check.
///
/// The build is read-only: it never persists the cache, so validating
/// an unwritten proposal cannot mutate `cache.json` or leak the proposed
/// bytes into a later build. An empty overlay yields the working-tree
/// graph, read-only. This is the substrate behind
/// `check --content <path>=-`: an agent's edit (or a batch of them) is graphed and validated
/// through the one build / resolve / rule pipeline *before* it reaches
/// disk, so no consumer has to reimplement nodex's parser to gate a
/// write.
pub fn build_with_overlay(
    root: &Path,
    config: &Config,
    overlay: &[(PathBuf, String)],
) -> Result<BuildOutcome> {
    build_inner(root, config, BuildMode::Overlay(overlay))
}

fn build_inner(root: &Path, config: &Config, mode: BuildMode<'_>) -> Result<BuildOutcome> {
    let full_rebuild = matches!(mode, BuildMode::WorkingTree { full_rebuild: true });
    let overlay: &[(PathBuf, String)] = match mode {
        BuildMode::Overlay(overlay) => overlay,
        BuildMode::WorkingTree { .. } => &[],
    };
    let persist_cache = matches!(mode, BuildMode::WorkingTree { .. });

    // 1. Scan scope. The scan is the single scope authority: overlay
    // paths participate exactly as if their proposed bytes were on
    // disk (membership, conditional excludes), so an overlay graph and
    // the real post-write build can never disagree about scope.
    let scanner::ScopeScan {
        paths,
        conditionally_excluded,
    } = scanner::scan_scope_with_overlay(root, config, overlay)?;

    // 2. Load cache (unless full rebuild). Invalidates if config
    // changed OR if the nodex binary itself was upgraded — the cache
    // holds serialised `Node` / `RawEdge` values, so a struct-shape
    // change in a new version would otherwise let an old cache silently
    // produce stale nodes on the next build. Mixing
    // `CARGO_PKG_VERSION` into the hashed input makes every upgrade a
    // one-time full rebuild, which is cheap and correct.
    //
    // The cache key is derived from the parse-affecting config surface
    // (`ParseConfig`) plus the binary version — see `ParseConfig::cache_key`.
    // Whitespace/comment edits never perturb it; semantic changes
    // (id_rules reordering, a new annotation pattern) always do.
    let cache_path = root.join(&config.output.dir).join("cache.json");
    let parse_config = parser::ParseConfig::new(config);
    let config_hash = parse_config.cache_key();
    let (mut cache, cache_warning) = if full_rebuild {
        (BuildCache::default(), None)
    } else {
        BuildCache::load(&cache_path, &config_hash)
    };
    cache.config_hash = config_hash;

    // 3. Read file contents (parallel). Proposed bytes substitute the
    // disk read for overlaid paths — the scan already admitted them, so
    // this is the single seam where bytes enter the pipeline. A file
    // the seam cannot deliver as text — unreadable, or not valid UTF-8
    // — is exactly as unbuildable as one whose YAML fails: it becomes a
    // typed [`ParseFailure`] (an Error-severity `parse_failure`
    // violation, a `target_unparsed` resolution cause, and a
    // covered-but-unbuildable path for `status`), never a warning a
    // gate ignores. Raw bytes are read first so the failure record can
    // carry the real content digest; a hard I/O failure (no bytes to
    // hash) records the empty string as its sentinel — no sha256
    // renders empty, so the status content probe can never confirm
    // `current` for a file the build could not read (in particular, a
    // later readable-and-empty file does not collide with it).
    let read_results: Vec<(
        std::path::PathBuf,
        std::result::Result<String, ParseFailure>,
    )> = paths
        .par_iter()
        .map(|rel_path| {
            if let Some(proposed) = scanner::overlay_content(overlay, rel_path) {
                return (rel_path.clone(), Ok(proposed.to_string()));
            }
            let abs_path = root.join(rel_path);
            // ParseFailure is a serialized graph record: its message
            // names the document by its graph identity (forward-slash,
            // like the `path` field and `Node.path`'s serialized form),
            // never by the platform-native spelling.
            let graph_path = crate::path_guard::forward_string(rel_path);
            let record_path = std::path::PathBuf::from(&graph_path);
            let result = match std::fs::read(&abs_path) {
                Ok(bytes) => String::from_utf8(bytes).map_err(|e| {
                    let message = crate::error::chain(&Error::Io {
                        path: record_path.clone(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            e.utf8_error(),
                        ),
                    });
                    ParseFailure {
                        path: graph_path.clone(),
                        message,
                        content_hash: crate::hash::sha256_hex(e.as_bytes()),
                    }
                }),
                Err(source) => Err(ParseFailure {
                    path: graph_path.clone(),
                    message: crate::error::chain(&Error::Io {
                        path: record_path.clone(),
                        source,
                    }),
                    content_hash: String::new(),
                }),
            };
            (rel_path.clone(), result)
        })
        .collect();

    let mut parse_failures: Vec<ParseFailure> = Vec::new();
    let mut file_contents: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (rel_path, result) in read_results {
        match result {
            Ok(content) => file_contents.push((rel_path, content)),
            Err(failure) => parse_failures.push(failure),
        }
    }

    // 4. Parse documents (parallel, with caching)
    let mut cached_count = 0usize;
    let mut parsed_count = 0usize;

    // Separate into cached hits and cache misses
    let mut cached_results: Vec<CachedEntry> = Vec::new();
    let mut to_parse: Vec<(std::path::PathBuf, String)> = Vec::new();

    for (rel_path, content) in &file_contents {
        if let Some(entry) = cache.get(rel_path, content) {
            cached_results.push((
                entry.node.clone(),
                entry.raw_edges.clone(),
                entry.raw_annotations.clone(),
                entry.raw_body_line_matches.clone(),
            ));
            cached_count += 1;
        } else {
            to_parse.push((rel_path.clone(), content.clone()));
        }
    }

    // Parse cache misses in parallel. Each result keeps its path and
    // content so a failure stays attributable — the Err arm below
    // becomes a typed ParseFailure record, never an anonymous drop.
    let fresh_results: Vec<(std::path::PathBuf, String, Result<ParsedDocument>)> = to_parse
        .par_iter()
        .map(|(rel_path, content)| {
            let doc = parser::parse_document(rel_path, content, &parse_config);
            (rel_path.clone(), content.clone(), doc)
        })
        .collect();

    let mut all_nodes: Vec<(String, Node)> = Vec::new();
    let mut all_raw_edges: Vec<(String, std::path::PathBuf, Vec<RawEdge>)> = Vec::new();
    let mut all_raw_annotations: Vec<(String, Vec<RawAnnotation>)> = Vec::new();
    let mut all_raw_body_line_matches: Vec<(String, Vec<RawBodyLineMatch>)> = Vec::new();

    // Collect cached results
    for (node, raw_edges, raw_annotations, raw_body_line_matches) in cached_results {
        let id = node.id.clone();
        let path = node.path.clone();
        all_raw_edges.push((id.clone(), path, raw_edges));
        all_raw_annotations.push((id.clone(), raw_annotations));
        all_raw_body_line_matches.push((id.clone(), raw_body_line_matches));
        all_nodes.push((id, node));
    }

    // Collect fresh results and update cache. A parse failure on a
    // single document never halts the build — its node simply never
    // enters the graph, mirroring the read-phase failures above — but
    // the drop is first-class graph data: a typed [`ParseFailure`]
    // serialized into the snapshot, surfaced structurally on the build
    // result, and turned into an Error-severity `parse_failure`
    // violation by `check`. The message carries the full error chain
    // (parse layer + wrapped yaml/json cause — each `Display` names
    // only its own layer); the content hash is the same digest the
    // cache keys on, so a snapshot consumer can tell "same broken
    // bytes" from "changed since build".
    for (rel_path, content, result) in fresh_results {
        match result {
            Ok(doc) => {
                parsed_count += 1;
                cache.insert(
                    rel_path,
                    &content,
                    doc.node.clone(),
                    &doc.raw_edges,
                    &doc.raw_annotations,
                    &doc.raw_body_line_matches,
                );
                let id = doc.node.id.clone();
                let path = doc.node.path.clone();
                all_raw_edges.push((id.clone(), path, doc.raw_edges));
                all_raw_annotations.push((id.clone(), doc.raw_annotations));
                all_raw_body_line_matches.push((id.clone(), doc.raw_body_line_matches));
                all_nodes.push((id, doc.node));
            }
            Err(err) => {
                // Re-attribute the parser's error to the document's
                // graph identity before rendering: the message is part
                // of a serialized record and names paths forward-slash,
                // like the `path` field beside it.
                let graph_path = crate::path_guard::forward_string(&rel_path);
                let err = match err {
                    Error::Parse { source, .. } => Error::Parse {
                        path: std::path::PathBuf::from(&graph_path),
                        source,
                    },
                    Error::Io { source, .. } => Error::Io {
                        path: std::path::PathBuf::from(&graph_path),
                        source,
                    },
                    // Every path-embedding variant the parser can
                    // return is re-attributed above; the remaining
                    // variants carry no path and render as-is.
                    other => other,
                };
                parse_failures.push(ParseFailure {
                    path: graph_path,
                    message: crate::error::chain(&err),
                    content_hash: crate::hash::sha256_hex(&content),
                });
            }
        }
    }
    parse_failures.sort_by(|a, b| a.path.cmp(&b.path));

    // Canonicalise node order up front (by id, then path) so every
    // downstream consumer is cache-state independent. `all_nodes` is
    // assembled as `[cached…] ++ [fresh…]`, an order that shifts as the
    // cache warms — without this sort the duplicate-id report below would
    // name the two colliding files (`first` / `second`) in an order that
    // flips between a warm build and a `--full` rebuild. Path is the
    // tie-break so two nodes sharing an id still order deterministically.
    all_nodes.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));

    // 5. Check for duplicate ids. `all_nodes` is sorted, so a colliding
    // pair is adjacent and `first` is the path-lesser of the two —
    // deterministic regardless of which file the cache served.
    {
        let mut seen: BTreeMap<&str, &Path> = BTreeMap::new();
        for (id, node) in &all_nodes {
            if let Some(&first_path) = seen.get(id.as_str()) {
                return Err(Error::DuplicateId {
                    id: id.clone(),
                    first: first_path.to_path_buf(),
                    second: node.path.clone(),
                });
            }
            seen.insert(id.as_str(), &node.path);
        }
    }

    // 6. Build resolution indices
    let path_index = build_path_index(&all_nodes);
    let id_set = build_id_set(&all_nodes);

    // 7. Resolve edges
    let mut edges = Vec::new();
    for (source, source_path, raw_edges) in all_raw_edges {
        let resolved = resolve_edges(
            &source,
            raw_edges,
            &source_path,
            &path_index,
            &id_set,
            &config.parser.extensions,
        );
        edges.extend(resolved);
    }

    // 7b. Translate each `superseded_by` scalar into its canonical
    //     `supersedes` edge. frontmatter `supersedes: [X]` on node N
    //     yields edge N → X; frontmatter `superseded_by: Y` on node M
    //     yields edge Y → M (same direction, different authoring style).
    //     Without this step, documents that author only the
    //     `superseded_by` field never show up in `backlinks` / `node`
    //     incoming, and `chain` had to traverse a scalar pointer that
    //     lived outside the edge graph — two representations of the
    //     same relation. Materialising both into edges unifies the
    //     graph so every query uses the same traversal.
    edges.extend(derive_superseded_by_edges(&all_nodes));

    // Dedupe by (source, target, relation) so documents that declare
    // both sides (N.supersedes=[X] AND X.superseded_by=N) produce a
    // single edge rather than two identical ones. The body-link
    // resolver never produces duplicates, so this only affects
    // frontmatter-sourced edges.
    dedupe_edges(&mut edges);

    // 8. Validate supersedes DAG
    validate_supersedes_dag(&edges)?;

    // 9. Sort edges for deterministic output
    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.relation.cmp(&b.relation))
            .then_with(|| a.location.cmp(&b.location))
    });

    // 10. Build the node map. `all_nodes` was already canonically sorted
    // (by id, then path) before the duplicate-id check, so insertion
    // order here is deterministic without a second sort.
    let mut node_map = IndexMap::new();
    for (id, node) in all_nodes {
        node_map.insert(id, node);
    }

    // 10a. Materialise annotations: drop raw matches whose source kind
    // is not in the pattern's `kinds` filter, then sort by
    // (name, key, source, line) for deterministic output.
    // The kind filter is applied here — *after* the node's kind is
    // settled — so a kind change on a doc whose body never moved still
    // produces the right set on the next build (the cache holds the
    // raw matches, not the filtered view).
    let annotations = materialise_annotations(&node_map, &all_raw_annotations, config);

    // 10b. Materialise body-line matches: same shape as annotations.
    // Per-block `kinds` is honoured here; enum validation is
    // a check-time concern owned by `BodyLineRule`.
    let body_line_matches =
        materialise_body_line_matches(&node_map, &all_raw_body_line_matches, config);

    // 11. Clean cache and save. `retain_paths` keeps only this pass's
    // successfully parsed files; a failed doc's entry is dropped and
    // the doc fully re-parses once fixed — the cache never vouches for
    // a path the current graph does not contain.
    let valid_paths: Vec<_> = node_map.values().map(|n| n.path.clone()).collect();
    cache.retain_paths(&valid_paths);
    let mut warnings = scope_coverage_warnings(config, &paths, &node_map);
    if let Some(msg) = cache_warning {
        warnings.push(msg);
    }
    // A read-only build (`build_with_overlay`, behind `check --content`)
    // never persists the cache: the proposed bytes must not become a
    // (path, hash) entry a later real build would serve, and the on-disk
    // files' entries stay untouched.
    if persist_cache && let Err(e) = cache.save(root, &cache_path) {
        warnings.push(format!("cache save failed: {e}"));
    }

    let stats = BuildStats {
        nodes: node_map.len(),
        edges: edges.len(),
        annotations: annotations.len(),
        body_line_matches: body_line_matches.len(),
        cached: cached_count,
        parsed: parsed_count,
    };

    Ok(BuildOutcome {
        graph: Graph::new(
            node_map,
            edges,
            annotations,
            body_line_matches,
            parse_failures,
            GraphMeta {
                nodex_version: env!("CARGO_PKG_VERSION").to_string(),
                config_hash: graph_config_hash(config),
            },
        ),
        stats,
        warnings,
        conditionally_excluded: conditionally_excluded
            .iter()
            .map(|p| crate::path_guard::forward_string(p))
            .collect(),
    })
}

/// Diagnose config declarations that matched nothing this build —
/// the silent-config-drift class the project's "no silent runtime
/// skips" doctrine forbids. A zero-match `scope.include` glob, a
/// `kind_rules`/`id_rules` entry that applies to no file, are almost
/// always typos or stale config (e.g. an include that points at a
/// renamed directory, leaving a whole kind silently absent). Emitted as
/// non-fatal warnings so the operator sees the dead declaration without
/// the build failing.
fn scope_coverage_warnings(
    config: &Config,
    paths: &[PathBuf],
    nodes: &IndexMap<String, Node>,
) -> Vec<String> {
    // An empty scan is either a brand-new project or a mis-scoped one (a
    // typo'd `scope.include` glob that misses the real docs) — and the
    // latter is a silent false-pass: `check` reports zero violations on a
    // corpus it never read. Surface ONE top-level warning so the empty
    // graph is never invisible; the per-declaration coverage diagnostics
    // below stay suppressed, because with nothing scanned every glob
    // trivially matches nothing and listing each would be pure noise.
    if paths.is_empty() {
        return vec![
            "scope matched no files — nothing was scanned, so check has nothing to validate; \
             verify scope.include if your project has documents"
                .to_string(),
        ];
    }

    let rels: Vec<String> = paths
        .iter()
        .map(|p| crate::path_guard::forward_string(p))
        .collect();
    let matcher = |glob: &str| {
        Glob::new(glob)
            .expect("identity/scope globs are validated by Config::load")
            .compile_matcher()
    };

    let mut out = Vec::new();

    // A leading literal segment of `pattern` that `scope.prune_dirs`
    // prunes — so the walk never descended into it and the include can
    // never match, no matter what is on disk. Turns a misleading generic
    // "matched no files" into a precise, actionable cause.
    let pruned_segment = |pattern: &str| -> Option<String> {
        pattern
            .split('/')
            .take_while(|seg| !seg.contains(['*', '?', '[', ']', '{']))
            .find(|seg| config.scope.prune_dirs.iter().any(|d| d == seg))
            .map(String::from)
    };

    for pattern in &config.scope.include {
        let m = matcher(pattern);
        if !rels.iter().any(|r| m.is_match(r)) {
            let hint = match pruned_segment(pattern) {
                Some(seg) => format!(
                    " — its path lies under {seg:?}, which scope.prune_dirs prunes from the walk; \
                     remove {seg:?} from scope.prune_dirs to scan it"
                ),
                None => String::new(),
            };
            out.push(format!(
                "scope.include pattern {pattern:?} matched no files{hint}"
            ));
        }
    }

    for rule in &config.identity.kind_rules {
        let m = matcher(&rule.glob);
        if !rels.iter().any(|r| m.is_match(r)) {
            out.push(format!(
                "identity.kind_rules glob {:?} (kind {:?}) matched no files",
                rule.glob, rule.kind
            ));
        }
    }

    for rule in &config.identity.id_rules {
        if nodes.is_empty() {
            break;
        }
        let glob = rule.glob.as_deref().map(matcher);
        let applies = nodes.values().any(|n| {
            (rule.kind == "*" || rule.kind == n.kind.as_str())
                && glob
                    .as_ref()
                    .is_none_or(|m| m.is_match(crate::path_guard::forward_string(&n.path)))
        });
        if !applies {
            out.push(format!(
                "identity.id_rules entry (kind {:?}, glob {:?}) applied to no node",
                rule.kind, rule.glob
            ));
        }
    }

    // Documents that ended up as the fallback kind even though the
    // project declares kind_rules: a classification scheme exists, but
    // these paths slipped through it. A project with *no* kind_rules has
    // opted out of classification (generic-everywhere is intentional), so
    // there is no gap to report. Surfacing the slip-through makes the
    // fallback observable instead of silently absorbing an unclassified
    // file as `generic`.
    if !config.identity.kind_rules.is_empty() {
        let kind_matchers: Vec<_> = config
            .identity
            .kind_rules
            .iter()
            .map(|rule| matcher(&rule.glob))
            .collect();
        let mut unclassified: Vec<String> = nodes
            .values()
            .filter(|n| n.kind.as_str() == crate::parser::identity::FALLBACK_KIND)
            .map(|n| crate::path_guard::forward_string(&n.path))
            .filter(|path| !kind_matchers.iter().any(|m| m.is_match(path)))
            .collect();
        unclassified.sort();
        for path in unclassified {
            out.push(format!(
                "{path:?} has kind {fallback:?} but no identity.kind_rules glob matches it; \
                 add a rule covering this path (map it to {fallback:?} explicitly if intended)",
                fallback = crate::parser::identity::FALLBACK_KIND
            ));
        }
    }

    out
}

/// Apply per-pattern scope (kind / status / tag) filtering and
/// produce the canonical sorted [`Annotation`] list that lands in
/// the graph.
fn materialise_annotations(
    node_map: &IndexMap<String, Node>,
    raw_by_source: &[(String, Vec<RawAnnotation>)],
    config: &Config,
) -> Vec<Annotation> {
    if raw_by_source.iter().all(|(_, v)| v.is_empty()) {
        return Vec::new();
    }
    // Pattern → kinds filter, built once so the per-marker check
    // is a hash lookup instead of a re-scan of `config.annotations`.
    let kinds_by_name: BTreeMap<&str, &[String]> = config
        .annotations
        .iter()
        .map(|p| (p.name.as_str(), p.kinds.as_slice()))
        .collect();

    let mut out: Vec<Annotation> = Vec::new();
    for (source, raws) in raw_by_source {
        let Some(node) = node_map.get(source) else {
            continue;
        };
        for raw in raws {
            // A raw match whose pattern name is no longer in config
            // (operator removed the block but cache still has the
            // entries) is dropped — the canonical answer comes from
            // config + current bodies.
            let Some(kinds) = kinds_by_name.get(raw.name.as_str()) else {
                continue;
            };
            if !node.matches_kinds(kinds) {
                continue;
            }
            out.push(Annotation {
                source: source.clone(),
                name: raw.name.clone(),
                key: raw.key.clone(),
                line: raw.line,
            });
        }
    }
    out.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// Apply per-block `kinds` filtering and produce the canonical sorted
/// [`BodyLineMatch`] list that lands in the graph. Symmetric with
/// [`materialise_annotations`]: raw matches survive a kind change
/// (content unchanged → same raw set); a stale rule_name from a
/// removed `[[rules.body_line]]` block is dropped here.
fn materialise_body_line_matches(
    node_map: &IndexMap<String, Node>,
    raw_by_source: &[(String, Vec<RawBodyLineMatch>)],
    config: &Config,
) -> Vec<BodyLineMatch> {
    if raw_by_source.iter().all(|(_, v)| v.is_empty()) {
        return Vec::new();
    }
    let kinds_by_name: BTreeMap<&str, &[String]> = config
        .rules
        .body_line
        .iter()
        .map(|b| (b.name.as_str(), b.kinds.as_slice()))
        .collect();

    let mut out: Vec<BodyLineMatch> = Vec::new();
    for (source, raws) in raw_by_source {
        let Some(node) = node_map.get(source) else {
            continue;
        };
        for raw in raws {
            let Some(kinds) = kinds_by_name.get(raw.rule_name.as_str()) else {
                // The `[[rules.body_line]]` block whose pattern produced
                // this match no longer exists in config — drop. Same
                // failure mode `materialise_annotations` defends against.
                continue;
            };
            if !node.matches_kinds(kinds) {
                continue;
            }
            out.push(BodyLineMatch {
                source: source.clone(),
                rule_name: raw.rule_name.clone(),
                line: raw.line,
                captures: raw.captures.clone(),
            });
        }
    }
    out.sort_by(|a, b| {
        a.rule_name
            .cmp(&b.rule_name)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// Build the edges implied by `superseded_by` scalars. Each `M.superseded_by = Y`
/// where `Y` is a known node becomes an edge `Y → M` with relation
/// `"supersedes"`, matching the canonical direction produced by
/// `supersedes: [...]` vectors. When `Y` is *not* a known node id, the
/// document's dangling declaration is surfaced as an unresolved
/// `superseded_by` edge from `M` — so `query issues` reports it exactly
/// like a bad `supersedes` / `implements` / `related` id, never silently
/// dropping it (`superseded_by` is a frontmatter scalar, so it would never
/// reappear through the body-link pipeline).
fn derive_superseded_by_edges(
    all_nodes: &[(String, crate::model::Node)],
) -> Vec<crate::model::Edge> {
    use crate::model::{Edge, ResolvedTarget};
    let known_ids: std::collections::BTreeSet<&str> =
        all_nodes.iter().map(|(id, _)| id.as_str()).collect();
    let mut out = Vec::new();
    for (id, node) in all_nodes {
        let Some(ref successor) = node.superseded_by else {
            continue;
        };
        if known_ids.contains(successor.as_str()) {
            out.push(Edge {
                source: successor.clone(),
                target: ResolvedTarget::resolved(id.as_str()),
                relation: "supersedes".to_string(),
                location: format!("frontmatter:superseded_by@{id}"),
            });
        } else {
            out.push(Edge {
                source: id.clone(),
                target: ResolvedTarget::unresolved(
                    successor,
                    crate::model::UnresolvedCause::IdNotFound,
                ),
                relation: "superseded_by".to_string(),
                location: format!("frontmatter:superseded_by@{id}"),
            });
        }
    }
    out
}

/// Remove duplicate edges by typed identity, keeping the first
/// occurrence (which carries the original `location`).
fn dedupe_edges(edges: &mut Vec<crate::model::Edge>) {
    let mut seen = std::collections::HashSet::with_capacity(edges.len());
    edges.retain(|edge| seen.insert(edge.identity()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AnnotationConfig, BodyLineRuleConfig, KindsConfig};
    use crate::model::{Kind, Node, Status};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str, kind: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.into(),
            kind: Kind::new(kind),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            content_hash: String::new(),
            parse_issues: vec![],
            inferred_fields: vec![],
        }
    }

    fn build_map(nodes: Vec<Node>) -> IndexMap<String, Node> {
        let mut m = IndexMap::new();
        for n in nodes {
            m.insert(n.id.clone(), n);
        }
        m
    }

    fn config_with(annotations: Vec<AnnotationConfig>, kinds: Vec<&str>) -> Config {
        Config {
            kinds: KindsConfig {
                allowed: kinds.into_iter().map(String::from).collect(),
            },
            annotations,
            ..Config::default()
        }
    }

    fn raw(pattern: &str, key: &str, line: usize) -> RawAnnotation {
        RawAnnotation {
            name: pattern.into(),
            key: key.into(),
            line,
        }
    }

    fn kind_rule(glob: &str, kind: &str) -> crate::config::KindRule {
        crate::config::KindRule {
            glob: glob.into(),
            kind: kind.into(),
        }
    }

    #[test]
    fn unknown_superseded_by_becomes_unresolved_edge() {
        // A typo'd `superseded_by` must surface as an unresolved edge (so
        // `query issues` reports it), never be silently dropped.
        let mut m = node("m", "generic");
        m.superseded_by = Some("ghost".to_string());
        let edges = derive_superseded_by_edges(&[("m".to_string(), m)]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "m");
        assert_eq!(edges[0].relation, "superseded_by");
        assert!(matches!(
            &edges[0].target,
            crate::model::ResolvedTarget::Unresolved { raw, .. } if raw == "ghost"
        ));
    }

    #[test]
    fn known_superseded_by_becomes_canonical_supersedes_edge() {
        let mut m = node("m", "generic");
        m.superseded_by = Some("y".to_string());
        let y = node("y", "generic");
        let edges = derive_superseded_by_edges(&[("m".to_string(), m), ("y".to_string(), y)]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "y");
        assert_eq!(edges[0].relation, "supersedes");
        assert_eq!(edges[0].target.id(), Some("m"));
    }

    #[test]
    fn fallback_kind_warns_only_on_slip_through_when_rules_declared() {
        // A classification scheme exists (`specs/**` → spec), but a
        // generic node sits outside it → observable gap.
        let mut config = config_with(vec![], vec!["generic", "spec"]);
        config.identity.kind_rules = vec![kind_rule("specs/**", "spec")];
        let nodes = build_map(vec![node("loose", "generic")]);
        let paths = vec![PathBuf::from("loose.md")];
        let warnings = scope_coverage_warnings(&config, &paths, &nodes);
        assert!(
            warnings.iter().any(
                |w| w.contains("loose.md") && w.contains("no identity.kind_rules glob matches")
            ),
            "expected slip-through warning, got {warnings:?}"
        );
    }

    #[test]
    fn fallback_kind_silent_when_no_rules_declared() {
        // No kind_rules → classification opted out; generic-everywhere is
        // intentional, so no gap to report.
        let config = config_with(vec![], vec!["generic"]);
        let nodes = build_map(vec![node("loose", "generic")]);
        let paths = vec![PathBuf::from("loose.md")];
        let warnings = scope_coverage_warnings(&config, &paths, &nodes);
        assert!(
            warnings.is_empty(),
            "expected no warnings, got {warnings:?}"
        );
    }

    #[test]
    fn fallback_kind_silent_when_rule_covers_it() {
        // A generic node whose path is explicitly mapped to generic by a
        // rule is intentional — no warning.
        let mut config = config_with(vec![], vec!["generic"]);
        config.identity.kind_rules = vec![kind_rule("**/*.md", "generic")];
        let nodes = build_map(vec![node("loose", "generic")]);
        let paths = vec![PathBuf::from("loose.md")];
        let warnings = scope_coverage_warnings(&config, &paths, &nodes);
        assert!(
            warnings.is_empty(),
            "expected no warnings, got {warnings:?}"
        );
    }

    #[test]
    fn empty_scan_warns_once_so_a_mis_scoped_project_is_not_a_silent_false_pass() {
        // Zero files scanned is either a new project or a typo'd
        // scope.include that misses the real docs — the latter makes
        // `check` pass on an unread corpus. Surface exactly one top-level
        // warning; do NOT emit the per-declaration coverage noise.
        let config = config_with(vec![], vec!["generic"]);
        let nodes = build_map(vec![]);
        let warnings = scope_coverage_warnings(&config, &[], &nodes);
        assert_eq!(warnings.len(), 1, "exactly one warning: {warnings:?}");
        assert!(
            warnings[0].contains("scope matched no files"),
            "got {warnings:?}"
        );
    }

    #[test]
    fn materialise_empty_input_returns_empty() {
        let nodes = build_map(vec![]);
        let cfg = Config::default();
        assert!(materialise_annotations(&nodes, &[], &cfg).is_empty());
    }

    #[test]
    fn materialise_passes_when_no_kind_filter() {
        let nodes = build_map(vec![node("doc-a", "generic")]);
        let cfg = config_with(
            vec![AnnotationConfig {
                name: "promotes".into(),
                pattern: r"(?P<id>\w+)".into(),
                key: "id".into(),

                kinds: vec![],
            }],
            vec!["generic"],
        );
        let raws = vec![("doc-a".into(), vec![raw("promotes", "spec-x", 5)])];
        let out = materialise_annotations(&nodes, &raws, &cfg);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "doc-a");
        assert_eq!(out[0].key, "spec-x");
    }

    #[test]
    fn materialise_filters_by_kind_when_filter_set() {
        // Pattern restricted to "learning" — a "generic" doc's match
        // must be dropped. This is the load-bearing self-consistency
        // check: without it, every kind would surface in `query
        // annotations` and the per-pattern semantic would mean nothing.
        let nodes = build_map(vec![node("a", "generic"), node("b", "learning")]);
        let cfg = config_with(
            vec![AnnotationConfig {
                name: "promotes".into(),
                pattern: r"(?P<id>\w+)".into(),
                key: "id".into(),
                kinds: vec!["learning".into()],
            }],
            vec!["generic", "learning"],
        );
        let raws = vec![
            ("a".into(), vec![raw("promotes", "spec-x", 1)]),
            ("b".into(), vec![raw("promotes", "spec-y", 2)]),
        ];
        let out = materialise_annotations(&nodes, &raws, &cfg);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "b");
    }

    #[test]
    fn materialise_drops_raw_with_pattern_no_longer_in_config() {
        // The cache may still hold a raw match for a `[[annotations]]`
        // block the operator removed from `nodex.toml`. The canonical
        // answer comes from `config + current bodies` — the stale raw
        // must be silently dropped, not surfaced.
        let nodes = build_map(vec![node("a", "generic")]);
        let cfg = config_with(vec![], vec!["generic"]);
        let raws = vec![("a".into(), vec![raw("removed-pattern", "x", 1)])];
        let out = materialise_annotations(&nodes, &raws, &cfg);
        assert!(out.is_empty());
    }

    #[test]
    fn materialise_drops_raw_for_missing_source_node() {
        // A raw entry whose source id never made it into the node map
        // (parse failure, scope exclusion) must not produce a dangling
        // annotation. Defensive guard against partial-state inputs.
        let nodes = build_map(vec![]);
        let cfg = config_with(
            vec![AnnotationConfig {
                name: "promotes".into(),
                pattern: r"(?P<id>\w+)".into(),
                key: "id".into(),

                kinds: vec![],
            }],
            vec!["generic"],
        );
        let raws = vec![("ghost".into(), vec![raw("promotes", "x", 1)])];
        assert!(materialise_annotations(&nodes, &raws, &cfg).is_empty());
    }

    // ─── materialise_body_line_matches ─────────────────────────────────

    fn raw_match(rule: &str, line: usize, captures: &[(&str, &str)]) -> RawBodyLineMatch {
        RawBodyLineMatch {
            rule_name: rule.into(),
            line,
            captures: captures
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    fn body_line_config(blocks: Vec<BodyLineRuleConfig>) -> Config {
        Config {
            kinds: KindsConfig {
                allowed: vec!["spec".into(), "generic".into(), "learning".into()],
            },
            rules: crate::config::RulesConfig {
                body_line: blocks,
                ..Default::default()
            },
            ..Config::default()
        }
    }

    fn block_for(name: &str, kinds: Vec<String>) -> BodyLineRuleConfig {
        let mut enums = BTreeMap::new();
        enums.insert("k".into(), vec!["v".into()]);
        BodyLineRuleConfig {
            name: name.into(),
            pattern: r"(?P<k>\w+)".into(),
            kinds,
            enums,
        }
    }

    #[test]
    fn materialise_body_line_empty_input_returns_empty() {
        let nodes = build_map(vec![]);
        let cfg = body_line_config(vec![block_for("r", vec![])]);
        assert!(materialise_body_line_matches(&nodes, &[], &cfg).is_empty());
    }

    #[test]
    fn materialise_body_line_filters_by_kind() {
        // Block restricted to "spec" — a "generic" doc's match must
        // be dropped. Parallel guarantee to materialise_annotations.
        let nodes = build_map(vec![node("a", "generic"), node("b", "spec")]);
        let cfg = body_line_config(vec![block_for("r", vec!["spec".into()])]);
        let raws = vec![
            ("a".into(), vec![raw_match("r", 1, &[("k", "v")])]),
            ("b".into(), vec![raw_match("r", 2, &[("k", "v")])]),
        ];
        let out = materialise_body_line_matches(&nodes, &raws, &cfg);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, "b");
    }

    #[test]
    fn materialise_body_line_drops_stale_rule_name() {
        // A cached raw match for a `[[rules.body_line]]` block the
        // operator removed from `nodex.toml` must not surface — the
        // canonical answer comes from config + current bodies.
        let nodes = build_map(vec![node("a", "generic")]);
        let cfg = body_line_config(vec![]); // no blocks configured
        let raws = vec![("a".into(), vec![raw_match("removed", 1, &[("k", "v")])])];
        assert!(materialise_body_line_matches(&nodes, &raws, &cfg).is_empty());
    }

    #[test]
    fn materialise_body_line_drops_match_for_missing_source() {
        let nodes = build_map(vec![]);
        let cfg = body_line_config(vec![block_for("r", vec![])]);
        let raws = vec![("ghost".into(), vec![raw_match("r", 1, &[("k", "v")])])];
        assert!(materialise_body_line_matches(&nodes, &raws, &cfg).is_empty());
    }

    #[test]
    fn materialise_body_line_output_sorted_by_rule_source_line() {
        let nodes = build_map(vec![node("a", "generic"), node("b", "generic")]);
        let cfg = body_line_config(vec![block_for("alpha", vec![]), block_for("beta", vec![])]);
        let raws = vec![
            (
                "b".into(),
                vec![
                    raw_match("beta", 1, &[("k", "v")]),
                    raw_match("alpha", 5, &[("k", "v")]),
                ],
            ),
            (
                "a".into(),
                vec![
                    raw_match("alpha", 9, &[("k", "v")]),
                    raw_match("alpha", 2, &[("k", "v")]),
                ],
            ),
        ];
        let out = materialise_body_line_matches(&nodes, &raws, &cfg);
        let sig: Vec<(&str, &str, usize)> = out
            .iter()
            .map(|m| (m.rule_name.as_str(), m.source.as_str(), m.line))
            .collect();
        assert_eq!(
            sig,
            vec![
                ("alpha", "a", 2),
                ("alpha", "a", 9),
                ("alpha", "b", 5),
                ("beta", "b", 1),
            ]
        );
    }

    #[test]
    fn materialise_output_sorted_by_pattern_key_source_line() {
        let nodes = build_map(vec![node("a", "generic"), node("b", "generic")]);
        let cfg = config_with(
            vec![
                AnnotationConfig {
                    name: "research".into(),
                    pattern: r"(?P<t>\w+)".into(),
                    key: "t".into(),

                    kinds: vec![],
                },
                AnnotationConfig {
                    name: "promotes".into(),
                    pattern: r"(?P<id>\w+)".into(),
                    key: "id".into(),

                    kinds: vec![],
                },
            ],
            vec!["generic"],
        );
        let raws = vec![
            (
                "b".into(),
                vec![raw("promotes", "k", 9), raw("research", "z", 1)],
            ),
            (
                "a".into(),
                vec![raw("promotes", "k", 5), raw("promotes", "j", 1)],
            ),
        ];
        let out = materialise_annotations(&nodes, &raws, &cfg);
        // Expected order: promotes/j(a,1), promotes/k(a,5), promotes/k(b,9), research/z(b,1).
        let signature: Vec<(&str, &str, &str, usize)> = out
            .iter()
            .map(|a| (a.name.as_str(), a.key.as_str(), a.source.as_str(), a.line))
            .collect();
        assert_eq!(
            signature,
            vec![
                ("promotes", "j", "a", 1),
                ("promotes", "k", "a", 5),
                ("promotes", "k", "b", 9),
                ("research", "z", "b", 1),
            ]
        );
    }

    #[test]
    fn malformed_doc_becomes_a_typed_parse_failure_without_halting_the_build() {
        // A whole-document failure (unparseable YAML) never halts the
        // build: the good doc enters the graph, the bad one is recorded
        // as canonical graph data — path, full error chain, and the
        // content digest the cache keys on.
        let dir = tempfile::TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("good.md"),
            "---\nid: good\ntitle: Good\n---\n# Good\n",
        )
        .unwrap();
        let bad_bytes = "---\nid: [unclosed\n---\n# Bad\n";
        std::fs::write(docs.join("bad.md"), bad_bytes).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".to_string()];

        let outcome = build(dir.path(), &config, true).expect("build never halts on one bad doc");
        assert_eq!(outcome.graph.node_count(), 1, "good doc graphed");
        let failures = outcome.graph.parse_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path, "docs/bad.md");
        assert!(
            failures[0].message.contains("docs/bad.md") && failures[0].message.contains("yaml"),
            "message carries the full chain: {}",
            failures[0].message
        );
        assert_eq!(
            failures[0].content_hash,
            crate::hash::sha256_hex(bad_bytes),
            "the digest matches the bytes the failed parse consumed"
        );
        assert!(
            !outcome.warnings.iter().any(|w| w.contains("bad.md")),
            "the drop is typed data, not a warning string: {:?}",
            outcome.warnings
        );

        // A warm rebuild reports the same failure (failed docs are never
        // cached) and keeps the good doc served from the cache.
        let warm = build(dir.path(), &config, false).expect("warm build");
        assert_eq!(warm.graph.parse_failures().len(), 1);
        assert_eq!(warm.stats.cached, 1, "good doc served from cache");
    }

    #[test]
    fn non_utf8_doc_becomes_a_typed_parse_failure_with_its_byte_digest() {
        // An in-scope file the read seam cannot deliver as text is
        // exactly as unbuildable as one whose YAML fails: a typed
        // ParseFailure carrying the digest of the raw bytes — never a
        // warning string, never a silent vanish.
        let dir = tempfile::TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("good.md"),
            "---\nid: good\ntitle: Good\n---\n# Good\n",
        )
        .unwrap();
        let raw_bytes: &[u8] = &[0xFF, 0xFE, 0x01, 0x02];
        std::fs::write(docs.join("raw.md"), raw_bytes).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".to_string()];

        let outcome = build(dir.path(), &config, true).expect("build never halts on one bad doc");
        assert_eq!(outcome.graph.node_count(), 1, "good doc graphed");
        let failures = outcome.graph.parse_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path, "docs/raw.md");
        assert!(
            failures[0].message.contains("docs/raw.md") && failures[0].message.contains("utf-8"),
            "message names the file and the cause: {}",
            failures[0].message
        );
        assert_eq!(
            failures[0].content_hash,
            crate::hash::sha256_hex(raw_bytes),
            "the digest covers the raw bytes the read delivered"
        );
        assert!(
            !outcome.warnings.iter().any(|w| w.contains("raw.md")),
            "the failure is typed data, not a warning string: {:?}",
            outcome.warnings
        );

        // The recorded failure feeds every downstream channel like a
        // YAML failure: an Error-severity `parse_failure` violation…
        let report = crate::rules::check(&outcome.graph, &config, dir.path(), None);
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule_id == "parse_failure" && v.path.as_deref() == Some("docs/raw.md")),
            "{:?}",
            report.violations
        );
        // …and covered-but-unbuildable for the divergence probe (never
        // membership divergence a rebuild could not clear).
        let divergence = crate::status::compute_divergence(
            &outcome.graph,
            &config,
            dir.path(),
            crate::status::DivergenceProbe::Content,
        )
        .expect("probe");
        assert!(
            divergence.added_paths.is_empty(),
            "a recorded failure is covered, not added: {divergence:?}"
        );
        assert_eq!(divergence.changed_paths, Some(vec![]));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_doc_becomes_a_typed_parse_failure_with_the_empty_sentinel_hash() {
        // A hard I/O failure (permissions) delivers no bytes at all —
        // the read seam records a typed ParseFailure whose message is
        // the io error chain and whose content_hash is the empty-string
        // sentinel: no sha256 renders empty, so the status content
        // probe can never confirm `current` for a file the build could
        // not read, even one that is later readable and empty.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("good.md"),
            "---\nid: good\ntitle: Good\n---\n# Good\n",
        )
        .unwrap();
        let blocked = docs.join("blocked.md");
        std::fs::write(&blocked, "---\nid: blocked\n---\n# Blocked\n").unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".to_string()];

        let outcome = build(dir.path(), &config, true).expect("build never halts on one bad doc");
        assert_eq!(outcome.graph.node_count(), 1, "good doc graphed");
        let failures = outcome.graph.parse_failures();
        assert_eq!(failures.len(), 1, "exactly one failure: {failures:?}");
        assert_eq!(failures[0].path, "docs/blocked.md");
        assert!(
            failures[0].message.contains("io error at docs/blocked.md")
                && failures[0].message.contains("os error"),
            "message is the io error chain: {}",
            failures[0].message
        );
        assert_eq!(
            failures[0].content_hash, "",
            "no bytes to hash — the empty-string sentinel, never a real digest"
        );
        assert!(
            !outcome.warnings.iter().any(|w| w.contains("blocked.md")),
            "the failure is typed data, not a warning string: {:?}",
            outcome.warnings
        );

        // The recorded failure feeds check as a node-less Error-severity
        // parse_failure violation…
        let report = crate::rules::check(&outcome.graph, &config, dir.path(), None);
        assert!(
            report.violations.iter().any(|v| {
                v.rule_id == "parse_failure"
                    && v.node_id.is_none()
                    && v.path.as_deref() == Some("docs/blocked.md")
            }),
            "{:?}",
            report.violations
        );

        // …and the content probe can never confirm `current` for it:
        // still-unreadable reads as changed, and so does a later
        // readable-and-empty file — sha256("") is a real digest, the
        // sentinel is not.
        let changed_while_blocked = crate::status::compute_divergence(
            &outcome.graph,
            &config,
            dir.path(),
            crate::status::DivergenceProbe::Content,
        )
        .expect("probe")
        .changed_paths
        .expect("content probe measures");
        assert!(
            changed_while_blocked.contains(&"docs/blocked.md".to_string()),
            "unreadable now ⇒ unconfirmable: {changed_while_blocked:?}"
        );

        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&blocked, "").unwrap();
        let changed_when_empty = crate::status::compute_divergence(
            &outcome.graph,
            &config,
            dir.path(),
            crate::status::DivergenceProbe::Content,
        )
        .expect("probe")
        .changed_paths
        .expect("content probe measures");
        assert!(
            changed_when_empty.contains(&"docs/blocked.md".to_string()),
            "an unreadable-at-build file must never read current: {changed_when_empty:?}"
        );
    }

    #[test]
    fn build_records_provenance_meta_on_the_graph() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        let mut config = Config::default();
        config.scope.include = vec!["docs/**/*.md".to_string()];

        let outcome = build(dir.path(), &config, true).expect("build");
        let meta = outcome.graph.meta();
        assert_eq!(meta.nodex_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            meta.config_hash,
            graph_config_hash(&config),
            "the recorded hash is the graph-shaping config hash"
        );
    }

    #[test]
    fn graph_config_hash_ignores_check_only_config_and_tracks_membership() {
        // Check-only tuning (trust / detection / schema) never perturbs
        // the hash — a snapshot must not read stale for config the
        // graph's content never consumed. Membership surface (scope)
        // always does. `statuses.terminal` participates only when a
        // conditional_exclude rule exists to consult it.
        let baseline = Config::default();
        let mut tuned = Config::default();
        tuned.trust.weights.status = 0.9;
        tuned.detection.stale_days = Some(7);
        tuned.schema.required = vec!["created".into()];
        assert_eq!(graph_config_hash(&baseline), graph_config_hash(&tuned));

        let mut scoped = Config::default();
        scoped.scope.include = vec!["docs/**/*.md".into()];
        assert_ne!(graph_config_hash(&baseline), graph_config_hash(&scoped));

        let mut terminal_only = Config::default();
        terminal_only.statuses.terminal = vec!["archived".into()];
        assert_eq!(
            graph_config_hash(&baseline),
            graph_config_hash(&terminal_only),
            "terminal retuning is invisible without a conditional_exclude"
        );

        let with_rule = |terminal: Vec<String>| {
            let mut c = Config::default();
            c.statuses.allowed = vec!["active".into(), "archived".into()];
            c.statuses.terminal = terminal;
            c.scope.conditional_exclude = vec![crate::config::ConditionalExclude {
                parent_glob: "specs/*/spec.md".into(),
                child_glob: "specs/**/tasks/**".into(),
                condition: "status_terminal".into(),
            }];
            c
        };
        assert_ne!(
            graph_config_hash(&with_rule(vec![])),
            graph_config_hash(&with_rule(vec!["archived".into()])),
            "with a conditional_exclude, the consulted terminal set is hashed"
        );
    }

    #[test]
    fn config_hash_changes_when_id_rules_reordered() {
        let mut config1 = Config::default();
        let mut config2 = Config::default();

        // Add two id_rules
        config1.identity.id_rules = vec![
            crate::config::IdRule {
                kind: "adr".into(),
                glob: Some("docs/decisions/*.md".into()),
                template: "adr-{stem}".into(),
            },
            crate::config::IdRule {
                kind: "*".into(),
                glob: None,
                template: "{kind}-{stem}".into(),
            },
        ];

        // config2 has same rules but reversed order
        config2.identity.id_rules = vec![
            crate::config::IdRule {
                kind: "*".into(),
                glob: None,
                template: "{kind}-{stem}".into(),
            },
            crate::config::IdRule {
                kind: "adr".into(),
                glob: Some("docs/decisions/*.md".into()),
                template: "adr-{stem}".into(),
            },
        ];

        let hash1 = crate::parser::ParseConfig::new(&config1).cache_key();
        let hash2 = crate::parser::ParseConfig::new(&config2).cache_key();

        assert_ne!(hash1, hash2, "id_rules reordering must change config hash");
    }

    #[test]
    fn config_hash_same_when_kind_rules_reordered_but_same_semantics() {
        let mut config1 = Config::default();
        let mut config2 = Config::default();

        // Both have same kind_rules in same order
        config1.identity.kind_rules = vec![crate::config::KindRule {
            glob: "docs/decisions/*.md".into(),
            kind: "adr".into(),
        }];
        config2.identity.kind_rules = config1.identity.kind_rules.clone();

        let hash1 = crate::parser::ParseConfig::new(&config1).cache_key();
        let hash2 = crate::parser::ParseConfig::new(&config2).cache_key();

        assert_eq!(hash1, hash2, "identical semantics must have same hash");
    }

    #[test]
    fn config_hash_changes_when_annotations_reordered() {
        let mut config1 = Config::default();
        let mut config2 = Config::default();

        config1.annotations = vec![
            crate::config::AnnotationConfig {
                name: "todo".into(),
                pattern: "TODO:(.*)".into(),
                key: "message".into(),
                kinds: vec![],
            },
            crate::config::AnnotationConfig {
                name: "fixme".into(),
                pattern: "FIXME:(.*)".into(),
                key: "message".into(),
                kinds: vec![],
            },
        ];

        config2.annotations = vec![
            config1.annotations[1].clone(),
            config1.annotations[0].clone(),
        ];

        let hash1 = crate::parser::ParseConfig::new(&config1).cache_key();
        let hash2 = crate::parser::ParseConfig::new(&config2).cache_key();

        assert_ne!(
            hash1, hash2,
            "annotation reordering must change hash (order is critical for materialisation)"
        );
    }

    #[test]
    fn config_hash_changes_when_link_patterns_reordered() {
        let mut config1 = Config::default();
        let mut config2 = Config::default();

        config1.parser.link_patterns = vec![
            crate::config::LinkPattern {
                relation: "implements".into(),
                pattern: "SPEC:(.*)".into(),
            },
            crate::config::LinkPattern {
                relation: "cites".into(),
                pattern: "@cite(.*)".into(),
            },
        ];

        config2.parser.link_patterns = vec![
            config1.parser.link_patterns[1].clone(),
            config1.parser.link_patterns[0].clone(),
        ];

        let hash1 = crate::parser::ParseConfig::new(&config1).cache_key();
        let hash2 = crate::parser::ParseConfig::new(&config2).cache_key();

        assert_ne!(
            hash1, hash2,
            "link_pattern reordering must change hash (first match wins in extraction)"
        );
    }

    #[test]
    fn config_hash_ignores_parse_irrelevant_config() {
        // The cache stores per-document *parse* output. Config that only
        // steers query ranking or validation — trust / similarity /
        // detection weights and thresholds — never changes a parsed
        // node, so tuning it must NOT invalidate the whole cache. This
        // is the precision the hand-rolled hash lacked: it folded every
        // config field in and forced a full reparse on a one-line weight
        // tweak.
        let baseline = Config::default();
        let mut tuned = Config::default();
        tuned.trust.weights.status = 0.9;
        tuned.similarity.default_limit = 99;
        tuned.detection.stale_days = Some(7);

        assert_eq!(
            crate::parser::ParseConfig::new(&baseline).cache_key(),
            crate::parser::ParseConfig::new(&tuned).cache_key(),
            "parse-irrelevant config must not change the cache key"
        );
    }

    #[test]
    fn config_hash_changes_when_status_fallback_changes() {
        // A frontmatter-less document's status is inferred from config
        // (`initial_status_for`), so the status-fallback inputs are a
        // genuine parse dependency. Changing `statuses.initial` must
        // invalidate the cache or a cached node keeps a stale default.
        let mut config1 = Config::default();
        let mut config2 = Config::default();
        config1.statuses.initial = Some("draft".into());
        config2.statuses.initial = Some("active".into());

        assert_ne!(
            crate::parser::ParseConfig::new(&config1).cache_key(),
            crate::parser::ParseConfig::new(&config2).cache_key(),
            "status-fallback change must change the cache key"
        );
    }

    #[test]
    fn config_hash_ignores_terminal_statuses() {
        // `statuses.terminal` is a pure check-time / lifecycle concern —
        // parsing reads only the resolved initial status. Editing it must
        // NOT bust the build cache; ParseConfig stores the resolved
        // `&str`, so the key cannot depend on `terminal` by construction.
        let baseline = Config::default();
        let mut retuned = Config::default();
        retuned.statuses.terminal = vec!["archived".into()];

        assert_eq!(
            crate::parser::ParseConfig::new(&baseline).cache_key(),
            crate::parser::ParseConfig::new(&retuned).cache_key(),
            "a terminal-status edit must not invalidate the parse cache"
        );
    }
}
