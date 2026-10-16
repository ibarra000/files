//! Tunables, the routing table, and the resolved settings the crate reads.
//!
//! Settings are layered, each overriding the one before: the shipped defaults
//! (see [`default_routes`]), the configuration file named by [`ConfigChoice`],
//! the environment, then the command line.
//!
//! Every strategy switch is readable from the environment so a fast path that
//! misbehaves on a real network share can be turned off without a rebuild.

pub mod file;
pub mod hidden;
pub mod migrate;
pub mod viewers;
pub mod write;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::paths::{MappingKind, Routes};

pub use viewers::{ViewerApp, Viewers};

// --- Roots -----------------------------------------------------------------

/// The tree share in the test and demo fixtures: a whole volume.
#[doc(hidden)]
pub const TREE_FIXTURE_PATH: &str = "R:\\";

/// The flat share in the test and demo fixtures: one large directory.
///
/// This is a subdirectory, not a drive root. Win32 calls that accept only a
/// volume root must derive one with [`crate::util::winpath::volume_root_of`];
/// otherwise the volume serial is lost and the persisted index's identity
/// check is silently disabled.
#[doc(hidden)]
pub const FLAT_FIXTURE_PATH: &str = r"V:\shared\files";

// --- Query -----------------------------------------------------------------

/// The shortest term the local search runs for.
///
/// Separate from [`MIN_SERVER_QUERY_LEN`] and [`MIN_REMEMBERED_LEN`], which
/// bound different costs.
pub const MIN_TERM_LEN: usize = 1;

/// The shortest term sent to a file server.
///
/// A shorter filter matches most of the share, so the round trip costs a full
/// listing and saves nothing. Below this only the local index answers.
pub const MIN_SERVER_QUERY_LEN: usize = 3;

/// The shortest search written to the history file.
///
/// Keeps one- and two-character prefixes typed on the way to a real search
/// out of the recent-searches list.
pub const MIN_REMEMBERED_LEN: usize = 3;

/// Results retained and reachable by scrolling.
///
/// Large enough that a broad query is useful, small enough that ranking still
/// matters and the tail stays reachable by arrow key.
pub const MAX_RESULTS: usize = 300;

/// Compact result rows on one screen, and the distance `PageDown` moves.
///
/// Lives here rather than in `gui::theme` so the state machine does not depend
/// on the GUI. Fixed by the panel height: six forty-point rows fill what the
/// field and footer leave, and `gui::theme` asserts that arithmetic.
pub const VISIBLE_ROWS: usize = 6;

/// [`VISIBLE_ROWS`] for the taller rows of [`ResultLayout::Detailed`].
pub const VISIBLE_ROWS_DETAILED: usize = 4;

/// Arena bytes in the first segment a walk publishes.
///
/// Small so something is searchable within the first second of a
/// minutes-long walk; later segment sizes double from here.
pub const SEGMENT_MIN_BYTES: usize = 512 << 10;

/// Ceiling on a segment's arenas.
///
/// Above this, sealing costs a visible copy and search efficiency barely
/// improves.
pub const SEGMENT_MAX_BYTES: usize = 16 << 20;

/// Most files one matching folder may contribute to a result set.
///
/// Stops one very large folder from filling every slot and hiding the other
/// folders that matched.
pub const MAX_FILES_PER_FOLDER: usize = 64;

/// Most results laid side by side on one line of the list.
///
/// At the panel's 600 points a fourth column truncates names to about a dozen
/// characters. See `app::state::grid` for keyboard movement.
pub const MAX_COLUMNS: usize = 3;

/// Folders below the root a live query descends, unless the mapping overrides.
///
/// Depth one is a single filtered round trip returning matching files and
/// folder names. Each further level costs one round trip per folder, because a
/// filtered query cannot reveal a non-matching folder, so the level above must
/// be listed in full.
pub const DEFAULT_LIVE_DEPTH: u16 = 1;

/// Deepest a configuration may ask a live query to go.
///
/// With a branching factor of eight, depth four is already 512 directories
/// against [`LIVE_ROUND_TRIP_BUDGET`], so deeper values could not be honoured.
pub const MAX_LIVE_DEPTH: u16 = 4;

/// Round trips one live pass may spend.
///
/// Bounds server load rather than latency: on a LAN this is under 60 ms. Over
/// a VPN (30-80 ms per round trip) it can reach seconds, so [`LIVE_DEADLINE`]
/// also applies.
pub const LIVE_ROUND_TRIP_BUDGET: u32 = 64;

/// Where a live pass stops, whatever it has reached by then.
///
/// Must stay shorter than [`REMEMBER_DEBOUNCE`], or a late live answer could
/// leave the recall list and result list disagreeing about the finished query.
pub const LIVE_DEADLINE: Duration = Duration::from_millis(1_200);

/// Quiet period after the last keystroke before a live share is asked.
///
/// Set by file-server cost, independently of [`SEARCH_DEBOUNCE`]: a leading
/// `*` defeats the NTFS index, so each query costs the server real CPU. 600 ms
/// clears the 200-300 ms pauses between the groups of a search such as
/// `AB-C-0704`, so typing it costs one query rather than several.
pub const LIVE_DEBOUNCE: Duration = Duration::from_millis(600);

/// Floor between two queries of the same share, whatever asks for them.
///
/// Covers held keys, paste loops and bursts of index updates that the debounce
/// does not, limiting each client to one query per share per second.
pub const LIVE_MIN_SPACING: Duration = Duration::from_secs(1);

/// Refusals to filter before a live share is switched off for the process.
///
/// Matches [`SERVER_FILTER_MISS_LIMIT`]. A source that ignores the filter
/// answers every query with a full enumeration, which live mode exists to
/// avoid.
pub const LIVE_FAILURE_LIMIT: u32 = 3;

/// Longest query handed to the server as a wildcard.
pub const MAX_SERVER_QUERY_LEN: usize = 64;

/// Stop a server-side wildcard enumeration past this many hits and report the
/// count as a lower bound. Far more than [`MAX_RESULTS`] can ever display;
/// this exists purely to bound the pathological case.
pub const MAX_SERVER_HITS: usize = 5_000;

/// Consecutive audit failures before the server-side filter is disabled for
/// the rest of the process. See `search::verify`.
pub const SERVER_FILTER_MISS_LIMIT: u32 = 3;

// --- Timing ----------------------------------------------------------------

/// How long a cached folder listing stays valid. These folders are small and a
/// re-scan is one round trip.
pub const FOLDER_CACHE_TTL: Duration = Duration::from_secs(20);

/// Bounded number of folder listings kept resident.
pub const FOLDER_CACHE_CAPACITY: usize = 64;

/// Quiet period after the last keystroke before the local match runs.
///
/// The match is sub-millisecond and local; the delay only avoids showing a
/// list for every prefix of a search being typed. The network is paced
/// separately by [`VERIFY_DEBOUNCE`] and [`LIVE_DEBOUNCE`].
///
/// `AppState::on_tick` still runs the matcher before verification, because
/// after a long stall both can fall due on the same tick.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(180);

