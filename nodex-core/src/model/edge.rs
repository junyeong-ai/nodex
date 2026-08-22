use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable category keys used in issue-report summaries
/// (`query issues` → `summary.by_category`).
///
/// Exposed as `const` so command-line consumers, the config validator
/// (the reserved-name guard on `[[detection.unresolved_policy]]` row
/// names), and tests reference the same identifiers; violations are
/// reported as `violation_<rule_id>`.
pub mod categories {
    pub const UNRESOLVED_EDGE: &str = "unresolved_edge";
    pub const VIOLATION_PREFIX: &str = "violation_";
    /// Name of the default `[[detection.unresolved_policy]]` row
    /// (`config::default_unresolved_policy`): links whose target exists
    /// on disk but sits outside scan scope (most commonly
    /// `[[scope.conditional_exclude]]`) report under this category at
    /// `info` severity — out of `summary.total`, because the reference
    /// points at a real, intentionally-ungraphed file. Unlike the keys
    /// above it is *not* reserved: a project that declares its own
    /// policy table re-declares this row to keep the behavior.
    pub const EXCLUDED_TARGET: &str = "excluded_target";
}

/// Why a reference target could not be resolved. Stable JSON surface so
/// external tooling can branch on the cause without string-matching
/// `reason` strings — and the typed vocabulary
/// `[[detection.unresolved_policy]]` rows declare their `cause` in.
///
/// The resolver records a cause on every [`ResolvedTarget::Unresolved`]
/// at the refusal site; the unresolved-edge classifier
/// (`query::issues`) refines the path-shaped [`Self::Missing`] into
/// [`Self::TargetUnparsed`] / [`Self::ExcludedFromScope`] through its
/// `Graph::parse_failures` and disk probes. [`Display`] renders each
/// cause's one human prose line — the single source every `reason`
/// string derives from.
///
/// [`Display`]: std::fmt::Display
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedCause {
    /// An id relation (`supersedes` / `superseded_by` / `implements` /
    /// `related`) whose value isn't a known node id.
    IdNotFound,
    /// A document reference no rung of the ladder bound — neither a path
    /// in scope nor a node id. The build records every unbound reference
    /// with this cause (it never stats the disk); a *reported* `Missing`
    /// has additionally survived the classifier's probes, so no file on
    /// disk under the project root answers it either.
    Missing,
    /// A reference whose file is in scope but failed to parse and so has
    /// no node — the path is recorded in
    /// [`crate::model::Graph::parse_failures`]. The reference is not
    /// excluded-by-design and not missing: fixing the target document
    /// resolves it.
    TargetUnparsed,
    /// A reference whose file exists on disk but isn't in the graph's
    /// scan scope — most commonly removed by
    /// `[[scope.conditional_exclude]]` on a terminal-status parent.
    ExcludedFromScope,
    /// A path that walks above the source file's directory via `..`
    /// segments. Refused as a security guard, never resolved.
    EscapesSource,
    /// A path written as an absolute path. Refused as out of project
    /// scope.
    Absolute,
}

impl UnresolvedCause {
    /// Whether an edge with this cause names something the resolution
    /// looked up — the set a `[[detection.unresolved_policy]]` row's
    /// `glob` matches against
    /// (`crate::builder::resolver::Sought`).
    ///
    /// A reference names an id or a path according to its relation, and
    /// either is a name a row may select on. What has no name is a
    /// spelling normalization refused before resolution began: an
    /// absolute or source-escaping path was never looked up, so a row
    /// for those causes is cause-only and `Config::validate` rejects a
    /// `glob` on them at load. The match is exhaustive by variant so
    /// adding a cause forces this decision at compile time.
    pub fn names_a_target(&self) -> bool {
        match self {
            Self::IdNotFound | Self::Missing | Self::TargetUnparsed | Self::ExcludedFromScope => {
                true
            }
            Self::EscapesSource | Self::Absolute => false,
        }
    }

