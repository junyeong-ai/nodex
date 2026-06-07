//! Line-level YAML scalar helpers.
//!
//! These exist for tool actions that write a small number of scalar
//! fields back to a frontmatter file (`scaffold`, `migrate`, `lifecycle`).
//! A full YAML round-trip would re-emit the entire document and
//! destroy the user's key order, comments, and quoting style — so we
//! only ever touch lines that name the keys we are changing.
//!
//! ## Supported scalar subset
//!
//! [`parse_scalar_value`] reads exactly the scalar forms a top-level
//! `key: value` frontmatter line can carry: plain, single-quoted
//! (`''` escaping), and double-quoted (the backslash-escape alphabet
//! below). Everything else — block scalars (`|`, `>`), flow
//! collections (`[`, `{`), malformed or unterminated quoting — is not
//! a scalar this module can reason about and surfaces as `None`
//! (an authoring error to the caller), never as a silently mangled
//! value. Within that subset the reader agrees with `yaml_serde` (the
//! build-time parser) and round-trips [`quote`]'s own output, so a
//! value written by one nodex command is always read back verbatim by
//! the next.

use std::borrow::Cow;

/// Emit a YAML scalar that is always safe to parse back. Strings go
/// through a minimal double-quoted escape — backslash and double-quote
/// are the only two characters that matter inside a double-quoted YAML
/// scalar; everything else (unicode, colons, leading hyphens) is legal
/// as-is. Control characters are escaped so a stray newline cannot
/// break the line into unrelated YAML.
pub fn quote(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for c in value.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                escaped.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}

/// `key: "value"` line — the canonical form written by every nodex
/// tool action.
pub fn render_scalar_line(key: &str, value: &str) -> String {
    format!("{key}: {}", quote(value))
}

/// Top-level scalar key on a frontmatter line, or `None` if the line
/// is blank, a comment, indented (a nested scalar or list child), or
/// otherwise not `<key>:` shaped.
pub fn parse_scalar_key(line: &str) -> Option<&str> {
    if line.is_empty() {
        return None;
    }
    let first = line.chars().next()?;
    if first == '#' || first.is_whitespace() {
        return None;
    }
    let colon = line.find(':')?;
    let key = &line[..colon];
    if key.is_empty()
        || key
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
    {
        return None;
    }
    Some(key)
}

/// Scalar value from a `key: value` line. Quoted forms are decoded
/// (escapes resolved, surrounding quotes stripped); a plain value has
/// any whitespace-preceded `# comment` dropped. Borrows from `line`
/// when no decoding was needed. Returns `None` for anything outside
/// the supported scalar subset (see module docs) — block, flow,
/// unterminated or malformed quoting — so callers report an authoring
/// error instead of acting on a misread value.
pub fn parse_scalar_value(line: &str) -> Option<Cow<'_, str>> {
    let colon = line.find(':')?;
    let rest = line[colon + 1..].trim_start();
    // Indicators that can never begin a plain scalar: block scalars
    // (`|` / `>`), flow collections (`[` / `{`), node-property / reserved
    // markers (`&` anchor, `*` alias, `!` tag, `@` / backtick reserved),
    // and the `%` directive. `yaml_serde` resolves or rejects each, so
    // echoing the raw line text would diverge — an aliased `status: *s`
    // must not read back as the literal "*s" and slip past a vocabulary
    // or lifecycle-terminal gate. Refuse, so the caller reports an
    // authoring error instead of acting on a misread value. (A leading
    // `#` stays a value-then-comment, and a quoted value is handled by
    // the branches below.)
    let bytes = rest.as_bytes();
    if matches!(
        bytes.first(),
        Some(b'|' | b'>' | b'[' | b'{' | b'&' | b'*' | b'!' | b'@' | b'`' | b'%' | b',')
    ) {
        return None;
    }
    // `-` `?` `:` are a plain-scalar first character only when *not*
    // followed by a space: `-x` / `?x` / `:x` are scalars `yaml_serde`
    // reads verbatim, but `- x` / `? x` / `: x` (and the bare forms) are
    // block indicators it rejects. (`,` has no such exception — it is a
    // flow separator `yaml_serde` rejects in any leading position, so it
    // sits in the unconditional set above.) Refuse only the indicator
    // forms, so the reader stays in lock-step with the build parser
    // without false-rejecting a legitimate value.
    if matches!(bytes.first(), Some(b'-' | b'?' | b':'))
        && bytes.get(1).is_none_or(u8::is_ascii_whitespace)
    {
        return None;
    }
    if let Some(body) = rest.strip_prefix('"') {
        return scan_double_quoted(body);
    }
    if let Some(body) = rest.strip_prefix('\'') {
        return scan_single_quoted(body);
    }
    Some(Cow::Borrowed(strip_plain_comment(rest)))
}