/// The local match answers before the server is asked.
///
/// Raising one debounce without the other is a build error rather than a
/// verification run against the previous query's results.
const _: () = assert!(SEARCH_DEBOUNCE.as_millis() <= VERIFY_DEBOUNCE.as_millis());

/// Quiet period after the last keystroke before the authoritative server-side
/// verification runs. A leading `*` defeats the NTFS index, so this costs real
/// server CPU and must not fire per keystroke.
pub const VERIFY_DEBOUNCE: Duration = Duration::from_millis(300);

/// Quiet period after the last keystroke before the search on the line
/// reaches the recall list.
///
/// Independent of [`VERIFY_DEBOUNCE`]: this is set by how long a person pauses
/// mid-search, so partial searches such as `AB-C` are not recorded on the way to
/// `AB-C-0704`. `History::record` collapses any prefix chain that gets through.
pub const REMEMBER_DEBOUNCE: Duration = Duration::from_millis(1_500);

/// How close together two clicks must be to count as a double-click.
///
/// Close to the Windows default, so a double-click that works in Explorer
/// works here too.
pub const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// Time after which an unanswered verify leaves the spinner state.
///
/// Guards against a wedged or panicked worker.
pub const VERIFY_WATCHDOG: Duration = Duration::from_secs(10);

/// Time after which an Enter still waiting on the matcher is given back.
///
/// Enter typed inside [`SEARCH_DEBOUNCE`] waits for the matcher, since the row
/// on screen answers the previous query. Shorter than [`VERIFY_WATCHDOG`]
/// because the match is local.
pub const ENTER_WATCHDOG: Duration = Duration::from_secs(1);

/// How often to ask the server whether `V:\` changed.
///
/// A metadata probe of about three round trips, not an enumeration.
pub const STAMP_PROBE_INTERVAL: Duration = Duration::from_secs(60);

/// Re-enumerate `V:\` unconditionally at least this often, in case the server
/// does not update the directory timestamp when entries are added or removed.
/// `--bench --allow-write` verifies whether it does.
pub const FULL_RESCAN_FLOOR: Duration = Duration::from_secs(60 * 60);

/// Longest a walked tree may go without a complete re-walk.
///
/// A backstop against a watcher that silently stops firing, and the only
/// freshness guarantee when change notifications are unavailable. Roughly a
/// 3-7% duty cycle for a one-to-three-minute walk.
pub const TREE_RESCAN_FLOOR: Duration = Duration::from_secs(30 * 60);

/// Two incremental updates are never started closer together than this.
///
/// A patch reads only the changed folders, so it is paced in seconds where
/// [`MIN_FULL_SCAN_SPACING`] paces full passes in minutes.
pub const PATCH_SPACING: Duration = Duration::from_secs(5);

/// How long a burst of change notifications accumulates before it is acted on.
///
/// Measured from the first pending event, not the last; see
/// [`crate::index::watch`]. Long enough that copying in a project folder arrives
/// as one update.
pub const WATCH_DEBOUNCE: Duration = Duration::from_secs(2);

/// Dirty directories past which a full re-walk replaces re-reading them
/// individually.
///
/// At three round trips each, a thousand directories already costs about a
/// third of a full walk.
pub const WATCH_DIRTY_CAP: usize = 1_000;

/// Two re-walks are never started closer together than this.
///
/// Ten times [`MIN_FULL_SCAN_SPACING`], in proportion to a walk's cost
/// relative to a flat enumeration.
pub const TREE_MIN_SCAN_SPACING: Duration = Duration::from_secs(5 * 60);

/// How old a persisted index may get before it is labelled stale.
///
/// Advisory only: a stale index is still served, with its age shown in the
/// status line. Discarding it would force the full pass that on-demand refresh
/// avoids, on every machine at once.
pub const MAX_INDEX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub const BACKOFF_BASE: Duration = Duration::from_secs(5);
pub const BACKOFF_CAP: Duration = Duration::from_secs(5 * 60);

/// Retry pacing for a failed full enumeration, as opposed to a failed probe.
///
/// Much slower than [`BACKOFF_BASE`] because a scan is seconds of traffic
/// against a large share. With full jitter (uniform in `0 ..= ceiling`), the
/// probe schedule could retry a failing scan within a second.
pub const SCAN_BACKOFF_BASE: Duration = Duration::from_secs(30);
pub const SCAN_BACKOFF_CAP: Duration = Duration::from_secs(10 * 60);

/// Two full enumerations are never started closer together than this, except
/// by an explicit F5.
///
/// A single backstop against any scheduling path looping a million-entry
/// enumeration.
pub const MIN_FULL_SCAN_SPACING: Duration = Duration::from_secs(30);

/// Consecutive "this share cannot answer the probe" failures before change
/// detection is declared unavailable.
///
/// More than one so a transient `ERROR_ACCESS_DENIED` does not disable the
/// check permanently.
pub const STAMP_FAILURES_BEFORE_BLIND: u32 = 2;

/// Jitter applied to the healthy probe cadence, in percent, so a fleet of
/// these apps does not hammer the file server in lockstep on the minute.
pub const PROBE_JITTER_PERCENT: u32 = 10;

/// Spread added on top of the full-rescan floor, in percent.
///
/// Probe jitter does not reach full passes (trees never probe), so without
/// this, clients started together re-walk in lockstep. Only ever later than
/// the floor, so the floor's freshness promise still holds.
pub const FLOOR_SPREAD_PERCENT: u32 = 25;

/// Shortest gap between two writes of the index to disk.
///
/// The cache is written whole (around 100 MB for a large tree share), so watcher
/// patches are persisted at most this often. A crash loses at most this
/// interval of folder updates.
pub const PERSIST_SPACING: Duration = Duration::from_secs(5 * 60);

/// Redraw cadence while something is animating.
///
/// One spinner frame; the elapsed readout is rounded to match.
pub const ANIMATION_TICK: Duration = Duration::from_millis(100);

/// How often a visible retry countdown is redrawn.
///
/// Armed only while a share is unreachable and a retry is pending.
pub const COUNTDOWN_TICK: Duration = Duration::from_secs(1);

/// Per-thread join budget at shutdown.
///
/// Past this the process exits rather than wait on an uncancellable SMB call.
pub const SHUTDOWN_JOIN_BUDGET: Duration = Duration::from_millis(250);

// --- Enumeration -----------------------------------------------------------

/// Directory buffer for the handle-based enumerator.
///
/// Large enough to turn tens of thousands of SMB round trips into a few hundred.
pub const DIR_BUFFER_BYTES: usize = 1 << 20;
pub const DIR_BUFFER_MIN: usize = 64 << 10;

/// Directories read at once during a recursive walk.
///
/// SMB2 flow control is credit-based, so past about eight large requests the
/// client waits for credits instead of gaining throughput. Same default as
/// `robocopy /MT`.
pub const WALK_CONCURRENCY: usize = 8;

/// How many shares are walked at once, by default.
///
/// Multiplies with [`WALK_CONCURRENCY`], so it stays low to protect a shared
/// file server. Two rather than one so a slow share does not stall the rest.
pub const DEFAULT_MAX_CONCURRENT_SCANS: usize = 2;

