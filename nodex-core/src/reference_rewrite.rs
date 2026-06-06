//! Rewriting body-link references when a document moves.
//!
//! `rename` moves a file and must repoint every other document's links so
//! the graph stays connected. The matching logic here is the *same* one
//! the build-time resolver uses — [`reference_path_candidates`] plus the
//! shared `normalize_relative` primitive — so the rewriter can never
//! disagree with the graph about what a link points to. Code is the one
//! place a fuzzy text rewrite could corrupt a document, so every token
//! inside a fenced/indented code block or an inline code span is left
//! untouched (mirroring pulldown-cmark's own link extraction).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
/// `source_dir` is the linking file's parent directory (project-root
/// relative); `old_path` / `new_path` are project-root-relative paths.
pub fn rewrite_references(
    content: &str,
    source_dir: &Path,
    old_path: &Path,
    new_path: &Path,
    parser: &ParserConfig,
) -> Option<String> {
    let old_norm = crate::path_guard::forward_string(old_path);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    // Markdown inline links: scanned across the whole document, skipping
    // code blocks and inline code spans exactly as pulldown-cmark does
    // when it extracts the corresponding edges.
    let protected = body::protected_byte_ranges(content);
    for caps in markdown_link_re().captures_iter(content) {
        let target = caps.get(1).expect("group 1 is the link target");
        if overlaps(target.start(), target.end(), &protected) {
            continue;
        }
        if let Some(replacement) = rewritten_target(
            target.as_str(),
            source_dir,
            &old_norm,
            new_path,
            &parser.extensions,
        ) {
            edits.push((target.start(), target.end(), replacement));
        }
    }

    // Wikilinks and custom patterns: scanned per line (their patterns are
    // line-anchored), skipping any capture whose bytes fall inside a code
    // block OR an inline code span — a mutating rewrite must never reach
    // into code, even a one-backtick sample on a prose line.
    let mut line_regexes: Vec<Regex> = Vec::new();
    if parser.wikilink_enabled {
        line_regexes.push(body::wikilink_regex().clone());
    }
    for pattern in &parser.link_patterns {
        line_regexes
            .push(Regex::new(&pattern.pattern).expect("link patterns validated by Config::load"));
    }
    if !line_regexes.is_empty() {
        let mut line_start = 0usize;
        for line in content.split_inclusive('\n') {
            let text_len = line.trim_end_matches('\n').len();
            for re in &line_regexes {
                for caps in re.captures_iter(&line[..text_len]) {
                    let Some(target) = caps.get(1) else {
                        continue;
                    };
                    let (start, end) = (line_start + target.start(), line_start + target.end());
                    if overlaps(start, end, &protected) {
                        continue;
                    }
                    if let Some(replacement) = rewritten_target(
                        target.as_str(),
                        source_dir,
                        &old_norm,
                        new_path,
                        &parser.extensions,
                    ) {
                        edits.push((start, end, replacement));
                    }
                }
            }
            line_start += line.len();
        }
    }

    if edits.is_empty() {
        return None;
    }
    Some(apply_edits(content, edits))
}

/// Rewrite every body id reference to `old_id` so it names `new_id`,
/// returning the rewritten document — or `None` when none was present.
///
/// Ids appear in the body only as `[[wikilink]]` or `[[parser.link_patterns]]`
/// targets (markdown links are paths, not ids), so only those are scanned,
/// per line and skipping any capture inside a code block or an inline code
/// span — a mutating rewrite must never reach into code. The capture must
/// equal `old_id` verbatim (a path reference that merely contains the id is
/// untouched).
pub fn rewrite_id_references(
    content: &str,
    old_id: &str,
    new_id: &str,
    parser: &ParserConfig,
) -> Option<String> {
    let mut line_regexes: Vec<Regex> = Vec::new();
    if parser.wikilink_enabled {
        line_regexes.push(body::wikilink_regex().clone());
    }
    for pattern in &parser.link_patterns {
        line_regexes
            .push(Regex::new(&pattern.pattern).expect("link patterns validated by Config::load"));
    }
    if line_regexes.is_empty() {
        return None;
    }

    let protected = body::protected_byte_ranges(content);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut line_start = 0usize;
    for line in content.split_inclusive('\n') {
        let text_len = line.trim_end_matches('\n').len();
        for re in &line_regexes {
            for caps in re.captures_iter(&line[..text_len]) {
                if let Some(target) = caps.get(1)
                    && target.as_str().trim() == old_id
                {
                    let (start, end) = (line_start + target.start(), line_start + target.end());
                    if overlaps(start, end, &protected) {
                        continue;
                    }
                    edits.push((start, end, new_id.to_string()));
                }
            }
        }
        line_start += line.len();
    }

    if edits.is_empty() {
        return None;
    }
    Some(apply_edits(content, edits))
}

