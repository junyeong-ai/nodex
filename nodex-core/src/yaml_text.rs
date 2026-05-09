//! Line-level YAML scalar helpers.
//!
//! These exist for tool actions that write a small number of scalar
//! fields back to a frontmatter file (`scaffold`, `migrate`, `lifecycle`).
//! A full YAML round-trip would re-emit the entire document and
//! destroy the user's key order, comments, and quoting style — so we
//! only ever touch lines that name the keys we are changing.

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

/// Scalar value from a `key: value` line, with surrounding quotes
/// stripped. Returns `None` if the line is not in scalar form (block,
/// flow sequence/map, multi-line literal). Inline `# comment` after an
/// unquoted value is dropped.
pub fn parse_scalar_value(line: &str) -> Option<&str> {
    let colon = line.find(':')?;
    let rest = line[colon + 1..].trim_start();
    if rest.starts_with('|')
        || rest.starts_with('>')
        || rest.starts_with('[')
        || rest.starts_with('{')
    {
        return None;
    }
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.rfind('"')?;
        return Some(&stripped[..end]);
    }
    if let Some(stripped) = rest.strip_prefix('\'') {
        let end = stripped.rfind('\'')?;
        return Some(&stripped[..end]);
    }
    let value = rest.split('#').next().unwrap_or(rest).trim_end();
    Some(value)
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
        assert_eq!(parse_scalar_value("id: foo"), Some("foo"));
        assert_eq!(parse_scalar_value("id: \"foo\""), Some("foo"));
        assert_eq!(parse_scalar_value("id: 'foo'"), Some("foo"));
        assert_eq!(parse_scalar_value("id: foo  # trailing"), Some("foo"));
        assert_eq!(
            parse_scalar_value("id: \"foo # not a comment\""),
            Some("foo # not a comment")
        );
        assert_eq!(parse_scalar_value("tags: [a, b]"), None);
        assert_eq!(parse_scalar_value("body: |"), None);
    }
}
