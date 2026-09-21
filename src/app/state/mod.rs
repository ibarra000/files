//! The interaction model, as a pure state transition.
//!
//! [`AppState::update`] takes an event and the current time and returns what
//! to redraw and what to do. It performs no I/O, reads no clock, sends on no
//! channel and spawns no thread. Everything time-dependent is driven by
//! deadlines the caller reads back out of [`AppState::next_deadline`].
//!
//! That is deliberate, and it is the single decision that makes this program
//! testable at all: the network drives do not exist on the machine this was
//! written on, but the entire debounce, staleness, selection and error model
//! can be driven from unit tests with a fake clock.
//!
//! # Timing
//!
//! There is no tick thread. The main loop asks for the next deadline and
//! blocks until then, synthesising [`AppEvent::Tick`] on expiry. Idle with
//! nothing stale on screen means a blocking receive with no deadline at all -
//! zero wakeups, zero CPU, and no added latency, since a keystroke wakes the
//! thread immediately.
//!
//! The status line's age readout is driven the same way. Its next deadline is
//! the exact moment the rendered string would change, computed from the wall
//! clock the renderer hands back on [`AppState::note_frame`] - so the state
//! still reads no clock itself, and an hour-old index costs one wakeup an
//! hour rather than thirty-six hundred.

mod keys;
mod model;
mod overlay;
pub mod pointer;
mod settings;

pub use model::{EmptyReason, LiveProgress, QueryPhase, Severity, TOAST_LIFETIME, Toast, Urgency};
pub use settings::SettingChange;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use super::event::{AppEvent, ClipboardMsg, Cmd, IndexMsg, OpenMsg, Redraw, Response};
use super::input::{self, Input};
use crate::config::COUNTDOWN_TICK;
use crate::config::{
    ENTER_WATCHDOG, LIVE_DEBOUNCE, MIN_SERVER_QUERY_LEN, MIN_TERM_LEN, REMEMBER_DEBOUNCE,
    SEARCH_DEBOUNCE, Settings, VERIFY_DEBOUNCE, VERIFY_WATCHDOG, ViewerKind,
};
use crate::history::History;
use crate::index::store::{IndexOverview, IndexStatus};
use crate::paths::MappingId;
use crate::search::matcher::{Hit, QueryReject};
use crate::search::query::Query;
use crate::search::verify::{AuditVerdict, SkipReason, VerifyOutcome};

/// The right-click menu.
///
/// The one thing in this program that floats over another. It earns that: it
/// is transient, it is dismissed by any means at all, and a context menu on a
/// right-click is the single most universal mouse idiom there is. Everything
/// else that could have been a pop-up - recall, help - borrows the results
/// pane instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    /// The cell the pointer was over.
    pub anchor: (u16, u16),
    /// The result it was opened on.
    pub rank: usize,
    pub cursor: usize,
}

/// Everything rendered, and everything that decides what to do next.
pub struct AppState {
    pub settings: Settings,
    pub input: Input,
    /// Codes used before, and where recall currently is within them.
    pub history: History,
    /// Whether the panel is up.
    ///
    /// Public because the shell reads it to start and finish the entrance, and
    /// nothing here is derived from anything else. Set only from a
    /// [`crate::app::event::HotkeyMsg`]: the thread that owns the window is the
    /// authority on whether the panel is on screen, and a second opinion here
    /// is how a panel comes to be drawn over a window that has already hidden
    /// it.
    pub overlay_up: bool,
    /// Whether the drive picker is open.
    ///
    /// All that is left of a five-way `Focus`. The text field always has the
    /// keyboard; this is the one thing that borrows the *body*, and it borrows
    /// it for one keystroke at a time.
    pub picking_share: bool,
    /// Where the keyboard is in the alias list, if it is in it at all.
    ///
    /// `None` means the list is on screen and nobody has stepped onto it -
    /// which is the ordinary case, because an alias is two or three
    /// characters and typing it is faster than walking to it. See
    /// [`Self::showing_aliases`].
    alias_cursor: Option<usize>,
    /// Whether the actions menu is up.
    ///
    /// Here rather than in the renderer because Escape has to close it, and
    /// which key means what is the state machine's business. It is a *flag*
    /// and not a cursor: the menu is keyboard-reachable through the shortcut
    /// each row names, so there is nothing to walk. See
    /// [`crate::view::actions`].
    pub actions_open: bool,
    pub hits: Vec<Hit>,
    pub matched: u32,
    pub total: u32,
    pub phase: QueryPhase,
    /// What the live shares are doing about this query, when any are
    /// configured. See [`LiveProgress`].
    pub live: Option<LiveProgress>,
    pub empty_reason: Option<EmptyReason>,
    /// One status per configured mapping, indexed by `MappingId`.
    ///
    /// Private, so [`Self::index`] cannot go stale: `on_index` is the only
    /// writer and it always recomputes. This was a single `Arc<IndexStatus>`
    /// with one actor per share writing into it, so the status line described
    /// whichever share happened to publish last.
    statuses: Vec<Arc<IndexStatus>>,
    /// Which row the share list is on. Only meaningful under `Focus::Shares`.
    shares_cursor: usize,
    /// The one-line truth about all of them, recomputed on every status.
    ///
    /// Cached rather than folded on demand because `next_deadline` reads it on
    /// every loop iteration and must agree with the frame that was drawn about
    /// which share is oldest and which is worst. Computing it at the single
    /// mutation point makes that agreement structural.
    pub index: IndexOverview,
    pub toast: Option<Toast>,
    pub should_quit: bool,
    /// Set once the user moves the selection, and cleared when they type.
    /// While set, incoming results never yank the cursor back to the top.
    pub selection_pinned: bool,
    /// Tracked by path, not by row, so a result update cannot silently change
    /// what `Enter` would open.
    pub selected_path: Option<Arc<str>>,
    /// Reported so the status line can say the results shifted underneath a
    /// pinned selection.
    pub selection_lost: bool,
    /// Whether `avwin.exe` could not be found on PATH when the program
    /// started.
    ///
    /// Carried here rather than left on [`crate::app::App`] because the only
    /// thing anybody can usefully do with it is say so, and saying things is
    /// what `view::status` is for. It was probed and discarded for the whole
    /// of the rewrite, so choosing the viewer that is not installed failed
    /// silently at the moment a drawing was wanted.
    pub avwin_missing: bool,
    /// Which viewer Enter uses, right now.
    ///
    /// Separate from `settings.viewer`, which is the value resolved at
    /// startup and stays immutable. `Settings` is cloned into the backend and
    /// every worker, so making it mutable would be one truth with several
    /// stale copies of it; this is the only copy that moves, and the choice
    /// travels to the worker on the command rather than being read from
    /// anywhere shared.
    pub viewer: ViewerKind,
    /// What the last look at the update folder found.
    ///
    /// `None` until the checker has answered once, which is a different thing
    /// from having looked and found nothing - the settings window says so
    /// rather than claiming to be up to date before it knows.
    pub update: Option<crate::update::Found>,
    /// Whether this process holds the global chord, as the hotkey thread
    /// reported it at startup.
    ///
    /// `None` until it has said, which for a hotkey that is switched off or
    /// a platform that has none is for ever. Recorded rather than asked
    /// again later because `RegisterHotKey` is per-thread and cannot be
    /// asked again correctly from in here - see [`crate::hotkey::Probe`].
    pub hotkey_claim: Option<Result<(), String>>,

