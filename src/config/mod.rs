//! Tunables, the routing table they are resolved against, and the settings
//! the rest of the crate reads.
//!
//! Settings are layered, each overriding the one before it: the shipped
//! defaults (see [`default_routes`]), the configuration file named by
//! [`ConfigChoice`], the environment, then the command line.
//!
//! Every strategy switch is readable from the environment so a fast path that
//! misbehaves on the real network shares can be turned off in the field
//! without a rebuild. That matters more than usual here: none of the Windows
//! I/O in this crate can be exercised on the machine it is written on.

pub mod file;
pub mod write;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::paths::{MappingKind, Routes};
use crate::util::winpath;

// --- Roots -----------------------------------------------------------------

/// Per-job folders. Each job code resolves to one small subdirectory.
pub const BASE_PATH: &str = "R:\\";

/// CustomPro. A flat directory: every job's files sit directly in it, so a
/// listing here is the whole share.
///
/// Note this is a *subdirectory*, not a drive root. Several Win32 calls accept
/// only a volume root, so anything volume-level derives one first - see
/// [`crate::util::winpath::volume_root_of`]. Getting that wrong fails quietly:
/// the volume serial disappears, which disables the persisted index's identity
/// check.
pub const CUSTPRO_PATH: &str = r"V:\Documents\custpro";

// --- Query -----------------------------------------------------------------

pub const MIN_QUERY_LEN: usize = 3;

/// Results retained and reachable by scrolling.
///
/// Fifteen was a single column's worth. A broad code legitimately matches far
/// more than that, and the cap was silently deciding which ones were worth
/// seeing - the drawing you wanted could be the sixteenth. Three hundred is
/// five screens of the grid below: enough that a broad query is useful,
/// bounded enough that ranking still means something and the tail stays
/// reachable by arrow key.
pub const MAX_RESULTS: usize = 300;

/// Arena bytes in the first segment a walk publishes.
///
/// Small on purpose. A walk of a large share runs for minutes, and the
/// difference between useful and useless is whether anything is searchable
/// after the first second - so the first segment is sealed early and the
/// ladder doubles from there, rather than every segment being the size that
/// suits the last one.
pub const SEGMENT_MIN_BYTES: usize = 512 << 10;

/// Ceiling on a segment's arenas.
///
/// Above this, sealing costs a visible copy and the marginal gain in search
/// efficiency is nil: at around twenty segments the per-segment overhead is
/// already lost in the sweep itself.
pub const SEGMENT_MAX_BYTES: usize = 16 << 20;

/// Most files one matching folder may contribute to a result set.
///
/// A job code names a folder as often as it names a file, and someone typing
/// one wants that folder's contents - at about seventeen files per folder, a
/// dozen matching folders still fit. The bound exists for the folder that
/// holds five thousand: without it, one such folder fills every slot and hides
/// every other folder that matched, which is the "files are missing" bug this
/// index was built to remove, wearing a new hat.
pub const MAX_FILES_PER_FOLDER: usize = 64;

/// Columns in the results grid, when the terminal is wide enough for them.
pub const GRID_COLUMNS: usize = 3;

/// Below this many cells a column is more marker and ellipsis than filename,
/// so the grid drops to fewer columns rather than rendering slivers.
pub const MIN_COLUMN_WIDTH: u16 = 24;

/// Results flow down each column before moving right, so a page divides
/// evenly and the last page is the only ragged one.
const _: () = assert!(MAX_RESULTS.is_multiple_of(GRID_COLUMNS));

/// Upper bound on a query we are willing to hand to the server as a wildcard.
pub const MAX_SERVER_QUERY_LEN: usize = 64;

/// Stop a server-side wildcard enumeration past this many hits and report the
/// count as a lower bound. Far more than [`MAX_RESULTS`] can ever display;
/// this exists purely to bound the pathological case.
pub const MAX_SERVER_HITS: usize = 5_000;

