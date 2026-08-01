use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use regex::Regex;
use std::sync::OnceLock;

use crate::config::{AnnotationConfig, BodyLineRuleConfig, ParserConfig};
use crate::model::{RawAnnotation, RawBodyLineMatch, RawEdge};

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
    let code_ranges = ProtectedSurfaces::of_body(body).blocks;
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
/// never emits `Tag::Link` inside a `CodeBlock`); wikilink and custom
/// regex captures are emitted per match and kept only when
/// `ProtectedSurfaces::admits` says so — the same verdict the
/// reference rewriter consults, so the two never disagree.
pub fn extract_links(body: &str, config: &ParserConfig) -> Vec<RawEdge> {
    let mut edges = Vec::new();
    let line_offsets = compute_line_offsets(body);

    // Pass 1: standard markdown links via pulldown-cmark. Naturally
    // fence-aware — `Tag::Link` events never fire inside a code block.
    //
    // An autolink is not one of them. `<foo:old.md>` is a URI by the
    // grammar that admits it — CommonMark requires the scheme — and it has
    // no destination to rewrite, only its own text, so binding it claimed
    // an edge `rename` could never follow and reported success leaving it
    // behind.
    for (event, range) in Parser::new_ext(body, Options::empty()).into_offset_iter() {
        if let Event::Start(Tag::Link {
            link_type,
            dest_url,
            ..
        }) = &event
            && !matches!(link_type, LinkType::Autolink | LinkType::Email)
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
    // line-by-line regex scan. A capture is an edge only when the shared
    // protection verdict admits it — code *blocks* are always opaque, an
    // inline code *span* yields only its full-content match to a
    // `code_spans` pattern. Sharing `ProtectedSurfaces` with the
    // reference rewriter is what keeps extraction and rewriting in
    // lockstep: the builder must never bind an edge the rewriter would
    // refuse to touch (a wikilink inside `` `[[x]]` `` is sample text,
    // not a reference).
    let compiled_patterns = compile_patterns(&config.link_patterns);
    let needs_line_pass = config.wikilink_enabled || !compiled_patterns.is_empty();
    if needs_line_pass {
        let protected = ProtectedSurfaces::of_body(body);
        let wikilink_re = config.wikilink_enabled.then(wikilink_regex);
        let mut scanned: Vec<(usize, RawEdge)> = Vec::new();

        for (idx, line) in body.lines().enumerate() {
            let line_start = line_offsets[idx];
            let mut push_capture = |m: regex::Match<'_>, relation: &str| {
                let target = m.as_str().trim();
                let (start, end) = (line_start + m.start(), line_start + m.end());
                if target.is_empty() || !protected.in_prose(start, end) {
                    return;
                }
                scanned.push((
                    start,
                    RawEdge {
                        target_path: target.to_string(),
                        relation: relation.to_string(),
                        location: format!("L{}", idx + 1),
                    },
                ));
            };

            if let Some(re) = wikilink_re {
                for caps in re.captures_iter(line) {
                    if let Some(m) = caps.get(1) {
                        push_capture(m, "references");
                    }
                }
            }
            for (regex, relation, _) in &compiled_patterns {
                for caps in regex.captures_iter(line) {
                    if let Some(m) = caps.get(1) {
                        push_capture(m, relation);
                    }
                }
            }
        }

        for (regex, relation, code_spans) in &compiled_patterns {
            if !code_spans {
                continue;
            }
            for (start, end) in protected.citations(body, regex) {
                let target = body[start..end].trim();
                if target.is_empty() {
                    continue;
                }
                scanned.push((
                    start,
                    RawEdge {
                        target_path: target.to_string(),
                        relation: relation.to_string(),
                        location: format!("L{}", line_for_offset(&line_offsets, start)),
                    },
                ));
            }
        }

        // Prose is read line by line and citations span by span, so the
        // order references are *found* in is the order of the passes.
        // Document order is the order they are *in*, and the only one a
        // reader of the graph can predict.
        scanned.sort_by_key(|(at, _)| *at);
        edges.extend(scanned.into_iter().map(|(_, edge)| edge));
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
                        name: cfg.name.clone(),
                        key: m.as_str().to_string(),
                        line: body_line.number,
                    });
                }
            }
        }
    }
    out
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
                    name: cfg.name.clone(),
                    line: body_line.number,
                    captures,
                });
            }
        }
    }
    out
}