/// Per-directory buffer during a tree walk.
///
/// An eighth of [`DIR_BUFFER_BYTES`]: still one round trip for nearly every
/// directory in a tree share, at far fewer SMB2 credits, so eight walkers can run at
/// once.
pub const WALK_DIR_BUFFER_BYTES: usize = 128 << 10;

/// Entries per rayon chunk in the matcher.
///
/// Lets a cancelled search stop within a few hundred microseconds without a
/// branch in the inner vectorized scan.
pub const MATCH_CHUNK_ENTRIES: usize = 8_192;

// --- Strategy selection ----------------------------------------------------

/// Which directory-enumeration implementation to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnumStrategy {
    /// `GetFileInformationByHandleEx` with a 1 MiB buffer. Documented, with no
    /// ntdll linkage.
    #[default]
    HandleDirInfo,
    /// `FindFirstFileExW` + `FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`.
    /// Also the only API that accepts a search pattern, so the server-side
    /// filter uses it regardless of this setting.
    FindFirstEx,
    /// `std::fs::read_dir`. Always available; the fallback.
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

    /// Strategies to try in order.
    ///
    /// Advance only on `Unsupported`; other errors are genuine answers, and
    /// retrying them through another API only adds latency.
    pub fn chain(self) -> &'static [EnumStrategy] {
        match self {
            Self::HandleDirInfo => &[Self::HandleDirInfo, Self::FindFirstEx, Self::StdReadDir],
            Self::FindFirstEx => &[Self::FindFirstEx, Self::StdReadDir],
            Self::StdReadDir => &[Self::StdReadDir],
        }
    }
}

/// Which matching implementation to use.
///
/// The naive matcher is a fallback selectable without a rebuild.
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
/// [`Self::App`] opens the single selected file. [`Self::Pdf`] gathers every
/// page of the base name and opens one assembled PDF. [`Self::Auto`] hands the file
/// to Windows. See [`crate::open::route_of`] for how a mode and file name
/// become a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerKind {
    /// Whatever Windows has registered for the file.
    #[default]
    Auto,
    /// Every page of the base name, merged into one PDF and opened with the
    /// system's `.pdf` handler. A file a configured viewer claims goes to that
    /// viewer instead.
    Pdf,
    /// The single selected file, handed to the configured viewer at this
    /// position in [`Viewers`].
    ///
    /// An index so the mode stays `Copy`; only meaningful against the list it
    /// came from. An out-of-range index behaves as [`Self::Auto`].
    App(usize),
}

/// How much of itself a result row shows.
///
/// [`Self::Detailed`] adds the folder under the name, which distinguishes
/// same-named files in different project folders, at the cost of fewer rows per
/// screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResultLayout {
    /// One line: an icon, the name, the drive it came off.
    #[default]
    Compact,
    /// Two: the name over the folder it is in.
    Detailed,
}

impl ResultLayout {
    pub const ALL: [Self; 2] = [Self::Compact, Self::Detailed];

    /// The spelling written back to the config file, so it must be one
    /// [`Self::parse`] accepts.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Detailed => "detailed",
        }
    }

    /// How the settings window spells it.
    pub const fn display(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Detailed => "Detailed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(text.trim()))
    }

    /// How far `PageUp` and `PageDown` move: one screen of this layout.
    ///
    /// The geometry behind these numbers is asserted beside the row heights in
    /// `gui::theme`.
    pub const fn rows_per_page(self) -> usize {
        match self {
            Self::Compact => VISIBLE_ROWS,
            Self::Detailed => VISIBLE_ROWS_DETAILED,
        }
    }
}

/// Where the panel sits when it is summoned.
///
/// [`Self::Free`] is a 600 by 400 window, placed twelve per cent down the
/// screen or where it was last dragged. The others pin a full-width bar to an
/// edge of the work area (not the monitor), so a bottom bar sits above the
/// taskbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dock {
    #[default]
    Free,
    Top,
    Bottom,
}

impl Dock {
    pub const ALL: [Self; 3] = [Self::Free, Self::Top, Self::Bottom];

    /// The spelling written back to the config file, so it must be one
    /// [`Self::parse`] accepts.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|d| d.name().eq_ignore_ascii_case(text.trim()))
    }

    /// Whether the panel is against an edge rather than floating.
    pub const fn is_docked(self) -> bool {
        !matches!(self, Self::Free)
    }
}

/// Which palette the panel is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Light,
    Dark,
    /// Follow the Windows setting.
    System,
}

impl ThemeChoice {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" | "auto" => Some(Self::System),
            _ => None,
        }
    }

    /// The spelling written back to the config file, so it must be one
    /// [`Self::parse`] accepts.
    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    /// Whether to draw dark, given what the system says.
    ///
    /// The system answer is passed in because this module does no I/O.
    pub fn is_dark(self, system_is_dark: bool) -> bool {
        match self {
            Self::Light => false,
            Self::Dark => true,
            Self::System => system_is_dark,
        }
    }
}

impl ViewerKind {
    /// Every mode the configured viewers allow, in F2 order.
    pub fn all(viewers: &Viewers) -> Vec<Self> {
        [Self::Auto, Self::Pdf]
            .into_iter()
            .chain((0..viewers.len()).map(Self::App))
            .collect()
    }

    /// Reads a mode from its configuration spelling: one of the built-in two,
    /// or the name of a configured viewer.
    pub fn parse(s: &str, viewers: &Viewers) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "by-extension" => Some(Self::Auto),
            "pdf" | "merge" | "merged" => Some(Self::Pdf),
            other => viewers.find(other).map(Self::App),
        }
    }

    /// Whether `name` is one of the spellings the built-in modes own, so no
    /// configured viewer may take it.
    pub fn is_reserved(name: &str) -> bool {
        matches!(
            name.trim().to_ascii_lowercase().as_str(),
            "auto" | "by-extension" | "pdf" | "merge" | "merged"
        )
    }

    /// The configured viewer this mode names, if it names one that exists.
    pub fn app(self, viewers: &Viewers) -> Option<&ViewerApp> {
        match self {
            Self::App(i) => viewers.get(i),
            _ => None,
        }
    }

    /// This mode, with an out-of-range viewer index replaced by `Auto`.
    pub fn within(self, viewers: &Viewers) -> Self {
        match self {
            Self::App(i) if i >= viewers.len() => Self::Auto,
            other => other,
        }
    }

    /// The spelling written to the config file.
    ///
    /// Must be one `parse` accepts against the same list, because an unknown
    /// value is a startup error; `every_name_the_writer_emits_parses_back`
    /// checks this.
    pub fn name(self, viewers: &Viewers) -> &str {
        match self.within(viewers) {
            Self::Auto => "auto",
            Self::Pdf => "pdf",
            Self::App(i) => &viewers.all()[i].name,
        }
    }

    /// How the mode is spelled on screen.
    ///
    /// Separate from [`Self::name`], the configuration spelling. A configured
    /// viewer is shown as its name is written.
    pub fn display(self, viewers: &Viewers) -> &str {
        match self.within(viewers) {
            Self::Auto => "Auto",
            Self::Pdf => "PDF",
            Self::App(i) => &viewers.all()[i].name,
        }
    }

    /// What F2 does: the built-in two, then every configured viewer in the
    /// order the file lists them, then round again.
    pub fn next(self, viewers: &Viewers) -> Self {
        match self.within(viewers) {
            Self::Auto => Self::Pdf,
            Self::Pdf if viewers.is_empty() => Self::Auto,
            Self::Pdf => Self::App(0),
            Self::App(i) if i + 1 < viewers.len() => Self::App(i + 1),
            Self::App(_) => Self::Auto,
        }
    }
}