/// Consecutive audit failures before the server-side filter is disabled for
/// the rest of the process. See `search::verify`.
pub const SERVER_FILTER_MISS_LIMIT: u32 = 3;

// --- Timing ----------------------------------------------------------------

/// How long an `R:\<job>\` listing stays valid. These folders are small and a
/// re-scan is one round trip.
pub const JOB_CACHE_TTL: Duration = Duration::from_secs(20);

/// Bounded number of `R:\` job listings kept resident. Replaces the unbounded
/// map the previous implementation grew for the life of a session.
pub const JOB_CACHE_CAPACITY: usize = 64;

/// Quiet period after the last keystroke before the authoritative server-side
/// verification runs. A leading `*` defeats the NTFS index, so this costs real
/// server CPU and must not fire per keystroke.
pub const VERIFY_DEBOUNCE: Duration = Duration::from_millis(300);

/// Quiet period before speculatively enumerating a resolvable `R:\` folder.
/// Shorter than the verify debounce because a prefetch is cheap and wrong
/// guesses are harmless.
pub const PREFETCH_DEBOUNCE: Duration = Duration::from_millis(60);

/// How close together two clicks must be to count as a double-click.
///
/// The terminal reports presses, not clicks, so this is the program's own
/// definition. Matches the Windows default closely enough that a double-click
/// which selects a code in Explorer selects one here too.
pub const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// If a verify has not reported back by now, transition out of the spinner
/// regardless. Guards against a wedged or panicked worker.
pub const VERIFY_WATCHDOG: Duration = Duration::from_secs(10);

/// How often to ask the server whether `V:\` changed. This is a ~3 round-trip
/// metadata probe, not an enumeration.
pub const STAMP_PROBE_INTERVAL: Duration = Duration::from_secs(60);

/// Re-enumerate `V:\` unconditionally at least this often, in case the server
/// does not update the directory timestamp when entries are added or removed.
/// `--bench --allow-write` verifies whether it does.
pub const FULL_RESCAN_FLOOR: Duration = Duration::from_secs(60 * 60);

/// Longest a walked tree may go without a complete re-walk.
///
/// Half an hour is roughly a 3-7% duty cycle against the share, for a pass
/// that costs one to three minutes. With live updates working this is a
/// backstop against a watcher that accepted the request and then silently
/// stopped firing - a failure with no symptom, which is exactly why the
/// backstop is not optional. With them unavailable it is the entire freshness
/// guarantee, and half an hour is the longest a new job folder should stay
/// invisible.
pub const TREE_RESCAN_FLOOR: Duration = Duration::from_secs(30 * 60);

/// Two incremental updates are never started closer together than this.
///
/// A patch reads the folders that changed, not the share, so it is paced in
/// seconds where a full pass is paced in minutes. Its own floor rather than a
/// share of [`MIN_FULL_SCAN_SPACING`], because a single number would have to
/// be wrong for one of them.
pub const PATCH_SPACING: Duration = Duration::from_secs(5);

/// How long a burst of change notifications is allowed to accumulate before
/// it is acted on.
///
/// Measured from the *first* pending event, not the last; see
/// [`crate::index::watch`] for why that distinction is the whole point.
/// Two seconds is long enough that copying a job folder in arrives as one
/// update rather than forty, and short enough that nobody waits for it.
pub const WATCH_DEBOUNCE: Duration = Duration::from_secs(2);

/// Dirty directories past which a full re-walk is cheaper than re-reading them
/// one at a time.
///
/// Three round trips each, so a thousand directories is three thousand - about
/// a third of a full walk, for a result that covers a fraction of the share.
/// Past that the honest answer is to walk it.
pub const WATCH_DIRTY_CAP: usize = 1_000;

/// Two re-walks are never started closer together than this.
///
/// Ten times [`MIN_FULL_SCAN_SPACING`], in proportion to what a walk costs
/// against a flat enumeration.
pub const TREE_MIN_SCAN_SPACING: Duration = Duration::from_secs(5 * 60);

