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
//! collections (`[`, `{`), malformed or unterminated quoting, and a
//! plain value carrying a block-mapping shape (a `:` followed by space
//! or tab, or a trailing `:`, which must be quoted) — is not a scalar
//! this module can reason about and surfaces as `None` (an authoring
//! error to the caller), never as a silently mangled value. Within that
//! subset the reader agrees with `yaml_serde` (the build-time parser)
//! and round-trips [`quote`]'s own output, so a value written by one
//! nodex command is always read back verbatim by the next.
//!
//! The agreement is over the reader's *reachable domain*: the printable,
//! line-break-free character set — exactly the set [`quote`] emits
//! unescaped ([`is_yaml_printable`] minus [`is_yaml_line_break`]) and the
//! only set a value reaching the editor can hold, because a raw control
//! or line-break code point makes the whole document a `yaml_serde` parse
//! failure (the build parser is the gate; the editor only ever sees a
//! line from an already-built, valid frontmatter block). Those code
//! points are therefore out of the reader's domain — the
//! `reader_matches_oracle_over_indicator_alphabet` differential proves
//! bit-exact agreement across this domain, including every YAML indicator
//! and whitespace class.

use std::borrow::Cow;

/// Emit a YAML scalar that is always safe to parse back: a
/// double-quoted string in which every code point outside
/// `yaml_serde`'s reader-acceptance set travels escaped (`\xNN` for
/// `<= U+00FF`, `\uNNNN` above — every non-printable is `<= U+FFFF`),
/// so a value written by a nodex command is always a stream the build
/// can parse. The Unicode line breaks (NEL / LS / PS) are
/// reader-accepted but travel escaped too, for value fidelity. The
/// dedicated `\n` / `\r` / `\t` / `\"` / `\\` arms keep the common
/// escapes readable.
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
            c if !is_yaml_printable(c) || is_yaml_line_break(c) => {
                let v = c as u32;
                if v <= 0xFF {
                    escaped.push_str(&format!("\\x{v:02x}"));
                } else {
                    escaped.push_str(&format!("\\u{v:04x}"));
                }
            }
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}

