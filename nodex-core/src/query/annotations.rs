//! Group body annotations by `(pattern, key)`. Operates purely on the
//! pre-extracted [`crate::model::Annotation`] records that live on the
//! graph — no filesystem access at query time, no regex re-evaluation.

use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

use crate::config::BUILTIN_FRONTMATTER_FIELDS;
use crate::model::{Annotation, Graph, Node};

/// Every marker for one `[[annotations]]` pattern, grouped by the
/// captured key. Patterns whose extraction yielded no entries are
/// omitted from the result entirely.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AnnotationGroup {
    pub name: String,
    pub entries: Vec<AnnotationEntry>,
}

/// One grouping key inside a pattern: how many times it was captured,
/// and where each occurrence lives.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AnnotationEntry {
    pub key: String,
    pub count: usize,
    pub sources: Vec<AnnotationSourceRef>,
}

/// One occurrence of a marker. `path` is the forward-slashed source
/// path so callers can render it without re-derivation; `frontmatter`
/// carries the source node's frontmatter fields the caller asked for
/// (only populated when `find_annotations` is called with a non-empty
/// field list). Omitted from JSON when no field was requested.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AnnotationSourceRef {
    pub source: String,
    pub path: String,
    pub line: usize,
    // `default + skip_serializing_if = empty` mirrors the convention `Node`
    // uses for its repeated fields (`tags`, `implements`, `attrs`, …). The
    // pair is load-bearing: it both omits the key from the emitted JSON
    // *and* marks the derived `JsonSchema` field as optional, so a real
    // envelope (no `--with-frontmatter` requested → no `frontmatter` key)
    // still validates against the emitted schema.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub frontmatter: BTreeMap<String, serde_json::Value>,
}

/// Knobs for [`find_annotations`]. Mixes predicates (`name`,
/// `min_count`) with an enrichment request (`with_frontmatter`), so
/// the project naming convention for this shape is `*Options` not
/// `*Filter` — see `nodex-core/CLAUDE.md`. Construct with
/// `..AnnotationOptions::default()` to specify only the knobs that
/// differ from the no-op defaults.
#[derive(Debug, Clone, Default)]
pub struct AnnotationOptions<'a> {
    /// Restrict output to a single annotation name (matching
    /// `[[annotations]].name`). `None` = every declared annotation.
    pub name: Option<&'a str>,
    /// Per-source frontmatter enrichment. For each source, the named
    /// fields are read from the source node (built-in or `attrs`) and
    /// surfaced under `sources[].frontmatter`. Empty slice → the
    /// `frontmatter` key is omitted from the JSON entirely. Callers
    /// validate the field names against
    /// [`crate::config::Config::declared_fields_universe`] before
    /// invoking; this function does not re-check.
    pub with_frontmatter: &'a [String],
    /// Drop [`AnnotationEntry`]s whose `count` is below the threshold;
    /// groups left empty after the filter are dropped from the result
    /// as well. The natural primitive for promotion-candidate /
    /// repeated-topic queries that would otherwise filter the full
    /// result in a downstream pipeline. `0` = no-op (every entry
    /// kept) and is the default.
    pub min_count: usize,
}

/// All annotations on the graph, grouped by pattern → key. Behaviour
/// is tuned via [`AnnotationOptions`]; pass `&AnnotationOptions::default()`
/// for the unfiltered listing.
///
/// Output ordering is deterministic: groups are sorted by `name`;
/// within a group, entries are sorted by `key`; within an entry,
/// sources are sorted by `(source, line)`.
pub fn find_annotations(graph: &Graph, opts: &AnnotationOptions<'_>) -> Vec<AnnotationGroup> {
    let mut by_pattern: std::collections::BTreeMap<&str, Vec<&Annotation>> =
        std::collections::BTreeMap::new();
    for ann in graph.annotations() {
        if let Some(filter) = opts.name
            && ann.name != filter
        {
            continue;
        }
        by_pattern.entry(ann.name.as_str()).or_default().push(ann);
    }

    by_pattern
        .into_iter()
        .map(|(name, anns)| build_group(graph, name, anns, opts.with_frontmatter))
        // Apply the count threshold inside each group, then drop
        // groups that fell empty. Keeping the filter at the seam
        // between grouping and emission means callers never see a
        // half-populated group whose only entries failed the filter.
        .filter_map(|mut g| {
            if opts.min_count > 1 {
                g.entries.retain(|e| e.count >= opts.min_count);
            }
            if g.entries.is_empty() { None } else { Some(g) }
        })
        .collect()
}