    /// Whether an edge carrying `relation` can fail with this cause.
    ///
    /// Resolution mode decides it, and the two partitions line up exactly: an
    /// id relation is looked up in the id index and fails only as
    /// [`Self::IdNotFound`], nothing else ever reaches that arm, and every
    /// other relation walks the path ladder — whose refusals are the rest of
    /// the vocabulary. So a `[[detection.unresolved_policy]]` row pairing the
    /// two the other way selects a set no project can produce, which
    /// `Config::validate` refuses at load.
    pub fn reachable_for(&self, relation: &str) -> bool {
        ID_RESOLVED_RELATIONS.contains(&relation) == matches!(self, Self::IdNotFound)
    }
}

/// The one prose rendering per cause — every human-facing `reason`
/// (issue reports, violation messages) derives from here, so the typed
/// cause and its prose can never disagree.
///
/// Each line states what failed, never which plane it failed in: the
/// relation on the edge says whether the target was an id or a path, and
/// a cause claiming one of them describes a reference it does not always
/// hold — a bare-id body citation is bound through the same ladder as a
/// path and fails it as `Missing`.
impl std::fmt::Display for UnresolvedCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::IdNotFound => "node id not found in graph",
            Self::Missing => "nothing in scope resolves this target",
            Self::TargetUnparsed => "target is in scope but failed to parse",
            Self::ExcludedFromScope => "target exists on disk but is excluded from scope",
            Self::EscapesSource => "path escapes source scope",
            Self::Absolute => "absolute paths are not in scope",
        })
    }
}

/// Every relation an edge in the graph can carry without a user-declared
/// `[[parser.link_patterns]]` block — the closed, typed core vocabulary,
/// partitioned exactly by resolution mode into [`PATH_ONLY_RELATION`],
/// [`ID_RESOLVED_RELATIONS`] and [`BODY_REFERENCE_RELATION`].
///
/// Each is a code-backed graph operation, not merely a label, which is
/// why the set is fixed rather than config-declared:
/// - `supersedes` drives the build-time supersession DAG check
/// - `superseded_by` records the same succession from the predecessor
/// - `implements` is the default `rules.acyclic_relations` member
/// - `covers` points at out-of-graph code paths (drift detection)
/// - `related` is the soft, unconstrained cross-link
/// - `references` is the default body-link relation
///
/// What varies between projects is link *syntax*, not these semantics —
/// and that is precisely what `[[parser.link_patterns]]` opens up: body
/// link patterns declare *document references*, mapping any regex to any
/// relation name whose resolution mode is not fixed in code. Every
/// relation here but [`BODY_REFERENCE_RELATION`] has code-fixed
/// resolution and is producible only by its frontmatter field;
/// `Config::validate` rejects a link pattern naming one, because
/// resolution semantics attach to the field that produces a relation,
/// never to a name a user can pick. `references` stays legal on
/// patterns: it resolves in document-reference mode either way, so a
/// pattern naming it shifts no semantics.
pub(crate) const EDGE_RELATIONS: &[&str] = &[
    "references",
    "supersedes",
    "superseded_by",
    "implements",
    "related",
    "covers",
];

/// The relations a *resolved* edge can carry — `EDGE_RELATIONS` less
/// the ones that exist only where resolution failed.
///
/// This is the vocabulary of every surface that reads resolved edges: a
/// traversal (`query dependents`, `impact`), a cycle check
/// (`rules.acyclic_relations`), a drift measurement
/// (`detection.git_drift_relations`). `Config::known_relations` adds the
/// project's own link-pattern relations to it, so a future built-in is
/// acknowledged in one place.
///
/// Naming a relation here that no resolved edge can carry would let each
/// of those surfaces accept a filter that matches nothing and report a
/// clean run over it — the vacuous pass `RuleRun::subjects` exists to
/// expose, arriving through the config instead.
pub const BUILTIN_EDGE_RELATIONS: &[&str] = &[
    "references",
    "supersedes",
    "implements",
    "related",
    "covers",
];

/// The relation a `superseded_by:` scalar leaves on an edge when it names
/// no node.
///
/// A resolved one is materialised by the builder as a `supersedes` edge
/// in the canonical direction, so this relation exists only where
/// resolution failed: it is in [`EDGE_RELATIONS`], and deliberately not
/// in [`BUILTIN_EDGE_RELATIONS`]. `Config::unresolved_edge_relations` is
/// the vocabulary that admits it — the one surface that reads unresolved
/// edges, `[[detection.unresolved_policy]]`.
pub(crate) const SUPERSEDED_BY_RELATION: &str = "superseded_by";

