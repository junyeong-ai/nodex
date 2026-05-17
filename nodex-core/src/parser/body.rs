use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::Regex;
use std::sync::OnceLock;

use std::collections::BTreeMap;

use crate::config::{AnnotationConfig, BodyBlockRuleConfig, BodyLineRuleConfig, ParserConfig};
use crate::model::{RawAnnotation, RawBodyBlockMatch, RawBodyLineMatch, RawEdge};

/// One markdown body line that is **not** inside a fenced or indented
/// code block, surfaced with its 1-based line number. Returned by
/// [`iter_body_lines`]; consumed by every body-level scanner (link
/// extraction, body-line rules, annotation queries) so each surface
/// applies the same fence-aware filter without re-implementing it.
#[derive(Debug, Clone)]
pub struct BodyLine<'a> {
    pub number: usize,
    pub text: &'a str,
}

/// Yield every body line outside a code block. Indented and fenced
/// (` ``` ` / `~~~`) blocks are recognised via pulldown-cmark's
/// offset stream so the classification matches the markdown spec
/// exactly — a fence-sniff string scan would mis-handle nested fences
/// (e.g. ```` ```` ` ``` ` `````` wrapping ``` ```` `).
pub fn iter_body_lines(body: &str) -> Vec<BodyLine<'_>> {
    let code_ranges = collect_code_block_ranges(body);
    let line_offsets = compute_line_offsets(body);
    body.lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            if line_in_code_range(&line_offsets, &code_ranges, idx, line.len()) {
                None
            } else {
                Some(BodyLine {
                    number: idx + 1,
                    text: line,
                })
            }
        })
        .collect()
}

/// Extract links from markdown body. Standard markdown links come from
/// pulldown-cmark's token stream (naturally fence-aware — the parser
/// never emits `Tag::Link` inside a `CodeBlock`); wikilinks (when
/// enabled) and custom regex patterns apply only to lines whose
/// entire byte span lies outside any code block, with code-block
/// detection delegated to the shared [`collect_code_block_ranges`].
pub fn extract_links(body: &str, config: &ParserConfig) -> Vec<RawEdge> {
    let mut edges = Vec::new();
    let line_offsets = compute_line_offsets(body);

    // Pass 1: standard markdown links via pulldown-cmark. Naturally
    // fence-aware — `Tag::Link` events never fire inside a code block.
    for (event, range) in Parser::new_ext(body, Options::empty()).into_offset_iter() {
        if let Event::Start(Tag::Link { dest_url, .. }) = &event
            && let Some(raw_edge) = process_link_target(
                dest_url,
                line_for_offset(&line_offsets, range.start),
                &config.extensions,
            )
        {
            edges.push(raw_edge);
        }
    }

    // Pass 2 (only when wikilink / custom patterns are configured):
    // line-by-line regex scan, skipping lines inside code blocks.
    // `collect_code_block_ranges` is the single source of truth for
    // what counts as a code block — shared with `iter_body_lines` so
    // pulldown-cmark spec changes update every scanner in one place.
    let compiled_patterns = compile_patterns(&config.link_patterns);
    let needs_line_pass = config.wikilink_enabled || !compiled_patterns.is_empty();
    if needs_line_pass {
        let code_ranges = collect_code_block_ranges(body);
        let wikilink_re = config.wikilink_enabled.then(wikilink_regex);

        for (idx, line) in body.lines().enumerate() {
            if line_in_code_range(&line_offsets, &code_ranges, idx, line.len()) {
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
                        location: format!("L{}", idx + 1),
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
                        location: format!("L{}", idx + 1),
                    });
                }
            }
        }
    }

    edges
}