/// Exactly the code points `yaml_serde`'s reader accepts in a stream.
/// Anything else must travel escaped or the build cannot parse the
/// file the tool just wrote — the predicate mirrors the actual
/// parser's declared acceptance set, not a guess.
fn is_yaml_printable(c: char) -> bool {
    matches!(c,
        '\u{09}' | '\u{0A}' | '\u{0D}' | '\u{20}'..='\u{7E}' | '\u{85}'
        | '\u{A0}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

/// The YAML line-break code points beyond `\n` / `\r` (which have
/// dedicated escape arms): NEL, LS, PS. The reader accepts them raw,
/// but inside a quoted scalar the parser treats them as breaks —
/// folding NEL into a space and discarding blanks that precede LS or
/// PS — so a raw occurrence cannot round-trip verbatim.
fn is_yaml_line_break(c: char) -> bool {
    matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}')
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
    let rest = line[colon + 1..].trim_start_matches(is_yaml_space);
    // Indicators that can never begin a plain scalar: block scalars
    // (`|` / `>`), flow collections — both open (`[` / `{`) and close
    // (`]` / `}`) — node-property / reserved markers (`&` anchor, `*`
    // alias, `!` tag, `@` / backtick reserved), and the `%` directive.
    // `yaml_serde` resolves or rejects each, so echoing the raw line text
    // would diverge — an aliased `status: *s` must not read back as the
    // literal "*s" and slip past a vocabulary or lifecycle-terminal gate.
    // Refuse, so the caller reports an authoring error instead of acting
    // on a misread value. (A leading `#` stays a value-then-comment, and a
    // quoted value is handled by the branches below.)
    let bytes = rest.as_bytes();
    if matches!(
        bytes.first(),
        Some(
            b'|' | b'>'
                | b'['
                | b'{'
                | b']'
                | b'}'
                | b'&'
                | b'*'
                | b'!'
                | b'@'
                | b'`'
                | b'%'
                | b','
        )
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
    // A plain scalar carrying a block-mapping shape is one `yaml_serde`
    // rejects ("mapping values are not allowed") — that text must be
    // quoted to travel as a value. The leading bare-indicator forms
    // (`-` / `?` / `:` then space) are handled above; `has_mapping_shape`
    // catches the *interior* and *trailing* shape. Refuse, so the reader
    // stays in lock-step with the build parser and the editor reports an
    // authoring error rather than reading back a value `yaml_serde` would
    // never produce.
    let value = strip_plain_comment(rest);
    if has_mapping_shape(value) {
        return None;
    }
    Some(Cow::Borrowed(value))
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
    // Trim only ASCII space / tab (the white space `yaml_serde` separates
    // on), never a wider Unicode space the parser keeps — otherwise a
    // post-close-quote NBSP would read as "only a comment" and the reader
    // would accept a line `yaml_serde` rejects.
    let trimmed = tail.trim_start_matches(is_yaml_space);
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// True when a plain scalar carries a block-mapping shape `yaml_serde`
/// rejects ("mapping values are not allowed"): a `:` followed by YAML
/// white space — space **or** tab — or a trailing `:`. Such text must be
/// quoted to travel as a value. A `:` glued to a non-space character
/// (`a:b`, `http://x`, `12:30`) is a legal plain scalar and is not
/// flagged. Byte scanning is sound because `:`, space, and tab are ASCII
/// and never collide with a UTF-8 continuation byte.
fn has_mapping_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.last() == Some(&b':')
        || bytes
            .windows(2)
            .any(|w| w[0] == b':' && matches!(w[1], b' ' | b'\t'))
}

/// Drop a plain scalar's trailing `# comment` and surrounding YAML white
/// space. A comment starts at `#` only when it begins the value or is
/// preceded by white space (`in#progress` is one plain scalar; `foo #bar`
/// is `foo` plus a comment). Edge trimming removes only ASCII space / tab
/// ([`is_yaml_space`]) — exactly the white space `yaml_serde` strips —
/// never a wider Unicode space the parser keeps inside the value.
fn strip_plain_comment(rest: &str) -> &str {
    let bytes = rest.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return rest[..i].trim_end_matches(is_yaml_space);
        }
    }
    rest.trim_end_matches(is_yaml_space)
}

/// The only code points YAML treats as separating white space around a
/// plain scalar on a line: ASCII space and tab. `yaml_serde` keeps every
/// other Unicode space (NBSP `U+00A0`, ideographic space `U+3000`, the
/// `U+2000`–`U+200A` run, …) as part of the scalar value, so trimming
/// with Rust's Unicode-aware `trim_start` / `trim_end` would silently
/// drop a code point the build parser preserves — `status:\u{a0}active`
/// would read back as `"active"` while the graph holds `"\u{a0}active"`,
/// diverging the surgical reader from the build and laundering a value
/// past a vocabulary / lifecycle-terminal gate. Trimming only this set
/// keeps the reader in lock-step with `yaml_serde`.
fn is_yaml_space(c: char) -> bool {
    matches!(c, ' ' | '\t')
}

/// Proptest strategies over the line grammar this module reads and
/// writes, colocated with it so the supported-subset definition and
/// its generator cannot drift. Shared by the `yaml_text` and
/// `parser::editor` property tests.
#[cfg(test)]
pub(crate) mod strategies {
    use proptest::prelude::*;
    use proptest::string::string_regex;

    /// Top-level frontmatter key — exactly the alphabet
    /// [`parse_scalar_key`](super::parse_scalar_key) admits.
    pub(crate) fn key() -> impl Strategy<Value = String> {
        string_regex("[a-z][a-z0-9_-]{0,7}").expect("hardcoded regex compiles")
    }

    /// The full domain [`quote`](super::quote) must round-trip.
    pub(crate) fn any_value() -> impl Strategy<Value = String> {
        any::<String>()
    }

