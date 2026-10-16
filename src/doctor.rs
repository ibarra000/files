//! `--doctor` and `--bench`: on-site diagnostics for share access.
//!
//! * **`--doctor`** is fast and read-only: drive type, UNC target, volume
//!   serial, filesystem flags, SMB dialect, round-trip time, and which
//!   enumeration strategies are usable.
//! * **`--bench`** times every enumeration strategy against one directory and
//!   checks that they all return the same entry count. A disagreement means a
//!   fast path must stay off.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{EnumStrategy, MAX_SERVER_HITS, Settings};
use crate::index::actor::scan_once;
use crate::index::enumerate::{CountingSink, DirSource, ListOpts, VecSink};
use crate::index::errors::EnumError;
use crate::index::persist;
use crate::index::walk::{WalkCounts, WalkOpts, WalkReport, walk_tree};
use crate::paths::{Mapping, MappingKind};
use crate::search::pattern;
use crate::util::cancel::CancelToken;
use crate::util::humanize;
use crate::util::rng::Rng;

/// Overall time budget for a `--doctor` report.
///
/// Applied through [`within`]: a blocked SMB call cannot be cancelled, so the
/// probe thread is abandoned past the deadline and the report says so.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Number of uncached lookups averaged for the RTT estimate.
///
/// Each is a real round trip, so the count is kept low; 16 samples give a
/// median within a millisecond or two of 32.
const RTT_SAMPLES: usize = 16;

/// Runs `work` on its own thread and stops waiting after `budget`.
///
/// A blocked SMB call cannot be interrupted, so on timeout the thread is
/// leaked. It exits once the redirector fails the call, usually within a
/// minute.
fn within<T: Send + 'static>(
    budget: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    if std::thread::Builder::new()
        .name("files-doctor-probe".to_owned())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .is_err()
    {
        return None;
    }
    rx.recv_timeout(budget).ok()
}

/// The remaining time in the report's single `PROBE_TIMEOUT` budget.
///
/// Shared across sections so the whole report, not each section, is bounded.
#[derive(Debug, Clone, Copy)]
pub struct Deadline(Instant);

impl Deadline {
    /// Starts a budget of `PROBE_TIMEOUT` from now.
    pub fn new() -> Self {
        Self(Instant::now() + PROBE_TIMEOUT)
    }

    /// Returns the remaining time, or `None` once expired.
    pub fn left(self) -> Option<Duration> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
    }

    /// Returns whether any time remains.
    pub fn open(self) -> bool {
        self.left().is_some()
    }
}

impl Default for Deadline {
    fn default() -> Self {
        Self::new()
    }
}

/// Prints the resolved routing table and where a search would be sent.
///
/// Makes no network calls, so it works on a machine with no drives mapped.
pub fn check_config(settings: &Settings, query: Option<&str>, out: &mut dyn Write) {
    let routes = &settings.routes;
    let _ = writeln!(out, "config: {}", routes.source().describe());
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {:<3} {:<14} {:<11} {:<8} {:<10} path",
        "#", "mapping", "kind", "enabled", "read"
    );
    for m in routes.all() {
        // Indexed shares show their refresh policy; live shares show search depth.
        let read = if m.kind.is_live() {
            format!("depth {}", m.depth)
        } else {
            m.refresh.label().to_string()
        };
        let _ = writeln!(
            out,
            "  {:<3} {:<14} {:<11} {:<8} {:<10} {}",
            m.id.index(),
            m.name,
            m.kind.label(),
            if m.enabled { "yes" } else { "no" },
            read,
            m.path.display(),
        );
    }

    // Every enabled share is searched, so list them in order.
    if let Some(term) = query {
        let _ = writeln!(out);
        let targets = routes.targets();
        if targets.is_empty() {
            let _ = writeln!(out, "search {term:?}: no share is enabled");
        } else {
            let _ = writeln!(out, "search {term:?}: every enabled share, in order");
            for t in &targets {
                let _ = writeln!(
                    out,
                    "  {:<14} {:<11} {:<24} {}",
                    routes.label(t.mapping),
                    t.kind.label(),
                    t.dir.display(),
                    if t.kind.is_live() {
                        "asked per search"
                    } else {
                        "indexed"
                    }
                );
            }
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "VIEWER  {}{}",
        settings.viewer.name(&settings.viewers),
        if settings.can_save(crate::config::write::SettingKey::Viewer) {
            ""
        } else {
            "  (F2 applies for the session only)"
        }
    );

    // Hidden files are omitted from results entirely, so report the filter.
    let _ = writeln!(out);
    let hidden = &settings.hidden;
    if hidden.is_empty() {
        let _ = writeln!(out, "HIDDEN  nothing is hidden by extension");
    } else {
        let list: Vec<&str> = hidden.suffixes().collect();
        let _ = writeln!(out, "HIDDEN  {}", list.join(" "));
    }
    let _ = writeln!(
        out,
        "        system and hidden files: {}{}",
        if hidden.hides_system() {
            "hidden"
        } else {
            "shown"
        },
        if hidden.hides_system() {
            "  (applies to a drive from its next scan)"
        } else {
            ""
        }
    );

    // Repeated here because the panel only shows it briefly as a toast.
    if let Some(migrated) = &settings.migrated {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "UPDATED brought forward from version {}",
            migrated.from
        );
        let _ = writeln!(
            out,
            "        the original is at {}",
            migrated.backup.display()
        );
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "OK");
}