/// The one relation resolved strictly by path: `covers` names
/// out-of-graph code paths, so extension-append and id-fallback would
/// corrupt its drift signal by binding a covered path to a
/// coincidentally-named node. Single source for the name, consumed by
/// [`is_document_ref_relation`] and the `Config::validate` guard that
/// keeps it off user-declared link patterns, so the boundary is never
/// spelled twice.
pub(crate) const PATH_ONLY_RELATION: &str = "covers";

/// The relations whose target is a node id — no path lookup, no
/// extension append, no id fallback. Like [`PATH_ONLY_RELATION`], their
/// resolution mode is fixed in code, so each is producible only by its
/// frontmatter field: `Config::validate` rejects a link pattern naming
/// any of them, which is what makes the resolver's id dispatch a closed,
/// code-owned vocabulary rather than a guess about a user-chosen name.
///
/// [`SUPERSEDED_BY_RELATION`] is one of them and never reaches
/// [`crate::builder::resolver::resolve_target`]: the builder materialises
/// the scalar directly into a canonical `supersedes` edge (known target)
/// or an unresolved `superseded_by` edge (unknown target). What the
/// membership decides for it is what it decides for the rest — that its
/// target is an id, so the ladder never runs over it and a
/// `[[detection.unresolved_policy]]` glob matches the id verbatim.
pub(crate) const ID_RESOLVED_RELATIONS: &[&str] = &[
    "supersedes",
    SUPERSEDED_BY_RELATION,
    "implements",
    "related",
];

/// The relation a body reference is resolved as when the asker has no
/// relation of its own to offer. Every reference the rewriter surfaces is
/// a body reference — the id-resolved and path-only relations are
/// producible only by their frontmatter fields, which it never reads —
/// and `references` resolves in document-reference mode exactly as all of
/// them do, so asking the ladder under this name shifts no semantics.
pub(crate) const BODY_REFERENCE_RELATION: &str = "references";

/// Whether `relation` is a *document reference* — resolved through the
/// full candidate ladder (literal/relative path, extension append,
/// bare id) — as opposed to the path-only [`PATH_ONLY_RELATION`].
/// Because `Config::validate` rejects a link pattern naming `covers`,
/// the path-only branch is reachable only through the frontmatter
/// `covers:` field: a typed dispatch on a closed, code-owned
/// vocabulary, never a guess about a user-chosen name.
pub(crate) fn is_document_ref_relation(relation: &str) -> bool {
    relation != PATH_ONLY_RELATION
}

/// Whether `relation` is the path-only relation (`covers`). A path-only
/// target names out-of-graph code — a file or a whole directory (git
/// measures either) — so the disk probes admit directories for it,
/// while document references stay file-only.
pub(crate) fn is_path_only_relation(relation: &str) -> bool {
    relation == PATH_ONLY_RELATION
}

/// A resolved edge in the graph.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Edge {
    pub source: String,
    pub target: ResolvedTarget,
    pub relation: String,
    /// Source location, e.g. "L42" or "frontmatter:supersedes".
    pub location: String,
}

/// Type-safe representation of an edge target. `Hash + Ord` participate
/// in `Edge` deduplication, so two unresolved edges with the same `raw`
/// but different causes still collapse — the *target* is the raw
/// string the user wrote, not our diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResolvedTarget {
    /// Successfully resolved to a node id.
    Resolved { id: String },
    /// Could not be resolved; `cause` is the typed refusal recorded at
    /// the resolver's refusal site (prose derives from its `Display`).
    Unresolved { raw: String, cause: UnresolvedCause },
}

impl ResolvedTarget {
    pub fn resolved(id: impl Into<String>) -> Self {
        Self::Resolved { id: id.into() }
    }

    pub fn unresolved(raw: impl Into<String>, cause: UnresolvedCause) -> Self {
        Self::Unresolved {
            raw: raw.into(),
            cause,
        }
    }

