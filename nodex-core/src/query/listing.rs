//! Generic node listing primitive — the universal "give me nodes
//! matching these predicates" operation.
//!
//! Distinct from the specialised query commands:
//!
//! - `detect::find_orphans` / `find_stale` apply *semantic* predicates
//!   (zero-incoming, past-review-threshold) that aren't reducible to
//!   filters
//! - `search::search` applies a *ranked* substring match across id /
//!   title / tags
//! - `recent::find_recent` applies a *date-window* predicate
//! - `find_nodes` here is the predicate listing — every result matches
//!   every named predicate, no ranking, no implicit policy. The spec
//!   is a pure predicate (`NodeFilter`); presentation capping
//!   (`--limit`) is the CLI envelope's concern, not the query's
//!
//! Every graph CLI has this primitive (SQL `SELECT WHERE`, Cypher
//! `MATCH (n) WHERE`, kubectl `get` with `--field-selector`). Keeping
//! the predicate set to the closed categories (kind, status, tag) plus
//! exact scalar field-equality (`--where field=value`, the `field_equals`
//! field) avoids the query-DSL trap; the long-tail filter case (ranges,
//! negation, joins) is served by `nodex_core::Graph::nodes()` for library
//! callers, or `nodex report && jq` for shell callers.

use crate::model::Graph;

use super::{NodeListingEntry, NodeRef};

/// Predicate spec for [`find_nodes`]. All fields are optional; the
/// filter combines them with **AND across categories, OR within a
/// category** — `kinds=[a,b], statuses=[c]` selects "nodes whose
/// kind ∈ {a,b} AND status ∈ {c}". Set `require_all_tags = true` to
/// switch the tag category from OR to AND.
///
/// `require_all_*` exists for `tags` but **not** for `kinds` /
/// `statuses` by design: a node has exactly one `kind` and exactly
/// one `status`, so the AND form of those categories would match at
/// most one value and is indistinguishable from `kinds = vec![x]`
/// with OR. Tags is the only multi-valued field, so OR-vs-AND is
/// the only category where the toggle is meaningful.
///
/// Empty vectors are treated as "no filter on this category" (i.e.
/// every node matches), so a default-constructed [`NodeFilter`] is
/// the identity predicate and `find_nodes(&graph, &NodeFilter::default())`
/// returns every node in deterministic order.
#[derive(Debug, Clone, Default)]
pub struct NodeFilter {
    pub kinds: Vec<String>,
    pub statuses: Vec<String>,
    pub tags: Vec<String>,
    pub require_all_tags: bool,
    /// Exact field-equality predicates (`--where field=value`) over
    /// scalar fields, ANDed with the categories above and with each
    /// other. Each is evaluated with the same field read as a
    /// `cross_field` `when field=value` predicate
    /// ([`crate::rules::schema::predicate_matches_node`]), so a `--where`
    /// and a `cross_field` agree on what a scalar field's value is. The
    /// CLI refuses a collection-valued built-in here, exactly as
    /// `cross_field` refuses equals/in on one at load — equality on a
    /// collection compares against a comma-joined string.
    pub field_equals: Vec<(String, String)>,
}

