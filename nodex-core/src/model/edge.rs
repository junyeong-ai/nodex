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
    pub const ORPHAN: &str = "orphan";
    pub const STALE: &str = "stale";
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
    /// Frontmatter id relation (`supersedes` / `implements` /
    /// `related` / `superseded_by`) whose value isn't a known node id.
    IdNotFound,
    /// Body-link path that resolves to no node. The build records every
    /// unmatched path-link with this cause (it never stats the disk);
    /// a *reported* `Missing` has additionally survived the
    /// classifier's probes, so its target corresponds to no file on
    /// disk under the project root.
    Missing,
    /// Body-link path whose file is in scope but failed to parse and
    /// so has no node — the path is recorded in
    /// [`crate::model::Graph::parse_failures`]. The reference is not
    /// excluded-by-design and not missing: fixing the target document
    /// resolves it.
    TargetUnparsed,
    /// Body-link path whose file exists on disk but isn't in the
    /// graph's scan scope — most commonly removed by
    /// `[[scope.conditional_exclude]]` on a terminal-status parent.
    ExcludedFromScope,
    /// Body-link path that walks above the source file's directory
    /// via `..` segments. Refused as a security guard, never resolved.
    EscapesSource,
    /// Body-link path written as an absolute path. Refused as out of
    /// project scope.
    Absolute,
}

impl UnresolvedCause {
    /// Whether edges with this cause carry normalized root-relative
    /// resolution candidates — the set a
    /// `[[detection.unresolved_policy]]` row's `glob` matches against
    /// and the disk probes (cause classifier, git-drift targets) may
    /// stat. `IdNotFound` names node ids, and `EscapesSource` /
    /// `Absolute` are refused before any root-relative resolution
    /// exists, so rows for those causes are cause-only
    /// (`Config::validate` rejects a `glob` on them at load). The match
    /// is exhaustive by variant so adding a cause forces this decision
    /// at compile time.
    pub fn has_path_candidates(&self) -> bool {
        match self {
            Self::Missing | Self::TargetUnparsed | Self::ExcludedFromScope => true,
            Self::IdNotFound | Self::EscapesSource | Self::Absolute => false,
        }
    }
}

/// The one prose rendering per cause — every human-facing `reason`
/// (issue reports, violation messages) derives from here, so the typed
/// cause and its prose can never disagree.
impl std::fmt::Display for UnresolvedCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::IdNotFound => "node id not found in graph",
            Self::Missing => "path not found in scope",
            Self::TargetUnparsed => "target is in scope but failed to parse",
            Self::ExcludedFromScope => "target exists on disk but is excluded from scope",
            Self::EscapesSource => "path escapes source scope",
            Self::Absolute => "absolute paths are not in scope",
        })
    }
}

/// Every edge relation the parser emits without a user-declared
/// `[[parser.link_patterns]]` block — the closed, typed core vocabulary.
///
/// Each built-in relation is a code-backed graph operation, not merely a
/// label, which is why the set is fixed rather than config-declared:
/// - `supersedes` drives the build-time supersession DAG check
/// - `implements` is the default `rules.acyclic_relations` member
/// - `covers` points at out-of-graph code paths (drift detection)
/// - `related` is the soft, unconstrained cross-link
/// - `references` is the default body-link relation
///
/// What varies between projects is link *syntax*, not these semantics —
/// and that is precisely what `[[parser.link_patterns]]` opens up:
/// body link patterns declare *document references*, mapping any regex
/// to any relation name whose resolution mode is not fixed in code. A
/// relation with code-fixed resolution semantics — the path-only
/// [`PATH_ONLY_RELATION`] (`covers`) and the id-resolved
/// [`ID_RESOLVED_RELATIONS`] (`supersedes` / `implements` / `related`)
/// — is producible only by its frontmatter field; `Config::validate`
/// rejects a link pattern naming one, because resolution semantics
/// attach to the field that produces a relation, never to a name a
/// user can pick. `references` stays legal on patterns: it resolves in
/// document-reference mode either way, so a pattern naming it shifts
/// no semantics. `Config::known_relations` and every
/// `--relations`-filtering query read from this list, so a future
/// built-in is acknowledged in one place.
pub const BUILTIN_EDGE_RELATIONS: &[&str] = &[
    "references",
    "supersedes",
    "implements",
    "related",
    "covers",
];

/// The one relation resolved strictly by path: `covers` names
/// out-of-graph code paths, so extension-append and id-fallback would
/// corrupt its drift signal by binding a covered path to a
/// coincidentally-named node. Single source for the name, consumed by
/// [`is_document_ref_relation`] and the `Config::validate` guard that
/// keeps it off user-declared link patterns, so the boundary is never
/// spelled twice.
pub(crate) const PATH_ONLY_RELATION: &str = "covers";

/// The relations resolved strictly by node id — no path lookup, no
/// extension append, no id fallback. Like [`PATH_ONLY_RELATION`], their
/// resolution mode is fixed in code, so each is producible only by its
/// frontmatter field (`supersedes:` / `implements:` / `related:`):
/// `Config::validate` rejects a link pattern naming any of them, which
/// is what makes the resolver's id dispatch a closed, code-owned
/// vocabulary rather than a guess about a user-chosen name. Distinct
/// from [`super::ID_RELATION_FIELDS`], the frontmatter-*field*
/// vocabulary the lock probes read — `superseded_by` is a field there
/// but emits no edge, so it has no relation to dispatch on.
pub(crate) const ID_RESOLVED_RELATIONS: &[&str] = &["supersedes", "implements", "related"];

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
}
