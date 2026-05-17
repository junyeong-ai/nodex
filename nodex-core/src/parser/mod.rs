pub mod body;
pub mod editor;
pub mod frontmatter;
pub mod identity;

use std::path::Path;

use crate::config::Config;
use crate::error::Result;
use crate::model::{Node, RawAnnotation, RawEdge, Status};

/// Result of parsing a single document.
pub struct ParsedDocument {
    pub node: Node,
    pub raw_edges: Vec<RawEdge>,
    pub raw_annotations: Vec<RawAnnotation>,
}

/// Parse a document: extract frontmatter, infer identity, extract links.
pub fn parse_document(path: &Path, content: &str, config: &Config) -> Result<ParsedDocument> {
    // 1. Parse frontmatter → partial node + body
    let (mut node, body) = frontmatter::parse_frontmatter(path, content)?;

    // 2. Infer kind if empty
    if node.kind.as_str().is_empty() {
        node.kind = identity::infer_kind(path, config);
    }

    // 3. Infer id if empty
    if node.id.is_empty() {
        node.id = identity::infer_id(path, &node.kind, config);
    }

    // 4. Infer status if empty — same source of truth scaffold uses,
    //    so a frontmatter-less document and a fresh scaffold land on
    //    the same default and the project's enum rules see only values
    //    its config has authorised.
    if node.status.as_str().is_empty() {
        node.status = Status::new(config.initial_status_for(node.kind.as_str()));
    }

    // 5. Extract links from body (pulldown-cmark + wikilinks + custom patterns)
    let mut raw_edges = body::extract_links(&body, &config.parser);

    // 5a. Extract config-declared annotations from the same body —
    // pre-graph markers (`[PROMOTES: …]`, `[NEEDS RESEARCH: …]`, …)
    // captured independently from edge resolution. Kind-based filtering
    // (`applies_to_kind`) is applied by the builder during
    // materialisation; this pass extracts every match so a doc whose
    // kind changes does not require a body re-read.
    let raw_annotations = body::extract_annotations(&body, &config.annotations);

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
    })
}