/// The project path a markdown destination names, or `None` when it
/// names something outside the project (an absolute URL, a bare
/// `#fragment`) or nothing at all — trimmed, and cut at the first `#`.
///
/// The one reading of a destination: link extraction binds this string
/// as an edge target and [`Destination`] hands it to the rewriter, so
/// the two cannot bind and repoint different paths.
pub(crate) fn destination_path(dest: &str) -> Option<&str> {
    let dest = dest.trim();
    if dest.starts_with("http://")
        || dest.starts_with("https://")
        || dest.starts_with("mailto:")
        || dest.starts_with('#')
        || dest.is_empty()
    {
        return None;
    }
    Some(dest.split('#').next().unwrap_or(dest))
}

fn process_link_target(dest: &str, line_num: usize, extensions: &[String]) -> Option<RawEdge> {
    let path = destination_path(dest)?;
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

fn compile_patterns(patterns: &[crate::config::LinkPattern]) -> Vec<(Regex, String, bool)> {
    patterns
        .iter()
        .map(|p| {
            let regex =
                Regex::new(&p.pattern).expect("link patterns are validated by Config::load");
            (regex, p.relation.clone(), p.code_spans)
        })
        .collect()
}

/// `[[<target>]]` or `[[<target>|<display>]]`, group 1 the target.
/// Compiled once per process. The single definition of the wikilink
/// syntax — shared with the reference rewriter so extraction and
/// rewriting can never disagree on what a wikilink is.
pub(crate) fn wikilink_regex() -> &'static Regex {
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

/// One standard markdown link destination: the byte span that spells it
/// and the path that spelling names.
///
/// The two are not the same text. A destination *encodes* a path —
/// `old&#x2e;md` and `a\(1\).md` are how CommonMark spells `old.md` and
/// `a(1).md` — so the builder binds what the parser decoded, while a
/// rewrite has to replace the bytes that did the spelling. Reading the
/// spelling as if it were the path is how a rename reported success over
/// links it never repointed: the edge was bound from one text and looked
/// for in another.
pub(crate) struct Destination {
    /// The whole spelling, pointy brackets and title excluded — so a
    /// rewrite repoints the destination and leaves the rest of the link
    /// verbatim. The `#fragment` is inside it: which bytes spell the
    /// fragment marker is a fact about the decoded text, not the source
    /// (the `#` in `old&#x2e;md` opens an entity), so the split is made
    /// where it is knowable and the rewriter re-renders both halves.
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// What the parser reads out of that span, via [`destination_path`].
    pub(crate) path: String,
    /// The decoded remainder, leading `#` included, or empty.
    pub(crate) fragment: String,
}

impl Destination {
    /// Every destination in a whole document, offset into it: the
    /// frontmatter is split off once here, so no caller re-splits a body
    /// whose first line is an `---` hrule.
    ///
    /// Derived from the same pulldown-cmark token stream [`extract_links`]
    /// binds edges from, so extraction and rewriting agree on every link
    /// form. Inline links (`(url)`, `(url "t")`, `(<url>)`) yield the
    /// inline destination; reference / collapsed / shortcut links carry
    /// their URL in a `[label]: url` definition line, so the destination
    /// is emitted from that definition — but only for labels actually
    /// used by a link, matching the edges the builder binds.
    pub(crate) fn in_document(content: &str) -> Vec<Self> {
        let body_start = match crate::parser::frontmatter::split_frontmatter(content) {
            Ok((Some(_), body)) => content.len() - body.len(),
            _ => 0,
        };
        markdown_destinations(&content[body_start..], body_start)
    }
}

fn markdown_destinations(content: &str, offset: usize) -> Vec<Destination> {
    // Snapshot link reference definitions (label → definition span)
    // before the offset iter consumes a parser. A definition's URL is
    // matched to its uses by reference label, which CommonMark compares
    // case- and whitespace-insensitively — so both sides go through
    // `normalize_reference_label` and a use like `[x][REF]` finds its
    // `[ref]: url` definition.
    let parser = Parser::new_ext(content, Options::empty());
    let definitions: Vec<(String, String, std::ops::Range<usize>)> = parser
        .reference_definitions()
        .iter()
        .map(|(label, def)| {
            (
                normalize_reference_label(label),
                def.dest.to_string(),
                def.span.clone(),
            )
        })
        .collect();

    let mut found: Vec<((usize, usize), String)> = Vec::new();
    // Every `Tag::Link` the parser has open, innermost last, carrying for
    // an inline one the running end of its label content (the byte just
    // before `]`) and the destination the parser decoded out of it.
    //
    // A stack rather than one slot because a link label can hold a link:
    // `[see <http://x> here](a.md)` opens an autolink inside one, and its
    // `End` took the outer link's slot, so the outer destination was never
    // emitted while the builder bound its edge — the rewriter then had
    // nothing to repoint and the rename reported success.
    let mut open: Vec<Option<(usize, String)>> = Vec::new();
    // Reference labels actually used by a link, so an unused definition
    // (no edge in the build) is left untouched.
    let mut used_labels: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Link {
                link_type: LinkType::Inline,
                dest_url,
                ..
            }) => open.push(Some((range.start + 1, dest_url.to_string()))),
            // Reference / collapsed / shortcut link: no inline URL; the
            // `id` is its definition label. An autolink lands here too and
            // carries neither, which is why the slot it occupies is empty.
            Event::Start(Tag::Link { id, .. }) => {
                used_labels.insert(normalize_reference_label(&id));
                open.push(None);
            }
            Event::End(TagEnd::Link) => {
                if let Some((label_end, dest)) = open.pop().flatten()
                    && let Some(span) = destination_span(content, label_end + 2, range.end - 1)
                {
                    found.push((span, dest));
                }
                // A closed link is part of the enclosing label's text.
                extend_label(&mut open, range.end);
            }
            other => {
                // Extend the label end past any inline child (text,
                // emphasis, code, …) so `label_end` lands on the `]`.
                if !matches!(other, Event::Start(_)) {
                    extend_label(&mut open, range.end);
                }
            }
        }
    }

    for (label, dest, def_span) in &definitions {
        if used_labels.contains(label)
            && let Some(span) = definition_destination_span(content, def_span)
        {
            found.push((span, dest.clone()));
        }
    }
    found
        .into_iter()
        .filter_map(|((start, end), dest)| {
            let dest = dest.trim();
            let path = destination_path(dest)?;
            Some(Destination {
                start: offset + start,
                end: offset + end,
                fragment: dest[path.len()..].to_string(),
                path: path.to_string(),
            })
        })
        .collect()
}