/// Extract config-declared body annotations. Code blocks are skipped
/// via [`iter_body_lines`]; every regex match contributes one
/// [`RawAnnotation`] whose `key` is the value of the configured named
/// capture. Pattern validity and the existence of `key` among the
/// pattern's named captures are guaranteed by `Config::validate`, so
/// compilation here cannot fail.
pub fn extract_annotations(body: &str, annotations: &[AnnotationConfig]) -> Vec<RawAnnotation> {
    if annotations.is_empty() {
        return Vec::new();
    }
    let compiled: Vec<(&AnnotationConfig, Regex)> = annotations
        .iter()
        .map(|a| {
            let re =
                Regex::new(&a.pattern).expect("annotation patterns are validated by Config::load");
            (a, re)
        })
        .collect();

    let mut out = Vec::new();
    for body_line in iter_body_lines(body) {
        for (cfg, re) in &compiled {
            for caps in re.captures_iter(body_line.text) {
                if let Some(m) = caps.name(&cfg.key) {
                    out.push(RawAnnotation {
                        pattern_name: cfg.name.clone(),
                        key: m.as_str().to_string(),
                        line: body_line.number,
                    });
                }
            }
        }
    }
    out
}

/// Extract config-declared body-block matches. One state machine
/// per `[[rules.body_block]]` block runs over the same fence-aware
/// line iterator every other body scanner uses, so a fence-shape
/// change in pulldown-cmark updates every extractor in one place.
///
/// State machine, per block:
///
/// - `Outside`: each non-code line is matched against `start_pattern`.
///   On match → `Inside { start_line, captures }`.
/// - `Inside`: each subsequent non-code line is first matched against
///   `start_pattern` (a sibling section closes the previous span and
///   opens a new one); failing that, against `end_pattern` (closes
///   the span with `end_line = this_line - 1`).
/// - End of body: any still-open span is closed with `end_line = the
///   last non-code body line's number`. Empty spans (header alone,
///   end_pattern matching on the very next line) emit with
///   `end_line == start_line` so consumers see "this header had no
///   content lines" rather than an invalid range.
///
/// The match's `captures` come from the *start* line's regex match.
/// Enum validation is a check-time concern owned by `BodyBlockRule`;
/// this pass is pure framing and stays enum-agnostic so a config-only
/// enum change does not force re-extraction.
pub fn extract_body_block_matches(
    body: &str,
    blocks: &[BodyBlockRuleConfig],
) -> Vec<RawBodyBlockMatch> {
    if blocks.is_empty() {
        return Vec::new();
    }
    let compiled: Vec<(&BodyBlockRuleConfig, Regex, Regex)> = blocks
        .iter()
        .map(|b| {
            let start = Regex::new(&b.start_pattern)
                .expect("body_block start_pattern validated by Config::load");
            let end = Regex::new(&b.end_pattern)
                .expect("body_block end_pattern validated by Config::load");
            (b, start, end)
        })
        .collect();

    let lines = iter_body_lines(body);
    let last_line_number = lines.last().map(|l| l.number).unwrap_or(0);

    let mut out = Vec::new();
    // One independent state machine per block. Sibling blocks may
    // overlap freely — the rule semantic is per-block, so a doc with
    // an outer "## Decision … ## End" block and an inner
    // "### Step … ### End" block produces matches for both rules
    // without one interfering with the other.
    for (cfg, start_re, end_re) in &compiled {
        let mut open: Option<(usize, BTreeMap<String, String>)> = None;
        for line in &lines {
            // While inside a span, a fresh start_pattern match closes
            // the previous span at the *previous* line. Checking
            // start *before* end here is what lets `end_pattern`
            // include `start_pattern` (e.g. `^## ` matching both
            // sibling section headers) without missing the new
            // section's start.
            if let Some(start_captures) = capture_named(start_re, line.text) {
                if let Some((open_start, open_captures)) = open.take() {
                    out.push(RawBodyBlockMatch {
                        rule_name: cfg.name.clone(),
                        start_line: open_start,
                        end_line: line.number.saturating_sub(1).max(open_start),
                        captures: open_captures,
                    });
                }
                open = Some((line.number, start_captures));
                continue;
            }
            if open.is_some() && end_re.is_match(line.text) {
                let (open_start, open_captures) = open.take().expect("open is Some");
                out.push(RawBodyBlockMatch {
                    rule_name: cfg.name.clone(),
                    start_line: open_start,
                    end_line: line.number.saturating_sub(1).max(open_start),
                    captures: open_captures,
                });
            }
        }
        // End-of-body closes any still-open span. `end_line` is the
        // last non-code body line we observed, which is the honest
        // boundary — code-block lines aren't part of the body the
        // rule reasons about.
        if let Some((open_start, open_captures)) = open {
            out.push(RawBodyBlockMatch {
                rule_name: cfg.name.clone(),
                start_line: open_start,
                end_line: last_line_number.max(open_start),
                captures: open_captures,
            });
        }
    }
    out
}

