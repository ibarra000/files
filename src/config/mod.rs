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
pub mod hidden;
pub mod migrate;
pub mod write;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::paths::{MappingKind, Routes};

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

/// How many results are on screen at once.
///
/// Here rather than beside the other layout numbers in `gui::theme` because the
/// state machine needs it: the arrows scroll a window over the result list, and
/// the window has to know how big it is. `keys.rs` used to reach into
/// `gui::theme` for this, which is a dependency pointing the wrong way - the
/// interaction model is meant to be drawable by anything.
///
/// Twelve rows of forty points is most of a laptop's vertical half. Past that
/// the panel stops being an overlay and starts being a file manager, which is a
/// different program; below it, a broad code spends too much of its time being
/// scrolled.
pub const VISIBLE_ROWS: usize = 12;

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
/// Folders below the root a live query descends, unless the mapping says
/// otherwise.
///
/// One: the share's own folder, answered by a single filtered round trip that
/// returns both the matching files and the matching *folder* names - which is
/// the common case, because a job code names a folder at least as often as it
/// names a file.
///
/// Deeper is not one more round trip, it is one per folder on the way down: a
/// filtered query cannot reveal a folder whose name does not match, so the
/// level above has to be listed in full first. Depth two against a root
/// holding two hundred folders is two hundred round trips for one keystroke,
/// which is a decision somebody should have to write down.
pub const DEFAULT_LIVE_DEPTH: u16 = 1;

/// Deepest a configuration may ask a live query to go.
///
/// Four. With a branching factor of eight, depth four is already 512
/// directories against a round-trip budget of 64 - so past this the number
/// stops describing anything that will actually happen, and a number in a
/// configuration file that will not be honoured is worse than no key at all.
pub const MAX_LIVE_DEPTH: u16 = 4;

/// Round trips one live pass may spend.
///
/// Sixty-four is the root's filtered query plus sixty-three folders expanded
/// or listed. On a LAN, where a `FindFirstFileExW` round trip is well under a
/// millisecond, that is under 60 ms - so the budget is not what bounds the
/// latency, it is what bounds the *server*. Over the VPN this is also run
/// across, where a round trip is 30-80 ms, sixty-four of them is four seconds,
/// which is why there is a deadline as well.
pub const LIVE_ROUND_TRIP_BUDGET: u32 = 64;

/// Where a live pass stops, whatever it has reached by then.
///
/// Shorter than [`REMEMBER_DEBOUNCE`] on purpose: that one decides when the
/// code on the line is a code somebody meant, and a live answer landing after
/// it would leave the recall list and the result list disagreeing about which
/// query was the finished one. Longer than any local answer by three orders of
/// magnitude, so the two phases are never mistaken for one.
pub const LIVE_DEADLINE: Duration = Duration::from_millis(1_200);

/// Quiet period after the last keystroke before a live share is asked.
///
/// Twice [`SEARCH_DEBOUNCE`], and the doubling is the justification. That one
/// is paced by what a *reader* can use, because the match itself is free. This
/// one is paced by what somebody else's file server can afford: a leading `*`
/// defeats the NTFS index, so the server walks its own directory to answer and
/// the answer costs it real CPU rather than a seek.
///
/// Somebody reading a code off a drawing pauses about 200-300 ms between
/// groups, which is what 300 ms was chosen to sit just past. Six hundred
/// clears the pause between a code and the modifier after it as well, so
/// `11-D-0704` costs one query rather than the two that 300 ms lets through -
/// halving the load across the fleet for 300 ms nobody notices, because the
/// local results are already on screen by then.
pub const LIVE_DEBOUNCE: Duration = Duration::from_millis(600);

/// Floor between two queries of the same share, whatever asks for them.
///
/// One second. The debounce above bounds what *typing* can cause; this bounds
/// everything else - a held key, a paste loop, a burst of index updates each
/// re-running the match. One client can therefore cost a share at most one
/// query per second however pathological its input.
pub const LIVE_MIN_SPACING: Duration = Duration::from_secs(1);

/// Refusals to filter before a live share is switched off for the process.
///
/// Three, matching [`SERVER_FILTER_MISS_LIMIT`]. A source that will not push
/// the filter down would answer every query with a full enumeration wearing a
/// filter, which is precisely the cost configuring the share this way was
/// meant to avoid - so the honest response is to stop and say so.
pub const LIVE_FAILURE_LIMIT: u32 = 3;

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

