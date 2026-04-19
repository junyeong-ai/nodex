use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{Confidence, Node, RawEdge};

/// Cached parse result for a single document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub content_hash: String,
    pub node: Node,
    pub raw_edges: Vec<CachedRawEdge>,
}

/// Serializable version of RawEdge (RawEdge itself doesn't derive Serialize).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRawEdge {
    pub target_path: String,
    pub relation: String,
    pub confidence: Confidence,
    pub location: String,
}

impl From<&RawEdge> for CachedRawEdge {
    fn from(e: &RawEdge) -> Self {
        Self {
            target_path: e.target_path.clone(),
            relation: e.relation.clone(),
            confidence: e.confidence,
            location: e.location.clone(),
        }
    }
}

impl From<CachedRawEdge> for RawEdge {
    fn from(e: CachedRawEdge) -> Self {
        Self {
            target_path: e.target_path,
            relation: e.relation,
            confidence: e.confidence,
            location: e.location,
        }
    }
}

/// Incremental build cache. Maps relative path → CacheEntry.
/// Includes config_hash to auto-invalidate when config changes.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct BuildCache {
    #[serde(default)]
    pub config_hash: String,
    pub entries: BTreeMap<PathBuf, CacheEntry>,
}

impl BuildCache {
    /// Load cache from disk. Returns empty cache when the file is
    /// absent, unreadable, corrupt, or was produced under a different
    /// config hash. The second return value is an optional warning
    /// string explaining why — callers surface it so users see why
    /// an unexpectedly-slow rebuild is happening.
    pub fn load(cache_path: &Path, current_config_hash: &str) -> (Self, Option<String>) {
        if !cache_path.exists() {
            return (Self::default(), None);
        }

        let raw = match std::fs::read_to_string(cache_path) {
            Ok(s) => s,
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!(
                        "cache unreadable at {}: {e}; rebuilding from scratch",
                        cache_path.display()
                    )),
                );
            }
        };

        let cache: Self = match serde_json::from_str(&raw) {
            Ok(c) => c,
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!(
                        "cache corrupt at {}: {e}; rebuilding from scratch",
                        cache_path.display()
                    )),
                );
            }
        };

        if cache.config_hash != current_config_hash {
            return (Self::default(), None); // config changed — expected invalidation, no warning
        }

        (cache, None)
    }

    /// Save cache to disk.
    pub fn save(&self, cache_path: &Path) -> Result<()> {
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let json = serde_json::to_string(self)
            .map_err(|e| Error::Other(format!("cache serialization error: {e}")))?;
        std::fs::write(cache_path, json).map_err(|e| Error::Io {
            path: cache_path.to_path_buf(),
            source: e,
        })
    }

    /// Get cached parse result if fresh.
    pub fn get(&self, rel_path: &Path, content: &str) -> Option<&CacheEntry> {
        let entry = self.entries.get(rel_path)?;
        if entry.content_hash == compute_hash(content) {
            Some(entry)
        } else {
            None
        }
    }

    /// Store a parse result.
    pub fn insert(&mut self, rel_path: PathBuf, content: &str, node: Node, raw_edges: &[RawEdge]) {
        self.entries.insert(
            rel_path,
            CacheEntry {
                content_hash: compute_hash(content),
                node,
                raw_edges: raw_edges.iter().map(CachedRawEdge::from).collect(),
            },
        );
    }

    /// Remove entries for paths no longer in scope.
    pub fn retain_paths(&mut self, valid_paths: &[PathBuf]) {
        let valid: std::collections::HashSet<&PathBuf> = valid_paths.iter().collect();
        self.entries.retain(|k, _| valid.contains(k));
    }
}

pub fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher.finalize().iter().fold(String::new(), |mut acc, b| {
        std::fmt::Write::write_fmt(&mut acc, format_args!("{b:02x}")).unwrap();
        acc
    })
}