    /// A plain (unquoted) scalar value `yaml_serde` reads verbatim
    /// from a `key: value` line: letter-leading, YAML-printable, no
    /// line breaks (`\n` / `\r` / NEL / LS / PS) or tabs, no comment
    /// start (` #`), no nested-mapping shape (`: ` or a trailing `:`),
    /// no edge whitespace.
    pub(crate) fn plain_value() -> impl Strategy<Value = String> {
        let tail = prop::collection::vec(
            any::<char>().prop_filter("travels raw on a plain-scalar line", |c| {
                super::is_yaml_printable(*c)
                    && !matches!(c, '\n' | '\r' | '\t')
                    && !super::is_yaml_line_break(*c)
            }),
            0..8,
        );
        (
            string_regex("[a-zA-Z]").expect("hardcoded regex compiles"),
            tail,
        )
            .prop_map(|(head, tail)| {
                let mut value = head;
                value.extend(tail);
                value
            })
            .prop_filter(
                "no comment / mapping indicators, no edge whitespace",
                |value| {
                    !value.contains(" #")
                        && !value.contains(": ")
                        && !value.ends_with(':')
                        && value.trim() == value.as_str()
                },
            )
    }

    /// One `key: value` line in a generated style — plain
    /// ([`plain_value`]), single-quoted (printable no-line-break
    /// value, `'` doubled), or double-quoted (the full [`any_value`]
    /// domain via [`quote`](super::quote)) — with 1..=3 spaces after
    /// the colon and an optional trailing `  # comment`. Returns the
    /// line and the decoded value it carries.
    pub(crate) fn scalar_line() -> impl Strategy<Value = (String, String)> {
        let single_quotable = prop::collection::vec(
            any::<char>().prop_filter("travels raw inside single quotes", |c| {
                super::is_yaml_printable(*c)
                    && !matches!(c, '\n' | '\r')
                    && !super::is_yaml_line_break(*c)
            }),
            0..8,
        )
        .prop_map(String::from_iter);
        let styled = prop_oneof![
            plain_value().prop_map(|v| (v.clone(), v)),
            single_quotable.prop_map(|v| (format!("'{}'", v.replace('\'', "''")), v)),
            any_value().prop_map(|v| (super::quote(&v), v)),
        ];
        let comment = prop::option::of(string_regex("[a-z0-9 ]{0,8}").expect("regex compiles"));
        (key(), styled, 1..=3usize, comment).prop_map(|(key, (rendered, decoded), pad, comment)| {
            let pad = " ".repeat(pad);
            let line = match comment {
                Some(c) => format!("{key}:{pad}{rendered}  # {c}"),
                None => format!("{key}:{pad}{rendered}"),
            };
            (line, decoded)
        })
    }
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
    fn reader_matches_oracle_over_indicator_alphabet() {
        // Exhaustive differential agreement on BOTH directions over the
        // tricky character space: for every `k: <value>` line built from a
        // letter plus the YAML indicator chars (`:`, space, tab, `#`, `-`,
        // `?`, `'`), the surgical reader must ACCEPT exactly when
        // `yaml_serde` accepts (with the same value) and REJECT (`None`)
        // exactly when it rejects. This guards the plain-scalar /
        // mapping-indicator boundary the round-trip generator (valid values
        // only) never reaches — the class that produced two prior
        // divergences (colon-space, colon-tab).
        fn walk(prefix: &str, depth: usize, alpha: &[char], visit: &mut impl FnMut(&str)) {
            visit(prefix);
            if depth == 0 {
                return;
            }
            let mut s = String::from(prefix);
            for &c in alpha {
                s.push(c);
                walk(&s, depth - 1, alpha, visit);
                s.pop();
            }
        }
        // Spans the reader's full reachable domain: a letter, a multibyte
        // letter (`é`), a non-ASCII space (`U+00A0`), and every printable
        // YAML indicator that can appear in a value — the mapping colon,
        // ASCII space/tab, `#`, the block-indicator chars `-` / `?`, the
        // flow indicators `[` `]` `{` `}` `,`, and a quote. (Node-property
        // markers `&` / `!` / `*` and block scalars `|` / `>` are the
        // module's documented intentional refusals and excluded here; raw
        // control / line-break code points are out of domain — see the
        // module docs.) An ASCII-only or indicator-incomplete alphabet let
        // four divergences slip past prior rounds; this covers them.
        let alpha = [
            'a', 'é', ':', ' ', '\t', '\u{a0}', '#', '-', '?', '[', ']', '{', '}', ',', '\'',
        ];
        let mut checked = 0u64;
        walk("a", 4, &alpha, &mut |value| {
            let line = format!("k: {value}");
            let oracle: Result<std::collections::BTreeMap<String, String>, _> =
                yaml_serde::from_str(&line);
            let reader = parse_scalar_value(&line);
            let agree = match &oracle {
                Ok(map) => map.get("k").map(String::as_str) == reader.as_deref(),
                Err(_) => reader.is_none(),
            };
            assert!(
                agree,
                "reader/oracle diverge on {line:?}: oracle={oracle:?} reader={reader:?}"
            );
            checked += 1;
        });
        assert!(
            checked > 1000,
            "fuzz should exercise many cases, got {checked}"
        );
    }

