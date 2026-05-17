use schemars::JsonSchema;

pub mod cache;
pub mod resolver;
pub mod scanner;
pub mod validator;

use indexmap::IndexMap;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{
    Annotation, BodyBlockMatch, BodyLineMatch, Graph, Node, RawAnnotation, RawBodyBlockMatch,
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
/// counter snapshot, and any non-fatal warnings collected during
/// scan / parse. Not a CLI envelope — the CLI layer projects the
/// counters + timing into [`crate::command_result::BuildResult`]
/// before serialising.
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
}

/// One cache hit, materialised into the per-doc tuple the build loop
/// passes around. Named so the type appears in error messages and
/// keeps clippy happy without a leading positional `Vec<(...)>` blob.
type CachedEntry = (
    Node,
    Vec<RawEdge>,
    Vec<RawAnnotation>,
    Vec<RawBodyLineMatch>,
    Vec<RawBodyBlockMatch>,
);

#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct BuildStats {
    pub nodes: usize,
    pub edges: usize,
    pub annotations: usize,
    pub body_line_matches: usize,
    pub body_block_matches: usize,
    pub cached: usize,
    pub parsed: usize,
}

/// Build the full document graph.
pub fn build(root: &Path, config: &Config, full_rebuild: bool) -> Result<BuildOutcome> {
    // 1. Scan scope
    let paths = scanner::scan_scope(root, config)?;

    // 2. Load cache (unless full rebuild). Invalidates if config
    // changed OR if the nodex binary itself was upgraded — the cache
    // holds serialised `Node` / `RawEdge` values, so a struct-shape
    // change in a new version would otherwise let an old cache silently
    // produce stale nodes on the next build. Mixing
    // `CARGO_PKG_VERSION` into the hashed input makes every upgrade a
    // one-time full rebuild, which is cheap and correct.
    //
    // `Config` is a plain, fully-serialisable struct — silently
    // falling back to an empty hash on serialisation failure (the
    // previous `unwrap_or_default`) would let a changed config reuse
    // stale cache entries. `expect` makes the invariant explicit so
    // anyone adding a non-serialisable field to `Config` fails fast.
    let cache_path = root.join(&config.output.dir).join("cache.json");
    let config_json = serde_json::to_string(config)
        .expect("Config is defined entirely over serializable primitives");
    let config_hash = crate::hash::sha256_hex(&format!(
        "nodex={}\n{}",
        env!("CARGO_PKG_VERSION"),
        config_json
    ));
    let (mut cache, cache_warning) = if full_rebuild {
        (BuildCache::default(), None)
    } else {
        BuildCache::load(&cache_path, &config_hash)
    };
    cache.config_hash = config_hash;

    // 3. Read file contents (parallel). Collect read errors for warning.
    let read_results: Vec<(
        std::path::PathBuf,
        std::result::Result<String, std::io::Error>,
    )> = paths
        .par_iter()
        .map(|rel_path| {
            let abs_path = root.join(rel_path);
            let result = std::fs::read_to_string(&abs_path);
            (rel_path.clone(), result)
        })
        .collect();

    let mut read_warnings = Vec::new();
    let mut file_contents: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (rel_path, result) in read_results {
        match result {
            Ok(content) => file_contents.push((rel_path, content)),
            Err(e) => read_warnings.push(format!("skipped {}: {e}", rel_path.display())),
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
                entry.raw_body_block_matches.clone(),
            ));
            cached_count += 1;
        } else {
            to_parse.push((rel_path.clone(), content.clone()));
        }
    }

    // Parse cache misses in parallel
    let fresh_results: Vec<Result<(std::path::PathBuf, String, ParsedDocument)>> = to_parse
        .par_iter()
        .map(|(rel_path, content)| {
            let doc = parser::parse_document(rel_path, content, config)?;
            Ok((rel_path.clone(), content.clone(), doc))
        })
        .collect();

    let mut all_nodes: Vec<(String, Node)> = Vec::new();
    let mut all_raw_edges: Vec<(String, std::path::PathBuf, Vec<RawEdge>)> = Vec::new();
    let mut all_raw_annotations: Vec<(String, Vec<RawAnnotation>)> = Vec::new();
    let mut all_raw_body_line_matches: Vec<(String, Vec<RawBodyLineMatch>)> = Vec::new();
    let mut all_raw_body_block_matches: Vec<(String, Vec<RawBodyBlockMatch>)> = Vec::new();

    // Collect cached results
    for (node, raw_edges, raw_annotations, raw_body_line_matches, raw_body_block_matches) in
        cached_results
    {
        let id = node.id.clone();
        let path = node.path.clone();
        all_raw_edges.push((id.clone(), path, raw_edges));
        all_raw_annotations.push((id.clone(), raw_annotations));
        all_raw_body_line_matches.push((id.clone(), raw_body_line_matches));
        all_raw_body_block_matches.push((id.clone(), raw_body_block_matches));
        all_nodes.push((id, node));
    }

    // Collect fresh results and update cache. Parse failures on a
    // single document degrade gracefully — the file is dropped from
    // the build (its node never enters the graph) and the failure is
    // surfaced as an envelope warning, *not* as a build-halting
    // error. This mirrors the read-phase behaviour (lines 75-94)
    // where an unreadable file becomes a warning instead of aborting
    // the whole pipeline, and matches the user-hostile-vs-correct
    // trade-off: a single typo in one document should not block the
    // operator from inspecting the rest of the graph.
    let mut parse_warnings: Vec<String> = Vec::new();
    for result in fresh_results {
        match result {
            Ok((rel_path, content, doc)) => {
                parsed_count += 1;
                cache.insert(
                    rel_path,
                    &content,
                    doc.node.clone(),
                    &doc.raw_edges,
                    &doc.raw_annotations,
                    &doc.raw_body_line_matches,
                    &doc.raw_body_block_matches,
                );
                let id = doc.node.id.clone();
                let path = doc.node.path.clone();
                all_raw_edges.push((id.clone(), path, doc.raw_edges));
                all_raw_annotations.push((id.clone(), doc.raw_annotations));
                all_raw_body_line_matches.push((id.clone(), doc.raw_body_line_matches));
                all_raw_body_block_matches.push((id.clone(), doc.raw_body_block_matches));
                all_nodes.push((id, doc.node));
            }
            Err(err) => {
                parse_warnings.push(format!("parse failed: {err}"));
            }
        }
    }

    // 5. Check for duplicate ids
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
    for (source_id, source_path, raw_edges) in all_raw_edges {
        let resolved = resolve_edges(&source_id, raw_edges, &source_path, &path_index, &id_set);
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

    // 10. Build sorted node map
    let mut node_map = IndexMap::new();
    all_nodes.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, node) in all_nodes {
        node_map.insert(id, node);
    }

    // 10a. Materialise annotations: drop raw matches whose source kind
    // is not in the pattern's `applies_to_kind` filter, then sort by
    // (pattern_name, key, source_id, line) for deterministic output.
    // The kind filter is applied here — *after* the node's kind is
    // settled — so a kind change on a doc whose body never moved still
    // produces the right set on the next build (the cache holds the
    // raw matches, not the filtered view).
    let annotations = materialise_annotations(&node_map, &all_raw_annotations, config);

    // 10b. Materialise body-line matches: same shape as annotations.
    // Per-block `applies_to_kind` is honoured here; enum validation is
    // a check-time concern owned by `BodyLineRule`.
    let body_line_matches =
        materialise_body_line_matches(&node_map, &all_raw_body_line_matches, config);

    // 10c. Materialise body-block matches: same shape as
    // body_line. Per-block `applies_to_kind` is honoured here;
    // enum validation is a check-time concern owned by
    // `BodyBlockRule`.
    let body_block_matches =
        materialise_body_block_matches(&node_map, &all_raw_body_block_matches, config);

    // 11. Clean cache and save. The cache retains only successfully
    // parsed files; a doc that failed to parse this pass leaves its
    // previous cached entry in place (if any) so a transient YAML
    // typo doesn't force re-parsing once fixed.
    let valid_paths: Vec<_> = node_map.values().map(|n| n.path.clone()).collect();
    cache.retain_paths(&valid_paths);
    let mut warnings = read_warnings;
    warnings.extend(parse_warnings);
    if let Some(msg) = cache_warning {
        warnings.push(msg);
    }
    if let Err(e) = cache.save(&cache_path) {
        warnings.push(format!("cache save failed: {e}"));
    }

    let stats = BuildStats {
        nodes: node_map.len(),
        edges: edges.len(),
        annotations: annotations.len(),
        body_line_matches: body_line_matches.len(),
        body_block_matches: body_block_matches.len(),
        cached: cached_count,
        parsed: parsed_count,
    };

    Ok(BuildOutcome {
        graph: Graph::new(
            node_map,
            edges,
            annotations,
            body_line_matches,
            body_block_matches,
        ),
        stats,
        warnings,
    })
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
    // Pattern → scope predicate, built once so the per-marker check
    // is a hash lookup instead of a re-scan of `config.annotations`.
    let predicates: BTreeMap<&str, crate::scope_predicate::ScopePredicate<'_>> = config
        .annotations
        .iter()
        .map(|p| (p.name.as_str(), p.applies.predicate()))
        .collect();

    let mut out: Vec<Annotation> = Vec::new();
    for (source_id, raws) in raw_by_source {
        let Some(node) = node_map.get(source_id) else {
            continue;
        };
        for raw in raws {
            // A raw match whose pattern name is no longer in config
            // (operator removed the block but cache still has the
            // entries) is dropped — the canonical answer comes from
            // config + current bodies.
            let Some(predicate) = predicates.get(raw.pattern_name.as_str()) else {
                continue;
            };
            if !predicate.matches(node) {
                continue;
            }
            out.push(Annotation {
                source_id: source_id.clone(),
                pattern_name: raw.pattern_name.clone(),
                key: raw.key.clone(),
                line: raw.line,
            });
        }
    }
    out.sort_by(|a, b| {
        a.pattern_name
            .cmp(&b.pattern_name)
            .then_with(|| a.key.cmp(&b.key))
            .then_with(|| a.source_id.cmp(&b.source_id))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// Apply per-block `applies_to_kind` filtering and produce the
/// canonical sorted [`BodyLineMatch`] list that lands in the graph.
/// Symmetric with [`materialise_annotations`]: raw matches survive a
/// kind change (content unchanged → same raw set); a stale rule_name
/// from a removed `[[rules.body_line]]` block is dropped here.
/// Apply per-block `applies_to_kind` filtering and produce the
/// canonical sorted [`BodyBlockMatch`] list that lands in the graph.
/// Symmetric with [`materialise_body_line_matches`]: raw matches
/// survive a kind change (frame unchanged → same raw set); a stale
/// rule_name from a removed `[[rules.body_block]]` block is dropped.
fn materialise_body_block_matches(
    node_map: &IndexMap<String, Node>,
    raw_by_source: &[(String, Vec<RawBodyBlockMatch>)],
    config: &Config,
) -> Vec<BodyBlockMatch> {
    if raw_by_source.iter().all(|(_, v)| v.is_empty()) {
        return Vec::new();
    }
    let predicates: BTreeMap<&str, crate::scope_predicate::ScopePredicate<'_>> = config
        .rules
        .body_block
        .iter()
        .map(|b| (b.name.as_str(), b.applies.predicate()))
        .collect();

    let mut out: Vec<BodyBlockMatch> = Vec::new();
    for (source_id, raws) in raw_by_source {
        let Some(node) = node_map.get(source_id) else {
            continue;
        };
        for raw in raws {
            let Some(predicate) = predicates.get(raw.rule_name.as_str()) else {
                // The `[[rules.body_block]]` block whose pattern
                // produced this match no longer exists in config —
                // drop. Same failure mode `materialise_annotations`
                // defends against.
                continue;
            };
            if !predicate.matches(node) {
                continue;
            }
            out.push(BodyBlockMatch {
                source_id: source_id.clone(),
                rule_name: raw.rule_name.clone(),
                start_line: raw.start_line,
                end_line: raw.end_line,
                captures: raw.captures.clone(),
            });
        }
    }
    out.sort_by(|a, b| {
        a.rule_name
            .cmp(&b.rule_name)
            .then_with(|| a.source_id.cmp(&b.source_id))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    out
}

fn materialise_body_line_matches(
    node_map: &IndexMap<String, Node>,
    raw_by_source: &[(String, Vec<RawBodyLineMatch>)],
    config: &Config,
) -> Vec<BodyLineMatch> {
    if raw_by_source.iter().all(|(_, v)| v.is_empty()) {
        return Vec::new();
    }
    let predicates: BTreeMap<&str, crate::scope_predicate::ScopePredicate<'_>> = config
        .rules
        .body_line
        .iter()
        .map(|b| (b.name.as_str(), b.applies.predicate()))
        .collect();

    let mut out: Vec<BodyLineMatch> = Vec::new();
    for (source_id, raws) in raw_by_source {
        let Some(node) = node_map.get(source_id) else {
            continue;
        };
        for raw in raws {
            let Some(predicate) = predicates.get(raw.rule_name.as_str()) else {
                // The `[[rules.body_line]]` block whose pattern produced
                // this match no longer exists in config — drop. Same
                // failure mode `materialise_annotations` defends against.
                continue;
            };
            if !predicate.matches(node) {
                continue;
            }
            out.push(BodyLineMatch {
                source_id: source_id.clone(),
                rule_name: raw.rule_name.clone(),
                line: raw.line,
                captures: raw.captures.clone(),
            });
        }
    }
    out.sort_by(|a, b| {
        a.rule_name
            .cmp(&b.rule_name)
            .then_with(|| a.source_id.cmp(&b.source_id))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// Build the edges implied by `superseded_by` scalars. Each `M.superseded_by = Y`
/// becomes an edge `Y → M` with relation `"supersedes"`, matching the
/// canonical direction produced by `supersedes: [...]` vectors. When Y
/// isn't itself a known node id the synthesized edge is skipped — an
/// unresolved target here is better caught by the regular body-link
/// path than by being smuggled into the graph as `ResolvedTarget::Unresolved`.
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
        if !known_ids.contains(successor.as_str()) {
            // `successor` isn't a known node id — skip. The standard
            // resolver will record this as an unresolved edge from the
            // body-link pipeline if the content references it.
            continue;
        }
        out.push(Edge {
            source: successor.clone(),
            target: ResolvedTarget::resolved(id.as_str()),
            relation: "supersedes".to_string(),
            location: format!("frontmatter:superseded_by@{id}"),
        });
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
    use crate::config::ApplyTo;
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
            pattern_name: pattern.into(),
            key: key.into(),
            line,
        }
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
                applies: ApplyTo::default(),
            }],
            vec!["generic"],
        );
        let raws = vec![("doc-a".into(), vec![raw("promotes", "spec-x", 5)])];
        let out = materialise_annotations(&nodes, &raws, &cfg);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source_id, "doc-a");
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
                applies: ApplyTo {
                    kinds: vec!["learning".into()],
                    statuses: vec![],
                    tags: vec![],
                },
            }],
            vec!["generic", "learning"],
        );
        let raws = vec![
            ("a".into(), vec![raw("promotes", "spec-x", 1)]),
            ("b".into(), vec![raw("promotes", "spec-y", 2)]),
        ];
        let out = materialise_annotations(&nodes, &raws, &cfg);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source_id, "b");
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
                applies: ApplyTo::default(),
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
            applies: ApplyTo {
                kinds,
                statuses: vec![],
                tags: vec![],
            },
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
        assert_eq!(out[0].source_id, "b");
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
            .map(|m| (m.rule_name.as_str(), m.source_id.as_str(), m.line))
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
                    applies: ApplyTo::default(),
                },
                AnnotationConfig {
                    name: "promotes".into(),
                    pattern: r"(?P<id>\w+)".into(),
                    key: "id".into(),
                    applies: ApplyTo::default(),
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
            .map(|a| {
                (
                    a.pattern_name.as_str(),
                    a.key.as_str(),
                    a.source_id.as_str(),
                    a.line,
                )
            })
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
}
