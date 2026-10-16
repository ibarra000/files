//! Matcher benchmarks on synthetic data. The target is single-digit
//! milliseconds for a million-entry listing.
//!
//! Run with `cargo bench`. Useful comparisons:
//!
//! * `simd` versus `naive` at the same size - the algorithmic win
//! * scaling across 10k / 100k / 1M entries - confirms it stays linear
//! * query length and hit density - `memmem` is weakest on short needles
//! * `cancellation` - validates the chunk size empirically rather than by
//!   arithmetic

use std::time::{Duration, Instant, SystemTime};

use files::search::query::Query;

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use files::config::MatcherKind;
use files::config::SEGMENT_MAX_BYTES;
use files::config::hidden::Hidden;
use files::index::builder::SnapshotBuilder;
use files::index::snapshot::Snapshot;
use files::index::tree::{SegmentBuilder, TreeIndex};
use files::search::{matcher, pages};
use files::util::cancel::{CancelToken, Epoch};

/// A listing shaped like the real one: job codes with a suffix.
fn synthetic(n: usize) -> Snapshot {
    let mut b = SnapshotBuilder::with_capacity("V:\\", n, 40);
    for i in 0..n {
        b.push_str(&format!("job_{i:07}_{}_report.pdf", i % 997));
    }
    b.finish(SystemTime::UNIX_EPOCH, 0, None)
}

fn arena_bytes(snap: &Snapshot) -> u64 {
    snap.lower().len() as u64
}

/// Compares the two implementations at a realistic size.
fn simd_versus_naive(c: &mut Criterion) {
    let snap = synthetic(200_000);
    let mut group = c.benchmark_group("simd_vs_naive");
    group.throughput(Throughput::Bytes(arena_bytes(&snap)));

    for kind in [MatcherKind::Simd, MatcherKind::Naive] {
        group.bench_function(BenchmarkId::from_parameter(format!("{kind:?}")), |b| {
            b.iter(|| {
                matcher::search(
                    &snap,
                    &Query::contains("job_0012345"),
                    kind,
                    &Hidden::none(),
                    &CancelToken::never(),
                )
                .unwrap()
            });
        });
    }
    group.finish();
}

/// Latency against listing size. 1M is the key size: the flat root holds
/// between half a million and several million entries.
fn scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("scaling");
    group.sample_size(20);

    for n in [10_000usize, 100_000, 1_000_000] {
        let snap = synthetic(n);
        group.throughput(Throughput::Bytes(arena_bytes(&snap)));
        group.bench_with_input(BenchmarkId::from_parameter(n), &snap, |b, snap| {
            b.iter(|| {
                matcher::search(
                    snap,
                    &Query::contains("_0042_"),
                    MatcherKind::Simd,
                    &Hidden::none(),
                    &CancelToken::never(),
                )
                .unwrap()
            });
        });
    }
    group.finish();
}

/// Needle length and hit density both matter: `memmem`'s prefilter fires more
/// often on short or common needles, which slows the scan.
fn query_shape(c: &mut Criterion) {
    let snap = synthetic(500_000);
    let mut group = c.benchmark_group("query_shape");
    group.sample_size(20);
    group.throughput(Throughput::Bytes(arena_bytes(&snap)));

    // rare: nothing matches. common: matches roughly one entry in a thousand.
    // dense: matches nearly everything.
    // one_char: the worst case the one-character search floor allows. It
    // matches nearly every name, so `topk` ranks the whole arena. If it is
    // slow, the fix is an early-out in `topk`, not a higher floor.
    for (label, query) in [
        ("one_char", "j"),
        ("rare_zzz", "zzzzzz"),
        ("short_job", "job"),
        ("selective", "job_0123456"),
        ("dense", "report"),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                matcher::search(
                    &snap,
                    &Query::contains(query),
                    MatcherKind::Simd,
                    &Hidden::none(),
                    &CancelToken::never(),
                )
                .unwrap()
            });
        });
    }
    group.finish();
}

/// How quickly a superseded search stops.
///
/// The matcher checks for cancellation once per rayon chunk rather than per
/// entry, to keep the branch out of the vectorised inner loop. This measures
/// the cost in responsiveness.
fn cancellation_latency(c: &mut Criterion) {
    let snap = synthetic(1_000_000);
    c.bench_function("cancellation_latency", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let epoch = Epoch::new();
                let token = epoch.token(epoch.current());
                // Cancelled before the sweep begins: measures the fixed cost
                // of noticing.
                epoch.bump();
                let started = Instant::now();
                let _ = matcher::search(
                    &snap,
                    &Query::contains("job"),
                    MatcherKind::Simd,
                    &Hidden::none(),
                    &token,
                )
                .unwrap();
                total += started.elapsed();
            }
            total
        });
    });
}

