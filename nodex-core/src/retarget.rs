//! Repointing references from one node id to another.
//!
//! When a document is replaced (typically `lifecycle supersede`), other
//! documents that referenced the old id should point at the successor.
//! This rewrites those references — frontmatter relation fields and body
//! id references — by *exact id match*: no prose heuristics, so an id that
//! merely appears in running text is never touched.

use std::path::Path;

use crate::config::ParserConfig;
use crate::error::Result;
use crate::model::Node;
use crate::parser::editor::FrontmatterEditor;
use crate::parser::frontmatter::split_frontmatter;
use crate::reference_rewrite::rewrite_id_references;

/// The *list*-valued id-relation frontmatter fields retarget iterates.
/// A narrower set than [`crate::model::ID_RELATION_FIELDS`] by design:
/// `superseded_by` is a scalar handled separately below, and `covers`
/// holds code paths, not ids.
const LIST_RELATION_FIELDS: [&str; 3] = ["supersedes", "implements", "related"];

/// The field that records succession itself, left out of what the successor
/// reports keeping.
///
/// Not because a repoint may not touch it — a *third* document's
/// `supersedes: [old]` is repointed like any other reference, and its own
/// succession record moves with it. Because on the successor it is
/// definitional: `supersedes: [old]` there is exactly what makes this
/// document old's successor, so it is present in every succession that ever
/// happens, and a report naming it would be a signal that is always on. It
/// is left standing for the same reason as everything else in that document —
/// a repoint does not rewrite the document it points at — and reporting it
/// would say only what the caller established by invoking the command.
///
/// `superseded_by` is not its twin here: on the successor it claims the
/// *predecessor* replaced it, which contradicts the repoint rather than
/// recording it, and is reported like any other reference.
///
/// The residual is that `lifecycle supersede A B` followed by `retarget A B`,
/// with nothing else naming A, reports `total_updated: 0` and no warning —
/// while `query backlinks A` shows B reaching A through `supersedes`. That
/// edge is the succession itself and this command is not what discloses it.
const SUCCESSION_FIELD: &str = "supersedes";

/// What a retarget did to one document: the rewritten content when it
/// changed, every body reference it had a replacement for and left
/// standing, and — for the successor — the references it declined to move.
///
/// Frontmatter relation fields carry no spelling of their own — an id
/// field holds the id — so there is nothing there a rewrite can fail to
/// respell, and everything [`Self::refused`] reports comes from the body.
#[derive(Debug, Default)]
pub struct Retargeted {
    pub content: Option<String>,
    pub refused: Vec<String>,
    pub self_edges: SelfEdges,
}

impl Retargeted {
    /// The outcome for a document a repoint may rewrite — every document but
    /// the successor, and so the one with no reference a replacement could
    /// turn on itself.
    fn rewritten(content: Option<String>, refused: Vec<String>) -> Self {
        Self {
            content,
            refused,
            self_edges: SelfEdges::default(),
        }
    }
}

/// The predecessor references the successor holds, which a repoint never
/// writes: it does not rewrite the document it points at, so nothing there
/// can be made to name it.
///
/// Everything else the repoint leaves behind comes to name nothing and
/// surfaces as an unresolved edge on the next build. These do not — the
/// predecessor still exists and the reference goes on naming it — so the
/// project this command leaves is in order, no rule has a reason to mention
/// them, and `total_updated: 0` reads the same whether the successor held a
/// reference or the project held none at all. The command is the only place
/// the difference is known, which is why it says it.
///
/// A frontmatter field holds the id under a name, so the name is what a
/// site has to say; a body id reference spells the id verbatim — that is
/// what makes it one — so a count is.
#[derive(Debug, Default)]
pub struct SelfEdges {
    pub fields: Vec<&'static str>,
    pub body_references: usize,
}

impl SelfEdges {
    /// How many references the successor kept: an id field holds one, and
    /// each body reference is one.
    pub fn len(&self) -> usize {
        self.fields.len() + self.body_references
    }