    query_epoch: u64,
    /// When the code on the line becomes worth matching against the index.
    ///
    /// A third clock, separate for the same reason the other two are: this one
    /// is paced by what a *reader* can use. The match is free; drawing its
    /// answer is not, because the panel is the window.
    search_due_at: Option<Instant>,
    /// Enter was pressed while the match for the code on the line was still
    /// pending.
    ///
    /// The rows on screen answer the *previous* code, so opening one would open
    /// the wrong file - silently, which is the one failure this program must
    /// not have. Enter therefore asks the matcher at once and takes its row
    /// from the answer. Cleared by any edit, by the answer, and by the watchdog.
    enter_pending: bool,
    /// And the backstop, so a wedged matcher cannot leave Enter dead.
    enter_watchdog_at: Option<Instant>,
    verify_due_at: Option<Instant>,
    /// When the live shares may be asked.
    ///
    /// A clock of its own rather than a share of `verify_due_at`, because the
    /// two are paced by different things: a verification is a courtesy check
    /// nobody is waiting on, while a live answer *is* the results, and the
    /// live one has to be slower because it costs somebody else's file server
    /// real work on every settled keystroke.
    live_due_at: Option<Instant>,
    /// When the code on the line becomes worth remembering.
    ///
    /// A clock of its own rather than a share of `verify_due_at`, because the
    /// two answer different questions. That one paces a round trip against
    /// somebody else's file server and is measured in the third of a second a
    /// typist pauses between syllables; this one decides what a colleague sees
    /// in their recall list tomorrow, and a third of a second is nowhere near
    /// long enough to tell a finished code from a half-typed one.
    remember_due_at: Option<Instant>,
    verify_watchdog_at: Option<Instant>,
    toast_expires_at: Option<Instant>,
    last_frame: Instant,
    /// Wall clock at the last frame, for working out when the age readout
    /// next changes.
    ///
    /// The state still reads no clock: this arrives as a parameter from the
    /// renderer, exactly as `Instant` already does on every `update`.
    last_frame_wall: SystemTime,
    /// The line, taken apart.
    ///
    /// Cached at the one place the text can change rather than re-parsed at
    /// each of the six sites that dispatch a search, for the reason
    /// `IndexOverview` is cached beside the statuses it summarises: a second
    /// derivation is a second opinion waiting to disagree. It also puts the
    /// term - as opposed to the line - in reach of the renderer, which needs
    /// it to underline the right characters, and of `OpenRequest`, which needs
    /// it to gather a drawing set.
    query: Query,
    /// The alias the line turned out to be, if it was one.
    ///
    /// Kept beside the parsed query rather than derived on demand because it
    /// answers a question the query cannot: `query` holds what is being
    /// searched for, and this holds *why* - which is the difference between a
    /// panel that shows its working and one that quietly searches for
    /// something nobody typed.
    expansion: Option<crate::alias::Alias>,
    last_verified_query: Option<Query>,
    /// Which result the pointer is over, if any.
    ///
    /// Never the same thing as `selected_path`: `Enter` opens the selection,
    /// and a highlight that could be mistaken for it would be a highlight that
    /// gets a file opened by accident. Cleared on any keystroke, because a
    /// terminal reports no "the pointer left the window" event and a hover
    /// that outlives the pointer is a lie.
    hovered: Option<usize>,
    /// How far the help panel is scrolled.
    ///
    /// Clamped against the pane on the way out rather than on the way in, so a
    /// terminal that grows cannot leave the panel parked below its own end.
    help_scroll: u16,
}

/// Whether a move that runs off the end comes back on at the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wrap {
    /// A step: Up from the first row is the last, and Down from the last is
    /// the first. What Ueli's arrows do.
    Around,
    /// A page: the ends hold. See [`AppState::move_selection`].
    Stop,
}

impl AppState {
    pub fn new(settings: Settings, now: Instant) -> Self {
        let viewer = settings.viewer;
        // Sized over `all()`, not `enabled()`. `MappingId` is a position in
        // the parsed list *including* disabled entries, so sizing over the
        // enabled subset would misindex the moment a disabled mapping precedes
        // an enabled one - and attribute one share's health to another, which
        // is the bug this keying exists to fix.
        let statuses: Vec<Arc<IndexStatus>> = (0..settings.routes.all().len())
            .map(|_| Arc::new(IndexStatus::default()))
            .collect();
        let mut state = Self {
            settings,
            input: Input::new(),
            history: History::new(),
            overlay_up: false,
            picking_share: false,
            alias_cursor: None,
            actions_open: false,
            // Replaced by the real size before the first frame; a sane default
            // means mouse arithmetic is never done against a zero rect.
            hits: Vec::new(),
            matched: 0,
            total: 0,
            phase: QueryPhase::Idle,
            live: None,
            empty_reason: Some(EmptyReason::NoQuery),
            statuses,
            shares_cursor: 0,
            index: IndexOverview::default(),
            toast: None,
            should_quit: false,
            selection_pinned: false,
            selected_path: None,
            selection_lost: false,
            avwin_missing: false,
            viewer,
            query_epoch: 0,
            search_due_at: None,
            enter_pending: false,
            enter_watchdog_at: None,
            verify_due_at: None,
            live_due_at: None,
            remember_due_at: None,
            verify_watchdog_at: None,
            toast_expires_at: None,
            last_frame: now,
            // The epoch until the first frame reports a real one. Nothing
            // reads it before then: `next_text_change` needs a published
            // index, which needs an actor to have answered.
            last_frame_wall: SystemTime::UNIX_EPOCH,
            query: Query::default(),
            expansion: None,
            update: None,
            hotkey_claim: None,
            last_verified_query: None,
            help_scroll: 0,
            hovered: None,
        };

        // Said on the first frame, because it has already happened by the time
        // anything can be drawn. A program that rewrote a file the user
        // maintains and never mentioned it would have, as its first symptom, a
        // comment of theirs having quietly vanished.
        if let Some(migrated) = state.settings.migrated.clone() {
            state.set_toast(migrated.detail(), Severity::Info, now);
        }
        state
    }

    /// The line, taken apart.
    ///
    /// What the renderer underlines and what a page set is gathered by are
    /// both the *term*, not the line, so both read it from here.
    pub fn query(&self) -> &Query {
        &self.query
    }

    /// The alias the line turned out to be, if it was one.
    ///
    /// Read by the renderer, and it has to be: an alias that fired without
    /// saying so is the program searching for something other than what is on
    /// the line. See [`crate::alias`].
    pub fn expansion(&self) -> Option<&crate::alias::Alias> {
        self.expansion.as_ref()
    }

    pub fn query_epoch(&self) -> u64 {
        self.query_epoch
    }

    /// Installs the codes loaded from disk.
    ///
    /// Separate from [`AppState::new`] rather than another constructor
    /// argument: every test builds a state, and almost none of them care
    /// about history.
    pub fn seed_history(&mut self, entries: Vec<String>) {
        self.history = History::from_entries(entries);
    }

    /// Records the startup probe for `avwin.exe`.
    ///
    /// Separate from [`AppState::new`] for the same reason as the history: the
    /// state machine does no I/O, and searching PATH is I/O.
    pub fn set_avwin_missing(&mut self, missing: bool) {
        self.avwin_missing = missing;
    }

    /// When the authoritative server-side check is due, if one is pending.
    ///
    /// Exposed so the debounce can be asserted directly rather than by
    /// poking at private state.
    /// When the local match is due, if one is pending. For the tests.
    pub fn search_due_at(&self) -> Option<Instant> {
        self.search_due_at
    }

    pub fn verify_due_at(&self) -> Option<Instant> {
        self.verify_due_at
    }

    /// How far the help panel is scrolled, for the renderer.
    pub fn help_scroll(&self) -> u16 {
        self.help_scroll
    }

    /// Which result the pointer is over, for the renderer.
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Row index of the current selection, for the list widget.
    pub fn selected_row(&self) -> Option<usize> {
        let target = self.selected_path.as_ref()?;
        self.hits.iter().position(|h| &h.path == target)
    }

    pub fn selected_hit(&self) -> Option<&Hit> {
        self.selected_row().map(|i| &self.hits[i])
    }

    /// True while something this program started has not finished.
    ///
    /// Derived from state rather than stored: a sticky flag is exactly how a
    /// UI ends up reporting work that ended long ago.
    ///
    /// It used to drive the spinner, and with the spinner gone it no longer
    /// asks for frames: nothing on the panel is a function of *elapsed time*
    /// while busy. The words come from `view::status` and change when a worker
    /// reports, which wakes the loop by itself; the mark beside them is a
    /// static glyph. What is left is a question the status line asks.
    pub fn is_busy(&self) -> bool {
        if self.live.as_ref().is_some_and(|l| l.is_asking()) {
            return true;
        }
        matches!(self.phase, QueryPhase::LocalPending)
            || self.phase.is_verifying()
            || self.index.is_busy()
    }

    pub fn note_frame(&mut self, now: Instant, wall: SystemTime) {
        self.last_frame = now;
        self.last_frame_wall = wall;
    }

    /// How to describe a drive failure to whoever is looking at it.
    ///
    /// Two differences, and both are the same decision. In dev mode the drive
    /// is named by its *path*, because `path_label` exists on the argument
    /// that "jobs" says nothing about which drive letter to go and reconnect -
    /// and the code is appended, because that is what a support call asks for.
    /// Otherwise it is named by the name its owner gave it and the sentence
    /// stops there.
    ///
    /// One function rather than the same two lines at five call sites: they
    /// were five copies of one policy, and a policy in five places is one that
    /// is about to be four.
    pub(crate) fn describe_drive_error(
        &self,
        id: MappingId,
        err: crate::index::errors::EnumError,
    ) -> String {
        let dev = self.settings.dev_mode;
        let target = if dev {
            self.settings.routes.path_label(id)
        } else {
            self.settings.routes.label(id).to_string()
        };
        err.describe_for(&target, dev)
    }