/// Pull every named capture from the first match of `re` against
/// `line`. Returns `None` when the pattern doesn't match; returns
/// `Some(map)` even when the pattern has no named captures (the map
/// is just empty) so callers can distinguish "no match" from "match
/// without captures". Centralised so body_line and body_block share
/// one capture-extraction convention.
fn capture_named(re: &Regex, line: &str) -> Option<BTreeMap<String, String>> {
    let caps = re.captures(line)?;
    Some(
        re.capture_names()
            .flatten()
            .filter_map(|name| {
                caps.name(name)
                    .map(|m| (name.to_string(), m.as_str().to_string()))
            })
            .collect(),
    )
}

/// Extract config-declared body-line regex matches. Every named
/// capture of every match outside a code block is recorded under
/// the corresponding `[[rules.body_line]]` block. Enum validation
/// happens later in `BodyLineRule::check`; this pass is pure
/// pattern extraction so a config-only enum change does not force
/// a re-extraction (cache invalidates on full config_hash, but the
/// data stored here is enum-agnostic and could survive any future
/// finer-grained cache key without re-parsing the body).
pub fn extract_body_line_matches(
    body: &str,
    blocks: &[BodyLineRuleConfig],
) -> Vec<RawBodyLineMatch> {
    if blocks.is_empty() {
        return Vec::new();
    }
    let compiled: Vec<(&BodyLineRuleConfig, Regex)> = blocks
        .iter()
        .map(|b| {
            let re =
                Regex::new(&b.pattern).expect("body_line patterns are validated by Config::load");
            (b, re)
        })
        .collect();

    let mut out = Vec::new();
    for body_line in iter_body_lines(body) {
        for (cfg, re) in &compiled {
            for caps in re.captures_iter(body_line.text) {
                let captures: std::collections::BTreeMap<String, String> = re
                    .capture_names()
                    .flatten()
                    .filter_map(|name| {
                        caps.name(name)
                            .map(|m| (name.to_string(), m.as_str().to_string()))
                    })
                    .collect();
                if captures.is_empty() {
                    // Pattern with no named captures yields no useful
                    // data for enum validation — skip rather than store
                    // an inert match record.
                    continue;
                }
                out.push(RawBodyLineMatch {
                    rule_name: cfg.name.clone(),
                    line: body_line.number,
                    captures,
                });
            }
        }
    }
    out
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

/// Every `(start, end)` byte range that is the span of a code block
/// (fenced or indented) per pulldown-cmark's classification.
fn collect_code_block_ranges(body: &str) -> Vec<(usize, usize)> {
    let parser = Parser::new_ext(body, Options::empty());
    let mut ranges = Vec::new();
    let mut open: Option<usize> = None;
    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => open = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = open.take() {
                    ranges.push((start, range.end));
                }
            }
            _ => {}
        }
    }
    ranges
}