/// Refuse a persisted index older than this.
pub const MAX_INDEX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub const BACKOFF_BASE: Duration = Duration::from_secs(5);
pub const BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);

/// Retry pacing for a failed *full enumeration*, as opposed to a failed probe.
///
/// Deliberately far slower than [`BACKOFF_BASE`]: a probe is three round trips
/// and retrying it a second later is free, whereas a full scan is seconds of
/// traffic against a million-entry share. The previous implementation used the
/// probe schedule for both, and because the jitter is *full* jitter - uniform
/// in `0 ..= ceiling` - a failing scan could be retried under a second later,
/// repeatedly.
pub const SCAN_BACKOFF_BASE: Duration = Duration::from_secs(30);
pub const SCAN_BACKOFF_CAP: Duration = Duration::from_secs(10 * 60);

/// Two full enumerations are never started closer together than this, by any
/// path except an explicit F5.
///
/// This is a backstop, not a schedule: every individual decision already
/// paces itself. It exists because the cost of getting one of those decisions
/// wrong is a million-entry enumeration in a loop, and a single invariant
/// enforced in one place is cheaper to trust than five separate ones.
pub const MIN_FULL_SCAN_SPACING: Duration = Duration::from_secs(30);

/// Consecutive "this share cannot answer the probe" failures before change
/// detection is declared unavailable.
///
/// Not one: a freak `ERROR_ACCESS_DENIED` should not permanently disable the
/// cheap freshness check. Not many: each attempt is a wasted round trip.
pub const STAMP_FAILURES_BEFORE_BLIND: u32 = 2;

/// Jitter applied to the healthy probe cadence, in percent, so a fleet of
/// these apps does not hammer the file server in lockstep on the minute.
pub const PROBE_JITTER_PERCENT: u32 = 10;

/// Redraw cadence while something is animating. Nothing animates at a finer
/// granularity than a spinner frame, and the elapsed readout is rounded to
/// match so consecutive frames actually differ.
pub const ANIMATION_TICK: Duration = Duration::from_millis(100);

/// Per-thread budget when shutting down. Past this the process exits rather
/// than waiting on a blocked SMB syscall that cannot be cancelled.
pub const SHUTDOWN_JOIN_BUDGET: Duration = Duration::from_millis(250);

// --- Enumeration -----------------------------------------------------------

/// Directory buffer for the handle-based enumerator. The whole point is to
/// turn tens of thousands of SMB round trips into a few hundred.
pub const DIR_BUFFER_BYTES: usize = 1 << 20;
pub const DIR_BUFFER_MIN: usize = 64 << 10;

/// Directories read at once during a recursive walk.
///
/// A tree walk is bound by round trips per *directory*, so walking several at
/// once converts almost directly into wall clock. Eight rather than more
/// because SMB2 flow control is credit-based: a 1 MiB directory query spends
/// around sixteen credits of a session's few hundred, so past roughly this
/// many large requests the client starts waiting for credits rather than
/// gaining throughput - and because a share is someone else's production file
/// server. It is the same default `robocopy /MT` picked.
pub const WALK_CONCURRENCY: usize = 8;

/// Per-directory buffer during a tree walk.
///
/// An eighth of [`DIR_BUFFER_BYTES`], which is sized for a single flat
/// directory of a million entries. In a job tree the median directory holds
/// well under a hundred, so this still reads almost all of them in one round
/// trip while costing sixteen times fewer SMB2 credits - which is what lets
/// eight walkers actually be in flight at once instead of starving each other.
pub const WALK_DIR_BUFFER_BYTES: usize = 128 << 10;

/// Entries per rayon chunk in the matcher. Sized so a cancelled search stops
/// within a few hundred microseconds without putting a branch in the inner
/// vectorized scan.
pub const MATCH_CHUNK_ENTRIES: usize = 8_192;

// --- Strategy selection ----------------------------------------------------

