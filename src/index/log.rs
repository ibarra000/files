//! The index decision log.
//!
//! Off by default; enabled with `--index-log <PATH>` or `FILES_INDEX_LOG`.
//!
//! It exists because the bug that produced it - the share being re-enumerated
//! every minute instead of every hour - is invisible from the outside. The
//! status line shows *that* the index is rebuilding, and by the time anybody
//! looks it has finished. One line per scheduler decision turns "it reloads at
//! random" into a sequence with timings and reasons, on the machine that has
//! the problem, without a debugger or a rebuild.
//!
//! Deliberately not a logging framework: a `log`/`tracing` backend would mean
//! a global initialiser, a dependency, and an output format nobody here
//! controls, for one writer on one thread.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::schedule::{Counters, ScanReason, StampHealth, Step};
use crate::util::humanize;

/// Past this the log is truncated and starts again, so leaving it enabled for
/// a week cannot fill the disk.
const MAX_BYTES: u64 = 4 << 20;

/// One decision, as the log wants it.
#[derive(Debug)]
pub struct Record<'a> {
    /// What produced this decision: `start`, `timer`, `f5`, or the result
    /// being folded back in (`probe`, `scan`).
    pub event: &'a str,
    /// Free-form specifics: the stamp comparison, an error, an entry count.
    pub detail: &'a str,
    pub step: Step,
    /// How long the actor will wait, for a terminal step.
    pub wait: Option<Duration>,
    pub counters: Counters,
    pub stamp_health: StampHealth,
}

/// An append-only decision log, or nothing at all.
pub struct IndexLog {
    file: Option<Mutex<File>>,
    started: Instant,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for IndexLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexLog")
            .field("path", &self.path)
            .field("enabled", &self.file.is_some())
            .finish()
    }
}

impl IndexLog {
    pub fn disabled() -> Self {
        Self {
            file: None,
            started: Instant::now(),
            path: None,
        }
    }

    /// Opens `path` for appending.
    ///
    /// A log that cannot be opened is not worth stopping for - the
    /// application's job is finding files - so it degrades to disabled and
    /// says so through [`IndexLog::is_enabled`], which `--doctor` reports.
    pub fn to_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(parent);
        }
        let opened = OpenOptions::new().create(true).append(true).open(&path);
        Self {
            file: opened.ok().map(Mutex::new),
            started: Instant::now(),
            path: Some(path),
        }
    }

    /// Enabled when a path was configured, disabled otherwise.
    ///
    /// Precedence between the flag and `FILES_INDEX_LOG` is resolved in
    /// [`crate::config`] along with every other setting, rather than a second
    /// time here.
    pub fn from_option(path: Option<&Path>) -> Self {
        match path {
            Some(p) => Self::to_path(p),
            None => Self::disabled(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.file.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Appends one line. Best effort throughout: a failed write costs a line
    /// of diagnostics and nothing else.
    pub fn record(&self, record: &Record<'_>) {
        let Some(file) = &self.file else {
            return;
        };
        let line = format_record(self.started.elapsed(), record);
        let mut guard = file.lock();
        // Truncating rather than rotating: there is one writer, the
        // interesting part is always the recent part, and a second file to
        // reason about is not worth it.
        if guard.metadata().map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
            let _ = guard.set_len(0);
            let _ = guard.write_all(b"-- truncated --\n");
        }
        let _ = guard.write_all(line.as_bytes());
        let _ = guard.flush();
    }
}

/// Renders one record. Separate and pure so the format can be asserted.
///
/// Elapsed-since-start rather than a wall clock: the question the log answers
/// is "how far apart were the rebuilds", and that reads straight off the
/// left-hand column without any date arithmetic.
pub fn format_record(elapsed: Duration, r: &Record<'_>) -> String {
    let mut s = String::with_capacity(192);
    let _ = write!(s, "[+{:>9.3}s] {:<6}", elapsed.as_secs_f64(), r.event);
    let _ = write!(s, " step={:<13}", step_name(r.step));
    match r.step.scan_reason() {
        Some(reason) => {
            let _ = write!(s, " reason={:<19}", reason_name(reason));
        }
        None => {
            let _ = write!(s, " {:27}", "");
        }
    }
    match r.wait {
        Some(d) => {
            let _ = write!(s, " wait={:<8}", humanize::elapsed(d));
        }
        None => {
            let _ = write!(s, " {:13}", "");
        }
    }
    let c = r.counters;
    let _ = write!(
        s,
        " scans={} probes={} probe_fail={} scan_fail={} moves={} deferred={} stamp={}",
        c.full_scans,
        c.probes,
        c.probe_failures,
        c.scan_failures,
        c.stamp_moves,
        c.deferred_scans,
        r.stamp_health.label()
    );
    if !r.detail.is_empty() {
        let _ = write!(s, " | {}", r.detail);
    }
    s.push('\n');
    s
}

fn step_name(step: Step) -> &'static str {
    match step {
        Step::Probe => "probe",
        Step::FullScan(_) => "full-scan",
        Step::ConfirmFresh => "confirm-fresh",
        Step::Wait => "wait",
    }
}

