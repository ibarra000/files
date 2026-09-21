//! The quick-search overlay, and the rule about what gets remembered.
//!
//! A child module rather than a free-standing one so it can still reach the
//! private fields the gate below depends on - `verify_due_at` in particular -
//! exactly as `mouse` reaches `dragging` and `last_click`.

use std::time::Instant;

use super::model::{QueryPhase, Severity};
use super::{AppState, Cmd, Redraw, Response};
use crate::app::event::HotkeyMsg;

impl AppState {
    /// The hotkey thread reporting what it did.
    pub(super) fn on_hotkey(&mut self, msg: HotkeyMsg, now: Instant) -> Response {
        match msg {
            HotkeyMsg::Summoned => self.enter_overlay(),
            HotkeyMsg::Dismissed => self.leave_overlay(),
            HotkeyMsg::Claimed => {
                self.hotkey_claim = Some(Ok(()));
                Response::none()
            }
            HotkeyMsg::Unavailable { reason } => {
                self.hotkey_claim = Some(Err(reason.clone()));
                // Said once, by the thread, and shown as a warning rather than
                // an error: the shortcut still switches to the compact layout,
                // so this is a reduced feature and not a broken one.
                self.set_toast(reason, Severity::Warn, now);
                Response::redraw()
            }
        }
    }

    /// The window has been snapped: shrink to match it.
    ///
    /// The code already on the line is kept, and selected. Clearing would throw
    /// away both a code somebody may still want and the results already
    /// computed for it; keeping it unselected would mean the next keystroke
    /// appended to a stale code. Selected is both at once - the first character
    /// typed replaces it, while the arrows and Ctrl+C still recover it.
    ///
    /// Deliberately does *not* call `on_input_changed`. Summoning the window is
    /// not editing the code: doing so would bump the query generation, re-arm
    /// the server check and spend a round trip on a code nobody retyped.
    fn enter_overlay(&mut self) -> Response {
        // Whatever was being browsed last time is not what this summon is
        // about, and the drive picker least of all: it must not be the first
        // thing somebody sees when they ask for a search box.
        self.leave_history();
        self.picking_share = false;
        self.input.select_all();
        self.overlay_up = true;
        Response::redraw()
    }

    fn leave_overlay(&mut self) -> Response {
        self.overlay_up = false;
        self.picking_share = false;
        self.leave_history();
        self.input.clear_selection();
        Response::redraw()
    }

    /// Asks for the overlay to be put away, and remembers the code first.
    ///
    /// The order matters. The gate refuses while recall is open, and
    /// `leave_history` destroys that fact, so the commit has to happen before
    /// the mode is unwound - otherwise browsing previous codes and then
    /// dismissing would silently reorder the list being browsed.
    ///
    /// `mode` is *not* changed here. The hotkey thread owns whether the window
    /// is small and reports back as [`HotkeyMsg::Dismissed`]; one thread hop of
    /// latency buys the impossibility of the two disagreeing.
    pub(super) fn request_dismiss(&mut self) -> Response {
        let mut response = Response {
            redraw: Redraw::Yes,
            cmds: Default::default(),
        };
        response = response.with(Cmd::DismissOverlay);
        if let Some(cmd) = self.remember_if_settled() {
            response = response.with(cmd);
        }
        response
    }

    /// Whether the code on the line has settled enough to be worth keeping.
    ///
    /// This is the filter that keeps the prefixes typed on the way to a code
    /// out of the recall list, and it is deliberately **not** "a search came
    /// back". The local match waits out [`crate::config::SEARCH_DEBOUNCE`],
    /// which is 300ms - an ordinary pause between syllables - so `inv` reaches
    /// [`QueryPhase::Local`] while somebody is still reading the rest of the
    /// code off a drawing. Gating on a resolved search would remember every
    /// prefix, which is precisely the bug this exists to prevent. That clock is
    /// five times shorter than [`crate::config::REMEMBER_DEBOUNCE`], and the
    /// gap between them is the whole of what separates a prefix from a code.
    ///
    /// Nor can `Local` simply be excluded. `run_verify` skips the server
    /// entirely when there is no single flat share to ask - the shipped
    /// configuration has two shares, so that is the ordinary case - and
    /// `on_verify` maps that skip back to `Local`. A fully settled code ends up
    /// there too, so excluding it would break the common path rather than the
    /// rare one.
    ///
    /// What actually separates a finished code from a half-typed one is that
    /// the typing stopped, and `remember_due_at` is exactly that value: armed
    /// on every keystroke that reaches a real query, cleared only when the tick
    /// finds [`crate::config::REMEMBER_DEBOUNCE`] expired. An empty deadline
    /// means the quiet period has passed with nothing typed since.
    ///
    /// It used to be `verify_due_at`, which is the same shape with a clock five
    /// times shorter - short enough that a pause in the middle of a code read
    /// off a drawing counted as the end of one.
    pub(super) fn query_settled(&self) -> bool {
        // Browsing is looking, not searching. The code on the line was put
        // there by an arrow key, and recalling a code must not rewrite the list
        // it was recalled from.
        if self.history.is_browsing() {
            return false;
        }
        if self.remember_due_at.is_some() {
            return false;
        }
        // Long enough to be worth recalling tomorrow. This used to be
        // enforced by accident: a query under three characters was rejected
        // outright and sat in a phase the match below does not accept, so
        // the floor on the history was a side effect of the floor on the
        // search. The search floor is one now, and without this line the
        // recent-codes list would fill with every one- and two-character
        // prefix anybody typed on the way to a real code.
        if self.query.term().chars().count() < crate::config::MIN_REMEMBERED_LEN {
            return false;
        }
        matches!(
            self.phase,
            QueryPhase::Local
                | QueryPhase::Verifying { .. }
                | QueryPhase::Verified { .. }
                | QueryPhase::VerifyFailed { .. }
        )
    }

    /// Remembers the code on the line, if it has settled.
    ///
    /// The clock-gated commit: the quiet-period tick, and the overlay being
    /// dismissed.
    pub(super) fn remember_if_settled(&mut self) -> Option<Cmd> {
        if !self.query_settled() {
            return None;
        }
        self.remember_now()
    }

    /// Remembers the code on the line whatever the clock says.
    ///
    /// Pressing Enter *is* the settle signal. Somebody who types a code and
    /// opens a file from it half a second later has said what they meant far
    /// more emphatically than a timer ever could, and making them wait out a
    /// debounce first would lose exactly the codes that matter most.
    ///
    /// The browsing guard stays: recalling a code must not reorder the list it
    /// was recalled from, and that is true however deliberate the keystroke.
    pub(super) fn remember_now(&mut self) -> Option<Cmd> {
        if self.history.is_browsing() {
            return None;
        }
        self.remember_due_at = None;
        let code = self.input.text().to_string();
        self.remember(&code)
    }
}