/// True when any byte of the 0-indexed body line falls inside any
/// of the supplied code-block ranges. Overlap (not strict
/// containment) is the right test because markdown code blocks are
/// line-aligned by spec — pulldown-cmark's fenced ranges fully
/// contain their lines, but the indented-block range can begin
/// after a line's leading whitespace, which a strict containment
/// check would mis-classify as prose.
fn line_in_code_range(
    line_offsets: &[usize],
    code_ranges: &[(usize, usize)],
    line_idx: usize,
    line_len: usize,
) -> bool {
    let line_start = line_offsets[line_idx];
    let line_end = line_start + line_len;
    code_ranges
        .iter()
        .any(|&(s, e)| line_start < e && line_end > s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApplyTo;
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

    // ─── iter_body_lines ───────────────────────────────────────────────

    #[test]
    fn iter_body_lines_emits_every_non_code_line() {
        let body = "first\nsecond\n\nfourth";
        let lines: Vec<_> = iter_body_lines(body);
        let pairs: Vec<(usize, &str)> = lines.iter().map(|l| (l.number, l.text)).collect();
        assert_eq!(
            pairs,
            vec![(1, "first"), (2, "second"), (3, ""), (4, "fourth")]
        );
    }

    #[test]
    fn iter_body_lines_skips_fenced_block() {
        let body = "before\n```\nin_fence\n```\nafter";
        let lines: Vec<_> = iter_body_lines(body);
        let texts: Vec<&str> = lines.iter().map(|l| l.text).collect();
        assert!(!texts.iter().any(|t| t.contains("in_fence")));
        assert!(texts.contains(&"before"));
        assert!(texts.contains(&"after"));
    }

    #[test]
    fn iter_body_lines_skips_tilde_fence() {
        let body = "before\n~~~\nin_fence\n~~~\nafter";
        let lines: Vec<_> = iter_body_lines(body);
        assert!(!lines.iter().any(|l| l.text.contains("in_fence")));
    }

    #[test]
    fn iter_body_lines_skips_indented_block() {
        let body = "before\n\n    indented code\n\nafter";
        let lines: Vec<_> = iter_body_lines(body);
        assert!(!lines.iter().any(|l| l.text.contains("indented code")));
        assert!(lines.iter().any(|l| l.text == "before"));
        assert!(lines.iter().any(|l| l.text == "after"));
    }

    #[test]
    fn iter_body_lines_handles_nested_fence() {
        // The outer fence is 4 backticks; the inner ``` is just content.
        let body = "outside\n````\n```\nstill_code\n```\n````\nafter";
        let lines: Vec<_> = iter_body_lines(body);
        assert!(!lines.iter().any(|l| l.text.contains("still_code")));
        assert!(lines.iter().any(|l| l.text == "outside"));
        assert!(lines.iter().any(|l| l.text == "after"));
    }

    #[test]
    fn iter_body_lines_preserves_one_based_numbers() {
        let body = "a\n```\nb\n```\nc";
        let lines: Vec<_> = iter_body_lines(body);
        let nums: Vec<usize> = lines.iter().map(|l| l.number).collect();
        // Line 1 (a) survives, lines 2-4 (fence+content+fence) drop, line 5 (c) survives.
        assert_eq!(nums, vec![1, 5]);
    }

    // ─── extract_annotations ───────────────────────────────────────────

    fn promotes_pattern() -> AnnotationConfig {
        AnnotationConfig {
            name: "promotes".to_string(),
            pattern: r"\[PROMOTES:\s*(?P<id>[\w-]+)\]".to_string(),
            key: "id".to_string(),
            applies: ApplyTo::default(),
        }
    }

    #[test]
    fn extract_annotations_captures_grouping_key() {
        let body = "Refers to [PROMOTES: spec-payment] in the body.\n";
        let out = extract_annotations(body, &[promotes_pattern()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pattern_name, "promotes");
        assert_eq!(out[0].key, "spec-payment");
        assert_eq!(out[0].line, 1);
    }

    #[test]
    fn extract_annotations_multiple_per_line_and_per_doc() {
        let body = "[PROMOTES: a] and [PROMOTES: b]\n[PROMOTES: c]";
        let out = extract_annotations(body, &[promotes_pattern()]);
        let keys: Vec<&str> = out.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
        let lines: Vec<usize> = out.iter().map(|a| a.line).collect();
        assert_eq!(lines, vec![1, 1, 2]);
    }

    #[test]
    fn extract_annotations_skips_code_blocks() {
        let body = "[PROMOTES: real]\n```\n[PROMOTES: fake]\n```";
        let out = extract_annotations(body, &[promotes_pattern()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key, "real");
    }

    #[test]
    fn extract_annotations_empty_when_no_patterns_or_no_matches() {
        let body = "nothing to see here";
        assert!(extract_annotations(body, &[]).is_empty());
        assert!(extract_annotations(body, &[promotes_pattern()]).is_empty());
    }

    // ─── extract_body_line_matches ─────────────────────────────────────

    fn decision_log_block() -> BodyLineRuleConfig {
        let mut enums = std::collections::BTreeMap::new();
        enums.insert("gate".into(), vec!["scope".into(), "design".into()]);
        BodyLineRuleConfig {
            name: "spec-decision-log".into(),
            pattern: r"^- \*\*(?P<gate>[a-z-]+)\*\*".into(),
            applies: ApplyTo::default(),
            enums,
        }
    }

    #[test]
    fn extract_body_line_matches_captures_named_groups() {
        let body = "- **scope**: settled\n- **bogus**: typo\n";
        let out = extract_body_line_matches(body, &[decision_log_block()]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].rule_name, "spec-decision-log");
        assert_eq!(out[0].line, 1);
        assert_eq!(out[0].captures.get("gate"), Some(&"scope".to_string()));
        assert_eq!(out[1].line, 2);
        assert_eq!(out[1].captures.get("gate"), Some(&"bogus".to_string()));
    }

    #[test]
    fn extract_body_line_matches_skips_code_blocks() {
        let body = "- **scope**: ok\n```\n- **fake**: example syntax\n```\n";
        let out = extract_body_line_matches(body, &[decision_log_block()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].captures.get("gate"), Some(&"scope".to_string()));
    }

    #[test]
    fn extract_body_line_matches_skips_when_no_named_captures_bound() {
        // The regex matches but has no named-capture content for this
        // particular hit — defensive guard against storing inert
        // match records.
        let block = BodyLineRuleConfig {
            name: "literal".into(),
            pattern: r"hello".into(), // no named captures at all
            applies: ApplyTo::default(),
            enums: Default::default(),
        };
        let body = "hello world";
        assert!(extract_body_line_matches(body, &[block]).is_empty());
    }

    #[test]
    fn extract_body_line_matches_empty_for_no_blocks_or_no_matches() {
        let body = "no matches here";
        assert!(extract_body_line_matches(body, &[]).is_empty());
        assert!(extract_body_line_matches(body, &[decision_log_block()]).is_empty());
    }

    #[test]
    fn extract_body_line_matches_records_every_block_independently() {
        let other = BodyLineRuleConfig {
            name: "other".into(),
            pattern: r"\((?P<k>\w+)\)".into(),
            applies: ApplyTo::default(),
            enums: Default::default(),
        };
        let body = "- **design**: pick a (foo)";
        let out = extract_body_line_matches(body, &[decision_log_block(), other]);
        let names: Vec<&str> = out.iter().map(|m| m.rule_name.as_str()).collect();
        assert!(names.contains(&"spec-decision-log"));
        assert!(names.contains(&"other"));
        assert_eq!(out.len(), 2);
    }

    // ─── extract_body_block_matches ────────────────────────────────────

    fn decision_block() -> BodyBlockRuleConfig {
        let mut enums = std::collections::BTreeMap::new();
        enums.insert(
            "status".into(),
            vec!["accepted".into(), "rejected".into(), "deferred".into()],
        );
        BodyBlockRuleConfig {
            name: "adr-decision".into(),
            start_pattern: r"^## Decision \((?P<status>[a-z]+)\)".into(),
            end_pattern: r"^## ".into(),
            applies: ApplyTo::default(),
            enums,
        }
    }

    #[test]
    fn body_block_emits_one_match_per_span_with_start_captures() {
        let body = "# Title\n\n## Decision (accepted)\n\
                    The reasoning is sound.\n\nMore detail.\n\
                    ## Implications\n\nFurther text.\n";
        let out = extract_body_block_matches(body, &[decision_block()]);
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m.rule_name, "adr-decision");
        assert_eq!(m.captures.get("status"), Some(&"accepted".to_string()));
        // start_line = the `## Decision (...)` line; end_line = the
        // last line BEFORE `## Implications` closes the span.
        assert_eq!(m.start_line, 3);
        assert_eq!(m.end_line, 6);
    }

    #[test]
    fn body_block_sibling_start_closes_previous_span() {
        // Two sibling `## Decision` headers without an explicit
        // closing section. The second start closes the first span
        // (start_pattern overrides end_pattern when checked from
        // INSIDE state) and opens a new one.
        let body = "## Decision (accepted)\n\
                    text\n\
                    ## Decision (rejected)\n\
                    more text\n";
        let out = extract_body_block_matches(body, &[decision_block()]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].captures.get("status"), Some(&"accepted".to_string()));
        assert_eq!(out[0].start_line, 1);
        assert_eq!(out[0].end_line, 2);
        assert_eq!(out[1].captures.get("status"), Some(&"rejected".to_string()));
        assert_eq!(out[1].start_line, 3);
        assert_eq!(out[1].end_line, 4);
    }

    #[test]
    fn body_block_end_of_body_closes_open_span() {
        // No `## Foo` ever appears to close the span — end-of-body
        // closes it naturally. `end_line` is the last non-code
        // body line we observed.
        let body = "## Decision (deferred)\nbody line\nanother";
        let out = extract_body_block_matches(body, &[decision_block()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start_line, 1);
        assert_eq!(out[0].end_line, 3);
    }

    #[test]
    fn body_block_skips_start_inside_fenced_code() {
        // A `## Decision` inside a fenced block is illustrative
        // markdown, not a real header. The fence-aware iterator
        // filters it out, and the state machine never sees it.
        let body = "```\n## Decision (accepted)\nfake\n```\n\
                    ## Decision (rejected)\nreal";
        let out = extract_body_block_matches(body, &[decision_block()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].captures.get("status"), Some(&"rejected".to_string()));
    }

    #[test]
    fn body_block_empty_for_no_blocks_or_no_matches() {
        let body = "no headers at all";
        assert!(extract_body_block_matches(body, &[]).is_empty());
        assert!(extract_body_block_matches(body, &[decision_block()]).is_empty());
    }

    #[test]
    fn body_block_records_every_rule_independently() {
        let mut step_enums = std::collections::BTreeMap::new();
        step_enums.insert("status".into(), vec!["pending".into(), "done".into()]);
        let step = BodyBlockRuleConfig {
            name: "step".into(),
            start_pattern: r"^### Step \((?P<status>[a-z]+)\)".into(),
            end_pattern: r"^### ".into(),
            applies: ApplyTo::default(),
            enums: step_enums,
        };
        let body = "## Decision (accepted)\ntext\n\
                    ### Step (pending)\nstep body\n\
                    ### Step (done)\nmore\n";
        let out = extract_body_block_matches(body, &[decision_block(), step]);
        let names: Vec<&str> = out.iter().map(|m| m.rule_name.as_str()).collect();
        assert!(names.contains(&"adr-decision"));
        assert!(names.iter().filter(|n| **n == "step").count() == 2);
    }

    #[test]
    fn body_block_immediately_closing_span_emits_with_start_eq_end() {
        // end_pattern matches the very next line — empty content
        // block. Convention: end_line == start_line so consumers
        // see a valid range, not an inverted one.
        let body = "## Decision (accepted)\n## Next\n";
        let out = extract_body_block_matches(body, &[decision_block()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start_line, 1);
        assert_eq!(out[0].end_line, 1);
    }

    #[test]
    fn extract_annotations_multiple_patterns_independent() {
        let promotes = promotes_pattern();
        let research = AnnotationConfig {
            name: "research".to_string(),
            pattern: r"\[NEEDS RESEARCH:\s*(?P<topic>[\w-]+)\]".to_string(),
            key: "topic".to_string(),
            applies: ApplyTo::default(),
        };
        let body = "Line one [PROMOTES: x] and [NEEDS RESEARCH: y].";
        let out = extract_annotations(body, &[promotes, research]);
        let pairs: Vec<(&str, &str)> = out
            .iter()
            .map(|a| (a.pattern_name.as_str(), a.key.as_str()))
            .collect();
        assert!(pairs.contains(&("promotes", "x")));
        assert!(pairs.contains(&("research", "y")));
        assert_eq!(out.len(), 2);
    }
}