/// Fast, read-only capability report.
pub fn doctor(
    settings: &Settings,
    source: Arc<dyn DirSource>,
    hotkey: Option<Result<(), String>>,
    out: &mut dyn Write,
) {
    let _ = writeln!(out, "files {} - diagnostics", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "source: {}", source.name());
    let _ = writeln!(out);

    // One budget for the whole report, not one per section.
    let deadline = Deadline::new();

    // Every enabled mapping, not only the derived flat and tree roots,
    // which may be empty.
    for mapping in settings.routes.enabled() {
        report_root(&mapping.path, &source, deadline, out);
        let _ = writeln!(out);
    }

    report_index_cache(settings, out);
    let _ = writeln!(out);
    report_live_shares(settings, out);
    report_live_updates(settings, deadline, out);
    let _ = writeln!(out);
    report_quick_search(settings, hotkey, out);
    let _ = writeln!(out);
    report_viewer(settings, out);
    report_directories(settings, out);
    report_updates(settings, out);
    report_recommendations(settings, out);
}

/// Reports the global hotkey and whether its target window can be found.
///
/// Both failure modes are silent at run time: a chord owned by another
/// program never fires, and under Windows Terminal the obvious console window
/// is a hidden pseudo-console that ignores move requests.
fn report_quick_search(
    settings: &Settings,
    hotkey: Option<Result<(), String>>,
    out: &mut dyn Write,
) {
    let _ = writeln!(out, "QUICK SEARCH");
    let probe = crate::hotkey::probe(settings.hotkey, hotkey);

    if !probe.supported {
        let _ = writeln!(out, "  unavailable on this platform");
        return;
    }
    match &probe.chord {
        Some(chord) => {
            let _ = writeln!(out, "  hotkey:    {chord}");
        }
        None => {
            let _ = writeln!(out, "  hotkey:    off");
            return;
        }
    }
    // Report whether a remembered drag position overrides default placement.
    match settings
        .placement_path
        .as_deref()
        .and_then(crate::placement::load)
    {
        Some((left, top)) => {
            let _ = writeln!(out, "  position:  remembered, at {left},{top}");
        }
        None => {
            let _ = writeln!(out, "  position:  placed by the program");
        }
    }
    match &probe.registered {
        Some(Ok(())) => {
            let _ = writeln!(out, "  claim:     accepted");
        }
        Some(Err(why)) => {
            let _ = writeln!(out, "  claim:     refused - {why}");
        }
        None => {}
    }
    match &probe.window {
        // Naming the window distinguishes working from moving another program.
        Some(Ok(how)) => {
            let _ = writeln!(out, "  window:    found, as {how}");
        }
        Some(Err(why)) => {
            let _ = writeln!(out, "  window:    not found - {why}");
            let _ = writeln!(
                out,
                "             the hotkey will still switch to the compact view,"
            );
            let _ = writeln!(out, "             but nothing will be moved or focused");
        }
        None => {}
    }
}

/// Reports whether each tree root accepts a subtree change watch.
///
/// A server can accept `CHANGE_NOTIFY` and never fire it, which is
/// indistinguishable at run time from a quiet share.
fn report_live_updates(settings: &Settings, deadline: Deadline, out: &mut dyn Write) {
    let _ = writeln!(out, "LIVE UPDATES");
    let trees: Vec<_> = settings
        .routes
        .enabled()
        .filter(|m| m.kind == MappingKind::Tree && !m.path.as_os_str().is_empty())
        .collect();
    if trees.is_empty() {
        let _ = writeln!(out, "  no tree mapping configured");
        return;
    }
    for m in trees {
        report_one_watch(settings, m, deadline, out);
    }
}