/// Extend the innermost open inline link's label end past `to`. Every
/// inline child is part of the label, so `label_end` ends up on the `]`.
fn extend_label(open: &mut [Option<(usize, String)>], to: usize) {
    if let Some((label_end, _)) = open.iter_mut().rev().flatten().next() {
        *label_end = (*label_end).max(to);
    }
}

/// Normalise a markdown link reference label for matching, the way
/// CommonMark compares them: trim, collapse internal whitespace runs to
/// a single space, and case-fold. Applied to both a definition's label
/// and a link's reference id so `[x][REF]` matches `[ref]: url`.
fn normalize_reference_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The destination span of a `[label]: <url> "title"` reference
/// definition. The URL follows the colon that ends `[label]:` and is
/// parsed by the same grammar as an inline destination.
fn definition_destination_span(
    content: &str,
    def_span: &std::ops::Range<usize>,
) -> Option<(usize, usize)> {
    let bytes = content.as_bytes();
    let colon = find_unescaped(content, def_span.start, def_span.end, b']')?;
    if bytes.get(colon + 1) != Some(&b':') {
        return None;
    }
    destination_span(content, colon + 2, def_span.end)
}

/// The span of a destination URL within `content[start..limit)`: skip
/// leading whitespace, then take the pointy `<…>` body or the plain run
/// up to the next whitespace. Brackets and title stay outside, so a
/// rewrite repoints the destination and leaves the link's syntax
/// verbatim. Shared by inline links and reference definitions so both
/// resolve identically.
fn destination_span(content: &str, start: usize, limit: usize) -> Option<(usize, usize)> {
    let bytes = content.as_bytes();
    let limit = limit.min(content.len());
    let mut i = start;
    while i < limit && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= limit {
        return None;
    }
    let (start, j) = if bytes[i] == b'<' {
        let s = i + 1;
        (s, find_unescaped(content, s, limit, b'>').unwrap_or(limit))
    } else {
        // Whitespace is what ends a plain destination, and whitespace is
        // not escapable, so the run is read as it stands.
        let s = i;
        let mut j = s;
        while j < limit && !bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        (s, j)
    };
    (start < j).then_some((start, j))
}

