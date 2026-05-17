//! Shared scope filtering for body-derived rules.
//!
//! Every body-derived primitive — annotations, body-line matches,
//! body-block matches — is gated by a triple `(applies_to_kind,
//! applies_to_status, applies_to_tag)`. The match contributes to the
//! graph only when the source node satisfies every populated
//! category, with OR semantics inside a category (the node's value
//! must appear in the list) and AND semantics across categories
//! (every populated list must accept the node). An empty list means
//! "no restriction on this axis" — the recommended default during
//! early authoring while the project decides what to lock down.
//!
//! Centralising the predicate guarantees that a future fourth axis
//! (`applies_to_owner`? `applies_to_path_glob`?) lands once, not in
//! three parallel implementations that would inevitably drift.

use crate::model::Node;

/// The three-axis predicate every body-derived rule consumes.
/// Borrowed slices: the lifetime is the config's lifetime, which is
/// always longer than the materialisation pass — passing slices keeps
/// the per-block hot loop allocation-free.
#[derive(Debug, Clone, Copy)]
pub struct ScopePredicate<'a> {
    pub kinds: &'a [String],
    pub statuses: &'a [String],
    pub tags: &'a [String],
}

impl<'a> ScopePredicate<'a> {
    /// True when `node` satisfies every populated category. Each
    /// category contributes "no constraint" when its list is empty
    /// and "must match at least one value" otherwise — same convention
    /// [`crate::query::NodeFilter`] uses for its kind / status / tag
    /// triple, so authors moving between query filters and rule
    /// scopes never have to relearn the semantics.
    pub fn matches(&self, node: &Node) -> bool {
        if !self.kinds.is_empty() && !self.kinds.iter().any(|k| k == node.kind.as_str()) {
            return false;
        }
        if !self.statuses.is_empty() && !self.statuses.iter().any(|s| s == node.status.as_str()) {
            return false;
        }
        if !self.tags.is_empty() && !self.tags.iter().any(|t| node.tags.contains(t)) {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Node, Status};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(kind: &str, status: &str, tags: &[&str]) -> Node {
        Node {
            id: "x".into(),
            path: PathBuf::from("x.md"),
            title: "x".into(),
            kind: Kind::new(kind),
            status: Status::new(status),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: tags.iter().map(|t| (*t).into()).collect(),
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
        }
    }

    fn p<'a>(
        kinds: &'a [String],
        statuses: &'a [String],
        tags: &'a [String],
    ) -> ScopePredicate<'a> {
        ScopePredicate {
            kinds,
            statuses,
            tags,
        }
    }

    #[test]
    fn empty_predicate_matches_every_node() {
        // The identity predicate. All three categories empty → every
        // node passes. This is the default when a rule omits all
        // scoping keys.
        let n = node("anything", "anything", &[]);
        assert!(p(&[], &[], &[]).matches(&n));
    }

    #[test]
    fn kind_filter_or_within_category() {
        let n = node("spec", "active", &[]);
        let kinds = vec!["spec".into(), "adr".into()];
        assert!(p(&kinds, &[], &[]).matches(&n));
        let other = vec!["readme".into()];
        assert!(!p(&other, &[], &[]).matches(&n));
    }

    #[test]
    fn status_filter_or_within_category() {
        let n = node("spec", "superseded", &[]);
        let statuses = vec!["superseded".into(), "archived".into()];
        assert!(p(&[], &statuses, &[]).matches(&n));
        let other = vec!["active".into()];
        assert!(!p(&[], &other, &[]).matches(&n));
    }

    #[test]
    fn tag_filter_requires_intersection() {
        let n = node("spec", "active", &["auth", "policy"]);
        // OR-within-category: at least one declared tag must match.
        let tags = vec!["policy".into(), "ops".into()];
        assert!(p(&[], &[], &tags).matches(&n));
        let absent = vec!["billing".into()];
        assert!(!p(&[], &[], &absent).matches(&n));
    }

    #[test]
    fn cross_category_is_and() {
        // Kind matches but status doesn't — the node fails the AND.
        let n = node("spec", "active", &["auth"]);
        let kinds = vec!["spec".into()];
        let statuses = vec!["superseded".into()];
        assert!(!p(&kinds, &statuses, &[]).matches(&n));
    }

    #[test]
    fn populated_category_with_empty_node_value_rejects() {
        // A node with no tags cannot satisfy a non-empty tag filter
        // — there's no value to match against.
        let n = node("spec", "active", &[]);
        let tags = vec!["auth".into()];
        assert!(!p(&[], &[], &tags).matches(&n));
    }
}