    /// The technical half of a message, when there is somebody to read it.
    ///
    /// `None` outside dev mode, so a caller writes `plain` and lets this
    /// decide whether anything is joined onto it.
    pub(crate) fn technical(&self, detail: impl Into<String>) -> Option<String> {
        self.settings.dev_mode.then(|| detail.into())
    }

    /// Whether the status line would say something without being asked.
    ///
    /// Kept beside the state it reads rather than in `view::status`, which
    /// draws the same facts: the panel has to know whether to leave room for
    /// the line *before* it asks what the line says. `index_warning` is the
    /// other half and calls this one, so there is no second opinion to drift
    /// - only the words are over there.
    ///
    /// Takes the clock rather than reading `last_frame_wall`, because the
    /// renderer has a fresher one and staleness is the one notice that
    /// arrives with no event behind it.
    /// Whether an empty box is showing the configured shortcuts.
    ///
    /// Ueli's favourites. Its empty screen lists them and that is the whole
    /// point of having them; ours lists aliases, which are the same kind of
    /// thing - configured, named, few, and a record of nothing.
    ///
    /// This is the one place the panel puts anything on an untouched screen,
    /// and it is worth saying why that is not a reversal of b719a20, which
    /// stripped the first screen to a search box and nothing else. What that
    /// removed was five lines of *instructions* - what to type, what a code
    /// looks like - in front of somebody who learned both on their first
    /// day. A list somebody wrote themselves is not instructions, and a
    /// machine with no aliases configured still gets a search box and
    /// nothing else.
    ///
    /// The recent codes are deliberately *not* here, and that is the line:
    /// they are a record of what this person looked up, and a panel summoned
    /// over somebody's shoulder must not put that on screen unasked. Ueli
    /// agrees - its search history is an opt-in dropdown rather than part of
    /// the list - so the privacy decision and its shape do not have to
    /// disagree. Up is still the only way in.
    pub fn showing_aliases(&self) -> bool {
        self.input.text().is_empty()
            && !self.picking_share
            && !self.showing_recent()
            && !self.settings.aliases.is_empty()
    }

    /// Which shortcut the keyboard is on, if it has been stepped onto.
    pub fn alias_cursor(&self) -> Option<usize> {
        self.showing_aliases()
            .then_some(self.alias_cursor)
            .flatten()
    }

    pub(crate) fn has_standing_notice(&self, wall: SystemTime) -> bool {
        self.index.origin.is_none()
            || self.index.degraded().is_some()
            || (self.settings.stale_notices && self.index.stale_at(wall).is_some())
    }

    /// True when something on screen goes stale on its own.
    ///
    /// The counterpart to [`Self::wants_animation`]: that one is about work in
    /// flight, this one about text that is wrong a second from now with no
    /// event to say so - the per-share ages in the drive picker, and the
    /// countdown to a retry.
    ///
    /// The index age used to be on the status line in every healthy phase, so
    /// this was true whenever there was an index at all and the panel ticked
    /// for as long as it was open. The ages live in the drive picker now, so
    /// the wakeups do too: an idle panel with a healthy index costs nothing.
    pub fn shows_elapsed_text(&self) -> bool {
        (self.picking_share && self.index.built_at.is_some()) || self.index.unreachable().is_some()
    }

    /// When the time-derived text next changes, if any is shown.
    ///
    /// Both terms are anchored on `last_frame`, so drawing always pushes the
    /// deadline forward - which is what stops the tick arm in `on_tick` from
    /// spinning.
    fn next_text_change(&self) -> Option<Instant> {
        let mut out: Option<Instant> = None;
        let mut earliest = |at: Instant| {
            out = Some(out.map_or(at, |prev: Instant| prev.min(at)));
        };

        // The countdown is pure monotonic arithmetic and needs no wall clock.
        //
        // Contributed only while the retry is still ahead. `next_retry_at` is
        // an absolute instant, unlike every other term here: once it passes,
        // an ungated term is permanently due, and the loop would wake, draw,
        // and find the deadline still in the past - a redraw at full speed
        // rather than a countdown.
        if let Some((_, crate::index::Health::Unreachable { next_retry_at, .. })) =
            self.index.unreachable()
            && *next_retry_at > self.last_frame
        {
            earliest(*next_retry_at);
            earliest(self.last_frame + COUNTDOWN_TICK);
        }

        // The per-share ages in the drive picker, whose next change is exactly
        // one bucket edge away. Only while the picker is up: nothing else on
        // screen is derived from the wall clock any more.
        if self.picking_share
            && let Some(age) = self.index.age(self.last_frame_wall)
        {
            earliest(self.last_frame + crate::util::humanize::next_age_change(age));
        }
        out
    }

    /// Whether a right-click menu is open, for the renderer and the tests.
    pub fn menu_is_open(&self) -> bool {
        false
    }