/// Which directory-enumeration implementation to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnumStrategy {
    /// `GetFileInformationByHandleEx` with a 1 MiB buffer. Documented, no
    /// ntdll linkage, and captures essentially the whole round-trip win.
    #[default]
    HandleDirInfo,
    /// `FindFirstFileExW` + `FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`.
    /// Also the only API that accepts a search pattern, so the server-side
    /// filter uses it regardless of this setting.
    FindFirstEx,
    /// `std::fs::read_dir`. Always available; the field escape hatch.
    StdReadDir,
}

impl EnumStrategy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "handle" | "handle-dirinfo" | "dirinfo" => Some(Self::HandleDirInfo),
            "find" | "findfirst" | "findfirstex" => Some(Self::FindFirstEx),
            "std" | "readdir" | "read_dir" => Some(Self::StdReadDir),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::HandleDirInfo => "handle-dirinfo",
            Self::FindFirstEx => "findfirstex",
            Self::StdReadDir => "std",
        }
    }

    /// Strategies to try in order. Advance only on `Unsupported` - the other
    /// errors are answers, and retrying them through a different API just
    /// multiplies latency on an operation that is already failing.
    pub fn chain(self) -> &'static [EnumStrategy] {
        match self {
            Self::HandleDirInfo => &[Self::HandleDirInfo, Self::FindFirstEx, Self::StdReadDir],
            Self::FindFirstEx => &[Self::FindFirstEx, Self::StdReadDir],
            Self::StdReadDir => &[Self::StdReadDir],
        }
    }
}

/// Which matching implementation to use. The naive one is retained for one
/// release so a field regression is a flag flip rather than a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MatcherKind {
    #[default]
    Simd,
    Naive,
}

impl MatcherKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "simd" | "fast" | "memmem" => Some(Self::Simd),
            "naive" | "slow" => Some(Self::Naive),
            _ => None,
        }
    }
}

/// Which application a chosen result is handed to.
///
/// The two differ in more than which executable is spawned. `Avwin` opens the
/// one file the cursor is on, which is all it can do: the pages of a drawing
/// set are separate files on the share, and a viewer given one of them shows
/// one page. `Pdf` treats the code as naming a *document*, gathers every page
/// of it and hands over a single assembled PDF - which is what someone asking
/// for `11-D-0704` almost always meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerKind {
    /// Every page of the code, merged into one PDF, opened with the system's
    /// `.pdf` handler.
    #[default]
    Pdf,
    /// The single selected file, handed to `avwin.exe`. The behaviour this
    /// program had before there was a choice.
    Avwin,
}

impl ViewerKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pdf" | "merge" | "merged" => Some(Self::Pdf),
            "avwin" | "av" | "avwin.exe" => Some(Self::Avwin),
            _ => None,
        }
    }

    /// The spelling written to the config file, so it must be one `parse`
    /// accepts. `every_key_the_writer_can_emit_is_an_accepted_setting` pins
    /// that, because an unknown value is a hard startup error.
    pub fn name(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Avwin => "avwin",
        }
    }

    /// What F2 does. A cycle rather than a boolean so a third viewer is one
    /// match arm rather than a rethink.
    pub fn next(self) -> Self {
        match self {
            Self::Pdf => Self::Avwin,
            Self::Avwin => Self::Pdf,
        }
    }
}

/// The routing table the shipped defaults describe.
///
/// Parsed from `assets/default_config.toml` through the ordinary loader, so
/// the file written on first run and the compiled-in fallback are the same
/// bytes validated the same way - they cannot drift.
///
/// Reproduces the previous hardcoded behaviour exactly, with the corrected
/// CustomPro directory: CustomPro is listed first and its rules `stop`, so a
/// dashed code such as `P12345-001` resolves there and nowhere else.
/// `tests/routing_parity.rs` proves that against a frozen copy of the
/// original implementation.
pub fn default_routes() -> Routes {
    file::builtin().routes
}

