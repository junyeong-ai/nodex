use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::Regex;
use std::sync::OnceLock;

use crate::config::ParserConfig;
use crate::model::RawEdge;

/// Extract links from markdown body. Standard markdown links come from
/// pulldown-cmark's token stream; wikilinks (when enabled) and custom
/// regex patterns apply only to lines whose entire byte span lies
/// outside any code block, using pulldown-cmark's authoritative ranges
/// rather than a string-level fence sniff.
pub fn extract_links(body: &str, config: &ParserConfig) -> Vec<RawEdge> {
    let mut edges = Vec::new();
    let compiled_patterns = compile_patterns(&config.link_patterns);
    let line_offsets = compute_line_offsets(body);

    let parser = Parser::new_ext(body, Options::empty());
    let mut code_ranges: Vec<(usize, usize)> = Vec::new();
    let mut code_open: Option<usize> = None;

    for (event, range) in parser.into_offset_iter() {
        match &event {
            Event::Start(Tag::CodeBlock(_)) => {
                code_open = Some(range.start);
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_open.take() {
                    code_ranges.push((start, range.end));
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                if let Some(raw_edge) = process_link_target(
                    dest_url,
                    line_for_offset(&line_offsets, range.start),
                    &config.extensions,
                ) {
                    edges.push(raw_edge);
                }
            }
            _ => {}
        }
    }

    let needs_line_pass = config.wikilink_enabled || !compiled_patterns.is_empty();
    if needs_line_pass {
        let wikilink_re = config.wikilink_enabled.then(wikilink_regex);

        for (i, line) in body.lines().enumerate() {
            let line_start = line_offsets[i];
            let line_end = line_start + line.len();
            if code_ranges
                .iter()
                .any(|&(s, e)| line_start >= s && line_end <= e)
            {
                continue;
            }

            if let Some(re) = wikilink_re {
                for caps in re.captures_iter(line) {
                    let target = caps
                        .get(1)
                        .map(|m| m.as_str().trim().to_string())
                        .unwrap_or_default();
                    if target.is_empty() {
                        continue;
                    }
                    edges.push(RawEdge {
                        target_path: target,
                        relation: "references".to_string(),
                        location: format!("L{}", i + 1),
                    });
                }
            }

            for (regex, relation) in &compiled_patterns {
                if let Some(caps) = regex.captures(line)
                    && let Some(m) = caps.get(1)
                {
                    edges.push(RawEdge {
                        target_path: m.as_str().trim().to_string(),
                        relation: relation.clone(),
                        location: format!("L{}", i + 1),
                    });
                }
            }
        }
    }

    edges
}

fn process_link_target(dest: &str, line_num: usize, extensions: &[String]) -> Option<RawEdge> {
    let dest = dest.trim();
    if dest.starts_with("http://")
        || dest.starts_with("https://")
        || dest.starts_with("mailto:")
        || dest.starts_with('#')
        || dest.is_empty()
    {
        return None;
    }
    let path = dest.split('#').next().unwrap_or(dest);
    if !extensions.iter().any(|ext| path.ends_with(ext)) {
        return None;
    }
    let normalized = path.strip_prefix("./").unwrap_or(path);
    Some(RawEdge {
        target_path: normalized.to_string(),
        relation: "references".to_string(),
        location: format!("L{line_num}"),
    })
}

fn compile_patterns(patterns: &[crate::config::LinkPattern]) -> Vec<(Regex, String)> {
    patterns
        .iter()
        .map(|p| {
            let regex =
                Regex::new(&p.pattern).expect("link patterns are validated by Config::load");
            (regex, p.relation.clone())
        })
        .collect()
}

/// `[[<target>]]` or `[[<target>|<display>]]`. Compiled once per process.
fn wikilink_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\[\[([^\[\]|\n]+?)(?:\|[^\]\n]*)?\]\]").expect("static regex compiles")
    })
}