    /// The earliest pending deadline, or `None` when the loop can block
    /// indefinitely.
    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.search_due_at,
            self.enter_watchdog_at,
            self.verify_due_at,
            self.live_due_at,
            self.remember_due_at,
            self.verify_watchdog_at,
            self.toast_expires_at,
            // Without this the loop parks in an unbounded receive whenever
            // nothing else is pending, so the age on screen froze until a
            // keystroke happened to arrive and then jumped.
            self.next_text_change(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The one state transition.
    pub fn update(&mut self, event: AppEvent, now: Instant) -> Response {
        // There is no longer a focus to watch for changes in. The text field
        // always has the keyboard, so nothing on screen moves because of where
        // the keyboard *is* - only because of what it did. This used to be
        // fourteen assignments guarded by one comparison; it is now nothing at
        // all, which is the clearest measure of what collapsing `Focus` bought.
        self.dispatch_event(event, now)
    }

    fn dispatch_event(&mut self, event: AppEvent, now: Instant) -> Response {
        match event {
            AppEvent::Key(key) => self.on_key(key, now),
            AppEvent::Intent(intent) => self.on_intent(intent, now),
            AppEvent::Setting(change) => self.on_setting(change, now),
            AppEvent::Update(msg) => self.on_update(msg, now),
            AppEvent::Aliases(list) => self.on_aliases(list, now),
            AppEvent::Drives(list) => self.on_drives(list),
            AppEvent::Paste(text) => self.on_paste(&text, now),
            AppEvent::WindowFocus(has_focus) => self.on_focus(has_focus),
            AppEvent::Tick => self.on_tick(now),
            AppEvent::Search(msg) => self.on_search(msg, now),
            AppEvent::Verify(msg) => self.on_verify(msg, now),
            AppEvent::Live(msg) => self.on_live(msg, now),
            AppEvent::Index(msg) => self.on_index(msg, now),
            AppEvent::Open(msg) => self.on_open(msg, now),
            AppEvent::Clipboard(msg) => self.on_clipboard(msg, now),
            AppEvent::Hotkey(msg) => self.on_hotkey(msg, now),
            AppEvent::ActorDied { actor, detail } => {
                self.clear_verifying();
                // The actor's own name is an internal one and means nothing to
                // the person reading it, so it goes with the detail rather
                // than into the sentence.
                self.set_toast_detailed(
                    "Something stopped working \u{b7} searching may be incomplete",
                    format!("{actor}: {detail}"),
                    Severity::Error,
                    now,
                );
                Response::redraw()
            }
            AppEvent::Shutdown => {
                self.should_quit = true;
                Response::none().with(Cmd::Quit)
            }
        }
    }

    // --- input ------------------------------------------------------------
    //
    // Key and mouse handling live in `keys.rs` and `mouse.rs`. They are child
    // modules rather than siblings so they can still reach the private fields
    // of the state they drive, which keeps those fields private to everyone
    // else.

    fn on_paste(&mut self, text: &str, now: Instant) -> Response {
        // Text from outside the program is the one place a control character
        // can reach the search line, and a stray newline in the query would
        // be invisible on screen and fatal to the match.
        let cleaned = input::sanitize(text);
        if cleaned.is_empty() {
            return Response::none();
        }
        self.leave_history();
        self.input.insert_str(&cleaned);
        self.on_input_changed(now, Urgency::Complete)
    }

    fn on_clipboard(&mut self, msg: ClipboardMsg, now: Instant) -> Response {
        match msg {
            ClipboardMsg::Copied { chars } => {
                // Said out loud: a copy that silently succeeded is
                // indistinguishable from a copy that silently failed.
                let unit = if chars == 1 {
                    "character"
                } else {
                    "characters"
                };
                self.set_toast(format!("Copied {chars} {unit}"), Severity::Info, now);
                Response::redraw()
            }
            ClipboardMsg::Read { text } => {
                let cleaned = input::sanitize(&text);
                if cleaned.is_empty() {
                    self.set_toast("The clipboard holds no text".into(), Severity::Info, now);
                    return Response::redraw();
                }
                self.leave_history();
                self.input.insert_str(&cleaned);
                self.on_input_changed(now, Urgency::Complete)
            }
            ClipboardMsg::Failed { detail } => {
                self.set_toast_detailed(
                    "The clipboard could not be read",
                    detail,
                    Severity::Warn,
                    now,
                );
                Response::redraw()
            }
        }
    }

    /// Stops browsing, keeping whatever code was recalled.
    fn leave_history(&mut self) {
        if self.history.is_browsing() {
            self.history.accept();
        }
    }

    /// Remembers a code once it has actually been searched for.
    ///
    /// Returns the write to perform, if the list changed. Nothing here touches
    /// the disk: the state transition stays pure and the caller does the I/O.
    fn remember(&mut self, code: &str) -> Option<Cmd> {
        if !self.settings.history || !self.history.record(code) {
            return None;
        }
        Some(Cmd::SaveHistory(Arc::new(self.history.snapshot())))
    }

    /// Empties the result list and everything derived from it.
    ///
    /// Only ever called from a branch that has *decided* there is nothing to
    /// search for. A query merely in flight deliberately does not come here -
    /// see the note in [`Self::on_input_changed`].
    fn clear_results(&mut self) {
        self.hits.clear();
        self.hovered = None;
        self.matched = 0;
        self.total = 0;
        self.selected_path = None;
    }

    fn on_input_changed(&mut self, now: Instant, urgency: Urgency) -> Response {
        // Editing the code accepts whatever was being recalled: the text in
        // the field is now something the user typed rather than something they
        // were looking at. This is the one place it happens, so no key handler
        // has to remember to do it.
        self.leave_history();
        // And steps off the shortcut list for the same reason. Kept here
        // rather than in the key handlers because there are half a dozen
        // ways to change the line and one of them would forget.
        self.alias_cursor = None;
        self.query_epoch += 1;
        // Editing the code un-pins: the user is choosing a different code, not
        // holding a place in the list for this one. The selected *path* is kept
        // - see below - so a result set that still contains it keeps it.
        self.selection_pinned = false;
        self.selection_lost = false;
        self.verify_watchdog_at = None;
        self.last_verified_query = None;

        // Resolved before anything is judged, because an alias decides what
        // the rest of this function is even looking at. The line the user
        // typed is kept exactly as they typed it - rewriting the field under
        // somebody mid-edit would fight their caret and their selection - so
        // what an alias replaces is the *query*, not the text.
        let expansion = self.settings.aliases.resolve(self.input.text()).cloned();
        self.query = Query::parse(match &expansion {
            Some(alias) => alias.code.as_ref(),
            None => self.input.text(),
        });
        // An alias is not somebody typing. The line was finished the moment it
        // resolved, so it runs now rather than waiting out a pause meant for a
        // person still reading a code off a drawing.
        let urgency = match expansion {
            Some(_) => Urgency::Complete,
            None => urgency,
        };
        self.expansion = expansion;

        if self.input.text().trim().is_empty() {
            self.phase = QueryPhase::Idle;
            self.empty_reason = Some(EmptyReason::NoQuery);
            self.stand_down();
            self.clear_results();
            return Response::redraw();
        }
        // Judged on the *term*, not the line: `ext:pdf` is a seven-character
        // line with nothing on it to search for.
        if let Err(reject) = self.query.check() {
            self.phase = match reject {
                QueryReject::Empty => QueryPhase::Idle,
                _ => QueryPhase::BadQuery {
                    detail: reject.detail(),
                },
            };
            self.empty_reason = Some(match reject {
                QueryReject::Empty => EmptyReason::NoQuery,
                _ => EmptyReason::BadQuery {
                    detail: reject.detail(),
                },
            });
            self.stand_down();
            self.clear_results();
            return Response::redraw();
        }

        if !self.settings.routes.any_searchable() {
            self.phase = QueryPhase::NoShares;
            self.empty_reason = Some(EmptyReason::NoSharesConfigured);
            self.stand_down();
            self.clear_results();
            return Response::redraw();
        }

        // The results, the selection and the counts are deliberately left
        // alone here. They are replaced wholesale by `apply_hits` when the
        // answer lands, and `on_search` is epoch-guarded, so the worst a stale
        // set can do is survive one frame.
        //
        // Clearing them instead is what made the panel flinch on every
        // keystroke: `Cmd::Search` is dispatched at the end of the turn, so the
        // matcher cannot answer before the frame is drawn - and a drawn frame
        // with no hits is a different *body*, three hundred points shorter,
        // reached through a cross-fade, with the footer chips re-flowing
        // around it. All of that, per character, for a list that was about to
        // come straight back.
        self.phase = QueryPhase::LocalPending;
        self.empty_reason = Some(EmptyReason::NotSearchedYet);

        // A code typed a character at a time waits out the pause; one that
        // arrived whole does not. Nobody pastes half a job number, and making
        // a paste sit for 300ms is 300ms of a panel that looks broken.
        let mut response = Response::redraw();
        self.enter_pending = false;
        self.enter_watchdog_at = None;
        match urgency {
            Urgency::Complete => {
                self.search_due_at = None;
                response = response.with(Cmd::Search {
                    query: self.query.clone(),
                    epoch: self.query_epoch,
                });
            }
            Urgency::Typed => self.search_due_at = Some(now + SEARCH_DEBOUNCE),
        }

        self.verify_due_at = Some(now + VERIFY_DEBOUNCE);
        // Deliberately armed even for `Urgency::Complete`. A paste skips the
        // local debounce because the match is free; this one is not free, and
        // 600 ms of showing the local results first costs nobody anything they
        // can perceive.
        if !self.settings.routes.live().next().is_none() {
            self.live_due_at = Some(now + LIVE_DEBOUNCE);
        }
        self.remember_due_at = Some(now + REMEMBER_DEBOUNCE);

        response.redraw = Redraw::Yes;
        response
    }

    /// Retires every clock a live query owns.
    ///
    /// One place, because they have to go together. A `Cmd::Search` left armed
    /// on a field that has since been cleared is *not* caught by the epoch
    /// guard in `on_search`: `on_input_changed` bumped the epoch on its way
    /// past, so the late dispatch carries the current one and its answer is
    /// accepted. The symptom is the previous code's results reappearing under
    /// an empty search box.
    fn stand_down(&mut self) {
        self.search_due_at = None;
        self.verify_due_at = None;
        // Retired here too. A `Cmd::Live` armed on a field that has since been
        // cleared is *not* caught by the epoch guard - the epoch was already
        // bumped on the way past - so the previous code's results would arrive
        // under an empty search box.
        self.live_due_at = None;
        self.live = None;
        self.remember_due_at = None;
        self.enter_pending = false;
        self.enter_watchdog_at = None;
    }

    fn on_enter(&mut self, now: Instant) -> Response {
        // Enter takes the row you are on. On a remembered code or a shortcut
        // that means filling the field and searching; on a file it means
        // opening it. One rule, three kinds of row - and the three look
        // different enough that nobody has to be told which is which.
        if self.history.is_browsing() {
            return self.accept_recall(now);
        }
        if let Some(rank) = self.alias_cursor() {
            return self.accept_alias(rank, now);
        }

        // Unless the match for what is on the line has not run yet, in which
        // case the row you are on answers the code *before* this one. Opening
        // it would open the wrong file without saying so, which is the one
        // failure this program must not have. So ask the matcher now and open
        // whatever the answer brings back - see `on_search`.
        if self.search_due_at.take().is_some() {
            self.enter_pending = true;
            self.enter_watchdog_at = Some(now + ENTER_WATCHDOG);
            return Response::redraw().with(Cmd::Search {
                query: self.query.clone(),
                epoch: self.query_epoch,
            });
        }

        self.open_selection(self.viewer, now)
    }

    /// Opens the row the selection is on. The second half of [`Self::on_enter`],
    /// split out because a deferred Enter re-enters it from `on_search`.
    /// Opens the row the selection is on with a viewer other than the
    /// current one, without changing which one is current.
    ///
    /// Public to the module so `keys` and `pointer` can both reach it: the
    /// three viewer-specific actions are a keystroke *and* a menu row, and
    /// the two have to be the same act.
    pub(super) fn open_with(&mut self, viewer: ViewerKind, now: Instant) -> Response {
        self.open_selection(viewer, now)
    }

    /// Opens the row the selection is on, with the viewer named.
    ///
    /// The viewer is a parameter rather than `self.viewer` because three of
    /// the actions in [`crate::view::actions`] are the same open with a
    /// different one - and `OpenRequest` has carried the viewer per request
    /// since it was written, precisely so this could be a parameter rather
    /// than a mode change followed by an open followed by a mode change back.
    fn open_selection(&mut self, viewer: ViewerKind, now: Instant) -> Response {
        // Never blocks on the network: if the file turns out to be gone, that
        // is reported afterwards.
        let Some(hit) = self.selected_hit().or_else(|| self.hits.first()) else {
            if !self.input.is_empty() {
                // A code that found nothing is exactly the one worth recalling
                // and correcting, and pressing Enter on it says so more
                // emphatically than letting the quiet period say it. Gated,
                // unlike the branch below: with a hit the result is the
                // evidence the code was meant, and with none the only evidence
                // available is that the typing stopped. Enter on `inv` on the
                // way to `invoice` is a keystroke, not a decision.
                let mut response = Response::redraw();
                if let Some(cmd) = self.remember_if_settled() {
                    response = response.with(cmd);
                }
                self.set_toast("Nothing to open".into(), Severity::Info, now);
                return response;
            }
            return Response::none();
        };
        let path = Arc::clone(&hit.path);

        // Opening a file is the strongest possible signal that this code was
        // the one meant, so it is remembered here rather than waiting out the
        // quiet period - someone who opens a result the moment it appears must
        // not lose the code.
        // The term, not the line. `pages::collect` matches by equality -
        // `code + marker? + ".pdf"` - so handed `11-D-0704 ext:pdf` it would
        // find no pages at all, and a thirteen-page drawing set would open as
        // a single page with nothing on screen to say so.
        let code = self.query.term().to_string();
        // Decided before the path is moved into the request, and kept, because
        // the answer is wanted again below.
        let route = crate::open::route_of(viewer, &path);
        let request = crate::open::OpenRequest {
            path,
            // The typed code, not the selected row: the page set is rebuilt
            // from it because `hits` is capped at MAX_RESULTS and ranked by
            // match position, so a long document would arrive truncated and
            // out of order.
            query: code.clone(),
            viewer,
        };
        let mut response = Response::none().with(Cmd::Open(request));

        // Assembling a document means reading every page off the share, and
        // converting a drawing means waiting for another program; neither is
        // instant. Enter used to return silently because handing one path to
        // one program was; saying nothing for a second or more now would read
        // as the keypress having been ignored.
        //
        // Asked of the *route* rather than the mode: assembling a document is
        // the only one of the three that goes to the share at all. Handing one
        // file to avwin or to the shell is a single call that returns at once,
        // and a notice for it would be on screen and gone inside a frame.
        if route == crate::open::Route::Document {
            self.set_toast("Opening\u{2026}".into(), Severity::Info, now);
            response.redraw = Redraw::Yes;
        }

        // Ungated, and the only commit point that is: a file opening is the
        // code being used, which outranks any question about whether the
        // typing has stopped. Clears the pending deadline on the way through,
        // so the tick it was armed for has nothing left to do.
        if let Some(cmd) = self.remember_now() {
            response = response.with(cmd);
        }

        // On by default now, and it was not. The objection was specific and
        // correct: the worker answers on its own thread, so `Opening…`, the
        // count of pages it skipped and `Could not open …` all arrived at a
        // window that had already gone, and nobody ever read one.
        //
        // That is answered rather than overruled - anything the open has to
        // say that the user must see arrives in a message box instead, from
        // `on_open` below. See `crate::notify`, and the note on
        // `Settings::hide_after_opening`.
        //
        // `request_dismiss` re-runs the gate and finds this code already at
        // the head of the list, so no second write happens.
        if self.overlay_up {
            if self.settings.hide_after_opening {
                response.merge(self.request_dismiss());
            } else {
                // Staying up, so leave the field the way a summon does: the
                // code just opened is exactly what the next keystroke should
                // replace. Inside the `overlay_up` arm rather than beside it,
                // because a window that was never summoned is not staying up -
                // it was already there, and selecting its text on an open
                // would be a keystroke nobody asked for.
                self.input.select_all();
            }
        }
        response
    }

    /// Moves the selection by `delta` ranks.
    ///
    /// Every relative move goes through here - Up and Down, PageUp and
    /// PageDown by a screen, and the same two under Ctrl - so the ends of the
    /// list behave the same way whichever of them was pressed. They did not
    /// always: a step by one wrapped while a step by a column clamped,
    /// because the two were separate copies of the same arithmetic.
    ///
    /// **A step wraps and a page does not**, which is a smaller rule than it
    /// sounds. Ueli's arrows wrap, and the reason this program's stopped is
    /// worth restating: the panel drew a twelve-row window over the list, so
    /// a step off the foot landed back on rank 0 and *the page snapped back
    /// to the first screen* - the results already read reappeared at the end,
    /// and getting back meant walking the whole list again. The content band
    /// is a scroller now; wrapping scrolls it to the top, which is where
    /// rank 0 is, and Up from the first row is the quickest way to the last.
    ///
    /// A page that wrapped would be a different matter, and Ueli has no page
    /// key to appeal to. Six rows is not a landmark, so a `PageDown` that
    /// came out somewhere near the top would be indistinguishable from one
    /// that had not moved.
    /// How far a page key moves.
    ///
    /// A screenful, which is a different number of rows in each layout. Read
    /// off the settings rather than off a constant so that switching the
    /// layout in the settings window changes the page on the next press, the
    /// same frame the rows change shape.
    fn rows_per_page(&self) -> isize {
        self.settings.result_layout.rows_per_page() as isize
    }

    fn move_selection(&mut self, delta: isize, wrap: Wrap) -> Response {
        if self.hits.is_empty() {
            return Response::none();
        }
        let len = self.hits.len() as isize;
        let Some(current) = self.selected_row().map(|i| i as isize) else {
            // Nothing selected, which `apply_hits` makes unreachable while
            // there are hits. Enter the list from the near end rather than
            // assume a rank the caller never asked for.
            let row = if delta > 0 { 0 } else { self.hits.len() - 1 };
            return self.jump_selection(row);
        };
        let target = match wrap {
            // `rem_euclid` rather than `%`, which in Rust keeps the sign of
            // the left operand - so Up from rank zero would ask for rank
            // minus one and land back on zero.
            Wrap::Around => (current + delta).rem_euclid(len),
            Wrap::Stop => (current + delta).clamp(0, len - 1),
        };
        if target == current {
            // Against the edge. The keypress still says "I am working in this
            // list", so it pins, but the frame it would produce is the one
            // already on screen and redrawing that is the thing this loop is
            // built to avoid.
            self.selection_pinned = true;
            self.selection_lost = false;
            return Response::none();
        }
        self.jump_selection(target as usize)
    }

    fn jump_selection(&mut self, row: usize) -> Response {
        if self.hits.is_empty() {
            return Response::none();
        }
        self.selection_pinned = true;
        self.selection_lost = false;
        self.selected_path = Some(Arc::clone(&self.hits[row.min(self.hits.len() - 1)].path));
        Response::redraw()
    }

    // --- results ----------------------------------------------------------

    fn on_search(&mut self, msg: super::event::SearchMsg, now: Instant) -> Response {
        if msg.epoch != self.query_epoch {
            return Response::none(); // superseded
        }
        match msg.result {
            Ok(outcome) => {
                if outcome.cancelled {
                    return Response::none();
                }
                self.apply_hits(outcome.hits);
                self.matched = outcome.matched;
                self.total = outcome.total;
                // Only ever forwards. The verification may already have spoken
                // for this query - both fall due on the same tick - and a local
                // answer is never news about the server.
                if !self.phase.server_has_spoken() {
                    self.phase = QueryPhase::Local;
                }
                self.empty_reason = if self.hits.is_empty() {
                    Some(self.no_match_reason(outcome.total))
                } else {
                    None
                };
                // The answer to the code on the line has arrived, so there is
                // nothing left to wait for. Reached by `SnapshotChanged`
                // re-running the match under a live query, by F5, and by every
                // test that delivers a result by hand - `tests/panel.rs`'s
                // `toasted` depends on it, because Enter there must raise
                // "nothing to open" rather than defer.
                self.search_due_at = None;

                let mut response = Response::redraw();
                if std::mem::take(&mut self.enter_pending) {
                    self.enter_watchdog_at = None;
                    response.merge(self.open_selection(self.viewer, now));
                }
                response
            }
            Err(QueryReject::Syntax(problem)) => {
                self.phase = QueryPhase::BadQuery {
                    detail: problem.detail().into(),
                };
                self.empty_reason = Some(EmptyReason::BadQuery {
                    detail: problem.detail().into(),
                });
                self.clear_results();
                Response::redraw()
            }
            Err(QueryReject::Empty) => {
                self.phase = QueryPhase::Idle;
                self.empty_reason = Some(EmptyReason::NoQuery);
                Response::redraw()
            }
            Err(QueryReject::ContainsNul) => {
                if !self.phase.server_has_spoken() {
                    self.phase = QueryPhase::Local;
                }
                self.empty_reason = Some(EmptyReason::NoMatches {
                    searched: self.total,
                });
                Response::redraw()
            }
        }
    }

    /// Folds one live share's answer into what is already on screen.
    ///
    /// Not `apply_hits`, which replaces. A live share answers a second after
    /// the indexes do, and replacing would empty the list and refill it - the
    /// three-hundred-row body change, with the footer reflowing around it,
    /// that `on_input_changed` has a paragraph about avoiding, except arriving
    /// a second after the typing stopped rather than during it.
    fn on_live(&mut self, msg: super::event::LiveMsg, _now: Instant) -> Response {
        if msg.epoch != self.query_epoch {
            return Response::none(); // superseded
        }
        // Worked out before `self.live` is borrowed, because describing a
        // failure reads the settings and the routing table. Cheap, and only
        // when there is one to describe.
        let failure = match &*msg.outcome {
            crate::search::live::LiveOutcome::Failed(err) => {
                Some(self.describe_drive_error(msg.mapping, *err))
            }
            _ => None,
        };

        let Some(progress) = self.live.as_mut() else {
            // The field was cleared while this was in flight, which means the
            // query it answers is no longer on the line.
            return Response::none();
        };
        progress.outstanding = progress.outstanding.saturating_sub(1);

        let extra = match *msg.outcome {
            crate::search::live::LiveOutcome::Answered {
                hits,
                matched,
                coverage,
            } => {
                progress.reached.push((msg.mapping, coverage));
                self.matched = self.matched.saturating_add(matched);
                hits
            }
            crate::search::live::LiveOutcome::Skipped(skip) => {
                progress.skipped.push((msg.mapping, skip));
                Vec::new()
            }
            crate::search::live::LiveOutcome::Failed(_) => {
                progress
                    .failed
                    .push((msg.mapping, failure.unwrap_or_default()));
                Vec::new()
            }
        };

        let changed = self.merge_hits(extra);
        // Whether the list grew or not, the *reason* an empty list is empty
        // may just have changed - a share that could not be asked turns "no
        // matches" into "not searched".
        if self.hits.is_empty() {
            self.empty_reason = Some(self.no_match_reason(self.total));
        }
        let _ = changed;
        Response::redraw()
    }

    /// Adds rows to the list already on screen, keeping it ranked.
    ///
    /// The union is re-ranked rather than appended: which share a file came
    /// from is shown on its row and must not decide where it sits, which is
    /// the same rule `matcher::merge_all` follows.
    fn merge_hits(&mut self, extra: Vec<Hit>) -> bool {
        if extra.is_empty() {
            return false;
        }
        let mut merged = std::mem::take(&mut self.hits);
        merged.extend(extra);
        // By the same key the matcher ranks with, then by path so that two
        // shares offering equally good matches order predictably rather than
        // by which answered first.
        merged.sort_by(|a, b| {
            a.is_inherited()
                .cmp(&b.is_inherited())
                .then(a.match_pos.cmp(&b.match_pos))
                .then(a.name.len().cmp(&b.name.len()))
                .then_with(|| a.path.cmp(&b.path))
        });
        merged.dedup_by(|a, b| a.path.eq_ignore_ascii_case(&b.path));
        merged.truncate(crate::config::MAX_RESULTS);

        if merged == self.hits {
            self.hits = merged;
            return false;
        }
        // `place_selection` keeps the selection by *path*, so a row arriving
        // above the cursor moves the cursor's rank without moving the file it
        // is on.
        self.apply_hits(merged);
        true
    }

    /// Chooses the honest explanation for an empty list.
    fn no_match_reason(&self, searched: u32) -> EmptyReason {
        // Nothing has asked the drives yet, so there is no answer to report -
        // only a wait. Without this an empty local sweep would render as "no
        // matches" for the 600ms before the query even goes out, and on a
        // share whose whole index is what past searches found, that first
        // answer is empty far more often than not.
        if self.settings.routes.live().next().is_some()
            && self.live.as_ref().is_none_or(|l| l.is_asking())
        {
            return EmptyReason::NotSearchedYet;
        }

        // A live share that could not be asked, or could not be asked about
        // everything, outranks every other explanation: it is the only one
        // where "nothing matched" would be a claim about folders nobody
        // looked in. Checked before the unreachable-index case for the same
        // reason that one is checked before `NoMatches`.
        if let Some(live) = &self.live
            && !live.is_asking()
            && !live.complete()
        {
            if let Some((id, detail)) = live.failed.first() {
                return EmptyReason::LiveUnavailable {
                    name: self.settings.routes.label(*id).to_string(),
                    detail: detail.clone(),
                };
            }
            if let Some((id, skip)) = live.skipped.first() {
                return EmptyReason::LiveUnavailable {
                    name: self.settings.routes.label(*id).to_string(),
                    detail: skip.label(),
                };
            }
            if let Some((id, coverage)) = live.worst() {
                return EmptyReason::LiveIncomplete {
                    name: self.settings.routes.label(id).to_string(),
                    searched: coverage.dirs_queried,
                    skipped: coverage.dirs_skipped,
                };
            }
        }
        match self.index.unreachable() {
            Some((id, crate::index::Health::Unreachable { err, .. })) if searched == 0 => {
                EmptyReason::IndexUnavailable {
                    // The share that actually failed, not the first flat one.
                    detail: self.describe_drive_error(id, *err),
                }
            }
            _ => EmptyReason::NoMatches { searched },
        }
    }

    pub fn shares_cursor(&self) -> usize {
        self.shares_cursor
    }

    /// The shares the update list offers, in configuration order.
    ///
    /// Only the ones a search actually visits: offering to update a share that
    /// is switched off, or has no path, would be offering to do nothing.
    pub fn share_ids(&self) -> Vec<MappingId> {
        self.settings
            .routes
            .enabled()
            .filter(|m| m.kind.is_indexed() && !m.path.as_os_str().is_empty())
            .map(|m| m.id)
            .collect()
    }

    /// One share, as the drive picker shows it.
    ///
    /// Assembled here rather than in the renderer because it joins three things
    /// the state owns - the mapping, its status, and whether the overview has
    /// judged it stale - and a renderer that joined them itself would be a
    /// second opinion about which drive wants updating.
    pub fn share_row(&self, id: MappingId) -> Option<crate::view::shares::Row<'_>> {
        let mapping = self.settings.routes.enabled().find(|m| m.id == id)?;
        let status = self.status_of(id)?;
        Some(crate::view::shares::Row {
            mapping,
            status,
            // The store's own note - events lost, live updates off - outranks
            // mere age, because it says the index is wrong rather than old.
            stale: status.stale.or_else(|| {
                self.index
                    .stalest
                    .filter(|(stale_id, _)| *stale_id == id)
                    .map(|(_, why)| why)
            }),
        })
    }

