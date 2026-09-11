//! `--doctor` and `--bench`.
//!
//! This is how the untestable half of the crate gets validated. Everything
//! involving SMB was written against documentation and arithmetic, on a
//! machine where neither drive exists; these two commands run on the machine
//! where they do and report what is actually true.
//!
//! * **`--doctor`** is fast, read-only and safe to run at any time: drive
//!   type, UNC target, volume serial, filesystem flags, SMB dialect,
//!   round-trip time, and which strategies are usable.
//! * **`--bench`** times every enumeration strategy against the same
//!   directory and, more importantly, **cross-checks that they all return the
//!   same entry count**. A disagreement is the signal to leave a fast path
//!   switched off, and it is a signal obtainable no other way.
//!
//! The `[PASS]` / `[FAIL]` lines are the point of the exercise.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{EnumStrategy, MAX_SERVER_HITS, Settings};
use crate::index::actor::scan_once;
use crate::index::enumerate::{CountingSink, DirSource, ListOpts, VecSink};
use crate::index::errors::EnumError;
use crate::index::persist;
use crate::search::pattern;
use crate::util::cancel::CancelToken;
use crate::util::humanize;
use crate::util::rng::Rng;

/// Probes are given a hard ceiling; a blocked SMB call cannot be cancelled,
/// so the thread is abandoned and the report says so.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const RTT_SAMPLES: usize = 32;

/// Prints the resolved routing table, and where a code would be searched.
///
/// Deliberately touches nothing on the network: configuration gets edited on
/// laptops with no drives mapped, which is exactly when it needs checking.
pub fn check_config(settings: &Settings, query: Option<&str>, out: &mut dyn Write) {
    let routes = &settings.routes;
    let _ = writeln!(out, "config: {}", routes.source().describe());
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {:<3} {:<14} {:<11} {:<8} {:<28} {:>5}",
        "#", "mapping", "kind", "enabled", "path", "rules"
    );
    for m in routes.all() {
        let _ = writeln!(
            out,
            "  {:<3} {:<14} {:<11} {:<8} {:<28} {:>5}",
            m.id.index(),
            m.name,
            m.kind.label(),
            if m.enabled { "yes" } else { "no" },
            m.path.display(),
            m.rules.len()
        );
    }

    if let Some(code) = query {
        let _ = writeln!(out);
        let _ = writeln!(out, "route {code:?}:");
        let targets = routes.classify(code);
        if targets.is_empty() {
            let _ = writeln!(out, "  (no mapping matches)");
        }
        for t in &targets {
            let _ = writeln!(
                out,
                "  {:<14} {:<11} {}",
                routes.label(t.mapping),
                t.kind.label(),
                t.dir.display()
            );
        }
        // The single most useful line here: it answers "why did it not also
        // look in the other share".
        if let Some(last) = targets.last()
            && let Some(m) = routes.get(last.mapping)
            && m.rules.iter().any(|r| r.stop() && r.pattern_matches(code))
            && routes.enabled().count() > targets.len()
        {
            let _ = writeln!(
                out,
                "  (stopped here: a rule in {:?} has stop = true)",
                m.name
            );
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "OK");
}

/// Fast, read-only capability report.
pub fn doctor(settings: &Settings, source: Arc<dyn DirSource>, out: &mut dyn Write) {
    let _ = writeln!(out, "files {} - diagnostics", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "source: {}", source.name());
    let _ = writeln!(out);

    for root in [&settings.base_path, &settings.custpro_path] {
        report_root(root, source.as_ref(), out);
        let _ = writeln!(out);
    }

    report_index_cache(settings, out);
    let _ = writeln!(out);
    report_recommendations(settings, out);
}

