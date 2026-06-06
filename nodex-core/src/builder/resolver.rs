use std::collections::BTreeMap;
use std::path::Path;

use crate::model::Node;
use crate::model::{Edge, RawEdge, ResolvedTarget};

/// Resolve raw edges (path-based targets) into edges with resolved node ids.
pub fn resolve_edges(
    source: &str,
    raw_edges: Vec<RawEdge>,
    source_path: &Path,
    path_index: &BTreeMap<String, String>,
    id_set: &BTreeMap<String, ()>,
    extensions: &[String],
) -> Vec<Edge> {
    raw_edges
        .into_iter()
        .map(|raw| {
            let target = resolve_target(
                &raw.target_path,
                &raw.relation,
                source_path,
                path_index,
                id_set,
                extensions,
            );
            Edge {
                source: source.to_string(),
                target,
                relation: raw.relation,
                location: raw.location,
            }
        })
        .collect()
}

fn resolve_target(
    target: &str,
    relation: &str,
    source_path: &Path,
    path_index: &BTreeMap<String, String>,
    id_set: &BTreeMap<String, ()>,
    extensions: &[String],
) -> ResolvedTarget {
    // Frontmatter relations (supersedes, implements, related) use node ids directly
    match relation {
        "supersedes" | "implements" | "related" => {
            if id_set.contains_key(target) {
                return ResolvedTarget::resolved(target);
            }
            return ResolvedTarget::unresolved(target, "node id not found in graph");
        }
        _ => {}
    }

    let normalized = crate::path_guard::forward_str(target);
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);

    // A root-anchored path inside a project-relative graph is
    // meaningless; keeping it would let `[link](/etc/passwd.md)`
    // accidentally hit a node with the literal path "/etc/passwd.md"
    // if one ever existed. `Path::has_root` (not `is_absolute`) is
    // the cross-platform predicate — on Windows the latter only
    // returns true for drive-letter or verbatim forms, missing
    // drive-relative `/etc/passwd` / `\etc\passwd`.
    if Path::new(normalized).has_root() {
        return ResolvedTarget::unresolved(target, "absolute paths are not in scope");
    }

    // `covers` names out-of-graph code paths by design — resolve it strictly
    // by path. Extension-append and id-fallback are reserved for in-graph
    // document references (body links: markdown links, `[[wikilinks]]`, and
    // `[[parser.link_patterns]]`); binding a covered code path to a
    // coincidentally-named node id would corrupt the drift signal.
    let document_ref = relation != "covers";

    // 1. Literal (root-relative) path, then with each configured extension
    //    appended so a bare `[[guides/intro]]` finds `guides/intro.md`.
    //    `[text](path.md)` already carries its extension and matches here.
    if let Some(id) = match_path(normalized, path_index, extensions, document_ref) {
        return ResolvedTarget::resolved(&id);
    }

    // 2. Same candidate, resolved relative to the source file's directory.
    if let Some(parent) = source_path.parent() {
        match crate::path_guard::normalize_relative(&parent.join(normalized)) {
            Some(rel) => {
                if let Some(id) = match_path(&rel, path_index, extensions, document_ref) {
                    return ResolvedTarget::resolved(&id);
                }
            }
            // More `..` than directories to consume — the path escapes
            // the project root. Surfaced (never silently dropped) so a
            // crafted link can't match an unrelated in-scope node.
            None => {
                return ResolvedTarget::unresolved(target, "path escapes source scope");
            }
        }
    }

    // 3. Obsidian-style bare node-id reference (`[[adr-001]]`). Tried last so
    //    an in-scope file always wins over a same-named id.
    if document_ref && id_set.contains_key(target) {
        return ResolvedTarget::resolved(target);
    }

    ResolvedTarget::unresolved(target, "path not found in scope")
}

/// Look up `base` in the path index, then — for document references only —
/// `base` with each configured extension appended. Returns the matched node
/// id. The extension pass lets extension-less references (`[[guides/intro]]`)
/// resolve to `guides/intro.md` without the author spelling out the suffix.
fn match_path(
    base: &str,
    path_index: &BTreeMap<String, String>,
    extensions: &[String],
    document_ref: bool,
) -> Option<String> {
    reference_path_candidates(base, extensions, document_ref)
        .iter()
        .find_map(|candidate| path_index.get(candidate).cloned())
}

