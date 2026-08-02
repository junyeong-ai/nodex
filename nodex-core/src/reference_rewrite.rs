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

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::builder::resolver::{Bindings, Frame, PathBinding, Worlds, path_binding};
use crate::config::ParserConfig;
use crate::parser::body;
use crate::parser::body::trim_span;

/// Rewrite every body reference in `content` that a move would otherwise
/// leave naming something else, returning the rewritten document — or
/// `None` when nothing changed.
///
/// One rule covers the whole move, because there is only one thing a
/// rename owes a reference: **it must go on naming the document it named**.
/// Which of the two files moved is not a second question — a reference
/// names a document, the move gives that document a new path or gives the
/// referring file a new vantage point, and either way the spelling is
/// recomputed from what it named. Split in two, the moved document's own
/// references were rewritten twice over one buffer, and the second pass
/// read the first's output as the text its author had written.
///
/// `rewriting` says which document of the move this content is, which is
/// where it stood when its text was written and where it stands after —
/// and, for the document the move carries, that a reference of its own to
/// itself still names itself wherever it lands. `old_path` / `new_path`
/// are that document's project-root-relative paths.
///
/// Markdown links, `[[wikilinks]]` (when enabled), and
/// `[[parser.link_patterns]]` custom references are all handled. The
/// author's style survives: a root-relative reference stays root-relative
/// (and is move-invariant unless what it named moved), a source-relative
/// one is recomputed from `to_dir`, and an extension-less reference (a
/// wikilink resolved by appending a configured extension) stays
/// extension-less. Left alone, deliberately: a reference that binds no
/// document (already dangling — a rewrite must never fabricate a
/// resolution), a bare-id reference, and anything inside code blocks,
/// inline code spans, or frontmatter.
pub fn rewrite_for_move(
    content: &str,
    rewriting: Rewriting<'_>,
    old_path: &Path,
    new_path: &Path,
    worlds: Worlds<'_>,
    parser: &ParserConfig,
) -> std::result::Result<Rewritten, crate::error::ParseError> {
    // Resolve and rewrite against the same canonical text the builder
    // parsed (BOM strip, CRLF/CR → LF), so a Windows-line-ending file's
    // frontmatter is split — and its references located — identically.
    let content = crate::parser::frontmatter::canonicalize(content);
    let old_norm = crate::path_guard::forward_string(old_path);
    let new_norm = crate::path_guard::forward_string(new_path);
    let parent = |path: &'_ Path| path.parent().unwrap_or(Path::new("")).to_path_buf();
    let (from_dir, to_dir) = match rewriting {
        Rewriting::Referrer(dir) => (dir.to_path_buf(), dir.to_path_buf()),
        Rewriting::Moved => (parent(old_path), parent(new_path)),
    };
    let moving = Moving {
        rewriting,
        from_dir: from_dir.as_path(),
        to_dir: to_dir.as_path(),
        old_norm: &old_norm,
        new_norm: &new_norm,
    };
    let (from_dir, to_dir) = (moving.from_dir, moving.to_dir);
    let References { frontmatter, spans } = references(&content, parser)?;
    let proposals: Vec<Proposal> = spans
        .into_iter()
        .map(|span| {
            let repoint = moved_target(
                &span.target,
                &span.form,
                &moving,
                worlds,
                &parser.extensions,
            );
            Proposal {
                binds: binding(&span.target, from_dir, worlds.before, parser).is_some(),
                repoint,
                span,
            }
        })
        .collect();
    let names = |text: &str| binding(text, to_dir, worlds.after, parser);
    let was = |text: &str| binding(text, from_dir, worlds.before, parser);
    Ok(apply_proposals(
        &content,
        frontmatter,
        proposals,
        parser,
        &names,
        &was,
    ))
}

/// The move one document's references are read against: which of the two
/// documents this is, where it stands before and after, and the paths the
/// move carries a document between.
struct Moving<'a> {
    rewriting: Rewriting<'a>,
    from_dir: &'a Path,
    to_dir: &'a Path,
    old_norm: &'a str,
    new_norm: &'a str,
}

/// Which document of a move a rewrite is reading.
///
/// The two stand in different places before and after, and one of them
/// carries a fact no pair of directories can: a reference the moved
/// document makes to itself names itself wherever it lands, whatever the
/// project comes to call it. Every other reference names some other
/// document, and what the move leaves standing at a path is not evidence
/// about which document that is.
#[derive(Clone, Copy)]
pub enum Rewriting<'a> {
    /// A document the move leaves where it is, standing in this
    /// directory.
    Referrer(&'a Path),
    /// The document the move carries.
    Moved,
}

/// What a rewrite did to one document.
#[derive(Debug, Default)]
pub struct Rewritten {
    /// The rewritten document, or `None` when nothing changed.
    pub content: Option<String>,
    /// References the rewrite leaves naming a different document — see
    /// [`Rebound`].
    pub rebound: Vec<Rebound>,
    /// References the rewrite had a replacement for and left standing, as
    /// they are spelled — the ones [`Rebound`] does not already speak for.
    ///
    /// Disjoint from it by construction, so every reference the rewrite
    /// has something to say about is named exactly once. What is left
    /// here binds nothing where it now sits: a repoint is only ever
    /// proposed for a reference the mutation stops naming what it named,
    /// so one that was refused either comes to name somebody else — which
    /// is a [`Rebound`] — or comes to name nothing at all.
    ///
    /// Which is to say it will surface as an unresolved edge, on the next
    /// build, at whatever severity the project's
    /// `[[detection.unresolved_policy]]` gives that cause. That is a
    /// report about the project, arriving later and phrased as a fact
    /// about a document; the command that left the reference is the only
    /// thing that can say it *declined* to repoint it, and the only place
    /// that is known.
    pub refused: Vec<String>,
}

/// A reference the move left spelled as it was, which names a different
/// document read from where the file now sits.
///
/// Leaving a reference alone is safe when what it named moved: it comes to
/// dangle, and `check` reports it. It is not safe when the *referring*
/// document moved, because a relative reference means whatever it means
/// from where it sits — `@ref(x)` beside `a/x.md` binds `b/x.md` once the
/// file is in `b/`. The graph that results is valid, so nothing downstream
/// has a reason to mention it, which is why this is carried out of the
/// rewrite rather than left to be noticed.
#[derive(Debug, Clone)]
pub struct Rebound {
    /// The reference as it is spelled.
    pub reference: String,
    /// The document it named before the move, or `None` where it named
    /// nothing — a dangling reference the move makes bind, which clears
    /// the very unresolved edge that would otherwise have reported it.
    pub was: Option<String>,
    /// The document it names now.
    pub now: String,
}

/// One rewritable reference: the slice to replace, the target the builder
/// bound from it, and the reader that found it — which is what a
/// replacement has to satisfy before it may be written. Every body
/// reference is a *document reference* (resolved with extension-append
/// and id-fallback) by construction — the path-only `covers` relation
/// exists only in frontmatter, which is never scanned here, and
/// `Config::validate` keeps it off link patterns.
struct ReferenceSpan {
    start: usize,
    end: usize,
    /// The reference the builder bound from this span. For every form but
    /// one it is the span's own text; a markdown destination *encodes*
    /// its path (`a\(1\).md` spells `a(1).md`), and the parser's reading
    /// of that encoding is what the build bound.
    target: String,
    /// The relation the builder binds this span under. Two references
    /// over one document naming one target under one relation are one
    /// edge, however many times the document spells it — so losing one
    /// of them costs a span and not an edge.
    relation: String,
    form: ReferenceForm,
}

/// How a reference was found, and so what it takes to read one back.
///
/// A rewrite is a proposal: the bytes go in only when the reader that
/// found the reference finds the intended target in their place. Carrying
/// the reader on the span is what makes that one question — three
/// rewriting entry points asked three, each holding the half of the
/// answer its own defect had taught it, and the halves kept being the
/// same half.
enum ReferenceForm {
    /// A standard markdown link destination. Its bytes *spell* the path,
    /// so the target arrives decoded, the `#fragment` rides along, and
    /// the replacement is chosen from the spellings the parser reads
    /// back.
    Destination { fragment: String },
    /// A `[[wikilink]]` or `[[parser.link_patterns]]` capture in prose.
    /// The surrounding syntax is the document's own, so the target is
    /// written as it reads and the pattern has to re-capture exactly it:
    /// a target carrying one of the pattern's delimiters (a `]` inside
    /// `[[…]]`, a `)` inside `@cite(…)`) otherwise becomes a reference
    /// the next build reads as a different one, or as none.
    Capture(Regex),
    /// A `code_spans` pattern's whole-span citation. A span is one text,
    /// so the guard is that the rewritten span is still a citation — a
    /// target carrying a backtick closes the span around itself.
    Citation(Regex),
}

impl ReferenceForm {
    /// The spellings of `target` to offer for this form, in the order a
    /// rewrite prefers them. Only a destination has a choice; every other
    /// form is written as the target reads.
    fn spellings(&self, target: &str) -> Vec<String> {
        match self {
            Self::Destination { fragment } => destination_spellings(target, fragment).to_vec(),
            Self::Capture(_) | Self::Citation(_) => vec![target.to_string()],
        }
    }

    /// Whether `candidate` reads the bytes at `written` back as a
    /// reference to `target`.
    fn reads_back(
        &self,
        reading: &Reading<'_>,
        landing: &Landing,
        names: &dyn Fn(&str) -> bool,
    ) -> bool {
        if matches!(landing, Landing::Severed) {
            return false;
        }
        let candidate = reading.text;
        let written = landing.range();
        match self {
            // The destination occupying the written bytes, whether that is
            // exactly them (a plain spelling), inside them (a pointy one),
            // or wider than them (padding the author left inside the
            // brackets). Destinations never overlap each other, so overlap
            // names this one.
            Self::Destination { fragment } => reading.destinations().iter().any(|destination| {
                destination.start < written.end
                    && destination.end > written.start
                    && names(&destination.path)
                    && destination.fragment == *fragment
            }),
            // Where a line sits in the markdown is not a property of the
            // line: an indented continuation is paragraph text after a
            // paragraph and a code block alone, so the surface is asked of
            // the document the write would produce rather than of the line
            // lifted out of it.
            Self::Capture(pattern) => {
                let line = line_range(candidate, written.start);
                pattern
                    .captures_iter(&candidate[line.clone()])
                    .filter_map(|caps| caps.get(1))
                    .filter_map(|capture| {
                        trim_span(
                            candidate,
                            line.start + capture.start(),
                            line.start + capture.end(),
                        )
                    })
                    .any(|span| landing.holds(candidate, span, names))
                    && reading.surfaces().in_prose(written.start, written.end)
            }
            Self::Citation(pattern) => reading
                .surfaces()
                .citations(candidate, pattern)
                .into_iter()
                .filter_map(|(start, end)| trim_span(candidate, start, end))
                .any(|span| landing.holds(candidate, span, names)),
        }
    }
}

/// Where a reference is to be read back.
///
/// A rewrite writes bytes, and the reference it wrote is read *at* them. A
/// reference the rewrite *covered* has no bytes of its own left — the
/// rewrite replaced the text it was made of — so it is read anywhere
/// *within* what the rewrite wrote, by what it says rather than by where
/// it sits. `docs/a.md` repointed to `docs2/a.md` still spells `a.md`;
/// repointed to `docs/b.md` it does not, and only then was it the
/// coverer's to subsume.
///
/// Either way the reference is read by its own reader — the pattern over
/// its line for a capture, the citation probe for a code span, the
/// markdown parse for a destination — which is the reader extraction runs
/// too. So a reference that reads back is one the finished document goes
/// on to bind, and only a *subsumed* one can cost an edge, which is the
/// trade this names. Reading within is a question about a set rather than
/// a multiset — two covered references spelled alike share one surviving
/// instance, as in a file named `a.md,a.md` renamed to `a.md` — and that
/// costs spans, never edges, whatever the pair has in common.
enum Landing {
    At(std::ops::Range<usize>),
    Within(std::ops::Range<usize>),
    /// Nowhere. A rewrite replaced part of the bytes it was read out of
    /// and not all of them, so no run of this document is it: what is
    /// left of its own text no longer joins up, and what replaced the
    /// rest was written for something else.
    Severed,
}

impl Landing {
    fn range(&self) -> std::ops::Range<usize> {
        match self {
            Self::At(range) | Self::Within(range) => range.clone(),
            Self::Severed => 0..0,
        }
    }

    /// Whether a capture found at `span` is the reference asked for.
    ///
    /// Both readings ask what the capture says, not only where it is. A
    /// reference is read at its own bytes only while those bytes are its
    /// own: a rewrite accepted *inside* an earlier-starting capture that
    /// nothing chose replaces that capture's text, and a position-only
    /// answer vouched for it whenever the successor happened to be the
    /// same length as what it replaced.
    fn holds(&self, text: &str, span: (usize, usize), names: &dyn Fn(&str) -> bool) -> bool {
        let says = names(&text[span.0..span.1]);
        match self {
            Self::At(range) => span == (range.start, range.end) && says,
            Self::Within(range) => span.0 >= range.start && span.1 <= range.end && says,
            Self::Severed => false,
        }
    }
}

/// One reading of a candidate document — what its markdown says about
/// where references are — taken once so every rewrite in a batch is
/// confirmed against the same one, and taken only as far as the forms
/// present actually ask.
struct Reading<'a> {
    text: &'a str,
    destinations: std::cell::OnceCell<Vec<body::Destination>>,
    surfaces: std::cell::OnceCell<body::ProtectedSurfaces>,
}

impl<'a> Reading<'a> {
    fn of(text: &'a str) -> Self {
        Self {
            text,
            destinations: std::cell::OnceCell::new(),
            surfaces: std::cell::OnceCell::new(),
        }
    }

    fn destinations(&self) -> &[body::Destination] {
        self.destinations
            .get_or_init(|| body::Destination::in_document(self.text))
    }

