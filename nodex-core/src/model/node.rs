use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::kind::Kind;
use super::status::Status;

/// A document node in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    // === Identity ===
    pub id: String,
    #[serde(
        serialize_with = "serialize_path",
        deserialize_with = "deserialize_path"
    )]
    pub path: PathBuf,
    pub title: String,
    pub kind: Kind,

    // === Lifecycle ===
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewed: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    // === Relations (from frontmatter) ===
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    // === Flags ===
    #[serde(default)]
    pub orphan_ok: bool,

    // === Extension point for project-specific frontmatter fields ===
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: BTreeMap<String, serde_json::Value>,
}

/// Always serialize path with forward slashes for cross-platform JSON.
fn serialize_path<S: serde::Serializer>(path: &Path, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&path.to_string_lossy().replace('\\', "/"))
}

/// Deserialize path from string.
fn deserialize_path<'de, D: serde::Deserializer<'de>>(d: D) -> Result<PathBuf, D::Error> {
    let s = String::deserialize(d)?;
    Ok(PathBuf::from(s))
}
