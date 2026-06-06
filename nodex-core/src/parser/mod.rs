pub mod body;
pub mod editor;
pub mod frontmatter;
pub mod identity;

use serde::Serialize;
use std::path::Path;

use crate::config::{
    AnnotationConfig, BodyLineRuleConfig, Config, IdentityConfig, ParserConfig, StatusesConfig,
    resolve_initial_status,
};
use crate::error::Result;
use crate::model::{Node, RawAnnotation, RawBodyLineMatch, RawEdge, Status};

/// The exact slice of [`Config`] that document parsing depends on.
///
/// Parsing reads nothing outside this view, which is what makes it the
/// single source of truth for cache invalidation: [`ParseConfig::cache_key`]
/// hashes precisely these fields (plus the binary version), so a new
/// parse-affecting config option *cannot* be added without surfacing
/// here — the compiler refuses to thread it through `parse_document`
/// otherwise. Config that only steers validation or query ranking
/// (`schema`, `trust`, `similarity`, `detection`, `scope`, `kinds`,
/// naming rules) is deliberately absent: it never changes a cached parse
/// result, so tuning it must not force a full reparse. `schema` in
/// particular is a pure check-time concern — the initial status a
/// frontmatter-less parse assigns comes from `statuses`, never from
/// `schema.enums` ordering.
#[derive(Serialize)]
pub struct ParseConfig<'a> {
    identity: &'a IdentityConfig,
    statuses: &'a StatusesConfig,
    parser: &'a ParserConfig,
    annotations: &'a [AnnotationConfig],
    body_line: &'a [BodyLineRuleConfig],
}

impl<'a> ParseConfig<'a> {
    /// Project the parse-affecting surface out of the full config.
    pub fn new(config: &'a Config) -> Self {
        Self {
            identity: &config.identity,
            statuses: &config.statuses,
            parser: &config.parser,
            annotations: &config.annotations,
            body_line: &config.rules.body_line,
        }
    }

    /// Content-addressed cache key for the build cache: a SHA-256 of the
    /// parse-affecting config plus the binary version. The version salt
    /// makes every nodex upgrade a one-time full rebuild, guarding
    /// against `Node` / `RawEdge` struct-shape drift in the serialised
    /// cache. Serialisation operates on the parsed structs, so TOML
    /// whitespace and comments never perturb the key while semantic
    /// changes (rule reordering, a new annotation pattern) always do.
    pub fn cache_key(&self) -> String {
        #[derive(Serialize)]
        struct Keyed<'a, 'b> {
            nodex: &'static str,
            parse: &'b ParseConfig<'a>,
        }
        let canonical = serde_json::to_string(&Keyed {
            nodex: env!("CARGO_PKG_VERSION"),
            parse: self,
        })
        .expect("ParseConfig is serialisable");
        crate::hash::sha256_hex(&canonical)
    }

    /// Initial status for a frontmatter-less document, resolved from the
    /// same source of truth `scaffold` uses.
    fn initial_status_for(&self) -> &str {
        resolve_initial_status(self.statuses)
    }
}

/// Result of parsing a single document.
pub struct ParsedDocument {
    pub node: Node,
    pub raw_edges: Vec<RawEdge>,
    pub raw_annotations: Vec<RawAnnotation>,
    pub raw_body_line_matches: Vec<RawBodyLineMatch>,
}

/// Parse a document: extract frontmatter, infer identity, extract links.
pub fn parse_document(
    path: &Path,
    content: &str,
    config: &ParseConfig<'_>,
) -> Result<ParsedDocument> {
    // 1. Parse frontmatter → partial node + body
    let (mut node, body) = frontmatter::parse_frontmatter(path, content)?;

    // 2. Infer kind if empty
    if node.kind.as_str().is_empty() {
        node.kind = identity::infer_kind(path, config.identity);
    }

    // 3. Infer id if empty
    if node.id.is_empty() {
        node.id = identity::infer_id(path, &node.kind, config.identity);
    }

    // 4. Infer status if empty — same source of truth scaffold uses,
    //    so a frontmatter-less document and a fresh scaffold land on
    //    the same default and the project's enum rules see only values
    //    its config has authorised.
    if node.status.as_str().is_empty() {
        node.status = Status::new(config.initial_status_for());
    }

    // 5. Extract links from body (pulldown-cmark + wikilinks + custom patterns)
    let mut raw_edges = body::extract_links(&body, config.parser);

    // 5a. Extract config-declared annotations from the same body —
    // pre-graph markers (`[PROMOTES: …]`, `[NEEDS RESEARCH: …]`, …)
    // captured independently from edge resolution. Kind-based filtering
    // (`kinds`) is applied by the builder during
    // materialisation; this pass extracts every match so a doc whose
    // kind changes does not require a body re-read.
    let raw_annotations = body::extract_annotations(&body, config.annotations);

    // 5b. Extract config-declared body-line pattern matches. Same
    // discipline as annotations — pattern matching only, no enum
    // validation. `BodyLineRule` validates the stored captures
    // against current enum config at check time, so the parser
    // stays a pure function of (body, pattern list) with no
    // rule-output coupling.
    let raw_body_line_matches = body::extract_body_line_matches(&body, config.body_line);

    // 5. Generate edges from frontmatter relations
    for target in &node.supersedes {
        raw_edges.push(RawEdge {
            target_path: target.clone(),
            relation: "supersedes".to_string(),
            location: "frontmatter:supersedes".to_string(),
        });
    }
    for target in &node.implements {
        raw_edges.push(RawEdge {
            target_path: target.clone(),
            relation: "implements".to_string(),
            location: "frontmatter:implements".to_string(),
        });
    }
    for target in &node.related {
        raw_edges.push(RawEdge {
            target_path: target.clone(),
            relation: "related".to_string(),
            location: "frontmatter:related".to_string(),
        });
    }
    // Code-coverage edges: out-of-graph paths the doc covers. The
    // resolver leaves them Unresolved by design (code paths aren't
    // graph nodes); GitDriftRule and `query covered-by` consume them
    // by their relation tag.
    for target in &node.covers {
        raw_edges.push(RawEdge {
            target_path: target.clone(),
            relation: "covers".to_string(),
            location: "frontmatter:covers".to_string(),
        });
    }

    Ok(ParsedDocument {
        node,
        raw_edges,
        raw_annotations,
        raw_body_line_matches,
    })
}
