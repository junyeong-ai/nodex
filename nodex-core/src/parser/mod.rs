pub mod body;
pub mod frontmatter;
pub mod identity;

use std::path::Path;

use crate::config::Config;
use crate::error::Result;
use crate::model::{Confidence, Node, RawEdge};

/// Result of parsing a single document.
pub struct ParsedDocument {
    pub node: Node,
    pub raw_edges: Vec<RawEdge>,
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

    // 4. Extract links from body (pulldown-cmark + custom patterns)
    let mut raw_edges = body::extract_links(&body, &config.parser.link_patterns);

    // 5. Generate edges from frontmatter relations
    for target in &node.supersedes {
        raw_edges.push(RawEdge {
            target_path: target.clone(),
            relation: "supersedes".to_string(),
            confidence: Confidence::Extracted,
            location: "frontmatter:supersedes".to_string(),
        });
    }
    for target in &node.implements {
        raw_edges.push(RawEdge {
            target_path: target.clone(),
            relation: "implements".to_string(),
            confidence: Confidence::Extracted,
            location: "frontmatter:implements".to_string(),
        });
    }
    for target in &node.related {
        raw_edges.push(RawEdge {
            target_path: target.clone(),
            relation: "related".to_string(),
            confidence: Confidence::Extracted,
            location: "frontmatter:related".to_string(),
        });
    }

    Ok(ParsedDocument { node, raw_edges })
}
