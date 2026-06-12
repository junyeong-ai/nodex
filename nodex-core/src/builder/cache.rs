use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::hash;
use crate::model::{Node, RawAnnotation, RawBodyLineMatch, RawEdge};
use crate::path_guard;

/// Cached parse result for a single document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub content_hash: String,
    pub node: Node,
    pub raw_edges: Vec<RawEdge>,
    #[serde(default)]
    pub raw_annotations: Vec<RawAnnotation>,
    #[serde(default)]
    pub raw_body_line_matches: Vec<RawBodyLineMatch>,
}

/// On-disk shape version of `cache.json`. Bump on any change to the
/// serialized shape of [`BuildCache`] / [`CacheEntry`] (including the
/// `Node` / `RawEdge` payloads) — the same discipline as
/// `model::graph::SCHEMA_VERSION` for `graph.json`. A mismatching or
/// absent version discards the cache on load (cold rebuild, never an
/// error), so a cache written by a binary with a different entry shape
/// can never deserialize leniently into defaulted fields and serve
/// stale nodes.
const CACHE_SCHEMA_VERSION: u32 = 1;

/// Incremental build cache. Maps relative path → CacheEntry.
///
/// Two load guards, independent in what they protect against:
/// `schema_version` rejects a cache whose serialized *shape* this
/// binary does not own (see [`CACHE_SCHEMA_VERSION`]); `config_hash`
/// auto-invalidates entries whenever the parse-affecting config
/// surface or the nodex binary version changes. The hash is computed
/// by `builder::build`; this struct only stores it for comparison on
/// the next load.
#[derive(Debug, Serialize, Deserialize)]
pub struct BuildCache {
    /// Stamped on save. The load guard reads the version from the
    /// *raw* JSON before the typed decode (a foreign-shape cache may
    /// not even deserialize as this struct, and must discard by
    /// version, never report corrupt); `#[serde(default)]` keeps the
    /// typed decode shape-tolerant for the same reason.
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub config_hash: String,
    pub entries: BTreeMap<PathBuf, CacheEntry>,
}

impl Default for BuildCache {
    /// The empty cache carries the current schema version, so a fresh
    /// save is admitted by the load guard it will face on the next run.
    fn default() -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            config_hash: String::new(),
            entries: BTreeMap::new(),
        }
    }
}

impl BuildCache {
    /// Load cache from disk. Returns empty cache when the file is
    /// absent, unreadable, corrupt, carries a foreign (or no)
    /// `schema_version`, or was produced under a different config
    /// hash. The second return value is an optional warning string
    /// explaining why — callers surface it so users see why an
    /// unexpectedly-slow rebuild is happening.
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

        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
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

