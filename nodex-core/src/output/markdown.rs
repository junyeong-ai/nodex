use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::config::Config;
use crate::hash;
use crate::model::Graph;

/// Collapse any interpolated field to a single line for safe insertion
/// into the report: any whitespace run — crucially a newline — becomes
/// one space, so a multi-line value cannot inject a heading or list item
/// and break the report's structure. EVERY interpolated value goes
/// through this — titles, ids, kinds, paths, and the status/kind tally
/// keys alike — because nothing constrains a hand-authored frontmatter
/// scalar (a double-quoted `id`/`status`/`kind` carrying `\n`) to a
/// single line, and the report renders straight from the graph (before
/// any `check`), so it must be robust to whatever the graph holds.
fn inline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render a deterministic GRAPH.md report.
pub fn render_markdown(graph: &Graph, config: &Config) -> String {
    let mut out = String::new();

    // Title
    writeln!(out, "# {}", inline(&config.report.title)).unwrap();
    writeln!(out).unwrap();

    // Summary
    render_summary(&mut out, graph, config);

    // God nodes
    render_god_nodes(&mut out, graph, config);

    // Supersession chains
    render_chains(&mut out, graph, config);

    // Orphans
    render_orphans(&mut out, graph, config);

    // Stale
    render_stale(&mut out, graph, config);

    // Generation hash
    let hash = compute_generation_hash(&out);
    writeln!(out, "---").unwrap();
    writeln!(out, "generation_id: {hash}").unwrap();

    out
}

fn render_summary(out: &mut String, graph: &Graph, _config: &Config) {
    writeln!(out, "## Summary").unwrap();
    writeln!(out).unwrap();
    writeln!(
        out,
        "**{} nodes** · **{} edges**",
        graph.node_count(),
        graph.edge_count()
    )
    .unwrap();
    writeln!(out).unwrap();

    // Per-status and per-kind distributions, omitted entirely when the
    // tally is empty so an empty-graph report doesn't print bare keys.
    let status_counts = tally(graph, |n| n.status.as_str());
    if !status_counts.is_empty() {
        writeln!(out, "Status: {}", format_tally(&status_counts)).unwrap();
    }
    let kind_counts = tally(graph, |n| n.kind.as_str());
    if !kind_counts.is_empty() {
        writeln!(out, "Kind: {}", format_tally(&kind_counts)).unwrap();
    }
    writeln!(out).unwrap();
}

