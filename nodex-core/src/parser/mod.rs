pub mod body;
pub mod editor;
pub mod frontmatter;
pub mod identity;

use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::config::{
    AnnotationConfig, BodyLineRuleConfig, Config, IdentityConfig, InitialStatusInputs, ParserConfig,
};
use crate::error::Result;
use crate::model::{Node, RawAnnotation, RawBodyLineMatch, RawEdge, Status};

/// The `[identity]` block projected to what resolves a document's kind and
/// id.
///
/// Built by destructuring each rule exhaustively, so a field added to one is
/// a compile error here until somebody decides whether parsing reads it. The
/// block is not borrowed whole: an attribute the parser cannot reach — one
/// that only decides whether an empty rule is worth reporting — would
/// otherwise sit in the cache key and cost every project a full reparse for
/// writing it down.
#[derive(Serialize)]
struct IdentityParse<'a> {
    kind_rules: Vec<KindResolution<'a>>,
    id_rules: Vec<IdResolution<'a>>,
}

/// One `identity.kind_rules` entry, as kind resolution reads it.
#[derive(Serialize)]
struct KindResolution<'a> {
    glob: &'a str,
    kind: &'a str,
}

/// One `identity.id_rules` entry, as id resolution reads it.
#[derive(Serialize)]
struct IdResolution<'a> {
    kind: &'a str,
    glob: Option<&'a str>,
    template: &'a str,
}

fn hash_identity_resolution<S: serde::Serializer>(
    identity: &IdentityConfig,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    IdentityParse::new(identity).serialize(serializer)
}

impl<'a> IdentityParse<'a> {
    fn new(identity: &'a IdentityConfig) -> Self {
        let IdentityConfig {
            kind_rules,
            id_rules,
        } = identity;
        Self {
            kind_rules: kind_rules
                .iter()
                .map(|rule| {
                    let crate::config::KindRule {
                        glob,
                        kind,
                        may_be_empty: _,
                    } = rule;
                    KindResolution { glob, kind }
                })
                .collect(),
            id_rules: id_rules
                .iter()
                .map(|rule| {
                    let crate::config::IdRule {
                        kind,
                        glob,
                        template,
                        may_be_empty: _,
                    } = rule;
                    IdResolution {
                        kind,
                        glob: glob.as_deref(),
                        template,
                    }
                })
                .collect(),
        }
    }
}

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
/// result, so tuning it must not force a full reparse. Of `statuses`,
/// parsing consumes *only* where a document starts (the default a
/// document declaring no status takes), which a governing `statuses.flow`
/// may set per kind; `terminal`, the transitions and the non-first
/// `allowed` entries are pure check-time concerns, so the view is keyed on
/// what [`Config::initial_status_for`] reads rather than on the whole
/// struct — editing `statuses.terminal` or a flow's `transitions` cannot
/// force a reparse.
#[derive(Serialize)]
pub struct ParseConfig<'a> {
    #[serde(flatten)]
    completion: Completion<'a>,
    parser: &'a ParserConfig,
    #[serde(serialize_with = "hash_annotations")]
    annotations: &'a [AnnotationConfig],
    #[serde(serialize_with = "hash_body_line")]
    body_line: &'a [BodyLineRuleConfig],
    #[serde(skip)]
    pub(crate) prepared: Arc<OnceLock<PreparedPatterns>>,
}

fn hash_annotations<S: serde::Serializer>(
    annotations: &&[AnnotationConfig],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    let projected: Vec<_> = annotations
        .iter()
        .map(
            |AnnotationConfig {
                 name,
                 pattern,
                 key,
                 kinds: _,
             }| (name, pattern, key),
        )
        .collect();
    projected.serialize(serializer)
}

fn hash_body_line<S: serde::Serializer>(
    blocks: &&[BodyLineRuleConfig],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    let projected: Vec<_> = blocks
        .iter()
        .map(
            |BodyLineRuleConfig {
                 name,
                 pattern,
                 kinds: _,
                 enums: _,
             }| (name, pattern),
        )
        .collect();
    projected.serialize(serializer)
}

#[derive(Clone)]
pub(crate) struct PreparedPatterns {
    links: Vec<(regex::Regex, String, bool)>,
    annotations: Vec<(String, String, regex::Regex)>,
    body_line: Vec<(String, regex::Regex)>,
}

/// How a document's inferrable fields are completed from config where it
/// left them out — the one completion chain, read by the parser for every
/// node it builds and by the scan for a `conditional_exclude` parent, whose
/// status decides membership before any node exists. Two chains would let
/// the scan and the graph give the same bytes two statuses.
#[derive(Serialize)]
pub struct Completion<'a> {
    #[serde(serialize_with = "hash_identity_resolution")]
    identity: &'a IdentityConfig,
    /// Where a document starts: the inputs [`Config::initial_status_for`]
    /// answers from — the seam `scaffold`, `migrate` and `status_entry`
    /// read — so the fallback writes exactly what `status_entry` requires.
    initial_status: InitialStatusInputs<'a>,
}

impl<'a> Completion<'a> {
    pub fn new(config: &'a Config) -> Self {
        Self {
            identity: &config.identity,
            initial_status: config.initial_status_inputs(),
        }
    }