        // Shape guard first, probed from the raw JSON before the typed
        // decode: a cache written under a foreign entry shape may not
        // even deserialize as the current `BuildCache`, and that must
        // read as the silent versioned discard (expected invalidation),
        // never as "corrupt". An absent or non-numeric version can
        // never equal the current one — discarded, never trusted.
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(CACHE_SCHEMA_VERSION))
        {
            return (Self::default(), None); // shape changed — expected invalidation, no warning
        }

        let cache: Self = match serde_json::from_value(value) {
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

    /// Save cache to disk via the project-wide guarded write primitive
    /// so a crash mid-write leaves the previous cache intact rather
    /// than producing a half-written `cache.json` that the next run
    /// would treat as corrupt and silently full-rebuild. `root`
    /// enforces containment: a cache path escaping the project through
    /// a symlinked ancestor is refused, which the caller surfaces as a
    /// warning — the cache is an optimization, the build's graph is
    /// still correct.
    pub fn save(&self, root: &Path, cache_path: &Path) -> Result<()> {
        let json = serde_json::to_string(self).expect("BuildCache is JSON-serialisable");
        path_guard::write_atomic_in_root(root, cache_path, &json)
    }

    /// Get cached parse result if fresh.
    pub fn get(&self, rel_path: &Path, content: &str) -> Option<&CacheEntry> {
        let entry = self.entries.get(rel_path)?;
        if entry.content_hash == hash::sha256_hex(content) {
            Some(entry)
        } else {
            None
        }
    }

    /// Store a parse result.
    ///
    /// The argument list mirrors the cache's payload one-for-one;
    /// bundling into a wrapper struct would obscure the data flow
    /// without removing the parameters from the caller's hot path.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        rel_path: PathBuf,
        content: &str,
        node: Node,
        raw_edges: &[RawEdge],
        raw_annotations: &[RawAnnotation],
        raw_body_line_matches: &[RawBodyLineMatch],
    ) {
        self.entries.insert(
            rel_path,
            CacheEntry {
                content_hash: hash::sha256_hex(content),
                node,
                raw_edges: raw_edges.to_vec(),
                raw_annotations: raw_annotations.to_vec(),
                raw_body_line_matches: raw_body_line_matches.to_vec(),
            },
        );
    }

    /// Remove entries for paths no longer in scope.
    pub fn retain_paths(&mut self, valid_paths: &[PathBuf]) {
        let valid: std::collections::HashSet<&PathBuf> = valid_paths.iter().collect();
        self.entries.retain(|k, _| valid.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with_one_entry() -> BuildCache {
        let node = Node {
            id: "doc-a".to_string(),
            path: PathBuf::from("docs/a.md"),
            title: "A".to_string(),
            kind: crate::model::Kind::new("generic"),
            status: crate::model::Status::new("active"),
            created: None,
            updated: None,
            reviewed: None,
            owner: None,
            supersedes: vec![],
            superseded_by: None,
            implements: vec![],
            related: vec![],
            tags: vec![],
            covers: vec![],
            orphan_ok: false,
            attrs: BTreeMap::new(),
            body_hash: String::new(),
            body_lines_hash: Vec::new(),
            content_hash: hash::sha256_hex("content"),
            parse_issues: vec![],
        };
        let mut cache = BuildCache {
            config_hash: "cfg".to_string(),
            ..BuildCache::default()
        };
        cache.insert(PathBuf::from("docs/a.md"), "content", node, &[], &[], &[]);
        cache
    }

    #[test]
    fn load_discards_cache_with_absent_or_foreign_schema_version() {
        // The shape guard is independent of the config hash: a cache
        // whose serialized shape this binary does not own — no
        // `schema_version` at all, or an older number — is discarded
        // on load (cold rebuild), never deserialized leniently into
        // defaulted entry fields.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cache.json");
        let cache = cache_with_one_entry();

        // Round-trip sanity: the current shape is admitted.
        cache.save(dir.path(), &path).unwrap();
        let (loaded, warning) = BuildCache::load(&path, "cfg");
        assert!(warning.is_none());
        assert_eq!(loaded.entries.len(), 1, "current shape round-trips");

        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // schema_version absent — a cache carrying no version field at all.
        json.as_object_mut().unwrap().remove("schema_version");
        std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();
        let (loaded, warning) = BuildCache::load(&path, "cfg");
        assert!(warning.is_none(), "expected invalidation, no warning");
        assert!(loaded.entries.is_empty(), "version-less cache is discarded");

        // schema_version carrying an older number.
        json["schema_version"] = serde_json::json!(CACHE_SCHEMA_VERSION - 1);
        std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();
        let (loaded, warning) = BuildCache::load(&path, "cfg");
        assert!(warning.is_none(), "expected invalidation, no warning");
        assert!(loaded.entries.is_empty(), "foreign version is discarded");

        // A foreign version whose entries no longer deserialize under
        // the current shape at all: the raw-JSON version probe fires
        // before the typed decode, so this is still the silent
        // versioned discard — never the "corrupt" warning.
        json["entries"] = serde_json::json!({ "docs/a.md": { "bogus": true } });
        std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();
        let (loaded, warning) = BuildCache::load(&path, "cfg");
        assert!(
            warning.is_none(),
            "foreign-shape entries under an old version discard silently: {warning:?}"
        );
        assert!(loaded.entries.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn build_cache_save_refuses_escaping_dir_and_target_symlink() {
        use std::os::unix::fs as unix_fs;
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let cache = BuildCache::default();

        // A cache path under a symlinked-out-of-root ancestor is refused.
        unix_fs::symlink(outside.path(), root.path().join("_index")).unwrap();
        let err = cache
            .save(root.path(), &root.path().join("_index/cache.json"))
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::OutsideRoot(_)));
        assert!(!outside.path().join("cache.json").exists());

        // A cache path whose final component is itself a symlink is
        // refused even when fully in-root — the staged rename would
        // silently replace the user's link with a regular file.
        let real = root.path().join("real-cache.json");
        std::fs::write(&real, "{}").unwrap();
        let link = root.path().join("cache.json");
        unix_fs::symlink(&real, &link).unwrap();
        let err = cache.save(root.path(), &link).unwrap_err();
        assert!(matches!(err, crate::error::Error::OutsideRoot(_)));
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "{}");
    }
}