fn build_group(
    graph: &Graph,
    name: &str,
    mut anns: Vec<&Annotation>,
    frontmatter_fields: &[String],
) -> AnnotationGroup {
    // Already sorted as a side-effect of the builder's canonical
    // ordering — but the slice we received is *partitioned* by pattern,
    // not necessarily sorted within. Re-sort by (key, source, line)
    // so the per-group view is independently deterministic.
    anns.sort_by(|a, b| {
        a.key
            .cmp(&b.key)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.line.cmp(&b.line))
    });

    let mut entries: Vec<AnnotationEntry> = Vec::new();
    let mut cursor = 0;
    while cursor < anns.len() {
        let current_key = anns[cursor].key.as_str();
        let mut end = cursor;
        while end < anns.len() && anns[end].key == current_key {
            end += 1;
        }
        let group = &anns[cursor..end];
        entries.push(AnnotationEntry {
            key: current_key.to_string(),
            count: group.len(),
            sources: group
                .iter()
                .map(|a| build_source_ref(graph, a, frontmatter_fields))
                .collect(),
        });
        cursor = end;
    }

    AnnotationGroup {
        name: name.to_string(),
        entries,
    }
}

fn build_source_ref(
    graph: &Graph,
    annotation: &Annotation,
    frontmatter_fields: &[String],
) -> AnnotationSourceRef {
    let node = graph.node(&annotation.source);
    let path = node
        .map(|n| crate::path_guard::forward_string(&n.path))
        .unwrap_or_default();
    let frontmatter = match node {
        Some(n) if !frontmatter_fields.is_empty() => collect_frontmatter(n, frontmatter_fields),
        _ => BTreeMap::new(),
    };
    AnnotationSourceRef {
        source: annotation.source.clone(),
        path,
        line: annotation.line,
        frontmatter,
    }
}

