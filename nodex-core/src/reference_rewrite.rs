//! Rewriting body-link references when a document moves.
//!
//! `rename` moves a file and must repoint every other document's links so
//! the graph stays connected. The matching logic here is the *same* one
//! the build-time resolver uses — `reference_path_candidates` plus the
//! shared `normalize_relative` primitive — so the rewriter can never
//! disagree with the graph about what a link points to. Code is the one
//! place a fuzzy text rewrite could corrupt a document, so tokens
//! inside code are judged by the same `body::ProtectedSurfaces`
//! verdict the builder's link extraction uses: code blocks stay
//! untouched unconditionally, and an inline code span yields only its
//! full-content match to a `code_spans` link pattern — whatever the
//! builder binds as an edge, and nothing else, is what a rewrite may
//! touch. A markdown destination is read the same way twice for the same
//! reason: its bytes *spell* a path rather than being one, so
//! `body::Destination` hands over the parser's reading of them — the
//! string the build resolved — and the span keeps the bytes a rewrite has
//! to replace.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::builder::resolver::reference_path_candidates;
use crate::config::ParserConfig;
use crate::parser::body;

/// Rewrite every body link in `content` that the resolver would bind to
/// `old_path` so it points to `new_path` instead, returning the rewritten
/// document — or `None` when nothing changed.
///
/// Markdown links, `[[wikilinks]]` (when enabled), and
/// `[[parser.link_patterns]]` custom references are all handled. The
/// author's style survives the rewrite: a root-relative link stays
/// root-relative, a source-relative one is recomputed relative to the
/// linking file, and an extension-less reference (a wikilink resolved by
/// appending a configured extension) stays extension-less.
///
/// `scope_paths` is the *pre-move* scope (forward-slashed,
/// project-root-relative paths with `old_path` present and `new_path`
/// absent) against which each link's binding is resolved the way the
/// build does — first matching candidate over the ordered ladder,
/// literal frame then relative frame. A link is rewritten only when
/// that binding is `old_path`; one that binds a different file
/// (including a bare extension-less sibling shadowing the renamed
/// `.md`) is an edge to that file and is left untouched.
///
/// `source_dir` is the linking file's parent directory (project-root
/// relative); `old_path` / `new_path` are project-root-relative paths.
pub fn rewrite_references(
    content: &str,
    source_dir: &Path,
    old_path: &Path,
    new_path: &Path,
    scope_paths: &BTreeSet<String>,
    parser: &ParserConfig,
) -> std::result::Result<Option<String>, crate::error::ParseError> {
    // Resolve and rewrite against the same canonical text the builder
    // parsed (BOM strip, CRLF/CR → LF), so a Windows-line-ending file's
    // frontmatter is split — and its references located — identically.
    let content = crate::parser::frontmatter::canonicalize(content);
    let old_norm = crate::path_guard::forward_string(old_path);
    let edits: Vec<(usize, usize, String)> = reference_target_spans(&content, parser)?
        .into_iter()
        .filter_map(|span| {
            let replacement = rewritten_target(
                &span.target,
                source_dir,
                &old_norm,
                new_path,
                scope_paths,
                &parser.extensions,
            )?;
            let Some(fragment) = &span.fragment else {
                return Some((span.start, span.end, replacement));
            };
            accepted_destination(&content, span.start..span.end, &replacement, fragment)
                .map(|spelling| (span.start, span.end, spelling))
        })
        .collect();

    if edits.is_empty() {
        return Ok(None);
    }
    Ok(Some(apply_edits(&content, edits)))
}

/// Rebase the *moved file's own* body references after a
/// cross-directory move. Links written relative to the file's old
/// directory still spell the old vantage point; every reference the
/// resolver would have bound to an in-scope file from `old_dir` — and
/// that no longer binds to that same file from `new_dir` — is
/// re-rendered from the new directory. Returns the rewritten document,
/// or `None` when nothing changed (including the same-directory case,
/// where no vantage point moved).
///
/// Left untouched, deliberately: root-relative links (the literal
/// interpretation wins, mirroring resolver precedence — they are
/// move-invariant), references that did not resolve from the old
/// directory (already dangling — a rewrite must never fabricate a
/// resolution), id-style references, and anything inside code blocks,
/// inline code spans, or frontmatter.
pub fn rewrite_moved_references(
    content: &str,
    old_dir: &Path,
    new_dir: &Path,
    in_scope_paths: &BTreeSet<String>,
    parser: &ParserConfig,
) -> std::result::Result<Option<String>, crate::error::ParseError> {
    if old_dir == new_dir {
        return Ok(None);
    }
    let content = crate::parser::frontmatter::canonicalize(content);
    let edits: Vec<(usize, usize, String)> = reference_target_spans(&content, parser)?
        .into_iter()
        .filter_map(|span| {
            rebased_target(
                &span.target,
                old_dir,
                new_dir,
                in_scope_paths,
                &parser.extensions,
            )
            .and_then(|replacement| match &span.fragment {
                None => Some((span.start, span.end, replacement)),
                Some(fragment) => {
                    accepted_destination(&content, span.start..span.end, &replacement, fragment)
                        .map(|spelling| (span.start, span.end, spelling))
                }
            })
        })
        .collect();

    if edits.is_empty() {
        return Ok(None);
    }
    Ok(Some(apply_edits(&content, edits)))
}

/// One rewritable reference: the byte span of its target slice in the
/// content. Every body reference is a *document reference* (resolved
/// with extension-append and id-fallback) by construction — the
/// path-only `covers` relation exists only in frontmatter, which is
/// never scanned here, and `Config::validate` keeps it off link
/// patterns.
struct ReferenceSpan {
    start: usize,
    end: usize,
    /// The reference the builder bound from this span. For every form
    /// but one it is the span's own text; a markdown destination
    /// *encodes* its path (`a\(1\).md` spells `a(1).md`), and the
    /// parser's reading of that encoding is what the build bound.
    target: String,
    /// What follows the target inside the span when the span is a
    /// markdown link destination: its decoded `#fragment`, or the empty
    /// string. `None` for every other reference form, whose span is the
    /// target alone and whose syntax the surrounding text carries — and
    /// whose replacement therefore needs no destination to read back.
    fragment: Option<String>,
}

