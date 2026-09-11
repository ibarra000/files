//! Matcher benchmarks.
//!
//! The performance claim behind this rewrite is that matching a
//! million-entry listing should cost single-digit milliseconds rather than
//! the hundreds the previous per-entry `str::find` needed. That claim is
//! measurable here, on synthetic data, with no network drives involved -
//! which is the only part of the speed story that *can* be measured on this
//! machine.
//!
//! Run with `cargo bench`. Useful comparisons:
//!
//! * `simd` versus `naive` at the same size - the algorithmic win
//! * scaling across 10k / 100k / 1M entries - confirms it stays linear
//! * query length and hit density - `memmem` is weakest on short needles
//! * `cancellation` - validates the chunk size empirically rather than by
//!   arithmetic

use std::time::{Duration, Instant, SystemTime};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use files::config::MatcherKind;
use files::index::builder::SnapshotBuilder;
use files::index::snapshot::Snapshot;
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

/// How the two implementations compare at a realistic size.
fn simd_versus_naive(c: &mut Criterion) {
    let snap = synthetic(200_000);
    let mut group = c.benchmark_group("simd_vs_naive");
    group.throughput(Throughput::Bytes(arena_bytes(&snap)));

    for kind in [MatcherKind::Simd, MatcherKind::Naive] {
        group.bench_function(BenchmarkId::from_parameter(format!("{kind:?}")), |b| {
            b.iter(|| matcher::search(&snap, "job_0012345", kind, &CancelToken::never()).unwrap());
        });
    }
    group.finish();
}

/// Latency against listing size. The interesting number is 1M: the flat root
/// is expected to hold between half a million and several million entries.
fn scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("scaling");
    group.sample_size(20);

    for n in [10_000usize, 100_000, 1_000_000] {
        let snap = synthetic(n);
        group.throughput(Throughput::Bytes(arena_bytes(&snap)));
        group.bench_with_input(BenchmarkId::from_parameter(n), &snap, |b, snap| {
            b.iter(|| {
                matcher::search(snap, "_0042_", MatcherKind::Simd, &CancelToken::never()).unwrap()
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
    for (label, query) in [
        ("rare_zzz", "zzzzzz"),
        ("short_job", "job"),
        ("selective", "job_0123456"),
        ("dense", "report"),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                matcher::search(&snap, query, MatcherKind::Simd, &CancelToken::never()).unwrap()
            });
        });
    }
    group.finish();
}

/// How quickly a superseded search stops.
///
/// The matcher checks for cancellation once per rayon chunk rather than per
/// entry, to keep the branch out of the vectorised inner loop. This measures
/// what that trade actually costs in responsiveness.
fn cancellation_latency(c: &mut Criterion) {
    let snap = synthetic(1_000_000);
    c.bench_function("cancellation_latency", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let epoch = Epoch::new();
                let token = epoch.token(epoch.current());
                // Superseded before the sweep begins, so this measures the
                // fixed cost of noticing.
                epoch.bump();
                let started = Instant::now();
                let _ = matcher::search(&snap, "job", MatcherKind::Simd, &token).unwrap();
                total += started.elapsed();
            }
            total
        });
    });
}

/// Building the index from names, which is the CPU half of a refresh.
///
/// Expected to be a rounding error next to the network I/O it accompanies -
/// this exists to confirm that assumption rather than assume it.
/// Gathering a code's pages, which runs once per Enter against the flat index.
///
/// Worth measuring because the sweep is uncapped and visits every entry: the
/// claim it rests on is that the O(1) name-length pre-filter rejects almost
/// everything before the arena is touched, so this should stay close to a
/// linear pass over the offsets array rather than over the names.
fn page_collection(c: &mut Criterion) {
    let mut group = c.benchmark_group("page_collection");
    for n in [100_000usize, 1_000_000] {
        let snap = synthetic(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("miss", n), &n, |b, _| {
            // The common shape: a code with no page set in this listing, so
            // every entry has to be rejected.
            b.iter(|| pages::collect(&snap, "11-D-0704", &CancelToken::never()));
        });
    }
    group.finish();
}

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

criterion_group!(
    benches,
    simd_versus_naive,
    scaling,
    query_shape,
    cancellation_latency,
    snapshot_build,
    page_collection
);
criterion_main!(benches);