/// Forward scan of a double-quoted scalar body (opening quote already
/// stripped). Decodes exactly the escape alphabet [`quote`] emits plus
/// the `\uHHHH` / `\UHHHHHHHH` forms `yaml_serde` accepts in
/// human-authored frontmatter; any other escape is malformed. Walking
/// bytes is sound because the syntax characters (`"`, `\`) are ASCII
/// and UTF-8 continuation bytes never collide with them.
fn scan_double_quoted(body: &str) -> Option<Cow<'_, str>> {
    let bytes = body.as_bytes();
    let mut decoded: Option<String> = None;
    let mut seg_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                if !is_only_trailing_comment(&body[i + 1..]) {
                    return None;
                }
                return Some(match decoded {
                    None => Cow::Borrowed(&body[..i]),
                    Some(mut s) => {
                        s.push_str(&body[seg_start..i]);
                        Cow::Owned(s)
                    }
                });
            }
            b'\\' => {
                let mut s = decoded.take().unwrap_or_default();
                s.push_str(&body[seg_start..i]);
                let (c, consumed) = decode_escape(&body[i + 1..])?;
                s.push(c);
                decoded = Some(s);
                i += 1 + consumed;
                seg_start = i;
            }
            _ => i += 1,
        }
    }
    None // unterminated
}

/// Forward scan of a single-quoted scalar body (opening quote already
/// stripped). The only escape in single-quoted YAML is `''` for a
/// literal quote; backslash carries no meaning.
fn scan_single_quoted(body: &str) -> Option<Cow<'_, str>> {
    let bytes = body.as_bytes();
    let mut decoded: Option<String> = None;
    let mut seg_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                let mut s = decoded.take().unwrap_or_default();
                s.push_str(&body[seg_start..i]);
                s.push('\'');
                decoded = Some(s);
                i += 2;
                seg_start = i;
            } else {
                if !is_only_trailing_comment(&body[i + 1..]) {
                    return None;
                }
                return Some(match decoded {
                    None => Cow::Borrowed(&body[..i]),
                    Some(mut s) => {
                        s.push_str(&body[seg_start..i]);
                        Cow::Owned(s)
                    }
                });
            }
        } else {
            i += 1;
        }
    }
    None // unterminated
}