    /// Whether the successor held nothing a repoint would otherwise have
    /// moved.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// What `node` holds that names `old_id` and a repoint declines to move.
    fn held_by(
        node: &Node,
        old_id: &str,
        content: &str,
        source_dir: &Path,
        bound: &crate::builder::resolver::Bindings,
        parser: &ParserConfig,
    ) -> Result<Self> {
        Ok(Self {
            fields: LIST_RELATION_FIELDS
                .into_iter()
                .filter(|field| *field != SUCCESSION_FIELD)
                .filter(|field| relation_values(node, field).iter().any(|v| v == old_id))
                .chain((node.superseded_by.as_deref() == Some(old_id)).then_some("superseded_by"))
                .collect(),
            body_references: crate::reference_rewrite::count_id_references(
                content, old_id, source_dir, bound, parser,
            )
            .map_err(|source| crate::error::Error::Parse {
                path: node.path.clone(),
                source,
            })?,
        })
    }
}

/// Rewrite every reference to `old_id` in `content` so it names `new_id`.
///
/// Covers the id-valued frontmatter relation fields (`supersedes`,
/// `implements`, `related`, `superseded_by`) and body id references
/// (wikilinks, custom patterns). The document whose own id is `new_id` is
/// never rewritten — a repoint does not rewrite the document it points at —
/// and it is the one document that reports what it kept, in
/// [`Retargeted::self_edges`].
pub fn retarget_document(
    content: &str,
    node: &Node,
    old_id: &str,
    new_id: &str,
    bound: &crate::builder::resolver::Bindings,
    parser: &ParserConfig,
) -> Result<Retargeted> {
    // Operate on the same canonical form the builder parsed (BOM stripped,
    // CRLF → LF), so `split_frontmatter` — which only recognizes `---\n` —
    // and the body scan agree with the node's parsed fields. A CRLF or
    // BOM-prefixed document would otherwise fail to split and the
    // frontmatter edit below would have nothing to act on.
    let canonical = crate::parser::frontmatter::canonicalize(content);
    let content = canonical.as_ref();
    let source_dir = node.path.parent().unwrap_or_else(|| Path::new(""));

    if node.id == new_id {
        return Ok(Retargeted {
            self_edges: SelfEdges::held_by(node, old_id, content, source_dir, bound, parser)?,
            ..Retargeted::default()
        });
    }

    let relation_edits: Vec<(&str, Vec<String>)> = LIST_RELATION_FIELDS
        .into_iter()
        .filter_map(|field| {
            let values = relation_values(node, field);
            values
                .iter()
                .any(|v| v == old_id)
                .then(|| (field, replace_dedup(values, old_id, new_id)))
        })
        .collect();
    let retarget_superseded_by = node.superseded_by.as_deref() == Some(old_id);

    let body_rewrite = rewrite_id_references(content, old_id, new_id, source_dir, bound, parser)
        .map_err(|source| crate::error::Error::Parse {
            path: node.path.clone(),
            source,
        })?;

    let refused = body_rewrite.refused;
    if relation_edits.is_empty() && !retarget_superseded_by && body_rewrite.content.is_none() {
        return Ok(Retargeted::rewritten(None, refused));
    }

    // Body rewriting leaves frontmatter untouched, so apply it first and
    // edit the frontmatter of whatever content we then hold.
    let working = body_rewrite.content.unwrap_or_else(|| content.to_string());
    if relation_edits.is_empty() && !retarget_superseded_by {
        return Ok(Retargeted::rewritten(Some(working), refused));
    }

    let (yaml, body) =
        split_frontmatter(&working).map_err(|source| crate::error::Error::Parse {
            path: node.path.clone(),
            source,
        })?;
    // Relation fields and `superseded_by` are frontmatter, so a node
    // carrying them always has a frontmatter block to edit.
    let yaml = yaml.expect("node with relation frontmatter has a frontmatter block");
    let mut editor = FrontmatterEditor::parse(yaml, &node.path)?;
    for (field, values) in &relation_edits {
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        editor.set_list(field, &refs);
    }
    if retarget_superseded_by {
        editor.set("superseded_by", new_id);
    }
    Ok(Retargeted::rewritten(
        Some(format!("---\n{}---\n{body}", editor.render())),
        refused,
    ))
}