/// Every body reference target span in `content`, extracted exactly as
/// the builder does (`parser::body::extract_links`): standard markdown
/// link destinations via pulldown-cmark (so pointy `<url>` and titled
/// `(url "t")` forms are handled identically), plus `[[wikilink]]` and
/// `parser.link_patterns` captures scanned per line. The frontmatter
/// block is excluded outright, and code is judged by the shared
/// `body::ProtectedSurfaces` verdict — blocks always opaque, an
/// inline code span rewritable only as a `code_spans` pattern's
/// full-content match — so a mutating rewrite reaches exactly the
/// spans the builder binds as edges, and nothing else.
///
/// Each span is the *trimmed* slice — the builder binds the trimmed
/// capture (`[[ a ]]` → `a`), so the rewriter replaces the same slice
/// and surrounding whitespace is preserved verbatim. What that slice
/// means is a second question: a markdown destination spells its path
/// (`docs/a&#x2e;md`) and carries its `#fragment`, so it arrives already
/// read by the parser that bound the edge, while every other form is its
/// own target.
fn reference_target_spans(
    content: &str,
    parser: &ParserConfig,
) -> std::result::Result<Vec<ReferenceSpan>, crate::error::ParseError> {
    let protected = body::ProtectedSurfaces::of_document(content);
    let frontmatter: Vec<(usize, usize)> = frontmatter_range(content)?.into_iter().collect();
    let mut spans: Vec<ReferenceSpan> = Vec::new();
    let mut cited: Vec<(usize, usize)> = Vec::new();
    // One admission for every form: the slice the builder binds, which is
    // the trimmed span, outside the frontmatter and in prose.
    let admit = |start: usize, end: usize| -> Option<(usize, usize)> {
        trim_span(content, start, end)
            .filter(|&(s, e)| !overlaps(s, e, &frontmatter) && protected.in_prose(s, e))
    };

    // Markdown links: same pulldown-cmark token stream the builder uses,
    // so the two agree on every inline-link form (plain / pointy /
    // titled) and never on code-span contents. A standard markdown link
    // is an edge only when its target already carries a configured
    // extension (the builder's `process_link_target` guard) — it never
    // extension-appends a bare path the way a wikilink does — so the
    // rewriter applies the same filter and leaves `[x](docs/old)`
    // (no extension, not an edge) untouched. The extension is read off the
    // decoded path, because that is the string the builder reads it off:
    // `[x](old&#x2e;md)` names a `.md` file and carries an edge, while the
    // bytes spelling it end in `;md`.
    for destination in body::Destination::in_document(content) {
        if parser
            .extensions
            .iter()
            .any(|ext| destination.path.ends_with(ext.as_str()))
            && let Some((start, end)) = admit(destination.start, destination.end)
        {
            spans.push(ReferenceSpan {
                start,
                end,
                target: destination.path,
                fragment: Some(destination.fragment),
            });
        }
    }

    // Wikilinks and custom patterns: line-anchored regex captures.
    let mut capture = |start: usize, end: usize| {
        if let Some((start, end)) = admit(start, end) {
            spans.push(ReferenceSpan {
                start,
                end,
                target: content[start..end].to_string(),
                fragment: None,
            });
        }
    };
    if parser.wikilink_enabled {
        scan_line_captures(content, body::wikilink_regex(), &mut capture);
    }
    for pattern in &parser.link_patterns {
        let re = Regex::new(&pattern.pattern).expect("link patterns validated by Config::load");
        scan_line_captures(content, &re, &mut capture);
        if pattern.code_spans {
            cited.extend(protected.citations(content, &re).into_iter().filter_map(
                |(start, end)| {
                    trim_span(content, start, end).filter(|&(s, e)| !overlaps(s, e, &frontmatter))
                },
            ));
        }
    }
    spans.extend(cited.into_iter().map(|(start, end)| ReferenceSpan {
        start,
        end,
        target: content[start..end].to_string(),
        fragment: None,
    }));
    Ok(spans)
}

/// Run `re` over each line of `content` and feed capture-group-1 byte
/// spans (absolute) to `push`. Line-anchored patterns (wikilinks,
/// custom link patterns) are scanned per line, matching the builder's
/// own line pass.
fn scan_line_captures(content: &str, re: &Regex, push: &mut impl FnMut(usize, usize)) {
    let mut line_start = 0usize;
    for line in content.split_inclusive('\n') {
        let text_len = line.trim_end_matches('\n').len();
        for caps in re.captures_iter(&line[..text_len]) {
            if let Some(target) = caps.get(1) {
                push(line_start + target.start(), line_start + target.end());
            }
        }
        line_start += line.len();
    }
}