/// Checks one tree; each may be on a different server.
fn report_one_watch(settings: &Settings, m: &Mapping, deadline: Deadline, out: &mut dyn Write) {
    let _ = writeln!(
        out,
        "  {} ({})  refresh = {}",
        m.name,
        m.path.display(),
        m.refresh.label()
    );
    if !settings.live_updates {
        let _ = writeln!(
            out,
            "    disabled by configuration; the {} rescan floor is the whole guarantee",
            crate::util::humanize::elapsed(crate::config::TREE_RESCAN_FLOOR)
        );
        return;
    }
    #[cfg(windows)]
    {
        // On a scratch thread: opening a watch on a vanished share blocks in the
        // kernel.
        let opened = {
            let asked = m.path.clone();
            deadline.left().and_then(|left| {
                within(left, move || {
                    crate::index::win_watch::DirectoryWatcher::open(&asked)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
            })
        };
        let Some(opened) = opened else {
            let _ = writeln!(
                out,
                "    watch:   GAVE UP - the share did not answer in time"
            );
            return;
        };
        match opened {
            Ok(()) => {
                let _ = writeln!(out, "    watch:   accepted");
                let _ = writeln!(
                    out,
                    "    note:    a share can accept the request and never fire it, and that failure has no symptom. {}",
                    fallback(m)
                );
            }
            Err(why) => {
                let _ = writeln!(out, "    watch:   unavailable - {why}");
                let _ = writeln!(out, "    effect:  {}", fallback(m));
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = writeln!(out, "    watch:   unavailable on this platform");
        let _ = writeln!(out, "    effect:  {}", fallback(m));
    }
}

/// Describes what keeps the share current when the watcher does not.
///
/// Depends on the refresh policy, so the text must match it.
fn fallback(m: &Mapping) -> String {
    if m.refresh.is_manual() {
        return "no timed re-read; F5 updates it, and the status line shows \
                when it was last read"
            .into();
    }
    format!(
        "new files appear within the {} re-read rather than in seconds",
        crate::util::humanize::elapsed(crate::config::TREE_RESCAN_FLOOR)
    )
}

fn report_root(root: &Path, source: &Arc<dyn DirSource>, deadline: Deadline, out: &mut dyn Write) {
    let _ = writeln!(out, "ROOT  {}", root.display());

    // Queried once and used twice: by the block below and by the RTT estimate,
    // which runs only for network shares. Runs on a scratch thread because
    // `volume_info` blocks on a dead share; see [`within`].
    #[cfg(windows)]
    let volume = {
        let asked = root.to_path_buf();
        match deadline
            .left()
            .and_then(|left| within(left, move || crate::index::volume::volume_info(&asked)))
        {
            Some(info) => info,
            None => {
                let _ = writeln!(out, "  GAVE UP - the share did not answer in time");
                return;
            }
        }
    };

    #[cfg(windows)]
    {
        let info = &volume;
        // Volume queries accept only a root, so name the root that was queried.
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
        // Informational only: NTFS always sets this flag. Pattern-matching
        // agreement is decided by the superset check below.
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

    // Reachability, twice: the first touch pays for session setup. Both run on
    // a scratch thread under the same budget, since this can block on a dead
    // share.
    let probed = {
        let asked = root.to_path_buf();
        let source = Arc::clone(source);
        deadline.left().and_then(move |left| {
            within(left, move || {
                let first = time_it(|| source.probe_stamp(&asked));
                let second = time_it(|| source.probe_stamp(&asked));
                (first, second)
            })
        })
    };
    let Some((first, second)) = probed else {
        let _ = writeln!(out, "  GAVE UP - the share did not answer in time");
        return;
    };
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

    // Skipped on local disks, where the figure is a cache miss, not an RTT.
    let remote = {
        #[cfg(windows)]
        {
            volume.remote_protocol.is_some() || volume.unc_target.is_some()
        }
        #[cfg(not(windows))]
        {
            true
        }
    };
    let rtt = {
        let asked = root.to_path_buf();
        remote
            .then(|| deadline.left())
            .flatten()
            .and_then(|left| within(left, move || measure_rtt(&asked)))
            .flatten()
    };
    if let Some((median, p95)) = rtt {
        let _ = writeln!(
            out,
            "  est. RTT            median {}, p95 {}",
            humanize::elapsed(median),
            humanize::elapsed(p95)
        );
    }
}

/// Times a metadata lookup on names that do not exist.
///
/// Each iteration uses a fresh random name because the redirector caches
/// negative lookups for about five seconds.
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
    // Every indexed mapping, since each has its own cache entry keyed by path
    // hash.
    for m in settings
        .routes
        .enabled()
        .filter(|m| m.kind.is_indexed() && !m.path.as_os_str().is_empty())
    {
        report_one_cache(dir, m, out);
    }

    let _ = writeln!(
        out,
        "  decision log        {}",
        match &settings.index_log {
            Some(p) => p.display().to_string(),
            None => "off (pass --index-log <PATH> to record why it reindexes)".into(),
        }
    );
}

/// Reports what past searches have cached for each live (unindexed) share.
///
/// Prints nothing when no live share is configured.
fn report_live_shares(settings: &Settings, out: &mut dyn Write) {
    if settings.routes.live().next().is_none() {
        return;
    }
    let _ = writeln!(out, "LIVE SHARES");
    for m in settings.routes.live() {
        let _ = writeln!(out, "  {} ({})", m.name, m.path.display());
        let _ = writeln!(
            out,
            "    read                never - every search asks the drive instead"
        );
        let _ = writeln!(
            out,
            "    depth               {}  ({})",
            m.depth,
            if m.depth == 1 {
                "the share's own folder, plus any folder whose name matches"
            } else {
                "every folder on the way down is listed first"
            }
        );
        let _ = writeln!(
            out,
            "    remembered          {}",
            remembered(settings, m).unwrap_or_else(|| "nothing yet".into())
        );
    }
    let _ = writeln!(out);
}

/// Describes how much of a live share past searches have cached, if any.
fn remembered(settings: &Settings, m: &Mapping) -> Option<String> {
    let dir = settings.cache_dir.as_deref()?;
    if !settings.persist {
        return Some("not kept - persistence is disabled".into());
    }
    let key = persist::MappingKey::of(&m.path);
    // The volume serial is not resolved, so the share is not touched.
    let loaded = persist::load_tree(dir, key, persist::Expect::new(&m.path, None)).ok()?;
    Some(format!(
        "{} files in {} folders, from earlier searches",
        loaded.index.len(),
        loaded.index.dir_count()
    ))
}

fn report_one_cache(dir: &std::path::Path, m: &Mapping, out: &mut dyn Write) {
    let indexed = m.path.as_path();
    let key = persist::MappingKey::of(indexed);
    let _ = writeln!(out, "  {} ({})", m.name, indexed.display());
    let _ = writeln!(out, "    cache key         {}", key.hex());

    // Validated against the real volume serial, as the application does, so the
    // report matches what a launch will accept.
    let serial = volume_serial_of(indexed);
    let _ = writeln!(
        out,
        "    volume serial     {}",
        match serial {
            Some(v) => format!("{v:08X}"),
            None => "unknown (the identity check will be skipped)".into(),
        }
    );

    match persist::load(dir, key, persist::Expect::new(indexed, serial)) {
        Ok(snapshot) => {
            let _ = writeln!(
                out,
                "    cached entries    {}",
                humanize::count(snapshot.len())
            );
            let age = std::time::SystemTime::now()
                .duration_since(snapshot.captured_at())
                .unwrap_or_default();
            let _ = writeln!(out, "    age               {}", humanize::age(age));
            let _ = writeln!(
                out,
                "    resident size     {}",
                humanize::bytes(snapshot.memory_bytes() as u64)
            );
        }
        Err(err) => {
            let _ = writeln!(out, "    cached index      {err}");
        }
    }
}

/// Returns a mapping root's volume serial, or `None` off Windows or on
/// failure.
fn volume_serial_of(dir: &std::path::Path) -> Option<u32> {
    #[cfg(windows)]
    {
        crate::index::volume::volume_serial(dir)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        None
    }
}

/// Reports how opening a result is handled: viewers, open mode and merge folder.
fn report_viewer(settings: &Settings, out: &mut dyn Write) {
    let _ = writeln!(out, "VIEWER");
    let _ = writeln!(
        out,
        "  viewer              {} ({})",
        settings.viewer.name(&settings.viewers),
        match settings.viewer.within(&settings.viewers) {
            crate::config::ViewerKind::Auto => "each file opens in its default app",
            crate::config::ViewerKind::Pdf => "a base name's pages, combined into one PDF",
            crate::config::ViewerKind::App(_) => "the selected file, opened with that program",
        }
    );

    // One line per configured viewer. None is built in.
    if settings.viewers.is_empty() {
        let _ = writeln!(out, "  viewers             (none configured)");
    }
    let missing = crate::open::probe_viewers(&settings.viewers);
    for (i, (app, &gone)) in settings
        .viewers
        .all()
        .iter()
        .zip(missing.iter())
        .enumerate()
    {
        let in_use = settings.viewer.within(&settings.viewers) == crate::config::ViewerKind::App(i);
        let found = if !gone {
            "found"
        } else if in_use {
            "NOT FOUND - Enter will fail"
        } else {
            // Not an error unless F2 mode is active.
            "NOT FOUND (not in use - F2 would need it)"
        };
        let _ = writeln!(out, "  {:<19} {} \u{b7} {found}", app.name, app.program);
        if !app.extensions.is_empty() {
            let _ = writeln!(
                out,
                "  {:<19} takes {} in pdf mode",
                "",
                app.extensions.join(", ")
            );
        }
    }

    let _ = writeln!(
        out,
        "  pdf target          {}",
        match &settings.pdf_viewer {
            Some(p) => p.display().to_string(),
            None => "the system's .pdf association".into(),
        }
    );

    let _ = writeln!(
        out,
        "  read-only           {}",
        if settings.pdf_read_only {
            "yes - an assembled document is handed over read-only"
        } else {
            "no - an assembled document can be saved over (pdf_read_only = false)"
        }
    );

    // Merged documents are written here and stay open in the viewer.
    let _ = writeln!(
        out,
        "  merged documents    {}",
        match &settings.cache_dir {
            Some(dir) => {
                let pdf_dir = dir.join("pdf");
                let (count, bytes, capped) = directory_size(&pdf_dir);
                format!(
                    "{} ({}{} file{}, {}{})",
                    pdf_dir.display(),
                    count,
                    if capped { "+" } else { "" },
                    if count == 1 { "" } else { "s" },
                    if capped { "at least " } else { "" },
                    humanize::bytes(bytes)
                )
            }
            None => "nowhere - documents will not be merged".into(),
        }
    );

    // Explain which of the three reasons prevents F2 from persisting.
    if let Some(pin) = settings.pin(crate::config::write::SettingKey::Viewer) {
        let reason = match pin {
            crate::config::Pin::Environment(var) => format!("{var} outranks the file"),
            crate::config::Pin::CommandLine => "--viewer outranks the file".to_string(),
            crate::config::Pin::NoFile => "there is no configuration file".to_string(),
        };
        let _ = writeln!(out, "  F2                  session only - {reason}");
    }
    let _ = writeln!(out);
}

/// Maximum files [`directory_size`] counts before stopping.
///
/// A cache directory normally holds tens of files; past this the count is
/// printed with a `+`.
const SIZE_CAP: usize = 5_000;

/// Counts a directory's files and total size, up to [`SIZE_CAP`].
///
/// Best effort. The `bool` is whether counting stopped early.
fn directory_size(dir: &Path) -> (usize, u64, bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0, false);
    };
    let mut count = 0usize;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        count += 1;
        bytes += meta.len();
        if count >= SIZE_CAP {
            return (count, bytes, true);
        }
    }
    (count, bytes, false)
}

/// Reports whether a newer version is published and reachable.
///
/// Silent when no update folder is configured.
fn report_updates(settings: &Settings, out: &mut dyn Write) {
    let Some(source) = crate::update::Source::of(settings) else {
        return;
    };
    let _ = writeln!(out, "UPDATES");
    let _ = writeln!(out, "  looking in          {}", source.describe());
    let _ = writeln!(
        out,
        "  running             {}",
        crate::update::Version::current()
    );

    match crate::update::look_at(
        &source,
        crate::update::Version::current(),
        settings.cache_dir.as_deref(),
    ) {
        crate::update::Found::Available { manifest, msi, .. } => {
            let _ = writeln!(out, "  available           {}", manifest.version);
            let _ = writeln!(out, "  installer           {}", msi.display());
            let _ = writeln!(
                out,
                "  installer found     {}",
                if msi.is_file() {
                    "yes"
                } else {
                    "NO - the manifest names a file that is not there"
                }
            );
            if let Some(notes) = &manifest.notes {
                let _ = writeln!(out, "  notes               {notes}");
            }
        }
        crate::update::Found::UpToDate => {
            let _ = writeln!(out, "  available           nothing newer");
        }
        crate::update::Found::Unavailable { detail } => {
            let _ = writeln!(out, "  available           unknown - {detail}");
        }
    }
    let _ = writeln!(out);
}

/// Reports whether each directory this program writes to exists.
///
/// Each is created on demand, so a missing or unwritable one (for example a
/// redirected `%APPDATA%` that is not mounted) otherwise fails one write at a
/// time in different workers.
fn report_directories(settings: &Settings, out: &mut dyn Write) {
    let _ = writeln!(out, "DIRECTORIES");
    let places = [
        (
            "settings",
            crate::config::file::default_config_path()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
        ),
        ("index cache", settings.cache_dir.clone()),
        (
            "recent searches",
            settings
                .history_path
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf),
        ),
    ];

    for (what, dir) in places {
        match dir {
            Some(dir) if dir.is_dir() => {
                let _ = writeln!(out, "  {what:<19} {}", dir.display());
            }
            Some(dir) => {
                let _ = writeln!(
                    out,
                    "  {what:<19} {}  MISSING - and could not be created",
                    dir.display()
                );
            }
            None => {
                let _ = writeln!(out, "  {what:<19} nowhere - this profile has none");
            }
        }
    }
    let _ = writeln!(out);
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
    let _ = writeln!(
        out,
        "  FILES_LIVE_UPDATES={}",
        if settings.live_updates { "on" } else { "off" }
    );
    let _ = writeln!(
        out,
        "  FILES_VIEWER={}",
        settings.viewer.name(&settings.viewers)
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Run `files-cli --bench` to compare enumeration strategies on these drives"
    );
    let _ = writeln!(
        out,
        "and confirm the server-side filter returns everything a full scan does."
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

/// Returns whether all strategies that succeeded report the same count.
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

/// Walks every enabled mapping and reports the shape of each tree.
///
/// Read-only. Walk cost is dominated by the directory count, which only a
/// walk can measure.
pub fn bench_walk(
    settings: &Settings,
    source: Arc<dyn DirSource>,
    args: crate::cli::WalkArgs,
    out: &mut dyn Write,
) {
    let mut opts = WalkOpts::default();
    if let Some(n) = args.concurrency {
        opts = opts.with_concurrency(n);
    }
    if let Some(d) = args.max_depth {
        opts = opts.with_max_depth(d);
    }

    let _ = writeln!(out, "files {} - tree walk", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        out,
        "reading {} directories at a time, to a depth of {}",
        opts.concurrency, opts.max_depth
    );
    let _ = writeln!(out, "read-only: nothing is written to the share.");

    for mapping in settings.routes.enabled() {
        let _ = writeln!(out);
        let _ = writeln!(out, "{} - {}", mapping.name, mapping.path.display());

        let sink = WalkCounts::default();
        let report = walk_tree(
            source.as_ref(),
            &mapping.path,
            &opts,
            &sink,
            &CancelToken::never(),
        );
        report_walk(&report, &sink, out);
    }
}

fn report_walk(report: &WalkReport, sink: &WalkCounts, out: &mut dyn Write) {
    use std::sync::atomic::Ordering;

    let dirs = report.dirs_visited.max(1);
    let files = report.files;
    let secs = report.elapsed.as_secs_f64().max(0.000_001);

    let _ = writeln!(
        out,
        "  directories         {:>12}",
        humanize::count(report.dirs_visited)
    );
    let _ = writeln!(out, "  files               {:>12}", humanize::count(files));
    let _ = writeln!(
        out,
        "  files per directory {:>12.1}",
        files as f64 / dirs as f64
    );
    let _ = writeln!(out, "  deepest             {:>12}", report.max_depth_seen);
    let _ = writeln!(
        out,
        "  round trips         {:>12}",
        humanize::count(report.round_trips as usize)
    );
    let _ = writeln!(
        out,
        "  elapsed             {:>12}",
        humanize::elapsed(report.elapsed)
    );
    let _ = writeln!(
        out,
        "  directories/second  {:>12.0}",
        report.dirs_visited as f64 / secs
    );

    // Estimated index memory. Directory paths are interned once, which is why
    // the directory count is reported separately.
    let name_bytes = sink.name_bytes.load(Ordering::Relaxed);
    let dir_bytes = sink.max_rel_len.load(Ordering::Relaxed) * report.dirs_visited as u64 / 2;
    let projected =
        name_bytes * 2 + dir_bytes * 2 + (files as u64 + report.dirs_visited as u64) * 8;
    let _ = writeln!(
        out,
        "  projected index     {:>12}",
        humanize::bytes(projected)
    );

    if report.skipped_reparse > 0 {
        let _ = writeln!(
            out,
            "  [NOTE] {} junction(s) not followed; anything only reachable through \
             one is not indexed",
            report.skipped_reparse
        );
    }
    if report.depth_clipped > 0 {
        let _ = writeln!(
            out,
            "  [WARN] {} subtree(s) cut off by --max-depth",
            report.depth_clipped
        );
    }
    let errors = &report.errors;
    if errors.holes() > 0 {
        let _ = writeln!(
            out,
            "  [WARN] {} unreadable: {} denied, {} transient, {} other",
            errors.holes(),
            errors.denied,
            errors.transient,
            errors.other
        );
        for (rel, err) in errors.recorded.iter().take(5) {
            let shown = if rel.is_empty() { "<root>" } else { rel };
            let _ = writeln!(out, "         {shown}: {}", err.describe(shown));
        }
    }
    // Reported without alarm: folders appear and vanish during a walk.
    if errors.vanished > 0 {
        let _ = writeln!(
            out,
            "  [NOTE] {} folder(s) were deleted while the walk was running",
            errors.vanished
        );
    }
    if let Some(err) = &report.aborted {
        let _ = writeln!(
            out,
            "  [FAIL] gave up: the share stopped answering ({})",
            err.describe("the share")
        );
    }

    let _ = writeln!(
        out,
        "  {}",
        if report.complete() {
            "[PASS] the whole tree was read"
        } else {
            "[WARN] this is a partial view of the tree"
        }
    );
}

/// Runs the full benchmark report.
pub fn bench(
    settings: &Settings,
    source: Arc<dyn DirSource>,
    query: Option<&str>,
    allow_write: bool,
    out: &mut dyn Write,
) {
    // The first enabled flat share, else the first indexed one, named in the
    // output.
    let Some(target) = settings
        .routes
        .flat()
        .next()
        .or_else(|| settings.routes.enabled().find(|m| m.kind.is_indexed()))
    else {
        let _ = writeln!(out, "no enabled mapping to benchmark");
        return;
    };
    let dir = target.path.clone();
    let _ = writeln!(out, "files {} - benchmark", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "target: {} ({})", target.name, dir.display());
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
        let _ = writeln!(out, "SERVER-SIDE FILTER  (pass --query <TEXT> to measure)");
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
    let parsed = crate::search::query::Query::parse(query);
    let wildcard = match pattern::wildcard_for(&parsed) {
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
                .filter(|n| pattern::confirms(n, &parsed))
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

            // Compare against a full local enumeration: the server must never return
            // less.
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
/// disjoint, complete partition (Win32 patterns have no character classes,
/// so first-character sharding cannot be). This is an oracle, not a speedup:
/// if the shards sum to the single-pass count, the server's pattern matching
/// drops nothing, which is what makes the server-side filter safe.
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
            "  unavailable - freshness will rely on the periodic full rescan"
        );
        let _ = writeln!(
            out,
            "  (that is {} between rescans; raise FILES_RESCAN_FLOOR to trade",
            humanize::elapsed(crate::config::FULL_RESCAN_FLOOR)
        );
        let _ = writeln!(out, "   staleness for load, or lower it for the reverse)");
        return;
    };
    let _ = writeln!(out, "  probe cost          {}", humanize::elapsed(elapsed));
    let _ = writeln!(out, "  value               {before:?}");
    let _ = writeln!(
        out,
        "  precision           {}",
        match before.kind {
            crate::index::StampKind::Full => "write + change time (handle query)",
            crate::index::StampKind::WriteOnly =>
                "write time only (attribute fallback - the handle query was refused)",
        }
    );

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

/// The probe budget, exposed so callers can describe it.
pub fn probe_timeout() -> Duration {
    PROBE_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FLAT_FIXTURE_PATH, TREE_FIXTURE_PATH};
    use crate::index::fake_source::FakeDirSource;

    fn settings() -> Settings {
        Settings {
            persist: false,
            ..Settings::fixture()
        }
    }

    fn source() -> Arc<dyn DirSource> {
        Arc::new(
            FakeDirSource::new()
                .with_dir(
                    FLAT_FIXTURE_PATH,
                    &["alpha_p12345.pdf", "beta.txt", "p12345_two.pdf"],
                )
                .with_dir(TREE_FIXTURE_PATH, &["notes.txt"]),
        )
    }

    /// The shipped configuration names no live share.
    #[test]
    fn the_live_share_section_is_absent_when_none_is_configured() {
        let report = text(|out| report_live_shares(&settings(), out));
        assert!(report.is_empty(), "got {report:?}");
    }

    #[test]
    fn a_live_share_is_reported_as_never_read_and_with_its_depth() {
        use crate::paths::{ConfigSource, Mapping, MappingId, MappingKind, RefreshPolicy, Routes};
        let routes = Routes::new(
            vec![Mapping {
                id: MappingId(0),
                name: "archive".into(),
                path: std::path::PathBuf::from(r"S:\old"),
                kind: MappingKind::Live,
                enabled: true,
                refresh: RefreshPolicy::Manual,
                depth: 2,
            }],
            ConfigSource::BuiltIn,
        );
        let s = Settings::with_routes(std::sync::Arc::new(routes), |s| Settings {
            persist: false,
            ..s
        });

        let report = text(|out| report_live_shares(&s, out));
        assert!(report.contains("LIVE SHARES"), "{report}");
        assert!(report.contains("archive"), "{report}");
        assert!(report.contains("never"), "{report}");
        assert!(report.contains("depth               2"), "{report}");
    }

    fn text(f: impl FnOnce(&mut Vec<u8>)) -> String {
        let mut buf = Vec::new();
        f(&mut buf);
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn doctor_reports_both_roots_without_panicking() {
        let report = text(|out| doctor(&settings(), source(), None, out));
        assert!(report.contains("ROOT  R:\\"), "{report}");
        assert!(report.contains("ROOT  V:\\"), "{report}");
        assert!(report.contains("CURRENT CONFIGURATION"));
    }

    #[test]
    fn doctor_reports_an_unreachable_root_rather_than_failing() {
        let src: Arc<dyn DirSource> = Arc::new(FakeDirSource::new());
        let report = text(|out| doctor(&settings(), Arc::clone(&src), None, out));
        assert!(report.contains("reachable           NO"), "{report}");
    }

    #[test]
    fn doctor_names_the_configured_strategy_so_it_can_be_reproduced() {
        let s = Settings {
            enum_strategy: EnumStrategy::FindFirstEx,
            ..settings()
        };
        let report = text(|out| doctor(&s, source(), None, out));
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

    /// The server filter must not be enabled if it returns fewer entries.
    #[test]
    fn bench_fails_loudly_when_the_server_filter_under_returns() {
        let src = FakeDirSource::new()
            .with_dir(FLAT_FIXTURE_PATH, &["alpha_p12345.pdf", "p12345_two.pdf"])
            .with_dir(TREE_FIXTURE_PATH, &["notes.txt"]);
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

    /// The cache report must validate against the volume serial, as the app does.
    #[test]
    fn the_cache_report_states_the_volume_identity_it_checked_against() {
        let mut s = settings();
        s.persist = true;
        let report = text(|out| report_index_cache(&s, out));
        assert!(report.contains("volume serial"), "{report}");
        assert!(report.contains("cache key"), "{report}");
    }

    #[test]
    fn the_cache_report_points_at_the_decision_log_when_it_is_off() {
        let mut s = settings();
        s.persist = true;
        s.index_log = None;
        let report = text(|out| report_index_cache(&s, out));
        assert!(
            report.contains("--index-log"),
            "the user needs to be told the switch exists: {report}"
        );
    }

    #[test]
    fn the_cache_report_names_the_decision_log_when_it_is_on() {
        let mut s = settings();
        s.persist = true;
        s.index_log = Some(std::path::PathBuf::from(r"C:\temp\idx.log"));
        let report = text(|out| report_index_cache(&s, out));
        assert!(report.contains("idx.log"), "{report}");
    }

    #[test]
    fn the_stamp_report_says_how_precise_the_probe_was() {
        let src = source();
        let report =
            text(|out| bench_stamp(src.as_ref(), Path::new(FLAT_FIXTURE_PATH), false, out));
        assert!(
            report.contains("precision"),
            "a write-only stamp detects less than a full one, and the user              should be able to see which they have: {report}"
        );
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