    /// One status per mapping, for anything needing detail the overview does
    /// not carry - tree coverage, or why a cache was rejected.
    pub fn status_of(&self, id: MappingId) -> Option<&Arc<IndexStatus>> {
        self.statuses.get(id.index())
    }

    /// Installs a new result set while keeping the cursor where the user put
    /// it.
    ///
    /// There used to be a second half to this: the state held the first rank
    /// on screen, and every arm of `place_selection` below had to remember to
    /// move it, because forgetting on one arm was a cursor on a screen nobody
    /// could see. The content band is an `egui::ScrollArea` now and owns its
    /// own offset, so the only thing that has to survive a result set landing
    /// under the cursor is the cursor.
    fn apply_hits(&mut self, hits: Vec<Hit>) {
        self.place_selection(hits);
    }

    fn place_selection(&mut self, hits: Vec<Hit>) {
        let previous_row = self.selected_row();
        let hovered_path = self
            .hovered
            .and_then(|rank| self.hits.get(rank))
            .map(|h| Arc::clone(&h.path));
        self.hits = hits;

        // The hover is a *rank*, and ranks do not survive the list being
        // replaced: results land from another thread, and a verification
        // replaces the whole list. Rank 100 of a list that is now empty would
        // be a highlight drawn off the end of the panel.
        //
        // Never clamped, which is the opposite of what the selection below
        // does - and deliberately. The selection is tracked by *path* because
        // Enter opens it and moving it silently would open the wrong file; a
        // hover is only ever a description of where the pointer is, and
        // clamping it would invent a hover the pointer is not over.
        //
        // It is kept, though, when the row at that rank is the same file it
        // was. That is not the same claim as clamping: nothing moved, so the
        // pointer really is still over what it was over. Dropping it
        // unconditionally meant the highlight under the pointer blinked out on
        // every result set that arrived - which, with the local matcher
        // answering per keystroke, was every character typed.
        self.hovered = match (self.hovered, hovered_path) {
            (Some(rank), Some(was)) if self.hits.get(rank).is_some_and(|h| h.path == was) => {
                Some(rank)
            }
            _ => None,
        };

        if let Some(target) = self.selected_path.clone() {
            if self.hits.iter().any(|h| h.path == target) {
                // Still present: keep it, wherever it moved to.
                self.selection_lost = false;
                return;
            }
            if self.selection_pinned {
                // Pinned but gone. Clamp to the same ordinal position rather
                // than teleporting to the top, and say so.
                self.selection_lost = true;
                if let Some(row) = previous_row {
                    let row = row.min(self.hits.len().saturating_sub(1));
                    self.selected_path = self.hits.get(row).map(|h| Arc::clone(&h.path));
                    return;
                }
            }
        }

        // Both the pinned and unpinned cases land here only once the pinned
        // path above has already failed to find anywhere better, so there is
        // one answer: the top of the new list.
        self.selected_path = self.hits.first().map(|h| Arc::clone(&h.path));
    }