/// The routing table the shipped defaults describe.
///
/// Parsed from `assets/default_config.toml` through the ordinary loader, so the
/// file written on first run and the compiled-in fallback cannot drift.
pub fn default_routes() -> Routes {
    file::builtin().routes
}

/// Two enabled drives for tests to search: the flat `shared` at
/// [`FLAT_FIXTURE_PATH`] and the tree `projects` at `R:\`.
const FIXTURE_MAPPINGS: &str = r#"
[[mapping]]
name    = "shared"
path    = 'V:\shared\files'
kind    = "flat"
enabled = true
refresh = "auto"

[[mapping]]
name    = "projects"
path    = 'R:\'
kind    = "tree"
enabled = true
refresh = "manual"
"#;

/// The shipped template with [`FIXTURE_MAPPINGS`] ahead of its examples.
///
/// Test fixture only. The shipped template enables no drives, so tests that
/// search, index or report on drives start from this instead.
#[doc(hidden)]
pub fn fixture_toml() -> &'static str {
    static TOML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let template = file::DEFAULT_CONFIG_TOML;
        let version = template
            .find("\nversion = ")
            .expect("the template declares a version");
        let end = version + 1 + template[version + 1..].find('\n').unwrap_or(0);
        format!("{}{FIXTURE_MAPPINGS}{}", &template[..end], &template[end..])
    });
    &TOML
}

/// The routing table of [`fixture_toml`]. Test fixture only.
#[doc(hidden)]
pub fn fixture_routes() -> Routes {
    file::parse(
        fixture_toml(),
        std::path::Path::new("<fixture>"),
        crate::paths::ConfigSource::BuiltIn,
    )
    .expect("the test fixture configuration must parse")
    .routes
}

/// Which configuration file to read, if any.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConfigChoice {
    /// `%APPDATA%\files\config.toml`, created from the shipped defaults if
    /// absent.
    #[default]
    Default,
    /// An explicit `--config <path>`. A missing file is an error.
    Explicit(PathBuf),
    /// `--no-config`: run on the shipped defaults only.
    None,
}

impl ConfigChoice {
    /// The file this resolves to, where there is one.
    ///
    /// `Default` resolves to the same path `Settings::load` uses.
    pub fn path(&self) -> Option<std::path::PathBuf> {
        match self {
            Self::Explicit(path) => Some(path.clone()),
            Self::Default => file::default_config_path(),
            Self::None => None,
        }
    }
}

/// Why a setting cannot be written back to the configuration file.
///
/// Each variant tells the user something different to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// An environment variable holds it. Named, so it can be found and unset.
    Environment(&'static str),
    /// A command-line flag holds it, for this run only.
    CommandLine,
    /// There is no configuration file to write to at all.
    NoFile,
}

impl Pin {
    /// What to say beside a setting that cannot be changed.
    ///
    /// A lowercase fragment for the second half of a ` · ` join. See
    /// `crate::view`.
    pub fn detail(self) -> String {
        match self {
            Self::Environment(var) => format!("set by {var} \u{b7} this session only"),
            Self::CommandLine => "set on the command line \u{b7} this session only".into(),
            Self::NoFile => "no configuration file \u{b7} this session only".into(),
        }
    }
}

/// Resolved runtime settings.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The routing table. Immutable once loaded, shared by every thread.
    pub routes: Arc<Routes>,
    /// The short names configured for searches typed often.
    ///
    /// Shared because `Settings` is cloned into every worker.
    pub aliases: Arc<crate::alias::Aliases>,
    /// The programs `[[viewer]]` names, in the order the file lists them.
    ///
    /// Carried on every open request because a [`ViewerKind::App`] index is
    /// only meaningful against the list it came from.
    pub viewers: Arc<Viewers>,
    pub enum_strategy: EnumStrategy,
    pub matcher: MatcherKind,
    /// Server-side wildcard filtering.
    ///
    /// Off by default; enable only after `--bench` confirms the server's
    /// pattern matching drops nothing.
    pub server_filter: bool,
    pub persist: bool,
    /// Whether the status line says a share needs refreshing.
    ///
    /// When off, share ages are still shown in the share list.
    pub stale_notices: bool,
    /// Whether a message may carry the technical detail behind it.
    ///
    /// Off by default, so a failure reads as "projects cannot be reached" rather
    /// than an OS error. `--doctor` output is never gated by this.
    pub dev_mode: bool,
    /// Whether clicking on something else puts the panel away.
    pub hide_on_blur: bool,
    /// Whether the panel goes away once a file is on its way.
    ///
    /// Open results that must be seen arrive in a message box, since the
    /// panel is gone by then; see [`crate::notify`]. Applies per action: a
    /// copy leaves the panel up so its toast can be read. See
    /// [`crate::view::actions::Action::hides`].
    pub hide_after_opening: bool,
    /// Whether Escape puts the panel away rather than clearing the box.
    ///
    /// When off, Escape clears the search and only the hotkey hides the panel.
    pub hide_on_escape: bool,
    /// Whether an assembled document is handed to the viewer read-only.
    ///
    /// On by default because merged documents are cached under a hash of their
    /// contents; an edit saved back would be served for every later open.
    /// Affects only files this program wrote, not PDFs opened from the share.
    pub pdf_read_only: bool,
    /// How many shares may be walked at once.
    ///
    /// Multiplies with [`WALK_CONCURRENCY`]: the default of 2 means sixteen
    /// outstanding requests against the file server.
    pub max_concurrent_scans: usize,
    /// Live change notification for the walked tree.
    ///
    /// On by default, unlike [`Self::server_filter`]: a watch that silently
    /// stops is bounded by [`TREE_RESCAN_FLOOR`] and reported in the status
    /// line, whereas a faulty filter makes files unfindable.
    pub live_updates: bool,
    pub cache_dir: Option<PathBuf>,
    /// Where to append the index decision log, if anywhere.
    ///
    /// Off by default. Diagnoses unexpected index reloads on the affected
    /// machine; see [`crate::index::log`].
    pub index_log: Option<PathBuf>,
    /// Remember searches between runs, for recall with the Up arrow.
    pub history: bool,
    /// Where history is stored. `None` disables storage but keeps in-session
    /// recall.
    pub history_path: Option<PathBuf>,
    /// Where the panel remembers the spot it was dragged to.
    ///
    /// Derived, not configured. `None` means the position is forgotten on
    /// exit. See [`crate::placement`].
    pub placement_path: Option<PathBuf>,
    /// The chord that summons the window into the compact overlay.
    ///
    /// Parsed at load so a typo is reported against its configuration line.
    pub hotkey: crate::hotkey::spec::HotkeySpec,
    /// The viewer at startup.
    ///
    /// Only the initial value. The current viewer, changed by F2, lives in
    /// `AppState::viewer`, since `Settings` is cloned into every worker.
    pub viewer: ViewerKind,
    /// Which palette the panel is drawn in.
    pub theme: ThemeChoice,
    /// What the compositor is asked to put behind the panel.
    ///
    /// Read only at startup: `DWMWA_SYSTEMBACKDROP_TYPE` is set once on the
    /// window handle. See [`crate::gui::window::Material`].
    pub backdrop: crate::gui::window::Material,
    /// How much of itself a result row shows.
    pub result_layout: ResultLayout,
    /// Whether the panel floats or is pinned to an edge of the screen.
    pub dock: Dock,
    /// How many results sit side by side on a line, from one to
    /// [`MAX_COLUMNS`].
    pub columns: usize,
    /// Whether to check GitHub for a newer version when no `update_from`
    /// share is configured.
    pub check_for_updates: bool,
    /// Overrides the system's `.pdf` association when set.
    ///
    /// Not validated at load, unlike share paths: a missing viewer fails
    /// visibly on use, and the same file roams to machines without it.
    /// `--doctor` and a toast report the problem instead.
    pub pdf_viewer: Option<PathBuf>,
    /// What was done to a configuration too old for this build to read.
    ///
    /// Carried so the rewrite can be reported to the user.
    pub migrated: Option<file::Migrated>,
    /// The folder to look in for a newer version.
    ///
    /// Unset disables share-based updates entirely: no thread, no round trip,
    /// nothing on screen.
    pub update_from: Option<PathBuf>,

    /// Whether a configuration file was actually read.
    ///
    /// When false, settings changed in the window last only for the session.
    pub have_file: bool,
    /// Settings a command-line flag has fixed for this run.
    ///
    /// One bit per [`write::SettingKey`]; see [`Self::pin`]. The environment
    /// needs no record because it cannot change while the process runs.
    pub cli_pinned: u32,
    /// The files that are never shown, however well they match.
    ///
    /// Derived from the list and the flag together so no consumer can read one
    /// without the other. Shared by every worker and read in the matcher's
    /// inner loop.
    pub hidden: Arc<hidden::Hidden>,
}

