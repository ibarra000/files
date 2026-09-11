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
//! blocks until then, synthesising [`AppEvent::Tick`] on expiry. Idle means a
//! blocking receive with no deadline at all - zero wakeups, zero CPU, and no
//! added latency, since a keystroke wakes the thread immediately.

mod keys;
mod model;
mod mouse;

pub use model::{EmptyReason, Focus, QueryPhase, Severity, TOAST_LIFETIME, Toast};

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use ratatui::layout::Rect;

use super::event::{AppEvent, ClipboardMsg, Cmd, IndexMsg, OpenMsg, PrefetchMsg, Redraw, Response};
use super::input::{self, Input};
use crate::config::{
    ANIMATION_TICK, MIN_QUERY_LEN, PREFETCH_DEBOUNCE, Settings, VERIFY_DEBOUNCE, VERIFY_WATCHDOG,
    ViewerKind,
};
use crate::history::History;
use crate::index::errors::EnumError;
use crate::index::store::FlatStatus;
use crate::paths::MappingKind;
use crate::search::matcher::{Hit, QueryReject};
use crate::search::verify::{AuditVerdict, SkipReason, VerifyOutcome};

/// The last mouse press, for working out double- and triple-clicks.
///
/// The terminal reports presses; a "click count" is this program's own idea,
/// so the previous press has to be remembered to recognise the next one.
#[derive(Debug, Clone, Copy)]
struct Click {
    at: Instant,
    column: u16,
    row: u16,
    count: u8,
}

/// Everything rendered, and everything that decides what to do next.
pub struct AppState {
    pub settings: Settings,
    pub input: Input,
    /// Codes used before, and where recall currently is within them.
    pub history: History,
    pub focus: Focus,
    /// Last known terminal size.
    ///
    /// Only mouse handling needs it - a click arrives in screen coordinates
    /// and has to be turned back into a caret position or a result row - and
    /// it is fed through [`ui::layout`](crate::ui::layout), the same function
    /// that drew the frame, so the two cannot disagree about where anything
    /// is.
    pub area: Rect,
    pub hits: Vec<Hit>,
    pub matched: u32,
    pub total: u32,
    pub phase: QueryPhase,
    pub empty_reason: Option<EmptyReason>,
    pub index: Arc<FlatStatus>,
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
    prefetch_due_at: Option<Instant>,
    verify_watchdog_at: Option<Instant>,
    toast_expires_at: Option<Instant>,
    last_frame: Instant,
    last_verified_query: Option<String>,
    last_click: Option<Click>,
    /// Whether the left button is down after a press inside the search box.
    /// Without it, dragging over the results would extend a text selection.
    dragging: bool,
}