/// Quiet period after the last keystroke before the local match runs.
///
/// The match itself is sub-millisecond and touches no network, so this is not
/// about what the machine can afford - it is about what the screen should say.
/// Somebody reading a code off a drawing does not want the list for `11`, then
/// `11-`, then `11-D`; they want the list for the code they finished typing.
/// Answering every prefix meant a result set, a body change and a window resize
/// per character, for answers nobody reads.
///
/// Equal to [`VERIFY_DEBOUNCE`] deliberately: one pause, one answer, one server
/// check. `AppState::on_tick` fires them in that order and relies on it.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// Quiet period after the last keystroke before the authoritative server-side
/// verification runs. A leading `*` defeats the NTFS index, so this costs real
/// server CPU and must not fire per keystroke.
pub const VERIFY_DEBOUNCE: Duration = Duration::from_millis(300);

/// Quiet period before the pane beside the list asks what a file is.
///
/// A twentieth of [`VERIFY_DEBOUNCE`] and a different kind of pause. That one
/// waits for somebody to stop *typing*, which is a decision they are still
/// making; this one waits for them to stop *pointing*, which is already made -
/// so it only has to be long enough that sweeping a mouse down twelve rows does
/// not spend twelve round trips on rows nobody stopped at.
///
/// Not zero, and the reason is where the answer comes from: `metadata` on a
/// drawing share reached over a VPN is tens of milliseconds, not microseconds.
pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(120);

/// Quiet period after the last keystroke before the code on the line reaches
/// the recall list.
///
/// Five times [`VERIFY_DEBOUNCE`], and a separate constant rather than a
/// multiple of it, because the two are paying for different things. That one
/// buys a round trip against somebody else's file server and is set by what a
/// server costs; this one decides what somebody reads off their recall list
/// tomorrow morning, and is set by how long a person pauses in the middle of
/// typing a code they are reading off a drawing.
///
/// Recording rode on the verification for most of this program's life, which
/// meant a third of a second. That is an ordinary mid-word pause, so `11`,
/// `11-D` and `11-D-07` all reached the list on the way to `11-D-0704`. A
/// second and a half is longer than a typist stops and shorter than anybody
/// notices, and `History::record` collapses the chain that gets through
/// anyway.
pub const REMEMBER_DEBOUNCE: Duration = Duration::from_millis(1_500);

/// How close together two clicks must be to count as a double-click.
///
/// The terminal reports presses, not clicks, so this is the program's own
/// definition. Matches the Windows default closely enough that a double-click
/// which selects a code in Explorer selects one here too.
pub const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// If a verify has not reported back by now, transition out of the spinner
/// regardless. Guards against a wedged or panicked worker.
pub const VERIFY_WATCHDOG: Duration = Duration::from_secs(10);

/// If the matcher has not answered an Enter by now, give the keystroke back.
///
/// Enter typed inside [`SEARCH_DEBOUNCE`] cannot open the row on screen - that
/// row answers the *previous* code - so it asks the matcher and opens whatever
/// comes back. This is the backstop for an answer that never does.
///
/// Far shorter than [`VERIFY_WATCHDOG`]: that one waits on somebody else's file
/// server, this one waits on a sweep over bytes this process already holds. A
/// second is a hundred times longer than it has ever taken.
pub const ENTER_WATCHDOG: Duration = Duration::from_secs(1);

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

/// How old a persisted index may get before it is called stale.
///
/// Advisory, not a rejection. It used to be one, and that was safe only while
/// every launch re-read the share anyway: with shares refreshed on demand, a
/// week-old index is an ordinary state rather than a fault, and discarding it
/// would leave nothing to serve and start exactly the full pass the on-demand
/// policy exists to avoid - simultaneously, on every machine whose cache was
/// built on the same rollout day.
///
/// A stale answer with an honest label beats an empty screen. The status line
/// says how old it is and names the share, and `F5` is one keystroke.
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

/// Spread applied to the full-rescan floor, in percent, added on top of it.
///
/// The probe jitter above does not reach a full pass: a scan is decided at a
/// probe wake by comparing against the floor, and a *tree* never probes at
/// all. So three hundred clients that started within a minute of each other
/// re-walked in permanent lockstep, and nothing in the schedule widened that
/// window - the floor is measured from the previous pass's completion, so each
/// cycle re-synchronised them.
///
/// One-sided, and deliberately: the floor is a promise about the longest an
/// index may go unchecked, so spreading a client *later* only ever costs
/// freshness it already agreed to, while spreading one earlier would break the
/// promise to buy nothing.
///
/// Twenty-five percent of half an hour is a seven-minute window, which is wide
/// enough that a fleet arrives as a trickle rather than a wall.
pub const FLOOR_SPREAD_PERCENT: u32 = 25;

