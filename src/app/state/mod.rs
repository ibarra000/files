//! The interaction model as a pure state transition.
//!
//! [`AppState::update`] takes an event and the current time and returns what
//! to redraw and what to do. It performs no I/O, reads no clock, sends on no
//! channel and spawns no thread, so the debounce, staleness, selection and
//! error model can be unit-tested with a fake clock.
//!
//! # Timing
//!
//! There is no tick thread. The loop reads [`AppState::next_deadline`] and
//! synthesises [`AppEvent::Tick`] on expiry; when idle there is no deadline at
//! all. Time-derived text schedules its next deadline for the moment the
//! rendered string would change, using the wall clock passed to
//! [`AppState::note_frame`].

mod grid;
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
    /// Previous queries, and the recall position within them.
    pub history: History,
    /// Whether the panel is up.
    /// Whether the panel is up.
    ///
    /// Public so the shell can drive the entrance animation. Set only from a
    /// [`crate::app::event::HotkeyMsg`], since the thread that owns the window
    /// is the authority on whether it is on screen.
    pub overlay_up: bool,
    /// Whether the drive picker is open.
    /// Whether the drive picker is open. The text field keeps the keyboard;
    /// the picker borrows only the body.
    pub picking_share: bool,
    /// Where the keyboard is in the alias list, if it is in it at all.
    /// The keyboard position in the alias list, if it has been stepped onto.
    ///
    /// `None` while the list is shown but not entered. See
    /// [`Self::showing_aliases`].
    alias_cursor: Option<usize>,
    /// Whether the actions menu is up.
    /// Whether the actions menu is up.
    ///
    /// Kept here because Escape must close it. A flag rather than a cursor:
    /// each row is reached by its shortcut. See [`crate::view::actions`].
    pub actions_open: bool,
    pub hits: Vec<Hit>,
    pub matched: u32,
    pub total: u32,
    pub phase: QueryPhase,
    /// Progress of the live shares on this query, when any are configured.
    pub live: Option<LiveProgress>,
    pub empty_reason: Option<EmptyReason>,
    /// One status per configured mapping, indexed by `MappingId`.
    ///
    /// Private so [`Self::index`] cannot go stale: `on_index` is the only
    /// writer and always recomputes it.
    statuses: Vec<Arc<IndexStatus>>,
    /// The highlighted row in the drive picker.
    shares_cursor: usize,
    /// Summary of all statuses, recomputed on every status update.
    ///
    /// Cached so `next_deadline` and the drawn frame agree on which share is
    /// oldest and which is worst.
    pub index: IndexOverview,
    pub toast: Option<Toast>,
    pub should_quit: bool,
    /// Set once the user moves the selection and cleared when they type.
    /// While set, incoming results do not move the selection to the top.
    pub selection_pinned: bool,
    /// Tracked by path, not row, so a result update cannot change what Enter
    /// opens.
    pub selected_path: Option<Arc<str>>,
    /// Set when the results shifted under a pinned selection.
    pub selection_lost: bool,
    /// Which configured viewers could not be found, indexed like
    /// `settings.viewers`, so `view::status` can report them.
    pub viewers_missing: Box<[bool]>,
    /// The viewer Enter currently uses.
    ///
    /// Separate from `settings.viewer`, the startup value. `Settings` is cloned
    /// into the backend and workers, so the choice travels on each command
    /// instead of mutating shared settings.
    pub viewer: ViewerKind,
    /// What the last check of the update folder found.
    ///
    /// `None` until the first answer, which differs from "nothing found".
    pub update: Option<crate::update::Found>,
    /// Whether this process holds the global chord, as reported at startup.
    ///
    /// `None` until reported, and forever when the hotkey is off or
    /// unsupported. Recorded because `RegisterHotKey` is per-thread and cannot
    /// be re-queried from here; see [`crate::hotkey::Probe`].
    pub hotkey_claim: Option<Result<(), String>>,

    query_epoch: u64,
    /// When the line is due to be matched against the index.
    search_due_at: Option<Instant>,
    /// Enter was pressed while the match for the current line was pending.
    ///
    /// The rows on screen answer the previous line, so Enter dispatches the
    /// match at once and opens from its answer. Cleared by any edit, by the
    /// answer, and by the watchdog.
    enter_pending: bool,
    /// Backstop so a wedged matcher cannot leave Enter dead.
    enter_watchdog_at: Option<Instant>,
    verify_due_at: Option<Instant>,
    /// When the live shares may be queried.
    ///
    /// Separate from `verify_due_at` and slower, because a live answer costs
    /// the file server real work on every settled keystroke.
    live_due_at: Option<Instant>,
    /// When the line becomes worth remembering.
    ///
    /// Separate from `verify_due_at` and much longer, since a typing pause is
    /// too short to tell a finished query from a half-typed one.
    remember_due_at: Option<Instant>,
    verify_watchdog_at: Option<Instant>,
    toast_expires_at: Option<Instant>,
    last_frame: Instant,
    /// Wall clock at the last frame, used to schedule the next age-text
    /// change. Passed in by the renderer; the state reads no clock.
    last_frame_wall: SystemTime,
    /// The line, parsed.
    ///
    /// Cached where the text changes so every dispatch site uses one parse.
    /// The renderer uses its term for highlighting and `OpenRequest` to gather
    /// a page set.
    query: Query,
    /// The alias the line resolved to, if any, so the panel can show why it
    /// searched for something other than the typed text.
    expansion: Option<crate::alias::Alias>,
    last_verified_query: Option<Query>,
    /// The result row under the pointer, if any.
    ///
    /// Never the selection, which Enter opens. Cleared on any keystroke.
    hovered: Option<usize>,
    /// Scroll offset of the help panel, clamped when rendered.
    help_scroll: u16,
}