impl Default for Settings {
    fn default() -> Self {
        Self::with_routes(Arc::new(default_routes()), |s| s)
    }
}

impl Settings {
    /// Defaults over [`fixture_routes`]. Test fixture only.
    #[doc(hidden)]
    pub fn fixture() -> Self {
        Self::with_routes(Arc::new(fixture_routes()), |s| s)
    }
}

impl Settings {
    /// Why a setting cannot be written back, if it cannot.
    ///
    /// The environment and command line override the file, so writing a key
    /// either holds would change nothing. The window greys such a field.
    pub fn pin(&self, key: write::SettingKey) -> Option<Pin> {
        if self.cli_pinned & key.bit() != 0 {
            return Some(Pin::CommandLine);
        }
        // Must match `apply_file_settings`: an empty value counts only for
        // keys where it means "nothing, for this run" (the hide list).
        let set = if key.empty_is_a_value() {
            std::env::var(key.env()).is_ok()
        } else {
            env_str(key.env()).is_some()
        };
        if set {
            return Some(Pin::Environment(key.env()));
        }
        if !self.have_file {
            return Some(Pin::NoFile);
        }
        None
    }

    /// Whether a change to this setting would persist across restarts.
    pub fn can_save(&self, key: write::SettingKey) -> bool {
        self.pin(key).is_none()
    }

    /// Records that a command-line flag has fixed this setting.
    ///
    /// Called by the flag parser as it applies each override.
    pub fn pin_to_session(&mut self, key: write::SettingKey) {
        self.cli_pinned |= key.bit();
    }

    /// Builds default settings around a routing table, then applies `tweak`.
    pub fn with_routes(routes: Arc<Routes>, tweak: impl FnOnce(Self) -> Self) -> Self {
        tweak(Self {
            routes,
            aliases: Arc::new(crate::alias::Aliases::default()),
            viewers: Arc::new(Viewers::default()),
            enum_strategy: EnumStrategy::default(),
            matcher: MatcherKind::default(),
            server_filter: false,
            live_updates: true,
            persist: true,
            stale_notices: true,
            dev_mode: false,
            hide_on_blur: true,
            hide_after_opening: true,
            hide_on_escape: true,
            pdf_read_only: true,
            max_concurrent_scans: DEFAULT_MAX_CONCURRENT_SCANS,
            cache_dir: default_cache_dir(),
            index_log: None,
            history: true,
            history_path: crate::history::default_path(),
            placement_path: crate::placement::default_path(),
            hotkey: crate::hotkey::spec::HotkeySpec::default(),
            viewer: ViewerKind::default(),
            theme: ThemeChoice::default(),
            backdrop: crate::gui::window::Material::default(),
            result_layout: ResultLayout::default(),
            dock: Dock::default(),
            columns: 1,
            check_for_updates: true,
            pdf_viewer: None,
            migrated: None,
            update_from: None,
            // `load` sets this once a file is read; test fixtures and
            // `--no-config` sessions must not claim they can save.
            have_file: false,
            cli_pinned: 0,
            // Set here as well as in the shipped TOML, because
            // `write_default_if_absent` never rewrites an existing file.
            hidden: Arc::new(hidden::Hidden::new(
                hidden::DEFAULT_HIDE_EXTENSIONS,
                hidden::DEFAULT_HIDE_SYSTEM_FILES,
            )),
        })
    }

    /// Replaces whichever half of [`Self::hidden`] was specified.
    ///
    /// The list and the flag can arrive from different layers; setting them
    /// separately would rebuild `Hidden` from the other's default.
    fn set_hidden(&mut self, extensions: Option<Vec<String>>, system: Option<bool>) {
        if extensions.is_none() && system.is_none() {
            return;
        }
        // Round-trips through the dotted form `Hidden` stores; its constructor
        // strips the dots again.
        let keep: Vec<String> = self.hidden.suffixes().map(str::to_string).collect();
        self.hidden = Arc::new(hidden::Hidden::new(
            &extensions.unwrap_or(keep),
            system.unwrap_or(self.hidden.hides_system()),
        ));
    }

    /// Settings over a single mapping, for tests and diagnostics.
    pub fn for_mapping(name: &str, path: impl Into<PathBuf>, kind: MappingKind) -> Self {
        Self::with_routes(Arc::new(Routes::single(name, path.into(), kind)), |s| s)
    }

    /// Reads overrides from the environment, ignoring anything unparseable
    /// rather than refusing to start.
    pub fn from_env() -> Self {
        Self::from_env_with(default_routes())
    }