/// Decode one backslash escape (backslash already consumed). Returns
/// the decoded character and the number of bytes consumed after the
/// backslash. Covers the complete YAML 1.1 double-quoted escape
/// alphabet `yaml_serde` (libyaml) decodes — anything BUILD can load
/// into the graph, the surgical reader must read identically. Unknown
/// escapes, short or non-hex digit runs, and out-of-range / surrogate
/// code points are malformed (`None`) — matching the error
/// `yaml_serde` raises rather than guessing.
fn decode_escape(rest: &str) -> Option<(char, usize)> {
    match rest.as_bytes().first()? {
        b'0' => Some(('\u{0000}', 1)),
        b'a' => Some(('\u{0007}', 1)),
        b'b' => Some(('\u{0008}', 1)),
        b't' => Some(('\t', 1)),
        b'n' => Some(('\n', 1)),
        b'v' => Some(('\u{000B}', 1)),
        b'f' => Some(('\u{000C}', 1)),
        b'r' => Some(('\r', 1)),
        b'e' => Some(('\u{001B}', 1)),
        b' ' => Some((' ', 1)),
        b'"' => Some(('"', 1)),
        b'/' => Some(('/', 1)),
        b'\\' => Some(('\\', 1)),
        b'N' => Some(('\u{0085}', 1)),
        b'_' => Some(('\u{00A0}', 1)),
        b'L' => Some(('\u{2028}', 1)),
        b'P' => Some(('\u{2029}', 1)),
        b'x' => hex_escape(rest.get(1..3)?).map(|c| (c, 3)),
        b'u' => hex_escape(rest.get(1..5)?).map(|c| (c, 5)),
        b'U' => hex_escape(rest.get(1..9)?).map(|c| (c, 9)),
        _ => None,
    }
}

/// Decode a fixed-width hex digit run into a character. `\xNN` covers
/// the full byte range as a code point (Latin-1 semantics, matching
/// `yaml_serde`); `char::from_u32` refuses surrogates and
/// beyond-Unicode values for the wider forms.
fn hex_escape(digits: &str) -> Option<char> {
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    char::from_u32(u32::from_str_radix(digits, 16).ok()?)
}

