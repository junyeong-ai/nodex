//! Throughput benchmark for the document graph builder.
//!
//! Generates a synthetic corpus of N markdown documents in a temp dir
//! once per criterion suite, then measures full-rebuild time and the
//! best-case incremental rebuild (every file cached). The numbers feed
//! the README's "Performance" section and guard against regressions —
//! a PR that doubles the per-node cost will show up here long before
//! it shows up in user reports.

use criterion::{Criterion, criterion_group, criterion_main};
use std::fs;
use std::hint::black_box;
use std::path::Path;
use tempfile::TempDir;

use nodex_core::{
    Config, builder,
    query::{
        similar::{self, SimilarityOptions, SimilarityTarget},
        trust,
    },
};

fn build_corpus(root: &Path, doc_count: usize) {
    let docs = root.join("docs");
    fs::create_dir_all(&docs).unwrap();

    fs::write(
        root.join("nodex.toml"),
        r#"
[scope]
include = ["docs/**/*.md"]
"#,
    )
    .unwrap();

    for i in 0..doc_count {
        let mut body = String::new();
        if i > 0 {
            body.push_str(&format!("See [prev](docs/doc-{:05}.md)\n", i - 1));
        }
        if i > 1 {
            body.push_str(&format!("Also [grandparent](docs/doc-{:05}.md)\n", i - 2));
        }
        let content = format!(
            "---\nid: doc-{i:05}\ntitle: Doc {i}\nkind: generic\nstatus: active\n---\n# Doc {i}\n\n{body}"
        );
        fs::write(docs.join(format!("doc-{i:05}.md")), content).unwrap();
    }
}

fn bench_build(c: &mut Criterion) {
    for &size in &[1_000usize, 10_000usize] {
        let label = format!("nodes={size}");

        c.bench_function(&format!("build_full[{label}]"), |b| {
            b.iter_with_setup(
                || {
                    let tmp = TempDir::new().unwrap();
                    build_corpus(tmp.path(), size);
                    let config = Config::load(tmp.path()).unwrap();
                    (tmp, config)
                },
                |(tmp, config)| {
                    let result = builder::build(tmp.path(), &config, true).unwrap();
                    black_box(result);
                },
            );
        });

        c.bench_function(&format!("build_cached[{label}]"), |b| {
            b.iter_with_setup(
                || {
                    let tmp = TempDir::new().unwrap();
                    build_corpus(tmp.path(), size);
                    let config = Config::load(tmp.path()).unwrap();
                    let _ = builder::build(tmp.path(), &config, true).unwrap();
                    (tmp, config)
                },
                |(tmp, config)| {
                    let result = builder::build(tmp.path(), &config, false).unwrap();
                    black_box(result);
                },
            );
        });
    }
}

fn bench_similar(c: &mut Criterion) {
    c.bench_function("compute_similarity[nodes=10000]", |b| {
        b.iter_with_setup(
            || {
                let tmp = TempDir::new().unwrap();
                build_corpus(tmp.path(), 10_000);
                let config = Config::load(tmp.path()).unwrap();
                let result = builder::build(tmp.path(), &config, true).unwrap();
                (tmp, config, result.graph)
            },
            |(_, config, graph)| {
                let entries = similar::compute_similarity(
                    &graph,
                    &config,
                    &SimilarityTarget::Node("doc-05000"),
                    &SimilarityOptions::from_config(&config),
                )
                .unwrap();
                black_box(entries);
            },
        );
    });
}

fn bench_trust_listing_at_scale(c: &mut Criterion) {
    c.bench_function("compute_trust_ranking_bottom[nodes=10000]", |b| {
        b.iter_with_setup(
            || {
                let tmp = TempDir::new().unwrap();
                build_corpus(tmp.path(), 10_000);
                let config = Config::load(tmp.path()).unwrap();
                let result = builder::build(tmp.path(), &config, true).unwrap();
                (tmp, config, result.graph)
            },
            |(tmp, config, graph)| {
                let reports = trust::compute_trust_ranking(
                    &graph,
                    &config,
                    tmp.path(),
                    &trust::TrustListOptions {
                        extreme: trust::TrustExtreme::Bottom,
                        limit: 100,
                        kind: None,
                        below: None,
                    },
                );
                black_box(reports);
            },
        );
    });
}

criterion_group!(
    benches,
    bench_build,
    bench_similar,
    bench_trust_listing_at_scale
);
criterion_main!(benches);