    fn surfaces(&self) -> &body::ProtectedSurfaces {
        self.surfaces
            .get_or_init(|| body::ProtectedSurfaces::of_document(self.text))
    }
}

/// Whether `target`, read from `source`, names a document — asked of the
/// ladder the graph binds edges with, so a rewrite and the builder agree
/// about which references carry an edge.
fn binds(target: &str, source_dir: &Path, bindings: &Bindings, parser: &ParserConfig) -> bool {
    binding(target, source_dir, bindings, parser).is_some()
}

/// Which document `target`, read from `source`, names — the ladder's own
/// answer, so a reference is judged by what it binds rather than by how
/// it is spelled. A path has more than one spelling and they are not the
/// same string; they are the same document.
fn binding(
    target: &str,
    source_dir: &Path,
    bindings: &Bindings,
    parser: &ParserConfig,
) -> Option<String> {
    crate::builder::resolver::resolve_target(
        target,
        crate::model::edge::BODY_REFERENCE_RELATION,
        source_dir,
        bindings,
        &parser.extensions,
    )
    .id()
    .map(str::to_string)
}

/// A reference and what a rewrite has for it.
struct Proposal {
    span: ReferenceSpan,
    /// What the mutation gives it to say instead, if anything.
    repoint: Option<Repoint>,
    /// Whether it names a document as the project stands. An edge a
    /// rewrite over its bytes would cost, which is why such a rewrite may
    /// not simply take it — see `take`.
    binds: bool,
}

/// A replacement, and the document it has to name for the write to be
/// accepted.
///
/// The two are one thing. A rendering is a frame as much as a path and
/// the resolver tries the literal frame first, so a source-relative
/// spelling can be shadowed by a root-relative document of the same name:
/// read as text it passes while binding somebody else. What it must name
/// is not recoverable from the rendering — the rendering is the thing in
/// doubt — and the seams mean different things by it, a move meaning the
/// document the reference named and a retarget the successor. Held apart,
/// a rewrite that could not say what it meant fell back to reading its
/// own text, and every shadow passed.
///
/// The document is the one named *before* the mutation, never the one
/// standing where the write points afterwards: asked of the world the
/// mutation leaves, the question answers itself. A relative symlink moved
/// across directories resolves to different bytes, an evicted target
/// leaves its path to a shadow — in both the destination holds a
/// different document, and a gate reading the destination calls the swap
/// a match.
struct Repoint {
    /// What the reference may say instead, in the order the rewrite
    /// prefers them. A path has spellings by frame as it has spellings by
    /// encoding, and which of them *names* the document is not knowable
    /// where they are made: a rendering is judged where the reader that
    /// found the reference reads it back. So both frames are offered and
    /// the gate takes the first that names `intends` — the author's own
    /// first, so a reference that would go on naming what it named never
    /// changes how it is spelled.
    spellings: Vec<String>,
    intends: String,
}

/// The rewritable references of one document, and the frontmatter
/// boundary every rewrite has to leave where it is.
struct References {
    frontmatter: Option<(usize, usize)>,
    spans: Vec<ReferenceSpan>,
}

/// Every rewritable body reference in `content`, extracted exactly as the
/// builder does (`parser::body::extract_links`): standard markdown link
/// destinations via pulldown-cmark (so pointy `<url>` and titled
/// `(url "t")` forms are handled identically), plus `[[wikilink]]` and
/// `parser.link_patterns` captures scanned per line, plus each
/// `code_spans` pattern's whole-span citations. The frontmatter block is
/// excluded outright, and code is judged by the shared
/// `body::ProtectedSurfaces` verdict — blocks always opaque, an inline
/// code span reachable only as a citation — so a mutating rewrite reaches
/// exactly the spans the builder binds as edges, and nothing else.
///
/// Each span is the *trimmed* slice — the builder binds the trimmed
/// capture (`[[ a ]]` → `a`), so the rewriter replaces the same slice and
/// surrounding whitespace is preserved verbatim. What that slice means is
/// a second question: a markdown destination spells its path
/// (`docs/a&#x2e;md`) and carries its `#fragment`, so it arrives already
/// read by the parser that bound the edge, while every other form is its
/// own target.
fn references(
    content: &str,
    parser: &ParserConfig,
) -> std::result::Result<References, crate::error::ParseError> {
    let protected = body::ProtectedSurfaces::of_document(content);
    let frontmatter: Vec<(usize, usize)> = frontmatter_range(content)?.into_iter().collect();
    let mut spans: Vec<ReferenceSpan> = Vec::new();
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
                relation: crate::model::edge::BODY_REFERENCE_RELATION.to_string(),
                form: ReferenceForm::Destination {
                    fragment: destination.fragment,
                },
            });
        }
    }

    // Wikilinks and custom patterns: line-anchored regex captures, plus —
    // for a `code_spans` pattern — the inline code spans it accounts for
    // whole.
    let mut patterns: Vec<(Regex, &str, bool)> = Vec::new();
    if parser.wikilink_enabled {
        patterns.push((
            body::wikilink_regex().clone(),
            crate::model::edge::BODY_REFERENCE_RELATION,
            false,
        ));
    }
    for pattern in &parser.link_patterns {
        patterns.push((
            Regex::new(&pattern.pattern).expect("link patterns validated by Config::load"),
            pattern.relation.as_str(),
            pattern.code_spans,
        ));
    }
    for (pattern, relation, code_spans) in &patterns {
        scan_line_captures(content, pattern, &mut |start, end| {
            if let Some((start, end)) = admit(start, end) {
                spans.push(ReferenceSpan {
                    start,
                    end,
                    target: content[start..end].to_string(),
                    relation: relation.to_string(),
                    form: ReferenceForm::Capture(pattern.clone()),
                });
            }
        });
        if !code_spans {
            continue;
        }
        for (start, end) in protected.citations(content, pattern) {
            if let Some((start, end)) = trim_span(content, start, end)
                && !overlaps(start, end, &frontmatter)
            {
                spans.push(ReferenceSpan {
                    start,
                    end,
                    target: content[start..end].to_string(),
                    relation: relation.to_string(),
                    form: ReferenceForm::Citation(pattern.clone()),
                });
            }
        }
    }
    Ok(References {
        frontmatter: frontmatter.first().copied(),
        spans,
    })
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
/// returning what the rewrite did — the document when it changed, and
/// every reference it had a replacement for and could not write.
///
/// Ids appear in the body only as `[[wikilink]]` or
/// `[[parser.link_patterns]]` targets, so a markdown destination — which
/// spells a path — is passed over; every other reference the document
/// carries is a candidate. The capture must equal `old_id` verbatim, and
/// — mirroring the build resolver's path-first precedence — it is an id
/// reference only when it does **not** bind an in-scope file: `[[old]]`
/// next to a file `old.md` resolves to that file (a path edge), so id
/// retargeting leaves it alone. `source_dir` is the scanned file's parent
/// directory; `bound` is the project the references are read against.
pub fn rewrite_id_references(
    content: &str,
    old_id: &str,
    new_id: &str,
    source_dir: &Path,
    bound: &Bindings,
    parser: &ParserConfig,
) -> std::result::Result<Rewritten, crate::error::ParseError> {
    // The capture is an id reference only where the resolver falls
    // through to the bare-id step, which is the resolver's own question:
    // anything its path rungs answer — a file by either frame, an
    // absolute spelling, a frame that leaves the root — never reaches
    // that step, so retargeting it would rewrite text the build binds as
    // something else.
    let falls_through_to_ids = |capture: &str| {
        matches!(
            crate::builder::resolver::path_binding(
                capture,
                source_dir,
                bound,
                &parser.extensions,
                true,
            ),
            crate::builder::resolver::PathBinding::Unbound
        )
    };

    let content = crate::parser::frontmatter::canonicalize(content);
    let content = content.as_ref();
    let References { frontmatter, spans } = references(content, parser)?;
    let proposals: Vec<Proposal> = spans
        .into_iter()
        .map(|span| {
            let retargeted = !matches!(span.form, ReferenceForm::Destination { .. })
                && span.target == old_id
                && falls_through_to_ids(&span.target);
            Proposal {
                binds: binds(&span.target, source_dir, bound, parser),
                repoint: retargeted.then(|| Repoint {
                    spellings: vec![new_id.to_string()],
                    intends: new_id.to_string(),
                }),
                span,
            }
        })
        .collect();
    let names = |text: &str| binding(text, source_dir, bound, parser);
    Ok(apply_proposals(
        content,
        frontmatter,
        proposals,
        parser,
        &names,
        &names,
    ))
}

/// The replacement for a reference `target` after the move, or `None`
/// when the move leaves it naming what it already named.
///
/// Asked of the ladder the graph binds edges with, so which rung wins is
/// the build's answer and not a second one: a candidate that is a *file*
/// but carries no document (one whose parse failed) is not a binding, and
/// the ladder goes on past it exactly as the resolver does. Read against
/// a set of scanned paths instead, a reference the graph bound to a
/// document lower down read as an edge to that file, and the move left it
/// behind reporting success.
///
/// The document it named is then followed rather than the path: what
/// moved gets the path the move leaves it standing on, what did not keep
/// theirs, and the spelling is re-rendered in the frame that read it —
/// root-relative stays root-relative, source-relative is recomputed from
/// `to_dir`. A reference the same spelling still names from there is left
/// alone, so a move writes only what it has to.
fn moved_target(
    target: &str,
    form: &ReferenceForm,
    moving: &Moving<'_>,
    worlds: Worlds<'_>,
    extensions: &[String],
) -> Option<Repoint> {
    let &Moving {
        rewriting,
        from_dir,
        to_dir,
        old_norm,
        new_norm,
    } = moving;
    let forward = crate::path_guard::forward_str(target);
    // The ladder is asked about the reference as it is written, `./` and
    // all: the marker names the frame, and taking it off first is asking
    // about a reference the document does not hold.
    let normalized = forward.strip_prefix("./").unwrap_or(&forward);
    let PathBinding::Bound(named) =
        path_binding(&forward, from_dir, worlds.before, extensions, true)
    else {
        return None;
    };
    // Where the document it named stands once the move has happened.
    let itself = named.path == old_norm;
    let stands = if itself {
        new_norm
    } else {
        named.path.as_str()
    };
    // What the write has to name. The document the reference named, read
    // before the mutation could bear on the answer — except where the
    // reference is the moved document's own to itself, which names itself
    // wherever it lands: the bytes the reference lives in are the bytes at
    // the destination, so there is no other document the write could
    // reach and nothing for the project to be asked about.
    let intends = match (rewriting, itself) {
        (Rewriting::Moved, true) => worlds.after.id_at(stands)?.to_string(),
        _ => named.id,
    };
    if let PathBinding::Bound(after) =
        path_binding(&forward, to_dir, worlds.after, extensions, true)
        && after.path == stands
    {
        return None;
    }
    let keep_extension = extensions
        .iter()
        .any(|ext| normalized.ends_with(ext.as_str()));
    // Rendered in the frame that read it, and then — where that frame is
    // the document's own — a second spelling that says which frame it is,
    // for the reference a document arriving at the root would otherwise
    // take. Which of the two *names* the document is the gate's to
    // answer, so both go to it in that order, and only a reference the
    // plain spelling would lose ever needs the second.
    //
    // What saying it amounts to is the vocabulary's answer, not this
    // function's. A destination spells a path, and `./` is a path saying
    // its own frame — CommonMark, every filesystem and every editor read
    // it the one way. A capture is written in whatever syntax its
    // document declared, and `[[./x]]` is in no wikilink vocabulary
    // anywhere: Obsidian, Foam, Dendron and Logseq all fail to follow it,
    // so writing it would trade a link readers follow for one only this
    // graph does. A capture says its frame by leaving the frame — named
    // from the root, which is where those readers look and which the
    // ladder tries first, so nothing arriving beside the document can
    // take it.
    //
    // A spelling that comes out as it went in is not a rewrite and is
    // dropped; where both do, the reference is one the move rebound and
    // the caller says so.
    let own = named.frame == Frame::Relative;
    let here = forward.starts_with("./");
    let rendered = render_target(
        Path::new(stands),
        own.then_some(to_dir),
        keep_extension,
        here,
        extensions,
    );
    let says_frame = own.then(|| match form {
        ReferenceForm::Destination { .. } => {
            (!rendered.starts_with('.')).then(|| format!("./{rendered}"))
        }
        ReferenceForm::Capture(_) | ReferenceForm::Citation(_) => Some(render_target(
            Path::new(stands),
            None,
            keep_extension,
            false,
            extensions,
        )),
    });
    let spellings: Vec<String> = [Some(rendered), says_frame.flatten()]
        .into_iter()
        .flatten()
        .filter(|spelling| spelling != target)
        .collect();
    (!spellings.is_empty()).then_some(Repoint { spellings, intends })
}

