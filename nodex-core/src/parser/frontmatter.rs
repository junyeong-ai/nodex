use chrono::NaiveDate;
use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, ParseError, Result};
use crate::model::{FieldParseIssue, Kind, Node, Status};

/// Typed holder for the built-in frontmatter roster, filled by the
/// lenient per-field pass: every field is coerced individually, a
/// failed coercion records a [`FieldParseIssue`] and the field stays
/// `None` — only whole-document failures (unparseable YAML, a
/// non-mapping block, an unclosed fence) drop the document.
#[derive(Debug, Default)]
struct RawFrontmatter {
    id: Option<String>,
    title: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    created: Option<NaiveDate>,
    updated: Option<NaiveDate>,
    reviewed: Option<NaiveDate>,
    owner: Option<String>,
    supersedes: Option<Vec<String>>,
    superseded_by: Option<String>,
    implements: Option<Vec<String>>,
    related: Option<Vec<String>>,
    tags: Option<Vec<String>>,
    covers: Option<Vec<String>>,
    orphan_ok: Option<bool>,
    /// Project-specific fields, in authored order.
    extra: BTreeMap<String, serde_json::Value>,
    /// Built-in fields whose value failed coercion, sorted by field.
    issues: Vec<FieldParseIssue>,
}

/// Canonicalise file content for parsing: strip leading UTF-8 BOM
/// and collapse `\r\n` / lone `\r` to `\n`. Borrowed when no change is
/// needed so the common case (LF-only, no BOM) costs nothing.
///
/// Every parser entry point routes through this so the rest of the
/// pipeline (frontmatter delimiter detection, body fingerprinting,
/// regex matching, line iteration) sees one canonical form.
pub fn canonicalize(content: &str) -> std::borrow::Cow<'_, str> {
    let stripped = content.strip_prefix('\u{FEFF}').unwrap_or(content);
    if stripped.contains('\r') {
        std::borrow::Cow::Owned(stripped.replace("\r\n", "\n").replace('\r', "\n"))
    } else if stripped.len() != content.len() {
        std::borrow::Cow::Borrowed(stripped)
    } else {
        std::borrow::Cow::Borrowed(content)
    }
}

/// True when `line` is a frontmatter fence: exactly `---` after
/// stripping trailing spaces and tabs (`^---[ \t]*$`). `----` and
/// `---suffix` are never fences.
fn is_fence_line(line: &str) -> bool {
    line.strip_prefix("---")
        .is_some_and(|rest| rest.chars().all(|c| c == ' ' || c == '\t'))
}

/// Split a canonicalised document into frontmatter YAML and body text.
///
/// Frontmatter opens when the first line is a whole-line fence
/// (`---`, trailing spaces/tabs tolerated) terminated by a newline,
/// and closes at the FIRST subsequent whole fence line — newline- or
/// EOF-terminated. A line like `----` or `---suffix` is never a
/// fence; the scan continues past it. Returns `(None, full_content)`
/// for a fenceless document and
/// [`ParseError::FrontmatterDelimiter`] for a fence that opens but
/// never closes — an opened fence is a declaration of intent, never
/// silently re-read as body prose.
///
/// Callers must pass content already run through [`canonicalize`] —
/// line-ending and BOM concerns belong to that single seam.
pub fn split_frontmatter(
    content: &str,
) -> std::result::Result<(Option<&'_ str>, &'_ str), ParseError> {
    let Some((first_line, rest)) = content.split_once('\n') else {
        return Ok((None, content));
    };
    if !is_fence_line(first_line) {
        return Ok((None, content));
    }
    let mut offset = 0usize;
    loop {
        let line_end = rest[offset..].find('\n').map(|i| offset + i);
        let line = match line_end {
            Some(end) => &rest[offset..end],
            None => &rest[offset..],
        };
        if is_fence_line(line) {
            // The YAML block excludes the newline that terminates its
            // last line (when one exists — a close on the very next
            // line yields an empty block).
            let yaml = if offset == 0 { "" } else { &rest[..offset - 1] };
            let body = match line_end {
                Some(end) => &rest[end + 1..],
                None => "",
            };
            return Ok((Some(yaml), body));
        }
        match line_end {
            Some(end) => offset = end + 1,
            None => return Err(ParseError::FrontmatterDelimiter),
        }
    }
}