impl AppState {
    pub fn new(settings: Settings, now: Instant) -> Self {
        let viewer = settings.viewer;
        Self {
            settings,
            input: Input::new(),
            history: History::new(),
            focus: Focus::Input,
            // Replaced by the real size before the first frame; a sane default
            // means mouse arithmetic is never done against a zero rect.
            area: Rect::new(0, 0, 80, 24),
            hits: Vec::new(),
            matched: 0,
            total: 0,
            phase: QueryPhase::Idle,
            empty_reason: Some(EmptyReason::NoQuery),
            index: Arc::new(FlatStatus::default()),
            toast: None,
            should_quit: false,
            selection_pinned: false,
            selected_path: None,
            selection_lost: false,
            viewer,
            query_epoch: 0,
            verify_due_at: None,
            prefetch_due_at: None,
            verify_watchdog_at: None,
            toast_expires_at: None,
            last_frame: now,
            last_verified_query: None,
            last_click: None,
            dragging: false,
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

    pub fn set_area(&mut self, area: Rect) {
        self.area = area;
    }

    /// Where everything is on screen, for hit-testing a mouse event.
    pub fn chunks(&self) -> crate::ui::Chunks {
        crate::ui::layout(self.area)
    }

    /// When the authoritative server-side check is due, if one is pending.
    ///
    /// Exposed so the debounce can be asserted directly rather than by
    /// poking at private state.
    pub fn verify_due_at(&self) -> Option<Instant> {
        self.verify_due_at
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
        self.phase.is_verifying() || self.index.activity.is_busy()
    }

    pub fn note_frame(&mut self, now: Instant) {
        self.last_frame = now;
    }

    /// The earliest pending deadline, or `None` when the loop can block
    /// indefinitely.
    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.verify_due_at,
            self.prefetch_due_at,
            self.verify_watchdog_at,
            self.toast_expires_at,
            self.wants_animation()
                .then(|| self.last_frame + ANIMATION_TICK),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// The one state transition.
    pub fn update(&mut self, event: AppEvent, now: Instant) -> Response {
        match event {
            AppEvent::Key(key) => self.on_key(key, now),
            AppEvent::Mouse(event) => self.on_mouse(event, now),
            AppEvent::Paste(text) => self.on_paste(&text, now),
            AppEvent::Resize { cols, rows } => {
                self.area = Rect::new(0, 0, cols, rows);
                Response::redraw()
            }
            AppEvent::Tick => self.on_tick(now),
            AppEvent::Search(msg) => self.on_search(msg),
            AppEvent::Verify(msg) => self.on_verify(msg, now),
            AppEvent::Index(msg) => self.on_index(msg, now),
            AppEvent::Prefetch(msg) => self.on_prefetch(msg),
            AppEvent::Open(msg) => self.on_open(msg, now),
            AppEvent::Clipboard(msg) => self.on_clipboard(msg, now),
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
        if self.focus == Focus::History {
            self.history.accept();
        }
        self.focus = Focus::Input;
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

    fn on_input_changed(&mut self, now: Instant) -> Response {
        self.query_epoch += 1;
        // Editing the code is always a return to editing it: the results are
        // about to be replaced, so a selection in a list of things that no
        // longer exist would be meaningless.
        self.focus = Focus::Input;
        self.selection_pinned = false;
        self.selection_lost = false;
        self.hits.clear();
        self.matched = 0;
        self.total = 0;
        self.verify_watchdog_at = None;
        self.last_verified_query = None;

        let chars = self.input.chars().count();
        if chars == 0 {
            self.phase = QueryPhase::Idle;
            self.empty_reason = Some(EmptyReason::NoQuery);
            self.verify_due_at = None;
            self.prefetch_due_at = None;
            self.selected_path = None;
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
            self.prefetch_due_at = None;
            self.selected_path = None;
            return Response::redraw();
        }

        let targets = self.settings.routes.classify(&self.input);
        if targets.is_empty() {
            self.phase = QueryPhase::Unresolvable;
            self.empty_reason = Some(EmptyReason::NoPathPattern);
            self.verify_due_at = None;
            self.prefetch_due_at = None;
            self.selected_path = None;
            return Response::redraw();
        }

        self.phase = QueryPhase::LocalPending;
        self.empty_reason = Some(EmptyReason::NotSearchedYet);
        self.selected_path = None;

        // The local match is sub-millisecond, so it runs on every keystroke
        // with no debounce at all. Only the network-bound work is delayed.
        let mut response = Response::redraw().with(Cmd::Search {
            query: self.input.text().to_string(),
            epoch: self.query_epoch,
        });

        self.verify_due_at = Some(now + VERIFY_DEBOUNCE);

        // A resolvable job folder can be fetched speculatively. Prefetch is
        // cheap and a wrong guess is harmless, so it fires sooner than the
        // verify. Flat mappings are never speculated on: they are served from
        // the in-memory index and there is nothing to warm.
        self.prefetch_due_at = targets
            .iter()
            .any(|t| t.kind == MappingKind::JobFolder)
            .then(|| now + PREFETCH_DEBOUNCE);

        response.redraw = Redraw::Yes;
        response
    }

    fn on_enter(&mut self, now: Instant) -> Response {
        // Enter always opens. Never blocks on the network: if the file turns
        // out to be gone, that is reported afterwards.
        let Some(hit) = self.selected_hit().or_else(|| self.hits.first()) else {
            if !self.input.is_empty() {
                self.set_toast("nothing to open".into(), Severity::Info, now);
                return Response::redraw();
            }
            return Response::none();
        };
        let path = Arc::clone(&hit.path);

        // Opening a file is the strongest possible signal that this code was
        // the one meant, so it is remembered here as well as on verification -
        // someone who opens a result before the server answers must not lose
        // the code.
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

        if let Some(cmd) = self.remember(&code) {
            response = response.with(cmd);
        }
        response
    }

    fn move_selection(&mut self, delta: isize) -> Response {
        if self.hits.is_empty() {
            return Response::none();
        }
        let current = self.selected_row().map(|i| i as isize).unwrap_or(-1);
        let len = self.hits.len() as isize;
        let next = if current < 0 {
            if delta > 0 { 0 } else { len - 1 }
        } else {
            (current + delta).rem_euclid(len)
        };
        self.selection_pinned = true;
        self.selection_lost = false;
        self.selected_path = Some(Arc::clone(&self.hits[next as usize].path));
        Response::redraw()
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
        match &self.index.health {
            crate::index::Health::Unreachable { err, .. } if searched == 0 => {
                EmptyReason::IndexUnavailable {
                    detail: err.describe(&self.flat_label()),
                }
            }
            _ => EmptyReason::NoMatches { searched },
        }
    }

    fn flat_label(&self) -> String {
        self.settings.custpro_path.to_string_lossy().into_owned()
    }

    /// Installs a new result set while keeping the cursor where the user put
    /// it.
    fn apply_hits(&mut self, hits: Vec<Hit>) {
        let previous_row = self.selected_row();
        self.hits = hits;

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
        let query = msg.query.clone();
        self.last_verified_query = Some(msg.query);

        // Recorded on verification rather than on every keystroke, and that
        // single choice is the whole filter: verification only ever runs for a
        // code that survived the debounce, cleared the minimum length and
        // resolved to a share, so the half-typed prefixes on the way to it
        // never reach the list. A code that found nothing is still recorded -
        // those are precisely the ones worth recalling and correcting.
        let remembered = self.remember(&query);

        let response = match msg.outcome {
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
                self.phase = QueryPhase::VerifyFailed {
                    detail: err.describe(&self.flat_label()),
                };
                Response::redraw()
            }
        };

        match remembered {
            Some(cmd) => response.with(cmd),
            None => response,
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
            IndexMsg::Status(status) => {
                self.index = status;
                Response::redraw()
            }
            IndexMsg::SnapshotChanged => {
                // Re-run the local match so the display agrees with the
                // index. Deliberately does NOT re-arm the verify debounce -
                // that cycle would be a livelock.
                if self.input.chars().count() >= MIN_QUERY_LEN
                    && self.settings.routes.resolves(&self.input)
                {
                    return Response::redraw().with(Cmd::Search {
                        query: self.input.text().to_string(),
                        epoch: self.query_epoch,
                    });
                }
                Response::redraw()
            }
            IndexMsg::RefreshReport {
                entries,
                elapsed,
                error,
            } => {
                let text = match error {
                    Some(err) => format!("refresh failed: {}", err.describe(&self.flat_label())),
                    None => format!(
                        "refreshed {} files in {}",
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

    fn on_prefetch(&mut self, msg: PrefetchMsg) -> Response {
        match msg {
            // A warmed cache changes nothing on screen until the next search
            // uses it.
            PrefetchMsg::Ready { .. } => Response::none(),
            PrefetchMsg::Failed { dir, err } => {
                if self.hits.is_empty() && self.phase == QueryPhase::Local {
                    self.empty_reason = Some(match err {
                        EnumError::PathNotFound(_) => EmptyReason::PathNotFound { dir },
                        EnumError::AccessDenied(_) => EmptyReason::AccessDenied { dir },
                        other => EmptyReason::IndexUnavailable {
                            detail: other.describe(&dir.to_string_lossy()),
                        },
                    });
                    return Response::redraw();
                }
                Response::none()
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

        if let Some(due) = self.prefetch_due_at
            && now >= due
        {
            self.prefetch_due_at = None;
            for target in self.settings.routes.classify(&self.input) {
                if target.kind != MappingKind::JobFolder {
                    continue;
                }
                response.merge(Response::none().with(Cmd::Prefetch {
                    dir: target.dir,
                    epoch: self.query_epoch,
                }));
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

        response
    }

    fn set_toast(&mut self, text: String, severity: Severity, now: Instant) {
        self.toast = Some(Toast { text, severity });
        self.toast_expires_at = Some(now + TOAST_LIFETIME);
    }

    /// Age of the served listing, for the status line.
    pub fn index_age(&self, now: SystemTime) -> Option<Duration> {
        self.index.age(now)
    }
}