fn reason_name(reason: ScanReason) -> &'static str {
    match reason {
        ScanReason::FirstRun => "first-run",
        ScanReason::Forced => "f5",
        ScanReason::StampMoved => "directory-changed",
        ScanReason::Floor => "periodic-refresh",
        ScanReason::Blind => "no-change-detection",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record<'a>(step: Step, detail: &'a str) -> Record<'a> {
        Record {
            event: "timer",
            detail,
            step,
            wait: Some(Duration::from_secs(58)),
            counters: Counters {
                full_scans: 2,
                probes: 17,
                ..Counters::default()
            },
            stamp_health: StampHealth::Working,
        }
    }

    #[test]
    fn a_disabled_log_writes_nothing_and_says_so() {
        let log = IndexLog::disabled();
        assert!(!log.is_enabled());
        log.record(&record(Step::Wait, ""));
    }

    #[test]
    fn a_scan_line_names_its_reason() {
        let line = format_record(
            Duration::from_millis(1234),
            &record(Step::FullScan(ScanReason::StampMoved), "stamp 1,1 -> 2,2"),
        );
        assert!(line.contains("step=full-scan"), "{line}");
        assert!(
            line.contains("reason=directory-changed"),
            "the whole point of the log: {line}"
        );
        assert!(line.contains("stamp 1,1 -> 2,2"), "{line}");
        assert!(line.contains("[+    1.234s]"), "{line}");
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn a_non_scan_line_omits_the_reason_field() {
        let line = format_record(Duration::ZERO, &record(Step::ConfirmFresh, ""));
        assert!(line.contains("step=confirm-fresh"), "{line}");
        assert!(!line.contains("reason="), "{line}");
    }

    #[test]
    fn every_counter_is_present_so_the_log_is_self_contained() {
        let line = format_record(Duration::ZERO, &record(Step::Wait, ""));
        for field in [
            "scans=",
            "probes=",
            "probe_fail=",
            "scan_fail=",
            "moves=",
            "deferred=",
            "stamp=",
        ] {
            assert!(line.contains(field), "missing {field} in {line}");
        }
    }

    #[test]
    fn writing_to_a_file_appends_one_line_per_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("index.log");
        let log = IndexLog::to_path(&path);
        assert!(log.is_enabled(), "a missing parent should be created");

        log.record(&record(Step::FullScan(ScanReason::FirstRun), ""));
        log.record(&record(Step::ConfirmFresh, ""));

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains("reason=first-run"), "{text}");
    }

    #[test]
    fn a_log_that_cannot_be_opened_degrades_to_disabled() {
        let dir = tempfile::tempdir().unwrap();
        // A directory is never openable as a file for appending.
        let log = IndexLog::to_path(dir.path());
        assert!(!log.is_enabled());
        assert!(log.path().is_some(), "the intent is still reportable");
        log.record(&record(Step::Wait, ""));
    }

    #[test]
    fn no_configured_path_means_no_log() {
        assert!(!IndexLog::from_option(None).is_enabled());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("configured.log");
        let log = IndexLog::from_option(Some(&path));
        assert!(log.is_enabled());
        assert_eq!(log.path(), Some(path.as_path()));
    }

    #[test]
    fn every_step_and_reason_has_a_log_name() {
        for step in [
            Step::Probe,
            Step::ConfirmFresh,
            Step::Wait,
            Step::FullScan(ScanReason::Floor),
        ] {
            assert!(!step_name(step).is_empty());
        }
        for reason in [
            ScanReason::FirstRun,
            ScanReason::Forced,
            ScanReason::StampMoved,
            ScanReason::Floor,
            ScanReason::Blind,
        ] {
            assert!(!reason_name(reason).contains(' '), "log fields are tokens");
        }
    }
}
