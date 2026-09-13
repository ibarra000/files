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

pub use model::{EmptyReason, QueryPhase, Severity, TOAST_LIFETIME, Toast};

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use super::event::{AppEvent, ClipboardMsg, Cmd, IndexMsg, OpenMsg, Redraw, Response};
use super::input::{self, Input};
use crate::config::COUNTDOWN_TICK;
use crate::config::{
    ANIMATION_TICK, MIN_QUERY_LEN, REMEMBER_DEBOUNCE, Settings, VERIFY_DEBOUNCE, VERIFY_WATCHDOG,
    VISIBLE_ROWS, ViewerKind,
};
use crate::history::History;
use crate::index::store::{IndexOverview, IndexStatus};
use crate::paths::MappingId;
use crate::search::matcher::{Hit, QueryReject};
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
    pub hits: Vec<Hit>,
    pub matched: u32,
    pub total: u32,
    pub phase: QueryPhase,
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
    /// The first result rank on screen. See [`Self::scroll_into_view`].
    scroll_top: usize,
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

    query_epoch: u64,
    verify_due_at: Option<Instant>,
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
    last_verified_query: Option<String>,
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
        Self {
            settings,
            input: Input::new(),
            history: History::new(),
            overlay_up: false,
            picking_share: false,
            // Replaced by the real size before the first frame; a sane default
            // means mouse arithmetic is never done against a zero rect.
            hits: Vec::new(),
            matched: 0,
            total: 0,
            phase: QueryPhase::Idle,
            empty_reason: Some(EmptyReason::NoQuery),
            statuses,
            shares_cursor: 0,
            index: IndexOverview::default(),
            toast: None,
            should_quit: false,
            selection_pinned: false,
            selected_path: None,
            selection_lost: false,
            scroll_top: 0,
            avwin_missing: false,
            viewer,
            query_epoch: 0,
            verify_due_at: None,
            remember_due_at: None,
            verify_watchdog_at: None,
            toast_expires_at: None,
            last_frame: now,
            // The epoch until the first frame reports a real one. Nothing
            // reads it before then: `next_text_change` needs a published
            // index, which needs an actor to have answered.
            last_frame_wall: SystemTime::UNIX_EPOCH,
            last_verified_query: None,
            help_scroll: 0,
            hovered: None,
        }
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

    /// True while something is in flight and the spinner should advance.
    ///
    /// Derived from state rather than stored: a sticky flag is exactly how a
    /// UI ends up spinning forever after the work has finished.
    pub fn wants_animation(&self) -> bool {
        self.phase.is_verifying() || self.index.is_busy()
    }

    pub fn note_frame(&mut self, now: Instant, wall: SystemTime) {
        self.last_frame = now;
        self.last_frame_wall = wall;
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
            self.verify_due_at,
            self.remember_due_at,
            self.verify_watchdog_at,
            self.toast_expires_at,
            self.wants_animation()
                .then(|| self.last_frame + ANIMATION_TICK),
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
        self.dispatch(event, now)
    }

    fn dispatch(&mut self, event: AppEvent, now: Instant) -> Response {
        match event {
            AppEvent::Key(key) => self.on_key(key, now),
            AppEvent::Intent(intent) => self.on_intent(intent, now),
            AppEvent::Paste(text) => self.on_paste(&text, now),
            AppEvent::Tick => self.on_tick(now),
            AppEvent::Search(msg) => self.on_search(msg),
            AppEvent::Verify(msg) => self.on_verify(msg, now),
            AppEvent::Index(msg) => self.on_index(msg, now),
            AppEvent::Open(msg) => self.on_open(msg, now),
            AppEvent::Clipboard(msg) => self.on_clipboard(msg, now),
            AppEvent::Hotkey(msg) => self.on_hotkey(msg, now),
            AppEvent::ActorDied { actor, detail } => {
                self.clear_verifying();
                self.set_toast(
                    format!("{actor} stopped unexpectedly: {detail}"),
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
        self.on_input_changed(now)
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
                self.set_toast(format!("copied {chars} {unit}"), Severity::Info, now);
                Response::redraw()
            }
            ClipboardMsg::Read { text } => {
                let cleaned = input::sanitize(&text);
                if cleaned.is_empty() {
                    self.set_toast("the clipboard holds no text".into(), Severity::Info, now);
                    return Response::redraw();
                }
                self.leave_history();
                self.input.insert_str(&cleaned);
                self.on_input_changed(now)
            }
            ClipboardMsg::Failed { detail } => {
                self.set_toast(format!("clipboard: {detail}"), Severity::Warn, now);
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
        self.scroll_top = 0;
        self.hovered = None;
        self.matched = 0;
        self.total = 0;
        self.selected_path = None;
    }

    fn on_input_changed(&mut self, now: Instant) -> Response {
        // Editing the code accepts whatever was being previewed: the text in
        // the field is now something the user typed rather than something they
        // were looking at. This is the one place it happens, so no key handler
        // has to remember to do it.
        self.leave_history();
        self.query_epoch += 1;
        // Editing the code un-pins: the user is choosing a different code, not
        // holding a place in the list for this one. The selected *path* is kept
        // - see below - so a result set that still contains it keeps it.
        self.selection_pinned = false;
        self.selection_lost = false;
        self.verify_watchdog_at = None;
        self.last_verified_query = None;

        let chars = self.input.chars().count();
        if chars == 0 {
            self.phase = QueryPhase::Idle;
            self.empty_reason = Some(EmptyReason::NoQuery);
            self.verify_due_at = None;
            self.remember_due_at = None;
            self.clear_results();
            return Response::redraw();
        }
        if chars < MIN_QUERY_LEN {
            self.phase = QueryPhase::TooShort {
                need: MIN_QUERY_LEN,
            };
            self.empty_reason = Some(EmptyReason::QueryTooShort {
                need: MIN_QUERY_LEN,
            });
            self.verify_due_at = None;
            self.remember_due_at = None;
            self.clear_results();
            return Response::redraw();
        }

        if !self.settings.routes.any_indexed() {
            self.phase = QueryPhase::NoShares;
            self.empty_reason = Some(EmptyReason::NoSharesConfigured);
            self.verify_due_at = None;
            self.remember_due_at = None;
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

        // The local match is sub-millisecond, so it runs on every keystroke
        // with no debounce at all. Only the network-bound work is delayed.
        let mut response = Response::redraw().with(Cmd::Search {
            query: self.input.text().to_string(),
            epoch: self.query_epoch,
        });

        self.verify_due_at = Some(now + VERIFY_DEBOUNCE);
        self.remember_due_at = Some(now + REMEMBER_DEBOUNCE);

        response.redraw = Redraw::Yes;
        response
    }

    fn on_enter(&mut self, now: Instant) -> Response {
        // Enter takes the row you are on. On a remembered code that means
        // filling the field and searching; on a file it means opening it. One
        // rule, two kinds of row - and the rows look different enough that
        // nobody has to be told which is which.
        if self.history.is_browsing() {
            return self.accept_recall(now);
        }

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
                self.set_toast("nothing to open".into(), Severity::Info, now);
                return response;
            }
            return Response::none();
        };
        let path = Arc::clone(&hit.path);

        // Opening a file is the strongest possible signal that this code was
        // the one meant, so it is remembered here rather than waiting out the
        // quiet period - someone who opens a result the moment it appears must
        // not lose the code.
        let code = self.input.text().to_string();
        let request = crate::open::OpenRequest {
            path,
            // The typed code, not the selected row: the page set is rebuilt
            // from it because `hits` is capped at MAX_RESULTS and ranked by
            // match position, so a long document would arrive truncated and
            // out of order.
            query: code.clone(),
            viewer: self.viewer,
        };
        let mut response = Response::none().with(Cmd::Open(request));

        // Assembling a document means reading every page off the share, which
        // is not instant. Enter used to return silently because handing one
        // path to one program was; saying nothing for a second or more now
        // would read as the keypress having been ignored.
        if self.viewer == ViewerKind::Pdf {
            self.set_toast("opening...".into(), Severity::Info, now);
            response.redraw = Redraw::Yes;
        }

        // Ungated, and the only commit point that is: a file opening is the
        // code being used, which outranks any question about whether the
        // typing has stopped. Clears the pending deadline on the way through,
        // so the tick it was armed for has nothing left to do.
        if let Some(cmd) = self.remember_now() {
            response = response.with(cmd);
        }

        // The overlay exists to be got rid of: the drawing is opening, so the
        // search is over. `request_dismiss` re-runs the gate and finds this
        // code already at the head of the list, so no second write happens.
        if self.overlay_up {
            response.merge(self.request_dismiss());
        }
        response
    }

    /// Moves the selection by `delta` ranks.
    ///
    /// Every relative move goes through here - Down, the wheel, Left and Right
    /// by a column, PageUp and PageDown by a screen - so the ends of the list
    /// behave the same way whichever of them was pressed. They did not before:
    /// a step by one wrapped while a step by a column clamped, because the two
    /// were separate copies of the same arithmetic.
    ///
    /// Both ends stop. The list used to be circular, so a step off the foot
    /// landed back on rank 0 and the page snapped back to the first screen -
    /// the results already read reappeared at the end, and returning to where
    /// someone was meant walking the whole list again. Stopping means the way
    /// back is the way they came.
    fn move_selection(&mut self, delta: isize) -> Response {
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
        let target = (current + delta).clamp(0, len - 1);
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
        self.scroll_into_view();
        Response::redraw()
    }

    /// The window over the result list: the first rank on screen.
    ///
    /// The list holds up to [`crate::config::MAX_RESULTS`] and the panel has
    /// room for [`VISIBLE_ROWS`], so most of a broad search is off screen. This
    /// is where.
    pub fn scroll_top(&self) -> usize {
        self.scroll_top
    }

    /// The ranks currently on screen.
    pub fn visible_rows(&self) -> std::ops::Range<usize> {
        let start = self.scroll_top.min(self.hits.len());
        start..(start + VISIBLE_ROWS).min(self.hits.len())
    }

    /// Moves the window as little as it takes to contain the selected row.
    ///
    /// The **only** place `scroll_top` moves, called from the only two places
    /// that can invalidate it: [`Self::jump_selection`], which moves the cursor,
    /// and [`Self::apply_hits`], which moves the list out from under it. A
    /// third caller would be a third opinion about where the window is.
    ///
    /// The terminal build derived its page from the selection instead, and its
    /// note argued a stored offset would be "a second source of truth that
    /// every result update would have to keep in step". That was written for a
    /// three-column grid, where the page was the unit somebody moved in. For a
    /// single column of twelve, flipping the whole list on the twelfth Down is
    /// worse than sliding it by one - so the offset is stored, and the
    /// invariant it has to hold is asserted directly by the interleaving
    /// fuzzer rather than argued about here.
    fn scroll_into_view(&mut self) {
        let Some(row) = self.selected_row() else {
            self.scroll_top = 0;
            return;
        };
        if row < self.scroll_top {
            self.scroll_top = row;
        } else if row >= self.scroll_top + VISIBLE_ROWS {
            self.scroll_top = row + 1 - VISIBLE_ROWS;
        }
        // A list that shrank under a window near its end would otherwise leave
        // the window pointing past it, showing fewer rows than there is room
        // for with nothing below them.
        self.scroll_top = self
            .scroll_top
            .min(self.hits.len().saturating_sub(VISIBLE_ROWS));
    }

    // --- results ----------------------------------------------------------

    fn on_search(&mut self, msg: super::event::SearchMsg) -> Response {
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
                self.phase = QueryPhase::Local;
                self.empty_reason = if self.hits.is_empty() {
                    Some(self.no_match_reason(outcome.total))
                } else {
                    None
                };
                Response::redraw()
            }
            Err(QueryReject::TooShort { need }) => {
                self.phase = QueryPhase::TooShort { need };
                self.empty_reason = Some(EmptyReason::QueryTooShort { need });
                Response::redraw()
            }
            Err(QueryReject::ContainsNul) => {
                self.phase = QueryPhase::Local;
                self.empty_reason = Some(EmptyReason::NoMatches {
                    searched: self.total,
                });
                Response::redraw()
            }
        }
    }

    /// Chooses the honest explanation for an empty list.
    fn no_match_reason(&self, searched: u32) -> EmptyReason {
        match self.index.unreachable() {
            Some((id, crate::index::Health::Unreachable { err, .. })) if searched == 0 => {
                EmptyReason::IndexUnavailable {
                    // The share that actually failed, not the first flat one.
                    detail: err.describe(&self.settings.routes.path_label(id)),
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
    /// it, and the window where the cursor is.
    ///
    /// Split so that `place_selection` below can return early from any of its
    /// four arms without each one having to remember the window. Forgetting it
    /// on one arm is a cursor on a screen nobody can see, which is the failure
    /// the stored offset has to be proof against.
    fn apply_hits(&mut self, hits: Vec<Hit>) {
        self.place_selection(hits);
        self.scroll_into_view();
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
                        "too many matches to count exactly".into(),
                        Severity::Info,
                        now,
                    );
                }
                if let AuditVerdict::ServerUnderReturned { missing } = audit {
                    self.set_toast(
                        format!(
                            "server filter missed {} file(s); falling back to the local index",
                            missing.len()
                        ),
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
                let target = self
                    .settings
                    .routes
                    .flat()
                    .next()
                    .map(|m| self.settings.routes.path_label(m.id))
                    .unwrap_or_else(|| "the share".into());
                self.phase = QueryPhase::VerifyFailed {
                    detail: err.describe(&target),
                };
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
                if self.input.chars().count() >= MIN_QUERY_LEN && self.settings.routes.any_indexed()
                {
                    return Response::redraw().with(Cmd::Search {
                        query: self.input.text().to_string(),
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
                    // The path, because this is a failure the user is expected
                    // to go and fix, and a chosen name does not say which
                    // drive letter is missing.
                    Some(err) => format!(
                        "refresh failed: {}",
                        err.describe(&self.settings.routes.path_label(id))
                    ),
                    // The name, because this is scope rather than failure and
                    // the path would add nothing.
                    None => format!(
                        "refreshed {} · {} files in {}",
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
                    format!("opened the first {pages} pages; the set is longer")
                } else {
                    let total = pages + skipped.len();
                    format!("opened {pages} of {total} pages")
                };
                if !skipped.is_empty() {
                    text.push_str("; skipped ");
                    text.push_str(&skipped.join(", "));
                }
                self.set_toast(text, Severity::Warn, now);
                Response::redraw()
            }
            OpenMsg::Failed { path, detail } => {
                let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
                self.set_toast(
                    format!("could not open {name}: {detail}"),
                    Severity::Error,
                    now,
                );
                Response::redraw()
            }
            OpenMsg::ViewerSaved { viewer } => {
                self.set_toast(format!("viewer: {}", viewer.name()), Severity::Info, now);
                Response::redraw()
            }
            // Not fatal, and not silent: the toggle still applies to this
            // session, so the message says what did and did not happen.
            OpenMsg::ViewerSaveFailed { detail } => {
                self.set_toast(
                    format!("viewer changed for this session only: {detail}"),
                    Severity::Warn,
                    now,
                );
                Response::redraw()
            }
        }
    }

    // --- timers -----------------------------------------------------------

    fn on_tick(&mut self, now: Instant) -> Response {
        let mut response = Response::none();

        if let Some(due) = self.verify_due_at
            && now >= due
        {
            self.verify_due_at = None;
            if self.input.chars().count() >= MIN_QUERY_LEN {
                self.phase = QueryPhase::Verifying { since: now };
                self.verify_watchdog_at = Some(now + VERIFY_WATCHDOG);
                response.merge(Response::redraw().with(Cmd::Verify {
                    query: self.input.text().to_string(),
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

        if self.wants_animation() && now >= self.last_frame + ANIMATION_TICK {
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

    fn set_toast(&mut self, text: String, severity: Severity, now: Instant) {
        self.toast = Some(Toast { text, severity });
        self.toast_expires_at = Some(now + TOAST_LIFETIME);
    }

    /// Age of the oldest served listing, for the status line.
    pub fn index_age(&self, now: SystemTime) -> Option<Duration> {
        self.index.age(now)
    }
}
