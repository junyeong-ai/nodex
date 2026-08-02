use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::{Edge, RawEdge, ResolvedTarget, UnresolvedCause};

/// What one reading of the project offers a reference to bind to: the
/// paths documents stand at and the ids they carry, which is everything
/// the ladder below consults and nothing else.
///
/// A reading, not the project — the same reference read against the
/// project as it stands and against the project a mutation would produce
/// is how a seam learns that the mutation moved it.
#[derive(Debug, Default, Clone)]
pub struct Bindings {
    path_index: BTreeMap<String, String>,
    id_set: BTreeMap<String, ()>,
}

impl Bindings {
    /// The documents a reading offers, each as the path it stands at
    /// paired with the id it carries.
    pub(crate) fn of<'a>(documents: impl IntoIterator<Item = (&'a Path, &'a str)>) -> Self {
        let mut path_index = BTreeMap::new();
        let mut id_set = BTreeMap::new();
        for (path, id) in documents {
            path_index.insert(crate::path_guard::forward_string(path), id.to_string());
            id_set.insert(id.to_string(), ());
        }
        Self { path_index, id_set }
    }

    /// The document standing at `path`, if this reading holds one.
    pub(crate) fn id_at(&self, path: &str) -> Option<&str> {
        self.path_index.get(path).map(String::as_str)
    }

    /// The bindings a built graph carries.
    ///
    /// See [`Worlds`] for the pair a mutation reads against.
    pub fn of_graph(graph: &crate::model::Graph) -> Self {
        Self::of(
            graph
                .nodes()
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        )
    }
}