    // --- verification -----------------------------------------------------

    fn on_verify(&mut self, msg: super::event::VerifyMsg, now: Instant) -> Response {
        if msg.epoch != self.query_epoch {
            return Response::none();
        }
        self.verify_watchdog_at = None;
        self.last_verified_query = Some(msg.query);

        // Deliberately *not* where the code is remembered any more. That rode
        // on `VERIFY_DEBOUNCE`, which is 300ms - an ordinary mid-word pause -
        // so `11`, `11-D` and `11-D-07` all reached the recall list on the way
        // to `11-D-0704`. Remembering is now its own deadline with its own
        // constant; see `remember_due_at` and `on_tick`.
        match msg.outcome {
            VerifyOutcome::IndexAuthoritative { .. } => {
                // The directory has not changed, so what is on screen is
                // already correct. No query was issued at all.
                self.phase = QueryPhase::Verified {
                    took: msg.elapsed,
                    by_stamp: true,
                };
                Response::redraw()
            }
            VerifyOutcome::Server {
                hits,
                matched,
                capped,
                audit,
            } => {
                // The server result replaces rather than unions: a union
                // would keep showing files that have since been deleted,
                // which is worse than being briefly stale.
                self.apply_hits(hits);
                self.matched = matched;
                self.empty_reason = if self.hits.is_empty() {
                    Some(EmptyReason::NoMatches {
                        searched: self.total,
                    })
                } else {
                    None
                };
                self.phase = QueryPhase::Verified {
                    took: msg.elapsed,
                    by_stamp: false,
                };
                if capped {
                    self.set_toast(
                        "Too many matches to count exactly".into(),
                        Severity::Info,
                        now,
                    );
                }
                if let AuditVerdict::ServerUnderReturned { missing } = audit {
                    let n = missing.len();
                    let unit = if n == 1 { "file" } else { "files" };
                    self.set_toast(
                        format!("Server filter missed {n} {unit} \u{b7} using the local index"),
                        Severity::Warn,
                        now,
                    );
                }
                Response::redraw()
            }
            VerifyOutcome::Skipped(reason) => {
                // Not an error: the local results stand.
                self.phase = match reason {
                    SkipReason::AuditFailed { .. } => QueryPhase::VerifyFailed {
                        detail: reason.label(),
                    },
                    _ => QueryPhase::Local,
                };
                Response::redraw()
            }
            VerifyOutcome::Failed(err) => {
                // Verification only ever runs against a flat share, and only
                // when there is exactly one - see `worker::run_verify` - so
                // that is the share this failure belongs to. Naming it beats
                // the old `flat_label()`, which named the first flat mapping
                // whatever had actually been checked.
                let said = match self.settings.routes.flat().next().map(|m| m.id) {
                    Some(id) => self.describe_drive_error(id, err),
                    // "Drive", because this reaches a status line. The code
                    // says share and the screen says drive.
                    None => err.describe_for("the drive", self.settings.dev_mode),
                };
                self.phase = QueryPhase::VerifyFailed { detail: said };
                Response::redraw()
            }
        }
    }

