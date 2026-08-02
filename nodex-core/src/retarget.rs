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

/// What a retarget did to one document: the rewritten content when it
/// changed, and every body reference it had a replacement for and left
/// standing.
///
/// Frontmatter relation fields carry no spelling of their own — an id
/// field holds the id — so there is nothing there a rewrite can fail to
/// respell, and everything this reports comes from the body.
#[derive(Debug, Default)]
pub struct Retargeted {
    pub content: Option<String>,
    pub refused: Vec<String>,
}

/// Rewrite every reference to `old_id` in `content` so it names `new_id`.
///
/// Covers the id-valued frontmatter relation fields (`supersedes`,
/// `implements`, `related`, `superseded_by`) and body id references
/// (wikilinks, custom patterns). The document whose own id is `new_id` is
/// never rewritten: rewriting a successor's `supersedes: [old_id]` would
/// turn it into a self-edge.
pub fn retarget_document(
    content: &str,
    node: &Node,
    old_id: &str,
    new_id: &str,
    bound: &crate::builder::resolver::Bindings,
    parser: &ParserConfig,
) -> Result<Retargeted> {
    if node.id == new_id {
        return Ok(Retargeted::default());
    }

    // Operate on the same canonical form the builder parsed (BOM stripped,
    // CRLF → LF), so `split_frontmatter` — which only recognizes `---\n` —
    // and the body scan agree with the node's parsed fields. A CRLF or
    // BOM-prefixed document would otherwise fail to split and the
    // frontmatter edit below would have nothing to act on.
    let canonical = crate::parser::frontmatter::canonicalize(content);
    let content = canonical.as_ref();

    let relation_edits: Vec<(&str, Vec<String>)> = LIST_RELATION_FIELDS
        .into_iter()
        .filter_map(|field| {
            let values = match field {
                "supersedes" => &node.supersedes,
                "implements" => &node.implements,
                "related" => &node.related,
                _ => unreachable!("field set is LIST_RELATION_FIELDS"),
            };
            values
                .iter()
                .any(|v| v == old_id)
                .then(|| (field, replace_dedup(values, old_id, new_id)))
        })
        .collect();
    let retarget_superseded_by = node.superseded_by.as_deref() == Some(old_id);

    let source_dir = node.path.parent().unwrap_or_else(|| Path::new(""));
    let body_rewrite = rewrite_id_references(content, old_id, new_id, source_dir, bound, parser)
        .map_err(|source| crate::error::Error::Parse {
            path: node.path.clone(),
            source,
        })?;

    let refused = body_rewrite.refused;
    if relation_edits.is_empty() && !retarget_superseded_by && body_rewrite.content.is_none() {
        return Ok(Retargeted {
            content: None,
            refused,
        });
    }

    // Body rewriting leaves frontmatter untouched, so apply it first and
    // edit the frontmatter of whatever content we then hold.
    let working = body_rewrite.content.unwrap_or_else(|| content.to_string());
    if relation_edits.is_empty() && !retarget_superseded_by {
        return Ok(Retargeted {
            content: Some(working),
            refused,
        });
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
    Ok(Retargeted {
        content: Some(format!("---\n{}---\n{body}", editor.render())),
        refused,
    })
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
        // a self-edge.
        let mut n = node("spec-new");
        n.supersedes = vec!["spec-old".into()];
        let content = "---\nid: spec-new\nsupersedes: [spec-old]\n---\n#\n";
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
            .is_none()
        );
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