/// The offset of the first `byte` in `content[from..limit)` that a
/// backslash does not escape, or `None`.
///
/// Escapes belong to the grammar the decoder reads, so a scanner reading
/// raw bytes cuts a span the decoder never would: the `>` in
/// `<a\>b.md>` closes nothing and the `]` in `[a\]b]:` ends no label.
/// Both cuts left the rewriter holding a fragment of a destination whose
/// decoded path the builder had bound, so the reference could not be
/// repointed — including one this crate writes itself, since a pointy
/// spelling escapes exactly these.
fn find_unescaped(content: &str, from: usize, limit: usize, byte: u8) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = from;
    while i < limit {
        if bytes[i] == b'\\' {
            i += 2;
        } else if bytes[i] == byte {
            return Some(i);
        } else {
            i += 1;
        }
    }
    None
}

/// One inline code span: the byte range of its markup (backticks
/// included) plus the content pulldown-cmark parsed out of it.
pub(crate) struct InlineCodeSpan {
    start: usize,
    end: usize,
    /// The source range between the delimiter runs — the span's own text,
    /// addressable in the document. Taken from the source rather than from
    /// the parser's rendering of it, which folds line breaks and drops a
    /// padding space, so a rewrite can name exactly the bytes it replaces.
    inner: (usize, usize),
}

/// The two surfaces a regex capture is judged against before it may be
/// treated as a reference — by link extraction and by the reference
/// rewriter alike, so the builder never binds an edge the rewriter
/// would refuse to touch. Code *blocks* (fenced or indented) are always
/// opaque. An inline code *span* is opaque too, except to a
/// `code_spans` pattern whose capture is the span's **entire content**
/// — the corpus shape where `` `adr-001` `` IS the citation, while a
/// partial match inside `` `just adr-tool` `` stays sample text.
pub(crate) struct ProtectedSurfaces {
    /// Every `(start, end)` byte range pulldown-cmark classifies as a code
    /// block, fenced or indented. The one answer to where code is: the
    /// line iterator every body scanner walks reads it too, so annotations
    /// and `rules.body_line` cannot come to a different conclusion from
    /// link extraction and the rewriter about what is prose.
    pub(crate) blocks: Vec<(usize, usize)>,
    spans: Vec<InlineCodeSpan>,
}

impl ProtectedSurfaces {
    /// The surfaces of a body, as given.
    ///
    /// Only a body is ever parsed, whichever a caller holds, because the
    /// markdown structure of "frontmatter then body" is not the structure
    /// of the body: a fence inside a YAML block scalar opens a code block
    /// that swallows everything after it. Both readers of this verdict
    /// reach it through here or through [`Self::of_document`], so the
    /// builder cannot bind a reference the rewriter then refuses to touch
    /// for a reason the builder never saw.
    ///
    /// Which one a caller wants is the caller's to say. A body that opens
    /// with an `---` hrule is indistinguishable from a document with
    /// frontmatter — the split is a fence-line reader, not a YAML one — so
    /// a constructor that guessed would split an already-split body a
    /// second time and hide the rest of it from one side only.
    pub(crate) fn of_body(body: &str) -> Self {
        Self::of_body_at(body, 0)
    }

    /// The surfaces of a whole document: its body's, offset into it.
    pub(crate) fn of_document(content: &str) -> Self {
        let body_start = match crate::parser::frontmatter::split_frontmatter(content) {
            Ok((Some(_), body)) => content.len() - body.len(),
            _ => 0,
        };
        Self::of_body_at(&content[body_start..], body_start)
    }