/// The path strings a reference target expands to, most specific first:
/// the target itself, plus — for a document reference (`document_ref`)
/// that doesn't already carry a configured extension — the target with
/// each extension appended. A plain `covers` path (`document_ref = false`)
/// is taken verbatim.
///
/// This is the single source of truth for "what could this body link
/// point to", shared by the build-time resolver ([`match_path`]) and the
/// query-time unresolved-edge classifier
/// ([`crate::query::issues`]'s disk probe) so the two can never disagree.
pub(crate) fn reference_path_candidates(
    base: &str,
    extensions: &[String],
    document_ref: bool,
) -> Vec<String> {
    let mut candidates = vec![base.to_string()];
    // Append a configured extension only when the target doesn't already
    // carry one — `[[guides/intro]]` becomes `guides/intro.md`, but a
    // markdown link `[x](spec.md)` must not expand to `spec.md.md`.
    if document_ref && !extensions.iter().any(|ext| base.ends_with(ext.as_str())) {
        candidates.extend(extensions.iter().map(|ext| format!("{base}{ext}")));
    }
    candidates
}

/// Build a path → node_id index from parsed nodes.
pub fn build_path_index(nodes: &[(String, Node)]) -> BTreeMap<String, String> {
    let mut index = BTreeMap::new();
    for (id, node) in nodes {
        let path_str = crate::path_guard::forward_string(&node.path);
        index.insert(path_str, id.clone());
    }
    index
}

