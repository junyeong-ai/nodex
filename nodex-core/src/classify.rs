//! How the configuration reads a document path before the graph is asked:
//! whether the scope admits a document there, and the identity a document
//! there takes when it declares none.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::builder::scanner::{self, Proposed};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::parser::{ParseConfig, parse_document};
use crate::warning::Warning;

/// How the configuration reads one path.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClassifyEntry {
    /// Normalized, forward-slash project path.
    pub path: String,
    /// Whether the scope admits a document at the path, as the scan every
    /// build reads decides it.
    pub in_scope: bool,
    /// The fields a document at the path takes when it declares none of
    /// them; absent where the scope admits no document. A document's own
    /// frontmatter overrides each, and `query node --path` reads the values a
    /// graphed document holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inferred: Option<InferredFields>,
}

/// The identity and status a build completes for a document that leaves them
/// out.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InferredFields {
    pub kind: String,
    pub id: String,
    pub status: String,
}

/// What [`classify`] found, with the scan's disclosures.
pub struct ClassifyOutcome {
    /// One entry per path asked, in the order asked.
    pub items: Vec<ClassifyEntry>,
    pub warnings: Vec<Warning>,
}

/// Classify each of `paths`, project paths as a user names them.
///
/// Each path is normalized as every write surface normalizes it, so the answer
/// is about the document a write there would create. A path the tree does not
/// hold is judged as a document declaring nothing: a `conditional_exclude`
/// keeps it out by its parent's status alone, since whether it is a terminal
/// parent itself is a question of content, which `check --content` judges.
pub fn classify(
    root: &Path,
    config: &Config,
    paths: &[impl AsRef<str>],
) -> Result<ClassifyOutcome> {
    let paths = paths
        .iter()
        .map(|raw| crate::path_guard::normalize_doc_path(root, raw.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    let mut overlay: Vec<(PathBuf, Proposed)> = Vec::new();
    for path in &paths {
        let path = PathBuf::from(path);
        if !holds_entry(root, &path)? && !overlay.iter().any(|(named, _)| *named == path) {
            overlay.push((path, Proposed::Content(String::new())));
        }
    }
    let scan = scanner::scan_scope_with_overlay(root, config, &overlay)?;
    let parse_config = ParseConfig::new(config);
    let items = paths
        .into_iter()
        .map(|path| {
            let in_scope = scan.admits(Path::new(&path))?;
            let inferred = in_scope
                .then(|| inferred_fields(Path::new(&path), &parse_config))
                .transpose()?;
            Ok(ClassifyEntry {
                path,
                in_scope,
                inferred,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ClassifyOutcome {
        items,
        warnings: scanner::boundary_warning(&scan.unfollowed_in_scope, "admit")
            .into_iter()
            .collect(),
    })
}

/// The fields a build completes for a document at `path` that declares none,
/// read from the parser rather than from the rules it applies.
fn inferred_fields(path: &Path, parse_config: &ParseConfig<'_>) -> Result<InferredFields> {
    let node = parse_document(path, "", parse_config)?.node;
    Ok(InferredFields {
        kind: node.kind.as_str().to_string(),
        id: node.id,
        status: node.status.as_str().to_string(),
    })
}

/// Whether the tree holds an entry at `path`. A component that is not a
/// directory leaves nothing there either.
fn holds_entry(root: &Path, path: &Path) -> Result<bool> {
    let full = root.join(path);
    match std::fs::symlink_metadata(&full) {
        Ok(_) => Ok(true),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(false)
        }
        Err(source) => Err(Error::Io { path: full, source }),
    }
}