/// Resolve raw edges (path-based targets) into edges with resolved node ids.
pub fn resolve_edges(
    source: &str,
    raw_edges: Vec<RawEdge>,
    source_path: &Path,
    bindings: &Bindings,
    extensions: &[String],
) -> Vec<Edge> {
    let source_dir = source_path.parent().unwrap_or_else(|| Path::new(""));
    raw_edges
        .into_iter()
        .map(|raw| {
            let target = resolve_target(
                &raw.target_path,
                &raw.relation,
                source_dir,
                bindings,
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

/// What `target`, read from `source_path`, names by *path* — the ladder's
/// first two rungs, literal then source-relative.
///
/// Split out because the build is not the only caller that needs it and
/// the others stop short of the id rung below it: retargeting an id must
/// leave a reference the build binds to a file alone, which is the
/// question "would the ladder have fallen through to the bare-id step",
/// and a move re-spells a reference by path, which the id rung has no
/// answer for. Answered twice it drifts — a second reading against the
/// scanned scope rather than the graph's own paths, or one that lets an
/// absolute or root-escaping frame reach a rung the resolver never gives
/// it.
pub(crate) enum PathBinding {
    /// A document, by one of the two frames.
    Bound(Binding),
    /// No frame of it is a document — and the id rung is next.
    Unbound,
    /// Root-anchored, which means nothing inside a project-relative
    /// graph.
    Absolute,
    /// The source-relative frame leaves the project root.
    Escapes,
}

/// Which document a path-framed reference names, and how it named it.
///
/// The id alone answers the build, which only ever asks what an edge
/// points at. A rewrite has to re-spell the reference, so it needs the
/// two facts that decide a spelling: which path the ladder matched — the
/// document may be about to stand somewhere else — and which frame read
/// it, because a root-relative reference is re-rendered root-relative and
/// a source-relative one from wherever the referring file now sits.
pub(crate) struct Binding {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) frame: Frame,
}

/// The two path frames of the ladder, in probe order.
#[derive(PartialEq, Eq)]
pub(crate) enum Frame {
    /// Read from the project root — the spelling a move never changes.
    Root,
    /// Read from the referring file's own directory.
    Relative,
}

pub(crate) fn path_binding(
    target: &str,
    source_dir: &Path,
    bindings: &Bindings,
    extensions: &[String],
    document_ref: bool,
) -> PathBinding {
    let Some((here, normalized)) = frame_and_path(target) else {
        return PathBinding::Absolute;
    };
    let normalized = normalized.as_str();

    // 1. Literal (root-relative) path, then with each configured extension
    //    appended so a bare `[[guides/intro]]` finds `guides/intro.md`.
    //    `[text](path.md)` already carries its extension and matches here.
    //    Skipped for a reference that opens `./`, which says which frame
    //    it is in and is read that way by everything else that follows it.
    if !here
        && let Some((path, id)) =
            match_path(normalized, &bindings.path_index, extensions, document_ref)
    {
        return PathBinding::Bound(Binding {
            id,
            path,
            frame: Frame::Root,
        });
    }

    // 2. Same candidate, resolved relative to the source file's directory.
    match crate::path_guard::normalize_relative(&source_dir.join(normalized)) {
        Some(rel) => {
            if let Some((path, id)) =
                match_path(&rel, &bindings.path_index, extensions, document_ref)
            {
                return PathBinding::Bound(Binding {
                    id,
                    path,
                    frame: Frame::Relative,
                });
            }
        }
        // More `..` than directories to consume — the path escapes the
        // project root. Surfaced (never silently dropped) so a crafted
        // link can't match an unrelated in-scope node.
        None => return PathBinding::Escapes,
    }
    PathBinding::Unbound
}

pub(crate) fn resolve_target(
    target: &str,
    relation: &str,
    source_dir: &Path,
    bindings: &Bindings,
    extensions: &[String],
) -> ResolvedTarget {
    // Frontmatter id relations resolve strictly by node id — no path
    // lookup, no extension append. The dispatch consumes the closed
    // `ID_RESOLVED_RELATIONS` vocabulary: `Config::validate` rejects a
    // link pattern naming any of these relations (exactly as it keeps
    // `covers` off patterns), so each is producible only by its
    // frontmatter field and this branch can never capture a
    // user-declared body pattern — closed by construction, never a
    // guess about a user-chosen name.
    if crate::model::edge::ID_RESOLVED_RELATIONS.contains(&relation) {
        if bindings.id_set.contains_key(target) {
            return ResolvedTarget::resolved(target);
        }
        return ResolvedTarget::unresolved(target, UnresolvedCause::IdNotFound);
    }

    // `covers` names out-of-graph code paths by design — resolve it strictly
    // by path. Extension-append and id-fallback are reserved for in-graph
    // document references (body links: markdown links, `[[wikilinks]]`, and
    // `[[parser.link_patterns]]`); binding a covered code path to a
    // coincidentally-named node id would corrupt the drift signal. The
    // path-only relation is reachable only through the frontmatter
    // `covers:` field — `Config::validate` keeps it off link patterns —
    // so this dispatch is over a closed, code-owned vocabulary.
    let document_ref = crate::model::edge::is_document_ref_relation(relation);

    match path_binding(target, source_dir, bindings, extensions, document_ref) {
        PathBinding::Bound(bound) => return ResolvedTarget::resolved(&bound.id),
        PathBinding::Absolute => {
            return ResolvedTarget::unresolved(target, UnresolvedCause::Absolute);
        }
        PathBinding::Escapes => {
            return ResolvedTarget::unresolved(target, UnresolvedCause::EscapesSource);
        }
        PathBinding::Unbound => {}
    }

    // 3. Obsidian-style bare node-id reference (`[[adr-001]]`). Tried last so
    //    an in-scope file always wins over a same-named id.
    if document_ref && bindings.id_set.contains_key(target) {
        return ResolvedTarget::resolved(target);
    }

    // `Missing` is the build-time base classification for a path the
    // index does not contain — the resolver never stats the disk. The
    // unresolved-edge classifier refines it (`target_unparsed` /
    // `excluded_from_scope`) through its probes at query time.
    ResolvedTarget::unresolved(target, UnresolvedCause::Missing)
}

/// Look up `base` in the path index, then — for document references only —
/// `base` with each configured extension appended. Returns the matched
/// path and the node id standing on it. The extension pass lets
/// extension-less references (`[[guides/intro]]`) resolve to
/// `guides/intro.md` without the author spelling out the suffix.
fn match_path(
    base: &str,
    path_index: &BTreeMap<String, String>,
    extensions: &[String],
    document_ref: bool,
) -> Option<(String, String)> {
    reference_path_candidates(base, extensions, document_ref)
        .into_iter()
        .find_map(|candidate| {
            let id = path_index.get(&candidate)?;
            Some((candidate, id.clone()))
        })
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

/// A reference's path as its segments actually name it, and whether it
/// said which frame it is in — or `None` where it is root-anchored, the
/// one shape no rung of the ladder will take.
///
/// Every reader of a reference's path begins here, so each of them means
/// the same thing by one. A root-anchored path inside a project-relative
/// graph is meaningless; keeping it would let `[link](/etc/passwd.md)`
/// accidentally hit a node with the literal path `/etc/passwd.md` if one
/// ever existed. `Path::has_root` (not `is_absolute`) is the
/// cross-platform predicate — on Windows the latter only returns true for
/// drive-letter or verbatim forms, missing drive-relative `/etc/passwd` /
/// `\etc\passwd`. It is asked of the path exactly as written, before
/// anything is taken off it, because a leading empty segment is what says
/// root and nothing else does.
///
/// `./x` says the frame out loud: CommonMark, every filesystem, and every
/// editor that follows the link read it from the directory the document
/// is in, and never from the project root. So the marker is not noise to
/// be normalised away — it is the reference telling the resolver which
/// rung it belongs on, and the graph binding it anywhere else describes a
/// link nobody else follows there.
///
/// Everything else a segment list can carry that names nothing *is*
/// noise, and is dropped so the ladder never sees it: a repeated
/// separator and a `.` segment are the same path to POSIX, to Windows,
/// and to every reader that resolves a link, with no lookup involved.
/// Left standing they are read as something else entirely — `.//x.md`
/// with one `./` taken off is a spelling the root check would call
/// absolute, and `docs//x.md` misses an index key it is the same path as.
///
/// `..` is not noise and stays: it is an operation on what precedes it,
/// and *where* it is resolved decides which frame a reference binds in —
/// a question the ladder below owns and this must not answer early.
fn frame_and_path(raw: &str) -> Option<(bool, String)> {
    let forward = crate::path_guard::forward_str(raw);
    if Path::new(&forward).has_root() {
        return None;
    }
    let noise = |segment: &str| matches!(segment, "." | "");
    if !forward.split('/').any(noise) {
        return Some((false, forward));
    }
    let here = forward.split('/').next() == Some(".");
    let named: Vec<&str> = forward
        .split('/')
        .filter(|segment| !noise(segment))
        .collect();
    Some((here, named.join("/")))
}

/// The *normalized root-relative* paths a reference `raw`, written from
/// `source_path`, could resolve to — every [`reference_path_candidates`]
/// ladder entry under both the root-relative and the source-relative
/// interpretation, deduped, probe order preserved. Mirrors
/// [`resolve_target`] exactly, so "what could this link mean" has
/// exactly one definition, shared by the resolver, the unresolved-edge
/// cause classifier's disk probe, and the
/// `[[detection.unresolved_policy]]` glob matcher:
///
/// - pre-processing matches (forward-slash fold, `./` strip,
///   root-anchored refusal);
/// - the root-relative interpretation is the resolver's *literal*
///   index lookup — the ladder runs over the target exactly as
///   written, and an entry yields a candidate only when it is already
///   in normalized form (index keys are normalized scan paths, so
///   nothing else can bind);
/// - the source-relative interpretation joins the target onto the
///   source directory and collapses dot segments through
///   [`crate::path_guard::normalize_relative`] *first* — escaping
///   joins dropped, the resolver refuses them — and the extension
///   ladder then runs over the normalized result, exactly as the
///   resolver's second probe does. Appending before normalizing would
///   invent candidates (`docs/a/...md` for a dot-trailing `a/..`) the
///   resolver never tries.
pub(crate) fn normalized_resolution_candidates(
    raw: &str,
    source_path: Option<&Path>,
    extensions: &[String],
    document_ref: bool,
) -> Vec<String> {
    let Some((here, normalized)) = frame_and_path(raw) else {
        return Vec::new();
    };
    let normalized = normalized.as_str();
    let mut candidates: Vec<String> = Vec::new();
    let mut push = |candidate: String| {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    };
    // Root-relative interpretation: literal, like the resolver's
    // direct index match — admitted only when already normalized, and
    // not at all for a reference that says which frame it is in.
    if !here {
        for base in reference_path_candidates(normalized, extensions, document_ref) {
            if crate::path_guard::normalize_relative(Path::new(&base)).as_deref()
                == Some(base.as_str())
            {
                push(base);
            }
        }
    }
    // Source-relative interpretation (the resolver's second probe):
    // normalize the join, then ladder over the normalized form.
    if let Some(parent) = source_path.and_then(Path::parent)
        && let Some(rel) = crate::path_guard::normalize_relative(&parent.join(normalized))
    {
        for candidate in reference_path_candidates(&rel, extensions, document_ref) {
            push(candidate);
        }
    }
    candidates
}

/// The first normalized resolution candidate that exists under `root`
/// — as a regular file, or (when `admit_dirs` is set, the path-only
/// relation's contract) as a directory. The candidates are exactly the
/// in-root set [`normalized_resolution_candidates`] yields — escaping
/// and absolute interpretations are dropped at candidate generation —
/// so no *lexical* interpretation can name a path outside the project
/// root (the final-component read follows symlinks, matching the
/// scanner's own read semantics — symlink containment is not this
/// probe's charge). One disk probe for every consumer of the ladder:
/// the unresolved-cause classifier (`query::issues`) and the git-drift
/// target probes (`rules::git_drift`, `query::trust`) all answer "does
/// this link name real on-disk content" through this single
/// definition; only path-only (`covers`) callers admit directories,
/// since git measures a directory's history as readily as a file's,
/// while a document reference must stay file-only.
pub(crate) fn first_candidate_on_disk(
    candidates: &[String],
    files: crate::builder::scanner::ProjectFiles<'_>,
    admit_dirs: bool,
) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|c| files.holds(Path::new(c), admit_dirs))
        .map(PathBuf::from)
}

/// Whether on-disk content exists at exactly `rel` (a normalised
/// root-relative path) under `root`, matching each path component
/// **case-sensitively**. The final component must resolve to a file —
/// or, with `admit_dirs`, a file or directory (symlinks are followed
/// there, consistent with the scanner).
///
/// `Path::is_file` follows the filesystem's case-folding, so on a
/// case-insensitive volume (APFS, Windows) a broken link whose spelling
/// differs only in letter case from a real file would be misread as
/// "exists on disk" — mislabelled `ExcludedFromScope` by the cause
/// classifier, or measured for drift against a file the build never
/// bound. The build's path index is case-sensitive, so the disk probe
/// must be too: walk from `root`, and at each level require a directory
/// entry whose name matches the component exactly.
pub(crate) fn exists_case_sensitive(root: &Path, rel: &Path, admit_dirs: bool) -> bool {
    use std::path::Component;
    let mut current = root.to_path_buf();
    let mut components = rel.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return false; // `rel` is normalised: only Normal components occur
        };
        let Ok(entries) = std::fs::read_dir(&current) else {
            return false;
        };
        if !entries.flatten().any(|e| e.file_name() == name) {
            return false;
        }
        current.push(name);
        if components.peek().is_none() {
            return current.is_file() || (admit_dirs && current.is_dir());
        }
    }
    false
}