/// Which configuration file to read, if any.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConfigChoice {
    /// `%APPDATA%\files\config.toml`, created with the shipped defaults if
    /// absent.
    #[default]
    Default,
    /// An explicit `--config <path>`. A missing file here is an error: it was
    /// asked for by name.
    Explicit(PathBuf),
    /// `--no-config`. The one-flag answer to "is my config the problem?".
    None,
}

/// Resolved runtime settings.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The routing table. Immutable once loaded, shared by every thread.
    pub routes: Arc<Routes>,
    /// First enabled job-folder mapping.
    ///
    /// Derived from `routes`, kept while the rest of the crate is migrated to
    /// addressing mappings by id. Never set independently.
    pub base_path: PathBuf,
    /// First enabled flat mapping. Derived from `routes`, as above.
    pub custpro_path: PathBuf,
    /// First enabled tree mapping, or empty when none is configured.
    ///
    /// Derived from `routes` like the two above, and for the same transitional
    /// reason: it lets the index and the search be written against a root
    /// rather than against the routing table, which is what makes the routing
    /// table removable later without re-plumbing them.
    pub tree_path: PathBuf,
    pub enum_strategy: EnumStrategy,
    pub matcher: MatcherKind,
    /// Server-side wildcard filtering. Ships **off** so it can be enabled only
    /// after `--bench` confirms the server's pattern matching drops nothing.
    pub server_filter: bool,
    pub persist: bool,
    pub cache_dir: Option<PathBuf>,
    /// Where to append the index decision log, if anywhere.
    ///
    /// Off by default. It exists so that "the index reloads at random" can be
    /// answered from the machine that has the problem, without a debugger and
    /// without a rebuild - see [`crate::index::log`].
    pub index_log: Option<PathBuf>,
    /// Remember codes between runs, for recall with the Up arrow.
    pub history: bool,
    /// Where they are remembered. `None` disables storage without disabling
    /// recall within the session.
    pub history_path: Option<PathBuf>,
    /// The viewer at startup.
    ///
    /// Deliberately the *initial* value and nothing more. F2 changes which
    /// viewer is in use, and that lives on `AppState`, not here: `Settings` is
    /// cloned into the backend and every worker, so a mutable field would be
    /// one truth with several stale copies of it. See `AppState::viewer`.
    pub viewer: ViewerKind,
    /// Overrides the system's `.pdf` association when set.
    ///
    /// Not validated at load, unlike every other path in the configuration. A
    /// mistyped *share* path is silent - the search simply finds nothing and
    /// the user concludes the job has no files - which is why `file` refuses
    /// to start on one. A missing viewer executable is the opposite: it fails
    /// loudly the first time it is used, and this same file roams to laptops
    /// where that executable legitimately is not installed. Refusing to start
    /// there would be a regression, so this is reported by `--doctor` and
    /// surfaced as a toast instead.
    pub pdf_viewer: Option<PathBuf>,
    /// Whether an F2 toggle can be written back to the configuration file.
    ///
    /// False when the environment or the command line set the viewer, because
    /// `apply_file_settings` lets those win and the saved value would be
    /// ignored at the next start; and false when there is no file to write to
    /// at all. Decided here rather than in the actor that does the writing, so
    /// the state machine can say "this session only" instead of reporting a
    /// save that changes nothing.
    pub viewer_persistable: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self::with_routes(Arc::new(default_routes()), |s| s)
    }
}