/// Shortest gap between two writes of the index to disk.
///
/// The watcher's patches are what keep a manually-refreshed share current, and
/// they only survive a restart if they reach the cache. But the cache file is
/// written whole - around a hundred megabytes for the job share - so writing
/// on every two-second watch batch would cost more than the patching saves.
///
/// Five minutes is the compromise: the most a crash can cost is five minutes
/// of folder updates, against a full pass that costs nine hundred thousand
/// round trips.
pub const PERSIST_SPACING: Duration = Duration::from_secs(5 * 60);

/// Redraw cadence while something is animating. Nothing animates at a finer
/// granularity than a spinner frame, and the elapsed readout is rounded to
/// match so consecutive frames actually differ.
pub const ANIMATION_TICK: Duration = Duration::from_millis(100);

/// How often a visible countdown is redrawn.
///
/// Only ever armed while a share is unreachable and a retry is still in the
/// future, so a healthy idle session still costs nothing. One second rather
/// than a hundred milliseconds because `humanize::elapsed` renders the
/// sub-minute form to a tenth, and a tenth that moves ten times a second is
/// noise rather than information.
pub const COUNTDOWN_TICK: Duration = Duration::from_secs(1);

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

/// How many shares are walked at once, by default.
///
/// Multiplies against [`WALK_CONCURRENCY`], so two is sixteen directory reads
/// in flight - already twice what that constant was sized for, and the reason
/// this is not simply "all of them". A configuration naming ten tree shares
/// would otherwise open eighty concurrent requests against one file server on
/// its first run, which is the behaviour a server administrator blocks rather
/// than tunes.
///
/// Two rather than one because a share that is merely slow should not stall
/// every other share behind it, and because the second walk is usually
/// waiting on the network rather than on this machine.
pub const DEFAULT_MAX_CONCURRENT_SCANS: usize = 2;

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
/// They differ in more than which executable is spawned. `Avwin` opens the one
/// file the cursor is on, which is all it can do: the pages of a drawing set
/// are separate files on the share, and a viewer given one of them shows one
/// page. `Pdf` treats the code as naming a *document*, gathers every page of it
/// and hands over a single assembled PDF - which is what someone asking for
/// `11-D-0704` almost always meant.
///
/// `Auto` is neither, and picks between them per file. See
/// [`crate::open::route_of`], which is where a mode and a file name become a
/// destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerKind {
    /// Whatever suits the file: documents assembled, everything else - a
    /// drawing included - handed to `avwin.exe`.
    #[default]
    Auto,
    /// Every page of the code, merged into one PDF, opened with the system's
    /// `.pdf` handler. A drawing is the one thing this cannot assemble - there
    /// is no Rust that reads DWG - so a drawing still goes to `avwin.exe`.
    Pdf,
    /// The single selected file, handed to `avwin.exe`. The behaviour this
    /// program had before there was a choice, and still the way to see a `.pdf`
    /// or a `.dwg` in avwin rather than anywhere else.
    Avwin,
}

/// Which palette the panel is drawn in.
///
/// A setting rather than a follow of the Windows theme, which is what it used
/// to be. The panel is a small bright thing summoned over whatever somebody is
/// working in, and "what the rest of my desktop does" turned out to be a poor
/// proxy for "what I want this to look like": a machine in dark mode got a
/// near-black panel nobody had asked for.
///
/// [`Self::System`] keeps the old behaviour for anyone who wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    Light,
    Dark,
    /// Follow the Windows setting, as the panel used to.
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
    /// The system answer is passed in rather than read here: this module does
    /// no I/O and has never heard of a window.
    pub fn is_dark(self, system_is_dark: bool) -> bool {
        match self {
            Self::Light => false,
            Self::Dark => true,
            Self::System => system_is_dark,
        }
    }
}