    /// Loads configuration, then layers the environment over it.
    ///
    /// Precedence, lowest first: shipped defaults, the config file, the
    /// environment, the command line (applied by the caller). A bad file is
    /// returned as errors rather than replaced by defaults; see [`mod@file`].
    pub fn load(choice: &ConfigChoice) -> Result<Self, Vec<file::ConfigError>> {
        // Whether a file was read decides whether settings can be saved.
        let mut have_file = false;
        let mut migrated = None;
        let parsed = match choice {
            ConfigChoice::None => file::builtin(),
            ConfigChoice::Explicit(path) => {
                have_file = true;
                let (parsed, was) = file::load_migrating(path, true)?;
                migrated = was;
                parsed
            }
            ConfigChoice::Default => match file::default_config_path() {
                Some(path) => {
                    // Best effort: a read-only profile means no file, and the
                    // built-in defaults are the same bytes.
                    let _ = file::write_default_if_absent(&path);
                    if path.exists() {
                        have_file = true;
                        let (parsed, was) = file::load_migrating(&path, false)?;
                        migrated = was;
                        parsed
                    } else {
                        file::builtin()
                    }
                }
                None => file::builtin(),
            },
        };

        let mut s = Self::from_env_with(parsed.routes);
        s.aliases = Arc::new(parsed.aliases);
        s.viewers = Arc::new(parsed.viewers);
        s.migrated = migrated;
        s.apply_file_settings(&parsed.settings);
        // The only part of `Settings::pin`'s answer that cannot be re-read
        // later.
        s.have_file = have_file;
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
        if env_usize("FILES_MAX_CONCURRENT_SCANS").is_none()
            && let Some(v) = f.max_concurrent_scans
        {
            self.max_concurrent_scans = v;
        }
        if env_bool("FILES_STALE_NOTICES").is_none()
            && let Some(v) = f.stale_notices
        {
            self.stale_notices = v;
        }
        if env_bool("FILES_DEV_MODE").is_none()
            && let Some(v) = f.dev_mode
        {
            self.dev_mode = v;
        }
        if env_bool("FILES_HIDE_ON_BLUR").is_none()
            && let Some(v) = f.hide_on_blur
        {
            self.hide_on_blur = v;
        }
        if env_bool("FILES_HIDE_AFTER_OPENING").is_none()
            && let Some(v) = f.hide_after_opening
        {
            self.hide_after_opening = v;
        }
        if env_bool("FILES_HIDE_ON_ESCAPE").is_none()
            && let Some(v) = f.hide_on_escape
        {
            self.hide_on_escape = v;
        }
        if env_bool("FILES_PDF_READ_ONLY").is_none()
            && let Some(v) = f.pdf_read_only
        {
            self.pdf_read_only = v;
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
        if env_bool("FILES_LIVE_UPDATES").is_none()
            && let Some(v) = f.live_updates
        {
            self.live_updates = v;
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
        if env_str("FILES_HOTKEY").is_none()
            && let Some(v) = f.hotkey
        {
            self.hotkey = v;
        }
        // Read here because a viewer name resolves only once the file has
        // listed the viewers. A set `FILES_VIEWER` still outranks the file
        // even when it names nothing, since the environment pins the setting.
        let viewers = Arc::clone(&self.viewers);
        if let Some(env) = env_str("FILES_VIEWER") {
            match ViewerKind::parse(&env, &viewers) {
                Some(v) => self.viewer = v,
                None => log::warn!("FILES_VIEWER={env:?} names no viewer; ignored"),
            }
        } else if let Some(v) = f
            .viewer
            .as_deref()
            .and_then(|v| ViewerKind::parse(v, &viewers))
        {
            self.viewer = v;
        }
        if env_str("FILES_PDF_VIEWER").is_none()
            && let Some(v) = &f.pdf_viewer
        {
            self.pdf_viewer = Some(v.clone());
        }
        if env_str("FILES_UPDATE_FROM").is_none()
            && let Some(v) = &f.update_from
        {
            self.update_from = Some(v.clone());
        }
        if env_str("FILES_INDEX_LOG").is_none()
            && let Some(v) = &f.index_log
        {
            self.index_log = Some(v.clone());
        }
        if env_str("FILES_THEME").is_none()
            && let Some(v) = f.theme.as_deref().and_then(ThemeChoice::parse)
        {
            self.theme = v;
        }
        if env_str("FILES_BACKDROP").is_none()
            && let Some(v) = f
                .backdrop
                .as_deref()
                .and_then(crate::gui::window::Material::parse)
        {
            self.backdrop = v;
        }
        if env_str("FILES_RESULT_LAYOUT").is_none()
            && let Some(v) = f.result_layout.as_deref().and_then(ResultLayout::parse)
        {
            self.result_layout = v;
        }
        if env_str("FILES_DOCK").is_none()
            && let Some(v) = f.dock.as_deref().and_then(Dock::parse)
        {
            self.dock = v;
        }
        if env_str("FILES_COLUMNS").is_none()
            && let Some(v) = f.columns
        {
            self.columns = v;
        }
        if env_bool("FILES_CHECK_FOR_UPDATES").is_none()
            && let Some(v) = f.check_for_updates
        {
            self.check_for_updates = v;
        }
        self.set_hidden(
            // `var`, not `env_str`: an empty `FILES_HIDE_EXTENSIONS` means
            // "hide nothing for this run", and `env_str` would treat it as
            // unset.
            std::env::var("FILES_HIDE_EXTENSIONS")
                .is_err()
                .then(|| f.hide_extensions.clone())
                .flatten(),
            env_bool("FILES_HIDE_SYSTEM_FILES")
                .is_none()
                .then_some(f.hide_system_files)
                .flatten(),
        );
    }

    /// As [`Settings::from_env`], but over a supplied routing table.
    pub fn from_env_with(routes: Routes) -> Self {
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
        if let Some(v) = env_usize("FILES_MAX_CONCURRENT_SCANS") {
            s.max_concurrent_scans = v;
        }
        if let Some(v) = env_bool("FILES_STALE_NOTICES") {
            s.stale_notices = v;
        }
        if let Some(v) = env_bool("FILES_DEV_MODE") {
            s.dev_mode = v;
        }
        if let Some(v) = env_bool("FILES_HIDE_ON_BLUR") {
            s.hide_on_blur = v;
        }
        if let Some(v) = env_bool("FILES_HIDE_AFTER_OPENING") {
            s.hide_after_opening = v;
        }
        if let Some(v) = env_bool("FILES_HIDE_ON_ESCAPE") {
            s.hide_on_escape = v;
        }
        if let Some(v) = env_bool("FILES_PDF_READ_ONLY") {
            s.pdf_read_only = v;
        }
        if let Some(v) = env_bool("FILES_LIVE_UPDATES") {
            s.live_updates = v;
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
        // Unparseable environment values are ignored, unlike file values,
        // which are reported against their line.
        if let Some(v) = env_str("FILES_HOTKEY").and_then(|v| crate::hotkey::spec::parse(&v).ok()) {
            s.hotkey = v;
        }
        if let Some(v) = env_str("FILES_THEME").and_then(|v| ThemeChoice::parse(&v)) {
            s.theme = v;
        }
        if let Some(v) =
            env_str("FILES_BACKDROP").and_then(|v| crate::gui::window::Material::parse(&v))
        {
            s.backdrop = v;
        }
        if let Some(v) = env_str("FILES_RESULT_LAYOUT").and_then(|v| ResultLayout::parse(&v)) {
            s.result_layout = v;
        }
        if let Some(v) = env_str("FILES_DOCK").and_then(|v| Dock::parse(&v)) {
            s.dock = v;
        }
        // Out of range is ignored rather than clamped.
        if let Some(v) = env_usize("FILES_COLUMNS").filter(|n| *n <= MAX_COLUMNS) {
            s.columns = v;
        }
        if let Some(v) = env_bool("FILES_CHECK_FOR_UPDATES") {
            s.check_for_updates = v;
        }
        // `FILES_VIEWER` is re-read in `apply_file_settings` once the file has
        // listed the viewers; here it can name only the built-in modes.
        if let Some(v) =
            env_str("FILES_VIEWER").and_then(|v| ViewerKind::parse(&v, &Viewers::default()))
        {
            s.viewer = v;
        }
        if let Some(v) = env_str("FILES_PDF_VIEWER") {
            s.pdf_viewer = Some(PathBuf::from(v));
        }
        if let Some(v) = env_str("FILES_UPDATE_FROM") {
            s.update_from = Some(PathBuf::from(v));
        }
        // Accepts commas, spaces, tabs and semicolons. An empty value means
        // "hide nothing".
        s.set_hidden(
            std::env::var("FILES_HIDE_EXTENSIONS").ok().map(|v| {
                v.split([',', ' ', '\t', ';'])
                    .map(str::trim)
                    .filter(|e| !e.is_empty())
                    .map(str::to_string)
                    .collect()
            }),
            env_bool("FILES_HIDE_SYSTEM_FILES"),
        );
        s
    }
}

pub(crate) fn env_str(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Reads a duration given in whole seconds.
///
/// Zero and unparseable values are ignored: a zero probe interval would
/// busy-loop the index thread against the file server.
pub(crate) fn env_secs(key: &str) -> Option<Duration> {
    let secs: u64 = env_str(key)?.parse().ok()?;
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Reads a positive whole number; zero and unparseable values are ignored.
pub(crate) fn env_usize(key: &str) -> Option<usize> {
    let n: usize = env_str(key)?.parse().ok()?;
    (n > 0).then_some(n)
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
/// The index is memory-mapped, so it must live on a local disk rather than
/// over SMB.
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

    // --- hiding files -------------------------------------------------------

    /// Serialises the tests below, because `apply_file_settings` reads the
    /// process-wide environment and some tests write to it while `cargo test`
    /// runs them in parallel. Mirrors the guard in `crate::log`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        // A poisoned lock only means another test panicked; keep going.
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn applied(f: file::FileSettings) -> Settings {
        let _guard = lock();
        let mut s = Settings::default();
        s.apply_file_settings(&f);
        s
    }

    #[test]
    fn the_index_log_can_be_set_in_the_configuration_file() {
        let s = applied(file::FileSettings {
            index_log: Some(PathBuf::from(r"C:\temp\files.log")),
            ..Default::default()
        });
        assert_eq!(s.index_log, Some(PathBuf::from(r"C:\temp\files.log")));
    }

    #[test]
    fn an_index_log_in_the_environment_outranks_the_file() {
        const ENV: &str = "FILES_INDEX_LOG";
        let _guard = lock();

        // SAFETY: as in the test below - the lock makes the write safe, and
        // both calls happen before the guard is dropped.
        unsafe { std::env::set_var(ENV, r"C:\temp\from-env.log") };

        let mut s = Settings::from_env_with(default_routes());
        s.apply_file_settings(&file::FileSettings {
            index_log: Some(PathBuf::from(r"C:\temp\from-file.log")),
            ..Default::default()
        });

        // SAFETY: as above.
        unsafe { std::env::remove_var(ENV) };

        assert_eq!(
            s.index_log,
            Some(PathBuf::from(r"C:\temp\from-env.log")),
            "the file overrode the environment"
        );
    }

    /// An empty `FILES_HIDE_EXTENSIONS` means "hide nothing" and must outrank
    /// the configuration file like any other override.
    #[test]
    fn an_empty_override_still_outranks_the_configuration_file() {
        const ENV: &str = "FILES_HIDE_EXTENSIONS";
        let _guard = lock();

        // SAFETY: `set_var` and `remove_var` are unsafe because another thread
        // reading the environment at the same instant is a data race. The lock
        // above is what makes that impossible: every read of this variable in
        // this binary goes through `apply_file_settings`, which the other
        // tests in this module reach only via `applied`, which takes the same
        // lock - and both calls below happen before the guard is dropped.
        unsafe { std::env::set_var(ENV, "") };

        // Both steps of `load`, in order: the environment over the defaults,
        // then the file yielding to the environment. The override is applied
        // in the first step, so testing only the second would prove nothing.
        let mut s = Settings::from_env_with(default_routes());
        s.apply_file_settings(&file::FileSettings {
            hide_extensions: Some(vec!["db".into()]),
            ..Default::default()
        });

        // SAFETY: as above.
        unsafe { std::env::remove_var(ENV) };

        assert!(
            s.hidden.is_empty(),
            "the file overrode an override: {:?}",
            s.hidden.suffixes().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_built_in_default_hides_the_files_that_prompted_it() {
        let s = Settings::default();
        assert!(s.hidden.hides("Thumbs.db"));
        assert!(s.hidden.hides("shortcut.lnk"));
        assert!(!s.hidden.hides("AB-C-0704.pdf"));
        assert!(s.hidden.hides_system());
    }

    #[test]
    fn a_file_setting_replaces_the_built_in_list() {
        let s = applied(file::FileSettings {
            hide_extensions: Some(vec!["xyz".into()]),
            ..Default::default()
        });
        assert!(s.hidden.hides("a.xyz"));
        assert!(
            !s.hidden.hides("Thumbs.db"),
            "the default list was merged in"
        );
    }

    /// An empty list means "hide nothing", not "unset".
    #[test]
    fn an_empty_list_hides_nothing() {
        let s = applied(file::FileSettings {
            hide_extensions: Some(Vec::new()),
            ..Default::default()
        });
        assert!(s.hidden.is_empty());
        assert!(!s.hidden.hides("Thumbs.db"));
    }

    #[test]
    fn setting_one_half_leaves_the_other_alone() {
        let flag_only = applied(file::FileSettings {
            hide_system_files: Some(false),
            ..Default::default()
        });
        assert!(!flag_only.hidden.hides_system());
        assert!(
            flag_only.hidden.hides("Thumbs.db"),
            "the extension list was lost"
        );

        let list_only = applied(file::FileSettings {
            hide_extensions: Some(vec!["xyz".into()]),
            ..Default::default()
        });
        assert!(list_only.hidden.hides_system(), "the system flag was lost");
        assert!(list_only.hidden.hides("a.xyz"));
    }

    #[test]
    fn a_file_that_says_nothing_changes_nothing() {
        let s = applied(file::FileSettings::default());
        assert_eq!(s.hidden, Settings::default().hidden);
    }

    #[test]
    fn parses_matcher_kinds() {
        assert_eq!(MatcherKind::parse("simd"), Some(MatcherKind::Simd));
        assert_eq!(MatcherKind::parse("NAIVE"), Some(MatcherKind::Naive));
        assert_eq!(MatcherKind::parse(""), None);
    }

    fn two_viewers() -> Viewers {
        Viewers::new(vec![
            ViewerApp::new("sketch", "sketch.exe", &["tif"]),
            ViewerApp::new("Notes", "notepad.exe", &[]),
        ])
    }

    #[test]
    fn parses_every_viewer_spelling() {
        let none = Viewers::default();
        assert_eq!(ViewerKind::parse("auto", &none), Some(ViewerKind::Auto));
        assert_eq!(ViewerKind::parse(" AUTO ", &none), Some(ViewerKind::Auto));
        assert_eq!(
            ViewerKind::parse("by-extension", &none),
            Some(ViewerKind::Auto)
        );
        assert_eq!(ViewerKind::parse("pdf", &none), Some(ViewerKind::Pdf));
        assert_eq!(ViewerKind::parse("  MERGE ", &none), Some(ViewerKind::Pdf));
        // No built-in modes beyond those two.
        assert_eq!(ViewerKind::parse("sketch", &none), None);

        let two = two_viewers();
        assert_eq!(ViewerKind::parse("SKETCH", &two), Some(ViewerKind::App(0)));
        assert_eq!(ViewerKind::parse("notes", &two), Some(ViewerKind::App(1)));
        assert_eq!(ViewerKind::parse("notepad", &two), None);
    }

    #[test]
    fn opening_by_file_type_is_the_default() {
        assert_eq!(Settings::default().viewer, ViewerKind::Auto);
        assert!(Settings::default().viewers.is_empty());
    }

    #[test]
    fn the_viewer_cycles_through_every_mode_and_closes() {
        for viewers in [Viewers::default(), two_viewers()] {
            let all = ViewerKind::all(&viewers);
            assert_eq!(all.len(), 2 + viewers.len());
            for &start in &all {
                let mut seen = vec![start];
                let mut v = start;
                for _ in 1..all.len() {
                    v = v.next(&viewers);
                    assert!(!seen.contains(&v), "{v:?} came round twice");
                    seen.push(v);
                }
                assert_eq!(v.next(&viewers), start, "the cycle does not close");
                assert_ne!(
                    start.next(&viewers),
                    start,
                    "F2 must always change something"
                );
            }
        }
    }

    /// An unparseable written value would be a startup error on next launch.
    #[test]
    fn every_name_the_writer_emits_parses_back() {
        let viewers = two_viewers();
        for v in ViewerKind::all(&viewers) {
            assert_eq!(ViewerKind::parse(v.name(&viewers), &viewers), Some(v));
        }
    }

    #[test]
    fn a_viewer_that_is_gone_behaves_as_auto() {
        let none = Viewers::default();
        assert_eq!(ViewerKind::App(3).within(&none), ViewerKind::Auto);
        assert_eq!(ViewerKind::App(3).name(&none), "auto");
        assert_eq!(ViewerKind::App(3).next(&none), ViewerKind::Pdf);
        assert!(ViewerKind::App(0).app(&none).is_none());
    }

    #[test]
    fn the_built_in_spellings_are_reserved() {
        for name in ["auto", "PDF", " merge ", "by-extension", "merged"] {
            assert!(ViewerKind::is_reserved(name), "{name}");
        }
        assert!(!ViewerKind::is_reserved("sketch"));
    }

    #[test]
    fn the_built_in_defaults_have_nowhere_to_persist_a_viewer() {
        let s = Settings::load(&ConfigChoice::None).expect("built-ins must load");
        assert!(!s.can_save(write::SettingKey::Viewer));
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
        // Enabled only after --bench proves the server's pattern matching
        // returns a superset of a full enumeration.
        assert!(!Settings::default().server_filter);
    }

    /// Ordering of the query-length limits, checked at compile time.
    const _: () = assert!(MIN_TERM_LEN >= 1);
    const _: () = assert!(MIN_SERVER_QUERY_LEN >= MIN_TERM_LEN);
    const _: () = assert!(MIN_REMEMBERED_LEN >= MIN_TERM_LEN);
    const _: () = assert!(MAX_SERVER_QUERY_LEN > MIN_SERVER_QUERY_LEN);

    // --- what may be written back -------------------------------------------

    use write::SettingKey;

    #[test]
    fn a_setting_can_be_saved_when_a_file_was_read_and_nothing_outranks_it() {
        let _guard = lock();
        let s = Settings {
            have_file: true,
            ..Settings::default()
        };
        assert_eq!(s.pin(SettingKey::Theme), None);
        assert!(s.can_save(SettingKey::Theme));
    }

    /// Covers `--no-config` and a profile with nowhere to put a file.
    #[test]
    fn nothing_can_be_saved_when_there_is_no_configuration_file() {
        let _guard = lock();
        let s = Settings::default();
        assert!(!s.have_file);
        for key in SettingKey::ALL {
            assert_eq!(s.pin(key), Some(Pin::NoFile), "{}", key.name());
        }
    }

    #[test]
    fn a_command_line_flag_pins_one_setting_and_leaves_the_others_alone() {
        let _guard = lock();
        let mut s = Settings {
            have_file: true,
            ..Settings::default()
        };
        s.pin_to_session(SettingKey::Viewer);

        assert_eq!(s.pin(SettingKey::Viewer), Some(Pin::CommandLine));
        assert!(!s.can_save(SettingKey::Viewer));
        assert!(s.can_save(SettingKey::Theme), "a flag pinned the wrong key");
    }

    #[test]
    fn an_environment_variable_pins_a_setting_and_says_which_one_it_is() {
        let _guard = lock();
        let s = Settings {
            have_file: true,
            ..Settings::default()
        };

        // SAFETY: as in the test above - every read of this variable in this
        // binary happens under the same lock, and both calls below are inside
        // the guard.
        unsafe { std::env::set_var("FILES_THEME", "dark") };
        let pin = s.pin(SettingKey::Theme);
        // SAFETY: as above.
        unsafe { std::env::remove_var("FILES_THEME") };

        assert_eq!(pin, Some(Pin::Environment("FILES_THEME")));
    }

    #[test]
    fn every_writable_key_has_a_bit_of_its_own() {
        let mut seen = 0u32;
        for key in SettingKey::ALL {
            assert_eq!(seen & key.bit(), 0, "{} shares a bit", key.name());
            seen |= key.bit();
        }
        assert_eq!(seen.count_ones() as usize, SettingKey::ALL.len());
    }

    #[test]
    fn every_writable_key_names_an_environment_variable_the_loader_reads() {
        for key in SettingKey::ALL {
            let env = key.env();
            assert!(env.starts_with("FILES_"), "{env} is not one of ours");
            assert!(
                crate::cli::HELP.contains(env),
                "{env} is not documented in --help"
            );
        }
    }

    #[test]
    fn every_reason_a_setting_cannot_be_saved_explains_itself() {
        for pin in [
            Pin::Environment("FILES_THEME"),
            Pin::CommandLine,
            Pin::NoFile,
        ] {
            let detail = pin.detail();
            assert!(!detail.is_empty());
            crate::view::style::check_all(
                "a pin",
                [detail.as_str()],
                crate::view::style::Slot::Status,
            );
        }
    }
}