impl Settings {
    /// Builds settings around a routing table, deriving the transitional
    /// convenience paths from it so the two can never disagree.
    pub fn with_routes(routes: Arc<Routes>, tweak: impl FnOnce(Self) -> Self) -> Self {
        let base_path = routes
            .enabled()
            .find(|m| m.kind == MappingKind::JobFolder)
            .map(|m| m.path.clone())
            .unwrap_or_default();
        let custpro_path = routes
            .enabled()
            .find(|m| m.kind == MappingKind::Flat)
            .map(|m| m.path.clone())
            .unwrap_or_default();
        // Empty when no tree mapping is configured, which is how the rest of
        // the program asks "is there a tree" without consulting the routing
        // table.
        let tree_path = routes
            .enabled()
            .find(|m| m.kind == MappingKind::Tree)
            .map(|m| m.path.clone())
            .unwrap_or_default();
        tweak(Self {
            routes,
            base_path,
            custpro_path,
            tree_path,
            enum_strategy: EnumStrategy::default(),
            matcher: MatcherKind::default(),
            server_filter: false,
            persist: true,
            cache_dir: default_cache_dir(),
            index_log: None,
            history: true,
            history_path: crate::history::default_path(),
            viewer: ViewerKind::default(),
            pdf_viewer: None,
            // Assume not, and let `load` say otherwise once it knows there is
            // a file and that nothing outranks it. Defaulting the other way
            // would make every test fixture and every `--no-config` session
            // claim it could save.
            viewer_persistable: false,
        })
    }

    /// Reads overrides from the environment, ignoring anything unparseable
    /// rather than refusing to start.
    pub fn from_env() -> Self {
        Self::from_env_with(default_routes())
    }

    /// Loads configuration, then layers the environment over it.
    ///
    /// Precedence, lowest first: the shipped defaults, the config file, the
    /// environment, the command line (applied by the caller). A bad file is
    /// returned as errors rather than swallowed - see [`file`] for why
    /// falling back would be worse.
    pub fn load(choice: &ConfigChoice) -> Result<Self, Vec<file::ConfigError>> {
        // Whether a file was actually read decides whether F2 has anywhere to
        // save to, so it is tracked here rather than rediscovered later.
        let mut have_file = false;
        let parsed = match choice {
            ConfigChoice::None => file::builtin(),
            ConfigChoice::Explicit(path) => {
                have_file = true;
                file::load_file(path, true)?
            }
            ConfigChoice::Default => match file::default_config_path() {
                Some(path) => {
                    // Best effort: a read-only profile means no file, and the
                    // built-in defaults are the same bytes anyway.
                    let _ = file::write_default_if_absent(&path);
                    if path.exists() {
                        have_file = true;
                        file::load_file(&path, false)?
                    } else {
                        file::builtin()
                    }
                }
                None => file::builtin(),
            },
        };

        let mut s = Self::from_env_with(parsed.routes);
        s.apply_file_settings(&parsed.settings);
        // The environment outranks the file, so saving into the file while
        // `FILES_VIEWER` is set would report success and change nothing.
        s.viewer_persistable = have_file && env_str("FILES_VIEWER").is_none();
        Ok(s)
    }

    /// Applies the config file's `[settings]` table, without letting it
    /// override anything the environment already set.
    fn apply_file_settings(&mut self, f: &file::FileSettings) {
        if env_str("FILES_FS_STRATEGY").is_none()
            && let Some(v) = f.enum_strategy.as_deref().and_then(EnumStrategy::parse)
        {
            self.enum_strategy = v;
        }
        if env_str("FILES_MATCHER").is_none()
            && let Some(v) = f.matcher.as_deref().and_then(MatcherKind::parse)
        {
            self.matcher = v;
        }
        if env_bool("FILES_SERVER_FILTER").is_none()
            && let Some(v) = f.server_filter
        {
            self.server_filter = v;
        }
        if env_bool("FILES_PERSIST").is_none()
            && let Some(v) = f.persist
        {
            self.persist = v;
        }
        if env_str("FILES_CACHE_DIR").is_none()
            && let Some(v) = &f.cache_dir
        {
            self.cache_dir = Some(v.clone());
        }
        if env_bool("FILES_HISTORY").is_none()
            && let Some(v) = f.history
        {
            self.history = v;
        }
        if env_str("FILES_VIEWER").is_none()
            && let Some(v) = f.viewer.as_deref().and_then(ViewerKind::parse)
        {
            self.viewer = v;
        }
        if env_str("FILES_PDF_VIEWER").is_none()
            && let Some(v) = &f.pdf_viewer
        {
            self.pdf_viewer = Some(v.clone());
        }
    }

