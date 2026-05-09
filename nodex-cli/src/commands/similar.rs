use anyhow::Result;
use clap::Args;
use std::path::{Path, PathBuf};

use nodex_core::query::similar::{self, SimilarityOptions, SimilarityTarget};

use super::query::load_graph;
use crate::format::{Envelope, ItemsEnvelope, print_json};

/// Args for `nodex similar`. Either `--id` (existing node) or
/// `--title` (pre-creation probe) is required; clap rejects both.
#[derive(Args)]
pub struct SimilarArgs {
    /// Existing node id to search neighbours of.
    #[arg(long, conflicts_with = "title")]
    pub id: Option<String>,
    /// Title text for a not-yet-created document.
    #[arg(long, conflicts_with = "id")]
    pub title: Option<String>,
    /// Kind for the prospective document (with `--title`).
    #[arg(long)]
    pub kind: Option<String>,
    /// Tags for the prospective document (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Parent directory for the prospective document.
    #[arg(long)]
    pub parent_dir: Option<PathBuf>,
    /// Override `config.similarity.threshold`.
    #[arg(long)]
    pub threshold: Option<f64>,
    /// Maximum candidates returned. Defaults to `config.similarity.default_limit`.
    #[arg(long)]
    pub limit: Option<usize>,
}

pub fn run(root: &Path, args: SimilarArgs, pretty: bool) -> Result<()> {
    let config = nodex_core::load_project(root)?;
    let graph = load_graph(root, &config)?;

    let opts = SimilarityOptions {
        threshold: args.threshold.unwrap_or(config.similarity.threshold),
        limit: args.limit.unwrap_or(config.similarity.default_limit),
    };

    let items = match (args.id.as_deref(), args.title.as_deref()) {
        (Some(id), _) => {
            similar::find_similar(&graph, &config, &SimilarityTarget::Node(id), &opts)?
        }
        (None, Some(title)) => {
            let target = SimilarityTarget::Spec {
                title,
                kind: args.kind.as_deref(),
                tags: &args.tags,
                parent_dir: args.parent_dir.as_deref(),
            };
            similar::find_similar(&graph, &config, &target, &opts)?
        }
        (None, None) => {
            anyhow::bail!("either --id or --title must be supplied");
        }
    };

    print_json(&Envelope::success(ItemsEnvelope::new(items)), pretty);
    Ok(())
}