/// Whether a reference `raw`, written from `source_dir`, resolves to
/// `target_path` (project-root-relative, forward-slashed) under the same
/// candidate ladder the resolver uses — literal first, then resolved
/// relative to the source directory. Shared with `impact`'s dangling-
/// reference detection so it agrees with the build on what a link points to.
pub(crate) fn reference_resolves_to(
    raw: &str,
    source_dir: &Path,
    target_path: &str,
    extensions: &[String],
    document_ref: bool,
) -> bool {
    let Some((here, normalized)) = frame_and_path(raw) else {
        return false;
    };
    let normalized = normalized.as_str();
    let matches = |base: &str| {
        reference_path_candidates(base, extensions, document_ref)
            .iter()
            .any(|candidate| candidate == target_path)
    };
    if !here && matches(normalized) {
        return true;
    }
    crate::path_guard::normalize_relative(&source_dir.join(normalized))
        .is_some_and(|rel| matches(&rel))
}

/// Build a path → node_id index from parsed nodes.
/// Build a set of known node ids for direct id-based resolution.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Node;
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
                content_hash: String::new(),
                parse_issues: vec![],
                inferred_fields: vec![],
            },
        )
    }

    #[test]
    fn a_segment_naming_nothing_is_the_path_without_it() {
        // Every reader of a link collapses a repeated separator and a `.`
        // segment before it looks — POSIX, Windows, CommonMark, the
        // editor that opens it. Left standing, one `./` taken off
        // `.//x.md` leaves a spelling the root check calls absolute, and
        // `docs//x.md` misses the index key it is the same path as.
        let nodes = [make_node("x", "docs/x.md")];
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );
        let extensions = [".md".to_string()];
        for spelling in ["./x.md", ".//x.md", ".///x.md", "././x.md", "./sub/../x.md"] {
            let bound = path_binding(spelling, Path::new("docs"), &bindings, &extensions, true);
            assert!(
                matches!(&bound, PathBinding::Bound(binding)
                    if binding.id == "x" && binding.frame == Frame::Relative),
                "{spelling} names docs/x.md from docs/, in the frame it says out loud"
            );
        }
        for spelling in ["docs/x.md", "docs//x.md", "docs/./x.md"] {
            let bound = path_binding(spelling, Path::new(""), &bindings, &extensions, true);
            assert!(
                matches!(&bound, PathBinding::Bound(binding)
                    if binding.id == "x" && binding.frame == Frame::Root),
                "{spelling} names docs/x.md from the root"
            );
        }
        // A leading empty segment is not noise: it is what says root.
        assert!(matches!(
            path_binding("/docs/x.md", Path::new(""), &bindings, &extensions, true),
            PathBinding::Absolute
        ));
    }

    #[test]
    fn resolve_direct_path() {
        let nodes = [make_node("guide-auth", "docs/guides/auth.md")];
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "docs/guides/auth.md".to_string(),
                relation: "references".to_string(),
                location: "L5".to_string(),
            }],
            Path::new("docs/decisions/0001-auth.md"),
            &bindings,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-auth"));
    }

    #[test]
    fn resolve_relative_path() {
        let nodes = [make_node("guide-auth", "docs/guides/auth.md")];
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );

        let edges = resolve_edges(
            "guide-index",
            vec![RawEdge {
                target_path: "auth.md".to_string(),
                relation: "references".to_string(),
                location: "L3".to_string(),
            }],
            Path::new("docs/guides/index.md"),
            &bindings,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-auth"));
    }

    #[test]
    fn resolve_frontmatter_relation_by_id() {
        let nodes = [
            make_node("adr-001", "docs/decisions/0001.md"),
            make_node("adr-002", "docs/decisions/0002.md"),
        ];
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );

        let edges = resolve_edges(
            "adr-002",
            vec![RawEdge {
                target_path: "adr-001".to_string(),
                relation: "supersedes".to_string(),
                location: "frontmatter:supersedes".to_string(),
            }],
            Path::new("docs/decisions/0002.md"),
            &bindings,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("adr-001"));
    }

    #[test]
    fn unresolved_target() {
        let nodes: Vec<(String, Node)> = vec![];
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );

        let edges = resolve_edges(
            "test",
            vec![RawEdge {
                target_path: "nonexistent.md".to_string(),
                relation: "references".to_string(),
                location: "L1".to_string(),
            }],
            Path::new("test.md"),
            &bindings,
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
        let nodes = [make_node("guide-setup", "docs/guides/setup.md")];
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "../guides/setup.md".to_string(),
                relation: "references".to_string(),
                location: "L5".to_string(),
            }],
            Path::new("docs/decisions/0001-auth.md"),
            &bindings,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.id(), Some("guide-setup"));
    }

    #[test]
    fn underflow_link_is_unresolved_with_reason() {
        let nodes = [make_node("guide-setup", "docs/guides/setup.md")];
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );

        let edges = resolve_edges(
            "adr-001",
            vec![RawEdge {
                target_path: "../../../../escape.md".to_string(),
                relation: "references".to_string(),
                location: "L1".to_string(),
            }],
            Path::new("docs/decisions/0001.md"),
            &bindings,
            &[".md".to_string()],
        );

        assert_eq!(edges.len(), 1);
        match &edges[0].target {
            crate::model::ResolvedTarget::Unresolved { cause, .. } => {
                assert_eq!(*cause, UnresolvedCause::EscapesSource);
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }
    }

    #[test]
    fn absolute_link_is_unresolved() {
        let nodes: Vec<(String, Node)> = vec![];
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );

        let edges = resolve_edges(
            "x",
            vec![RawEdge {
                target_path: "/etc/passwd.md".to_string(),
                relation: "references".to_string(),
                location: "L1".to_string(),
            }],
            Path::new("docs/x.md"),
            &bindings,
            &[".md".to_string()],
        );
        assert_eq!(edges.len(), 1);
        match &edges[0].target {
            crate::model::ResolvedTarget::Unresolved { cause, .. } => {
                assert_eq!(*cause, UnresolvedCause::Absolute);
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
        let bindings = Bindings::of(
            nodes
                .iter()
                .map(|(id, node)| (node.path.as_path(), id.as_str())),
        );
        let edges = resolve_edges(
            "src",
            vec![RawEdge {
                target_path: target.to_string(),
                relation: relation.to_string(),
                location: "L1".to_string(),
            }],
            Path::new(source),
            &bindings,
            &[".md".to_string()],
        );
        edges.into_iter().next().unwrap().target
    }

    #[test]
    fn wikilink_resolves_via_extension_append() {
        // `[[docs/guides/auth]]` — no extension — finds `docs/guides/auth.md`
        // by appending a configured extension. This is the Obsidian-style
        // ergonomic the parser advertises.
        let nodes = [make_node("guide-auth", "docs/guides/auth.md")];
        let t = resolve_one("docs/guides/auth", "references", "docs/index.md", &nodes);
        assert_eq!(t.id(), Some("guide-auth"));
    }

    #[test]
    fn wikilink_resolves_relative_stem_via_extension() {
        // `[[auth]]` from `docs/guides/index.md` → `docs/guides/auth.md`
        // (relative to the source dir, with the extension appended).
        let nodes = [make_node("guide-auth", "docs/guides/auth.md")];
        let t = resolve_one("auth", "references", "docs/guides/index.md", &nodes);
        assert_eq!(t.id(), Some("guide-auth"));
    }

    #[test]
    fn wikilink_resolves_bare_node_id() {
        // `[[adr-0002]]` — a bare node id, not a path — resolves through the
        // id fallback so authors can cite a document by its id.
        let nodes = [make_node("adr-0002", "docs/decisions/0002.md")];
        let t = resolve_one("adr-0002", "references", "docs/decisions/0001.md", &nodes);
        assert_eq!(t.id(), Some("adr-0002"));
    }

    #[test]
    fn custom_link_pattern_resolves_bare_node_id() {
        // A `[[parser.link_patterns]]` relation is a document reference too,
        // so `@cite(spec-login)` resolves to the node id `spec-login`.
        let nodes = [make_node("spec-login", "docs/specs/login.md")];
        let t = resolve_one("spec-login", "cites", "docs/decisions/0001.md", &nodes);
        assert_eq!(t.id(), Some("spec-login"));
    }

    #[test]
    fn path_match_wins_over_id_match() {
        // A bare `[[shared]]` matches both a file (`shared.md`, via the
        // extension pass) and a node whose id is `shared`. The file must
        // win — the id fallback is tried last. Uses an extension-less
        // target so the id-collision path is actually exercised.
        let nodes = [
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
        let nodes = [make_node("double", "docs/spec.md.md")];
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
        let nodes = [make_node("src-auth", "src/auth.rs")];
        let t = resolve_one("src-auth", "covers", "docs/decisions/0001.md", &nodes);
        assert!(
            matches!(t, ResolvedTarget::Unresolved { .. }),
            "covers must not id-fallback; got {t:?}"
        );
    }

    #[test]
    fn dot_segment_target_normalizes_source_relative_only() {
        // The root-relative interpretation is the resolver's *literal*
        // index lookup: a base carrying dot segments can never bind
        // there, so `a/../docs/x.md` written from `deep/d.md` means
        // exactly `deep/docs/x.md` (the source-relative collapse) and
        // never the root-relative collapse `docs/x.md` — normalizing
        // root-relatively would invent a resolution the build never
        // performs.
        let candidates = normalized_resolution_candidates(
            "a/../docs/x.md",
            Some(Path::new("deep/d.md")),
            &[".md".to_string()],
            true,
        );
        assert!(
            candidates.contains(&"deep/docs/x.md".to_string()),
            "source-relative collapse expected: {candidates:?}"
        );
        assert!(
            !candidates.contains(&"docs/x.md".to_string()),
            "the root-relative interpretation is literal-only: {candidates:?}"
        );
    }

    #[test]
    fn dot_trailing_target_ladders_over_the_normalized_join() {
        // `a/..` written from `docs/d.md` means `docs` under the
        // source-relative interpretation; the extension ladder runs
        // over that *normalized* form — `docs`, then `docs.md` —
        // exactly as the resolver's second probe does. Appending the
        // extension to the raw join would invent `docs/a/...md`, a
        // path the resolver never tries.
        let candidates = normalized_resolution_candidates(
            "a/..",
            Some(Path::new("docs/d.md")),
            &[".md".to_string()],
            true,
        );
        let docs = candidates.iter().position(|c| c == "docs");
        let docs_md = candidates.iter().position(|c| c == "docs.md");
        assert!(
            docs.is_some() && docs_md.is_some(),
            "normalized join and its extension append expected: {candidates:?}"
        );
        assert!(
            docs < docs_md,
            "resolver order: literal before extension append: {candidates:?}"
        );
        assert!(
            !candidates.contains(&"docs/a/...md".to_string()),
            "the ladder never runs over the un-normalized join: {candidates:?}"
        );
    }

    #[test]
    fn extensionless_relative_target_pins_inter_group_ladder_order() {
        // The candidate list is two strictly ordered groups — the
        // entire root-relative group before the source-relative group
        // — and each group ladders target-then-extensions, mirroring
        // the resolver's probe order exactly. The full ordered list is
        // pinned because order decides which node a link binds to when
        // several candidates exist in the index.
        let candidates = normalized_resolution_candidates(
            "auth",
            Some(Path::new("docs/guides/index.md")),
            &[".md".to_string()],
            true,
        );
        assert_eq!(
            candidates,
            vec![
                "auth".to_string(),
                "auth.md".to_string(),
                "docs/guides/auth".to_string(),
                "docs/guides/auth.md".to_string(),
            ],
            "root-relative group entirely first, each group laddering \
             target then extensions"
        );
    }

    #[test]
    fn covers_does_not_append_extension() {
        // Extension-append is for document references only; a covered path
        // must match verbatim or stay unresolved.
        let nodes = [make_node("guide", "docs/guide.md")];
        let t = resolve_one("docs/guide", "covers", "x.md", &nodes);
        assert!(matches!(t, ResolvedTarget::Unresolved { .. }));
    }
}

/// The project a rewrite reads references against: as it stands, and as
/// the mutation it is part of would leave it.
///
/// A pair, because every question the seam asks is really two — what a
/// reference named, and what it names once the mutation lands — and the
/// two are only ever meaningful together.
#[derive(Debug, Clone, Copy)]
pub struct Worlds<'a> {
    pub before: &'a Bindings,
    pub after: &'a Bindings,
}