    /// As [`Settings::from_env`], but over a supplied routing table.
    ///
    /// The path overrides repoint a mapping *in the table* rather than
    /// writing to the derived fields, so the two can never disagree. Paths are
    /// normalised on the way in: a volume root keeps its trailing separator,
    /// anything deeper loses one, so a stray trailing backslash on a
    /// subdirectory can never reach `CreateFileW` as a non-root path with a
    /// separator.
    pub fn from_env_with(mut routes: Routes) -> Self {
        if let Some(v) = env_str("FILES_BASE_PATH") {
            routes.set_path("jobs", PathBuf::from(v));
        }
        if let Some(v) = env_str("FILES_CUSTPRO_PATH") {
            routes.set_path("custompro", PathBuf::from(v));
        }

        let mut s = Self::with_routes(Arc::new(routes), |s| s);
        if let Some(v) = env_str("FILES_FS_STRATEGY").and_then(|v| EnumStrategy::parse(&v)) {
            s.enum_strategy = v;
        }
        if let Some(v) = env_str("FILES_MATCHER").and_then(|v| MatcherKind::parse(&v)) {
            s.matcher = v;
        }
        if let Some(v) = env_bool("FILES_SERVER_FILTER") {
            s.server_filter = v;
        }
        if let Some(v) = env_bool("FILES_PERSIST") {
            s.persist = v;
        }
        if let Some(v) = env_str("FILES_CACHE_DIR") {
            s.cache_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = env_str("FILES_INDEX_LOG") {
            s.index_log = Some(PathBuf::from(v));
        }
        if let Some(v) = env_bool("FILES_HISTORY") {
            s.history = v;
        }
        if let Some(v) = env_str("FILES_VIEWER").and_then(|v| ViewerKind::parse(&v)) {
            s.viewer = v;
        }
        if let Some(v) = env_str("FILES_PDF_VIEWER") {
            s.pdf_viewer = Some(PathBuf::from(v));
        }
        s
    }

    /// True when `dir` is the flat CustomPro directory, which is the only one
    /// large enough to justify the persisted index and the stamp probe.
    ///
    /// Compares by path identity rather than `PathBuf` equality, which would
    /// call `V:\x` and `V:\x\` different places.
    pub fn is_flat_root(&self, dir: &Path) -> bool {
        winpath::same_dir(dir, &self.custpro_path)
    }
}

pub(crate) fn env_str(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Reads a duration given in whole seconds. Zero and unparseable values are
/// ignored rather than accepted: a zero probe interval would busy-loop the
/// index thread against the file server.
pub(crate) fn env_secs(key: &str) -> Option<Duration> {
    let secs: u64 = env_str(key)?.parse().ok()?;
    (secs > 0).then(|| Duration::from_secs(secs))
}

pub(crate) fn env_bool(key: &str) -> Option<bool> {
    match env_str(key)?.to_ascii_lowercase().as_str() {
        "1" | "on" | "true" | "yes" => Some(true),
        "0" | "off" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// `%LOCALAPPDATA%\files`, falling back to the system temp directory.
///
/// The index must live on a local disk: it is memory-mapped, and mapping a
/// file over SMB would reintroduce the very latency the cache exists to avoid.
pub fn default_cache_dir() -> Option<PathBuf> {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let p = PathBuf::from(local);
        if !p.as_os_str().is_empty() {
            return Some(p.join("files"));
        }
    }
    let t = std::env::temp_dir();
    if t.as_os_str().is_empty() {
        None
    } else {
        Some(t.join("files-index"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_enum_strategy_spelling() {
        assert_eq!(
            EnumStrategy::parse("handle"),
            Some(EnumStrategy::HandleDirInfo)
        );
        assert_eq!(
            EnumStrategy::parse("  DirInfo "),
            Some(EnumStrategy::HandleDirInfo)
        );
        assert_eq!(
            EnumStrategy::parse("findfirst"),
            Some(EnumStrategy::FindFirstEx)
        );
        assert_eq!(EnumStrategy::parse("std"), Some(EnumStrategy::StdReadDir));
        assert_eq!(EnumStrategy::parse("nonsense"), None);
    }

    #[test]
    fn parses_matcher_kinds() {
        assert_eq!(MatcherKind::parse("simd"), Some(MatcherKind::Simd));
        assert_eq!(MatcherKind::parse("NAIVE"), Some(MatcherKind::Naive));
        assert_eq!(MatcherKind::parse(""), None);
    }

    #[test]
    fn parses_every_viewer_spelling() {
        assert_eq!(ViewerKind::parse("pdf"), Some(ViewerKind::Pdf));
        assert_eq!(ViewerKind::parse("  MERGE "), Some(ViewerKind::Pdf));
        assert_eq!(ViewerKind::parse("avwin"), Some(ViewerKind::Avwin));
        assert_eq!(ViewerKind::parse("AVWIN.EXE"), Some(ViewerKind::Avwin));
        assert_eq!(ViewerKind::parse("notepad"), None);
    }

    /// Assembling the whole document is what someone typing a code almost
    /// always meant; opening one page of it is the special case.
    #[test]
    fn the_pdf_viewer_is_the_default() {
        assert_eq!(Settings::default().viewer, ViewerKind::Pdf);
    }

    #[test]
    fn toggling_the_viewer_returns_to_where_it_started() {
        for v in [ViewerKind::Pdf, ViewerKind::Avwin] {
            assert_eq!(v.next().next(), v);
            assert_ne!(v.next(), v);
        }
    }

    /// Every spelling the writer can emit has to be one the parser accepts,
    /// or F2 would write a value that stops the program at the next start.
    #[test]
    fn every_name_the_writer_emits_parses_back() {
        for v in [ViewerKind::Pdf, ViewerKind::Avwin] {
            assert_eq!(ViewerKind::parse(v.name()), Some(v));
        }
    }

    /// Nothing to save to, so F2 must say "this session only" rather than
    /// report a save that goes nowhere.
    #[test]
    fn the_built_in_defaults_have_nowhere_to_persist_a_viewer() {
        let s = Settings::load(&ConfigChoice::None).expect("built-ins must load");
        assert!(!s.viewer_persistable);
    }

    #[test]
    fn every_chain_ends_at_the_always_available_strategy() {
        for s in [
            EnumStrategy::HandleDirInfo,
            EnumStrategy::FindFirstEx,
            EnumStrategy::StdReadDir,
        ] {
            let chain = s.chain();
            assert_eq!(chain[0], s, "chain must start with the requested strategy");
            assert_eq!(
                *chain.last().unwrap(),
                EnumStrategy::StdReadDir,
                "chain must never leave the caller stranded"
            );
        }
    }

    #[test]
    fn server_filter_is_off_by_default() {
        // It is enabled only after --bench proves the server's pattern
        // matching returns a superset of a full enumeration.
        assert!(!Settings::default().server_filter);
    }

    #[test]
    fn recognises_the_flat_root() {
        let s = Settings::default();
        assert!(s.is_flat_root(Path::new(CUSTPRO_PATH)));
        assert!(!s.is_flat_root(&Path::new(BASE_PATH).join("ab1234")));
    }

    /// Checked at compile time: a shorter needle would make `memmem` weak
    /// and would make a leading wildcard match an unreasonable share of a
    /// million-entry directory.
    const _: () = assert!(MIN_QUERY_LEN >= 3);
    const _: () = assert!(MAX_SERVER_QUERY_LEN > MIN_QUERY_LEN);
}