    /// Fill the fields a document may leave to config, in the order the
    /// rules depend on: `kind` first, because `identity.id_rules` are
    /// keyed by it and a governing `statuses.flow` is chosen by it; then
    /// `id`, which those rules select; then the status the kind starts at,
    /// so a document that declares none and a fresh `scaffold` land on the
    /// same default and the project's enum rules only ever see values its
    /// config authorised.
    pub fn resolve_identity(&self, node: &mut Node, path: &Path) {
        if node.kind.as_str().is_empty() {
            node.kind = identity::infer_kind(path, self.identity);
        }
        if node.id.is_empty() {
            node.id = identity::infer_id(path, &node.kind, self.identity);
        }
        if node.status.as_str().is_empty() {
            node.status = Status::new(self.initial_status.initial_for(node.kind.as_str()));
        }
    }
}

impl<'a> ParseConfig<'a> {
    /// Project the parse-affecting surface out of the full config.
    pub fn new(config: &'a Config) -> Self {
        Self {
            completion: Completion::new(config),
            parser: &config.parser,
            annotations: &config.annotations,
            body_line: &config.rules.body_line,
            prepared: Arc::default(),
        }
    }

    pub(crate) fn patterns(&self) -> &PreparedPatterns {
        self.prepared.get_or_init(|| PreparedPatterns {
            links: body::compile_patterns(&self.parser.link_patterns),
            annotations: body::compile_annotations(self.annotations),
            body_line: body::compile_body_line(self.body_line),
        })
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

    /// Complete a parsed node through [`Completion::resolve_identity`].
    ///
    /// Every reader that pairs one parsed node against another completes
    /// them both here. A second completion chain elsewhere would let two
    /// nodes built from the same bytes disagree about a field the
    /// document never wrote — and an id is what a pairing is keyed on.
    pub fn resolve_identity(&self, node: &mut Node, path: &Path) {
        self.completion.resolve_identity(node, path);
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
    parse_with_patterns(path, content, config, config.patterns())
}

pub(crate) fn parse_with_patterns(
    path: &Path,
    content: &str,
    config: &ParseConfig<'_>,
    prepared: &PreparedPatterns,
) -> Result<ParsedDocument> {
    // 1. Parse frontmatter → partial node + body. The content hash is
    //    taken over the exact bytes the parse consumed
    //    (pre-canonicalisation) — the same digest the build cache keys
    //    on, so snapshot consumers can compare a node against the
    //    working tree without re-deriving a second hashing convention.
    let (mut node, body) = frontmatter::parse_frontmatter(path, content)?;
    node.content_hash = crate::hash::sha256_hex(content);

    // 2. Fill the identity fields the document left to config.
    config.resolve_identity(&mut node, path);

    // 3. Extract links from body (pulldown-cmark + wikilinks + custom patterns)
    let mut raw_edges = body::extract_links_prepared(&body, config.parser, &prepared.links);

    // 3a. Extract config-declared annotations from the same body —
    // pre-graph markers (`[PROMOTES: …]`, `[NEEDS RESEARCH: …]`, …)
    // captured independently from edge resolution. Kind-based filtering
    // (`kinds`) is applied by the builder during
    // materialisation; this pass extracts every match so a doc whose
    // kind changes does not require a body re-read.
    let raw_annotations = body::extract_annotations_prepared(&body, &prepared.annotations);

    // 3b. Extract config-declared body-line pattern matches. Same
    // discipline as annotations — pattern matching only, no enum
    // validation. `BodyLineRule` validates the stored captures
    // against current enum config at check time, so the parser
    // stays a pure function of (body, pattern list) with no
    // rule-output coupling.
    let raw_body_line_matches =
        body::extract_body_line_matches_prepared(&body, &prepared.body_line);

    // 4. Generate edges from frontmatter relations
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StatusFlowConfig;

    /// A document declaring no status starts where its kind does, for every
    /// kind it can declare, one outside `kinds.allowed` included: a flow over
    /// every kind governs that one too, and a scoped flow leaves it at the
    /// global.
    #[test]
    fn a_status_less_document_starts_where_its_kind_does() {
        for (flow_kinds, starts) in [
            (
                vec![],
                [
                    ("generic", "draft"),
                    ("spec", "draft"),
                    ("outsider", "draft"),
                ],
            ),
            (
                vec!["spec".to_string()],
                [
                    ("generic", "active"),
                    ("spec", "draft"),
                    ("outsider", "active"),
                ],
            ),
        ] {
            let mut config = Config::default();
            config.kinds.allowed = vec!["generic".into(), "spec".into()];
            config.statuses.allowed = vec!["active".into(), "draft".into(), "archived".into()];
            config.statuses.initial = Some("active".into());
            config.statuses.flow = Some(StatusFlowConfig {
                kinds: flow_kinds.clone(),
                initial: Some("draft".into()),
                transitions: [("draft".to_string(), vec!["active".to_string()])].into(),
            });
            let parse = ParseConfig::new(&config);
            for (kind, status) in starts {
                let content = format!("---\nid: p\ntitle: P\nkind: {kind}\n---\n# P\n");
                let parsed = parse_document(Path::new("p.md"), &content, &parse).unwrap();
                assert_eq!(
                    (parsed.node.status.as_str(), config.initial_status_for(kind)),
                    (status, status),
                    "flow kinds {flow_kinds:?}, kind {kind}"
                );
            }
        }
    }
}