/// Byte offsets where each line begins.
fn compute_line_offsets(body: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (i, b) in body.bytes().enumerate() {
        if b == b'\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

/// 1-indexed line number for `byte_offset`. O(log n).
fn line_for_offset(line_offsets: &[usize], byte_offset: usize) -> usize {
    match line_offsets.binary_search(&byte_offset) {
        Ok(idx) => idx + 1,
        Err(idx) => idx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LinkPattern;

    fn cfg() -> ParserConfig {
        ParserConfig::default()
    }

    fn cfg_with_patterns(patterns: Vec<LinkPattern>) -> ParserConfig {
        ParserConfig {
            link_patterns: patterns,
            ..ParserConfig::default()
        }
    }

    fn cfg_with_wikilinks() -> ParserConfig {
        ParserConfig {
            wikilink_enabled: true,
            ..ParserConfig::default()
        }
    }

    #[test]
    fn extract_markdown_links() {
        let body = "See [ADR 1](docs/decisions/0001-auth.md) for details.\n\
                     Also [external](https://example.com).";
        let edges = extract_links(body, &cfg());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "docs/decisions/0001-auth.md");
        assert_eq!(edges[0].relation, "references");
    }

    #[test]
    fn skip_links_in_fenced_code_blocks() {
        let body = "```\n[not a link](fake.md)\n```\n\n[real](real.md)";
        let edges = extract_links(body, &cfg());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real.md");
    }

    #[test]
    fn skip_links_in_indented_code_blocks() {
        let body = "    [not a link](fake.md)\n\n[real](real.md)";
        let edges = extract_links(body, &cfg());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real.md");
    }

    #[test]
    fn skip_links_in_tilde_fence() {
        let body = "~~~\n[not](fake.md)\n~~~\n\n[real](real.md)";
        let edges = extract_links(body, &cfg());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real.md");
    }

    #[test]
    fn skip_links_in_four_backtick_fence() {
        let body = "````\n```\n[not](fake.md)\n```\n````\n\n[real](real.md)";
        let edges = extract_links(body, &cfg());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real.md");
    }

    #[test]
    fn strip_anchor_fragment() {
        let body = "[link](docs/guide.md#section-3)";
        let edges = extract_links(body, &cfg());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "docs/guide.md");
    }

    #[test]
    fn custom_import_pattern() {
        let body = "@import scripts/docs/parse.py\n\nSome text.";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"^@import\s+(.+?)\s*$".to_string(),
                relation: "imports".to_string(),
            }]),
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "scripts/docs/parse.py");
        assert_eq!(edges[0].relation, "imports");
    }

    #[test]
    fn custom_pattern_skipped_in_code_block() {
        let body = "```\n@import not/real.py\n```\n\n@import real/file.py";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"^@import\s+(.+?)\s*$".to_string(),
                relation: "imports".to_string(),
            }]),
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real/file.py");
    }

    #[test]
    fn normalize_leading_dot_slash() {
        let body = "[link](./relative/path.md)";
        let edges = extract_links(body, &cfg());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "relative/path.md");
    }

    #[test]
    fn skip_non_listed_extensions() {
        let body = "[img](picture.png)\n[doc](file.md)";
        let edges = extract_links(body, &cfg());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "file.md");
    }

    #[test]
    fn additional_extensions_picked_up_from_config() {
        let body = "[a](one.md)\n[b](two.markdown)\n[c](three.txt)";
        let cfg = ParserConfig {
            extensions: vec![".md".into(), ".markdown".into()],
            ..ParserConfig::default()
        };
        let edges = extract_links(body, &cfg);
        let targets: Vec<&str> = edges.iter().map(|e| e.target_path.as_str()).collect();
        assert_eq!(targets, vec!["one.md", "two.markdown"]);
    }

    #[test]
    fn wikilinks_emit_references_when_enabled() {
        let body = "Refers to [[adr-001]] and [[guides/intro|see intro]].";
        let edges = extract_links(body, &cfg_with_wikilinks());
        let targets: Vec<&str> = edges.iter().map(|e| e.target_path.as_str()).collect();
        assert_eq!(targets, vec!["adr-001", "guides/intro"]);
        assert!(edges.iter().all(|e| e.relation == "references"));
    }

    #[test]
    fn wikilinks_off_by_default() {
        let body = "[[adr-001]]";
        let edges = extract_links(body, &cfg());
        assert!(edges.is_empty());
    }

    #[test]
    fn wikilinks_skipped_in_code_block() {
        let body = "```\n[[fake]]\n```\n\n[[real]]";
        let edges = extract_links(body, &cfg_with_wikilinks());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real");
    }

    #[test]
    fn line_offsets_sentinel() {
        let offsets = compute_line_offsets("a\nbb\n\nccc");
        assert_eq!(offsets, vec![0, 2, 5, 6]);
        assert_eq!(line_for_offset(&offsets, 0), 1);
        assert_eq!(line_for_offset(&offsets, 2), 2);
        assert_eq!(line_for_offset(&offsets, 7), 4);
    }
}
