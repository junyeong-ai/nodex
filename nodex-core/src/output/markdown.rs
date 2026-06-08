use std::collections::BTreeMap;
use std::fmt::Write;

use crate::config::Config;
use crate::hash;
use crate::model::Graph;

/// Collapse a free-text field (a node or report title) to a single line
/// for safe interpolation into the report: any whitespace run — crucially
/// a newline — becomes one space, so a multi-line title cannot inject a
/// heading or list item and break the report's structure. Node ids,
/// kinds, and paths are constrained vocabularies and need no such
/// treatment.
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
        .map(|(k, v)| format!("{k}={v}"))
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
        .map(|id| (id.as_str(), graph.incoming_indices(id).len()))
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

    // Walk from each chain tail (a node that is superseded but doesn't
    // itself supersede anything). `find_chain` follows the successor
    // chain forward, so starting from tails visits the full chain
    // exactly once per chain.
    let mut chain_starts: Vec<&str> = graph
        .nodes()
        .values()
        .filter(|n| n.superseded_by.is_some() && n.supersedes.is_empty())
        .map(|n| n.id.as_str())
        .collect();
    chain_starts.sort();

    if chain_starts.is_empty() {
        writeln!(out, "_None_").unwrap();
    }

    // Highlight non-terminal nodes in bold and terminal ones struck-
    // through. Terminality is config-driven (`statuses.terminal`), not a
    // fixed "active" vocabulary — a project that uses "live" or
    // "current" still renders correctly.
    for start in &chain_starts {
        let chain = crate::query::traverse::find_chain(graph, start);
        if chain.len() > 1 {
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
        }
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
    use super::inline;

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
}