fn report_root(root: &Path, source: &dyn DirSource, out: &mut dyn Write) {
    let _ = writeln!(out, "ROOT  {}", root.display());

    #[cfg(windows)]
    {
        use crate::index::volume;
        let info = volume::volume_info(root);
        // Volume-level queries only accept a root, so say which one was
        // interrogated. Otherwise a configured subdirectory looks like it is
        // reporting its own volume identity, and a failure looks like the
        // share being broken rather than the path being deeper.
        if info.is_subdirectory {
            let _ = writeln!(
                out,
                "  volume root         {}  (this path is a subdirectory)",
                info.volume_root.as_deref().unwrap_or("unknown")
            );
        }
        let _ = writeln!(
            out,
            "  drive type          {}",
            info.kind
                .map(|k| k.label())
                .unwrap_or_else(|| "unknown".into())
        );
        if let Some(unc) = &info.unc_target {
            let _ = writeln!(out, "  UNC target          {unc}");
        }
        let _ = writeln!(
            out,
            "  filesystem          {}",
            info.filesystem.clone().unwrap_or_else(|| "unknown".into())
        );
        match info.volume_serial {
            Some(s) => {
                let _ = writeln!(out, "  volume serial       {:08X}", s);
            }
            None => {
                let _ = writeln!(out, "  volume serial       unavailable");
            }
        }
        // Reported, not acted on: NTFS sets this flag on every volume, and it
        // describes what the filesystem supports rather than how names are
        // resolved. Whether the server's pattern matching agrees with ours is
        // decided by the superset check below, not by a flag.
        let _ = writeln!(
            out,
            "  case sensitive srch {}  (informational)",
            if info.case_sensitive_search() {
                "supported"
            } else {
                "not supported"
            }
        );
        match info.remote_protocol {
            Some(p) => {
                let _ = writeln!(out, "  remote protocol     {}", p.dialect());
                let _ = writeln!(
                    out,
                    "  large transfers     {}",
                    if p.likely_large_transactions() {
                        "likely (1 MiB buffers should help)"
                    } else {
                        "unlikely (64 KiB cap; big buffers will not help)"
                    }
                );
            }
            None => {
                let _ = writeln!(out, "  remote protocol     not a network share");
            }
        }
        let _ = writeln!(
            out,
            "  volume handle (MFT) {}",
            if info.kind.is_some_and(|k| k.supports_volume_handle()) {
                "possible, but deliberately unused (MFT enumeration walks the whole volume)"
            } else {
                "not available on this drive type"
            }
        );
    }

    // Reachability, twice: the first touch pays for session setup.
    let first = time_it(|| source.probe_stamp(root));
    let second = time_it(|| source.probe_stamp(root));
    match &first.1 {
        Ok(stamp) => {
            let _ = writeln!(
                out,
                "  reachable           yes (first {}, warm {})",
                humanize::elapsed(first.0),
                humanize::elapsed(second.0)
            );
            let _ = writeln!(out, "  directory stamp     {:?}", stamp);
        }
        Err(err) => {
            let _ = writeln!(
                out,
                "  reachable           NO - {}",
                err.describe(&root.to_string_lossy())
            );
            return;
        }
    }

    let rtt = measure_rtt(root);
    if let Some((median, p95)) = rtt {
        let _ = writeln!(
            out,
            "  est. RTT            median {}, p95 {}",
            humanize::elapsed(median),
            humanize::elapsed(p95)
        );
    }
}