/// Rewrite every body *id* reference to `old_id` so it names `new_id`,
/// returning the rewritten document — or `None` when none was present.
///
/// Ids appear in the body only as `[[wikilink]]` or `[[parser.link_patterns]]`
/// targets (markdown links are paths, not ids), so only those are scanned,
/// per line and under the shared `body::ProtectedSurfaces` verdict —
/// a capture in a code block never rewrites, one in an inline code span
/// rewrites only as a `code_spans` pattern's full-content match. The capture must
/// equal `old_id` verbatim, and — mirroring the build resolver's path-first
/// precedence — it is an id reference only when it does **not** bind an
/// in-scope file: `[[old]]` next to a file `old.md` resolves to that file
/// (a path edge), so id retargeting leaves it alone. `source_dir` is the
/// scanned file's parent directory; `in_scope_paths` is the
/// forward-slashed set of in-scope file paths.
pub fn rewrite_id_references(
    content: &str,
    old_id: &str,
    new_id: &str,
    source_dir: &Path,
    in_scope_paths: &BTreeSet<String>,
    parser: &ParserConfig,
) -> std::result::Result<Option<String>, crate::error::ParseError> {
    let mut line_regexes: Vec<(Regex, bool)> = Vec::new();
    if parser.wikilink_enabled {
        line_regexes.push((body::wikilink_regex().clone(), false));
    }
    for pattern in &parser.link_patterns {
        line_regexes.push((
            Regex::new(&pattern.pattern).expect("link patterns validated by Config::load"),
            pattern.code_spans,
        ));
    }
    if line_regexes.is_empty() {
        return Ok(None);
    }

    // The capture is an id reference only when the resolver would fall
    // through to the bare-id step — i.e. it does not bind a file by
    // path (literal or source-relative frame) first.
    let binds_a_path = |capture: &str| {
        let forward = crate::path_guard::forward_str(capture);
        let normalized = forward.strip_prefix("./").unwrap_or(&forward);
        resolve_in_set(normalized, in_scope_paths, &parser.extensions).is_some()
            || crate::path_guard::normalize_relative(&source_dir.join(normalized))
                .and_then(|rel| resolve_in_set(&rel, in_scope_paths, &parser.extensions))
                .is_some()
    };

    let protected = body::ProtectedSurfaces::of_document(content);
    let frontmatter: Vec<(usize, usize)> = frontmatter_range(content)?.into_iter().collect();
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut line_start = 0usize;
    for line in content.split_inclusive('\n') {
        let text_len = line.trim_end_matches('\n').len();
        for (re, _) in &line_regexes {
            for caps in re.captures_iter(&line[..text_len]) {
                if let Some(target) = caps.get(1)
                    && target.as_str().trim() == old_id
                    && !binds_a_path(target.as_str().trim())
                {
                    // The slice the builder binds, which is the trimmed
                    // capture: replacing the padded one instead writes a
                    // reference the pattern no longer matches, and the guard
                    // below then declines a rewrite that had a correct form
                    // nobody tried.
                    let Some((start, end)) = trim_span(
                        content,
                        line_start + target.start(),
                        line_start + target.end(),
                    ) else {
                        continue;
                    };
                    if overlaps(start, end, &frontmatter) || !protected.in_prose(start, end) {
                        continue;
                    }
                    // Round-trip guard: the rewritten line must still be
                    // read as this reference, the pattern re-capturing
                    // exactly `new_id` where it captured the old one. An id
                    // carrying one of the pattern's own delimiters (a `)`
                    // inside `@cite(...)`) would otherwise be written into a
                    // reference the next build parses as a *different* id. A
                    // reference that cannot round-trip is left untouched: it
                    // stays visible (and, once the old node is gone,
                    // surfaces as an unresolved edge) rather than mangled.
                    let candidate_line = rewritten_line(
                        &line[..text_len],
                        (start - line_start)..(end - line_start),
                        new_id,
                    );
                    let recaptures = re.captures_iter(&candidate_line).any(|c| {
                        c.get(1).is_some_and(|m| {
                            m.start() == target.start() && m.as_str().trim() == new_id
                        })
                    });
                    // Where a line sits in the markdown is not a property of
                    // the line: an indented continuation is paragraph text
                    // after a paragraph and a code block alone, so the
                    // surface is asked of the document the write would
                    // produce rather than of the line lifted out of it.
                    //
                    // That document must also still be this document. Only
                    // body ranges are edited here, so the frontmatter
                    // boundary cannot legitimately move — and a successor
                    // that reads as a delimiter moves it, turning the lines
                    // under it into frontmatter and giving the document
                    // somebody else's id.
                    let candidate = rewritten_line(content, start..end, new_id);
                    let stays_prose = body::ProtectedSurfaces::of_document(&candidate)
                        .in_prose(start, start + new_id.len());
                    // A candidate whose frontmatter cannot be read at all is
                    // not this document either: folding that error into
                    // "no frontmatter" made it equal to a frontmatterless
                    // original, and the corrupt candidate then reached the
                    // gate, which refuses the whole batch — one span vetoing
                    // every other file's clean rewrite, where a span that
                    // cannot round-trip is meant to be skipped alone.
                    let stays_this_document = matches!(
                        frontmatter_range(&candidate),
                        Ok(range) if range == frontmatter.first().copied()
                    );
                    if !recaptures || !stays_prose || !stays_this_document {
                        continue;
                    }
                    edits.push((start, end, new_id.to_string()));
                }
            }
        }
        line_start += line.len();
    }

    // Citations: a span is one text, so the guard is that the rewritten
    // span is still a citation of `new_id`. An id carrying a backtick
    // closes the span around itself, leaving a bound edge erased by a write
    // that answered success — the same question, asked of the same helper.
    for (re, code_spans) in &line_regexes {
        if !code_spans {
            continue;
        }
        for (raw_start, raw_end) in protected.citations(content, re) {
            let Some((start, end)) = trim_span(content, raw_start, raw_end) else {
                continue;
            };
            if overlaps(start, end, &frontmatter)
                || content[start..end] != *old_id
                || binds_a_path(old_id)
            {
                continue;
            }
            let span = protected
                .span_of(start)
                .expect("a citation lies inside the span it was read from");
            let candidate = rewritten_line(
                &content[span.0..span.1],
                (start - span.0)..(end - span.0),
                new_id,
            );
            let rewritten = body::ProtectedSurfaces::of_body(&candidate);
            let survives = rewritten
                .citations(&candidate, re)
                .into_iter()
                .any(|(s, e)| candidate[s..e].trim() == new_id);
            if survives {
                edits.push((start, end, new_id.to_string()));
            }
        }
    }

    if edits.is_empty() {
        return Ok(None);
    }
    Ok(Some(apply_edits(content, edits)))
}

/// The replacement for a link `target`, or `None` unless the resolver
/// would bind it to `old_path`. Resolves the link exactly as
/// [`builder::resolver`] does — literal (root-relative) frame first,
/// then the source-relative frame; within each frame the *first*
/// candidate present in `scope_paths` wins (extension-append included).
/// The rewrite fires only when that binding is `old_path`: a link
/// whose first binding is a *different* file — including a bare
/// extension-less sibling that shadows the renamed `.md` — is an edge
/// to that file and is left untouched.
///
/// `scope_paths` is the scope in which the link's pre-rewrite binding
/// is evaluated, so `old_path` must be present in it (and `new_path`
/// absent) — the caller supplies the pre-move scope.
fn rewritten_target(
    target: &str,
    source_dir: &Path,
    old_norm: &str,
    new_path: &Path,
    scope_paths: &BTreeSet<String>,
    extensions: &[String],
) -> Option<String> {
    let forward = crate::path_guard::forward_str(target);
    let normalized = forward.strip_prefix("./").unwrap_or(&forward);
    if Path::new(normalized).has_root() {
        return None;
    }
    let keep_extension = extensions
        .iter()
        .any(|ext| normalized.ends_with(ext.as_str()));

    // Literal frame: if it binds anything, that binding is final (the
    // resolver never falls through to the relative frame once the
    // literal frame matches). Rewrite only when it is `old_path`.
    if let Some(bound) = resolve_in_set(normalized, scope_paths, extensions) {
        return (bound == old_norm)
            .then(|| render_target(new_path, None, keep_extension, extensions));
    }
    // Source-relative frame.
    if let Some(rel) = crate::path_guard::normalize_relative(&source_dir.join(normalized))
        && let Some(bound) = resolve_in_set(&rel, scope_paths, extensions)
    {
        return (bound == old_norm)
            .then(|| render_target(new_path, Some(source_dir), keep_extension, extensions));
    }
    None
}

/// The replacement for a moved file's own `target`, or `None` when the
/// move does not change what it points to. Mirrors the resolver's
/// precedence exactly: the literal (root-relative) interpretation is
/// tried first and is move-invariant; a target that only resolves
/// *relative to the old directory* is bound to that file, then
/// re-rendered from the new directory — unless the original spelling
/// happens to bind to the same file from there already (minimal diff:
/// only references whose resolution actually changes are touched).
fn rebased_target(
    target: &str,
    old_dir: &Path,
    new_dir: &Path,
    in_scope_paths: &BTreeSet<String>,
    extensions: &[String],
) -> Option<String> {
    let forward = crate::path_guard::forward_str(target);
    let normalized = forward.strip_prefix("./").unwrap_or(&forward);
    if Path::new(normalized).has_root() {
        return None;
    }
    // Literal interpretation first — resolver precedence. A target the
    // in-scope set satisfies root-relatively never moves with the file.
    if resolve_in_set(normalized, in_scope_paths, extensions).is_some() {
        return None;
    }
    // Bind from the old vantage point. No binding → already dangling
    // before the move; never fabricate a resolution.
    let old_rel = crate::path_guard::normalize_relative(&old_dir.join(normalized))?;
    let bound_path = resolve_in_set(&old_rel, in_scope_paths, extensions)?;

    // Minimal diff: leave the reference alone when it still binds to
    // the same file from the new directory.
    let still_bound = crate::path_guard::normalize_relative(&new_dir.join(normalized))
        .and_then(|rel| resolve_in_set(&rel, in_scope_paths, extensions))
        .is_some_and(|path| path == bound_path);
    if still_bound {
        return None;
    }

    let keep_extension = extensions
        .iter()
        .any(|ext| normalized.ends_with(ext.as_str()));
    let rendered = render_target(
        Path::new(&bound_path),
        Some(new_dir),
        keep_extension,
        extensions,
    );
    (rendered != target).then_some(rendered)
}

