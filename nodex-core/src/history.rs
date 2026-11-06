//! What the project's history holds of documents its `HEAD` no longer
//! holds: the document each was when it left, and the commits that brought
//! its path in and took it out.

use std::path::Path;

use schemars::JsonSchema;
use serde::Serialize;

use crate::builder::scanner::ScopeSpelling;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::{CommitStamp, Removal, Repository};
use crate::parser::{ParseConfig, parse_document};
use crate::query::NodeRef;
use crate::warning::Warning;

/// What [`find_history`] is asked about.
#[derive(Debug, Clone, Copy)]
pub enum HistorySubject<'a> {
    Id(&'a str),
    /// A project path, normalized for lookup.
    Path(&'a str),
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HistoryReport {
    /// Whether the working tree holds a document under the id, or at the
    /// path.
    pub present: bool,
    /// Each document a commit `HEAD` reaches held under the id, or at the
    /// path, whose path `HEAD` no longer holds, latest removal first.
    pub removed: Vec<RemovedDocument>,
    /// Removed paths whose last revision does not parse, so the id each held
    /// is unknown; for a path lookup, that path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unparsed: Vec<String>,
}

/// A document as its last revision reads under the current configuration,
/// with the commits that added and removed its path.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RemovedDocument {
    #[serde(flatten)]
    pub node: NodeRef,
    /// The last revision's blob; `git cat-file -p <blob>` prints it.
    pub blob: String,
    /// The earliest commit adding the path; absent where only a merge's
    /// resolution added it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_added: Option<CommitStamp>,
    /// The latest commit removing the path.
    pub last_removed: CommitStamp,
}

/// What [`find_history`] found, with the working-tree build's disclosures.
pub struct HistoryOutcome {
    pub report: HistoryReport,
    pub warnings: Vec<Warning>,
}

/// Every document the history `HEAD` reaches held under `subject` and no
/// longer holds, read under the current configuration: a removed path counts
/// where the scope's spelling admits it, and its last revision is parsed as a
/// build would parse it now. `present` reads the working tree.
///
/// A clone whose history git reshapes — shallow, grafted or replaced — is
/// refused: a document it does not hold is one it cannot say was never there.
pub fn find_history(
    root: &Path,
    config: &Config,
    repository: &Repository,
    subject: HistorySubject<'_>,
) -> Result<HistoryOutcome> {
    let git_error = |context: &str, error: std::io::Error| Error::Git {
        context: context.to_string(),
        stderr: error.to_string(),
    };
    if repository
        .reshapes_history()
        .map_err(|e| git_error("whether the clone holds its whole history", e))?
    {
        return Err(Error::Git {
            context: "nodex history requires the whole history; fetch it".to_string(),
            stderr: "this clone's history is shallow, grafted or replaced".to_string(),
        });
    }
    let built = crate::builder::build_with_overlay(root, config, &[])?;
    let present = match subject {
        HistorySubject::Id(id) => built.graph.node(id).is_some(),
        HistorySubject::Path(path) => built
            .graph
            .nodes()
            .values()
            .any(|node| crate::path_guard::forward_string(&node.path) == path),
    };

    let head = repository
        .head()
        .map_err(|e| git_error("resolving HEAD", e))?;
    let spelling = ScopeSpelling::new(config)?;
    let removals: Vec<Removal> = match head {
        Some(head) => repository
            .removals(&head, &spelling.include_roots())
            .map_err(|e| git_error("reading the documents history removed", e))?
            .into_iter()
            .filter(|removal| spelling.admits(&removal.path))
            .filter(|removal| match subject {
                HistorySubject::Path(path) => {
                    crate::path_guard::forward_string(&removal.path) == path
                }
                HistorySubject::Id(_) => true,
            })
            .collect(),
        None => Vec::new(),
    };
    let contents = repository
        .blobs(
            &removals
                .iter()
                .map(|removal| removal.blob.clone())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| git_error("reading the removed documents' last revisions", e))?;

    let parse_config = ParseConfig::new(config);
    let mut removed = Vec::new();
    let mut unparsed = Vec::new();
    for (removal, content) in removals.into_iter().zip(contents) {
        let node = String::from_utf8(content)
            .ok()
            .and_then(|content| parse_document(&removal.path, &content, &parse_config).ok())
            .map(|parsed| parsed.node);
        let Some(node) = node else {
            unparsed.push(crate::path_guard::forward_string(&removal.path));
            continue;
        };
        if let HistorySubject::Id(id) = subject
            && node.id != id
        {
            continue;
        }
        removed.push(RemovedDocument {
            node: NodeRef::from_node(&node),
            blob: removal.blob,
            first_added: removal.first_added,
            last_removed: removal.last_removed,
        });
    }
    Ok(HistoryOutcome {
        report: HistoryReport {
            present,
            removed,
            unparsed,
        },
        warnings: built.warnings,
    })
}