/// Parse frontmatter YAML into a partial Node (id/kind may need inference).
/// Returns `(Node, body_text)`. The returned body is canonicalised
/// (`\r\n` and lone `\r` → `\n`, leading BOM stripped) so body
/// fingerprints, regex matches, and line iteration agree across
/// Windows-checked-out / mixed-line-ending sources — raw bytes
/// would otherwise yield phantom hash diffs and brittle pattern
/// matches.
///
/// Built-in fields parse leniently, field by field: a value that
/// fails its type lands in `Node::parse_issues` and the field reads
/// as absent everywhere downstream (a wrong-typed `status` infers
/// `statuses.initial`, a wrong-typed date is `None` — the existing
/// absence semantics, nothing fabricated). Failed values never reach
/// `attrs`, so `field_type` / `unknown_field` cannot double-report.
/// Only unparseable YAML, a non-mapping block, or an unclosed fence
/// fail the whole document.
pub fn parse_frontmatter(path: &Path, content: &str) -> Result<(Node, String)> {
    let canonical = canonicalize(content);
    let (yaml_opt, body) = split_frontmatter(&canonical).map_err(|source| Error::Parse {
        path: path.to_path_buf(),
        source,
    })?;

    let raw = match yaml_opt {
        Some(yaml) => parse_fields_leniently(path, yaml)?,
        None => RawFrontmatter::default(),
    };

    // Which inferrable built-ins the document did NOT author with a
    // usable value. "Authored" means present AND non-empty — an empty
    // string takes the same inference the absent case does (`id` / `kind`
    // / `status` fall back via `unwrap_or_default()` below; a blank title
    // falls back to the H1/stem, just like an absent one), so an empty
    // value is "not authored" here exactly as it is to the resolver. A
    // malformed value is different: it already carries a `FieldParseIssue`
    // (which `field_parse` reds), so it is excluded — this set is
    // "genuinely not authored", never "we could not read what you wrote".
    // Captured before the fallbacks below consume the raw options. Sorted
    // for deterministic output.
    let authored = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.is_empty());
    let mut inferred_fields: Vec<String> = [
        ("id", authored(&raw.id)),
        ("title", authored(&raw.title)),
        ("kind", authored(&raw.kind)),
        ("status", authored(&raw.status)),
    ]
    .into_iter()
    .filter(|&(field, is_authored)| !is_authored && !raw.issues.iter().any(|i| i.field == field))
    .map(|(field, _)| field.to_string())
    .collect();
    inferred_fields.sort();

    // An empty `title:` is treated as absent — it falls back to the H1 /
    // stem, the same inference the absent case takes (and exactly what
    // `inferred_fields` above records by counting an empty value as not
    // authored). Without the `filter`, `Some("")` would short-circuit the
    // fallback and keep a blank title the resolver never intends.
    let title = raw
        .title
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| extract_h1(body, path));

    // Compute body fingerprints once, at the only place that owns the
    // body string. `body_hash` powers `body_immutable.frozen`;
    // `body_lines_hash` powers `body_immutable.append_only` (prefix
    // equality of the per-line vector). Stored on the node so rules
    // stay pure functions of `(graph, config)`.
    let body_hash = crate::hash::sha256_hex(body);
    let body_lines_hash: Vec<String> = body.lines().map(crate::hash::sha256_hex).collect();

    let node = Node {
        id: raw.id.unwrap_or_default(), // empty = needs inference
        path: path.to_path_buf(),
        title,
        kind: Kind::new(raw.kind.unwrap_or_default()), // empty = needs inference
        status: Status::new(raw.status.unwrap_or_default()), // empty = needs inference
        created: raw.created,
        updated: raw.updated,
        reviewed: raw.reviewed,
        owner: raw.owner,
        supersedes: raw.supersedes.unwrap_or_default(),
        superseded_by: raw.superseded_by,
        implements: raw.implements.unwrap_or_default(),
        related: raw.related.unwrap_or_default(),
        tags: raw.tags.unwrap_or_default(),
        covers: raw.covers.unwrap_or_default(),
        orphan_ok: raw.orphan_ok.unwrap_or(false),
        attrs: raw.extra,
        body_hash,
        body_lines_hash,
        content_hash: String::new(),
        parse_issues: raw.issues,
        inferred_fields,
    };

    Ok((node, body.to_string()))
}