/// Render `new_path` as a link target in the author's style: root-relative
/// when `relative_to` is `None`, otherwise relative to the linking file's
/// directory. Strips a configured extension when the original reference
/// carried none (an extension-less wikilink stays extension-less).
fn render_target(
    new_path: &Path,
    relative_to: Option<&Path>,
    keep_extension: bool,
    keep_here: bool,
    extensions: &[String],
) -> String {
    let base = match relative_to {
        None => crate::path_guard::forward_string(new_path),
        Some(dir) => relative_from(dir, new_path),
    };
    let base = if keep_extension {
        base
    } else {
        strip_configured_extension(&base, extensions)
    };
    // The author said `./` — an explicit source-relative frame — and a
    // reader inside the destination may read for it. Kept where it still
    // marks something: a render that already begins with `..` says the
    // frame itself, and `./..` says it twice.
    if keep_here && !base.starts_with('.') {
        format!("./{base}")
    } else {
        base
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

/// Rewrite what the document that results can be read as, returning it —
/// or `None` when nothing was.
///
/// A rewrite is a proposal, and the reader that found the reference is
/// what accepts it: not of the document as it stands, but of the one the
/// write would leave. Three captures on a line reading `xxx` each rewrite
/// to `-` without moving the frontmatter boundary, and together they spell
/// `---`, so the document takes somebody else's id and loses every edge it
/// had. Proposals are applied in document order and each is read back in
/// the document carrying the ones accepted before it, the boundary
/// included — the last such document being the one written. Two proposals
/// claiming overlapping source need no rule of their own: the second is
/// asked about the text the first left.
///
/// A reference an accepted rewrite's span covers has no bytes of its own
/// left, so it is read by what the rewrite wrote rather than by where it
/// sat — under either name that write could have left it. `[t](./a.md)`
/// repointed to `./b.md` spells the covered `a.md` as the `b.md` the
/// rename had for it, and repointed to `docs2/a.md` still spells `a.md`
/// itself; both are the reference, once carried and once carried along.
/// Only where neither name is there did it stop existing.
///
/// A reference the rename left nothing to ask for is then the coverer's
/// to subsume: `docs/a.md` repointed to `docs/b.md` no longer spells the
/// `a.md` a basename pattern captured, nothing could have kept both
/// spellings — which is why only the earlier of two overlapping proposals
/// is honoured at all — and the same trade retires a bare `old` captured
/// inside `[t](old.md)`.
///
/// Subsuming is for those alone. A reference the rename *did* give a
/// target is held to the same account as every other however wholly its
/// bytes were replaced, because two readers over one text are two edges
/// when their relations differ, and taking one exempts it from every
/// check that follows — the edge only it carried then stops existing
/// where nothing reports it.
///
/// Every other reference the document holds must survive, the ones left
/// alone as much as the ones rewritten. A pattern whose match reaches past
/// its capture depends on text another reference occupies: `(\S+\.md)
/// a\.md` over `a.md a.md` holds while its own rewrite lands, and the
/// rewrite after it takes away the tail the match needed. Such a reference
/// does not come to dangle, which `check` reports; it stops being a
/// reference at all, which nothing reports. So a reference found to be
/// lost is named, and from then on a rewrite is refused when the document
/// it would make cannot be read as still holding it. Naming is what makes
/// the refusal exact: a trial differs from what is accepted by one
/// rewrite, so what the trial loses, that rewrite cost — where refusing by
/// recency would give up every rewrite after the culprit as well.
///
/// A trial also asks the rewrite under it to read back, which nothing
/// depends on: a spelling accepted that does not read back is named lost
/// by the sweep below and re-chosen on the next pass, so the loop
/// arrives at the same document either way. What it buys is that pass —
/// twelve hundred references repointed in 0.32s with it and 0.63s
/// without — and it is written here as an acceptance criterion rather
/// than a guard, because a guard nothing can be shown to need is one
/// somebody later removes for the wrong reason.
///
/// A pass names at least one reference no pass named before, so there are
/// as many passes as there are references to lose, and one for any pattern
/// whose match is its capture. Only what the document reads back to begin
/// with can be named, so what the two readers disagree about can neither
/// be lost nor loop.
///
/// That bound is reachable and it is the cost: a trial lays the whole
/// document out and reads back everything named so far, so a document
/// where every pass names one more reference costs passes × references ×
/// what a document of that size costs to read. Documents do not do this
/// on their own — sixty thousand generated ones never went past two
/// passes — it takes a config with one reach-past pattern per reference,
/// and one built with a hundred and sixty of them takes seconds rather
/// than milliseconds. Bounding it would mean giving up exact attribution,
/// which is what makes a refusal name the rewrite that earned it.
///
/// What no refusal can rescue is left as it was: it stays visible, and
/// once what it named is gone, surfaces as an unresolved edge — rather
/// than being written into a reference to nothing.
///
/// A rewrite may take only what it replaced *entirely*. A reference it
/// merely reaches into still stands in the bytes outside it and is read
/// there, so a rewrite that breaks one is refused: a greedy
/// `\b(\S+\.md)\b` capturing `x](a.md` out of `[x](a.md)` keeps its own
/// head, and a rewrite nested inside a longer capture has replaced part
/// of that capture, not it. Refusing there costs help rather than safety,
/// and the alternative costs safety — read by enclosure the shorter
/// rewrite would take the longer reference, which is how an edge to a file
/// that still exists was silently moved.
fn apply_proposals(
    content: &str,
    frontmatter: Option<(usize, usize)>,
    mut proposals: Vec<Proposal>,
    parser: &ParserConfig,
    names: &dyn Fn(&str) -> Option<String>,
    named_before: &dyn Fn(&str) -> Option<String>,
) -> Rewritten {
    proposals.sort_by_key(|proposal| proposal.span.start);
    // What each reference names where the document now stands, read out
    // of its own bytes. Constant across trials: the bytes a trial does not
    // rewrite are the bytes its author wrote.
    let stands: Vec<Option<String>> = proposals
        .iter()
        .map(|proposal| names(&proposal.span.target))
        .collect();
    // And the document each one named before any of this — what a rewrite
    // reaching over its bytes has to leave it still naming, since it can
    // report nothing about a reference whose spelling it has replaced. A
    // repoint carries the answer already; for the moved document's own
    // self-reference it carries a different one, which is why it is asked
    // first.
    let meant: Vec<Option<String>> = proposals
        .iter()
        .map(|proposal| {
            proposal
                .repoint
                .as_ref()
                .map(|repoint| repoint.intends.clone())
                .or_else(|| named_before(&proposal.span.target))
        })
        .collect();
    // What the document already holds, by this reader's own reckoning: a
    // reference it cannot find where the reference stands is not one a
    // rewrite can be asked to keep.
    let intact = {
        let untouched: Vec<Option<&str>> = vec![None; proposals.len()];
        let subsumed = vec![false; proposals.len()];
        let (text, landings) = lay_out(content, &proposals, &untouched, &subsumed);
        let reading = Reading::of(&text);
        (0..proposals.len())
            .map(|at| {
                reads(
                    &proposals, &untouched, at, &landings, &meant, &reading, names,
                )
            })
            .collect::<Vec<bool>>()
    };
    let mut held = vec![false; proposals.len()];
    // Whether a trial must also answer for what it mints. Off until a
    // finished document is found holding a reference the original did
    // not, because the question is about a whole document and a trial is
    // not one — the rewrites after it write again. Asking every trial
    // costs an extraction per candidate; asking the pass costs one, and
    // a document that mints nothing — nearly every document — pays that.
    let mut strict = false;
    let (content_out, standing) = loop {
        let mut spellings: Vec<Option<String>> = vec![None; proposals.len()];
        let mut subsumed = vec![false; proposals.len()];
        let mut consumed = 0usize;
        for index in 0..proposals.len() {
            let Proposal { span, repoint, .. } = &proposals[index];
            // No rewrite reaches inside another: the text the first wrote
            // is not the text the second was read out of.
            if span.start < consumed {
                continue;
            }
            let Some(repoint) = repoint.as_ref() else {
                continue;
            };
            let accepted = repoint
                .spellings
                .iter()
                .flat_map(|spelling| span.form.spellings(spelling))
                .find(|spelling| {
                    let mut chosen: Vec<Option<&str>> =
                        spellings.iter().map(Option::as_deref).collect();
                    chosen[index] = Some(spelling);
                    let (text, landings) = lay_out(content, &proposals, &chosen, &subsumed);
                    matches!(frontmatter_range(&text), Ok(range) if range == frontmatter)
                        // Before the read-back, though it is the dearer
                        // question: strict is on because this document
                        // mints, so it is the answer that comes back `no`
                        // most often, and the read-back is what the
                        // spellings it turns down never have to pay for.
                        && (!strict
                            || mints_nothing(
                                &text,
                                &account(&proposals, &chosen, &landings, &meant, &stands),
                                parser,
                                names,
                            ))
                        && {
                            let reading = Reading::of(&text);
                            std::iter::once(index)
                                .chain((0..proposals.len()).filter(|&at| held[at]))
                                .all(|at| {
                                    reads(&proposals, &chosen, at, &landings, &meant, &reading, names)
                                })
                        }
                });
            if let Some(spelling) = accepted {
                spellings[index] = Some(spelling);
                consumed = span.end;
                take(
                    content,
                    &proposals,
                    &spellings,
                    &mut subsumed,
                    index,
                    &meant,
                    names,
                );
            }
        }

        let chosen: Vec<Option<&str>> = spellings.iter().map(Option::as_deref).collect();
        let (text, landings) = lay_out(content, &proposals, &chosen, &subsumed);
        let reading = Reading::of(&text);
        let read: Vec<bool> = (0..proposals.len())
            .map(|at| reads(&proposals, &chosen, at, &landings, &meant, &reading, names))
            .collect();
        let lost: Vec<usize> = (0..proposals.len())
            .filter(|&at| intact[at] && !read[at] && !carried(&proposals, &read, at))
            .collect();
        if lost.is_empty() {
            if mints_nothing(
                &text,
                &account(&proposals, &chosen, &landings, &meant, &stands),
                parser,
                names,
            ) {
                let standing = standing(&proposals, &chosen, &landings, &reading);
                break ((text != content).then_some(text), standing);
            }
            if strict {
                // It mints under every spelling still reachable, so it
                // writes nothing at all: an edge no author wrote costs
                // more than the repoints it arrived with.
                break (None, vec![true; proposals.len()]);
            }
            strict = true;
            continue;
        }
        let mut named = false;
        for at in lost {
            named |= !std::mem::replace(&mut held[at], true);
        }
        if !named {
            // Every trial cost the document a reference, so none was
            // written and the document is the one it started as: every
            // reference in it stands exactly where its author put it.
            break (None, vec![true; proposals.len()]);
        }
    };

    // Everything the rewrite has to answer for is about a reference it
    // left standing, and both answers are read off the same list. Asked of
    // the finished text instead, a reference the rewrite repointed
    // correctly reads as one that changed what it names, because it did.
    let left = |at: usize| standing[at].then(|| proposals[at].span.target.clone());
    // A mutation takes the rung a reference stood on out from under it:
    // the next candidate down the ladder can be a different document.
    let rebound: Vec<Rebound> = dedup(
        (0..proposals.len())
            .filter_map(|at| {
                let reference = left(at)?;
                let was = named_before(&reference);
                let now = names(&reference)?;
                (was.as_deref() != Some(now.as_str())).then_some(Rebound {
                    reference,
                    was,
                    now,
                })
            })
            .collect(),
        |one| one.reference.as_str(),
    );
    // A reference the rewrite asked to change and did not — where what
    // it asked for is not what the reference names anyway. A repoint is
    // proposed off the path rungs alone, and the ladder has one below
    // them: a capture the move leaves can go on naming the document by
    // its bare id, having lost nothing and needing no repoint at all.
    let refused = (0..proposals.len())
        .filter_map(|at| {
            let meant = proposals[at].repoint.as_ref()?.intends.as_str();
            let reference = left(at)?;
            (names(&reference).as_deref() != Some(meant)).then_some(reference)
        })
        .filter(|reference| !rebound.iter().any(|one| one.reference == *reference))
        .collect();
    Rewritten {
        content: content_out,
        rebound,
        refused: dedup(refused, String::as_str),
    }
}

/// Whether the candidate document holds an edge none of its references
/// answer for.
///
/// The twin of the lost-reference sweep, and the same argument the other
/// way round: a rewrite may not trade an edge for a repoint, and it may
/// not mint one either. What it writes is text, and text is read by every
/// reader the project declares — a successor spelling can satisfy a
/// pattern that matched nothing before, giving the document an edge no
/// author wrote and nothing downstream a reason to doubt.
///
/// Asked as edges, because an edge is what a mint is, and both sides are
/// read in the world the mutation leaves — which is what makes the
/// question answerable at all. What the candidate holds is read out of
/// the candidate; what it may hold is its [`account`], which is
/// [`reads`]'s criterion asked of the document instead of a reference. A
/// reference the move alone comes to rebind is therefore answered for,
/// being read from where the document now stands on both sides: it is no
/// arrival, and the seam reports it as a [`Rebound`]. Documents rather
/// than spellings, because the graph holds one edge however many times a
/// document spells it — a spelling that arrives carrying an edge the
/// document already carries changes nothing, and a repoint is not given
/// up for it.
///
/// A reference the candidate holds that names nothing is no edge, and is
/// not refused here. Nor is it unseen: it surfaces as an unresolved edge
/// at whatever severity the project's `[[detection.unresolved_policy]]`
/// gives its cause, and where that is an error the write seam's own
/// `introduced` gate refuses the mutation whole. This guard exists for
/// what `check` cannot see — an edge, silently arrived — and a project
/// says for itself what an unresolved reference is worth.
///
/// Asked of the finished document, because that is what the project will
/// read; a trial is not one, since the rewrites after it write again. A
/// pass that mints is retried with every trial answering for it too — the
/// spelling that mints is then the one refused, and a reference has other
/// spellings, the next of which may carry the repoint without the edge.
///
/// That retry is what a document pays for minting, and it pays per
/// trial: twelve hundred references cost 0.25s where nothing mints and
/// 2.70s where everything does, against 0.08s and 0.71s at six hundred —
/// the same shape the pass mechanism above has, with a larger constant.
/// It is the price of the exact question, and the trial pays it because
/// the trial decides which spelling is given up: asked anything cheaper
/// there, a spelling the pass would have taken is refused before the
/// pass ever sees it, which is the whole defect one question in two
/// places produces. Reading the candidate any cheaper means reading it
/// with something other than the reader the builder binds edges with,
/// and two readers of one text that may disagree is what this seam
/// exists to prevent — a second one introduced to make the guard fast
/// would be the first place it came back.
fn mints_nothing(
    candidate: &str,
    account: &std::collections::BTreeSet<(&str, &str)>,
    parser: &ParserConfig,
    names: &dyn Fn(&str) -> Option<String>,
) -> bool {
    references(candidate, parser).is_ok_and(|found| {
        found.spans.iter().all(|span| {
            names(&span.target)
                .is_none_or(|held| account.contains(&(span.relation.as_str(), held.as_str())))
        })
    })
}

/// Every edge the candidate's own references answer for: the relation a
/// reference is read under, and the one document [`reads`] holds it to
/// where the candidate leaves it.
///
/// The two functions decide one thing between them, so they decide it by
/// one criterion, arm for arm. A reference the candidate rewrote must
/// name what its [`Repoint`] intends — the rendering is the thing in
/// doubt, and what it must name is the only fact about it the rewrite
/// holds. One the candidate left where it stands says what its own bytes
/// say, which may be somebody new: the reference is still the author's,
/// the seam reports the change as a [`Rebound`], and it is nothing the
/// rewrite invented. A *covered* one — bytes replaced by a rewrite around
/// it — is held to the document it named, because a rewrite that has
/// spelled a reference away cannot report having moved it. And one
/// severed or taken answers for nothing: its bytes are gone, and a taken
/// one bound nothing to lose — [`take`] admits no other kind.
///
/// One document apiece is what makes this a set and not a tally. Each
/// reference reaches one document, so what the candidate may hold is
/// exactly what they name between them; an arrival is a document none of
/// them was held to, however ordinary it looks beside the rest.
fn account<'a>(
    proposals: &'a [Proposal],
    chosen: &[Option<&str>],
    landings: &[Option<Landing>],
    meant: &'a [Option<String>],
    stands: &'a [Option<String>],
) -> std::collections::BTreeSet<(&'a str, &'a str)> {
    (0..proposals.len())
        .filter_map(|index| {
            let Proposal { span, repoint, .. } = &proposals[index];
            let reaches = match (
                chosen[index].and(repoint.as_ref()),
                landings[index].as_ref()?,
            ) {
                (Some(repoint), _) => repoint.intends.as_str(),
                (None, Landing::Within(_)) => match meant[index].as_deref() {
                    Some(document) => document,
                    // It named none, so there is no document to hold it
                    // to and its spelling is all it has — and a spelling
                    // the write left saying what its author wrote names
                    // whatever it names, which the write did not do.
                    None => stands[index].as_deref()?,
                },
                (None, Landing::At(_)) => stands[index].as_deref()?,
                (None, Landing::Severed) => return None,
            };
            Some((span.relation.as_str(), reaches))
        })
        .collect()
}

/// Which references the rewrite left standing: the ones the finished
/// document still says, in the words their author wrote, where they
/// landed.
///
/// Not the acceptance list, because those are different questions —
/// answering the second with the first names references the document no
/// longer holds: a move warning that it repointed one it had re-rendered
/// correctly, a retarget reporting it gave up on one that plainly reads
/// as the successor. And not the landing map either. A reference a rewrite
/// wrote *over* can survive it word for word — `[t](sub/w.md)` rebased to
/// `[t](c/sub/w.md)` still says `w.md` — and that reference is as much the
/// author's as any other. What it says is unchanged; what it *reaches* may
/// not be, because a relative reference means whatever it means from where
/// it sits and the document may have moved. Left out of this list, the
/// only reference a move can rebind without the seam noticing is one the
/// move rebinds by carrying it somewhere else.
fn standing(
    proposals: &[Proposal],
    chosen: &[Option<&str>],
    landings: &[Option<Landing>],
    reading: &Reading<'_>,
) -> Vec<bool> {
    (0..proposals.len())
        .map(|at| {
            let Some(landing) = landings[at].as_ref() else {
                return false;
            };
            let own = proposals[at].span.target.as_str();
            chosen[at].is_none()
                && proposals[at]
                    .span
                    .form
                    .reads_back(reading, landing, &|says: &str| says == own)
        })
        .collect()
}

/// One entry per spelling, in the order they are met — two readers of one
/// reference have one thing to say about it, and both answers say it about
/// the reference rather than about the reader.
fn dedup<T>(mut answers: Vec<T>, spelling: impl Fn(&T) -> &str) -> Vec<T> {
    let mut seen = std::collections::BTreeSet::new();
    answers.retain(|answer| seen.insert(spelling(answer).to_string()));
    answers
}

/// Whether another reader of the very same bytes, binding the very same
/// edge, still reads the finished document.
///
/// `[[old]]` is one edge when the wikilink reader and a `\[\[(old)\]\]`
/// pattern of the same relation both bind it — one span, one relation,
/// one target, and the graph holds it once. So the reader whose fixed
/// spelling cannot follow the repoint takes nothing with it, and refusing
/// the rewrite over it would cost the repoint to protect an edge that was
/// never at risk.
///
/// Only over one span: two readers elsewhere in the document may agree on
/// relation and target and still be two references, and whether the
/// surviving one *ends up* carrying that edge depends on whether its own
/// repoint landed — a question answered per reference, not per pair. The
/// same bytes have no such freedom, which is what makes this one safe.
/// Differ in relation and they are two edges; neither answers for the
/// other, which is why the relation is carried this far.
fn carried(proposals: &[Proposal], read: &[bool], index: usize) -> bool {
    let mine = &proposals[index].span;
    (0..proposals.len()).any(|other| {
        let theirs = &proposals[other].span;
        other != index
            && read[other]
            && theirs.start == mine.start
            && theirs.end == mine.end
            && theirs.relation == mine.relation
            && theirs.target == mine.target
    })
}

/// Mark every reference the rewrite just accepted at `index` has taken:
/// one whose bytes it replaced entirely, which the rename left nothing to
/// ask for, and whose text what it wrote no longer holds. Asked here, of
/// the document that rewrite makes, because that is the only place the
/// answer is about that rewrite — a later one changing the text again is
/// a loss, not a taking.
///
/// A reference the rename *did* give a target is never the rewrite's to
/// take, however wholly the bytes were replaced: it asked to be repointed,
/// so either the write repointed it and it stands, or it was lost and the
/// write must give itself up. Taking it instead exempts it from every
/// check that follows, and an edge only it carried — one text bound under
/// a second relation — stops existing where nothing reports it.
///
/// A rewrite encloses nothing in the ordinary case, so the document is
/// laid out again only when it does.
fn take(
    content: &str,
    proposals: &[Proposal],
    spellings: &[Option<String>],
    subsumed: &mut [bool],
    index: usize,
    meant: &[Option<String>],
    names: &dyn Fn(&str) -> Option<String>,
) {
    let rewritten = &proposals[index].span;
    let enclosed: Vec<usize> = (0..proposals.len())
        .filter(|&other| {
            let Proposal {
                span,
                repoint,
                binds,
            } = &proposals[other];
            other != index
                && !subsumed[other]
                && repoint.is_none()
                && !binds
                && span.start >= rewritten.start
                && span.end <= rewritten.end
        })
        .collect();
    if enclosed.is_empty() {
        return;
    }
    let chosen: Vec<Option<&str>> = spellings.iter().map(Option::as_deref).collect();
    let (text, landings) = lay_out(content, proposals, &chosen, subsumed);
    let reading = Reading::of(&text);
    for other in enclosed {
        subsumed[other] = !reads(proposals, &chosen, other, &landings, meant, &reading, names);
    }
}

/// The document `chosen` produces, and where each reference of the
/// original is read back in it — `None` for one a rewrite has taken.
///
/// A reference stands in its own bytes, moved by whatever the rewrites
/// before it added or took away — unless a rewrite touched them. One that
/// replaced *all* of them leaves the reference none of its own, so it is
/// read within what that rewrite wrote; one that replaced *some* leaves it
/// nowhere to be read at all, because what remains of its text no longer
/// joins up. Enclosure and reaching-into are different things and a
/// reference is never guessed into a range that is not its own: read at a
/// range widened to cover the rewrite, a destination beside it — one the
/// rename never touched, naming a file still there — answered for it, and
/// the write went out over the loss.
///
/// A reached-into reference can survive, where what the rewrite wrote
/// re-spells the bytes it took: `md) end` repointed to `md) fin` leaves
/// the `md` it took from the destination beside it exactly as it was. That
/// one is refused too, and the refusal is the trade — finding it would
/// mean reading a reference at a surviving image the document does not say
/// it has, which is the guess this stopped making. What it costs is help:
/// the reference stays, naming a file that has moved, and `check` says so.
fn lay_out(
    content: &str,
    proposals: &[Proposal],
    chosen: &[Option<&str>],
    taken: &[bool],
) -> (String, Vec<Option<Landing>>) {
    // The rewrites, in order, with where each lands. They never overlap:
    // a proposal is only ever chosen from beyond the last one's end.
    let mut text = String::with_capacity(content.len());
    let mut rewrites: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    let mut of_proposal: Vec<Option<usize>> = vec![None; proposals.len()];
    let mut cursor = 0usize;
    let mut shift = 0isize;
    for (index, Proposal { span, .. }) in proposals.iter().enumerate() {
        let Some(spelling) = chosen[index] else {
            continue;
        };
        text.push_str(&content[cursor..span.start]);
        text.push_str(spelling);
        cursor = span.end;
        let start = span.start.wrapping_add_signed(shift);
        of_proposal[index] = Some(rewrites.len());
        rewrites.push((span.start..span.end, start..start + spelling.len()));
        shift += spelling.len() as isize - (span.end - span.start) as isize;
    }
    text.push_str(&content[cursor..]);

    // Rewrites and proposals are both in document order, so this walks
    // each once.
    let mut landings = Vec::with_capacity(proposals.len());
    let mut first = 0usize;
    let mut before = 0isize;
    for (index, Proposal { span, .. }) in proposals.iter().enumerate() {
        while first < rewrites.len() && rewrites[first].0.end <= span.start {
            let (from, to) = &rewrites[first];
            before += to.len() as isize - from.len() as isize;
            first += 1;
        }
        if let Some(own) = of_proposal[index] {
            landings.push(Some(Landing::At(rewrites[own].1.clone())));
            continue;
        }
        if taken[index] {
            landings.push(None);
            continue;
        }
        let enclosing = rewrites[first..]
            .iter()
            .take_while(|(from, _)| from.start <= span.start)
            .find(|(from, _)| span.end <= from.end);
        if let Some((_, to)) = enclosing {
            landings.push(Some(Landing::Within(to.clone())));
            continue;
        }
        let reached_into = rewrites[first..]
            .iter()
            .take_while(|(from, _)| from.start < span.end)
            .any(|(from, _)| from.end > span.start);
        if reached_into {
            landings.push(Some(Landing::Severed));
            continue;
        }
        // Every rewrite is now wholly before this reference or wholly
        // after it, so `before` is the whole of what moved it and the
        // range is exactly where its bytes went — a position this
        // document has, which the readers slice at.
        let start = span.start.wrapping_add_signed(before);
        let end = span.end.wrapping_add_signed(before);
        landings.push(Some(Landing::At(start..end)));
    }
    (text, landings)
}

/// Whether the reference at `index` is read where it landed, as whatever
/// was written there.
///
/// Where its own rewrite landed it must say what the rename gave it, and
/// where nothing was written it still says what it said. Between those
/// is a reference whose bytes *another* write replaced, and that one may
/// legitimately be read by either name: the write carried its repoint, or
/// it carried the reference along unrenamed to dangle where `check`
/// reports it. Asking for one name alone answers no to the other case —
/// which retires a second reader of the text as though the write had
/// taken it, and an edge only that reader carried stops existing where
/// nothing reports it.
///
/// Carried means carried to the document it *named*, never to whatever
/// its old spelling has come to name since. Those differ exactly where a
/// shadow has moved in behind the file it named, and a rewrite reaching
/// over a reference can report nothing about it — the spelling a report
/// would name is gone. What survives unrenamed is a separate matter and
/// stays a question about the text: bytes the write left saying what
/// their author wrote are not bytes the write moved, whatever they have
/// come to mean.
///
/// The carried name is not asked for as a string, because a path is not
/// one. The
/// write is somebody else's rendering of the file, and a reader inside it
/// gets a frame of its own: repointing `[t](./a.md)` in `docs/` to
/// `docs2/` writes `../docs2/a.md`, out of which a pattern reading past
/// `./` says `docs2/a.md` — a third spelling, and the same document by
/// the frame above. Read as text that reference is lost and the repoint
/// is given up for it, leaving two edges dangling where the write it
/// refused kept both. So a covered reference is asked what it *binds*,
/// and the spelling stands for it only where the project has no answer.
fn reads(
    proposals: &[Proposal],
    chosen: &[Option<&str>],
    index: usize,
    landings: &[Option<Landing>],
    meant: &[Option<String>],
    reading: &Reading<'_>,
    names: &dyn Fn(&str) -> Option<String>,
) -> bool {
    let Some(at) = landings[index].as_ref() else {
        return true;
    };
    let Proposal { span, repoint, .. } = &proposals[index];
    let own = span.target.as_str();
    match (chosen[index].and(repoint.as_ref()), at) {
        // Its own rewrite landed here, so what it says is the seam's own
        // rendering — asked what that rendering *names*, because a frame
        // it did not intend can be shadowing the one it did.
        (Some(repoint), _) => span.form.reads_back(reading, at, &|says: &str| {
            names(says).as_deref() == Some(repoint.intends.as_str())
        }),
        // The bytes are somebody else's rendering, and a path has more
        // than one: read out of `../docs2/a.md`, a capture of
        // `docs2/a.md` is the same file by the frame above it. So what a
        // covered reference says is asked of the project rather than of
        // the spelling — and what it has to say is the document it named,
        // never merely what its own old spelling has come to name.
        (None, Landing::Within(_)) => {
            let carried = meant[index].as_deref();
            span.form.reads_back(reading, at, &|says: &str| {
                says == own
                    || carried.is_some_and(|document| names(says).as_deref() == Some(document))
            })
        }
        (None, Landing::At(_) | Landing::Severed) => {
            span.form.reads_back(reading, at, &|says: &str| says == own)
        }
    }
}

/// The spellings `path` + `fragment` can take as a markdown destination,
/// in the order a rewrite prefers them: as the path is written, then with
/// the destination grammar's escapes, then inside pointy brackets. The
/// author's plain spelling survives every ordinary rename, and a name a
/// plain destination cannot carry still gets repointed rather than left
/// behind — a destination carrying a space ends where the space is, so
/// `[x](old.md)` rewritten to `[x](new name.md)` is no link at all, while
/// pointy brackets admit the space and a backslash admits the delimiters.
/// `&` is escaped with them: a raw one opens an entity, so `a&copy;.md`
/// written plainly is read back as `a©.md` and names a different file.
/// The brackets are markdown the document did not have before, and a
/// `[[parser.link_patterns]]` regex that matches past its capture can
/// match across them — a reference the project reads that its author did
/// not write, exactly as if the author had typed the brackets. It is the
/// pattern's reach, not the spelling, and `check` reports what it binds.
///
/// What no spelling reaches is a name a *destination* cannot mean: one
/// carrying `#`, which [`body::destination_path`] reads as the start of a
/// fragment; one spelled with edge whitespace, which it trims; and one
/// beginning `http://`, `https://` or `mailto:`, which it reads as
/// leaving the project. Such a document is unreachable by markdown link
/// whatever a rewrite does, so a move onto that name strands the links to
/// it — and the write gate refuses the move exactly when the project's
/// own `[[detection.unresolved_policy]]` calls a stranded reference an
/// error.
fn destination_spellings(path: &str, fragment: &str) -> [String; 3] {
    let plain = format!("{path}{fragment}");
    let mut escaped = String::with_capacity(plain.len());
    for character in plain.chars() {
        if matches!(character, '\\' | '&' | '(' | ')' | '<' | '>') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    let pointy = format!("<{escaped}>");
    [plain, escaped, pointy]
}

/// The byte range of the line holding `offset`, its newline excluded.
fn line_range(text: &str, offset: usize) -> std::ops::Range<usize> {
    let start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index);
    start..end
}

/// Whether `[start, end)` overlaps any protected `(s, e)` range.
fn overlaps(start: usize, end: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(s, e)| start < e && end > s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LinkPattern;
    use std::collections::BTreeSet;

    fn parser() -> ParserConfig {
        ParserConfig::default()
    }

    fn rewrite(content: &str, source_dir: &str, old: &str, new: &str, p: &ParserConfig) -> String {
        // Pre-move scope: the binding is resolved as it was before the
        // move, so `old` is in scope (and `new` is not).
        rewrite_for_move(
            content,
            Rewriting::Referrer(Path::new(source_dir)),
            Path::new(old),
            Path::new(new),
            Worlds {
                before: &bound_of(&scope(&[old])),
                after: &after_move(&scope(&[old]), old, new),
            },
            p,
        )
        .unwrap()
        .content
        .unwrap_or_else(|| content.to_string())
    }

    #[test]
    fn a_severed_reference_answers_for_nothing() {
        // Asked of `account` rather than of a rename, because a rename
        // cannot ask it: a severed reference never reads back, so the
        // lost-reference sweep gives the whole rewrite up before the
        // account is consulted about the document it left. The arm is
        // still the one that would admit an edge nobody wrote if the
        // sweep above it ever stopped dominating, which is reason enough
        // for it to be pinned by something.
        let reference = |start, end, target: &str, relation: &str| Proposal {
            span: ReferenceSpan {
                start,
                end,
                target: target.to_string(),
                relation: relation.to_string(),
                form: ReferenceForm::Destination {
                    fragment: String::new(),
                },
            },
            repoint: None,
            binds: true,
        };
        let proposals = [
            reference(0, 6, "a.md", "kept"),
            reference(4, 12, "b.md", "cut"),
        ];
        let landings = [
            Some(Landing::At(0..6)),
            // Reached into by the rewrite over the first: part of its
            // bytes are gone and what is left no longer joins up.
            Some(Landing::Severed),
        ];
        let stands = [Some("alpha".to_string()), Some("beta".to_string())];
        let account = account(&proposals, &[None, None], &landings, &[None, None], &stands);
        assert_eq!(
            account,
            std::collections::BTreeSet::from([("kept", "alpha")]),
            "the severed reference says nothing, so nothing may arrive under `cut`"
        );
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
            rewrite_for_move(
                "[x](docs/other.md)",
                Rewriting::Referrer(Path::new("docs")),
                Path::new("docs/a.md"),
                Path::new("docs/b.md"),
                Worlds {
                    before: &bound_of(&scope(&["docs/a.md", "docs/other.md"])),
                    after: &after_move(
                        &scope(&["docs/a.md", "docs/other.md"]),
                        "docs/a.md",
                        "docs/b.md"
                    ),
                },
                &parser(),
            )
            .unwrap()
            .content
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
            rewrite_for_move(
                "[[adr-001]]",
                Rewriting::Referrer(Path::new("docs")),
                Path::new("docs/old.md"),
                Path::new("docs/new.md"),
                Worlds {
                    before: &bound_of(&scope(&["docs/old.md"])),
                    after: &after_move(&scope(&["docs/old.md"]), "docs/old.md", "docs/new.md"),
                },
                &p,
            )
            .unwrap()
            .content
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
            rewrite_for_move(
                "[x](/etc/a.md)",
                Rewriting::Referrer(Path::new("")),
                Path::new("etc/a.md"),
                Path::new("etc/b.md"),
                Worlds {
                    before: &bound_of(&scope(&["etc/a.md"])),
                    after: &after_move(&scope(&["etc/a.md"]), "etc/a.md", "etc/b.md"),
                },
                &parser(),
            )
            .unwrap()
            .content
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
            rewrite_for_move(
                "[x](shared.md)",
                Rewriting::Referrer(Path::new("docs/sub")),
                Path::new("docs/sub/shared.md"),
                Path::new("docs/sub/renamed.md"),
                Worlds {
                    before: &bound_of(&scope(&[
                        "shared.md",
                        "docs/sub/shared.md",
                        "docs/sub/s.md"
                    ])),
                    after: &after_move(
                        &scope(&["shared.md", "docs/sub/shared.md", "docs/sub/s.md"]),
                        "docs/sub/shared.md",
                        "docs/sub/renamed.md",
                    ),
                },
                &parser(),
            )
            .unwrap()
            .content
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
            rewrite_for_move(
                "[[shared]]",
                Rewriting::Referrer(Path::new("docs/sub")),
                Path::new("docs/sub/shared.md"),
                Path::new("docs/sub/renamed.md"),
                Worlds {
                    before: &bound_of(&scope(&["docs/sub/shared", "docs/sub/shared.md"])),
                    after: &after_move(
                        &scope(&["docs/sub/shared", "docs/sub/shared.md"]),
                        "docs/sub/shared.md",
                        "docs/sub/renamed.md"
                    ),
                },
                &p,
            )
            .unwrap()
            .content
            .is_none(),
            "the bare sibling is the first candidate and binds the link — the .md rename must not touch it"
        );
        // Control: without the bare sibling, `[[shared]]` binds the .md
        // and the rename rewrites it.
        assert_eq!(
            rewrite_for_move(
                "[[shared]]",
                Rewriting::Referrer(Path::new("docs/sub")),
                Path::new("docs/sub/shared.md"),
                Path::new("docs/sub/renamed.md"),
                Worlds {
                    before: &bound_of(&scope(&["docs/sub/shared.md"])),
                    after: &after_move(
                        &scope(&["docs/sub/shared.md"]),
                        "docs/sub/shared.md",
                        "docs/sub/renamed.md"
                    ),
                },
                &p,
            )
            .unwrap()
            .content
            .as_deref(),
            Some("[[renamed]]")
        );
    }

    // ─── rewrite_moved_references ───────────────────────────────────────

    /// The world a retarget runs in: it refuses an id the graph does not
    /// carry, so both are always in it. The paths are ones no reference
    /// in these fixtures can spell, so the id rung is what answers.
    fn ids(carried: &[&str]) -> Bindings {
        let paths: Vec<(String, String)> = carried
            .iter()
            .map(|id| (format!("_ids/{id}.x"), (*id).to_string()))
            .collect();
        Bindings::of(
            paths
                .iter()
                .map(|(path, id)| (Path::new(path.as_str()), id.as_str())),
        )
    }

    /// The world a rename leaves: the same documents under the same ids,
    /// one of them at a new path.
    fn after_move(paths: &BTreeSet<String>, old: &str, new: &str) -> Bindings {
        let moved: Vec<(String, String)> = paths
            .iter()
            .map(|path| {
                let at = if path == old { new } else { path.as_str() };
                (at.to_string(), path.clone())
            })
            .collect();
        Bindings::of(
            moved
                .iter()
                .map(|(at, id)| (Path::new(at.as_str()), id.as_str())),
        )
    }

    /// The world a scope makes, each document's id being its own path —
    /// a project where no reference binds by id, which is what every test
    /// but the ones about binding is asking about.
    fn bound_of(paths: &BTreeSet<String>) -> Bindings {
        Bindings::of(
            paths
                .iter()
                .map(|path| (Path::new(path.as_str()), path.as_str())),
        )
    }

    fn scope(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    /// The documents a reading offers, each id being its own path — a
    /// project where no reference binds by id, which is what every test
    /// but the two about rebinding is asking about.
    fn world(paths: &[&str]) -> Bindings {
        Bindings::of(paths.iter().map(|path| (Path::new(*path), *path)))
    }

    fn rebase(
        content: &str,
        old_dir: &str,
        new_dir: &str,
        paths: &[&str],
        p: &ParserConfig,
    ) -> std::result::Result<Option<String>, crate::error::ParseError> {
        rebase_moved(content, old_dir, new_dir, paths, &world(paths), p).map(|moved| moved.content)
    }

    fn rebase_moved(
        content: &str,
        old_dir: &str,
        new_dir: &str,
        paths: &[&str],
        after: &Bindings,
        p: &ParserConfig,
    ) -> std::result::Result<Rewritten, crate::error::ParseError> {
        rewrite_for_move(
            content,
            Rewriting::Moved,
            &Path::new(old_dir).join("moved.md"),
            &Path::new(new_dir).join("moved.md"),
            Worlds {
                before: &world(paths),
                after,
            },
            p,
        )
    }

    #[test]
    fn a_move_says_which_references_it_left_naming_something_else() {
        // The capture takes only letters, so `../a/x` is a spelling the
        // pattern cannot read back and the reference is left as it is. A
        // relative reference means whatever it means from where it sits,
        // so the move repointed it at `b/x.md` — a real document, leaving
        // a valid graph that `check` has nothing to say about.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"@ref\(([a-z]+)\)".to_string(),
                relation: "ref".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        let paths = ["a/x.md", "b/x.md"];
        let moved = rebase_moved("@ref(x)", "a", "b", &paths, &world(&paths), &p).unwrap();
        assert!(moved.content.is_none(), "no spelling of it reads back");
        let rebound = &moved.rebound;
        assert_eq!(rebound.len(), 1, "{rebound:?}");
        assert_eq!(rebound[0].reference, "x");
        assert_eq!(rebound[0].was.as_deref(), Some("a/x.md"));
        assert_eq!(rebound[0].now, "b/x.md");
    }

    #[test]
    fn a_move_says_so_when_it_gives_a_reference_that_named_nothing_a_document() {
        // The reference dangled where the file stood, and binds `b/x.md`
        // where it now stands. Left alone it reads as a repair: the
        // unresolved edge that would have reported it is the very thing
        // the move took away, so nothing downstream has anything to say.
        let mut p = parser();
        p.wikilink_enabled = true;
        let moved = rewrite_for_move(
            "[[x]]",
            Rewriting::Moved,
            Path::new("docs/mover.md"),
            Path::new("b/mover.md"),
            Worlds {
                before: &Bindings::of([(Path::new("b/x.md"), "doc-bx")]),
                after: &Bindings::of([(Path::new("b/x.md"), "doc-bx")]),
            },
            &p,
        )
        .unwrap();
        let rebound = &moved.rebound;
        assert_eq!(rebound.len(), 1, "{rebound:?}");
        assert_eq!(rebound[0].was, None);
        assert_eq!(rebound[0].now, "doc-bx");
    }

    #[test]
    fn a_rename_says_which_of_a_referrer_s_references_it_left_binding_something_else() {
        // The referrer stands still, and the rename takes the rung its
        // reference stood on out from under it: `a.md` bound the root
        // document by the literal frame, and once that path is gone the
        // source-relative frame answers with the neighbour instead. No
        // spelling of `a#1.md` reads back, so the reference is left — and
        // the graph it leaves is valid, which is why nothing else says so.
        let rewritten = rewrite_for_move(
            "[x](a.md)",
            Rewriting::Referrer(Path::new("docs")),
            Path::new("a.md"),
            Path::new("a#1.md"),
            Worlds {
                before: &Bindings::of([
                    (Path::new("a.md"), "root-a"),
                    (Path::new("docs/a.md"), "shadow"),
                ]),
                after: &Bindings::of([
                    (Path::new("a#1.md"), "root-a"),
                    (Path::new("docs/a.md"), "shadow"),
                ]),
            },
            &parser(),
        )
        .unwrap();
        assert_eq!(rewritten.content, None, "no spelling of it reads back");
        let rebound = &rewritten.rebound;
        assert_eq!(rebound.len(), 1, "{rebound:?}");
        assert_eq!(rebound[0].was.as_deref(), Some("root-a"));
        assert_eq!(rebound[0].now, "shadow");
    }

    #[test]
    fn a_rename_that_repoints_a_referrer_says_nothing_about_it() {
        // The same shape where the repoint lands: the reference names
        // what it named, so there is nothing to report — asked of the
        // finished text instead, every repointed reference would read as
        // one that changed what it names, because it did.
        let rewritten = rewrite_for_move(
            "[x](a.md)",
            Rewriting::Referrer(Path::new("docs")),
            Path::new("a.md"),
            Path::new("b.md"),
            Worlds {
                before: &Bindings::of([(Path::new("a.md"), "root-a")]),
                after: &Bindings::of([(Path::new("b.md"), "root-a")]),
            },
            &parser(),
        )
        .unwrap();
        assert_eq!(rewritten.content.as_deref(), Some("[x](b.md)"));
        assert!(rewritten.rebound.is_empty(), "{:?}", rewritten.rebound);
    }

    #[test]
    fn a_rebase_will_not_write_a_frame_a_document_of_the_same_name_shadows() {
        // Rebasing `[[../../docs/sub/x]]` from `a/b/` to `docs/` renders
        // `sub/x` — source-relative, and the literal rung gets there
        // first, where `sub/x.md` is somebody else. Read as text the
        // rendering passes; read as what it names it is the wrong
        // document. So the reference says which frame it is in — and a
        // wikilink says that by naming from the root, the rung nothing
        // can get in front of and the one every other wikilink reader
        // looks in. `[[./sub/x]]` would say it in a vocabulary only this
        // graph has.
        let mut p = parser();
        p.wikilink_enabled = true;
        let world = Bindings::of([
            (Path::new("docs/sub/x.md"), "desired"),
            (Path::new("sub/x.md"), "shadow"),
        ]);
        let moved = rewrite_for_move(
            "[[../../docs/sub/x]]",
            Rewriting::Moved,
            Path::new("a/b/mover.md"),
            Path::new("docs/mover.md"),
            Worlds {
                before: &world,
                after: &world,
            },
            &p,
        )
        .unwrap();
        assert_eq!(
            moved.content.as_deref(),
            Some("[[docs/sub/x]]"),
            "the spelling nothing can take names `desired`"
        );
    }

    #[test]
    fn a_rebase_nothing_shadows_lands() {
        // The same move without the shadow: the rendering names what the
        // reference named, and it is written.
        let mut p = parser();
        p.wikilink_enabled = true;
        let world = Bindings::of([(Path::new("docs/sub/x.md"), "desired")]);
        let moved = rewrite_for_move(
            "[[../../docs/sub/x]]",
            Rewriting::Moved,
            Path::new("a/b/mover.md"),
            Path::new("c/mover.md"),
            Worlds {
                before: &world,
                after: &world,
            },
            &p,
        )
        .unwrap();
        assert_eq!(moved.content.as_deref(), Some("[[../docs/sub/x]]"));
    }

    #[test]
    fn a_move_says_so_when_the_document_it_repointed_to_is_named_by_id() {
        // Before the move nothing at `a/x` answers the reference and it
        // binds the id `x`; after it, `b/x.md` does. Both are documents
        // and they are not the same one, which a ladder that stops at
        // paths cannot say — it finds nothing before the move and reads
        // the change as no change.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"@ref\(([a-z]+)\)".to_string(),
                relation: "ref".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        let documents = [
            (Path::new("ids/x.md"), "x"),
            (Path::new("b/x.md"), "path-x"),
        ];
        let moved = rewrite_for_move(
            "@ref(x)",
            Rewriting::Moved,
            Path::new("a/mover.md"),
            Path::new("b/mover.md"),
            Worlds {
                before: &Bindings::of(documents),
                after: &Bindings::of(documents),
            },
            &p,
        )
        .unwrap();
        let rebound = &moved.rebound;
        assert_eq!(rebound.len(), 1, "{rebound:?}");
        assert_eq!(rebound[0].was.as_deref(), Some("x"));
        assert_eq!(rebound[0].now, "path-x");
    }

    #[test]
    fn a_move_says_so_when_a_self_reference_comes_to_name_somebody_else() {
        // The document referred to itself, the capture takes only
        // letters so no spelling of the new name reads back, and a
        // document of the old name stands in the directory it lands in:
        // the self-edge becomes an edge to somebody else, which is the
        // whole of what there is to say about it.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"@ref\(([a-z]+)\)".to_string(),
                relation: "ref".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        let moved = rewrite_for_move(
            "@ref(x)",
            Rewriting::Moved,
            Path::new("a/x.md"),
            Path::new("b/y2.md"),
            Worlds {
                before: &Bindings::of([
                    (Path::new("a/x.md"), "self"),
                    (Path::new("b/x.md"), "other"),
                ]),
                after: &Bindings::of([
                    (Path::new("b/y2.md"), "self"),
                    (Path::new("b/x.md"), "other"),
                ]),
            },
            &p,
        )
        .unwrap();
        let rebound = &moved.rebound;
        assert_eq!(rebound.len(), 1, "{rebound:?}");
        assert_eq!(rebound[0].was.as_deref(), Some("self"));
        assert_eq!(rebound[0].now, "other");
    }

    #[test]
    fn a_move_that_rebased_a_reference_says_nothing_about_it() {
        // The same shape under a pattern that can spell the rebased form:
        // the reference is re-rendered and still names what it named, so
        // there is nothing to report.
        let mut p = parser();
        p.wikilink_enabled = true;
        let paths = ["a/x.md", "b/x.md"];
        let moved = rebase_moved("[[x]]", "a", "b", &paths, &world(&paths), &p).unwrap();
        assert_eq!(moved.content.as_deref(), Some("[[../a/x]]"));
        assert!(moved.rebound.is_empty(), "{:?}", moved.rebound);
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
    fn leaves_a_redundant_spelling_that_still_binds_from_the_new_dir() {
        // The same binding by a spelling the seam would not have written:
        // `../b/../x.md` names `a/x.md` from `a/b` and from `a/c` alike,
        // and its own rendering is the shorter `../x.md`. A move writes
        // only what it has to, so what still names what it named is left
        // exactly as the author spelled it.
        assert!(
            rebase("[x](../b/../x.md)", "a/b", "a/c", &["a/x.md"], &parser())
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
            &ids(&["old", "new"]),
            &p,
        )
        .unwrap()
        .content
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
            &ids(&["old-id", "new-id"]),
            &p,
        )
        .unwrap()
        .content
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
        let out = rewrite_for_move(
            "see `@cite( docs/a.md )` here",
            Rewriting::Referrer(Path::new("docs")),
            Path::new("docs/a.md"),
            Path::new("docs/b.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["docs/a.md".to_string()])),
                after: &after_move(
                    &BTreeSet::from(["docs/a.md".to_string()]),
                    "docs/a.md",
                    "docs/b.md",
                ),
            },
            &p,
        )
        .unwrap()
        .content
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
            &ids(&["old-id", "new-id"]),
            &p,
        )
        .unwrap()
        .content
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
            &ids(&["old-id", "---"]),
            &p,
        )
        .unwrap()
        .content
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
            // Both rungs at once: the space forces the brackets and the
            // `>` would close them, so it is escaped inside them.
            ("new >x.md", "[Old](<new \\>x.md>)"),
            // A raw `&` opens an entity, so the plain spelling of this
            // name is read back as `a©.md` and names a different file.
            ("a&copy;.md", "[Old](a\\&copy;.md)"),
        ] {
            let out = rewrite_for_move(
                "[Old](old.md)",
                Rewriting::Referrer(Path::new("")),
                Path::new("old.md"),
                Path::new(new_path),
                Worlds {
                    before: &bound_of(&BTreeSet::from(["old.md".to_string()])),
                    after: &after_move(&BTreeSet::from(["old.md".to_string()]), "old.md", new_path),
                },
                &p,
            )
            .unwrap()
            .content
            .expect("the link is repointed rather than left behind");
            assert_eq!(out, after, "new_path: {new_path:?}");
        }
    }

    #[test]
    fn proposals_that_pass_alone_are_refused_when_they_do_not_pass_together() {
        // Three captures on a line reading `xxx`, each rewritten to `-`.
        // Alone, none moves the frontmatter boundary; together they spell
        // `---`, and the document acquires somebody else's id and loses
        // every edge it had — under an envelope reporting success.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: "(x|-)".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        let out = rewrite_for_move(
            "xxx\nid: accidental\n---\n",
            Rewriting::Referrer(Path::new("")),
            Path::new("x.md"),
            Path::new("-.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["x.md".to_string()])),
                after: &after_move(&BTreeSet::from(["x.md".to_string()]), "x.md", "-.md"),
            },
            &p,
        )
        .unwrap()
        .content
        .expect("the proposals that hold are applied");
        assert_eq!(out, "--x\nid: accidental\n---\n");
        assert!(
            crate::parser::frontmatter::split_frontmatter(&out)
                .unwrap()
                .0
                .is_none(),
            "the document is still the one that was edited"
        );
    }

    #[test]
    fn a_rewrite_gives_itself_up_rather_than_cost_the_document_a_reference() {
        // A pattern whose match reaches past its capture depends on text
        // another reference occupies. Repointing both leaves the first
        // matching nowhere: its edge did not come to dangle — which
        // `check` reports — it stopped existing, which nothing reports.
        // Backing off the later rewrite keeps both references, one
        // repointed and one visibly naming a file that has moved.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"(\S+\.md) a\.md".to_string(),
                    relation: "references".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"(\S+\.md)$".to_string(),
                    relation: "mentions".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let out = rewrite_for_move(
            "a.md a.md",
            Rewriting::Referrer(Path::new("")),
            Path::new("a.md"),
            Path::new("new.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["a.md".to_string()])),
                after: &after_move(&BTreeSet::from(["a.md".to_string()]), "a.md", "new.md"),
            },
            &p,
        )
        .unwrap()
        .content
        .expect("the rewrite that holds is applied");
        assert_eq!(out, "new.md a.md");
        assert_eq!(
            references(&out, &p).unwrap().spans.len(),
            references("a.md a.md", &p).unwrap().spans.len(),
            "every reference the document had, it still has"
        );
    }

    #[test]
    fn a_span_two_readers_bind_is_repointed_for_both_or_for_neither() {
        // `@r(old.md)` is one text two readers bind, and repointing it
        // re-spells the bytes both were read out of. The literal reader
        // cannot be read out of what the repoint says, so the edge it
        // carried would stop existing — which nothing reports — and it is
        // not the other reader's to retire, because the two name one
        // target under two relations and so are two edges. The rewrite
        // gives itself up; the reference stays, naming a file that moved.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"@r\(([a-z0-9./]+)\)".to_string(),
                    relation: "references".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"@r\((old\.md)\)".to_string(),
                    relation: "mentions".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let out = rewrite_for_move(
            "@r(old.md)",
            Rewriting::Referrer(Path::new("")),
            Path::new("old.md"),
            Path::new("new.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["old.md".to_string()])),
                after: &after_move(&BTreeSet::from(["old.md".to_string()]), "old.md", "new.md"),
            },
            &p,
        )
        .unwrap()
        .content;
        assert_eq!(out, None, "a repoint that costs an edge is not made");
    }

    #[test]
    fn a_reference_a_repoint_carried_may_not_be_lost_by_the_rewrite_after_it() {
        // The literal reader is carried by the first repoint — the bytes
        // it was read out of say what it now names — so it is a reference
        // like any other, and the rewrite after it may not take the tail
        // its match reaches for.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"@r\(([a-z0-9./]+)\)".to_string(),
                    relation: "references".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"@r\(([a-z0-9./]+)\) @r\(old\.md\)".to_string(),
                    relation: "mentions".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let out = rewrite_for_move(
            "@r(old.md) @r(old.md)",
            Rewriting::Referrer(Path::new("")),
            Path::new("old.md"),
            Path::new("new.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["old.md".to_string()])),
                after: &after_move(&BTreeSet::from(["old.md".to_string()]), "old.md", "new.md"),
            },
            &p,
        )
        .unwrap()
        .content
        .expect("the repoint that holds is applied");
        assert_eq!(out, "@r(new.md) @r(old.md)");
    }

    #[test]
    fn a_covered_reference_survives_a_spelling_of_its_own_target_it_did_not_write() {
        // The destination is repointed across directories, and the
        // capture inside it comes to say `docs2/a.md` where the rename
        // rendered `../docs2/a.md`. Neither spelling is the other and
        // both are the same file, so read as text the reference is lost
        // and the whole repoint is given up for it — leaving two edges
        // dangling where one rewrite would have kept both.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"\./([a-z0-9./]+)".to_string(),
                relation: "dotted".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        let out = rewrite_for_move(
            "see [t](./a.md) here",
            Rewriting::Referrer(Path::new("docs")),
            Path::new("docs/a.md"),
            Path::new("docs2/a.md"),
            Worlds {
                before: &Bindings::of([(Path::new("docs/a.md"), "doc-a")]),
                after: &Bindings::of([(Path::new("docs2/a.md"), "doc-a")]),
            },
            &p,
        )
        .unwrap()
        .content;
        assert_eq!(out.as_deref(), Some("see [t](../docs2/a.md) here"));
    }

    #[test]
    fn a_covered_reference_keeps_the_frame_its_author_wrote_for_it() {
        // `./old.md` says the frame out loud, and a reader inside the
        // destination reads for it. Rendering the repoint without it
        // leaves that reader nothing to be read out of, and the rewrite
        // gives itself up for a reference no spelling had to cost —
        // so the frame the author wrote is one of the things preserved,
        // like the extension they wrote and the padding they left.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"\./([a-z0-9.]+)".to_string(),
                relation: "dotted".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        let out = rewrite_for_move(
            "[t](./old.md)",
            Rewriting::Referrer(Path::new("")),
            Path::new("old.md"),
            Path::new("deep.md"),
            Worlds {
                before: &Bindings::of([(Path::new("old.md"), "doc-a")]),
                after: &Bindings::of([(Path::new("deep.md"), "doc-a")]),
            },
            &p,
        )
        .unwrap()
        .content;
        assert_eq!(out.as_deref(), Some("[t](./deep.md)"));
    }

    #[test]
    fn a_covered_reference_that_names_a_document_is_not_the_coverer_s_to_take() {
        // A basename pattern reads `a.md` out of `docs/a.md`, and here
        // that names a document of its own. Repointing the destination
        // spells it away, and the edge it carried would go with it —
        // reported by nothing, because the graph left behind is valid. It
        // asked for nothing and so cannot be repointed; what is left is
        // to give the rewrite up, and the reference dangles where `check`
        // says so.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"(docs/\S+\.md)".to_string(),
                    relation: "full".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"\b(a\.md)\b".to_string(),
                    relation: "base".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let paths = BTreeSet::from(["docs/a.md".to_string(), "a.md".to_string()]);
        let out = rewrite_for_move(
            "docs/a.md",
            Rewriting::Referrer(Path::new("")),
            Path::new("docs/a.md"),
            Path::new("docs/b.md"),
            Worlds {
                before: &bound_of(&paths),
                after: &after_move(&paths, "docs/a.md", "docs/b.md"),
            },
            &p,
        )
        .unwrap()
        .content;
        assert_eq!(out, None, "a repoint that costs an edge is not made");
    }

    #[test]
    fn covered_references_spelled_alike_are_one_edge_and_answer_as_one() {
        // `a.md,a.md` renamed to `a.md` leaves one `a.md` where the
        // destination held two, and both covered captures are answered by
        // it. Alike means the same pattern and the same text, which is one
        // relation to one target — an edge the graph holds once however
        // many times the document spells it, so the pair that collapses is
        // a pair it never told apart.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"\b(a\.md)\b".to_string(),
                    relation: "alpha".to_string(),
                    code_spans: false,
                },
                // The same reader under a second relation: two edges from
                // one text, so a landing answering for both must not cost
                // either.
                LinkPattern {
                    pattern: r"\b(a\.md)\b".to_string(),
                    relation: "beta".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let out = rewrite_for_move(
            "[x](a.md,a.md)",
            Rewriting::Referrer(Path::new("")),
            Path::new("a.md,a.md"),
            Path::new("a.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["a.md,a.md".to_string()])),
                after: &after_move(
                    &BTreeSet::from(["a.md,a.md".to_string()]),
                    "a.md,a.md",
                    "a.md",
                ),
            },
            &p,
        )
        .unwrap()
        .content
        .expect("the destination is repointed");
        assert_eq!(out, "[x](a.md)");
        let edges = |body: &str| {
            let mut targets: Vec<String> = crate::parser::body::extract_links(body, &p)
                .into_iter()
                .map(|edge| format!("{}/{}", edge.relation, edge.target_path))
                .collect();
            targets.sort_unstable();
            targets.dedup();
            targets
        };
        assert_eq!(edges(&out).len(), edges("[x](a.md,a.md)").len());
    }

    #[test]
    fn a_rewrite_inside_a_reference_nothing_chose_does_not_pass_for_it() {
        // The accepted span starts inside an earlier-starting capture that
        // nothing chose, so the rewrite replaces that capture's text. Read
        // by position alone the capture answered for bytes that were no
        // longer its own — but only when the successor happened to be the
        // same length, so the same mutation was written or refused by a
        // coincidence of bytes.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"(docs/\S+\.md)".to_string(),
                    relation: "full".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"docs/(\S+\.md)".to_string(),
                    relation: "base".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let scope = BTreeSet::from(["old.md".to_string(), "docs/old.md".to_string()]);
        for new in ["new.md", "newer.md"] {
            assert!(
                rewrite_for_move(
                    "see docs/old.md here",
                    Rewriting::Referrer(Path::new("")),
                    Path::new("old.md"),
                    Path::new(new),
                    Worlds {
                        before: &bound_of(&scope),
                        after: &after_move(&scope, "old.md", new),
                    },
                    &p,
                )
                .unwrap()
                .content
                .is_none(),
                "the reference to docs/old.md, a file still there, is not repointed: {new}"
            );
        }
    }

    #[test]
    fn a_citation_never_shares_a_span_with_a_reference_in_prose() {
        // A citation lies inside an inline code span and every other form
        // is admitted only where prose is, so the two can never overlap —
        // which is why no citation is ever covered by a rewrite, or covers
        // one.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"(adr-\d+)".to_string(),
                relation: "cites".to_string(),
                code_spans: true,
            }],
            ..parser()
        };
        let content = "[x](adr-1.md) `adr-2` adr-3 `adr-4`[y](adr-5.md)\n";
        let found = references(content, &p).unwrap();
        for (index, one) in found.spans.iter().enumerate() {
            for other in &found.spans[index + 1..] {
                let cited = |span: &ReferenceSpan| matches!(span.form, ReferenceForm::Citation(_));
                if cited(one) != cited(other) {
                    assert!(
                        one.start >= other.end || other.start >= one.end,
                        "a citation shares a span with prose: {:?} {:?}",
                        one.start..one.end,
                        other.start..other.end
                    );
                }
            }
        }
    }

    #[test]
    fn a_rewrite_that_reached_into_a_reference_leaves_it_nowhere_to_be_read() {
        // The capture straddles two link destinations, so repointing it
        // rewrites part of the second one. Read at a range widened to
        // cover what the rewrite wrote, the *first* destination — which
        // the rename never touched, and which names a file still there —
        // answered for the second, and `[y](a.md)` went out as
        // `[y](b.md)` under a success envelope.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"(md\) \[y]\(\S)".to_string(),
                relation: "odd".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        assert!(
            rewrite_for_move(
                "[x](a.md) [y](a.md)",
                Rewriting::Referrer(Path::new("")),
                Path::new("md) [y](a.md"),
                Path::new("md) [y](b.md"),
                Worlds {
                    before: &bound_of(&BTreeSet::from([
                        "a.md".to_string(),
                        "md) [y](a.md".to_string()
                    ])),
                    after: &after_move(
                        &BTreeSet::from(["a.md".to_string(), "md) [y](a.md".to_string()]),
                        "md) [y](a.md",
                        "md) [y](b.md",
                    ),
                },
                &p,
            )
            .unwrap()
            .content
            .is_none(),
            "the destinations the capture reaches into are not the capture's to change"
        );
    }

    #[test]
    fn a_reached_into_reference_is_refused_even_where_it_would_have_survived() {
        // What the rewrite writes re-spells the `md` it took from the
        // destination beside it, so that destination is still there to be
        // read — but at no range this document says is its. Refusing is
        // the trade for never guessing one: the capture stays, naming a
        // file that has moved, which `check` reports.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"(md\) \S+)".to_string(),
                relation: "odd".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        assert!(
            rewrite_for_move(
                "[x](a.md) end",
                Rewriting::Referrer(Path::new("")),
                Path::new("md) end.md"),
                Path::new("md) fin.md"),
                Worlds {
                    before: &bound_of(&BTreeSet::from([
                        "a.md".to_string(),
                        "md) end.md".to_string(),
                    ])),
                    after: &after_move(
                        &BTreeSet::from(["a.md".to_string(), "md) end.md".to_string()]),
                        "md) end.md",
                        "md) fin.md",
                    ),
                },
                &p,
            )
            .unwrap()
            .content
            .is_none(),
            "a reference read at no range of its own is not repointed on a guess"
        );
    }

    #[test]
    fn a_rewrite_takes_nothing_it_only_reached_into() {
        // The capture starts where the rewrite does and runs past it, so
        // the rewrite replaced part of it rather than it. Read as though
        // it had, the capture would be the rewrite's to take and its edge
        // would go without a word.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"(a\.md)".to_string(),
                    relation: "short".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"(a\.md,\S+)".to_string(),
                    relation: "long".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        assert!(
            rewrite_for_move(
                "see a.md,keep here",
                Rewriting::Referrer(Path::new("")),
                Path::new("a.md"),
                Path::new("b.md"),
                Worlds {
                    before: &bound_of(&BTreeSet::from([
                        "a.md".to_string(),
                        "a.md,keep".to_string()
                    ])),
                    after: &after_move(
                        &BTreeSet::from(["a.md".to_string(), "a.md,keep".to_string()]),
                        "a.md",
                        "b.md",
                    ),
                },
                &p,
            )
            .unwrap()
            .content
            .is_none(),
            "the longer capture keeps the bytes the rewrite did not replace"
        );
    }

    #[test]
    fn a_covered_reference_the_rewrite_still_spells_is_not_subsumed() {
        // `docs/a.md` repointed to `docs2/a.md` still spells the `a.md`
        // nested inside it, so that reference survived its coverer and is
        // a reference like any other — the rewrite of the tail after it
        // may not cost it. Exempting every covered span alike dropped it
        // for a rewrite that never overlapped it.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"^(\S+/a\.md)".to_string(),
                    relation: "full".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"(a\.md) a\.md".to_string(),
                    relation: "nested".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"(\S+\.md)$".to_string(),
                    relation: "tail".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let before = "docs/a.md a.md";
        let out = rewrite_for_move(
            before,
            Rewriting::Referrer(Path::new("docs")),
            Path::new("docs/a.md"),
            Path::new("docs2/a.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["docs/a.md".to_string()])),
                after: &after_move(
                    &BTreeSet::from(["docs/a.md".to_string()]),
                    "docs/a.md",
                    "docs2/a.md",
                ),
            },
            &p,
        )
        .unwrap()
        .content
        .expect("the covering rewrite is applied");
        assert_eq!(out, "docs2/a.md a.md");
        assert_eq!(
            references(&out, &p).unwrap().spans.len(),
            references(before, &p).unwrap().spans.len(),
            "every reference the document had, it still has"
        );
    }

    #[test]
    fn a_reference_the_rewritten_span_covers_is_not_one_the_rewrite_lost() {
        // A full-path pattern and a basename pattern see the same file
        // through overlapping text, and no rewrite can keep both spellings
        // — the second was never independently satisfiable, which is why
        // only the earlier of two overlapping proposals is honoured. Read
        // back as if it were, a rename became a no-op for every such line.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"(docs/\S+\.md)".to_string(),
                    relation: "full".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"\b(a\.md)\b".to_string(),
                    relation: "base".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let out = rewrite_for_move(
            "xx docs/a.md yy",
            Rewriting::Referrer(Path::new("")),
            Path::new("docs/a.md"),
            Path::new("docs/b.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["docs/a.md".to_string()])),
                after: &after_move(
                    &BTreeSet::from(["docs/a.md".to_string()]),
                    "docs/a.md",
                    "docs/b.md",
                ),
            },
            &p,
        )
        .unwrap()
        .content
        .expect("the covering rewrite is applied");
        assert_eq!(out, "xx docs/b.md yy");
    }

    #[test]
    fn only_the_rewrite_that_costs_a_reference_is_refused() {
        // The first rewrite is what breaks `second`, and it is ahead of
        // `third` in the document — so refusing by recency would give up
        // `third` as well, and the one after that, down to rewriting
        // nothing. A refusal is attributed instead: a trial differs from
        // what is accepted by one rewrite, so what it loses, that rewrite
        // cost. `third` is repointed and `first` is left naming a file
        // that has moved, which `check` reports.
        let p = ParserConfig {
            link_patterns: vec![
                LinkPattern {
                    pattern: r"(\S+\.md) \S+\.md".to_string(),
                    relation: "first".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"a\.md (\S+\.md)".to_string(),
                    relation: "second".to_string(),
                    code_spans: false,
                },
                LinkPattern {
                    pattern: r"and (\S+\.md)".to_string(),
                    relation: "third".to_string(),
                    code_spans: false,
                },
            ],
            ..parser()
        };
        let before = "a.md keep.md and a.md";
        let out = rewrite_for_move(
            before,
            Rewriting::Referrer(Path::new("")),
            Path::new("a.md"),
            Path::new("new.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["a.md".to_string(), "keep.md".to_string()])),
                after: &after_move(
                    &BTreeSet::from(["a.md".to_string(), "keep.md".to_string()]),
                    "a.md",
                    "new.md",
                ),
            },
            &p,
        )
        .unwrap()
        .content
        .expect("the rewrite that costs nothing is applied");
        assert_eq!(out, "a.md keep.md and new.md");
        assert_eq!(
            references(&out, &p).unwrap().spans.len(),
            references(before, &p).unwrap().spans.len(),
            "every reference the document had, it still has"
        );
    }

    #[test]
    fn a_capture_the_pattern_would_not_read_back_is_left_alone() {
        // The id rewriter asked whether a pattern still reads its own
        // replacement; the path rewriters did not, and wrote `[[b]c]]`
        // over a wikilink — no reference at all, the edge gone from a
        // write that named the file updated.
        let mut p = parser();
        p.wikilink_enabled = true;
        assert!(
            rewrite_for_move(
                "see [[a]] here",
                Rewriting::Referrer(Path::new("")),
                Path::new("a.md"),
                Path::new("b]c.md"),
                Worlds {
                    before: &bound_of(&BTreeSet::from(["a.md".to_string()])),
                    after: &after_move(&BTreeSet::from(["a.md".to_string()]), "a.md", "b]c.md"),
                },
                &p,
            )
            .unwrap()
            .content
            .is_none(),
            "the wikilink stays visible rather than being written out of existence"
        );
    }

    #[test]
    fn a_rebased_capture_answers_the_same_question() {
        // `rewrite_moved_references` asked nothing at all. A pattern that
        // spells bare words got `@ref(b/a)` written into it — no
        // reference to the next build, so the moved file's own edge was
        // gone from a write that answered success.
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"@ref\(([a-z]+)\)".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }],
            ..parser()
        };
        assert!(
            rebase("@ref(a)", "a/b", "a", &["a/b/a.md"], &p)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_pointy_destination_keeps_its_brackets_when_the_path_needs_them() {
        // The span sits inside the author's `<…>`, so the escaped spelling
        // is the one that reads back — a second pair of brackets would not.
        let p = ParserConfig {
            extensions: vec![".md".to_string()],
            ..ParserConfig::default()
        };
        let out = rewrite_for_move(
            "[Old](<old.md>)",
            Rewriting::Referrer(Path::new("")),
            Path::new("old.md"),
            Path::new("new name.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["old.md".to_string()])),
                after: &after_move(
                    &BTreeSet::from(["old.md".to_string()]),
                    "old.md",
                    "new name.md",
                ),
            },
            &p,
        )
        .unwrap()
        .content
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
            &ids(&["old-id", "new-id"]),
            &p,
        )
        .unwrap()
        .content
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
        let out = rewrite_for_move(
            "see [x](<docs/a.md >) here",
            Rewriting::Referrer(Path::new("")),
            Path::new("docs/a.md"),
            Path::new("docs/b.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["docs/a.md".to_string()])),
                after: &after_move(
                    &BTreeSet::from(["docs/a.md".to_string()]),
                    "docs/a.md",
                    "docs/b.md",
                ),
            },
            &p,
        )
        .unwrap()
        .content
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
        let out = rewrite_for_move(
            "---\nid: linker\nnote: |\n  ```\n---\n\n[x](docs/a.md)\n",
            Rewriting::Referrer(Path::new("")),
            Path::new("docs/a.md"),
            Path::new("docs/b.md"),
            Worlds {
                before: &bound_of(&BTreeSet::from(["docs/a.md".to_string()])),
                after: &after_move(
                    &BTreeSet::from(["docs/a.md".to_string()]),
                    "docs/a.md",
                    "docs/b.md",
                ),
            },
            &p,
        )
        .unwrap()
        .content
        .expect("the link is repointed");
        assert!(out.ends_with("[x](docs/b.md)\n"), "{out:?}");
    }

    #[test]
    fn a_second_reader_of_one_edge_does_not_refuse_the_repoint_it_cannot_follow() {
        // The wikilink reader and a pattern fixed on the old id bind the
        // same bytes, under the same relation, to the same target: one
        // edge the graph holds once. The pattern cannot be read out of
        // `[[new]]`, but it takes no edge with it — the reader beside it
        // carries the same one — so refusing the repoint over it would
        // cost the rewrite to protect nothing.
        let p = ParserConfig {
            wikilink_enabled: true,
            link_patterns: vec![LinkPattern {
                pattern: r"\[\[(old)\]\]".to_string(),
                relation: "references".to_string(),
                code_spans: false,
            }],
            ..ParserConfig::default()
        };
        assert_eq!(
            rewrite_id_references(
                "see [[old]]\n",
                "old",
                "new",
                Path::new(""),
                &ids(&["old", "new"]),
                &p,
            )
            .unwrap()
            .content
            .as_deref(),
            Some("see [[new]]\n")
        );
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
                &ids(&["old-id", "---"]),
                &p,
            )
            .unwrap()
            .content
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
                &ids(&["old-id", "~~~"]),
                &p,
            )
            .unwrap()
            .content
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
                &ids(&["old-id", "new`id"]),
                &p,
            )
            .unwrap()
            .content
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
            &ids(&["old", "paren)id"]),
            &p,
        )
        .unwrap()
        .content
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
                &ids(&["old", "new"]),
                &p
            )
            .unwrap()
            .content
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
            rewrite_id_references(
                "see [[old]]",
                "old",
                "new",
                Path::new(""),
                &bound_of(&scope),
                &p
            )
            .unwrap()
            .content
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
                &ids(&["old", "new"]),
                &p
            )
            .unwrap()
            .content
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
            &ids(&["old-id", "new-id"]),
            &p,
        )
        .unwrap()
        .content
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
        let out = rewrite_id_references(
            content,
            "old",
            "new",
            Path::new(""),
            &ids(&["old", "new"]),
            &p,
        )
        .unwrap()
        .content
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
        let out = rewrite_id_references(
            content,
            "old",
            "new",
            Path::new(""),
            &ids(&["old", "new"]),
            &p,
        )
        .unwrap()
        .content
        .expect("changed");
        assert!(
            out.contains("note: \"[[old]]\""),
            "frontmatter untouched: {out}"
        );
        assert!(out.contains("Body [[new]]."), "body rewritten: {out}");
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        /// The fragments a generated document is built from — every
        /// reference form the rewriter reaches, plus the text around them
        /// that decides how markdown reads them.
        fn fragment() -> impl Strategy<Value = &'static str> {
            prop::sample::select(vec![
                "[t](old.md)",
                "[t](./old.md)",
                "[t](<old.md>)",
                "[t](old.md#sec)",
                "[t](o&#x6c;d.md)",
                "[t][r]",
                "[[old]]",
                "[[ old ]]",
                "`old`",
                "@ref(old)",
                "old.md old.md",
                " and ",
                "`",
                "\n",
                "---",
                "x",
                "> ",
                "    ",
            ])
        }

        /// Names a rename can land on, including the ones no plain
        /// destination carries.
        fn new_path() -> impl Strategy<Value = &'static str> {
            prop::sample::select(vec![
                "new.md",
                "new name.md",
                "new(1).md",
                "new(1.md",
                "new>x.md",
                "new&copy;.md",
                "new`x.md",
                "new].md",
                "-.md",
                "sub/new.md",
            ])
        }

        /// References that overlap each other were never independently
        /// rewritable — the rewrite that wins the span answers for all of
        /// them — so a cluster of them counts once.
        fn clusters(spans: &[ReferenceSpan]) -> usize {
            let mut ranges: Vec<(usize, usize)> =
                spans.iter().map(|span| (span.start, span.end)).collect();
            ranges.sort_unstable();
            let mut count = 0usize;
            let mut reach = 0usize;
            for (start, end) in ranges {
                if count == 0 || start >= reach {
                    count += 1;
                    reach = end;
                } else {
                    reach = reach.max(end);
                }
            }
            count
        }

        /// [`parser_config`] plus a capture whose successor can spell a
        /// whole line. It is kept out of the general property because a
        /// bare word nests inside every other form, and rewrites that
        /// subsume one — or mint a longer capture bridging two — move the
        /// count that property measures without a reference being lost.
        fn fence_spelling_config() -> ParserConfig {
            let mut parser = parser_config();
            parser.link_patterns.push(LinkPattern {
                pattern: r"(old|-)".to_string(),
                relation: "word".to_string(),
                code_spans: false,
            });
            parser
        }

        fn parser_config() -> ParserConfig {
            ParserConfig {
                extensions: vec![".md".to_string()],
                wikilink_enabled: true,
                link_patterns: vec![
                    LinkPattern {
                        pattern: r"@ref\(([^)\n]+)\)".to_string(),
                        relation: "references".to_string(),
                        code_spans: true,
                    },
                    // A pattern whose *match* reaches past its capture, into
                    // text another proposal may edit — and one that edits
                    // exactly there.
                    LinkPattern {
                        pattern: r"(\S+\.md) old\.md".to_string(),
                        relation: "tail".to_string(),
                        code_spans: false,
                    },
                    LinkPattern {
                        pattern: r"(\S+\.md)$".to_string(),
                        relation: "line_end".to_string(),
                        code_spans: false,
                    },
                ],
            }
        }

        /// Readers that bind one another's bytes: the literal one is read
        /// out of exactly the span the general one is, and the dotted one
        /// out of a span inside the destination a markdown link binds
        /// whole. Every reference these spell names the file being
        /// renamed, so none of them is a coverer's to subsume.
        fn shared_span_config() -> ParserConfig {
            ParserConfig {
                extensions: vec![".md".to_string()],
                wikilink_enabled: true,
                link_patterns: vec![
                    LinkPattern {
                        pattern: r"@ref\(([a-z0-9./]+)\)".to_string(),
                        relation: "references".to_string(),
                        code_spans: false,
                    },
                    LinkPattern {
                        pattern: r"@ref\((old\.md)\)".to_string(),
                        relation: "literal".to_string(),
                        code_spans: false,
                    },
                    LinkPattern {
                        pattern: r"\./([a-z0-9./]+)".to_string(),
                        relation: "dotted".to_string(),
                        code_spans: false,
                    },
                ],
            }
        }

        fn shared_span_fragment() -> impl Strategy<Value = &'static str> {
            prop::sample::select(vec![
                "@ref(old.md)",
                "[t](./old.md)",
                "[t](old.md)",
                "[[old]]",
                "old.md",
                " and ",
                "\n",
                "x",
            ])
        }

        proptest! {
            /// Extraction and rewriting surface the same references.
            ///
            /// Everything the rewriter may do rests on this: it may touch
            /// exactly what the builder binds as an edge, so a reference
            /// only one of the two readers finds is either an edge no
            /// rewrite can repoint or text no edge protects. The two share
            /// their helpers to make it true; this is the assertion that
            /// they still do, over every document the fragments compose.
            #[test]
            fn extraction_and_rewriting_surface_the_same_references(
                fragments in prop::collection::vec(fragment(), 1..16),
                frontmatter in any::<bool>(),
            ) {
                let head = if frontmatter { "---\nid: doc\n---\n" } else { "" };
                let content = format!("{head}{}\n\n[r]: old.md\n", fragments.concat());
                let parser = parser_config();
                let Ok((_, body)) = crate::parser::frontmatter::split_frontmatter(&content)
                else {
                    return Ok(());
                };
                let extracted: BTreeSet<(String, String)> = body::extract_links(body, &parser)
                    .into_iter()
                    .map(|edge| (edge.relation, edge.target_path))
                    .collect();
                let Ok(found) = references(&content, &parser) else { return Ok(()) };
                let surfaced: BTreeSet<(String, String)> = found
                    .spans
                    .into_iter()
                    .map(|span| (span.relation, span.target))
                    .collect();
                prop_assert_eq!(surfaced, extracted, "content={:?}", content);
            }

            /// A rewrite never puts a frontmatter boundary where the
            /// document had none.
            ///
            /// Asked separately because the general property reaches it
            /// only when three independent draws agree — a document
            /// without frontmatter, a fragment whose rewrites can spell a
            /// whole line, and a successor short enough to do it. Here all
            /// three are fixed and only the surrounding text varies.
            #[test]
            fn a_rewrite_never_gives_the_document_a_frontmatter_it_had_none_of(
                fragments in prop::collection::vec(fragment(), 1..8),
            ) {
                let content = format!("oldoldold{}\n", fragments.concat());
                let parser = fence_spelling_config();
                let rewritten = rewrite_for_move(
            &content,
            Rewriting::Referrer(Path::new("")),
            Path::new("old.md"),
            Path::new("-.md"),
            Worlds {
                        before: &bound_of(&BTreeSet::from(["old.md".to_string()])),
                        after: &after_move(&BTreeSet::from(["old.md".to_string()]), "old.md", "-.md"),
                    },
            &parser,
        );
                let Ok(Rewritten { content: Some(rewritten), .. }) = rewritten else { return Ok(()) };
                prop_assert!(
                    crate::parser::frontmatter::split_frontmatter(&rewritten)
                        .is_ok_and(|(yaml, _)| yaml.is_none()),
                    "the document had no frontmatter and the rewrite gave it one\n{:?}",
                    rewritten
                );
            }

            /// A rewrite never destroys a reference.
            ///
            /// Proposals are applied one after another, and each is read
            /// back only in the document the ones before it made — so the
            /// question this leaves open is whether a *later* edit can stop
            /// an earlier one from being read. It cannot be allowed to:
            /// the reference would be gone from a write that reported
            /// success, which is the whole failure this seam exists to
            /// prevent. Counting is enough to catch it, since a proposal
            /// that is refused leaves its reference exactly where it was.
            #[test]
            fn a_rewrite_never_leaves_fewer_references_than_it_found(
                fragments in prop::collection::vec(fragment(), 1..12),
                new in new_path(),
                frontmatter in any::<bool>(),
            ) {
                let body = fragments.concat();
                let head = if frontmatter { "---\nid: doc\n---\n" } else { "" };
                let content = format!("{head}{body}\n\n[r]: old.md\n");
                let parser = parser_config();
                let before = references(&content, &parser);
                let after = rewrite_for_move(
            &content,
            Rewriting::Referrer(Path::new("")),
            Path::new("old.md"),
            Path::new(new),
            Worlds {
                        before: &bound_of(&BTreeSet::from(["old.md".to_string()])),
                        after: &after_move(&BTreeSet::from(["old.md".to_string()]), "old.md", new),
                    },
            &parser,
        );
                let Ok(Rewritten { content: Some(rewritten), .. }) = after else { return Ok(()) };
                let before = before.expect("the generated document parses");
                let found = references(&rewritten, &parser)
                    .expect("a rewrite leaves a document that still parses");
                prop_assert_eq!(
                    found.frontmatter, before.frontmatter,
                    "the document a rewrite leaves is still this document\n{:?}", rewritten
                );
                prop_assert!(
                    clusters(&found.spans) >= clusters(&before.spans),
                    "{} references went in and {} came out\n{:?}",
                    clusters(&before.spans), clusters(&found.spans), rewritten
                );
            }

            /// A rewrite never leaves a relation none of its references
            /// carry.
            ///
            /// Asked separately, and of edges rather than spans, because
            /// the property above counts overlap clusters: a span several
            /// readers bind is one cluster however many edges it holds,
            /// so a reader retired out of it is invisible there. Every
            /// reference these fragments spell names the file being
            /// renamed, so each has a target of its own, none is a
            /// coverer's to subsume, and the document that comes out must
            /// still be read by every reader that read the one going in.
            #[test]
            fn a_rewrite_never_leaves_a_relation_none_of_its_references_carry(
                fragments in prop::collection::vec(shared_span_fragment(), 1..10),
                new in new_path(),
            ) {
                let content = format!("---\nid: doc\n---\n{}\n", fragments.concat());
                let parser = shared_span_config();
                let relations = |body: &str| -> BTreeSet<String> {
                    crate::parser::body::extract_links(body, &parser)
                        .into_iter()
                        .map(|edge| edge.relation)
                        .collect()
                };
                let before = relations(&content);
                let after = rewrite_for_move(
            &content,
            Rewriting::Referrer(Path::new("")),
            Path::new("old.md"),
            Path::new(new),
            Worlds {
                        before: &bound_of(&BTreeSet::from(["old.md".to_string()])),
                        after: &after_move(&BTreeSet::from(["old.md".to_string()]), "old.md", new),
                    },
            &parser,
        );
                let Ok(Rewritten { content: Some(rewritten), .. }) = after else { return Ok(()) };
                let found = relations(&rewritten);
                prop_assert!(
                    before.is_subset(&found),
                    "relations {:?} went in and {:?} came out\n{:?}",
                    before, found, rewritten
                );
            }
        }
    }
}