    /// Returns the resolved node id, or `None` if unresolved.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Resolved { id } => Some(id),
            Self::Unresolved { .. } => None,
        }
    }

    /// Component used for edge deduplication. For unresolved targets we
    /// key on the raw user-written string and ignore the diagnostic
    /// `cause`, so two callers' different explanations don't yield a
    /// duplicate edge.
    fn dedup_target(&self) -> DedupTarget {
        match self {
            Self::Resolved { id } => DedupTarget::Resolved(id.clone()),
            Self::Unresolved { raw, .. } => DedupTarget::Unresolved(raw.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DedupTarget {
    Resolved(String),
    Unresolved(String),
}

impl Edge {
    /// Identity for deduplication: source, target, and relation.
    /// `location` is not part of identity — two authoring sites for
    /// the same logical relation collapse to the first encountered.
    pub(crate) fn identity(&self) -> EdgeIdentity {
        EdgeIdentity {
            source: self.source.clone(),
            target: self.target.dedup_target(),
            relation: self.relation.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct EdgeIdentity {
    source: String,
    target: DedupTarget,
    relation: String,
}

/// An edge before target resolution (produced by the parser).
///
/// Carries `Serialize` / `Deserialize` so the build cache stores it
/// directly — there is no mirror struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawEdge {
    /// Raw target path or id from the document.
    pub target_path: String,
    pub relation: String,
    /// Source location, e.g. "L42" or "frontmatter:supersedes".
    pub location: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_covers_is_path_only() {
        // The predicate's domain splits exactly once: `covers` is the
        // single path-only relation; every other built-in and any
        // user-declared link-pattern relation is a document reference.
        assert!(!is_document_ref_relation(PATH_ONLY_RELATION));
        for relation in BUILTIN_EDGE_RELATIONS {
            assert_eq!(
                is_document_ref_relation(relation),
                *relation != PATH_ONLY_RELATION,
                "built-in {relation:?} misclassified"
            );
        }
        assert!(is_document_ref_relation("cites"));
    }

    #[test]
    fn edge_relations_partition_into_resolution_modes() {
        // The relations an edge can carry partition exactly into the
        // three resolution modes: path-only (`covers`), id-resolved (the
        // frontmatter id relations), and the document-reference default
        // (`references`). Sorted multiset equality catches a relation
        // missing from every mode, claimed by two modes, or duplicated —
        // a vocabulary edit cannot silently widen or narrow the
        // resolver's closed dispatch.
        let mut expected: Vec<&str> = vec![PATH_ONLY_RELATION, BODY_REFERENCE_RELATION];
        expected.extend_from_slice(ID_RESOLVED_RELATIONS);
        expected.sort_unstable();
        let mut actual: Vec<&str> = EDGE_RELATIONS.to_vec();
        actual.sort_unstable();
        assert_eq!(
            actual, expected,
            "EDGE_RELATIONS must partition into \
             {{PATH_ONLY_RELATION}} ∪ ID_RESOLVED_RELATIONS ∪ {{BODY_REFERENCE_RELATION}}"
        );
    }

    #[test]
    fn only_the_unresolved_only_relation_is_kept_off_the_resolved_vocabulary() {
        // A resolved `superseded_by:` is materialised as a `supersedes`
        // edge, so no resolved edge carries the relation and no
        // traversal, cycle check or drift measurement may name it. Every
        // other relation an edge can carry is nameable there — a
        // built-in absent from both sets would be one no surface can
        // filter on, and one present in both would be a filter that
        // matches nothing.
        let mut expected: Vec<&str> = EDGE_RELATIONS
            .iter()
            .copied()
            .filter(|relation| *relation != SUPERSEDED_BY_RELATION)
            .collect();
        expected.sort_unstable();
        let mut actual: Vec<&str> = BUILTIN_EDGE_RELATIONS.to_vec();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn every_cause_that_names_a_target_is_one_a_glob_can_select() {
        // The predicate gates `[[detection.unresolved_policy]]` globs at
        // load, and what it admits must be what `Sought` produces a name
        // for — a cause admitted here whose edges seek nothing would
        // accept a row that can never fire.
        for cause in [
            UnresolvedCause::IdNotFound,
            UnresolvedCause::Missing,
            UnresolvedCause::TargetUnparsed,
            UnresolvedCause::ExcludedFromScope,
        ] {
            assert!(cause.names_a_target(), "{cause:?}");
        }
        for cause in [UnresolvedCause::EscapesSource, UnresolvedCause::Absolute] {
            assert!(!cause.names_a_target(), "{cause:?}");
        }
    }
}