/// The status a document declares, read the way the build reads it: `Err`
/// where the build produces no node at all, `Ok(None)` where the document
/// leaves the field to inference, and `Ok(Some)` for a value it authored.
///
/// The scan asks this to decide whether a `scope.conditional_exclude` parent
/// is terminal, which it must do before any node exists — so it goes through
/// the pass the build goes through rather than reading the frontmatter again.
/// What that pass rejects is not only bad YAML: a non-mapping block and a
/// mapping under a non-string key each fail the whole document too, and every
/// one of them is a path the graph carries no node at. A second reading is
/// free to admit a shape this one rejects, and membership is where that costs
/// the most — the sub-artifacts of a parent the project does not hold would
/// leave the project on its authority, with no node for any rule to report.
pub(crate) fn declared_status(path: &Path, content: &str) -> Result<Option<String>> {
    let canonical = canonicalize(content);
    let (yaml_opt, _) = split_frontmatter(&canonical).map_err(|source| Error::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    let Some(yaml) = yaml_opt else {
        return Ok(None);
    };
    // An empty value is not a declaration: `inferred_fields` records it as
    // inferred and the fallbacks fill it, so it has to reach the fallback here
    // as well.
    Ok(parse_fields_leniently(path, yaml)?
        .status
        .filter(|status| !status.is_empty()))
}

/// The lenient per-field pass: parse the YAML once into a mapping
/// (undeserializable YAML and non-mapping shapes remain whole-document
/// failures), then coerce each built-in field individually, recording
/// a [`FieldParseIssue`] for every value that fails its type. Leftover
/// mapping entries convert to `attrs`; a non-string key fails the
/// whole document — `attrs` is a string-keyed map by contract.
fn parse_fields_leniently(path: &Path, yaml: &str) -> Result<RawFrontmatter> {
    let parse_err = |source: ParseError| Error::Parse {
        path: path.to_path_buf(),
        source,
    };
    let value: yaml_serde::Value =
        yaml_serde::from_str(yaml).map_err(|e| parse_err(ParseError::Yaml(e)))?;
    let mut mapping = match value {
        // An empty (or comments-only) block is a present-but-empty
        // frontmatter declaration — every field is simply absent.
        yaml_serde::Value::Null => return Ok(RawFrontmatter::default()),
        yaml_serde::Value::Mapping(m) => m,
        _ => return Err(parse_err(ParseError::FrontmatterShape)),
    };

    let mut raw = RawFrontmatter::default();
    let mut issues: Vec<FieldParseIssue> = Vec::new();

    raw.id = coerce_string("id", mapping.remove("id"), &mut issues);
    raw.title = coerce_string("title", mapping.remove("title"), &mut issues);
    raw.kind = coerce_string("kind", mapping.remove("kind"), &mut issues);
    raw.status = coerce_string("status", mapping.remove("status"), &mut issues);
    raw.created = coerce_date("created", mapping.remove("created"), &mut issues);
    raw.updated = coerce_date("updated", mapping.remove("updated"), &mut issues);
    raw.reviewed = coerce_date("reviewed", mapping.remove("reviewed"), &mut issues);
    raw.owner = coerce_string("owner", mapping.remove("owner"), &mut issues);
    raw.supersedes = coerce_string_list("supersedes", mapping.remove("supersedes"), &mut issues);
    raw.superseded_by = coerce_string(
        "superseded_by",
        mapping.remove("superseded_by"),
        &mut issues,
    );
    raw.implements = coerce_string_list("implements", mapping.remove("implements"), &mut issues);
    raw.related = coerce_string_list("related", mapping.remove("related"), &mut issues);
    raw.tags = coerce_string_list("tags", mapping.remove("tags"), &mut issues);
    raw.covers = coerce_string_list("covers", mapping.remove("covers"), &mut issues);
    raw.orphan_ok = coerce_bool("orphan_ok", mapping.remove("orphan_ok"), &mut issues);

    for (key, value) in mapping {
        let Some(key) = key.as_str() else {
            return Err(parse_err(ParseError::Json(serde::de::Error::custom(
                format!(
                    "frontmatter key must be a string, got {}",
                    describe_yaml_value(&key)
                ),
            ))));
        };
        let json = serde_json::to_value(&value).map_err(|e| parse_err(ParseError::Json(e)))?;
        raw.extra.insert(key.to_string(), json);
    }

    issues.sort_by(|a, b| a.field.cmp(&b.field));
    raw.issues = issues;
    Ok(raw)
}

/// The YAML value's type name, plus the rendered scalar (truncated)
/// for scalar values — the `found` half of a [`FieldParseIssue`].
/// Type vocabulary mirrors the `field_type` rule's value descriptions.
fn describe_yaml_value(value: &yaml_serde::Value) -> String {
    use yaml_serde::Value;
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => format!("bool {b}"),
        Value::Number(n) if n.is_i64() || n.is_u64() => format!("integer {n}"),
        Value::Number(n) => format!("float {n}"),
        Value::String(s) => format!("string {:?}", truncate_scalar(s)),
        Value::Sequence(_) => "array".to_string(),
        Value::Mapping(_) => "object".to_string(),
        Value::Tagged(_) => "tagged value".to_string(),
    }
}

