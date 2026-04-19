use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write;

use crate::config::Config;
use crate::model::Graph;

/// Render a deterministic GRAPH.md report.
pub fn render_markdown(graph: &Graph, config: &Config) -> String {
    let mut out = String::new();

    // Title
    writeln!(out, "# {}", config.report.title).unwrap();
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

    // Status counts
    let mut status_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for node in graph.nodes().values() {
        *status_counts.entry(node.status.as_str()).or_default() += 1;
    }
    let status_parts: Vec<String> = status_counts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    writeln!(out, "Status: {}", status_parts.join(" · ")).unwrap();

    // Kind counts
    let mut kind_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for node in graph.nodes().values() {
        *kind_counts.entry(node.kind.as_str()).or_default() += 1;
    }
    let kind_parts: Vec<String> = kind_counts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    writeln!(out, "Kind: {}", kind_parts.join(" · ")).unwrap();
    writeln!(out).unwrap();
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
        let title = graph.node(id).map(|n| n.title.as_str()).unwrap_or(id);
        writeln!(out, "- **{id}** ({count} backlinks) — {title}").unwrap();
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
                    if config.is_terminal(c.status.as_str()) {
                        format!("~~{}~~", c.id)
                    } else {
                        format!("**{}**", c.id)
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
            writeln!(out, "- {} ({}) — {}", orphan.id, orphan.kind, orphan.path).unwrap();
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
                entry.id, entry.reviewed, entry.days_since
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
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let hex: String = hasher.finalize().iter().fold(String::new(), |mut acc, b| {
        Write::write_fmt(&mut acc, format_args!("{b:02x}")).unwrap();
        acc
    });
    hex[..16].to_string()
}
