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
    session::{self, LogEventSpec},
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
        // Simple cross-reference graph: doc N points to N-1 and N-2.
        // Average degree ≈ 2, enough to exercise the resolver and
        // adjacency builder without dominating wall-clock with regex.
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
            // Warm cache once per benchmark setup, then measure the
            // best-case incremental rebuild where every file is a hit.
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

/// Append `events` events to one session document, exercising the
/// minimal-diff frontmatter editor + body append. Rollover via the
/// supersession chain is not crossed at the default 200-event cap.
fn bench_session_log_append(c: &mut Criterion) {
    c.bench_function("session_log_append[events=100]", |b| {
        b.iter_with_setup(
            || {
                let tmp = TempDir::new().unwrap();
                fs::write(
                    tmp.path().join("nodex.toml"),
                    r#"
[scope]
include = ["docs/**/*.md", "_sessions/**/*.md"]

[kinds]
allowed = ["generic", "session"]

[session]
log_kind = "session"
session_dir = "_sessions"
max_events_per_session = 1000
"#,
                )
                .unwrap();
                let config = Config::load(tmp.path()).unwrap();
                (tmp, config)
            },
            |(tmp, config)| {
                let session_id = "session-bench";
                for i in 0..100 {
                    let r = session::log_event(
                        tmp.path(),
                        &config,
                        LogEventSpec {
                            session_id: Some(session_id.to_string()),
                            summary: format!("event {i}"),
                            related: if i % 5 == 0 {
                                vec![format!("doc-{i:05}")]
                            } else {
                                Vec::new()
                            },
                            tags: Vec::new(),
                        },
                    )
                    .unwrap();
                    black_box(r);
                }
            },
        );
    });
}

/// Similarity over a 10k-node corpus exercises the 2-stage pruning:
/// the cheap title/tags/kind/directory pass should reject most
/// candidates before the linked Jaccard runs.
fn bench_similar(c: &mut Criterion) {
    c.bench_function("find_similar[nodes=10000]", |b| {
        b.iter_with_setup(
            || {
                let tmp = TempDir::new().unwrap();
                build_corpus(tmp.path(), 10_000);
                let config = Config::load(tmp.path()).unwrap();
                let result = builder::build(tmp.path(), &config, true).unwrap();
                (tmp, config, result.graph)
            },
            |(_, config, graph)| {
                let entries = similar::find_similar(
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

/// Trust score for every node in a 10k-node corpus — the natural
/// shape of `find_low_trust` and the bottleneck once `git_drift` is
/// enabled, since it shells out per outgoing edge.
fn bench_trust_low_at_scale(c: &mut Criterion) {
    c.bench_function("find_low_trust[nodes=10000]", |b| {
        b.iter_with_setup(
            || {
                let tmp = TempDir::new().unwrap();
                build_corpus(tmp.path(), 10_000);
                let config = Config::load(tmp.path()).unwrap();
                let result = builder::build(tmp.path(), &config, true).unwrap();
                (tmp, config, result.graph)
            },
            |(tmp, config, graph)| {
                let reports = trust::find_low_trust(&graph, &config, tmp.path(), 1.0, None);
                black_box(reports);
            },
        );
    });
}

criterion_group!(
    benches,
    bench_build,
    bench_session_log_append,
    bench_similar,
    bench_trust_low_at_scale,
);
criterion_main!(benches);