/// Cap a rendered scalar so a pathological value cannot balloon a
/// violation message; the prefix is enough to identify the field value.
fn truncate_scalar(s: &str) -> String {
    const MAX: usize = 64;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let prefix: String = s.chars().take(MAX).collect();
    format!("{prefix}…")
}

fn record_issue(
    issues: &mut Vec<FieldParseIssue>,
    field: &str,
    expected: &str,
    value: &yaml_serde::Value,
) {
    issues.push(FieldParseIssue {
        field: field.to_string(),
        expected: expected.to_string(),
        found: describe_yaml_value(value),
    });
}

fn coerce_string(
    field: &str,
    value: Option<yaml_serde::Value>,
    issues: &mut Vec<FieldParseIssue>,
) -> Option<String> {
    match value? {
        yaml_serde::Value::Null => None,
        yaml_serde::Value::String(s) => Some(s),
        other => {
            record_issue(issues, field, "string", &other);
            None
        }
    }
}

fn coerce_date(
    field: &str,
    value: Option<yaml_serde::Value>,
    issues: &mut Vec<FieldParseIssue>,
) -> Option<NaiveDate> {
    const EXPECTED: &str = "date (YYYY-MM-DD)";
    match value? {
        yaml_serde::Value::Null => None,
        yaml_serde::Value::String(s) => match NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
            Ok(date) => Some(date),
            Err(_) => {
                record_issue(
                    issues,
                    field,
                    EXPECTED,
                    &yaml_serde::Value::String(s.clone()),
                );
                None
            }
        },
        other => {
            record_issue(issues, field, EXPECTED, &other);
            None
        }
    }
}

fn coerce_bool(
    field: &str,
    value: Option<yaml_serde::Value>,
    issues: &mut Vec<FieldParseIssue>,
) -> Option<bool> {
    match value? {
        yaml_serde::Value::Null => None,
        yaml_serde::Value::Bool(b) => Some(b),
        other => {
            record_issue(issues, field, "bool", &other);
            None
        }
    }
}

/// Accepts both `"single"` and `["a", "b"]` — the two authoring styles
/// every list-valued built-in supports. A sequence with any non-string
/// element fails the whole field (one issue), never a partial list.
fn coerce_string_list(
    field: &str,
    value: Option<yaml_serde::Value>,
    issues: &mut Vec<FieldParseIssue>,
) -> Option<Vec<String>> {
    const EXPECTED: &str = "string or list of strings";
    match value? {
        yaml_serde::Value::Null => None,
        yaml_serde::Value::String(s) => Some(vec![s]),
        yaml_serde::Value::Sequence(seq) => {
            if let Some(bad) = seq
                .iter()
                .find(|v| !matches!(v, yaml_serde::Value::String(_)))
            {
                issues.push(FieldParseIssue {
                    field: field.to_string(),
                    expected: EXPECTED.to_string(),
                    found: format!("array containing {}", describe_yaml_value(bad)),
                });
                return None;
            }
            Some(
                seq.into_iter()
                    .map(|v| match v {
                        yaml_serde::Value::String(s) => s,
                        _ => unreachable!("non-string elements rejected above"),
                    })
                    .collect(),
            )
        }
        other => {
            record_issue(issues, field, EXPECTED, &other);
            None
        }
    }
}