    fn of_body_at(body: &str, offset: usize) -> Self {
        let parser = Parser::new_ext(body, Options::empty());
        let mut blocks = Vec::new();
        let mut spans = Vec::new();
        let mut open: Option<usize> = None;
        for (event, range) in parser.into_offset_iter() {
            let (start, end) = (offset + range.start, offset + range.end);
            match event {
                Event::Start(Tag::CodeBlock(_)) => open = Some(start),
                Event::End(TagEnd::CodeBlock) => {
                    if let Some(opened) = open.take() {
                        blocks.push((opened, end));
                    }
                }
                Event::Code(_) => {
                    let fence = body[range.clone()]
                        .bytes()
                        .take_while(|b| *b == b'`')
                        .count();
                    spans.push(InlineCodeSpan {
                        start,
                        end,
                        inner: (start + fence, end - fence),
                    });
                }
                _ => {}
            }
        }
        Self { blocks, spans }
    }

    /// Whether a capture at `[start, end)` sits in prose — outside every
    /// code block and every inline code span.
    ///
    /// A line scan asks this and nothing else: a span is one text, and
    /// whether it is a citation is a question about the whole of it, which
    /// [`Self::citations`] asks of the span's own bytes.
    pub(crate) fn in_prose(&self, start: usize, end: usize) -> bool {
        let overlaps = |s: usize, e: usize| start < e && end > s;
        !self.blocks.iter().any(|&(s, e)| overlaps(s, e))
            && !self.spans.iter().any(|s| overlaps(s.start, s.end))
    }