/// After a closing quote only whitespace and an optional
/// whitespace-separated `# comment` may follow — anything else is not
/// a line `yaml_serde` would accept as a single scalar, so the caller
/// reports an authoring error instead of silently dropping text.
fn is_only_trailing_comment(tail: &str) -> bool {
    if tail.is_empty() {
        return true;
    }
    if !tail.starts_with(|c: char| c.is_ascii_whitespace()) {
        return false;
    }
    let trimmed = tail.trim_start();
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// YAML starts a comment at `#` only when it begins the value or is
/// preceded by whitespace: `in#progress` is one plain scalar,
/// `foo #bar` is `foo` plus a comment.
fn strip_plain_comment(rest: &str) -> &str {
    let bytes = rest.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return rest[..i].trim_end();
        }
    }
    rest.trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_escapes_special() {
        assert_eq!(quote("hello"), "\"hello\"");
        assert_eq!(quote("with \"quote\""), "\"with \\\"quote\\\"\"");
        assert_eq!(quote("back\\slash"), "\"back\\\\slash\"");
        assert_eq!(quote("line\nbreak"), "\"line\\nbreak\"");
    }

    #[test]
    fn quote_round_trips_through_parse() {
        for value in [
            "hello",
            "with \"quote\"",
            "back\\slash",
            "line\nbreak",
            "tab\there",
            "bell\x07ring",
            "café ünïcode 😀",
            "trailing space ",
            "it's quoted \"twice\"",
            "",
        ] {
            let line = render_scalar_line("k", value);
            assert_eq!(
                parse_scalar_value(&line).as_deref(),
                Some(value),
                "round-trip failed for {value:?} (line: {line:?})"
            );
        }
    }

    #[test]
    fn parse_key_rejects_non_scalars() {
        assert_eq!(parse_scalar_key("id: foo"), Some("id"));
        assert_eq!(parse_scalar_key("created: 2026-05-09"), Some("created"));
        assert_eq!(parse_scalar_key("# comment"), None);
        assert_eq!(parse_scalar_key("  nested: value"), None);
        assert_eq!(parse_scalar_key("- list item"), None);
        assert_eq!(parse_scalar_key(""), None);
        assert_eq!(parse_scalar_key("not a key"), None);
    }

    #[test]
    fn parse_value_strips_quotes_and_comments() {
        assert_eq!(parse_scalar_value("id: foo").as_deref(), Some("foo"));
        assert_eq!(parse_scalar_value("id: \"foo\"").as_deref(), Some("foo"));
        assert_eq!(parse_scalar_value("id: 'foo'").as_deref(), Some("foo"));
        assert_eq!(
            parse_scalar_value("id: foo  # trailing").as_deref(),
            Some("foo")
        );
        assert_eq!(
            parse_scalar_value("id: \"foo # not a comment\"").as_deref(),
            Some("foo # not a comment")
        );
        assert_eq!(parse_scalar_value("tags: [a, b]"), None);
        assert_eq!(parse_scalar_value("body: |"), None);
    }

    #[test]
    fn quoted_value_keeps_comment_after_closing_quote_out() {
        assert_eq!(
            parse_scalar_value("id: \"foo\" # it's \"x\"").as_deref(),
            Some("foo")
        );
        assert_eq!(
            parse_scalar_value("status: 'done' # wasn't 'active'").as_deref(),
            Some("done")
        );
    }

    #[test]
    fn double_quoted_escapes_decode() {
        assert_eq!(
            parse_scalar_value("id: \"with \\\"quote\\\"\"").as_deref(),
            Some("with \"quote\"")
        );
        assert_eq!(
            parse_scalar_value("id: \"a\\\\b\"").as_deref(),
            Some("a\\b")
        );
        assert_eq!(
            parse_scalar_value("id: \"a\\nb\\tc\"").as_deref(),
            Some("a\nb\tc")
        );
        assert_eq!(parse_scalar_value("id: \"\\x41\"").as_deref(), Some("A"));
        assert_eq!(parse_scalar_value("id: \"\\xe9\"").as_deref(), Some("é"));
        assert_eq!(parse_scalar_value("id: \"\\u00e9\"").as_deref(), Some("é"));
        assert_eq!(
            parse_scalar_value("id: \"\\U0001F600\"").as_deref(),
            Some("😀")
        );
    }

    #[test]
    fn single_quoted_doubles_decode() {
        assert_eq!(parse_scalar_value("id: 'it''s'").as_deref(), Some("it's"));
        assert_eq!(parse_scalar_value("id: 'a\\nb'").as_deref(), Some("a\\nb"));
    }

    #[test]
    fn malformed_quoting_is_not_a_scalar() {
        assert_eq!(parse_scalar_value("id: \"foo"), None);
        assert_eq!(parse_scalar_value("id: 'foo"), None);
        assert_eq!(parse_scalar_value("id: \"\\q\""), None);
        assert_eq!(parse_scalar_value("id: \"\\x4\""), None);
        assert_eq!(parse_scalar_value("id: \"\\u00\""), None);
        assert_eq!(parse_scalar_value("id: \"\\ud800\""), None);
        assert_eq!(parse_scalar_value("id: \"foo\" bar"), None);
        assert_eq!(parse_scalar_value("id: 'foo' bar"), None);
    }

    #[test]
    fn anchors_aliases_tags_are_not_scalars() {
        // Outside the plain-scalar subset: yaml_serde resolves or rejects
        // each, so the line reader must refuse rather than echo the raw
        // indicator text — an aliased `status: *s` reading back as the
        // literal "*s" would diverge from the build and slip past the
        // lifecycle-terminal / vocabulary gates.
        assert_eq!(parse_scalar_value("status: *s"), None);
        assert_eq!(parse_scalar_value("status: &a superseded"), None);
        assert_eq!(parse_scalar_value("title: !!str x"), None);
        assert_eq!(parse_scalar_value("title: !custom x"), None);
        assert_eq!(parse_scalar_value("k: @reserved"), None);
        assert_eq!(parse_scalar_value("k: `reserved"), None);
        // A mid-value indicator is an ordinary plain scalar — only a
        // *leading* indicator changes the node's meaning.
        assert_eq!(parse_scalar_value("title: A & B").as_deref(), Some("A & B"));
        assert_eq!(parse_scalar_value("title: 2 * 3").as_deref(), Some("2 * 3"));
        // Empty value and value-then-comment forms are unaffected.
        assert_eq!(parse_scalar_value("id:").as_deref(), Some(""));
        assert_eq!(parse_scalar_value("id: # c").as_deref(), Some(""));
    }

    #[test]
    fn block_and_flow_indicator_first_chars_match_yaml_serde() {
        // The directive marker and the block / flow indicators that can
        // never begin a plain scalar — yaml_serde rejects each, so the
        // line reader must refuse rather than echo the raw text.
        assert_eq!(parse_scalar_value("k: %TAG"), None);
        // `,` is a flow separator with no leading-position exception —
        // refused glued or spaced.
        assert_eq!(parse_scalar_value("k: ,ref"), None);
        assert_eq!(parse_scalar_value("k: , leading"), None);
        // `-` `?` `:` are indicators only when followed by a space (or
        // nothing): the indicator forms are refused...
        assert_eq!(parse_scalar_value("k: - x"), None);
        assert_eq!(parse_scalar_value("k: ? maybe"), None);
        assert_eq!(parse_scalar_value("k: : colon"), None);
        assert_eq!(parse_scalar_value("k: -"), None);
        // ...but the same characters glued to a value are ordinary plain
        // scalars yaml_serde reads verbatim, so they must NOT be refused.
        assert_eq!(parse_scalar_value("k: -5").as_deref(), Some("-5"));
        assert_eq!(parse_scalar_value("k: -word").as_deref(), Some("-word"));
        assert_eq!(parse_scalar_value("k: ?query").as_deref(), Some("?query"));
        assert_eq!(parse_scalar_value("k: :ref").as_deref(), Some(":ref"));
        assert_eq!(
            parse_scalar_value("created: 2026-05-09").as_deref(),
            Some("2026-05-09")
        );
    }

    #[test]
    fn plain_comment_requires_preceding_whitespace() {
        assert_eq!(
            parse_scalar_value("status: in#progress").as_deref(),
            Some("in#progress")
        );
        assert_eq!(
            parse_scalar_value("id: foo#bar # real comment").as_deref(),
            Some("foo#bar")
        );
        assert_eq!(
            parse_scalar_value("id: # only comment").as_deref(),
            Some("")
        );
    }

    #[test]
    fn agrees_with_yaml_serde_within_subset() {
        for line in [
            "k: foo",
            "k: \"foo\"",
            "k: 'foo'",
            "k: foo # comment",
            "k: \"foo # not a comment\"",
            "k: \"with \\\"quote\\\"\"",
            "k: 'it''s'",
            "k: \"\\u00e9 caf\\xe9\"",
            "k: in#progress",
            "k: \"foo\" # it's \"x\"",
            "k: a\\nb",
            "k: \"adr\\/001\"",
            "k: \"nul\\0sep\"",
            "k: \"esc\\e[0m\"",
            "k: \"nb\\_space\"",
            "k: \"bell\\a back\\b vt\\v ff\\f\"",
            "k: \"next\\N line\\L para\\P\"",
            "k: \"sp\\ ace\"",
        ] {
            let oracle: std::collections::BTreeMap<String, String> =
                yaml_serde::from_str(line).expect("oracle parses the subset line");
            assert_eq!(
                parse_scalar_value(line).as_deref(),
                Some(oracle["k"].as_str()),
                "diverged from yaml_serde on {line:?}"
            );
        }
    }

    #[test]
    fn undecoded_values_borrow_from_the_line() {
        assert!(matches!(
            parse_scalar_value("id: foo"),
            Some(Cow::Borrowed(_))
        ));
        assert!(matches!(
            parse_scalar_value("id: \"foo\""),
            Some(Cow::Borrowed(_))
        ));
        assert!(matches!(
            parse_scalar_value("id: \"a\\nb\""),
            Some(Cow::Owned(_))
        ));
        assert!(matches!(
            parse_scalar_value("id: 'it''s'"),
            Some(Cow::Owned(_))
        ));
    }
}