/// Build a set of known node ids for direct id-based resolution.
pub fn build_id_set(nodes: &[(String, Node)]) -> BTreeMap<String, ()> {
    nodes.iter().map(|(id, _)| (id.clone(), ())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, RawEdge, Status};
    use std::path::PathBuf;

    fn make_node(id: &str, path: &str) -> (String, Node) {
        (
            id.to_string(),
            Node {
                id: id.to_string(),
                path: PathBuf::from(path),
                title: "Test".to_string(),
                kind: Kind::new("generic"),
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
            },
        )
    }

    #[test]
    fn resolve_direct_path() {
        let nodes = vec![make_node("guide-auth", "docs/guides/auth.md")];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "docs/guides/auth.md".to_string(),
                relation: "references".to_string(),
                location: "L5".to_string(),
            }],
            Path::new("docs/decisions/0001-auth.md"),
            &path_index,
            &id_set,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-auth"));
    }

    #[test]
    fn resolve_relative_path() {
        let nodes = vec![make_node("guide-auth", "docs/guides/auth.md")];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "guide-index",
            vec![RawEdge {
                target_path: "auth.md".to_string(),
                relation: "references".to_string(),
                location: "L3".to_string(),
            }],
            Path::new("docs/guides/index.md"),
            &path_index,
            &id_set,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-auth"));
    }

    #[test]
    fn resolve_frontmatter_relation_by_id() {
        let nodes = vec![
            make_node("adr-001", "docs/decisions/0001.md"),
            make_node("adr-002", "docs/decisions/0002.md"),
        ];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "adr-002",
            vec![RawEdge {
                target_path: "adr-001".to_string(),
                relation: "supersedes".to_string(),
                location: "frontmatter:supersedes".to_string(),
            }],
            Path::new("docs/decisions/0002.md"),
            &path_index,
            &id_set,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("adr-001"));
    }

    #[test]
    fn unresolved_target() {
        let nodes: Vec<(String, Node)> = vec![];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "test",
            vec![RawEdge {
                target_path: "nonexistent.md".to_string(),
                relation: "references".to_string(),
                location: "L1".to_string(),
            }],
            Path::new("test.md"),
            &path_index,
            &id_set,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert!(matches!(
            edges[0].target,
            crate::model::ResolvedTarget::Unresolved { .. }
        ));
    }

    #[test]
    fn resolve_relative_path_with_dotdot() {
        let nodes = vec![make_node("guide-setup", "docs/guides/setup.md")];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "../guides/setup.md".to_string(),
                relation: "references".to_string(),
                location: "L5".to_string(),
            }],
            Path::new("docs/decisions/0001-auth.md"),
            &path_index,
            &id_set,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-setup"));
    }

    #[test]
    fn underflow_link_is_unresolved_with_reason() {
        let nodes = vec![make_node("guide-setup", "docs/guides/setup.md")];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "../../../../escape.md".to_string(),
                relation: "references".to_string(),
                location: "L1".to_string(),
            }],
            Path::new("docs/decisions/0001.md"),
            &path_index,
            &id_set,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        match &edges[0].target {
            crate::model::ResolvedTarget::Unresolved { reason, .. } => {
                assert!(reason.contains("escapes"), "reason was {reason:?}");
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn absolute_link_is_unresolved() {
        let nodes: Vec<(String, Node)> = vec![];
        let path_index = build_path_index(&nodes);
        let id_set = build_id_set(&nodes);

        let edges = resolve_edges(
            "x",
            vec![RawEdge {
                target_path: "/etc/passwd.md".to_string(),
                relation: "references".to_string(),
                location: "L1".to_string(),
            }],
            Path::new("docs/x.md"),
            &path_index,
            &id_set,
            &[".md".to_string()],
        );
        assert_eq!(edges.len(), 1);
        match &edges[0].target {
            crate::model::ResolvedTarget::Unresolved { reason, .. } => {
                assert!(reason.contains("absolute"), "reason was {reason:?}");
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    /// Helper: resolve a single body-link `target` against `nodes`.
    fn resolve_one(
        target: &str,
        relation: &str,
        source: &str,
        nodes: &[(String, Node)],
    ) -> ResolvedTarget {
        let path_index = build_path_index(nodes);
        let id_set = build_id_set(nodes);
        let edges = resolve_edges(
            "src",
            vec![RawEdge {
                target_path: target.to_string(),
                relation: relation.to_string(),
                location: "L1".to_string(),
            }],
            Path::new(source),
            &path_index,
            &id_set,
            &[".md".to_string()],
        );
        edges.into_iter().next().unwrap().target
    }

    #[test]
    fn wikilink_resolves_via_extension_append() {
        // `[[docs/guides/auth]]` — no extension — finds `docs/guides/auth.md`
        // by appending a configured extension. This is the Obsidian-style
        // ergonomic the parser advertises.
        let nodes = vec![make_node("guide-auth", "docs/guides/auth.md")];
        let t = resolve_one("docs/guides/auth", "references", "docs/index.md", &nodes);
        assert_eq!(t.id(), Some("guide-auth"));
    }

    #[test]
    fn wikilink_resolves_relative_stem_via_extension() {
        // `[[auth]]` from `docs/guides/index.md` → `docs/guides/auth.md`
        // (relative to the source dir, with the extension appended).
        let nodes = vec![make_node("guide-auth", "docs/guides/auth.md")];
        let t = resolve_one("auth", "references", "docs/guides/index.md", &nodes);
        assert_eq!(t.id(), Some("guide-auth"));
    }

    #[test]
    fn wikilink_resolves_bare_node_id() {
        // `[[adr-0002]]` — a bare node id, not a path — resolves through the
        // id fallback so authors can cite a document by its id.
        let nodes = vec![make_node("adr-0002", "docs/decisions/0002.md")];
        let t = resolve_one("adr-0002", "references", "docs/decisions/0001.md", &nodes);
        assert_eq!(t.id(), Some("adr-0002"));
    }

    #[test]
    fn custom_link_pattern_resolves_bare_node_id() {
        // A `[[parser.link_patterns]]` relation is a document reference too,
        // so `@cite(spec-login)` resolves to the node id `spec-login`.
        let nodes = vec![make_node("spec-login", "docs/specs/login.md")];
        let t = resolve_one("spec-login", "cites", "docs/decisions/0001.md", &nodes);
        assert_eq!(t.id(), Some("spec-login"));
    }

    #[test]
    fn path_match_wins_over_id_match() {
        // A bare `[[shared]]` matches both a file (`shared.md`, via the
        // extension pass) and a node whose id is `shared`. The file must
        // win — the id fallback is tried last. Uses an extension-less
        // target so the id-collision path is actually exercised.
        let nodes = vec![
            make_node("real-file", "shared.md"),
            make_node("shared", "docs/other.md"),
        ];
        let t = resolve_one("shared", "references", "index.md", &nodes);
        assert_eq!(
            t.id(),
            Some("real-file"),
            "an in-scope file (via extension append) outranks a same-named id"
        );
    }

    #[test]
    fn already_extensioned_target_does_not_double_append() {
        // `[x](docs/spec.md)` whose target is absent must NOT resolve to a
        // pathological `docs/spec.md.md` — extension-append is skipped when
        // the target already carries a configured extension.
        let nodes = vec![make_node("double", "docs/spec.md.md")];
        let t = resolve_one("docs/spec.md", "references", "index.md", &nodes);
        assert!(
            matches!(t, ResolvedTarget::Unresolved { .. }),
            "an already-extensioned target must not bind to a double-extension node; got {t:?}"
        );
    }

    #[test]
    fn covers_does_not_fall_back_to_id() {
        // `covers` names out-of-graph code paths. A covered path that happens
        // to equal a node id must stay Unresolved — never bind to the node.
        let nodes = vec![make_node("src-auth", "src/auth.rs")];
        let t = resolve_one("src-auth", "covers", "docs/decisions/0001.md", &nodes);
        assert!(
            matches!(t, ResolvedTarget::Unresolved { .. }),
            "covers must not id-fallback; got {t:?}"
        );
    }

    #[test]
    fn covers_does_not_append_extension() {
        // Extension-append is for document references only; a covered path
        // must match verbatim or stay unresolved.
        let nodes = vec![make_node("guide", "docs/guide.md")];
        let t = resolve_one("docs/guide", "covers", "x.md", &nodes);
        assert!(matches!(t, ResolvedTarget::Unresolved { .. }));
    }
}