/// Count nodes by the category returned from `key`.
fn tally<'a, F>(graph: &'a Graph, key: F) -> BTreeMap<&'a str, usize>
where
    F: Fn(&'a crate::model::Node) -> &'a str,
{
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for node in graph.nodes().values() {
        *counts.entry(key(node)).or_default() += 1;
    }
    counts
}

fn format_tally(counts: &BTreeMap<&str, usize>) -> String {
    counts
        .iter()
        // The key is a status/kind value — `inline` it like every other
        // interpolated field, so a newline-bearing hand-authored status
        // (`status: "active\n## heading"`) can't inject structure into the
        // Summary tally.
        .map(|(k, v)| format!("{}={v}", inline(k)))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn render_god_nodes(out: &mut String, graph: &Graph, config: &Config) {
    writeln!(
        out,
        "## God Nodes (top-{} by backlinks)",
        config.report.god_node_display_limit
    )
    .unwrap();
    writeln!(out).unwrap();

    let mut backlink_counts: Vec<(&str, usize)> = graph
        .nodes()
        .keys()
        .filter(|id| {
            graph
                .node(id)
                .map(|n| !config.is_terminal(n.status.as_str()))
                .unwrap_or(false)
        })
        // "Backlinks" here is the same external-attention measure
        // `query backlinks`, the trust `backlinks_score`, and orphan
        // detection use — `external_incoming_edges` excludes self-loops,
        // so a self-referencing node can't inflate its own god-node rank
        // (and a node `query orphans` calls an orphan can't also appear
        // here with a phantom backlink).
        .map(|id| (id.as_str(), graph.external_incoming_edges(id).len()))
        .filter(|(_, count)| *count > 0)
        .collect();

    backlink_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    for (id, count) in backlink_counts
        .iter()
        .take(config.report.god_node_display_limit)
    {
        let title = graph
            .node(id)
            .map(|n| inline(&n.title))
            .unwrap_or_else(|| inline(id));
        writeln!(out, "- **{}** ({count} backlinks) — {title}", inline(id)).unwrap();
    }

    if backlink_counts.is_empty() {
        writeln!(out, "_None_").unwrap();
    }
    writeln!(out).unwrap();
}

fn render_chains(out: &mut String, graph: &Graph, config: &Config) {
    writeln!(out, "## Supersession Chains").unwrap();
    writeln!(out).unwrap();

    // `find_chain` returns the whole connected supersession component from
    // any member, ordered identically regardless of which member anchors
    // it. So we visit node ids in order and render each component exactly
    // once: the lex-smallest member emits it, and every other member is
    // skipped via `seen`. This is authoring-agnostic — a component
    // declared purely through `supersedes:` (no `superseded_by` tail)
    // still appears — and never duplicates a component reachable from
    // more than one root.
    let mut ids: Vec<String> = graph.nodes().keys().cloned().collect();
    ids.sort();

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut rendered_any = false;
    for id in &ids {
        if seen.contains(id) {
            continue;
        }
        let chain = crate::query::traverse::find_chain(graph, id);
        for entry in &chain {
            seen.insert(entry.node.id.clone());
        }
        if chain.len() < 2 {
            continue;
        }
        // Terminal (superseded) nodes are struck-through, live ones bold.
        // Terminality is config-driven (`statuses.terminal`), not a fixed
        // "active" vocabulary — a project that uses "live" or "current"
        // still renders correctly.
        let parts: Vec<String> = chain
            .iter()
            .map(|c| {
                if config.is_terminal(&c.node.status) {
                    format!("~~{}~~", inline(&c.node.id))
                } else {
                    format!("**{}**", inline(&c.node.id))
                }
            })
            .collect();
        writeln!(out, "- {}", parts.join(" → ")).unwrap();
        rendered_any = true;
    }

    if !rendered_any {
        writeln!(out, "_None_").unwrap();
    }
    writeln!(out).unwrap();
}

fn render_orphans(out: &mut String, graph: &Graph, config: &Config) {
    writeln!(out, "## Orphans").unwrap();
    writeln!(out).unwrap();

    let orphans = crate::query::detect::find_orphans(graph, config);

    if orphans.is_empty() {
        writeln!(out, "_None_").unwrap();
    } else {
        for orphan in orphans.iter().take(config.report.orphan_display_limit) {
            writeln!(
                out,
                "- {} ({}) — {}",
                inline(&orphan.node.id),
                inline(orphan.node.kind.as_str()),
                inline(&orphan.node.path)
            )
            .unwrap();
        }
        if orphans.len() > config.report.orphan_display_limit {
            writeln!(
                out,
                "- _...and {} more_",
                orphans.len() - config.report.orphan_display_limit
            )
            .unwrap();
        }
    }
    writeln!(out).unwrap();
}

fn render_stale(out: &mut String, graph: &Graph, config: &Config) {
    writeln!(out, "## Stale").unwrap();
    writeln!(out).unwrap();

    let stale = crate::query::detect::find_stale(graph, config);

    if stale.is_empty() {
        writeln!(out, "_None_").unwrap();
    } else {
        for entry in stale.iter().take(config.report.stale_display_limit) {
            writeln!(
                out,
                "- {} — reviewed {} ({} days ago)",
                inline(&entry.node.id),
                entry.reviewed,
                entry.days_since
            )
            .unwrap();
        }
        if stale.len() > config.report.stale_display_limit {
            writeln!(
                out,
                "- _...and {} more_",
                stale.len() - config.report.stale_display_limit
            )
            .unwrap();
        }
    }
    writeln!(out).unwrap();
}

fn compute_generation_hash(content: &str) -> String {
    hash::sha256_hex(content)[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::{inline, render_chains};
    use crate::config::Config;
    use crate::model::{Edge, Graph, GraphMeta, Kind, Node, ResolvedTarget, Status};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn inline_collapses_newlines_so_a_title_cannot_inject_structure() {
        // A multi-line title must not break the report — a newline-borne
        // `## heading` is flattened onto the entry line instead of
        // starting a real heading.
        assert_eq!(inline("a\n## Heading"), "a ## Heading");
        assert_eq!(inline("  spaced   out \n line "), "spaced out line");
        assert_eq!(inline("plain title"), "plain title");
        assert_eq!(inline(""), "");
    }

    fn node(id: &str, superseded_by: Option<&str>) -> Node {
        Node {
            id: id.into(),
            path: PathBuf::from(format!("{id}.md")),
            title: id.into(),
            kind: Kind::new("generic"),
            status: Status::new("active"),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: superseded_by.map(String::from),
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

    fn supersedes_edge(newer: &str, older: &str) -> Edge {
        Edge {
            source: newer.into(),
            target: ResolvedTarget::resolved(older),
            relation: "supersedes".into(),
            location: "frontmatter:supersedes".into(),
        }
    }

    fn graph(nodes: Vec<Node>, edges: Vec<Edge>) -> Graph {
        let mut map = IndexMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph::new(map, edges, vec![], vec![], vec![], GraphMeta::default())
    }

    /// The bullet lines of a rendered "Supersession Chains" section.
    fn chain_lines(out: &str) -> Vec<&str> {
        out.lines().filter(|l| l.starts_with("- ")).collect()
    }

    #[test]
    fn chains_render_a_supersedes_only_component_once_with_every_member() {
        // `x supersedes [a, b]`, authored purely as supersedes edges — no
        // `superseded_by` tail on a/b. The old tail heuristic found no
        // start node and silently omitted the whole component; the
        // component model renders it once, with all three members.
        let g = graph(
            vec![node("a", None), node("b", None), node("x", None)],
            vec![supersedes_edge("x", "a"), supersedes_edge("x", "b")],
        );
        let mut out = String::new();
        render_chains(&mut out, &g, &Config::default());
        let lines = chain_lines(&out);
        assert_eq!(lines.len(), 1, "exactly one component line: {out}");
        for id in ["a", "b", "x"] {
            assert!(lines[0].contains(id), "member {id} missing: {out}");
        }
        assert!(!out.contains("_None_"));
    }

    #[test]
    fn chains_do_not_duplicate_a_multi_root_component() {
        // Two tails (`a`, `b`) both author `superseded_by: x`. Under the
        // component model `find_chain` returns the same component from
        // either, so the report renders it exactly once, not twice.
        let g = graph(
            vec![node("a", Some("x")), node("b", Some("x")), node("x", None)],
            vec![supersedes_edge("x", "a"), supersedes_edge("x", "b")],
        );
        let mut out = String::new();
        render_chains(&mut out, &g, &Config::default());
        assert_eq!(chain_lines(&out).len(), 1, "no duplicate component: {out}");
    }

    #[test]
    fn chains_render_none_when_no_supersession_exists() {
        let g = graph(vec![node("solo", None)], vec![]);
        let mut out = String::new();
        render_chains(&mut out, &g, &Config::default());
        assert!(out.contains("_None_"), "no chains → _None_: {out}");
        assert!(chain_lines(&out).is_empty());
    }
}
