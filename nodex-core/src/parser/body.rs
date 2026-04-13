use pulldown_cmark::{Event, Options, Parser, Tag};
use regex::Regex;

use crate::config::LinkPattern;
use crate::model::{Confidence, RawEdge};

/// Extract links from markdown body using pulldown-cmark (no regex heuristics).
/// Also applies custom link patterns to non-code text regions.
pub fn extract_links(body: &str, custom_patterns: &[LinkPattern]) -> Vec<RawEdge> {
    let mut edges = Vec::new();
    let compiled_patterns = compile_patterns(custom_patterns);

    // Track code block depth to skip custom patterns inside code.
    let mut in_code_block = false;
    let mut code_block_lines: Vec<(usize, usize)> = Vec::new();
    let mut current_line = 0;

    // First pass: identify code block line ranges.
    for (i, line) in body.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            if in_code_block {
                code_block_lines.push((current_line, i));
                in_code_block = false;
            } else {
                current_line = i;
                in_code_block = true;
            }
        }
    }

    // Second pass: pulldown-cmark for standard markdown links.
    let opts = Options::empty();
    let parser = Parser::new_ext(body, opts);

    for (event, range) in parser.into_offset_iter() {
        let line_num = body[..range.start].matches('\n').count() + 1;

        if let Event::Start(Tag::Link { dest_url, .. }) = &event
            && let Some(raw_edge) = process_link_target(dest_url, line_num)
        {
            edges.push(raw_edge);
        }
    }

    // Third pass: custom link patterns on non-code lines only.
    for (i, line) in body.lines().enumerate() {
        let in_code = code_block_lines
            .iter()
            .any(|&(start, end)| i >= start && i <= end);
        if in_code {
            continue;
        }

        for (regex, relation) in &compiled_patterns {
            if let Some(caps) = regex.captures(line)
                && let Some(m) = caps.get(1)
            {
                edges.push(RawEdge {
                    target_path: m.as_str().trim().to_string(),
                    relation: relation.clone(),
                    confidence: Confidence::Extracted,
                    location: format!("L{}", i + 1),
                });
            }
        }
    }

    edges
}

fn process_link_target(dest: &str, line_num: usize) -> Option<RawEdge> {
    let dest = dest.trim();

    // Skip external URLs, mailto, anchors
    if dest.starts_with("http://")
        || dest.starts_with("https://")
        || dest.starts_with("mailto:")
        || dest.starts_with('#')
        || dest.is_empty()
    {
        return None;
    }

    // Strip anchor fragment
    let path = dest.split('#').next().unwrap_or(dest);

    // Only process markdown files
    if !path.ends_with(".md") {
        return None;
    }

    // Normalize: strip leading ./
    let normalized = path.strip_prefix("./").unwrap_or(path);

    Some(RawEdge {
        target_path: normalized.to_string(),
        relation: "references".to_string(),
        confidence: Confidence::Extracted,
        location: format!("L{line_num}"),
    })
}

fn compile_patterns(patterns: &[LinkPattern]) -> Vec<(Regex, String)> {
    patterns
        .iter()
        .filter_map(|p| match Regex::new(&p.pattern) {
            Ok(r) => Some((r, p.relation.clone())),
            Err(e) => {
                eprintln!("warning: invalid link pattern {:?}: {e}", p.pattern);
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_markdown_links() {
        let body = "See [ADR 1](docs/decisions/0001-auth.md) for details.\n\
                     Also [external](https://example.com).";
        let edges = extract_links(body, &[]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "docs/decisions/0001-auth.md");
        assert_eq!(edges[0].relation, "references");
    }

    #[test]
    fn skip_links_in_code_blocks() {
        let body = "```\n[not a link](fake.md)\n```\n\n[real](real.md)";
        let edges = extract_links(body, &[]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real.md");
    }

    #[test]
    fn strip_anchor_fragment() {
        let body = "[link](docs/guide.md#section-3)";
        let edges = extract_links(body, &[]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "docs/guide.md");
    }

    #[test]
    fn custom_import_pattern() {
        let patterns = vec![LinkPattern {
            pattern: r"^@import\s+(.+?)\s*$".to_string(),
            relation: "imports".to_string(),
        }];
        let body = "@import scripts/docs/parse.py\n\nSome text.";
        let edges = extract_links(body, &patterns);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "scripts/docs/parse.py");
        assert_eq!(edges[0].relation, "imports");
    }

    #[test]
    fn custom_pattern_skipped_in_code_block() {
        let patterns = vec![LinkPattern {
            pattern: r"^@import\s+(.+?)\s*$".to_string(),
            relation: "imports".to_string(),
        }];
        let body = "```\n@import not/real.py\n```\n\n@import real/file.py";
        let edges = extract_links(body, &patterns);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real/file.py");
    }

    #[test]
    fn normalize_leading_dot_slash() {
        let body = "[link](./relative/path.md)";
        let edges = extract_links(body, &[]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "relative/path.md");
    }

    #[test]
    fn skip_non_markdown() {
        let body = "[img](picture.png)\n[doc](file.md)";
        let edges = extract_links(body, &[]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "file.md");
    }
}