impl NodeFilter {
    fn matches(&self, node: &crate::model::Node) -> bool {
        if !self.kinds.is_empty() && !self.kinds.iter().any(|k| k == node.kind.as_str()) {
            return false;
        }
        if !self.statuses.is_empty() && !self.statuses.iter().any(|s| s == node.status.as_str()) {
            return false;
        }
        for (field, value) in &self.field_equals {
            let predicate = crate::config::WhenPredicate::Equals {
                field: field.clone(),
                value: value.clone(),
            };
            if !crate::rules::schema::predicate_matches_node(&predicate, node) {
                return false;
            }
        }
        if !self.tags.is_empty() {
            // Case-insensitive tag comparison: tags authored as
            // `Auth` and `auth` both match `--tag auth`. Same fold
            // every other tag-consuming surface uses.
            let needle: Vec<String> = self.tags.iter().map(|t| t.to_lowercase()).collect();
            let haystack: Vec<String> = node.tags.iter().map(|t| t.to_lowercase()).collect();
            let ok = if self.require_all_tags {
                needle.iter().all(|t| haystack.contains(t))
            } else {
                needle.iter().any(|t| haystack.contains(t))
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

/// Every node whose state satisfies every category of `filter`.
/// Output is complete and deterministic: sorted by `id` ascending —
/// callers that cap for presentation take a prefix of this order.
pub fn find_nodes(graph: &Graph, filter: &NodeFilter) -> Vec<NodeRef> {
    let mut out: Vec<NodeRef> = graph
        .nodes()
        .values()
        .filter(|n| filter.matches(n))
        .map(NodeRef::from_node)
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// [`find_nodes`] with field projection. `spine_fields` lists exactly
/// which of the five [`NodeRef`] identity fields appear (empty = none —
/// the CLI passes the full [`NodeRef`] spine for the default listing, so
/// it always carries the five-field identity); `extra_fields` names
/// non-spine frontmatter fields (other built-ins and project-declared
/// `attrs` keys) to enrich each entry's `attrs` map (empty = none). Both
/// vocabularies are validated up front by the caller against
/// `Config::declared_fields_universe()` ∪ the spine. Output is complete
/// and deterministic, sorted by id — callers cap for presentation.
pub fn find_nodes_projected(
    graph: &Graph,
    filter: &NodeFilter,
    spine_fields: &[String],
    extra_fields: &[String],
) -> Vec<NodeListingEntry> {
    let mut matched: Vec<&crate::model::Node> = graph
        .nodes()
        .values()
        .filter(|n| filter.matches(n))
        .collect();
    matched.sort_by(|a, b| a.id.cmp(&b.id));
    matched
        .into_iter()
        .map(|n| NodeListingEntry::project(n, spine_fields, extra_fields))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Node, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn node(id: &str, kind: &str, status: &str, tags: &[&str]) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("docs/{id}.md")),
            title: id.into(),
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
            tags: tags.iter().map(|t| t.to_string()).collect(),
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

    fn graph(nodes: Vec<Node>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(
            map,
            vec![],
            vec![],
            vec![],
            vec![],
            crate::model::GraphMeta::default(),
        )
    }

    #[test]
    fn empty_filter_returns_every_node_sorted_by_id() {
        let g = graph(vec![
            node("c", "generic", "active", &[]),
            node("a", "generic", "active", &[]),
            node("b", "generic", "active", &[]),
        ]);
        let out = find_nodes(&g, &NodeFilter::default());
        assert_eq!(
            out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn kind_filter_or_within_category() {
        let g = graph(vec![
            node("a", "spec", "active", &[]),
            node("b", "adr", "active", &[]),
            node("c", "guide", "active", &[]),
        ]);
        let out = find_nodes(
            &g,
            &NodeFilter {
                kinds: vec!["spec".into(), "adr".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn status_filter_or_within_category() {
        let g = graph(vec![
            node("a", "generic", "active", &[]),
            node("b", "generic", "superseded", &[]),
            node("c", "generic", "archived", &[]),
        ]);
        let out = find_nodes(
            &g,
            &NodeFilter {
                statuses: vec!["active".into(), "superseded".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn kind_and_status_intersect_across_categories() {
        let g = graph(vec![
            node("a", "spec", "active", &[]),
            node("b", "spec", "archived", &[]),
            node("c", "adr", "active", &[]),
        ]);
        let out = find_nodes(
            &g,
            &NodeFilter {
                kinds: vec!["spec".into()],
                statuses: vec!["active".into()],
                ..Default::default()
            },
        );
        assert_eq!(out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn tag_any_of_default() {
        let g = graph(vec![
            node("a", "generic", "active", &["auth"]),
            node("b", "generic", "active", &["policy"]),
            node("c", "generic", "active", &["unrelated"]),
        ]);
        let out = find_nodes(
            &g,
            &NodeFilter {
                tags: vec!["auth".into(), "policy".into()],
                ..Default::default()
            },
        );
        assert_eq!(
            out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn tag_all_of_when_require_all_tags() {
        let g = graph(vec![
            node("a", "generic", "active", &["auth", "policy"]),
            node("b", "generic", "active", &["auth"]),
        ]);
        let out = find_nodes(
            &g,
            &NodeFilter {
                tags: vec!["auth".into(), "policy".into()],
                require_all_tags: true,
                ..Default::default()
            },
        );
        assert_eq!(out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn field_equals_filters_on_a_builtin() {
        let mut a = node("a", "spec", "active", &[]);
        a.owner = Some("alice".into());
        let mut b = node("b", "spec", "active", &[]);
        b.owner = Some("bob".into());
        let out = find_nodes(
            &graph(vec![a, b]),
            &NodeFilter {
                field_equals: vec![("owner".into(), "alice".into())],
                ..Default::default()
            },
        );
        assert_eq!(out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn field_equals_filters_on_a_declared_attr_and_ands_with_categories() {
        let mut a = node("a", "spec", "active", &[]);
        a.attrs.insert("priority".into(), serde_json::json!("high"));
        let mut b = node("b", "adr", "active", &[]); // wrong kind
        b.attrs.insert("priority".into(), serde_json::json!("high"));
        let mut c = node("c", "spec", "active", &[]); // wrong priority
        c.attrs.insert("priority".into(), serde_json::json!("low"));
        let out = find_nodes(
            &graph(vec![a, b, c]),
            &NodeFilter {
                kinds: vec!["spec".into()],
                field_equals: vec![("priority".into(), "high".into())],
                ..Default::default()
            },
        );
        assert_eq!(out.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn projection_keeps_spine_and_enriches_attrs() {
        use serde_json::json;
        let mut n = node("a", "spec", "active", &[]);
        n.owner = Some("alice".into());
        n.attrs.insert("priority".into(), json!("high"));
        let g = graph(vec![n]);

        let items = find_nodes_projected(
            &g,
            &NodeFilter::default(),
            &["id".to_string()],
            &["owner".to_string(), "priority".to_string()],
        );
        assert_eq!(items.len(), 1);
        let v = serde_json::to_value(&items[0]).unwrap();
        let obj = v.as_object().unwrap();
        assert_eq!(obj["id"], json!("a"));
        assert!(
            obj.get("title").is_none(),
            "an unrequested spine field is dropped: {obj:?}"
        );
        assert_eq!(obj["attrs"]["owner"], json!("alice"));
        assert_eq!(obj["attrs"]["priority"], json!("high"));
    }

    #[test]
    fn projection_without_extras_omits_attrs() {
        let g = graph(vec![node("a", "spec", "active", &[])]);
        let all: Vec<String> = super::super::NODE_REF_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let items = find_nodes_projected(&g, &NodeFilter::default(), &all, &[]);
        let v = serde_json::to_value(&items[0]).unwrap();
        let obj = v.as_object().unwrap();
        assert!(
            obj.get("attrs").is_none(),
            "no extras requested → attrs omitted"
        );
        assert_eq!(obj.len(), 5, "the full spine is emitted");
    }

    #[test]
    fn tag_match_is_case_insensitive() {
        let g = graph(vec![node("a", "generic", "active", &["Auth"])]);
        let out = find_nodes(
            &g,
            &NodeFilter {
                tags: vec!["auth".into()],
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
    }
}