    /// Every inline code span `re` accounts for the whole of, as the
    /// absolute range of its capture.
    ///
    /// The pattern is matched against the span's own text, so `^` and `$`
    /// mean the span — the reading `code_spans` invites, and the one a
    /// line scan cannot give, since a span's text is never at the start of
    /// its line. A match that leaves any of the span unexplained means the
    /// span is sample code (`just adr-tool`), not a citation.
    pub(crate) fn citations(&self, content: &str, re: &regex::Regex) -> Vec<(usize, usize)> {
        self.spans
            .iter()
            .filter(|span| {
                !self
                    .blocks
                    .iter()
                    .any(|&(s, e)| span.start < e && span.end > s)
            })
            .filter_map(|span| {
                let (from, to) = span.inner;
                let text = &content[from..to];
                let caps = re.captures(text)?;
                let (whole, capture) = (caps.get(0)?, caps.get(1)?);
                (whole.as_str() == text.trim())
                    .then(|| (from + capture.start(), from + capture.end()))
            })
            .collect()
    }
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
                code_spans: false,
            }]),
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "scripts/docs/parse.py");
        assert_eq!(edges[0].relation, "imports");
    }

    #[test]
    fn custom_pattern_records_every_match_on_a_line() {
        // Every match on a line becomes an edge (not just the first) — so a
        // line referencing two files yields two edges, consistent with the
        // wikilink pass and the reference rewriter.
        let body = "@cite(a/one.md) and @cite(b/two.md)";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"@cite\(([^)]+)\)".to_string(),
                relation: "cites".to_string(),
                code_spans: false,
            }]),
        );
        let targets: Vec<&str> = edges.iter().map(|e| e.target_path.as_str()).collect();
        assert_eq!(targets, vec!["a/one.md", "b/two.md"]);
    }

    #[test]
    fn code_spans_pattern_binds_full_content_span_and_bare_prose() {
        // `` `adr-target` `` IS the citation under a `code_spans` pattern;
        // the same pattern still binds a bare prose mention.
        let body = "cite `adr-target` and bare adr-plain here";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"\b(adr-[a-z0-9-]+)\b".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }]),
        );
        let targets: Vec<&str> = edges.iter().map(|e| e.target_path.as_str()).collect();
        assert_eq!(targets, vec!["adr-target", "adr-plain"]);
    }

    #[test]
    fn code_spans_pattern_binds_a_span_its_whole_match_accounts_for() {
        // The citation idiom decorates the id, so the capture is not the
        // whole span — the pattern's match is, and that is what makes the
        // span a citation rather than sample code.
        let body = "see `@cite(adr-target)` here";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"@cite\(([^)]+)\)".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }]),
        );
        let targets: Vec<&str> = edges.iter().map(|e| e.target_path.as_str()).collect();
        assert_eq!(targets, vec!["adr-target"]);
    }

    #[test]
    fn code_spans_pattern_leaves_a_decorated_span_it_only_partly_explains() {
        // Same pattern, and the span holds more than the match accounts
        // for: what is left over is what makes it sample code.
        let body = "run `just @cite(adr-target)` now";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"@cite\(([^)]+)\)".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }]),
        );
        assert_eq!(edges.len(), 0);
    }

    #[test]
    fn a_body_opening_with_an_hrule_is_not_a_document_with_frontmatter() {
        // The split reads fence lines, not YAML, so a body whose first line
        // is `---` looks exactly like a document carrying frontmatter. Only
        // the caller knows which it holds: a constructor that guessed split
        // an already-split body a second time and hid the rest of it from
        // the builder alone, while the rewriter read the whole thing.
        let body = "---\ntext\n~~~\n---\ncite [[adr-old]]\n";
        let document = format!("---\nid: citer\n---\n{body}");
        let from_body = ProtectedSurfaces::of_body(body);
        let from_document = ProtectedSurfaces::of_document(&document);
        let offset = document.len() - body.len();
        assert_eq!(
            from_body.blocks,
            from_document
                .blocks
                .iter()
                .map(|&(s, e)| (s - offset, e - offset))
                .collect::<Vec<_>>(),
            "both readings see the same code, in the same places"
        );
        let cite = body.find("adr-old").expect("the citation");
        assert!(
            !from_body.in_prose(cite, cite + "adr-old".len()),
            "the fence is open, so the citation is inside code"
        );
    }

    #[test]
    fn code_spans_pattern_may_anchor_to_the_span_it_cites() {
        // The opt-in reads "a span whose entire content matches", so an
        // anchored pattern is the spelling it invites. It is only the
        // spelling that works when the pattern is matched against the
        // span's own text rather than against the line, whose first
        // character is a delimiter no citation pattern accounts for.
        let body = "cite `adr-target` here";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"^(adr-[a-z0-9-]+)$".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }]),
        );
        let targets: Vec<&str> = edges.iter().map(|e| e.target_path.as_str()).collect();
        assert_eq!(targets, vec!["adr-target"]);
    }

    #[test]
    fn code_spans_pattern_leaves_partial_span_match_as_code() {
        // The capture is only part of the span's content — `` `just
        // adr-tool` `` is sample code, not a citation.
        let body = "run `just adr-tool` now";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"\b(adr-[a-z0-9-]+)\b".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }]),
        );
        assert_eq!(edges.len(), 0);
    }

    #[test]
    fn a_fence_in_the_frontmatter_is_not_a_block_in_the_body() {
        // The document's markdown structure is not the body's: a fence
        // inside a YAML block scalar opens a code block that swallows
        // everything after it. Both readers of the verdict parse the body,
        // so a citation after such frontmatter is a citation to both — it
        // was one the builder bound and the rewriter refused, silently.
        let document = "---\nid: a\nnote: |\n  ```\n---\n\ncite `adr-target` here\n";
        let surfaces = ProtectedSurfaces::of_document(document);
        assert!(surfaces.blocks.is_empty(), "{:?}", surfaces.blocks);
        let re = regex::Regex::new(r"\b(adr-[a-z0-9-]+)\b").unwrap();
        let cited: Vec<&str> = surfaces
            .citations(document, &re)
            .into_iter()
            .map(|(s, e)| &document[s..e])
            .collect();
        assert_eq!(cited, ["adr-target"]);
    }

    #[test]
    fn code_spans_pattern_still_skips_code_blocks() {
        let body = "```\n`adr-fenced` and adr-bare\n```\n\ncite `adr-real`";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"\b(adr-[a-z0-9-]+)\b".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }]),
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "adr-real");
    }

    #[test]
    fn span_content_stays_protected_without_code_spans_flag() {
        // Default policy unchanged: the span surface is opaque, only the
        // bare prose mention binds.
        let body = "cite `adr-target` and bare adr-plain here";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"\b(adr-[a-z0-9-]+)\b".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }]),
        );
        let targets: Vec<&str> = edges.iter().map(|e| e.target_path.as_str()).collect();
        assert_eq!(targets, vec!["adr-plain"]);
    }

    #[test]
    fn custom_pattern_skipped_in_code_block() {
        let body = "```\n@import not/real.py\n```\n\n@import real/file.py";
        let edges = extract_links(
            body,
            &cfg_with_patterns(vec![LinkPattern {
                pattern: r"^@import\s+(.+?)\s*$".to_string(),
                relation: "imports".to_string(),
                code_spans: false,
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
    fn wikilinks_skipped_in_inline_code_span() {
        // A wikilink inside an inline code span is sample text, not a
        // reference — the builder must not bind an edge the reference
        // rewriter (which protects inline code) would refuse to touch.
        let body = "use `[[fake]]` inline, but [[real]] is an edge";
        let edges = extract_links(body, &cfg_with_wikilinks());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target_path, "real");
    }

    #[test]
    fn destinations_cover_inline_link_forms() {
        // Plain, titled, pointy, and fragment inline links yield the
        // destination span; a code-span link yields nothing. The
        // reference-style link's URL is surfaced from its definition
        // line (`[ref]: defn.md`), since that is where it lives.
        let content = "[a](one.md) [b](two.md \"t\") [c](<three.md>) [d](four.md#sec) \
                       `[e](code.md)` [f][ref]\n\n[ref]: defn.md\n";
        let got: Vec<&str> = Destination::in_document(content)
            .iter()
            .map(|d| &content[d.start..d.end])
            .collect();
        assert_eq!(
            got,
            vec!["one.md", "two.md", "three.md", "four.md#sec", "defn.md"]
        );
    }

    #[test]
    fn destinations_cover_reference_collapsed_shortcut() {
        // Reference, collapsed, and shortcut links all resolve through
        // their definition line — its URL is the single rewritable span.
        let content = "[full][r] and [coll][] and [short]\n\n[r]: one.md\n[coll]: two.md\n[short]: three.md\n";
        let mut got: Vec<&str> = Destination::in_document(content)
            .iter()
            .map(|d| &content[d.start..d.end])
            .collect();
        got.sort_unstable();
        assert_eq!(got, vec!["one.md", "three.md", "two.md"]);
    }

    #[test]
    fn destinations_skip_unused_definition() {
        // A definition with no referencing link is not an edge in the
        // build, so the rewriter must leave it untouched.
        let content = "no links here\n\n[unused]: orphan.md\n";
        assert!(Destination::in_document(content).is_empty());
    }

    #[test]
    fn destinations_match_labels_case_and_whitespace_insensitively() {
        // CommonMark compares reference labels case- and
        // whitespace-insensitively. The builder binds `[x][REF]` to
        // `[ref]: target.md`, so the rewriter must surface that
        // definition's span despite the casing / spacing mismatch —
        // otherwise rename would dangle the edge.
        for content in [
            "[x][REF]\n\n[ref]: target.md\n",
            "[x][My  Ref]\n\n[my ref]: target.md\n",
        ] {
            let got: Vec<&str> = Destination::in_document(content)
                .iter()
                .map(|d| &content[d.start..d.end])
                .collect();
            assert_eq!(got, vec!["target.md"], "content: {content:?}");
        }
    }

    #[test]
    fn a_destination_names_the_path_its_spelling_encodes() {
        // Entity and backslash escapes are part of the destination
        // grammar, so `old&#x2e;md` and `a\(1\).md` name `old.md` and
        // `a(1).md` — the strings the builder binds as edge targets. The
        // span stays the bytes that spelled them, which is what a rewrite
        // has to replace.
        let content = "[a](old&#x2e;md) [b](a\\(1\\).md) [c](plain.md#sec)\n";
        let destinations = Destination::in_document(content);
        let got: Vec<(&str, &str, &str)> = destinations
            .iter()
            .map(|d| {
                (
                    &content[d.start..d.end],
                    d.path.as_str(),
                    d.fragment.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("old&#x2e;md", "old.md", ""),
                ("a\\(1\\).md", "a(1).md", ""),
                ("plain.md#sec", "plain.md", "#sec"),
            ]
        );
    }

    #[test]
    fn an_escaped_delimiter_does_not_end_the_span_it_is_inside() {
        // `\>` inside pointy brackets and `\]` inside a reference label
        // are the destination grammar's own escapes. Cut on the raw byte,
        // the span held a fragment of a destination whose decoded path the
        // builder had bound — and a pointy spelling is what this crate
        // writes for a path carrying `>`, so it could not read its own
        // output back.
        for (content, span, path) in [
            ("[x](<a\\>b.md>)\n", "a\\>b.md", "a>b.md"),
            ("[q][a\\]b]\n\n[a\\]b]: x.md\n", "x.md", "x.md"),
        ] {
            let destinations = Destination::in_document(content);
            let got: Vec<(&str, &str)> = destinations
                .iter()
                .map(|d| (&content[d.start..d.end], d.path.as_str()))
                .collect();
            assert_eq!(got, vec![(span, path)], "content: {content:?}");
        }
    }

    #[test]
    fn a_link_inside_a_link_label_does_not_take_the_outer_link_slot() {
        // A label can hold a link: an autolink opens one, and pulldown
        // still parses the outer link around it. Sharing one slot, the
        // inner `End` consumed the outer link's, so the builder bound an
        // edge whose destination the rewriter never saw.
        for content in [
            "[see <http://x> here](a.md)\n",
            "[mail <a@b.com> me](a.md)\n",
            "[img ![alt](i.png) here](a.md)\n",
        ] {
            let destinations = Destination::in_document(content);
            let got: Vec<&str> = destinations.iter().map(|d| d.path.as_str()).collect();
            assert_eq!(got, vec!["a.md"], "content: {content:?}");
        }
    }

    #[test]
    fn a_destination_outside_the_project_names_no_path() {
        // The one reading of a destination is shared with link
        // extraction, so what `process_link_target` refuses to bind the
        // rewriter never offers to repoint.
        let content = "[a](https://example.com/x.md) [b](mailto:a@b.md) [c](#frag)\n";
        assert!(Destination::in_document(content).is_empty());
    }

    #[test]
    fn extract_links_requires_extension_on_markdown_targets() {
        // A standard markdown link is an edge only when its target
        // carries a configured extension — `[x](docs/old)` is not an
        // edge, mirroring the guard the reference rewriter applies so
        // the two never disagree on extensionless markdown links.
        let cfg = ParserConfig {
            extensions: vec![".md".into()],
            ..ParserConfig::default()
        };
        let edges = extract_links("bare [x](docs/old) and full [y](docs/old.md)", &cfg);
        let targets: Vec<&str> = edges.iter().map(|e| e.target_path.as_str()).collect();
        assert_eq!(targets, vec!["docs/old.md"]);
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

            kinds: vec![],
        }
    }

    #[test]
    fn extract_annotations_captures_grouping_key() {
        let body = "Refers to [PROMOTES: spec-payment] in the body.\n";
        let out = extract_annotations(body, &[promotes_pattern()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "promotes");
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
            enums,

            kinds: vec![],
        }
    }

    #[test]
    fn extract_body_line_matches_captures_named_groups() {
        let body = "- **scope**: settled\n- **bogus**: typo\n";
        let out = extract_body_line_matches(body, &[decision_log_block()]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "spec-decision-log");
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
            enums: Default::default(),

            kinds: vec![],
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
            enums: Default::default(),

            kinds: vec![],
        };
        let body = "- **design**: pick a (foo)";
        let out = extract_body_line_matches(body, &[decision_log_block(), other]);
        let names: Vec<&str> = out.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"spec-decision-log"));
        assert!(names.contains(&"other"));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn extract_annotations_multiple_patterns_independent() {
        let promotes = promotes_pattern();
        let research = AnnotationConfig {
            name: "research".to_string(),
            pattern: r"\[NEEDS RESEARCH:\s*(?P<topic>[\w-]+)\]".to_string(),
            key: "topic".to_string(),

            kinds: vec![],
        };
        let body = "Line one [PROMOTES: x] and [NEEDS RESEARCH: y].";
        let out = extract_annotations(body, &[promotes, research]);
        let pairs: Vec<(&str, &str)> = out
            .iter()
            .map(|a| (a.name.as_str(), a.key.as_str()))
            .collect();
        assert!(pairs.contains(&("promotes", "x")));
        assert!(pairs.contains(&("research", "y")));
        assert_eq!(out.len(), 2);
    }
}