/// First candidate of the shared ladder present in the in-scope path
/// set — the file the resolver would bind this reference to. Every
/// body reference is a document reference, so the ladder always
/// includes the extension-append candidates (the build resolver's
/// path-only branch belongs to the frontmatter-produced `covers`
/// relation, which never reaches the rewriter).
fn resolve_in_set(
    base: &str,
    in_scope_paths: &BTreeSet<String>,
    extensions: &[String],
) -> Option<String> {
    reference_path_candidates(base, extensions, true)
        .into_iter()
        .find(|candidate| in_scope_paths.contains(candidate))
}

/// Render `new_path` as a link target in the author's style: root-relative
/// when `relative_to` is `None`, otherwise relative to the linking file's
/// directory. Strips a configured extension when the original reference
/// carried none (an extension-less wikilink stays extension-less).
fn render_target(
    new_path: &Path,
    relative_to: Option<&Path>,
    keep_extension: bool,
    extensions: &[String],
) -> String {
    let base = match relative_to {
        None => crate::path_guard::forward_string(new_path),
        Some(dir) => relative_from(dir, new_path),
    };
    if keep_extension {
        base
    } else {
        strip_configured_extension(&base, extensions)
    }
}

fn strip_configured_extension(target: &str, extensions: &[String]) -> String {
    for ext in extensions {
        if let Some(stripped) = target.strip_suffix(ext.as_str()) {
            return stripped.to_string();
        }
    }
    target.to_string()
}

/// `target` expressed relative to `from_dir` (both project-root-relative),
/// emitting `..` segments where needed and `.` when they coincide.
fn relative_from(from_dir: &Path, target: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = target.components().collect();
    let common = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = PathBuf::new();
    for _ in 0..(from.len() - common) {
        result.push("..");
    }
    for component in &to[common..] {
        result.push(component);
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    crate::path_guard::forward_string(&result)
}

/// The byte range of the leading frontmatter block (delimiters included),
/// or `None` when the document has none. Treated as protected: the builder
/// extracts links from the body only, so a relation or attribute value that
/// happens to contain link syntax is never an edge and must never be
/// rewritten. An unclosed fence propagates as the typed parse error —
/// unreachable for graph-bound candidates (they parsed at build), but a
/// rewriter must never guess at a boundary it cannot establish.
fn frontmatter_range(
    content: &str,
) -> std::result::Result<Option<(usize, usize)>, crate::error::ParseError> {
    let (yaml, body) = crate::parser::frontmatter::split_frontmatter(content)?;
    Ok(yaml.map(|_| (0, content.len() - body.len())))
}

/// The bytes to write over `range` so the parser reads a destination
/// naming `path` with `fragment` there, or `None` when no spelling of it
/// does.
///
/// A destination is an encoding, so rendering one is a choice among
/// spellings and the parser is the only reader whose answer counts: each
/// candidate is written into the document and read back, and the first
/// one returned intact is the one written. That is what makes the write
/// total — a path carrying a space ends a plain destination where the
/// space is, and `[x](old.md)` rewritten to `[x](new name.md)` is no link
/// at all, so before this the edge the build carried was dropped by a
/// write that reported success.
fn accepted_destination(
    content: &str,
    range: std::ops::Range<usize>,
    path: &str,
    fragment: &str,
) -> Option<String> {
    let start = range.start;
    destination_spellings(path, fragment)
        .into_iter()
        .find(|spelling| {
            let candidate = rewritten_line(content, range.clone(), spelling);
            body::Destination::in_document(&candidate)
                .into_iter()
                .any(|destination| {
                    // The destination occupying the written bytes, whether
                    // it is exactly them (a plain spelling), inside them
                    // (a pointy one), or wider (padding the author left
                    // inside the brackets). Destinations never overlap
                    // each other, so overlap names this one.
                    destination.start < start + spelling.len()
                        && destination.end > start
                        && destination.path == path
                        && destination.fragment == fragment
                })
        })
}

/// The spellings `path` + `fragment` can take as a markdown destination,
/// in the order a rewrite prefers them: as the path is written, then with
/// the destination grammar's escapes, then inside pointy brackets. Every
/// name a project walk can produce has one — pointy destinations admit
/// spaces and a backslash admits the delimiters themselves — so the
/// author's plain spelling survives every ordinary rename and an
/// awkward filename still gets repointed instead of left behind.
fn destination_spellings(path: &str, fragment: &str) -> [String; 3] {
    let plain = format!("{path}{fragment}");
    let mut escaped = String::with_capacity(plain.len());
    for character in plain.chars() {
        if matches!(character, '\\' | '(' | ')' | '<' | '>') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    let pointy = format!("<{escaped}>");
    [plain, escaped, pointy]
}

/// `text` with `range` replaced by `replacement`.
fn rewritten_line(text: &str, range: std::ops::Range<usize>, replacement: &str) -> String {
    let mut out = String::with_capacity(text.len() + replacement.len());
    out.push_str(&text[..range.start]);
    out.push_str(replacement);
    out.push_str(&text[range.end..]);
    out
}

/// Whether `[start, end)` overlaps any protected `(s, e)` range.
fn overlaps(start: usize, end: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(s, e)| start < e && end > s)
}

/// The byte range of `content[start..end]` with surrounding whitespace
/// excluded — the slice the builder actually binds for a padded capture
/// (`[[ a ]]` → `a`). Uses the same Unicode [`char::is_whitespace`]
/// semantics as the `str::trim()` the extractor applies, so the two
/// agree on padded captures down to exotic spaces (NBSP, …). `None`
/// when the span is entirely whitespace (nothing to rewrite).
fn trim_span(content: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let slice = &content[start..end];
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        return None;
    }
    let offset = trimmed.as_ptr() as usize - slice.as_ptr() as usize;
    Some((start + offset, start + offset + trimmed.len()))
}