    #[test]
    fn non_ascii_unicode_whitespace_is_kept_like_yaml_serde() {
        // `yaml_serde` strips only ASCII space / tab around a plain
        // scalar; every other Unicode space (NBSP, ideographic space, the
        // U+2000–U+200A run, …) is part of the value. Rust's Unicode-aware
        // `trim_start` / `trim_end` would silently drop a leading or
        // trailing one, reading back a value the build never produced and
        // laundering it past a vocabulary / lifecycle-terminal gate. The
        // reader must keep exactly what `yaml_serde` keeps.
        for line in [
            "k: \u{a0}active", // leading NBSP
            "k: active\u{a0}", // trailing NBSP
            "k: a\u{a0}b",     // interior NBSP (always kept)
            "k: \u{3000}wide", // leading ideographic space
            "k: \u{2000}x",    // leading en quad
            "k: \u{200a}hair", // leading hair space
            "k: v\u{a0} # c",  // NBSP belongs to the value, then a comment
        ] {
            let oracle: std::collections::BTreeMap<String, String> =
                yaml_serde::from_str(line).expect("oracle parses the unicode-space value");
            assert_eq!(
                parse_scalar_value(line).as_deref(),
                Some(oracle["k"].as_str()),
                "reader dropped a Unicode space yaml_serde keeps on {line:?}"
            );
        }
    }

    #[test]
    fn plain_scalar_with_mapping_shape_refused_like_yaml_serde() {
        // A plain value carrying `": "` or a trailing `:` is invalid YAML
        // — `yaml_serde` rejects it ("mapping values are not allowed") — so
        // the surgical reader must refuse it too (→ `NonScalar` at the
        // editor), never read it back as a literal a write seam would
        // rewrite. This is the agreement the module docstring promises.
        for line in [
            "k: foo: bar",
            "k: a: b: c",
            "k: trailing colon:",
            "k: foo:",
            "k: a:\tb", // colon-TAB is a mapping indicator too, not only colon-space
        ] {
            assert!(
                yaml_serde::from_str::<std::collections::BTreeMap<String, String>>(line).is_err(),
                "oracle should reject the mapping-shaped line {line:?}"
            );
            assert_eq!(
                parse_scalar_value(line),
                None,
                "reader must refuse the mapping-shaped plain value {line:?}"
            );
        }
        // A colon NOT followed by a space (and not trailing) is a valid
        // plain scalar `yaml_serde` reads verbatim — the reader keeps it.
        for line in ["k: a:b", "k: http://example.com"] {
            let oracle: std::collections::BTreeMap<String, String> =
                yaml_serde::from_str(line).expect("oracle parses the colon-bearing value");
            assert_eq!(
                parse_scalar_value(line).as_deref(),
                Some(oracle["k"].as_str()),
                "reader must keep the valid colon-bearing value {line:?}"
            );
        }
    }