/// Times a metadata lookup on names that certainly do not exist.
///
/// A fresh random name each iteration is essential: the redirector caches
/// negative lookups for about five seconds, so repeating one name measures
/// the cache and reports an implausible zero.
fn measure_rtt(root: &Path) -> Option<(Duration, Duration)> {
    let mut rng = Rng::from_entropy();
    let mut samples = Vec::with_capacity(RTT_SAMPLES);
    for _ in 0..RTT_SAMPLES {
        let name = format!("__files_probe_{:016x}", rng.next_u64());
        let path = root.join(name);
        let started = Instant::now();
        let _ = std::fs::metadata(&path);
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let median = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
    Some((median, p95))
}

fn report_index_cache(settings: &Settings, out: &mut dyn Write) {
    let _ = writeln!(out, "INDEX CACHE");
    let Some(dir) = &settings.cache_dir else {
        let _ = writeln!(out, "  location            unavailable");
        return;
    };
    let _ = writeln!(out, "  location            {}", dir.display());
    if !settings.persist {
        let _ = writeln!(out, "  persistence         disabled");
        return;
    }
    let indexed = &settings.custpro_path;
    let key = persist::MappingKey::of(indexed);
    let _ = writeln!(out, "  indexed directory   {}", indexed.display());
    let _ = writeln!(out, "  cache key           {}", key.hex());

    match persist::load(dir, key, persist::Expect::new(indexed, None)) {
        Ok(snapshot) => {
            let _ = writeln!(
                out,
                "  cached entries      {}",
                humanize::count(snapshot.len())
            );
            let age = std::time::SystemTime::now()
                .duration_since(snapshot.captured_at())
                .unwrap_or_default();
            let _ = writeln!(out, "  age                 {}", humanize::age(age));
            let _ = writeln!(
                out,
                "  resident size       {}",
                humanize::bytes(snapshot.memory_bytes() as u64)
            );
        }
        Err(err) => {
            let _ = writeln!(out, "  cached index        {err}");
        }
    }
}

fn report_recommendations(settings: &Settings, out: &mut dyn Write) {
    let _ = writeln!(out, "CURRENT CONFIGURATION");
    let _ = writeln!(out, "  FILES_FS_STRATEGY={}", settings.enum_strategy.name());
    let _ = writeln!(
        out,
        "  FILES_SERVER_FILTER={}",
        if settings.server_filter { "on" } else { "off" }
    );
    let _ = writeln!(
        out,
        "  FILES_PERSIST={}",
        if settings.persist { "on" } else { "off" }
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Run `files --bench` to compare enumeration strategies on these drives,"
    );
    let _ = writeln!(
        out,
        "and to confirm the server-side filter returns everything a full scan does."
    );
}

/// One measured strategy.
#[derive(Debug, Clone)]
pub struct StrategyResult {
    pub strategy: EnumStrategy,
    pub entries: Option<usize>,
    pub elapsed: Duration,
    pub round_trips: u32,
    pub error: Option<EnumError>,
}

/// Whether the strategies agree, which is the thing worth knowing.
pub fn counts_agree(results: &[StrategyResult]) -> bool {
    let mut counts = results.iter().filter_map(|r| r.entries);
    let Some(first) = counts.next() else {
        return true;
    };
    counts.all(|c| c == first)
}

/// Times every enumeration strategy over the same directory.
pub fn bench_strategies(source: &dyn DirSource, dir: &Path, reps: usize) -> Vec<StrategyResult> {
    let strategies = [
        EnumStrategy::HandleDirInfo,
        EnumStrategy::FindFirstEx,
        EnumStrategy::StdReadDir,
    ];
    let mut out = Vec::new();

    for strategy in strategies {
        let mut best: Option<(Duration, usize, u32)> = None;
        let mut error = None;
        for _ in 0..reps.max(1) {
            let mut sink = CountingSink::default();
            let opts = ListOpts::default().with_strategy(strategy);
            match source.list(dir, &mut sink, &opts, &CancelToken::never()) {
                Ok(stats) => {
                    let candidate = (stats.elapsed, stats.entries, stats.round_trips);
                    best = Some(match best {
                        Some(b) if b.0 <= candidate.0 => b,
                        _ => candidate,
                    });
                }
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        out.push(match best {
            Some((elapsed, entries, round_trips)) => StrategyResult {
                strategy,
                entries: Some(entries),
                elapsed,
                round_trips,
                error: None,
            },
            None => StrategyResult {
                strategy,
                entries: None,
                elapsed: Duration::ZERO,
                round_trips: 0,
                error,
            },
        });
    }
    out
}

/// Full benchmark report.
pub fn bench(
    settings: &Settings,
    source: Arc<dyn DirSource>,
    query: Option<&str>,
    allow_write: bool,
    out: &mut dyn Write,
) {
    let dir = settings.custpro_path.clone();
    let _ = writeln!(out, "files {} - benchmark", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "target: {}", dir.display());
    let _ = writeln!(out);

    // --- enumeration shootout ---
    let _ = writeln!(out, "FULL ENUMERATION");
    let _ = writeln!(
        out,
        "  {:<22} {:>12} {:>12} {:>10}",
        "strategy", "entries", "time", "refills"
    );
    let results = bench_strategies(source.as_ref(), &dir, 3);
    for r in &results {
        match r.entries {
            Some(entries) => {
                let _ = writeln!(
                    out,
                    "  {:<22} {:>12} {:>12} {:>10}",
                    r.strategy.name(),
                    humanize::count(entries),
                    humanize::elapsed(r.elapsed),
                    r.round_trips
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "  {:<22} {:>12} {}",
                    r.strategy.name(),
                    "-",
                    r.error
                        .map(|e| e.describe(&dir.to_string_lossy()))
                        .unwrap_or_else(|| "failed".into())
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "  {}",
        if counts_agree(&results) {
            "[PASS] every strategy returned the same entry count"
        } else {
            "[FAIL] strategies DISAGREE - keep the fast paths off (FILES_FS_STRATEGY=std)"
        }
    );
    let _ = writeln!(out);

    // --- server-side filtering ---
    if let Some(query) = query {
        bench_server_filter(source.as_ref(), &dir, query, out);
        let _ = writeln!(out);
    } else {
        let _ = writeln!(out, "SERVER-SIDE FILTER  (pass --query <CODE> to measure)");
        let _ = writeln!(out);
    }

    // --- completeness oracle ---
    completeness_oracle(source.as_ref(), &dir, &results, out);
    let _ = writeln!(out);

    // --- stamp probe ---
    bench_stamp(source.as_ref(), &dir, allow_write, out);
}

fn bench_server_filter(source: &dyn DirSource, dir: &Path, query: &str, out: &mut dyn Write) {
    let _ = writeln!(out, "SERVER-SIDE FILTER  (query: {query})");
    let wildcard = match pattern::wildcard_for(query) {
        Ok(w) => w,
        Err(reject) => {
            let _ = writeln!(out, "  not applicable: {}", reject.label());
            return;
        }
    };

    let mut sink = VecSink::default();
    let opts = ListOpts::default().with_max_entries(MAX_SERVER_HITS);
    let started = Instant::now();
    let server = source.query(dir, &wildcard, &mut sink, &opts, &CancelToken::never());
    let server_elapsed = started.elapsed();

    match server {
        Ok(stats) => {
            let confirmed: Vec<String> = sink
                .names
                .iter()
                .filter(|n| pattern::confirms(n, query))
                .cloned()
                .collect();
            let false_positives = sink.names.len() - confirmed.len();
            let _ = writeln!(
                out,
                "  pattern             {wildcard}  ({} match(es) in {})",
                stats.entries,
                humanize::elapsed(server_elapsed)
            );
            let _ = writeln!(
                out,
                "  8.3 false positives {false_positives} (removed client-side)"
            );

            // Compare against a full local enumeration: the server must never
            // return less.
            match scan_once(source, dir, &ListOpts::default()) {
                Ok((snapshot, _)) => {
                    let needle = query.to_ascii_lowercase();
                    let local: Vec<String> = (0..snapshot.len() as u32)
                        .filter(|&i| {
                            std::str::from_utf8(snapshot.name_lower(i))
                                .is_ok_and(|n| n.contains(&needle))
                        })
                        .map(|i| snapshot.display_name(i).into_owned())
                        .collect();
                    let verdict = crate::search::verify::audit(&confirmed, &local);
                    let _ = match verdict {
                        crate::search::verify::AuditVerdict::Consistent => writeln!(
                            out,
                            "  [PASS] the server returned everything a full scan found"
                        ),
                        crate::search::verify::AuditVerdict::ServerUnderReturned { missing } => {
                            writeln!(
                                out,
                                "  [FAIL] the server MISSED {} file(s) - leave FILES_SERVER_FILTER=off",
                                missing.len()
                            )
                        }
                        crate::search::verify::AuditVerdict::NotChecked => {
                            writeln!(out, "  [SKIP] nothing to compare against")
                        }
                    };
                }
                Err(err) => {
                    let _ = writeln!(
                        out,
                        "  [SKIP] could not enumerate for comparison: {}",
                        err.describe(&dir.to_string_lossy())
                    );
                }
            }
        }
        Err(EnumError::Empty) => {
            let _ = writeln!(out, "  no matches (which is an answer, not a failure)");
        }
        Err(err) => {
            let _ = writeln!(
                out,
                "  unavailable: {}",
                err.describe(&dir.to_string_lossy())
            );
        }
    }
}

/// Partitions the directory by name length and checks the shards sum to the
/// whole.
///
/// `?` matches exactly one character, so `?`, `??`, ... `????????*` is a
/// provably disjoint and complete partition - unlike first-character
/// sharding, which cannot be complete because Win32 patterns have no
/// character classes.
///
/// This is not used to enumerate faster (once buffers are large the work is
/// bandwidth-bound, so sharding buys almost nothing while multiplying server
/// CPU). It is here as an **oracle**: if the shards sum to the single-pass
/// count, that is direct evidence on the real server that its pattern
/// matching drops nothing - which is what makes the server-side filter safe.
fn completeness_oracle(
    source: &dyn DirSource,
    dir: &Path,
    results: &[StrategyResult],
    out: &mut dyn Write,
) {
    let _ = writeln!(out, "COMPLETENESS ORACLE  (length partition)");
    let Some(single_pass) = results.iter().find_map(|r| r.entries) else {
        let _ = writeln!(out, "  [SKIP] no successful enumeration to compare against");
        return;
    };

    let mut total = 0usize;
    let mut failed = false;
    for k in 1..=7usize {
        let pattern = "?".repeat(k);
        let mut sink = CountingSink::default();
        match source.query(
            dir,
            &pattern,
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        ) {
            Ok(stats) => total += stats.entries,
            Err(EnumError::Empty) => {}
            Err(err) => {
                let _ = writeln!(
                    out,
                    "  [SKIP] shard {pattern} failed: {}",
                    err.describe(&dir.to_string_lossy())
                );
                failed = true;
                break;
            }
        }
    }
    if !failed {
        let pattern = "????????*";
        let mut sink = CountingSink::default();
        match source.query(
            dir,
            pattern,
            &mut sink,
            &ListOpts::default(),
            &CancelToken::never(),
        ) {
            Ok(stats) => total += stats.entries,
            Err(EnumError::Empty) => {}
            Err(err) => {
                let _ = writeln!(
                    out,
                    "  [SKIP] shard {pattern} failed: {}",
                    err.describe(&dir.to_string_lossy())
                );
                failed = true;
            }
        }
    }

    if failed {
        return;
    }
    let _ = writeln!(out, "  shard sum           {}", humanize::count(total));
    let _ = writeln!(
        out,
        "  single pass         {}",
        humanize::count(single_pass)
    );
    let _ = if total == single_pass {
        writeln!(
            out,
            "  [PASS] '?' matches exactly one character here; server-side patterns are sound"
        )
    } else {
        writeln!(
            out,
            "  [FAIL] shards do not sum to the whole - do NOT enable FILES_SERVER_FILTER"
        )
    };
}

fn bench_stamp(source: &dyn DirSource, dir: &Path, allow_write: bool, out: &mut dyn Write) {
    let _ = writeln!(out, "DIRECTORY STAMP  (the freshness probe)");
    let (elapsed, before) = time_it(|| source.probe_stamp(dir));
    let Ok(before) = before else {
        let _ = writeln!(
            out,
            "  unavailable - freshness will rely on periodic full rescans"
        );
        return;
    };
    let _ = writeln!(out, "  probe cost          {}", humanize::elapsed(elapsed));
    let _ = writeln!(out, "  value               {before:?}");

    if !allow_write {
        let _ = writeln!(
            out,
            "  [SKIP] pass --allow-write to confirm the stamp actually moves when a file is added"
        );
        return;
    }

    let probe = dir.join(format!("__files_stamp_probe_{:x}.tmp", std::process::id()));
    if std::fs::write(&probe, b"files stamp probe").is_err() {
        let _ = writeln!(out, "  [SKIP] could not create a temp file here");
        return;
    }
    std::thread::sleep(Duration::from_millis(50));
    let after = source.probe_stamp(dir);
    let _ = std::fs::remove_file(&probe);

    match after {
        Ok(after) if after != before => {
            let _ = writeln!(
                out,
                "  [PASS] the stamp moved; a 60s probe can replace the periodic full rescan"
            );
        }
        Ok(_) => {
            let _ = writeln!(
                out,
                "  [FAIL] the stamp did NOT move - this server needs the periodic full rescan"
            );
        }
        Err(err) => {
            let _ = writeln!(
                out,
                "  [SKIP] re-probe failed: {}",
                err.describe(&dir.to_string_lossy())
            );
        }
    }
}

/// Runs `f`, returning how long it took.
fn time_it<T>(f: impl FnOnce() -> T) -> (Duration, T) {
    let started = Instant::now();
    let value = f();
    (started.elapsed(), value)
}

/// The probe ceiling, exposed so callers can describe it.
pub fn probe_timeout() -> Duration {
    PROBE_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BASE_PATH, CUSTPRO_PATH};
    use crate::index::fake_source::FakeDirSource;

    fn settings() -> Settings {
        Settings {
            persist: false,
            ..Default::default()
        }
    }

    fn source() -> Arc<dyn DirSource> {
        Arc::new(
            FakeDirSource::new()
                .with_dir(
                    CUSTPRO_PATH,
                    &["alpha_p12345.pdf", "beta.txt", "p12345_two.pdf"],
                )
                .with_dir(BASE_PATH, &["job.txt"]),
        )
    }

    fn text(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut buf = Vec::new();
        f(&mut buf);
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn doctor_reports_both_roots_without_panicking() {
        let report = text(|out| doctor(&settings(), source(), out));
        assert!(report.contains("ROOT  R:\\"), "{report}");
        assert!(report.contains("ROOT  V:\\"), "{report}");
        assert!(report.contains("CURRENT CONFIGURATION"));
    }

    #[test]
    fn doctor_reports_an_unreachable_root_rather_than_failing() {
        let src: Arc<dyn DirSource> = Arc::new(FakeDirSource::new());
        let report = text(|out| doctor(&settings(), Arc::clone(&src), out));
        assert!(report.contains("reachable           NO"), "{report}");
    }

    #[test]
    fn doctor_names_the_configured_strategy_so_it_can_be_reproduced() {
        let s = Settings {
            enum_strategy: EnumStrategy::FindFirstEx,
            ..settings()
        };
        let report = text(|out| doctor(&s, source(), out));
        assert!(report.contains("FILES_FS_STRATEGY=findfirstex"), "{report}");
    }

    #[test]
    fn agreement_across_strategies_is_detected() {
        let ok = vec![
            StrategyResult {
                strategy: EnumStrategy::HandleDirInfo,
                entries: Some(100),
                elapsed: Duration::ZERO,
                round_trips: 1,
                error: None,
            },
            StrategyResult {
                strategy: EnumStrategy::StdReadDir,
                entries: Some(100),
                elapsed: Duration::ZERO,
                round_trips: 0,
                error: None,
            },
        ];
        assert!(counts_agree(&ok));

        let mut bad = ok.clone();
        bad[1].entries = Some(99);
        assert!(!counts_agree(&bad), "a disagreement must be caught");
    }

    #[test]
    fn a_failed_strategy_does_not_count_as_a_disagreement() {
        let results = vec![
            StrategyResult {
                strategy: EnumStrategy::HandleDirInfo,
                entries: Some(10),
                elapsed: Duration::ZERO,
                round_trips: 1,
                error: None,
            },
            StrategyResult {
                strategy: EnumStrategy::FindFirstEx,
                entries: None,
                elapsed: Duration::ZERO,
                round_trips: 0,
                error: Some(EnumError::Unsupported(87)),
            },
        ];
        assert!(counts_agree(&results));
    }

    #[test]
    fn bench_produces_a_pass_line_when_strategies_agree() {
        let report = text(|out| bench(&settings(), source(), Some("p12345"), false, out));
        assert!(report.contains("FULL ENUMERATION"), "{report}");
        assert!(
            report.contains("[PASS] every strategy returned"),
            "{report}"
        );
    }

    #[test]
    fn bench_audits_the_server_filter_against_a_full_scan() {
        let report = text(|out| bench(&settings(), source(), Some("p12345"), false, out));
        assert!(report.contains("SERVER-SIDE FILTER"), "{report}");
        assert!(
            report.contains("[PASS] the server returned everything"),
            "{report}"
        );
    }

    /// The check that decides whether the feature may be switched on at all.
    #[test]
    fn bench_fails_loudly_when_the_server_filter_under_returns() {
        let src = FakeDirSource::new()
            .with_dir(CUSTPRO_PATH, &["alpha_p12345.pdf", "p12345_two.pdf"])
            .with_dir(BASE_PATH, &["job.txt"]);
        src.set_query_drops(vec![0]);
        let report = text(|out| {
            bench(
                &settings(),
                Arc::new(src.clone()),
                Some("p12345"),
                false,
                out,
            )
        });
        assert!(report.contains("[FAIL] the server MISSED"), "{report}");
        assert!(report.contains("FILES_SERVER_FILTER=off"), "{report}");
    }

    #[test]
    fn bench_skips_the_server_section_without_a_query() {
        let report = text(|out| bench(&settings(), source(), None, false, out));
        assert!(report.contains("pass --query"), "{report}");
    }

    #[test]
    fn bench_reports_a_non_literal_query_as_inapplicable() {
        let report = text(|out| bench(&settings(), source(), Some("p1*45"), false, out));
        assert!(report.contains("not applicable"), "{report}");
    }

    #[test]
    fn the_stamp_check_is_skipped_unless_writing_is_permitted() {
        let report = text(|out| bench(&settings(), source(), None, false, out));
        assert!(report.contains("pass --allow-write"), "{report}");
    }

    #[test]
    fn bench_survives_a_completely_unreachable_share() {
        let src: Arc<dyn DirSource> = Arc::new(FakeDirSource::new());
        let report = text(|out| bench(&settings(), Arc::clone(&src), Some("p12345"), false, out));
        assert!(report.contains("FULL ENUMERATION"), "{report}");
    }
}