/// Extract the first H1 heading via pulldown-cmark, concatenating its
/// text + inline-code children. Falls back to the filename stem when
/// no H1 exists. Re-exported by `parser` module for callers (migrate)
/// that need title inference without a full document parse.
pub fn extract_h1(body: &str, path: &Path) -> String {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

    let mut depth: u32 = 0;
    let mut buf = String::new();
    for event in Parser::new(body) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) => {
                depth += 1;
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) if depth > 0 => {
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
                depth = 0;
                buf.clear();
            }
            Event::Text(t) | Event::Code(t) if depth > 0 => buf.push_str(&t),
            _ => {}
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_basic_frontmatter() {
        let content = "---\ntitle: Hello\n---\nBody text";
        let (yaml, body) = split_frontmatter(content).unwrap();
        assert_eq!(yaml, Some("title: Hello"));
        assert_eq!(body, "Body text");
    }

    #[test]
    fn split_no_frontmatter() {
        let content = "Just body text";
        let (yaml, body) = split_frontmatter(content).unwrap();
        assert!(yaml.is_none());
        assert_eq!(body, "Just body text");
    }

    #[test]
    fn split_skips_non_fence_dash_lines_to_the_real_close() {
        // `----` and `---suffix` are not fences — the scan continues
        // past them to the first whole-line `---`, so dash runs inside
        // the YAML never close the block early.
        let content = "---\ntitle: T\nnote: ----\n---\nBody";
        let (yaml, body) = split_frontmatter(content).unwrap();
        assert_eq!(yaml, Some("title: T\nnote: ----"));
        assert_eq!(body, "Body");

        let suffixed = "---\ntitle: T\n---suffix\n---\nBody";
        let (yaml, body) = split_frontmatter(suffixed).unwrap();
        assert_eq!(yaml, Some("title: T\n---suffix"));
        assert_eq!(body, "Body");
    }

    #[test]
    fn split_close_fence_tolerates_trailing_whitespace() {
        let spaces = "---\ntitle: T\n---  \nBody";
        let (yaml, body) = split_frontmatter(spaces).unwrap();
        assert_eq!(yaml, Some("title: T"));
        assert_eq!(body, "Body");

        let tab = "---\ntitle: T\n---\t\nBody";
        let (yaml, body) = split_frontmatter(tab).unwrap();
        assert_eq!(yaml, Some("title: T"));
        assert_eq!(body, "Body");
    }

    #[test]
    fn split_close_fence_at_eof_closes() {
        let content = "---\ntitle: T\n---";
        let (yaml, body) = split_frontmatter(content).unwrap();
        assert_eq!(yaml, Some("title: T"));
        assert_eq!(body, "");
    }

    #[test]
    fn split_close_fence_at_eof_with_trailing_whitespace_closes() {
        // The close fence is the first whole line `^---[ \t]*$`, newline-
        // OR EOF-terminated — trailing whitespace on an EOF-terminated
        // close still closes.
        let content = "---\ntitle: T\n---  ";
        let (yaml, body) = split_frontmatter(content).unwrap();
        assert_eq!(yaml, Some("title: T"));
        assert_eq!(body, "");
    }

    #[test]
    fn split_bare_three_dashes_is_a_fenceless_document() {
        // An open fence is a declaration only when its line is
        // newline-terminated. A document that is exactly `---` has no
        // such line: it is a bare document whose body is the dashes.
        let (yaml, body) = split_frontmatter("---").unwrap();
        assert_eq!(yaml, None);
        assert_eq!(body, "---");
    }

    #[test]
    fn split_open_fence_tolerates_trailing_whitespace() {
        let content = "---  \ntitle: T\n---\nBody";
        let (yaml, body) = split_frontmatter(content).unwrap();
        assert_eq!(yaml, Some("title: T"));
        assert_eq!(body, "Body");
    }

    #[test]
    fn split_unclosed_fence_is_a_typed_parse_failure() {
        // An opened fence that never closes is a reportable violation,
        // never a silent "whole file is body" reinterpretation.
        let err = split_frontmatter("---\ntitle: T\nno close").unwrap_err();
        assert!(matches!(err, ParseError::FrontmatterDelimiter));

        let err = split_frontmatter("---\n").unwrap_err();
        assert!(matches!(err, ParseError::FrontmatterDelimiter));
    }

    #[test]
    fn non_mapping_frontmatter_fails_the_document() {
        let err = parse_frontmatter(Path::new("doc.md"), "---\n- a\n- b\n---\nBody").unwrap_err();
        assert!(matches!(
            err,
            Error::Parse {
                source: ParseError::FrontmatterShape,
                ..
            }
        ));
    }

    #[test]
    fn unparseable_yaml_fails_the_document() {
        let err =
            parse_frontmatter(Path::new("doc.md"), "---\nid: [unclosed\n---\nBody").unwrap_err();
        assert!(matches!(
            err,
            Error::Parse {
                source: ParseError::Yaml(_),
                ..
            }
        ));
    }

    #[test]
    fn parse_full_frontmatter() {
        let content = "---\nid: test-doc\ntitle: Test\nkind: guide\nstatus: active\ncreated: 2026-01-01\ntags:\n  - foo\n  - bar\n---\n# Heading\n\nBody";
        let path = Path::new("docs/test.md");
        let (node, body) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.id, "test-doc");
        assert_eq!(node.title, "Test");
        assert_eq!(node.kind.as_str(), "guide");
        assert_eq!(node.tags, vec!["foo", "bar"]);
        assert!(node.parse_issues.is_empty());
        assert!(body.contains("Body"));
    }

    #[test]
    fn parse_missing_fields_leaves_blanks_for_inference() {
        let content = "---\ntitle: Minimal\n---\nBody";
        let path = Path::new("readme.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.id, "");
        assert_eq!(node.kind.as_str(), "");
        assert_eq!(node.status.as_str(), "");
    }

    // ─── lenient field parsing ─────────────────────────────────────────
    //
    // A wrong-typed built-in never drops the document: the node stays,
    // the field reads as absent (existing absence semantics — nothing
    // fabricated), and the failure is recorded as a `FieldParseIssue`
    // that `field_parse` turns into an Error-severity check violation.

    #[test]
    fn bad_date_records_issue_and_field_reads_absent() {
        let content = "---\nid: x\ncreated: yesterday\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert_eq!(node.id, "x");
        assert!(node.created.is_none());
        assert_eq!(
            node.parse_issues,
            vec![FieldParseIssue {
                field: "created".into(),
                expected: "date (YYYY-MM-DD)".into(),
                found: "string \"yesterday\"".into(),
            }]
        );
    }

    #[test]
    fn bad_orphan_ok_records_issue_and_defaults_false() {
        let content = "---\nid: x\norphan_ok: maybe\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert!(!node.orphan_ok);
        assert_eq!(node.parse_issues.len(), 1);
        assert_eq!(node.parse_issues[0].field, "orphan_ok");
        assert_eq!(node.parse_issues[0].expected, "bool");
        assert_eq!(node.parse_issues[0].found, "string \"maybe\"");
    }

    /// A scalar behind a custom tag is not a string, and the coercion says so
    /// by matching the value's own shape. `yaml_serde`'s accessors resolve a
    /// tag before they answer, so an implementation written with one reads
    /// `!custom archived` as the status `archived` — a value the document
    /// never wrote, admitted with no `field_parse` to red it. Asserted here
    /// rather than left to the scan/graph differential, which holds the two
    /// readers to each other and would follow them both.
    #[test]
    fn a_tagged_scalar_is_not_a_string_and_reads_as_absent() {
        let content = "---\nid: x\ntitle: T\nstatus: !custom archived\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert_eq!(
            node.status.as_str(),
            "",
            "the tagged value must not become the status"
        );
        assert_eq!(node.parse_issues.len(), 1);
        assert_eq!(node.parse_issues[0].field, "status");
        assert_eq!(node.parse_issues[0].expected, "string");
        assert_eq!(node.parse_issues[0].found, "tagged value");
    }

    #[test]
    fn non_string_id_records_issue_and_leaves_id_for_inference() {
        let content = "---\nid: 123\ntitle: T\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert_eq!(node.id, "", "failed id must read absent (→ id_rules)");
        assert_eq!(node.parse_issues.len(), 1);
        assert_eq!(node.parse_issues[0].field, "id");
        assert_eq!(node.parse_issues[0].expected, "string");
        assert_eq!(node.parse_issues[0].found, "integer 123");
    }

    #[test]
    fn inferred_fields_records_unauthored_builtins() {
        // No frontmatter at all → every inferrable built-in fell back.
        let (node, _) = parse_frontmatter(Path::new("doc.md"), "# Heading\nBody").unwrap();
        assert_eq!(node.inferred_fields, vec!["id", "kind", "status", "title"]);
    }

    #[test]
    fn inferred_fields_excludes_authored_builtins() {
        // `title` and `status` are authored → only `id` / `kind` inferred.
        let content = "---\ntitle: T\nstatus: active\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert_eq!(node.inferred_fields, vec!["id", "kind"]);
    }

    #[test]
    fn inferred_fields_counts_an_empty_authored_builtin_as_inferred() {
        // `status: ""` (or `kind: ""`) is authored-but-empty; the resolver
        // infers an empty built-in all the same (`unwrap_or_default`), so
        // it must count as NOT authored — else `require_explicit` would
        // pass a document whose status silently fell back to the initial.
        let content = "---\nid: real\nstatus: \"\"\nkind: \"\"\n---\n# H\n";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert!(node.inferred_fields.contains(&"status".to_string()));
        assert!(node.inferred_fields.contains(&"kind".to_string()));
        // `id: real` is a genuine authored value.
        assert!(!node.inferred_fields.contains(&"id".to_string()));
    }

    #[test]
    fn inferred_fields_excludes_a_malformed_field() {
        // `id: 123` is authored-but-malformed: it carries a parse_issue,
        // so it must NOT appear in `inferred_fields` (that would let
        // `explicit_field` falsely claim it was "not authored").
        let content = "---\nid: 123\ntitle: T\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert!(
            !node.inferred_fields.contains(&"id".to_string()),
            "a malformed (parse_issue) field is not 'inferred': {:?}",
            node.inferred_fields
        );
        // title authored, status/kind genuinely absent.
        assert_eq!(node.inferred_fields, vec!["kind", "status"]);
    }

    #[test]
    fn non_string_status_reads_absent_for_initial_status_fallback() {
        // Absence semantics, not fabrication: the wrong-typed status is
        // recorded, and the empty status takes the same
        // `statuses.initial` inference a status-less document gets.
        let content = "---\nid: x\nstatus: 123\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert_eq!(node.status.as_str(), "");
        assert_eq!(node.parse_issues.len(), 1);
        assert_eq!(node.parse_issues[0].field, "status");
    }

    #[test]
    fn list_with_non_string_element_records_issue_on_the_field() {
        let content = "---\nid: x\ntags: [a, {b: c}]\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert!(node.tags.is_empty());
        assert_eq!(node.parse_issues.len(), 1);
        assert_eq!(node.parse_issues[0].field, "tags");
        assert_eq!(node.parse_issues[0].expected, "string or list of strings");
        assert_eq!(node.parse_issues[0].found, "array containing object");
    }

    #[test]
    fn failed_field_never_lands_in_attrs() {
        let content = "---\nid: x\ncreated: yesterday\npriority: high\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert!(!node.attrs.contains_key("created"));
        assert_eq!(
            node.attrs.get("priority"),
            Some(&serde_json::json!("high")),
            "sibling project-specific fields still land in attrs"
        );
    }

    #[test]
    fn multiple_failures_each_recorded_sorted_by_field() {
        let content = "---\nid: x\nupdated: soon\ncreated: yesterday\norphan_ok: 1\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        let fields: Vec<&str> = node.parse_issues.iter().map(|i| i.field.as_str()).collect();
        assert_eq!(fields, vec!["created", "orphan_ok", "updated"]);
    }

    #[test]
    fn sibling_fields_parse_intact_alongside_a_failure() {
        let content = "---\nid: x\ntitle: T\nstatus: active\ncreated: nope\ntags: [a]\n---\nBody";
        let (node, _) = parse_frontmatter(Path::new("doc.md"), content).unwrap();
        assert_eq!(node.id, "x");
        assert_eq!(node.title, "T");
        assert_eq!(node.status.as_str(), "active");
        assert_eq!(node.tags, vec!["a"]);
        assert_eq!(node.parse_issues.len(), 1);
    }

    #[test]
    fn extract_h1_handles_setext_and_inline() {
        let content = "---\nid: x\n---\nMy `Title`\n=========\n\nBody";
        let path = Path::new("doc.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.title, "My Title");
    }

    #[test]
    fn title_fallback_to_h1() {
        let content = "# My Document\n\nSome text";
        let path = Path::new("doc.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.title, "My Document");
    }

    #[test]
    fn empty_title_falls_back_to_h1_and_reads_as_inferred() {
        // An explicit `title: ""` is not authoring — it must take the same
        // H1/stem inference an absent title does, and `inferred_fields`
        // must list "title" so the two views agree (the resolver and the
        // explicit-field check both see "not authored").
        let content = "---\ntitle: \"\"\n---\n# Real Heading\n\nBody";
        let path = Path::new("doc.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.title, "Real Heading", "empty title infers from H1");
        assert!(
            node.inferred_fields.contains(&"title".to_string()),
            "an empty title is recorded as inferred: {:?}",
            node.inferred_fields
        );
    }

    #[test]
    fn empty_title_with_no_h1_falls_back_to_stem() {
        // No H1 either — the stem is the last resort, never a blank title.
        let content = "---\ntitle: \"\"\n---\nplain body, no heading";
        let path = Path::new("my-doc.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.title, "my-doc");
    }

    #[test]
    fn string_or_vec_single() {
        let content = "---\ntitle: T\nsupersedes: old-doc\n---\n";
        let path = Path::new("doc.md");
        let (node, _) = parse_frontmatter(path, content).unwrap();
        assert_eq!(node.supersedes, vec!["old-doc"]);
    }

    // ─── body fingerprint ──────────────────────────────────────────────
    //
    // The body's whole-document hash and per-line hash vector are the
    // primitive `body_immutable` consumes. These tests pin the
    // invariants the rule depends on: same body → same hash, different
    // body → different hash, line vector length matches `body.lines()`,
    // and identical line content produces byte-identical hash entries
    // (so prefix equality is well-defined for the append-only mode).

    #[test]
    fn body_hash_changes_with_body_text() {
        let path = Path::new("doc.md");
        let (a, _) = parse_frontmatter(path, "---\ntitle: T\n---\nfirst").unwrap();
        let (b, _) = parse_frontmatter(path, "---\ntitle: T\n---\nsecond").unwrap();
        assert_ne!(
            a.body_hash, b.body_hash,
            "different bodies must produce different hashes"
        );
    }

    #[test]
    fn body_hash_stable_for_identical_body() {
        let path = Path::new("doc.md");
        let (a, _) = parse_frontmatter(path, "---\ntitle: T\n---\nbody").unwrap();
        let (b, _) = parse_frontmatter(path, "---\ntitle: T\n---\nbody").unwrap();
        assert_eq!(
            a.body_hash, b.body_hash,
            "identical bodies must produce identical hashes"
        );
        assert_eq!(a.body_lines_hash, b.body_lines_hash);
    }

    #[test]
    fn body_lines_hash_matches_body_lines_iter() {
        // The per-line vector is what `append_only` mode compares
        // prefix-wise. Its length must equal `body.lines()` so a
        // reviewer reasoning about "the first N lines" can map
        // 1:1 between the source and the fingerprint.
        let path = Path::new("doc.md");
        let body = "alpha\nbeta\ngamma";
        let (n, _) = parse_frontmatter(path, &format!("---\nid: x\n---\n{body}")).unwrap();
        assert_eq!(n.body_lines_hash.len(), 3);
        // Identical lines produce identical hashes — append-only
        // prefix comparison relies on this byte-level stability.
        let (m, _) = parse_frontmatter(path, &format!("---\nid: x\n---\n{body}\nnew")).unwrap();
        assert_eq!(m.body_lines_hash[..3], n.body_lines_hash[..]);
        assert_eq!(m.body_lines_hash.len(), 4);
    }

    #[test]
    fn body_hash_for_empty_body_is_sha256_of_empty_string() {
        // A document with no body still gets a well-defined hash
        // (SHA-256 of the empty string). This means the body_immutable
        // rule treats "frontmatter-only doc" as a real state, never
        // a special case.
        let path = Path::new("doc.md");
        let (n, _) = parse_frontmatter(path, "---\nid: x\n---\n").unwrap();
        assert_eq!(
            n.body_hash,
            crate::hash::sha256_hex(""),
            "empty body must hash to SHA-256(\"\")"
        );
        assert!(n.body_lines_hash.is_empty());
    }

    // ─── canonicalisation ──────────────────────────────────────────────
    //
    // The same document must produce byte-identical fingerprints
    // regardless of how the host's editor / version-control workflow
    // serialised its line endings. Otherwise a Windows checkout would
    // false-fire every body_immutable rule on first parse.

    #[test]
    fn crlf_and_lf_produce_identical_fingerprints() {
        let path = Path::new("doc.md");
        let (lf, lf_body) = parse_frontmatter(path, "---\nid: x\n---\nalpha\nbeta").unwrap();
        let (crlf, crlf_body) =
            parse_frontmatter(path, "---\r\nid: x\r\n---\r\nalpha\r\nbeta").unwrap();
        assert_eq!(lf.body_hash, crlf.body_hash);
        assert_eq!(lf.body_lines_hash, crlf.body_lines_hash);
        assert_eq!(lf_body, crlf_body);
    }

    #[test]
    fn leading_bom_stripped_before_frontmatter_detection() {
        let path = Path::new("doc.md");
        let (with_bom, _) = parse_frontmatter(path, "\u{FEFF}---\nid: x\n---\nbody").unwrap();
        let (without_bom, _) = parse_frontmatter(path, "---\nid: x\n---\nbody").unwrap();
        assert_eq!(with_bom.id, "x");
        assert_eq!(with_bom.body_hash, without_bom.body_hash);
    }

    #[test]
    fn lone_cr_treated_as_line_break() {
        let path = Path::new("doc.md");
        let (lf, _) = parse_frontmatter(path, "---\nid: x\n---\nalpha\nbeta").unwrap();
        let (cr, _) = parse_frontmatter(path, "---\rid: x\r---\ralpha\rbeta").unwrap();
        assert_eq!(lf.id, cr.id);
        assert_eq!(lf.body_hash, cr.body_hash);
        assert_eq!(lf.body_lines_hash, cr.body_lines_hash);
    }

    #[test]
    fn canonicalize_lf_only_borrows() {
        let s = "---\nid: x\n---\nbody";
        let cow = canonicalize(s);
        assert!(matches!(cow, std::borrow::Cow::Borrowed(_)));
    }
}