/// Whether a move that runs off the end comes back on at the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wrap {
    /// A step: Up from the first row is the last, and Down from the last is
    /// the first.
    Around,
    /// A page: the ends hold. See [`AppState::move_selection`].
    Stop,
}

impl AppState {
    pub fn new(settings: Settings, now: Instant) -> Self {
        let viewer = settings.viewer;
        // Sized over `all()`, not `enabled()`: `MappingId` is a position in
        // the full list including disabled entries.
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
            viewers_missing: Box::default(),
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
            // Placeholder until the first frame. Nothing reads it before then:
            // `next_text_change` needs a published index.
            last_frame_wall: SystemTime::UNIX_EPOCH,
            query: Query::default(),
            expansion: None,
            update: None,
            hotkey_claim: None,
            last_verified_query: None,
            help_scroll: 0,
            hovered: None,
        };

        // Report a config migration on the first frame, since the file the
        // user maintains was rewritten.
        if let Some(migrated) = state.settings.migrated.clone() {
            state.set_toast(migrated.detail(), Severity::Info, now);
        }
        state
    }

    /// The line, parsed. The renderer and page gathering use its term.
    pub fn query(&self) -> &Query {
        &self.query
    }

    /// The alias the line resolved to, if any. The renderer shows it so an
    /// expansion is never silent. See [`crate::alias`].
    pub fn expansion(&self) -> Option<&crate::alias::Alias> {
        self.expansion.as_ref()
    }

    pub fn query_epoch(&self) -> u64 {
        self.query_epoch
    }

    /// Installs history loaded from disk.
    pub fn seed_history(&mut self, entries: Vec<String>) {
        self.history = History::from_entries(entries);
    }

    /// Records which configured viewers a probe could not find.
    ///
    /// Separate from [`AppState::new`] because probing PATH is I/O. See
    /// [`crate::open::probe_viewers`].
    pub fn set_viewers_missing(&mut self, missing: Box<[bool]>) {
        self.viewers_missing = missing;
    }

    /// Whether the viewer `viewer` names is one a probe could not find.
    pub fn viewer_missing(&self, viewer: ViewerKind) -> bool {
        match viewer.within(&self.settings.viewers) {
            ViewerKind::App(i) => self.viewers_missing.get(i).copied().unwrap_or(false),
            _ => false,
        }
    }

    /// When the local match is due, if pending. Exposed for tests.
    pub fn search_due_at(&self) -> Option<Instant> {
        self.search_due_at
    }

    /// When the server-side check is due, if pending. Exposed for tests.
    pub fn verify_due_at(&self) -> Option<Instant> {
        self.verify_due_at
    }

    /// Scroll offset of the help panel.
    pub fn help_scroll(&self) -> u16 {
        self.help_scroll
    }

    /// The result row under the pointer.
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// Row index of the current selection.
    pub fn selected_row(&self) -> Option<usize> {
        let target = self.selected_path.as_ref()?;
        self.hits.iter().position(|h| &h.path == target)
    }

    pub fn selected_hit(&self) -> Option<&Hit> {
        self.selected_row().map(|i| &self.hits[i])
    }

    /// True while work this program started has not finished.
    ///
    /// Derived rather than stored, so it cannot report work that already
    /// ended. Does not request frames; workers wake the loop when they report.
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

    /// Describes a drive failure for display.
    ///
    /// In dev mode the drive is named by path and the error code is appended;
    /// otherwise it is named by its configured label.
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

    /// The technical detail of a message, or `None` outside dev mode.
    pub(crate) fn technical(&self, detail: impl Into<String>) -> Option<String> {
        self.settings.dev_mode.then(|| detail.into())
    }

    /// Whether an empty box is showing the configured aliases.
    ///
    /// History is never shown unasked, since it records what this person
    /// looked up; only Up opens it.
    pub fn showing_aliases(&self) -> bool {
        self.input.text().is_empty()
            && !self.picking_share
            && !self.showing_recent()
            && !self.settings.aliases.is_empty()
    }

    /// The alias the keyboard is on, if it has been stepped onto.
    pub fn alias_cursor(&self) -> Option<usize> {
        self.showing_aliases()
            .then_some(self.alias_cursor)
            .flatten()
    }

    /// Whether the status line has something to say unprompted.
    ///
    /// Lives beside the state so the panel can reserve room before asking for
    /// the text; `view::status::index_warning` supplies the words. Takes the
    /// renderer's wall clock, which is fresher than `last_frame_wall`.
    pub(crate) fn has_standing_notice(&self, wall: SystemTime) -> bool {
        self.index.origin.is_none()
            || self.index.degraded().is_some()
            || (self.settings.stale_notices && self.index.stale_at(wall).is_some())
    }

    /// True when on-screen text goes stale without an event: the per-share
    /// ages in the drive picker and the retry countdown.
    pub fn shows_elapsed_text(&self) -> bool {
        (self.picking_share && self.index.built_at.is_some()) || self.index.unreachable().is_some()
    }

    /// When the time-derived text next changes, if any is shown.
    ///
    /// Both terms are anchored on `last_frame`, so drawing pushes the deadline
    /// forward and `on_tick` cannot spin.
    fn next_text_change(&self) -> Option<Instant> {
        let mut out: Option<Instant> = None;
        let mut earliest = |at: Instant| {
            out = Some(out.map_or(at, |prev: Instant| prev.min(at)));
        };

        // The countdown needs no wall clock. Only while the retry is ahead:
        // `next_retry_at` is absolute, so once past it would stay due and the
        // loop would redraw continuously.
        if let Some((_, crate::index::Health::Unreachable { next_retry_at, .. })) =
            self.index.unreachable()
            && *next_retry_at > self.last_frame
        {
            earliest(*next_retry_at);
            earliest(self.last_frame + COUNTDOWN_TICK);
        }

        // Picker ages change at the next bucket edge. Only while the picker is
        // up; nothing else on screen derives from the wall clock.
        if self.picking_share
            && let Some(age) = self.index.age(self.last_frame_wall)
        {
            earliest(self.last_frame + crate::util::humanize::next_age_change(age));
        }
        out
    }

    /// Whether a right-click menu is open.
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
            // Keeps time-derived text current while otherwise idle.
            self.next_text_change(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Applies one event.
    pub fn update(&mut self, event: AppEvent, now: Instant) -> Response {
        self.dispatch_event(event, now)
    }

    fn dispatch_event(&mut self, event: AppEvent, now: Instant) -> Response {
        match event {
            AppEvent::Key(key) => self.on_key(key, now),
            AppEvent::Intent(intent) => self.on_intent(intent, now),
            AppEvent::Adopt(fresh) => self.on_adopt(*fresh, now),
            AppEvent::Update(msg) => self.on_update(msg, now),
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
                // The internal actor name goes in the detail, not the sentence.
                self.set_toast_detailed(
                    "A background task stopped \u{b7} results may be incomplete",
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
    // Key and pointer handling live in the `keys` and `pointer` child modules.

    fn on_paste(&mut self, text: &str, now: Instant) -> Response {
        // A stray control character such as a newline would be invisible yet
        // break the match.
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
                // Confirm the copy; silent success looks like silent failure.
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
                    self.set_toast("Clipboard has no text".into(), Severity::Info, now);
                    return Response::redraw();
                }
                self.leave_history();
                self.input.insert_str(&cleaned);
                self.on_input_changed(now, Urgency::Complete)
            }
            ClipboardMsg::Failed { detail } => {
                self.set_toast_detailed(
                    "Could not read the clipboard",
                    detail,
                    Severity::Warn,
                    now,
                );
                Response::redraw()
            }
        }
    }

    /// Stops browsing history, keeping the recalled entry.
    fn leave_history(&mut self) {
        if self.history.is_browsing() {
            self.history.accept();
        }
    }

    /// Records a query in history.
    ///
    /// Returns the write to perform if the list changed; the caller does I/O.
    fn remember(&mut self, code: &str) -> Option<Cmd> {
        if !self.settings.history || !self.history.record(code) {
            return None;
        }
        Some(Cmd::SaveHistory(Arc::new(self.history.snapshot())))
    }

    /// Empties the result list and everything derived from it.
    /// Only called once it is decided there is nothing to search; a query in
    /// flight keeps its results (see [`Self::on_input_changed`]).
    fn clear_results(&mut self) {
        self.hits.clear();
        self.hovered = None;
        self.matched = 0;
        self.total = 0;
        self.selected_path = None;
    }

    fn on_input_changed(&mut self, now: Instant, urgency: Urgency) -> Response {
        // Editing accepts any recalled entry. Centralised here so no key
        // handler has to remember it.
        self.leave_history();
        // Likewise leave the alias list.
        self.alias_cursor = None;
        self.query_epoch += 1;
        // Editing unpins the selection, but the selected path is kept so a
        // result set that still contains it keeps it selected.
        self.selection_pinned = false;
        self.selection_lost = false;
        self.verify_watchdog_at = None;
        self.last_verified_query = None;

        // Resolve aliases first. The typed line is never rewritten, which
        // would fight the caret and selection; an alias replaces only the
        // query.
        let expansion = self.settings.aliases.resolve(self.input.text()).cloned();
        self.query = Query::parse(match &expansion {
            Some(alias) => alias.code.as_ref(),
            None => self.input.text(),
        });
        // A resolved alias is complete, so it runs without the typing debounce.
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
        // Judged on the term, not the line: `ext:pdf` has nothing to search.
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

        // Results, selection and counts are kept until `apply_hits` replaces
        // them; `on_search` is epoch-guarded. Clearing here would draw an
        // empty body on every keystroke, since the matcher cannot answer
        // before the frame.
        self.phase = QueryPhase::LocalPending;
        self.empty_reason = Some(EmptyReason::NotSearchedYet);

        // Typed input waits out the debounce; pasted or composed input runs
        // immediately.
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
        // Armed even for `Urgency::Complete`: the server check is not free,
        // and showing local results first costs nothing perceptible.
        if !self.settings.routes.live().next().is_none() {
            self.live_due_at = Some(now + LIVE_DEBOUNCE);
        }
        self.remember_due_at = Some(now + REMEMBER_DEBOUNCE);

        response.redraw = Redraw::Yes;
        response
    }

    /// Retires every clock a live query owns.
    ///
    /// They must go together: a `Cmd::Search` or `Cmd::Live` left armed after
    /// the field is cleared would carry the current epoch, pass the guard in
    /// `on_search`, and show the previous results under an empty box.
    fn stand_down(&mut self) {
        self.search_due_at = None;
        self.verify_due_at = None;
        self.live_due_at = None;
        self.live = None;
        self.remember_due_at = None;
        self.enter_pending = false;
        self.enter_watchdog_at = None;
    }

    fn on_enter(&mut self, now: Instant) -> Response {
        // Enter takes the current row: a history entry or alias fills the
        // field and searches; a file opens.
        if self.history.is_browsing() {
            return self.accept_recall(now);
        }
        if let Some(rank) = self.alias_cursor() {
            return self.accept_alias(rank, now);
        }

        // If the match for the current line has not run, the rows answer the
        // previous line. Dispatch the match now and open from its answer;
        // see `on_search`.
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

    /// Opens the selected row with a given viewer, leaving the current
    /// viewer unchanged.
    ///
    /// Shared by `keys` and `pointer` so a key and its menu row are the same
    /// act.
    pub(super) fn open_with(&mut self, viewer: ViewerKind, now: Instant) -> Response {
        self.open_selection(viewer, now)
    }

    /// Opens the selected row with the given viewer.
    ///
    /// Also re-entered from `on_search` for a deferred Enter.
    fn open_selection(&mut self, viewer: ViewerKind, now: Instant) -> Response {
        // Never blocks on the network; a missing file is reported afterwards.
        let Some(hit) = self.selected_hit().or_else(|| self.hits.first()) else {
            if !self.input.is_empty() {
                // A query that found nothing is worth recalling. Gated on the
                // debounce, since without a hit only a typing pause shows the
                // query was finished.
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

        // The term, not the line: `pages::collect` matches by equality, so
        // `11-D-0704 ext:pdf` would find no pages.
        let code = self.query.term().to_string();
        // Computed before the path moves into the request.
        let viewers = Arc::clone(&self.settings.viewers);
        let route = crate::open::route_of(viewer, &path, &viewers);
        let request = crate::open::OpenRequest {
            path,
            // The typed term, not the row: `hits` is capped at MAX_RESULTS and
            // ranked by match position, so the page set is rebuilt from it.
            query: code.clone(),
            viewer,
            viewers,
        };
        let mut response = Response::none().with(Cmd::Open(request));

        // Assembling a document reads every page from the share and is not
        // instant, so announce it. Other routes return at once.
        if route == crate::open::Route::Document {
            self.set_toast("Opening\u{2026}".into(), Severity::Info, now);
            response.redraw = Redraw::Yes;
        }

        // Ungated: opening a file shows the query was meant. Also clears the
        // pending remember deadline.
        if let Some(cmd) = self.remember_now() {
            response = response.with(cmd);
        }

        // Messages the open must report after the panel hides arrive as a
        // message box from `on_open`; see `crate::notify`.
        // `request_dismiss` re-runs the remember gate and finds this entry
        // already at the head, so nothing is written twice.
        if self.overlay_up {
            if self.settings.hide_after_opening {
                response.merge(self.request_dismiss());
            } else {
                // Staying up: select the line as a summon does, so the next
                // keystroke replaces it. Only for a summoned overlay.
                self.input.select_all();
            }
        }
        response
    }

    /// How far a page key moves, in ranks.
    ///
    /// One screenful for the current layout, times the column count so a page
    /// keeps its column. Read from settings so changes apply on the next press.
    fn rows_per_page(&self) -> isize {
        (self.settings.result_layout.rows_per_page() * self.columns()) as isize
    }

    /// Results per line, clamped to the range the list can draw.
    pub fn columns(&self) -> usize {
        self.settings.columns.clamp(1, crate::config::MAX_COLUMNS)
    }

    /// Moves one line up or down within the same column, wrapping within it.
    /// See [`grid::vertical`].
    fn move_vertically(&mut self, down: bool) -> Response {
        let Some(current) = self.selected_row() else {
            return self.move_selection(if down { 1 } else { -1 }, Wrap::Around);
        };
        let target = grid::vertical(current, self.hits.len(), self.columns(), down);
        self.move_selection(target as isize - current as isize, Wrap::Stop)
    }

    /// Moves to the next or previous cell on the same line, stopping at its
    /// ends.
    fn move_across(&mut self, forward: bool) -> Response {
        let Some(current) = self.selected_row() else {
            return Response::none();
        };
        match grid::across(current, self.hits.len(), self.columns(), forward) {
            Some(target) => self.jump_selection(target),
            // At the end of the line: pin, as a step at the end of the list does.
            None => self.move_selection(0, Wrap::Stop),
        }
    }

    /// Moves the selection by `delta` ranks.
    ///
    /// All relative moves go through here so list ends behave consistently. A
    /// step wraps; a page holds at the ends, since a wrapped page would be
    /// indistinguishable from one that had not moved.
    fn move_selection(&mut self, delta: isize, wrap: Wrap) -> Response {
        if self.hits.is_empty() {
            return Response::none();
        }
        let len = self.hits.len() as isize;
        let Some(current) = self.selected_row().map(|i| i as isize) else {
            // Unreachable while there are hits (`apply_hits` selects one).
            // Enter from the near end.
            let row = if delta > 0 { 0 } else { self.hits.len() - 1 };
            return self.jump_selection(row);
        };
        let target = match wrap {
            // `rem_euclid`, since `%` keeps the sign of the left operand.
            Wrap::Around => (current + delta).rem_euclid(len),
            Wrap::Stop => (current + delta).clamp(0, len - 1),
        };
        if target == current {
            // At the edge: pin the selection, but the frame would be unchanged,
            // so request no redraw.
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
                // Never overwrite a server phase; both can fall due on the
                // same tick.
                if !self.phase.server_has_spoken() {
                    self.phase = QueryPhase::Local;
                }
                self.empty_reason = if self.hits.is_empty() {
                    Some(self.no_match_reason(outcome.total))
                } else {
                    None
                };
                // The current line's answer has arrived, so cancel any pending
                // match. `tests/panel.rs` (`toasted`) relies on this so Enter
                // reports "nothing to open" rather than deferring.
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

    /// Folds one live share's answer into the list already on screen.
    ///
    /// Merges rather than replacing via `apply_hits`, which would empty and
    /// refill the list a second after typing stopped.
    fn on_live(&mut self, msg: super::event::LiveMsg, _now: Instant) -> Response {
        if msg.epoch != self.query_epoch {
            return Response::none(); // superseded
        }
        // Computed before `self.live` is borrowed, since it reads settings.
        let failure = match &*msg.outcome {
            crate::search::live::LiveOutcome::Failed(err) => {
                Some(self.describe_drive_error(msg.mapping, *err))
            }
            _ => None,
        };

        let Some(progress) = self.live.as_mut() else {
            // The field was cleared while this was in flight.
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
        // An unreachable share can turn "no matches" into "not searched", so
        // recompute the reason even when the list did not change.
        if self.hits.is_empty() {
            self.empty_reason = Some(self.no_match_reason(self.total));
        }
        let _ = changed;
        Response::redraw()
    }

    /// Adds rows to the list already on screen, keeping it ranked.
    ///
    /// The union is re-ranked rather than appended, so the source share does
    /// not decide a row's position (as in `matcher::merge_all`).
    fn merge_hits(&mut self, extra: Vec<Hit>) -> bool {
        if extra.is_empty() {
            return false;
        }
        let mut merged = std::mem::take(&mut self.hits);
        merged.extend(extra);
        // The matcher's ranking key, then path, so equal matches from two
        // shares order deterministically.
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
        // `place_selection` tracks the selection by path, so rows arriving
        // above it do not change the selected file.
        self.apply_hits(merged);
        true
    }

    /// Chooses the explanation for an empty list.
    fn no_match_reason(&self, searched: u32) -> EmptyReason {
        // No live share has answered yet, so there is nothing to report but a
        // wait. Otherwise an empty local sweep would read "no matches" before
        // the live query even goes out.
        if self.settings.routes.live().next().is_some()
            && self.live.as_ref().is_none_or(|l| l.is_asking())
        {
            return EmptyReason::NotSearchedYet;
        }

        // An incomplete live search outranks other explanations: "nothing
        // matched" would be a claim about folders nobody looked in.
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
    /// Only enabled, indexed shares with a path.
    pub fn share_ids(&self) -> Vec<MappingId> {
        self.settings
            .routes
            .enabled()
            .filter(|m| m.kind.is_indexed() && !m.path.as_os_str().is_empty())
            .map(|m| m.id)
            .collect()
    }

    /// One share as the drive picker shows it.
    ///
    /// Assembled here so the picker and the overview agree on which share is
    /// stale.
    pub fn share_row(&self, id: MappingId) -> Option<crate::view::shares::Row<'_>> {
        let mapping = self.settings.routes.enabled().find(|m| m.id == id)?;
        let status = self.status_of(id)?;
        Some(crate::view::shares::Row {
            mapping,
            status,
            // The store's own note (events lost, live updates off) outranks
            // age, since it says the index is wrong rather than old.
            stale: status.stale.or_else(|| {
                self.index
                    .stalest
                    .filter(|(stale_id, _)| *stale_id == id)
                    .map(|(_, why)| why)
            }),
        })
    }

    /// One status per mapping, for detail the overview does not carry.
    pub fn status_of(&self, id: MappingId) -> Option<&Arc<IndexStatus>> {
        self.statuses.get(id.index())
    }

    /// Installs a new result set, keeping the selection where the user put it.
    /// The scroll area owns its own offset.
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

        // The hover is a rank, and ranks do not survive replacement. Keep it
        // only when the same file is still at that rank; never clamp, which
        // would invent a hover the pointer is not over. Unlike the selection,
        // which is tracked by path because Enter opens it.
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

        // Either unpinned, or pinned with nothing to fall back to: select the
        // top of the new list.
        self.selected_path = self.hits.first().map(|h| Arc::clone(&h.path));
    }

    // --- verification -----------------------------------------------------

    fn on_verify(&mut self, msg: super::event::VerifyMsg, now: Instant) -> Response {
        if msg.epoch != self.query_epoch {
            return Response::none();
        }
        self.verify_watchdog_at = None;
        self.last_verified_query = Some(msg.query);

        // History is not recorded here; remembering has its own deadline. See
        // `remember_due_at` and `on_tick`.
        match msg.outcome {
            VerifyOutcome::IndexAuthoritative { .. } => {
                // The directory is unchanged, so the screen is already
                // correct. No query was issued.
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
                // Replace rather than union, so deleted files do not linger.
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
                // Verification runs only when there is exactly one flat share
                // (see `worker::run_verify`), so this failure belongs to it.
                let said = match self.settings.routes.flat().next().map(|m| m.id) {
                    Some(id) => self.describe_drive_error(id, err),
                    // The UI says "drive" where the code says "share".
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
                // An actor with a routing table this state lacks is a bug, but
                // not worth a panic.
                let Some(slot) = self.statuses.get_mut(id.index()) else {
                    return Response::none();
                };
                *slot = status;
                self.index = IndexOverview::of(&self.settings.routes, &self.statuses);
                Response::redraw()
            }
            IndexMsg::SnapshotChanged => {
                // Re-run the local match so the display agrees with the index.
                // Must not re-arm the verify debounce, which would livelock.
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
                    // Named by path in dev mode, since the user must find the
                    // missing drive letter.
                    Some(err) => {
                        format!(
                            "Refresh failed \u{b7} {}",
                            self.describe_drive_error(id, err)
                        )
                    }
                    // By name: a success report needs no path.
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

    /// The window gained or lost keyboard focus.
    ///
    /// Losing it dismisses a summoned panel when `hide_on_blur` is set. Gaining
    /// it is ignored; [`HotkeyMsg::Summoned`] reports a summon. The shell drops
    /// the event while the settings window has focus.
    fn on_focus(&mut self, has_focus: bool) -> Response {
        if has_focus || !self.overlay_up || !self.settings.hide_on_blur {
            return Response::none();
        }
        self.request_dismiss()
    }

    fn on_open(&mut self, msg: OpenMsg, now: Instant) -> Response {
        match msg {
            // A complete document says nothing; a document missing pages must
            // say so, since nothing on screen would reveal it.
            OpenMsg::Launched {
                pages,
                skipped,
                truncated,
                ..
            } => {
                if skipped.is_empty() && !truncated {
                    // Clear the "Opening..." notice now that the viewer is up.
                    self.toast = None;
                    self.toast_expires_at = None;
                    return Response::redraw();
                }
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
                self.also_say("Some pages were not opened", text)
            }
            OpenMsg::Failed { path, detail } => {
                let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
                let title = format!("Could not open {name}");
                self.set_toast_detailed(title.clone(), detail.clone(), Severity::Error, now);
                self.also_say(&title, detail)
            }
            OpenMsg::ViewerSaved { name } => {
                // `display`, not the written config spelling.
                let viewers = &self.settings.viewers;
                let said =
                    ViewerKind::parse(&name, viewers).map_or(name.as_ref(), |v| v.display(viewers));
                self.set_toast(format!("Viewer: {said}"), Severity::Info, now);
                Response::redraw()
            }
            // Already applied for this session before the write was requested.
            OpenMsg::SettingSaveFailed { label, detail } => {
                self.set_toast_detailed(
                    format!("{label} changed for this session only"),
                    detail,
                    Severity::Warn,
                    now,
                );
                Response::redraw()
            }
            // The toggle still applies to this session.
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

    /// Raises a message box as well as the toast when the panel is not up.
    ///
    /// Opens report on another thread, often after `hide_after_opening` has
    /// hidden the panel, and missing pages must not go unnoticed. If the panel
    /// was summoned again meanwhile, the toast is enough.
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

        // The local match first, so when both fall due on one tick the phase
        // ends where `on_verify` expects.
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

        // A matcher that never answered an Enter: release the keystroke.
        if let Some(due) = self.enter_watchdog_at
            && now >= due
        {
            self.enter_watchdog_at = None;
            self.enter_pending = false;
            self.set_toast("Search did not answer".into(), Severity::Warn, now);
            response.merge(Response::redraw());
        }

        if let Some(due) = self.live_due_at
            && now >= due
        {
            self.live_due_at = None;
            // Also enforce the server's minimum length, so a single character
            // does not query every live share.
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

        // Typing has paused long enough that the line is a finished query
        // rather than a prefix.
        if let Some(due) = self.remember_due_at
            && now >= due
        {
            self.remember_due_at = None;
            if let Some(cmd) = self.remember_if_settled() {
                response.merge(Response::redraw().with(cmd));
            }
        }

        // Independent of the worker, so a wedged or panicked verify cannot
        // leave the spinner up.
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

        // Required counterpart of `next_text_change` in `next_deadline`: a
        // deadline without a redraw would wake the loop repeatedly.
        if let Some(due) = self.next_text_change()
            && now >= due
        {
            response.merge(Response::redraw());
        }

        response
    }

    /// Records the update checker's report.
    ///
    /// Toasts once per new version rather than on every periodic check.
    fn on_update(&mut self, msg: crate::app::event::UpdateMsg, now: Instant) -> Response {
        let crate::app::event::UpdateMsg::Looked(found) = msg;

        if let crate::update::Found::Available { manifest, .. } = found.as_ref()
            && offered_version(self.update.as_ref()) != Some(manifest.version)
        {
            self.set_toast(
                format!(
                    "Version {} is available \u{b7} install it from Settings",
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
    /// `statuses` is indexed by `MappingId`, a list position, so it must match
    /// the list length. Existing entries are kept.
    pub(super) fn resize_statuses(&mut self) {
        let wanted = self.settings.routes.all().len();
        self.statuses.truncate(wanted);
        while self.statuses.len() < wanted {
            self.statuses.push(Arc::new(IndexStatus::default()));
        }
    }

    /// Raises a toast whose detail is shown only in dev mode, joined with ` · `.
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

    /// Age of the oldest served listing.
    pub fn index_age(&self, now: SystemTime) -> Option<Duration> {
        self.index.age(now)
    }
}

/// The version a previous check offered, if any.
///
/// A free function so the caller can use it while borrowing the field.
fn offered_version(found: Option<&crate::update::Found>) -> Option<crate::update::Version> {
    match found? {
        crate::update::Found::Available { manifest, .. } => Some(manifest.version),
        _ => None,
    }
}