/// Read the requested frontmatter fields off a node.
///
/// Built-in fields are looked up against the canonical serde
/// projection of [`Node`] itself, so the field vocabulary stays
/// single-sourced through [`BUILTIN_FRONTMATTER_FIELDS`] and any
/// future built-in addition (struct field + const entry) is surfaced
/// automatically — no parallel match arm to maintain. Project-declared
/// fields are drawn from [`Node::attrs`], the canonical catch-all for
/// any frontmatter key not built in.
///
/// Fields the source node does not carry (a built-in `Option::None`,
/// an empty `Vec`, or a missing `attrs` key) are omitted entirely
/// rather than surfaced as JSON `null` — matching the
/// `skip_serializing_if` convention already used elsewhere in the
/// envelope.
fn collect_frontmatter(node: &Node, fields: &[String]) -> BTreeMap<String, serde_json::Value> {
    let node_json =
        serde_json::to_value(node).expect("Node is always JSON-serialisable by construction");
    let mut out = BTreeMap::new();
    for field in fields {
        if BUILTIN_FRONTMATTER_FIELDS.contains(&field.as_str()) {
            // Built-in: top-level key on Node's serde projection. If
            // the field was omitted by `skip_serializing_if`, it's
            // absent from the JSON — propagate that absence.
            if let Some(v) = node_json.get(field) {
                out.insert(field.clone(), v.clone());
            }
        } else if let Some(v) = node.attrs.get(field) {
            // Project-declared: lives under the `attrs` catch-all.
            out.insert(field.clone(), v.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Annotation, Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.to_string(),
            kind: Kind::new("learning"),
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

    fn graph(nodes: Vec<Node>, anns: Vec<Annotation>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(
            map,
            vec![],
            anns,
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    fn ann(source: &str, pattern: &str, key: &str, line: usize) -> Annotation {
        Annotation {
            source: source.into(),
            name: pattern.into(),
            key: key.into(),
            line,
        }
    }

    #[test]
    fn empty_graph_returns_empty() {
        let g = graph(vec![], vec![]);
        assert!(
            find_annotations(
                &g,
                &AnnotationOptions {
                    name: None,
                    with_frontmatter: &[],
                    min_count: 1
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn groups_by_key_and_counts() {
        let g = graph(
            vec![node("a"), node("b")],
            vec![
                ann("a", "promotes", "spec-x", 5),
                ann("b", "promotes", "spec-x", 12),
                ann("a", "promotes", "spec-y", 9),
            ],
        );
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &[],
                min_count: 1,
            },
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "promotes");
        assert_eq!(groups[0].entries.len(), 2);
        let spec_x = groups[0]
            .entries
            .iter()
            .find(|m| m.key == "spec-x")
            .unwrap();
        assert_eq!(spec_x.count, 2);
        assert_eq!(spec_x.sources.len(), 2);
    }

    #[test]
    fn pattern_filter_isolates_one_pattern() {
        let g = graph(
            vec![node("a")],
            vec![ann("a", "promotes", "x", 1), ann("a", "research", "y", 2)],
        );
        let only_promotes = find_annotations(
            &g,
            &AnnotationOptions {
                name: Some("promotes"),
                with_frontmatter: &[],
                min_count: 1,
            },
        );
        assert_eq!(only_promotes.len(), 1);
        assert_eq!(only_promotes[0].name, "promotes");
        let unknown = find_annotations(
            &g,
            &AnnotationOptions {
                name: Some("ghost"),
                with_frontmatter: &[],
                min_count: 1,
            },
        );
        assert!(unknown.is_empty());
    }

    #[test]
    fn sources_are_sorted_by_source_then_line() {
        let g = graph(
            vec![node("alpha"), node("beta")],
            vec![
                ann("beta", "promotes", "k", 4),
                ann("alpha", "promotes", "k", 9),
                ann("alpha", "promotes", "k", 3),
            ],
        );
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &[],
                min_count: 1,
            },
        );
        let sources = &groups[0].entries[0].sources;
        // alpha (line 3) < alpha (line 9) < beta (line 4).
        assert_eq!(sources[0].source, "alpha");
        assert_eq!(sources[0].line, 3);
        assert_eq!(sources[1].source, "alpha");
        assert_eq!(sources[1].line, 9);
        assert_eq!(sources[2].source, "beta");
    }

    #[test]
    fn source_path_resolved_from_node() {
        let g = graph(vec![node("doc-1")], vec![ann("doc-1", "promotes", "k", 1)]);
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &[],
                min_count: 1,
            },
        );
        assert_eq!(groups[0].entries[0].sources[0].path, "docs/doc-1.md");
    }

    #[test]
    fn frontmatter_omitted_when_no_fields_requested() {
        let g = graph(vec![node("a")], vec![ann("a", "promotes", "x", 1)]);
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &[],
                min_count: 1,
            },
        );
        let src = &groups[0].entries[0].sources[0];
        assert!(
            src.frontmatter.is_empty(),
            "empty frontmatter must be present-but-empty (serde omits on serialise)"
        );
        // And the JSON envelope must actually omit it.
        let v = serde_json::to_value(src).unwrap();
        assert!(
            v.get("frontmatter").is_none(),
            "frontmatter key must be omitted: {v}"
        );
    }

    #[test]
    fn frontmatter_includes_requested_builtin_fields() {
        use chrono::NaiveDate;
        let mut a_node = node("a");
        a_node.created = Some(NaiveDate::from_ymd_opt(2026, 1, 12).unwrap());
        a_node.tags = vec!["auth".into(), "policy".into()];
        let g = graph(vec![a_node], vec![ann("a", "promotes", "x", 1)]);

        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                with_frontmatter: &["created".to_string(), "tags".to_string()],
                ..Default::default()
            },
        );
        let fm = &groups[0].entries[0].sources[0].frontmatter;
        assert_eq!(
            fm.get("created").and_then(|v| v.as_str()),
            Some("2026-01-12")
        );
        let tags = fm.get("tags").and_then(|v| v.as_array()).unwrap();
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn frontmatter_includes_attrs_for_project_declared_fields() {
        let mut a_node = node("a");
        a_node
            .attrs
            .insert("priority".into(), serde_json::Value::String("high".into()));
        let g = graph(vec![a_node], vec![ann("a", "promotes", "x", 1)]);
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &["priority".to_string()],
                min_count: 1,
            },
        );
        let fm = &groups[0].entries[0].sources[0].frontmatter;
        assert_eq!(fm.get("priority").and_then(|v| v.as_str()), Some("high"));
    }

    #[test]
    fn frontmatter_omits_field_when_node_lacks_it() {
        let g = graph(vec![node("a")], vec![ann("a", "promotes", "x", 1)]);
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &["created".to_string()],
                min_count: 1,
            },
        );
        let fm = &groups[0].entries[0].sources[0].frontmatter;
        assert!(
            fm.get("created").is_none(),
            "absent field must be omitted, not surfaced as null: {fm:?}"
        );
    }

    #[test]
    fn frontmatter_rejects_path_lookup_even_though_node_serialises_path() {
        // `Node` serialises a `path` field but it is *not* a frontmatter
        // field (it's the on-disk location, not authoring metadata).
        // `BUILTIN_FRONTMATTER_FIELDS` excludes it, so the lookup must
        // miss — consumers wanting the file path read it from
        // `AnnotationSourceRef::path` instead.
        let g = graph(vec![node("a")], vec![ann("a", "promotes", "x", 1)]);
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &["path".to_string()],
                min_count: 1,
            },
        );
        let fm = &groups[0].entries[0].sources[0].frontmatter;
        assert!(
            fm.get("path").is_none(),
            "path is not a frontmatter field: {fm:?}"
        );
    }

    // ─── min_count threshold ───────────────────────────────────────────
    //
    // `min_count` is the filter authors use to surface only repeated
    // keys (promotion candidates, repeated research topics). Pin
    // the boundary cases: default keeps every entry, threshold > 1
    // drops below-threshold entries, fully-emptied groups vanish.

    #[test]
    fn min_count_one_keeps_every_entry() {
        let g = graph(
            vec![node("a"), node("b")],
            vec![
                ann("a", "promotes", "k", 1),
                ann("b", "promotes", "k", 1),
                ann("a", "promotes", "single", 5),
            ],
        );
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &[],
                min_count: 1,
            },
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].entries.len(), 2);
    }

    #[test]
    fn min_count_two_drops_singletons() {
        let g = graph(
            vec![node("a"), node("b")],
            vec![
                ann("a", "promotes", "shared", 1),
                ann("b", "promotes", "shared", 2),
                ann("a", "promotes", "alone", 9),
            ],
        );
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &[],
                min_count: 2,
            },
        );
        // The repeated key survives; the singleton drops out.
        assert_eq!(groups.len(), 1);
        let keys: Vec<&str> = groups[0].entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["shared"]);
    }

    #[test]
    fn min_count_empties_whole_group() {
        // The threshold removes every entry → the group itself
        // disappears rather than surfacing as a hollow shell.
        let g = graph(
            vec![node("a"), node("b")],
            vec![
                ann("a", "promotes", "alone-1", 1),
                ann("b", "promotes", "alone-2", 1),
            ],
        );
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                name: None,
                with_frontmatter: &[],
                min_count: 2,
            },
        );
        assert!(groups.is_empty(), "empty groups must drop out: {groups:?}");
    }

    #[test]
    fn frontmatter_built_in_status_kind_id_title_always_present() {
        // Built-in scalars that are never None must surface verbatim.
        let g = graph(vec![node("a")], vec![ann("a", "promotes", "x", 1)]);
        let groups = find_annotations(
            &g,
            &AnnotationOptions {
                with_frontmatter: &[
                    "id".to_string(),
                    "title".to_string(),
                    "kind".to_string(),
                    "status".to_string(),
                ],
                ..Default::default()
            },
        );
        let fm = &groups[0].entries[0].sources[0].frontmatter;
        assert_eq!(fm.get("id").and_then(|v| v.as_str()), Some("a"));
        assert_eq!(fm.get("title").and_then(|v| v.as_str()), Some("a"));
        assert_eq!(fm.get("kind").and_then(|v| v.as_str()), Some("learning"));
        assert_eq!(fm.get("status").and_then(|v| v.as_str()), Some("active"));
    }
}