/// The replacement for a link `target`, or `None` when it does not point
/// to `old_path`. Tries the literal (root-relative) form first, then the
/// form resolved relative to the linking file's directory — the same two
/// passes the resolver runs.
fn rewritten_target(
    target: &str,
    source_dir: &Path,
    old_norm: &str,
    new_path: &Path,
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

    if points_to(normalized, old_norm, extensions) {
        return Some(render_target(new_path, None, keep_extension, extensions));
    }
    if let Some(rel) = crate::path_guard::normalize_relative(&source_dir.join(normalized))
        && points_to(&rel, old_norm, extensions)
    {
        return Some(render_target(
            new_path,
            Some(source_dir),
            keep_extension,
            extensions,
        ));
    }
    None
}

/// Whether `base` (a literal or source-relative target) denotes `old_norm`
/// under the shared candidate ladder — the single source of truth for
/// "what file does this reference point to".
fn points_to(base: &str, old_norm: &str, extensions: &[String]) -> bool {
    reference_path_candidates(base, extensions, true)
        .iter()
        .any(|candidate| candidate == old_norm)
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

/// Whether `[start, end)` overlaps any protected `(s, e)` range.
fn overlaps(start: usize, end: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(s, e)| start < e && end > s)
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

/// `](<target>)` — group 1 is the link target, group 2 an optional anchor
/// left untouched (only the target span is rewritten).
fn markdown_link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\]\(([^)#\s]+)(#[^)]*)?\)").expect("static regex compiles"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LinkPattern;

    fn parser() -> ParserConfig {
        ParserConfig::default()
    }

    fn rewrite(content: &str, source_dir: &str, old: &str, new: &str, p: &ParserConfig) -> String {
        rewrite_references(
            content,
            Path::new(source_dir),
            Path::new(old),
            Path::new(new),
            p,
        )
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
                &parser(),
            )
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
                &p,
            )
            .is_none()
        );
    }

    #[test]
    fn rewrites_custom_pattern_target_only() {
        let p = ParserConfig {
            link_patterns: vec![LinkPattern {
                pattern: r"@import\s+(\S+)".to_string(),
                relation: "imports".to_string(),
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
                &parser(),
            )
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
            }],
            ..ParserConfig::default()
        };
        let out =
            rewrite_id_references("see [[old]] and @cite(old)", "old", "new", &p).expect("changed");
        assert_eq!(out, "see [[new]] and @cite(new)");
    }

    #[test]
    fn id_rewrite_requires_exact_match() {
        let mut p = parser();
        p.wikilink_enabled = true;
        // `[[old-spec]]` must not match id `old` (substring), and prose `old`
        // is never a capture.
        assert!(rewrite_id_references("[[old-spec]] and old in prose", "old", "new", &p).is_none());
    }

    #[test]
    fn id_rewrite_skips_inline_code_and_fences() {
        let mut p = parser();
        p.wikilink_enabled = true;
        // A wikilink inside an inline code span or a fenced block is a sample,
        // not a reference — a mutating rewrite must leave it alone.
        let content = "real [[old]]\n`[[old]]`\n```\n[[old]]\n```\n";
        let out = rewrite_id_references(content, "old", "new", &p).expect("changed");
        assert_eq!(out, "real [[new]]\n`[[old]]`\n```\n[[old]]\n```\n");
    }
}