    #[test]
    fn quote_escapes_yaml_unprintables() {
        // Code points the yaml_serde reader rejects raw travel
        // escaped, and so do the Unicode line breaks the reader
        // accepts but the parser folds (NEL becomes a space; a blank
        // before LS / PS is discarded).
        assert_eq!(quote("a\u{7f}b"), "\"a\\x7fb\"");
        assert_eq!(quote("\u{90}"), "\"\\x90\"");
        assert_eq!(quote("\u{fffe}"), "\"\\ufffe\"");
        assert_eq!(quote("\u{ffff}"), "\"\\uffff\"");
        assert_eq!(quote("a\u{85}b"), "\"a\\x85b\"");
        assert_eq!(quote(" \u{2028}"), "\" \\u2028\"");
        assert_eq!(quote("\u{2029}"), "\"\\u2029\"");
        for value in [
            "a\u{7f}b",
            "\u{90}",
            "\u{fffe}",
            "\u{ffff}",
            "a\u{85}b",
            " \u{2028}",
            "\u{2029}",
        ] {
            let line = render_scalar_line("k", value);
            let oracle: std::collections::BTreeMap<String, String> =
                yaml_serde::from_str(&line).expect("escaped line parses");
            assert_eq!(oracle["k"], value, "yaml_serde round-trip for {value:?}");
        }
    }

    mod properties {
        use proptest::prelude::*;

        use super::super::*;

        proptest! {
            /// Three-way agreement on the writer's full input domain:
            /// a line rendered by [`render_scalar_line`] must parse
            /// through `yaml_serde` (the build parser) back to the
            /// original value, and [`parse_scalar_value`] (the
            /// surgical reader) must read the same value.
            #[test]
            fn quote_round_trips_any_string_through_yaml_serde(value in strategies::any_value()) {
                let line = render_scalar_line("k", &value);
                let oracle: std::collections::BTreeMap<String, String> =
                    match yaml_serde::from_str(&line) {
                        Ok(map) => map,
                        Err(e) => {
                            return Err(TestCaseError::fail(format!(
                                "yaml_serde refused a tool-written line {line:?}: {e}"
                            )));
                        }
                    };
                prop_assert_eq!(&oracle["k"], &value, "yaml_serde diverged on {:?}", line);
                let read = parse_scalar_value(&line);
                prop_assert_eq!(
                    read.as_deref(),
                    Some(value.as_str()),
                    "parse_scalar_value diverged on {:?}",
                    line
                );
            }

            /// Every line in the generated supported subset reads the
            /// same through `yaml_serde` and [`parse_scalar_value`],
            /// and both match the generator's own decoded value. Plain
            /// style may resolve to a non-string node (`true`, `null`,
            /// numbers) — those lines are outside the string-typed
            /// comparison.
            #[test]
            fn generated_subset_lines_agree_with_yaml_serde(
                (line, decoded) in strategies::scalar_line()
            ) {
                let oracle: std::collections::BTreeMap<String, yaml_serde::Value> =
                    yaml_serde::from_str(&line).expect("generated subset line parses");
                let key = parse_scalar_key(&line).expect("generated line carries a key");
                prop_assume!(matches!(&oracle[key], yaml_serde::Value::String(_)));
                let yaml_serde::Value::String(oracle_value) = &oracle[key] else {
                    unreachable!("prop_assume admits string-typed lines only");
                };
                prop_assert_eq!(oracle_value, &decoded, "yaml_serde diverged on {:?}", line);
                let read = parse_scalar_value(&line);
                prop_assert_eq!(
                    read.as_deref(),
                    Some(decoded.as_str()),
                    "parse_scalar_value diverged on {:?}",
                    line
                );
            }
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