    fn clear_verifying(&mut self) {
        if self.phase.is_verifying() {
            self.phase = QueryPhase::Local;
        }
        self.verify_watchdog_at = None;
    }

    // --- background -------------------------------------------------------

    fn on_index(&mut self, msg: IndexMsg, now: Instant) -> Response {
        match msg {
            IndexMsg::Status { id, status } => {
                // Dropped rather than panicking: an actor running against a
                // routing table this state does not have is a bug, but not a
                // reason to take the terminal down with it.
                let Some(slot) = self.statuses.get_mut(id.index()) else {
                    return Response::none();
                };
                *slot = status;
                self.index = IndexOverview::of(&self.settings.routes, &self.statuses);
                Response::redraw()
            }
            IndexMsg::SnapshotChanged => {
                // Re-run the local match so the display agrees with the
                // index. Deliberately does NOT re-arm the verify debounce -
                // that cycle would be a livelock.
                if self.query.is_searchable() && self.settings.routes.any_searchable() {
                    return Response::redraw().with(Cmd::Search {
                        query: self.query.clone(),
                        epoch: self.query_epoch,
                    });
                }
                Response::redraw()
            }
            IndexMsg::RefreshReport {
                id,
                entries,
                elapsed,
                error,
            } => {
                let text = match error {
                    // Named by its *path* in dev mode: this is a failure the
                    // user is expected to go and fix, and a chosen name does
                    // not say which drive letter is missing.
                    Some(err) => {
                        format!(
                            "Refresh failed \u{b7} {}",
                            self.describe_drive_error(id, err)
                        )
                    }
                    // The name, because this is scope rather than failure and
                    // the path would add nothing.
                    None => format!(
                        "Refreshed {} \u{b7} {} files \u{b7} {}",
                        self.settings.routes.label(id),
                        crate::util::humanize::count(entries),
                        crate::util::humanize::elapsed(elapsed)
                    ),
                };
                let severity = if error.is_some() {
                    Severity::Error
                } else {
                    Severity::Info
                };
                self.set_toast(text, severity, now);
                Response::redraw()
            }
        }
    }

    /// The window gained or lost the keyboard.
    ///
    /// Losing it is how a launcher knows to get out of the way: the user has
    /// clicked on something else, and the panel is over it. Gaining it is
    /// nothing - the hotkey thread reports a summon as
    /// [`HotkeyMsg::Summoned`], which is a stronger fact and arrives first.
    ///
    /// Guarded three ways, and each guard is a bug that would otherwise be
    /// reachable. `overlay_up` because a window nobody summoned has nothing
    /// to dismiss. The setting because it is a setting. And the shell drops
    /// the event entirely while an auxiliary window of ours has the keyboard,
    /// because the settings window *is* somewhere else to click and closing
    /// the panel would take it with them.
    fn on_focus(&mut self, has_focus: bool) -> Response {
        if has_focus || !self.overlay_up || !self.settings.hide_on_blur {
            return Response::none();
        }
        self.request_dismiss()
    }