impl ViewerKind {
    /// Every mode, for the tests that must cover all of them.
    ///
    /// A constant rather than a literal at each site: four test arrays used to
    /// spell `[Pdf, Avwin]` out, which means a new mode makes them quietly stop
    /// covering it instead of failing.
    pub const ALL: [Self; 3] = [Self::Auto, Self::Pdf, Self::Avwin];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "by-extension" => Some(Self::Auto),
            "pdf" | "merge" | "merged" => Some(Self::Pdf),
            "avwin" | "av" | "avwin.exe" => Some(Self::Avwin),
            _ => None,
        }
    }

    /// Whether `avwin.exe` can be reached from this mode.
    ///
    /// `Auto` used to be on this list, back when it sent everything that was
    /// not a document to avwin. It hands those to the system now, so a machine
    /// with no avwin installed is no longer warned at startup about a viewer
    /// nothing was going to ask for. F2 is still the way to reach it, and the
    /// warning is still right for anyone who does.
    pub fn may_use_avwin(self) -> bool {
        matches!(self, Self::Avwin)
    }

    /// The spelling written to the config file, so it must be one `parse`
    /// accepts. `every_key_the_writer_can_emit_is_an_accepted_setting` pins
    /// that, because an unknown value is a hard startup error.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Pdf => "pdf",
            Self::Avwin => "avwin",
        }
    }

    /// How the mode is spelled on screen.
    ///
    /// Separate from [`Self::name`], which is the configuration file's spelling
    /// and is round-tripped through [`Self::parse`]. Using a serialisation
    /// identifier as a label is how the footer came to say `viewer: pdf` in a
    /// panel where every other word is capitalised.
    pub fn display(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Pdf => "PDF",
            Self::Avwin => "avwin",
        }
    }

    /// What F2 does. A cycle rather than a boolean so a third viewer is one
    /// match arm rather than a rethink.
    pub fn next(self) -> Self {
        match self {
            Self::Pdf => Self::Avwin,
            Self::Avwin => Self::Auto,
            Self::Auto => Self::Pdf,
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

/// Why a setting cannot be written back to the configuration file.
///
/// Carried rather than flattened to a bool because the three have different
/// answers: two of them the user can undo, and which one it is decides what
/// they would have to do about it.
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
    /// A fragment, lowercase, for the second half of a ` · ` join - which is
    /// where every one of these is used. See `crate::view`.
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
    /// The short names configured for codes typed often.
    ///
    /// Shared for the same reason [`Self::routes`] is: `Settings` is cloned
    /// into the backend and every worker, and an alias table is read on a
    /// keystroke rather than copied on one.
    pub aliases: Arc<crate::alias::Aliases>,
    pub enum_strategy: EnumStrategy,
    pub matcher: MatcherKind,
    /// Server-side wildcard filtering. Ships **off** so it can be enabled only
    /// after `--bench` confirms the server's pattern matching drops nothing.
    pub server_filter: bool,
    pub persist: bool,
    /// Whether the status line says a share needs refreshing.
    ///
    /// On by default, because a list that has stopped tracking the drive is
    /// exactly the thing this program refuses to hide. Off for someone handed
    /// the tool who does not need current data and should not be nagged about
    /// it - the ages are still there in the share list, which they only see by
    /// asking for it.
    pub stale_notices: bool,
    /// Whether a message may carry the technical detail behind it.
    ///
    /// Off, so an ordinary failure reads as a sentence: "jobs cannot be
    /// reached" rather than "V:\Documents\custpro unreachable (os error
    /// 53)". The second is the more useful of the two to exactly one person,
    /// and they can turn it on.
    ///
    /// `--doctor` is not gated by this and never will be. It is the report a
    /// support call asks for, and a report that has to be switched on first is
    /// a report nobody has when they need it.
    pub dev_mode: bool,
    /// Whether the panel ever puts itself away.
    ///
    /// Off by default, which is a reversal. The panel used to vanish the
    /// instant Enter opened something, and everything the open had to say
    /// arrived afterwards from the worker thread - `Opening...`, the count of
    /// pages it had to skip, `Could not open ...` - onto a window that was
    /// already gone. None of it was ever read by anybody. Staying up is also
    /// what makes opening a second code a keystroke rather than a hotkey.
    ///
    /// On for anyone who wants the old behaviour. Escape and the hotkey close
    /// the panel either way: this is about the times it decides for itself.
    pub auto_hide: bool,
    /// Whether an assembled document is handed to the viewer read-only.
    ///
    /// On by default, and the reason is the cache rather than the share. A
    /// merged document is named after a hash of its own contents, so a viewer
    /// that saves a change back into it leaves a file whose name no longer
    /// describes it, and every later open of that code gets the edited copy.
    ///
    /// Off for somebody who genuinely wants to annotate what comes out and
    /// save it somewhere. It reaches only files this program wrote: a single
    /// PDF opened straight off the share is as writable as it is on disk.
    pub pdf_read_only: bool,
    /// How many shares may be walked at once.
    ///
    /// One walk already keeps [`WALK_CONCURRENCY`] directory reads in flight,
    /// so this multiplies against that: the shipped 2 is sixteen outstanding
    /// requests against a file server that belongs to somebody else. Raising
    /// it shortens a cold start over several shares and lengthens everyone
    /// else's afternoon.
    pub max_concurrent_scans: usize,
    /// Live change notification for the walked tree.
    ///
    /// Ships **on**, unlike [`Self::server_filter`], and the asymmetry is
    /// deliberate. A server filter that silently drops matches makes a file
    /// unfindable, so it has to be proven before it is trusted; a watch that
    /// silently stops firing costs nothing, because the rescan floor still
    /// re-walks the share every half hour and the status line says the watch
    /// is dead. The downside is bounded and the upside is a new job folder
    /// appearing in seconds rather than in half an hour.
    pub live_updates: bool,
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
    /// Where the panel remembers the spot it was dragged to.
    ///
    /// Derived rather than configured, exactly like [`Self::history_path`]:
    /// there is no `placement_path` key, so no existing configuration file can
    /// become a hard startup error over a feature it has never heard of. `None`
    /// means the panel can still be dragged, and simply forgets on exit.
    ///
    /// See [`crate::placement`] for why a position is not a setting.
    pub placement_path: Option<PathBuf>,
    /// The chord that summons the window into the compact overlay.
    ///
    /// Parsed at the edge rather than carried as text, so a typo is reported
    /// against the line of the configuration file that holds it instead of
    /// becoming a key that silently never fires.
    pub hotkey: crate::hotkey::spec::HotkeySpec,
    /// The viewer at startup.
    ///
    /// Deliberately the *initial* value and nothing more. F2 changes which
    /// viewer is in use, and that lives on `AppState`, not here: `Settings` is
    /// cloned into the backend and every worker, so a mutable field would be
    /// one truth with several stale copies of it. See `AppState::viewer`.
    pub viewer: ViewerKind,
    /// Which palette the panel is drawn in.
    pub theme: ThemeChoice,
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
    /// What was done to a configuration too old for this build to read.
    ///
    /// Carried so it can be said out loud. A program that rewrites a file the
    /// user maintains and never mentions it is one whose first symptom is a
    /// comment of theirs having vanished.
    pub migrated: Option<file::Migrated>,
    /// The folder to look in for a newer version.
    ///
    /// Unset means the whole feature is inert: no thread, no round trip and
    /// nothing on screen. That matters, because this ships to machines where
    /// no such share exists and to laptops that are off the domain half the
    /// week.
    pub update_from: Option<PathBuf>,

    /// Whether a configuration file was actually read.
    ///
    /// `--no-config`, or a profile with nowhere to put one, means there is
    /// nothing to write back to - so a setting changed in the window applies
    /// for the session and no longer.
    pub have_file: bool,
    /// Settings a command-line flag has fixed for this run.
    ///
    /// The environment can be asked directly whenever the question comes up,
    /// because it cannot change while this runs. A flag leaves nothing to ask,
    /// so it is recorded here as it is applied. One bit per
    /// [`write::SettingKey`]; see [`Self::pin`].
    pub cli_pinned: u16,
    /// The files that are never shown, however well they match.
    ///
    /// One derived value rather than the two settings it is built from, so
    /// that nothing downstream can consult the list while disagreeing about
    /// the flag. `Arc` for the same reason as [`Self::routes`]: it is shared
    /// by every worker and read in the matcher's inner loop.
    pub hidden: Arc<hidden::Hidden>,
}

impl Default for Settings {
    fn default() -> Self {
        Self::with_routes(Arc::new(default_routes()), |s| s)
    }
}

impl Settings {
    /// Builds settings around a routing table.
    ///
    /// It used to also derive a `custpro_path` and a `tree_path` by taking the
    /// first enabled mapping of each kind. Everything downstream was written
    /// against those two paths, which is why a configuration could name ten
    /// shares and have two of them indexed - and why an actor spawned with no
    /// flat mapping configured probed an empty path forever.
    /// Why a setting cannot be written back, if it cannot.
    ///
    /// Generalises the question `viewer_persistable` asked for one key. It
    /// exists because the layering runs file, then environment, then command
    /// line: writing a key one of the upper two holds would report a save,
    /// change the file, and change nothing about the program - now or at the
    /// next start. The window greys the field and says who is holding it
    /// instead, which is what F2 has always done in words.
    pub fn pin(&self, key: write::SettingKey) -> Option<Pin> {
        if self.cli_pinned & key.bit() != 0 {
            return Some(Pin::CommandLine);
        }
        // Asked exactly the way `apply_file_settings` asks it. For most keys
        // an empty value is not a value; for the hide list it is a deliberate
        // "nothing, for this run". The two have to agree, or the window would
        // offer to change something the loader is about to overrule.
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

    /// Whether a change to this setting would still be there tomorrow.
    pub fn can_save(&self, key: write::SettingKey) -> bool {
        self.pin(key).is_none()
    }

    /// Records that a command-line flag has fixed this setting.
    ///
    /// Called by the flag parser as it applies an override, because that is
    /// the only moment anything knows a flag was given.
    pub fn pin_to_session(&mut self, key: write::SettingKey) {
        self.cli_pinned |= key.bit();
    }

    pub fn with_routes(routes: Arc<Routes>, tweak: impl FnOnce(Self) -> Self) -> Self {
        tweak(Self {
            routes,
            aliases: Arc::new(crate::alias::Aliases::default()),
            enum_strategy: EnumStrategy::default(),
            matcher: MatcherKind::default(),
            server_filter: false,
            live_updates: true,
            persist: true,
            stale_notices: true,
            dev_mode: false,
            auto_hide: false,
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
            pdf_viewer: None,
            migrated: None,
            update_from: None,
            // Assume not, and let `load` say otherwise once it knows there
            // is a file. Defaulting the other way would make every test
            // fixture and every `--no-config` session claim it could save.
            have_file: false,
            cli_pinned: 0,
            // Set here and not only in the shipped TOML, because
            // `write_default_if_absent` never rewrites a file that exists:
            // everybody who already has a `config.toml` gets this value and
            // never sees the block the asset file documents it with.
            hidden: Arc::new(hidden::Hidden::new(
                hidden::DEFAULT_HIDE_EXTENSIONS,
                hidden::DEFAULT_HIDE_SYSTEM_FILES,
            )),
        })
    }

    /// Replaces whichever half of [`Self::hidden`] was specified.
    ///
    /// One setter rather than two assignments, because `Hidden` is a single
    /// value derived from two settings that arrive by different routes - the
    /// list from the file, the flag possibly only from the environment. Left
    /// to write into it separately they would each rebuild it from the other's
    /// default and the last one would win.
    fn set_hidden(&mut self, extensions: Option<Vec<String>>, system: Option<bool>) {
        if extensions.is_none() && system.is_none() {
            return;
        }
        // Round-trips through the dotted form `Hidden` stores, which its
        // constructor strips again - so "keep what is already there" needs no
        // second copy of the list hanging off `Settings`.
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
    /// Precedence, lowest first: the shipped defaults, the config file, the
    /// environment, the command line (applied by the caller). A bad file is
    /// returned as errors rather than swallowed - see [`file`] for why
    /// falling back would be worse.
    pub fn load(choice: &ConfigChoice) -> Result<Self, Vec<file::ConfigError>> {
        // Whether a file was actually read decides whether F2 has anywhere to
        // save to, so it is tracked here rather than rediscovered later.
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
                    // built-in defaults are the same bytes anyway.
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
        s.migrated = migrated;
        s.apply_file_settings(&parsed.settings);
        // Everything else about what may be written is asked of the
        // environment when the question comes up; this is the one part of the
        // answer that is not still lying around to be read.
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
        if env_bool("FILES_AUTO_HIDE").is_none()
            && let Some(v) = f.auto_hide
        {
            self.auto_hide = v;
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
        self.set_hidden(
            // `env_str` rather than `var` everywhere else, but not here: it
            // discards an empty value, and an empty `FILES_HIDE_EXTENSIONS` is
            // the deliberate way to say "hide nothing for this run". Asked the
            // usual way, that request would look unset and the file would
            // quietly win.
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
        if let Some(v) = env_usize("FILES_MAX_CONCURRENT_SCANS") {
            s.max_concurrent_scans = v;
        }
        if let Some(v) = env_bool("FILES_STALE_NOTICES") {
            s.stale_notices = v;
        }
        if let Some(v) = env_bool("FILES_DEV_MODE") {
            s.dev_mode = v;
        }
        if let Some(v) = env_bool("FILES_AUTO_HIDE") {
            s.auto_hide = v;
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
        // Unparseable is ignored rather than fatal, which is this loader's
        // rule for the environment throughout - unlike the configuration file,
        // where the same mistake is reported against its line.
        if let Some(v) = env_str("FILES_HOTKEY").and_then(|v| crate::hotkey::spec::parse(&v).ok()) {
            s.hotkey = v;
        }
        if let Some(v) = env_str("FILES_THEME").and_then(|v| ThemeChoice::parse(&v)) {
            s.theme = v;
        }
        if let Some(v) = env_str("FILES_VIEWER").and_then(|v| ViewerKind::parse(&v)) {
            s.viewer = v;
        }
        if let Some(v) = env_str("FILES_PDF_VIEWER") {
            s.pdf_viewer = Some(PathBuf::from(v));
        }
        if let Some(v) = env_str("FILES_UPDATE_FROM") {
            s.update_from = Some(PathBuf::from(v));
        }
        // Commas as well as spaces, because `db,js,lnk` is how anybody would
        // write this one. An empty value means "hide nothing", which is the
        // one-variable way to ask whether the filter is what is hiding a file
        // - the same job `--no-config` does for the file as a whole.
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

/// Reads a duration given in whole seconds. Zero and unparseable values are
/// ignored rather than accepted: a zero probe interval would busy-loop the
/// index thread against the file server.
pub(crate) fn env_secs(key: &str) -> Option<Duration> {
    let secs: u64 = env_str(key)?.parse().ok()?;
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Reads a positive whole number. Zero and unparseable values are ignored
/// rather than accepted, for the same reason [`env_secs`] rejects zero.
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

    // --- hiding files -------------------------------------------------------

    /// Serialises everything below, because `apply_file_settings` reads the
    /// environment and one of these tests writes to it. Modelled on the same
    /// guard in `crate::log`, and needed for the same reason: `cargo test`
    /// runs these in parallel against one process-wide environment.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        // A poisoned lock means another of these tests panicked mid-assertion.
        // That is a failure to report, not a reason to stop taking the lock.
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn applied(f: file::FileSettings) -> Settings {
        let _guard = lock();
        let mut s = Settings::default();
        s.apply_file_settings(&f);
        s
    }

    /// The index log used to be reachable only from a flag or a variable,
    /// which put the one diagnostic for "the drives reload at random" out of
    /// reach of anybody who could not be talked through a command line.
    #[test]
    fn the_index_log_can_be_set_in_the_configuration_file() {
        let s = applied(file::FileSettings {
            index_log: Some(PathBuf::from(r"C:\temp\files.log")),
            ..Default::default()
        });
        assert_eq!(s.index_log, Some(PathBuf::from(r"C:\temp\files.log")));
    }

    /// And obeys the same layering as every other path: the environment sits
    /// over the file, so a variable set for one run is not quietly overruled
    /// by what the file says.
    #[test]
    fn an_index_log_in_the_environment_outranks_the_file() {
        const ENV: &str = "FILES_INDEX_LOG";
        let _guard = lock();

        // SAFETY: as in the test below - the lock is what makes the write
        // safe, and both calls happen before the guard is dropped.
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

    /// An empty `FILES_HIDE_EXTENSIONS` is the deliberate one-variable way to
    /// ask "is the filter what is hiding my file?", so it has to outrank the
    /// configuration file like every other override.
    ///
    /// It did not. The precedence guard used `env_str`, which discards an
    /// empty value, so the request looked unset and the file quietly won -
    /// leaving the one diagnostic for this feature silently doing nothing.
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

        // The two steps `load` performs, in its order: the environment over
        // the built-in defaults, then the file yielding to the environment.
        // Testing only the second would prove nothing - the override is
        // applied in the first.
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
        assert!(!s.hidden.hides("11-D-0704.pdf"));
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

    /// An empty list is a real answer - "hide nothing" - and the quickest way
    /// to find out whether this is why an expected file is missing. It must
    /// not read as "unset" and fall back to the shipped list.
    #[test]
    fn an_empty_list_hides_nothing() {
        let s = applied(file::FileSettings {
            hide_extensions: Some(Vec::new()),
            ..Default::default()
        });
        assert!(s.hidden.is_empty());
        assert!(!s.hidden.hides("Thumbs.db"));
    }

    /// The reason the two settings go through one setter. Each arrives by its
    /// own route, and written separately the second would rebuild `Hidden`
    /// from the first's default and discard it.
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

    #[test]
    fn parses_every_viewer_spelling() {
        assert_eq!(ViewerKind::parse("auto"), Some(ViewerKind::Auto));
        assert_eq!(ViewerKind::parse(" AUTO "), Some(ViewerKind::Auto));
        assert_eq!(ViewerKind::parse("by-extension"), Some(ViewerKind::Auto));
        assert_eq!(ViewerKind::parse("pdf"), Some(ViewerKind::Pdf));
        assert_eq!(ViewerKind::parse("  MERGE "), Some(ViewerKind::Pdf));
        assert_eq!(ViewerKind::parse("avwin"), Some(ViewerKind::Avwin));
        assert_eq!(ViewerKind::parse("AVWIN.EXE"), Some(ViewerKind::Avwin));
        assert_eq!(ViewerKind::parse("notepad"), None);
    }

    /// Assembling the whole document is what someone typing a code almost
    /// always meant; opening one page of it is the special case.
    #[test]
    fn opening_by_file_type_is_the_default() {
        // Only for a fresh install. Every config.toml this program has ever
        // written names a viewer explicitly, so nobody's behaviour changes
        // without them editing the file.
        assert_eq!(Settings::default().viewer, ViewerKind::Auto);
    }

    #[test]
    fn the_viewer_cycles_through_every_mode_and_closes() {
        for start in ViewerKind::ALL {
            let mut seen = vec![start];
            let mut v = start;
            for _ in 1..ViewerKind::ALL.len() {
                v = v.next();
                assert!(!seen.contains(&v), "{v:?} came round twice");
                seen.push(v);
            }
            assert_eq!(v.next(), start, "the cycle does not close");
            assert_ne!(start.next(), start, "F2 must always change something");
        }
    }

    /// Every spelling the writer can emit has to be one the parser accepts,
    /// or F2 would write a value that stops the program at the next start.
    #[test]
    fn every_name_the_writer_emits_parses_back() {
        for v in ViewerKind::ALL {
            assert_eq!(ViewerKind::parse(v.name()), Some(v));
        }
    }

    /// Nothing to save to, so F2 must say "this session only" rather than
    /// report a save that goes nowhere.
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
        // It is enabled only after --bench proves the server's pattern
        // matching returns a superset of a full enumeration.
        assert!(!Settings::default().server_filter);
    }

    /// The shipped configuration names two shares, and both are indexed.
    ///
    /// Replaces a test for `is_flat_root`, which asked whether a path was
    /// *the* flat root - a question with no answer once a configuration can
    /// name several.
    #[test]
    fn the_shipped_configuration_indexes_every_share_it_names() {
        let s = Settings::default();
        let indexed: Vec<_> = s
            .routes
            .enabled()
            .filter(|m| m.kind.is_indexed())
            .map(|m| m.name.as_ref())
            .collect();
        assert_eq!(indexed, vec!["custompro", "jobs"]);
    }

    /// Checked at compile time: a shorter needle would make `memmem` weak
    /// and would make a leading wildcard match an unreasonable share of a
    /// million-entry directory.
    const _: () = assert!(MIN_QUERY_LEN >= 3);
    const _: () = assert!(MAX_SERVER_QUERY_LEN > MIN_QUERY_LEN);

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

    /// `--no-config`, or a profile with nowhere to put a file. The setting
    /// still applies; it just will not be there tomorrow.
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

    /// The variable is named, because the user has to find and unset it.
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

    /// One bit each, or two settings would pin each other.
    #[test]
    fn every_writable_key_has_a_bit_of_its_own() {
        let mut seen = 0u16;
        for key in SettingKey::ALL {
            assert_eq!(seen & key.bit(), 0, "{} shares a bit", key.name());
            seen |= key.bit();
        }
        assert_eq!(seen.count_ones() as usize, SettingKey::ALL.len());
    }

    /// Each key names the variable that actually overrules it. A wrong name
    /// here would send somebody hunting for a variable that is not the one.
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