/// Apply non-overlapping `(start, end, replacement)` edits, splicing each
/// replacement in for its byte span. Edits are sorted by start; any that
/// would overlap an earlier one is skipped (two patterns can't claim the
/// same target span without one being spurious).
fn apply_edits(content: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|&(start, ..)| start);
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0;
    for (start, end, replacement) in edits {
        if start < cursor {
            continue;
        }
        out.push_str(&content[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&content[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LinkPattern;

    fn parser() -> ParserConfig {
        ParserConfig::default()
    }

    fn rewrite(content: &str, source_dir: &str, old: &str, new: &str, p: &ParserConfig) -> String {
        // Pre-move scope: the binding is resolved as it was before the
        // move, so `old` is in scope (and `new` is not).
        rewrite_references(
            content,
            Path::new(source_dir),
            Path::new(old),
            Path::new(new),
            &scope(&[old]),
            p,
        )
        .unwrap()
        .unwrap_or_else(|| content.to_string())
    }

    #[test]
    fn rewrites_root_relative_markdown_link() {
        let out = rewrite(
            "See [x](docs/a.md).",
            "docs",
            "docs/a.md",
            "docs/b.md",
            &parser(),
        );
        assert_eq!(out, "See [x](docs/b.md).");
    }

    #[test]
    fn rewrites_padded_wikilink_preserving_surrounding_whitespace() {
        // The builder binds the *trimmed* capture (`[[ a ]]` → `a`), so
        // the rewriter must resolve and replace the same trimmed slice
        // — leaving the author's surrounding padding verbatim.
        let mut p = parser();
        p.wikilink_enabled = true;
        let out = rewrite("[[ a ]]", "", "a.md", "b.md", &p);
        assert_eq!(out, "[[ b ]]");
    }

    #[test]
    fn rewrites_source_relative_markdown_link_preserving_style() {
        // Written file-relative from `docs/`, must stay file-relative.
        let out = rewrite("[x](a.md)", "docs", "docs/a.md", "docs/b.md", &parser());
        assert_eq!(out, "[x](b.md)");
    }

    #[test]
    fn recomputes_relative_link_across_directories() {
        // `../guides/auth.md` from docs/decisions resolves to
        // docs/guides/auth.md; after the move the relative form is
        // recomputed to the new file, staying relative.
        let out = rewrite(
            "[x](../guides/auth.md)",
            "docs/decisions",
            "docs/guides/auth.md",
            "docs/guides/authn.md",
            &parser(),
        );
        assert_eq!(out, "[x](../guides/authn.md)");
    }

    #[test]
    fn repoints_a_destination_whose_spelling_encodes_its_path() {
        // `old&#x2e;md` and `a\(1\).md` are CommonMark spellings of
        // `docs/a.md`'s name, and the builder binds the path they spell.
        // Resolving the spelling instead found nothing to repoint and
        // answered success over three edges it had just stranded.
        for (before, after) in [
            ("[x](docs/a&#x2e;md)", "[x](docs/b.md)"),
            ("[x](docs/a\\.md)", "[x](docs/b.md)"),
            ("[x](<docs/a&#x2e;md>)", "[x](<docs/b.md>)"),
            (
                "[x][r]\n\n[r]: docs/a&#x2e;md\n",
                "[x][r]\n\n[r]: docs/b.md\n",
            ),
        ] {
            assert_eq!(
                rewrite(before, "docs", "docs/a.md", "docs/b.md", &parser()),
                after,
                "before: {before:?}"
            );
        }
    }

    #[test]
    fn repoints_a_destination_spelling_a_path_that_needs_escaping() {
        // A file whose name carries parens is linked with them escaped,
        // so the span holds `a\(1\).md` where the graph holds `a(1).md`.
        let out = rewrite(
            "[x](docs/a\\(1\\).md)",
            "docs",
            "docs/a(1).md",
            "docs/b.md",
            &parser(),
        );
        assert_eq!(out, "[x](docs/b.md)");
    }

    #[test]
    fn preserves_anchor_fragment() {
        let out = rewrite(
            "[x](docs/a.md#section)",
            "docs",
            "docs/a.md",
            "docs/b.md",
            &parser(),
        );
        assert_eq!(out, "[x](docs/b.md#section)");
    }

    #[test]
    fn does_not_rewrite_links_in_fenced_code() {
        let content = "```\n[x](docs/a.md)\n```\n\n[y](docs/a.md)";
        let out = rewrite(content, "", "docs/a.md", "docs/b.md", &parser());
        assert_eq!(out, "```\n[x](docs/a.md)\n```\n\n[y](docs/b.md)");
    }

    #[test]
    fn does_not_rewrite_links_in_inline_code() {
        let out = rewrite(
            "`[x](docs/a.md)` but [y](docs/a.md)",
            "",
            "docs/a.md",
            "docs/b.md",
            &parser(),
        );
        assert_eq!(out, "`[x](docs/a.md)` but [y](docs/b.md)");
    }

    #[test]
    fn leaves_unrelated_links_untouched() {
        assert!(
            rewrite_references(
                "[x](docs/other.md)",
                Path::new("docs"),
                Path::new("docs/a.md"),
                Path::new("docs/b.md"),
                &scope(&["docs/a.md", "docs/other.md"]),
                &parser(),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn rewrites_extensionless_wikilink_preserving_stem_style() {
        let mut p = parser();
        p.wikilink_enabled = true;
        // `[[guides/auth]]` resolves to guides/auth.md via extension
        // append; rewrite stays extension-less.
        let out = rewrite(
            "[[guides/auth]]",
            "",
            "guides/auth.md",
            "guides/authn.md",
            &p,
        );
        assert_eq!(out, "[[guides/authn]]");
    }

    #[test]
    fn wikilink_display_label_survives() {
        let mut p = parser();
        p.wikilink_enabled = true;
        let out = rewrite(
            "[[guides/auth|see auth]]",
            "",
            "guides/auth.md",
            "guides/authn.md",
            &p,
        );
        assert_eq!(out, "[[guides/authn|see auth]]");
    }

    #[test]
    fn bare_id_wikilink_is_not_rewritten() {
        // `[[adr-001]]` is an id reference, not a path — a file rename
        // must not touch it (the id is kept stable by anchoring).
        let mut p = parser();
        p.wikilink_enabled = true;
        assert!(
            rewrite_references(
                "[[adr-001]]",
                Path::new("docs"),
                Path::new("docs/old.md"),
                Path::new("docs/new.md"),
                &scope(&["docs/old.md"]),
                &p,
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn rewrites_custom_pattern_target_only() {
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"@import\s+(\S+)".to_string(),
                relation: "imports".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        let out = rewrite(
            "@import scripts/a.md here",
            "",
            "scripts/a.md",
            "scripts/b.md",
            &p,
        );
        assert_eq!(out, "@import scripts/b.md here");
    }

    #[test]
    fn ignores_absolute_targets() {
        assert!(
            rewrite_references(
                "[x](/etc/a.md)",
                Path::new(""),
                Path::new("etc/a.md"),
                Path::new("etc/b.md"),
                &scope(&["etc/a.md"]),
                &parser(),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn frontmatter_link_syntax_is_not_rewritten() {
        // A frontmatter value that happens to contain link syntax is never a
        // body edge — only the body link is rewritten.
        let content = "---\nid: doc\nnote: \"[see](docs/a.md)\"\n---\nBody [see](docs/a.md).";
        let out = rewrite(content, "", "docs/a.md", "docs/b.md", &parser());
        assert!(
            out.contains("note: \"[see](docs/a.md)\""),
            "frontmatter untouched: {out}"
        );
        assert!(
            out.contains("Body [see](docs/b.md)."),
            "body rewritten: {out}"
        );
    }

    #[test]
    fn leaves_link_whose_literal_form_binds_another_in_scope_file() {
        // Resolver disagreement: `[x](shared.md)` written in docs/sub
        // binds the ROOT `shared.md` (literal frame wins, exactly as
        // the build resolver). Renaming docs/sub/shared.md must NOT
        // repoint it — the link was never an edge to the renamed file.
        assert!(
            rewrite_references(
                "[x](shared.md)",
                Path::new("docs/sub"),
                Path::new("docs/sub/shared.md"),
                Path::new("docs/sub/renamed.md"),
                // Pre-move scope: old path present, root shadow present.
                &scope(&["shared.md", "docs/sub/shared.md", "docs/sub/s.md"]),
                &parser(),
            )
            .unwrap()
            .is_none(),
            "literal binding to a different in-scope file must win over the relative frame"
        );
        // Control: without the shadowing root file, the same relative
        // link does bind the renamed file and is rewritten.
        assert_eq!(
            rewrite(
                "[x](shared.md)",
                "docs/sub",
                "docs/sub/shared.md",
                "docs/sub/renamed.md",
                &parser(),
            ),
            "[x](renamed.md)"
        );
    }

    #[test]
    fn leaves_wikilink_whose_extension_append_shadows_a_bare_sibling() {
        // Extension-append precedence: `[[shared]]` from docs/sub binds
        // the bare `docs/sub/shared` (first candidate in the ladder),
        // NOT `docs/sub/shared.md`. Renaming the `.md` file must leave
        // the wikilink alone — it points at the bare sibling.
        let mut p = parser();
        p.wikilink_enabled = true;
        assert!(
            rewrite_references(
                "[[shared]]",
                Path::new("docs/sub"),
                Path::new("docs/sub/shared.md"),
                Path::new("docs/sub/renamed.md"),
                // Pre-move scope: both the bare sibling and the old .md.
                &scope(&["docs/sub/shared", "docs/sub/shared.md"]),
                &p,
            )
            .unwrap()
            .is_none(),
            "the bare sibling is the first candidate and binds the link — the .md rename must not touch it"
        );
        // Control: without the bare sibling, `[[shared]]` binds the .md
        // and the rename rewrites it.
        assert_eq!(
            rewrite_references(
                "[[shared]]",
                Path::new("docs/sub"),
                Path::new("docs/sub/shared.md"),
                Path::new("docs/sub/renamed.md"),
                &scope(&["docs/sub/shared.md"]),
                &p,
            )
            .unwrap()
            .as_deref(),
            Some("[[renamed]]")
        );
    }

    // ─── rewrite_moved_references ───────────────────────────────────────

    fn scope(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    fn rebase(
        content: &str,
        old_dir: &str,
        new_dir: &str,
        paths: &[&str],
        p: &ParserConfig,
    ) -> std::result::Result<Option<String>, crate::error::ParseError> {
        rewrite_moved_references(
            content,
            Path::new(old_dir),
            Path::new(new_dir),
            &scope(paths),
            p,
        )
    }

    #[test]
    fn rewrites_relative_reference_when_dir_changes() {
        // `../t/auth.md` from a/b binds a/t/auth.md; after the file
        // moves to a/, the same file is `t/auth.md` away.
        let out = rebase("[x](../t/auth.md)", "a/b", "a", &["a/t/auth.md"], &parser()).unwrap();
        assert_eq!(out.as_deref(), Some("[x](t/auth.md)"));
    }

    #[test]
    fn leaves_literal_root_relative_reference_untouched() {
        // The literal interpretation wins (resolver precedence) — a
        // root-relative link never moves with the file.
        assert!(
            rebase("[x](t/auth.md)", "a/b", "a", &["t/auth.md"], &parser())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn leaves_dangling_relative_reference_untouched() {
        // No binding from the old directory → already dangling before
        // the move; a rewrite must never fabricate a resolution.
        assert!(
            rebase(
                "[x](../missing.md)",
                "a/b",
                "a",
                &["a/t/auth.md"],
                &parser()
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn leaves_reference_that_still_binds_from_new_dir() {
        // Sibling-directory move: `../x.md` binds a/x.md from both a/b
        // and a/c — byte-identical output (minimal diff).
        assert!(
            rebase("[x](../x.md)", "a/b", "a/c", &["a/x.md"], &parser())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rebases_when_new_dir_would_bind_a_different_file() {
        // `x.md` from a/ binds a/x.md; from b/ it would silently bind
        // the *other* file b/x.md — the rewrite must repoint to the
        // original binding.
        let out = rebase("[l](x.md)", "a", "b", &["a/x.md", "b/x.md"], &parser()).unwrap();
        assert_eq!(out.as_deref(), Some("[l](../a/x.md)"));
    }

    #[test]
    fn preserves_extensionless_wikilink_style_on_rebase() {
        let mut p = parser();
        p.wikilink_enabled = true;
        let out = rebase(
            "[[../guides/auth]]",
            "docs/sub",
            "docs",
            &["docs/guides/auth.md"],
            &p,
        )
        .unwrap();
        assert_eq!(out.as_deref(), Some("[[guides/auth]]"));
    }

    #[test]
    fn rebase_skips_code_and_frontmatter() {
        let content = "---\nid: doc\nnote: \"[fm](../t/a.md)\"\n---\n\
                       ```\n[code](../t/a.md)\n```\n[real](../t/a.md)\n";
        let out = rebase(content, "a/b", "a", &["a/t/a.md"], &parser())
            .unwrap()
            .expect("changed");
        assert!(
            out.contains("note: \"[fm](../t/a.md)\""),
            "frontmatter: {out}"
        );
        assert!(out.contains("[code](../t/a.md)"), "code fence: {out}");
        assert!(out.contains("[real](t/a.md)"), "body rewritten: {out}");
    }

    #[test]
    fn rebase_ignores_id_only_wikilink() {
        let mut p = parser();
        p.wikilink_enabled = true;
        // `[[adr-001]]` is an id reference; it binds no in-scope path
        // from either directory and must not be touched.
        assert!(
            rebase("[[adr-001]]", "a/b", "a", &["a/t/auth.md"], &p)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rebase_is_noop_when_dir_unchanged() {
        assert!(
            rebase(
                "[x](../t/auth.md)",
                "a/b",
                "a/b",
                &["a/t/auth.md"],
                &parser()
            )
            .unwrap()
            .is_none()
        );
    }

    // ─── rewrite_id_references ──────────────────────────────────────────

    #[test]
    fn id_rewrite_repoints_wikilink_and_custom() {
        let p = ParserConfig {
            wikilink_enabled: true,
            link_patterns: vec![LinkPattern {
                pattern: r"@cite\(([^)]+)\)".to_string(),
                relation: "cites".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        let out = rewrite_id_references(
            "see [[old]] and @cite(old)",
            "old",
            "new",
            Path::new(""),
            &BTreeSet::new(),
            &p,
        )
        .unwrap()
        .expect("changed");
        assert_eq!(out, "see [[new]] and @cite(new)");
    }

    #[test]
    fn id_rewrite_reaches_a_reference_on_an_indented_continuation_line() {
        // Where a line sits in the markdown is not a property of the line:
        // an indented line after a paragraph is paragraph text, and the same
        // line alone is a code block. A guard that judged the rewritten line
        // in isolation refused a reference the build binds.
        let p = ParserConfig {
            wikilink_enabled: true,
            ..ParserConfig::default()
        };
        let out = rewrite_id_references(
            "para line\n    see [[old-id]] more\n",
            "old-id",
            "new-id",
            Path::new(""),
            &BTreeSet::new(),
            &p,
        )
        .unwrap()
        .expect("the continuation is prose, and gets rewritten");
        assert_eq!(out, "para line\n    see [[new-id]] more\n");
    }

    #[test]
    fn a_padded_citation_is_rewritten_as_the_slice_the_build_binds() {
        // The builder binds the trimmed capture, so the rewriter must
        // resolve and replace the same slice — a citation extended raw gave
        // the resolver `" docs/a.md "`, which binds nothing, and the write
        // reported success over a reference it never touched.
        let p = ParserConfig {
            extensions: vec![".md".to_string()],
            link_patterns: vec![LinkPattern {
                pattern: r"@cite\(([^)]+)\)".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }],
            ..ParserConfig::default()
        };
        let out = rewrite_references(
            "see `@cite( docs/a.md )` here",
            Path::new("docs"),
            Path::new("docs/a.md"),
            Path::new("docs/b.md"),
            &BTreeSet::from(["docs/a.md".to_string()]),
            &p,
        )
        .unwrap()
        .expect("the padded citation is repointed");
        assert_eq!(out, "see `@cite( docs/b.md )` here");
    }

    #[test]
    fn a_padded_id_reference_is_rewritten_as_the_slice_the_build_binds() {
        // The builder binds the trimmed capture, so a pattern whose capture
        // carries its own padding bound `old-id` while the rewriter replaced
        // `" old-id "` whole — producing text the pattern no longer matches,
        // which the round-trip guard then declined. The correct rewrite,
        // padding preserved, was never tried.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"@cite\(( \S+ )\)".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        let out = rewrite_id_references(
            "@cite( old-id )\n",
            "old-id",
            "new-id",
            Path::new(""),
            &BTreeSet::new(),
            &p,
        )
        .unwrap()
        .expect("the padded reference is repointed");
        assert_eq!(out, "@cite( new-id )\n");
    }

    #[test]
    fn a_candidate_whose_frontmatter_cannot_be_read_is_skipped_alone() {
        // The boundary check folded "unreadable" into "no frontmatter",
        // which equals a frontmatterless original — so the corrupt
        // candidate passed the guard and reached the gate, which refuses
        // the whole batch. A span that cannot round-trip is skipped by
        // itself; everything else in the document still rewrites.
        let p = ParserConfig {
            wikilink_enabled: true,
            link_patterns: vec![LinkPattern {
                pattern: r"^(\S+)$".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        let out = rewrite_id_references(
            "old-id\nsee [[old-id]] too\n",
            "old-id",
            "---",
            Path::new(""),
            &BTreeSet::new(),
            &p,
        )
        .unwrap()
        .expect("the wikilink still rewrites");
        assert_eq!(
            out, "old-id\nsee [[---]] too\n",
            "the line-1 span alone is left, having nowhere safe to land"
        );
    }

    #[test]
    fn a_path_the_plain_spelling_cannot_carry_is_spelled_another_way() {
        // A destination carrying a space ends where the space is, so a
        // plain replacement was not a link at all — first written out of
        // existence, then declined and left pointing at a file that had
        // moved. Both answered success over an edge the build had. The
        // pointy spelling carries it, and the parser is what says so.
        let p = ParserConfig {
            extensions: vec![".md".to_string()],
            ..ParserConfig::default()
        };
        for (new_path, after) in [
            ("new name.md", "[Old](<new name.md>)"),
            ("new(1).md", "[Old](new(1).md)"),
            ("new(1.md", "[Old](new\\(1.md)"),
        ] {
            let out = rewrite_references(
                "[Old](old.md)",
                Path::new(""),
                Path::new("old.md"),
                Path::new(new_path),
                &BTreeSet::from(["old.md".to_string()]),
                &p,
            )
            .unwrap()
            .expect("the link is repointed rather than left behind");
            assert_eq!(out, after, "new_path: {new_path:?}");
        }
    }

    #[test]
    fn a_pointy_destination_keeps_its_brackets_when_the_path_needs_them() {
        // The span sits inside the author's `<…>`, so the escaped spelling
        // is the one that reads back — a second pair of brackets would not.
        let p = ParserConfig {
            extensions: vec![".md".to_string()],
            ..ParserConfig::default()
        };
        let out = rewrite_references(
            "[Old](<old.md>)",
            Path::new(""),
            Path::new("old.md"),
            Path::new("new name.md"),
            &BTreeSet::from(["old.md".to_string()]),
            &p,
        )
        .unwrap()
        .expect("the link is repointed");
        assert_eq!(out, "[Old](<new name.md>)");
    }

    #[test]
    fn a_padded_citation_id_is_repointed_as_the_slice_the_build_binds() {
        // The prose path was taught to edit the trimmed capture; the
        // citation path inside the same function still replaced the padded
        // one, so a padded citation declined its own rewrite.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"@cite\(( \S+ )\)".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }],
            ..ParserConfig::default()
        };
        let out = rewrite_id_references(
            "see `@cite( old-id )`\n",
            "old-id",
            "new-id",
            Path::new(""),
            &BTreeSet::new(),
            &p,
        )
        .unwrap()
        .expect("the padded citation is repointed");
        assert_eq!(out, "see `@cite( new-id )`\n");
    }

    #[test]
    fn a_pointy_destination_padded_inside_its_brackets_is_rewritten() {
        // The builder trims a destination before reading its extension, so
        // `<docs/a.md >` is an edge. The rewriter read the extension off the
        // untrimmed slice, never offered the span, and answered success over
        // a link it left pointing at the old path.
        let p = ParserConfig {
            extensions: vec![".md".to_string()],
            ..ParserConfig::default()
        };
        let out = rewrite_references(
            "see [x](<docs/a.md >) here",
            Path::new(""),
            Path::new("docs/a.md"),
            Path::new("docs/b.md"),
            &BTreeSet::from(["docs/a.md".to_string()]),
            &p,
        )
        .unwrap()
        .expect("the padded destination is repointed");
        assert_eq!(out, "see [x](<docs/b.md >) here");
    }

    #[test]
    fn a_fence_in_the_frontmatter_does_not_hide_a_markdown_link() {
        // `markdown_destination_spans` parsed whatever it was handed, and
        // the rewriter hands it a document: a fence lookalike in a YAML
        // block scalar opened a code block over the whole body, so pulldown
        // emitted no link at all and `rename` answered success with nothing
        // updated.
        let p = ParserConfig {
            extensions: vec![".md".to_string()],
            ..ParserConfig::default()
        };
        let out = rewrite_references(
            "---\nid: linker\nnote: |\n  ```\n---\n\n[x](docs/a.md)\n",
            Path::new(""),
            Path::new("docs/a.md"),
            Path::new("docs/b.md"),
            &BTreeSet::from(["docs/a.md".to_string()]),
            &p,
        )
        .unwrap()
        .expect("the link is repointed");
        assert!(out.ends_with("[x](docs/b.md)\n"), "{out:?}");
    }

    #[test]
    fn id_rewrite_skips_a_successor_that_would_make_the_body_frontmatter() {
        // The guard asks whether the rewritten text is still this
        // reference. It has to ask whether the rewritten document is still
        // this document: a successor reading as a delimiter at the top of a
        // frontmatterless file turns the lines under it into frontmatter,
        // so the next build reads somebody else's id off the document and
        // the references below it are gone.
        let p = ParserConfig {
            wikilink_enabled: true,
            link_patterns: vec![LinkPattern {
                pattern: r"^(\S+)$".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        assert!(
            rewrite_id_references(
                "old-id\nid: accidental\n---\nsee [[other]]\n",
                "old-id",
                "---",
                Path::new(""),
                &BTreeSet::new(),
                &p,
            )
            .unwrap()
            .is_none(),
            "the document keeps its own identity"
        );
    }

    #[test]
    fn id_rewrite_skips_a_successor_that_would_fence_off_what_follows() {
        // A successor that reads as a fence opener closes over every
        // reference after it, so the next build binds none of them — a
        // write that reported success erasing edges it never named. The
        // prose guard asks the protected surface about the rewritten line,
        // as the citation guard asks about the rewritten span.
        let p = ParserConfig {
            wikilink_enabled: true,
            link_patterns: vec![LinkPattern {
                pattern: r"^(\S+)$".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        assert!(
            rewrite_id_references(
                "old-id\n\nsee [[other]]\n",
                "old-id",
                "~~~",
                Path::new(""),
                &BTreeSet::new(),
                &p,
            )
            .unwrap()
            .is_none(),
            "the reference stays visible rather than fencing off the rest"
        );
    }

    #[test]
    fn id_rewrite_skips_a_citation_the_successor_would_close() {
        // A successor id carrying a backtick ends the code span around it,
        // so the rewritten line reads as prose the next build binds
        // nothing from — a bound edge erased by a write that reported
        // success. The round-trip guard asks the protected-surface verdict
        // about the rewritten text, not only whether the pattern still
        // captures, so the citation is left alone.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"(old-id|new`id)".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }],
            ..ParserConfig::default()
        };
        assert!(
            rewrite_id_references(
                "cite `old-id`",
                "old-id",
                "new`id",
                Path::new(""),
                &BTreeSet::new(),
                &p,
            )
            .unwrap()
            .is_none(),
            "the citation is preserved verbatim rather than closed"
        );
    }

    #[test]
    fn id_rewrite_skips_a_span_that_cannot_round_trip() {
        // A successor id carrying the pattern's own delimiter (`)` for
        // `@cite(...)`) would re-parse as a different id — the span is
        // left untouched instead of silently corrupted, while syntaxes
        // the id is safe for (the wikilink) still rewrite.
        let p = ParserConfig {
            wikilink_enabled: true,
            link_patterns: vec![LinkPattern {
                pattern: r"@cite\(([^)]+)\)".to_string(),
                relation: "cites".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        let out = rewrite_id_references(
            "see [[old]] and @cite(old)",
            "old",
            "paren)id",
            Path::new(""),
            &BTreeSet::new(),
            &p,
        )
        .unwrap()
        .expect("the wikilink still rewrites");
        assert_eq!(
            out, "see [[paren)id]] and @cite(old)",
            "the un-round-trippable @cite span is preserved verbatim"
        );
    }

    #[test]
    fn id_rewrite_requires_exact_match() {
        let mut p = parser();
        p.wikilink_enabled = true;
        // `[[old-spec]]` must not match id `old` (substring), and prose `old`
        // is never a capture.
        assert!(
            rewrite_id_references(
                "[[old-spec]] and old in prose",
                "old",
                "new",
                Path::new(""),
                &BTreeSet::new(),
                &p
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn id_rewrite_leaves_a_capture_that_binds_a_file_by_path() {
        // `[[old]]` next to an in-scope file `old.md` is a *path* edge to
        // that file (resolver path-first precedence), not an id reference
        // — id retargeting must leave it alone even though the text
        // equals `old_id`.
        let mut p = parser();
        p.wikilink_enabled = true;
        p.extensions = vec![".md".into()];
        let scope: BTreeSet<String> = ["old.md".to_string()].into_iter().collect();
        assert!(
            rewrite_id_references("see [[old]]", "old", "new", Path::new(""), &scope, &p)
                .unwrap()
                .is_none(),
            "a path-bound wikilink must not be retargeted as an id"
        );
        // With no file `old.md` in scope, the same wikilink is a genuine
        // id reference and is retargeted.
        assert_eq!(
            rewrite_id_references(
                "see [[old]]",
                "old",
                "new",
                Path::new(""),
                &BTreeSet::new(),
                &p
            )
            .unwrap()
            .as_deref(),
            Some("see [[new]]")
        );
    }

    #[test]
    fn id_rewrite_repoints_full_content_code_span_under_code_spans_pattern() {
        // The builder binds `` `old-id` `` as an edge under a `code_spans`
        // pattern, so retarget must repoint the same span — backticks
        // preserved, partial-match and fenced samples untouched.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"\b(old-id|new-id)\b".to_string(),
                relation: "references".to_string(),
                code_spans: true,
            }],
            ..ParserConfig::default()
        };
        let content = "cite `old-id`\nrun `use old-id here`\n```\n`old-id`\n```\nbare old-id\n";
        let out = rewrite_id_references(
            content,
            "old-id",
            "new-id",
            Path::new(""),
            &BTreeSet::new(),
            &p,
        )
        .unwrap()
        .expect("changed");
        assert_eq!(
            out,
            "cite `new-id`\nrun `use old-id here`\n```\n`old-id`\n```\nbare new-id\n"
        );
    }

    #[test]
    fn id_rewrite_skips_inline_code_and_fences() {
        let mut p = parser();
        p.wikilink_enabled = true;
        // A wikilink inside an inline code span or a fenced block is a sample,
        // not a reference — a mutating rewrite must leave it alone.
        let content = "real [[old]]\n`[[old]]`\n```\n[[old]]\n```\n";
        let out = rewrite_id_references(content, "old", "new", Path::new(""), &BTreeSet::new(), &p)
            .unwrap()
            .expect("changed");
        assert_eq!(out, "real [[new]]\n`[[old]]`\n```\n[[old]]\n```\n");
    }

    #[test]
    fn id_rewrite_leaves_frontmatter_untouched() {
        // A wikilink in a frontmatter value is not a body edge — only the body
        // reference is repointed.
        let mut p = parser();
        p.wikilink_enabled = true;
        let content = "---\nid: doc\nnote: \"[[old]]\"\n---\nBody [[old]].";
        let out = rewrite_id_references(content, "old", "new", Path::new(""), &BTreeSet::new(), &p)
            .unwrap()
            .expect("changed");
        assert!(
            out.contains("note: \"[[old]]\""),
            "frontmatter untouched: {out}"
        );
        assert!(out.contains("Body [[new]]."), "body rewritten: {out}");
    }
}