    fn on_open(&mut self, msg: OpenMsg, now: Instant) -> Response {
        match msg {
            // A document that opened whole says nothing. One that lost pages
            // has to say so: a drawing set silently missing page seven is the
            // worst outcome available here, because nothing on screen would
            // ever reveal it.
            OpenMsg::Launched {
                pages,
                skipped,
                truncated,
                ..
            } => {
                if skipped.is_empty() && !truncated {
                    // Clears the "opening..." notice rather than leaving it up
                    // for its full lifetime after the viewer has appeared.
                    self.toast = None;
                    self.toast_expires_at = None;
                    return Response::redraw();
                }
                // Two different kinds of incomplete, and both have to be said
                // out loud: a document quietly missing pages is the worst
                // outcome this path can produce, because nothing on screen
                // would ever reveal it.
                let mut text = if truncated {
                    format!("Opened the first {pages} pages \u{b7} the set is longer")
                } else {
                    let total = pages + skipped.len();
                    format!("Opened {pages} of {total} pages")
                };
                if !skipped.is_empty() {
                    text.push_str(" \u{b7} skipped ");
                    text.push_str(&skipped.join(", "));
                }
                self.set_toast(text.clone(), Severity::Warn, now);
                self.also_say("Opened, but not whole", text)
            }
            OpenMsg::Failed { path, detail } => {
                let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
                let title = format!("Could not open {name}");
                self.set_toast_detailed(title.clone(), detail.clone(), Severity::Error, now);
                self.also_say(&title, detail)
            }
            OpenMsg::ViewerSaved { viewer } => {
                // `display`, not `name`: the latter is the config spelling
                // and is round-tripped through the file.
                self.set_toast(format!("Viewer: {}", viewer.display()), Severity::Info, now);
                Response::redraw()
            }
            OpenMsg::SettingSaved { label } => {
                self.set_toast(format!("Saved \u{b7} {label}"), Severity::Info, now);
                Response::redraw()
            }
            // The change is already in force for this session - the state
            // machine applied it before asking for it to be written - so the
            // message says what did and did not happen rather than implying
            // nothing did.
            OpenMsg::SettingSaveFailed { label, detail } => {
                self.set_toast_detailed(
                    format!("{label} changed for this session only"),
                    detail,
                    Severity::Warn,
                    now,
                );
                Response::redraw()
            }
            // Not fatal, and not silent: the toggle still applies to this
            // session, so the message says what did and did not happen.
            OpenMsg::ViewerSaveFailed { detail } => {
                self.set_toast_detailed(
                    "Viewer changed for this session only",
                    detail,
                    Severity::Warn,
                    now,
                );
                Response::redraw()
            }
        }
    }

    /// Raises a message box as well as the toast, when there is no panel to
    /// read the toast on.
    ///
    /// The whole of what makes `hide_after_opening` safe to ship switched on.
    /// An open is answered on another thread, long after the panel has gone,
    /// and a document quietly missing page seven is the worst outcome this
    /// program can produce - nothing on screen would ever reveal it. So the
    /// panel is allowed to leave and the message follows the user instead.
    ///
    /// The toast is still raised, and that is not redundant: the panel may
    /// have been summoned again by the time this lands, in which case there
    /// *is* somewhere to read it and `overlay_up` says so.
    fn also_say(&self, title: impl Into<String>, detail: impl Into<String>) -> Response {
        if self.overlay_up {
            return Response::redraw();
        }
        Response::redraw().with(Cmd::Announce {
            title: title.into(),
            detail: detail.into(),
        })
    }

    // --- timers -----------------------------------------------------------

    fn on_tick(&mut self, now: Instant) -> Response {
        let mut response = Response::none();

        // The local match, first. It is the cheap half of the same pause, and
        // the answer it produces is what the verification below gets checked
        // against - so when both fall due on one tick, the matcher is asked
        // first and the phase ends up where `on_verify` wants it.
        if let Some(due) = self.search_due_at
            && now >= due
        {
            self.search_due_at = None;
            if self.query.is_searchable() && self.settings.routes.any_searchable() {
                response.merge(Response::redraw().with(Cmd::Search {
                    query: self.query.clone(),
                    epoch: self.query_epoch,
                }));
            }
        }

        // A matcher that never answered an Enter. Hands the keystroke back
        // rather than leaving it dead.
        if let Some(due) = self.enter_watchdog_at
            && now >= due
        {
            self.enter_watchdog_at = None;
            self.enter_pending = false;
            self.set_toast("The search did not answer".into(), Severity::Warn, now);
            response.merge(Response::redraw());
        }

        if let Some(due) = self.live_due_at
            && now >= due
        {
            self.live_due_at = None;
            // The server's floor as well as the panel's. `is_searchable` is
            // about to be true for a single character, and dispatching that
            // would be a network round trip to every live share on the first
            // keystroke - which the footer would then report as "asking" and
            // "not searched" in the same second.
            if self.query.is_searchable()
                && self.query.term().chars().count() >= MIN_SERVER_QUERY_LEN
            {
                let outstanding = self.settings.routes.live().count();
                self.live = Some(LiveProgress::asking(now, outstanding));
                response.merge(Response::redraw().with(Cmd::Live {
                    query: self.query.clone(),
                    epoch: self.query_epoch,
                }));
            }
        }

        if let Some(due) = self.verify_due_at
            && now >= due
        {
            self.verify_due_at = None;
            if self.input.chars().count() >= MIN_TERM_LEN {
                self.phase = QueryPhase::Verifying { since: now };
                self.verify_watchdog_at = Some(now + VERIFY_WATCHDOG);
                response.merge(Response::redraw().with(Cmd::Verify {
                    query: self.query.clone(),
                    epoch: self.query_epoch,
                }));
            }
        }

        // The typing has stopped for long enough that the code on the line is
        // a code somebody meant rather than a prefix on the way to one. This
        // used to ride on the verification coming back, which is the same
        // event 300ms earlier - long enough to catch a finished code, and
        // also long enough to catch every pause in the middle of one.
        if let Some(due) = self.remember_due_at
            && now >= due
        {
            self.remember_due_at = None;
            if let Some(cmd) = self.remember_if_settled() {
                response.merge(Response::redraw().with(cmd));
            }
        }

        // Independent of the worker: guards against a wedged or panicked
        // verify leaving a spinner up forever.
        if let Some(due) = self.verify_watchdog_at
            && now >= due
        {
            self.verify_watchdog_at = None;
            self.phase = QueryPhase::VerifyFailed {
                detail: "verification timed out".into(),
            };
            response.merge(Response::redraw());
        }

        if let Some(due) = self.toast_expires_at
            && now >= due
        {
            self.toast_expires_at = None;
            self.toast = None;
            response.merge(Response::redraw());
        }

        // The other half of the deadline above, and not optional. A deadline
        // with no redraw arm is a busy spin: the loop wakes, synthesises a
        // tick, gets `Response::none()`, leaves `dirty` false and re-blocks on
        // a deadline already in the past. Both edits or neither.
        if let Some(due) = self.next_text_change()
            && now >= due
        {
            response.merge(Response::redraw());
        }

        response
    }

    /// The checker reporting what it saw.
    ///
    /// Said once per version rather than once per look. The timer fires every
    /// few hours and the answer does not change between releases, so a toast
    /// each time would be nagging about something the settings window is
    /// already showing.
    fn on_update(&mut self, msg: crate::app::event::UpdateMsg, now: Instant) -> Response {
        let crate::app::event::UpdateMsg::Looked(found) = msg;

        if let crate::update::Found::Available { manifest, .. } = found.as_ref()
            && offered_version(self.update.as_ref()) != Some(manifest.version)
        {
            self.set_toast(
                format!(
                    "Version {} is available \u{b7} open the settings to install it",
                    manifest.version
                ),
                Severity::Info,
                now,
            );
        }
        self.update = Some(*found);
        Response::redraw()
    }

    /// Keeps one status per configured drive.
    ///
    /// `MappingId` is a position in the list, and `statuses` is indexed by it,
    /// so a list that changed length leaves a report from an actor filed
    /// against a slot that is not there. Existing entries are kept: a drive
    /// that did not move keeps what is known about it.
    pub(super) fn resize_statuses(&mut self) {
        let wanted = self.settings.routes.all().len();
        self.statuses.truncate(wanted);
        while self.statuses.len() < wanted {
            self.statuses.push(Arc::new(IndexStatus::default()));
        }
    }

    /// A message whose second half is only for somebody debugging.
    ///
    /// Outside dev mode the caller's sentence is the whole message. Inside it,
    /// the detail is joined on with ` · ` like any other independent fact.
    /// Written this way round - plain first, detail supplied separately -
    /// because the alternative is a `format!` at the call site that has to be
    /// taken apart again, and one of them would eventually not be.
    fn set_toast_detailed(
        &mut self,
        plain: impl Into<String>,
        detail: impl Into<String>,
        severity: Severity,
        now: Instant,
    ) {
        let plain = plain.into();
        let text = match self.technical(detail) {
            Some(detail) => format!("{plain} \u{b7} {detail}"),
            None => plain,
        };
        self.set_toast(text, severity, now);
    }

    fn set_toast(&mut self, text: String, severity: Severity, now: Instant) {
        self.toast = Some(Toast { text, severity });
        self.toast_expires_at = Some(now + TOAST_LIFETIME);
    }

    /// Age of the oldest served listing, for the status line.
    pub fn index_age(&self, now: SystemTime) -> Option<Duration> {
        self.index.age(now)
    }
}

/// The version a previous look offered, if it offered one.
///
/// Free-standing rather than a method, because it answers a question about a
/// value rather than about the state - and the caller needs it while holding
/// a borrow of the field it reads.
fn offered_version(found: Option<&crate::update::Found>) -> Option<crate::update::Version> {
    match found? {
        crate::update::Found::Available { manifest, .. } => Some(manifest.version),
        _ => None,
    }
}