/// Gathering a code's pages, which runs once per Enter against the flat index.
///
/// The sweep is uncapped, so this relies on the O(1) name-length prefilter
/// rejecting almost every entry before the arena is touched.
fn page_collection(c: &mut Criterion) {
    let mut group = c.benchmark_group("page_collection");
    for n in [100_000usize, 1_000_000] {
        let snap = synthetic(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("miss", n), &n, |b, _| {
            // Common case: no page set for the code, so every entry is rejected.
            b.iter(|| pages::collect(&snap, "11-D-0704", &CancelToken::never()));
        });
    }
    group.finish();
}

/// Building the index from names, the CPU half of a refresh. Expected to be
/// negligible next to the network I/O.
fn snapshot_build(c: &mut Criterion) {
    let names: Vec<String> = (0..200_000)
        .map(|i| format!("job_{i:07}_{}_report.pdf", i % 997))
        .collect();

    let mut group = c.benchmark_group("build");
    group.sample_size(20);
    group.throughput(Throughput::Elements(names.len() as u64));
    group.bench_function("push_str", |b| {
        b.iter(|| {
            let mut builder = SnapshotBuilder::with_capacity("V:\\", names.len(), 40);
            for n in &names {
                builder.push_str(n);
            }
            builder.finish(SystemTime::UNIX_EPOCH, 0, None)
        });
    });
    group.finish();
}

/// A walked tree shaped like the real share: `dirs` folders, `per_dir` files
/// in each.
///
/// Segments are sealed on the sink's arena budget, so the segment count matches
/// the application's.
fn synthetic_tree(dirs: usize, per_dir: usize) -> TreeIndex {
    let mut index = TreeIndex::empty("R:\\");
    let mut b = SegmentBuilder::new();
    let mut files: Vec<String> = Vec::with_capacity(per_dir);

    for d in 0..dirs {
        let rel = format!("{:02}\\{:02}\\job_{d:06}", d % 64, (d / 64) % 64);
        files.clear();
        for f in 0..per_dir {
            files.push(format!("{d:06}-{f:02} drawing rev a.pdf"));
        }
        b.push_dir(&rel, &files);
        if b.arena_bytes() >= SEGMENT_MAX_BYTES {
            index = index.appended(Arc::new(b.seal()));
        }
    }
    if !b.is_empty() {
        index = index.appended(Arc::new(b.seal()));
    }
    index
}

/// A query at the scale of the real share: about 300,000 folders and 5,000,000
/// files. The expected cost is 5-16 ms; above roughly 15 ms a per-segment
/// n-gram prefilter is needed.
///
/// Each query sweeps the directory arena (folder matches) and then the filename
/// arena, and both are included in the measurement.
fn tree_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_scaling");
    group.sample_size(10);

    // 17 files per folder, as measured on the real share.
    for (dirs, per_dir) in [(30_000usize, 17usize), (300_000, 17)] {
        let index = synthetic_tree(dirs, per_dir);
        let bytes: u64 = index
            .segments()
            .iter()
            .map(|s| (s.files().lower().len() + s.dirs().lower().len()) as u64)
            .sum();
        group.throughput(Throughput::Bytes(bytes));
        group.bench_with_input(
            BenchmarkId::new("files", index.len()),
            &index,
            |b, index| {
                b.iter(|| {
                    matcher::search_tree(
                        index,
                        &Query::contains("123456-07"),
                        &Hidden::none(),
                        &CancelToken::never(),
                    )
                    .unwrap()
                });
            },
        );
        // A query naming a folder, so hits come through the directory table
        // and its per-folder expansion.
        group.bench_with_input(
            BenchmarkId::new("folder", index.len()),
            &index,
            |b, index| {
                b.iter(|| {
                    matcher::search_tree(
                        index,
                        &Query::contains("job_012345"),
                        &Hidden::none(),
                        &CancelToken::never(),
                    )
                    .unwrap()
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    simd_versus_naive,
    scaling,
    tree_scaling,
    query_shape,
    cancellation_latency,
    snapshot_build,
    page_collection
);
criterion_main!(benches);