/// The values a [`LIST_RELATION_FIELDS`] field holds on `node`.
fn relation_values<'a>(node: &'a Node, field: &str) -> &'a [String] {
    match field {
        "supersedes" => &node.supersedes,
        "implements" => &node.implements,
        "related" => &node.related,
        _ => unreachable!("field set is LIST_RELATION_FIELDS"),
    }
}

/// `values` with every `old_id` replaced by `new_id`, dropping any
/// duplicate the replacement introduces (the successor may already be
/// referenced alongside the predecessor).
fn replace_dedup(values: &[String], old_id: &str, new_id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        let mapped = if value == old_id {
            new_id
        } else {
            value.as_str()
        };
        if !out.iter().any(|existing| existing == mapped) {
            out.push(mapped.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::resolver::Bindings;
    use std::collections::BTreeSet;

    /// The world a scope makes, each document's id being its own path.
    fn bound_of(paths: &BTreeSet<String>) -> Bindings {
        Bindings::of(
            paths
                .iter()
                .map(|path| (Path::new(path.as_str()), path.as_str())),
        )
    }
    use crate::model::{Kind, Status};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.into(),
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
        }
    }

    fn parser() -> ParserConfig {
        ParserConfig::default()
    }

    /// A parser that reads `[[wikilink]]`, so the body carries id
    /// references at all.
    fn wikilink_parser() -> ParserConfig {
        ParserConfig {
            wikilink_enabled: true,
            ..ParserConfig::default()
        }
    }

    #[test]
    fn the_succession_field_is_one_a_repoint_would_otherwise_move() {
        // The exclusion says something only while the field is one the
        // non-successor path repoints; naming a field outside that set would
        // leave the successor reporting every relation it holds.
        assert!(LIST_RELATION_FIELDS.contains(&SUCCESSION_FIELD));
    }

    #[test]
    fn rewrites_related_field_value() {
        let mut n = node("doc");
        n.related = vec!["spec-old".into(), "other".into()];
        let content = "---\nid: doc\nrelated: [spec-old, other]\n---\n# Doc\n";
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &bound_of(&BTreeSet::new()),
            &parser(),
        )
        .unwrap()
        .content
        .expect("changed");
        assert!(out.contains("spec-new"), "out: {out}");
        assert!(!out.contains("spec-old"), "out: {out}");
        assert!(out.contains("other"), "unrelated value preserved: {out}");
    }

    #[test]
    fn rewrites_superseded_by_scalar() {
        let mut n = node("doc");
        n.superseded_by = Some("spec-old".into());
        let content = "---\nid: doc\nsuperseded_by: spec-old\n---\n# Doc\n";
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &bound_of(&BTreeSet::new()),
            &parser(),
        )
        .unwrap()
        .content
        .expect("changed");
        assert!(out.contains("spec-new"), "out: {out}");
        assert!(!out.contains("spec-old"), "out: {out}");
    }

    #[test]
    fn dedups_when_successor_already_referenced() {
        let mut n = node("doc");
        n.related = vec!["spec-new".into(), "spec-old".into()];
        let content = "---\nid: doc\nrelated: [spec-new, spec-old]\n---\n#\n";
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &bound_of(&BTreeSet::new()),
            &parser(),
        )
        .unwrap()
        .content
        .expect("changed");
        assert_eq!(out.matches("spec-new").count(), 1, "deduped: {out}");
    }

    #[test]
    fn skips_the_successor_document() {
        // The successor's own `supersedes: [old]` must stay — never become
        // a self-edge. It is the succession record, so nothing is reported
        // either: a line here would be on for every succession there is.
        let mut n = node("spec-new");
        n.supersedes = vec!["spec-old".into()];
        let content = "---\nid: spec-new\nsupersedes: [spec-old]\n---\n#\n";
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &Bindings::default(),
            &parser(),
        )
        .unwrap();
        assert!(out.content.is_none());
        assert!(out.self_edges.is_empty(), "{:?}", out.self_edges);
    }

    #[test]
    fn the_successor_names_every_reference_it_keeps() {
        // Everything but the succession record is a reference a repoint
        // would have moved anywhere else, and it stands only because of
        // where it sits — which the command is the only place to know.
        let mut n = node("spec-new");
        n.supersedes = vec!["spec-old".into()];
        n.implements = vec!["spec-old".into()];
        n.related = vec!["spec-old".into()];
        let content = "---\nid: spec-new\nsupersedes: [spec-old]\nimplements: [spec-old]\n\
                       related: [spec-old]\n---\n\
                       # N\n\nsee [[spec-old]] and again [[spec-old]]\n";
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &bound_of(&BTreeSet::new()),
            &wikilink_parser(),
        )
        .unwrap();
        assert!(out.content.is_none(), "the successor is never rewritten");
        assert_eq!(out.self_edges.fields, ["implements", "related"]);
        assert_eq!(out.self_edges.body_references, 2);
    }

    #[test]
    fn the_successor_names_a_superseded_by_naming_its_predecessor() {
        // The inverse of the succession record, and not its twin: on the
        // successor it claims the predecessor replaced it, which contradicts
        // the repoint rather than recording it. Fixtured alone, because a
        // document holding both is a succession cycle no build accepts.
        let mut n = node("spec-new");
        n.superseded_by = Some("spec-old".into());
        let content = "---\nid: spec-new\nsuperseded_by: spec-old\n---\n# N\n";
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &Bindings::default(),
            &parser(),
        )
        .unwrap();
        assert!(out.content.is_none(), "the successor is never rewritten");
        assert_eq!(out.self_edges.fields, ["superseded_by"]);
    }

    #[test]
    fn two_readers_of_one_body_reference_count_once() {
        // `[[old]]` bound by the wikilink reader and by a pattern of the
        // same relation is one span, one relation, one target — the graph
        // holds it once and a rewrite writes it once, so the account of what
        // a repoint left has to say one too.
        let parser = ParserConfig {
            wikilink_enabled: true,
            link_patterns: vec![crate::config::LinkPattern {
                pattern: r"\[\[([^\]]+)\]\]".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        let mut n = node("spec-new");
        n.related = vec!["spec-old".into()];
        let content = "---\nid: spec-new\nrelated: [spec-old]\n---\n# N\n\nonce: [[spec-old]]\n";
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &bound_of(&BTreeSet::new()),
            &parser,
        )
        .unwrap();
        assert_eq!(out.self_edges.body_references, 1, "{:?}", out.self_edges);
    }

    #[test]
    fn a_successor_body_reference_bound_by_path_is_not_kept_as_an_id() {
        // The build reads `[[spec-old]]` beside `spec-old.md` as a path
        // edge, so a repoint has no replacement for it anywhere — and the
        // successor must not claim it declined one.
        let mut n = node("spec-new");
        n.related = vec!["spec-old".into()];
        let content = "---\nid: spec-new\nrelated: [spec-old]\n---\n# N\n\nsee [[spec-old]]\n";
        let world = BTreeSet::from(["docs/spec-old.md".to_string()]);
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &bound_of(&world),
            &wikilink_parser(),
        )
        .unwrap();
        assert_eq!(out.self_edges.fields, ["related"]);
        assert_eq!(out.self_edges.body_references, 0);
    }

    #[test]
    fn untouched_when_no_reference() {
        let n = node("doc");
        let content = "---\nid: doc\n---\nMentions spec-old in prose only.\n";
        assert!(
            retarget_document(
                content,
                &n,
                "spec-old",
                "spec-new",
                &Bindings::default(),
                &parser()
            )
            .unwrap()
            .content
            .is_none(),
            "prose mention of the id must not be rewritten"
        );
    }

    #[test]
    fn handles_crlf_frontmatter_without_panicking() {
        // A CRLF document still parses into the graph (the builder
        // canonicalizes), so retarget must canonicalize too rather than
        // panic when `split_frontmatter` can't see a `---\n` delimiter.
        let mut n = node("doc");
        n.related = vec!["spec-old".into()];
        let content = "---\r\nid: doc\r\nrelated: [spec-old]\r\n---\r\n# Doc\r\n";
        let out = retarget_document(
            content,
            &n,
            "spec-old",
            "spec-new",
            &bound_of(&BTreeSet::new()),
            &parser(),
        )
        .unwrap()
        .content
        .expect("changed");
        assert!(out.contains("spec-new"), "out: {out}");
        assert!(!out.contains("spec-old"), "out: {out}");
    }
}
