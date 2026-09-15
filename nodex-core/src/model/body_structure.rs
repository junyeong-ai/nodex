//! What the markdown parser reads in a document body that the body's lines
//! alone do not show — extracted at parse time so a body lock can judge an
//! edit by the document's structure while every check-time rule stays a pure
//! function of `(graph, config)`.
//!
//! Line positions count the body's lines from 0 — the same positions as
//! [`crate::model::Node::body_lines_hash`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A body's top-level sections and the link reference definitions its
/// references resolve to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BodyStructure {
    /// In body order ([`crate::parser::body::extract_structure`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<BodySection>,
    /// Only the definitions some reference resolves to, in body order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub definitions: Vec<ReferenceDefinition>,
}

impl BodyStructure {
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() && self.definitions.is_empty()
    }
}

/// A heading as the markdown parser reads it: its level and its plain text,
/// whichever way it is spelled (ATX or setext, closing `#`s, inline
/// emphasis).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SectionHeading {
    pub level: u8,
    pub text: String,
}

/// The run of body lines a top-level heading opens, or the lines before the
/// first heading when they hold anything but blank lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BodySection {
    /// `None` for the lines before the first heading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<SectionHeading>,
    /// The line the section opens on.
    pub start: usize,
    /// One past the section's last non-blank line.
    pub content_end: usize,
}

/// A link reference definition (`[label]: destination "title"`) that at least
/// one reference link or image in the body resolves to. A definition applies
/// to references anywhere in the document, before it as much as after, which
/// is why a line added at the end can change what an earlier line says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReferenceDefinition {
    /// The line the definition opens on.
    pub start: usize,
    /// One past the definition's last line.
    pub end: usize,
    /// The line the first reference resolving to it starts on.
    pub first_use: usize,
}
