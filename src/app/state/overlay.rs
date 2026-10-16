//! The quick-search overlay and the rule for which queries are remembered.
//!
//! A child module of `state` so the gate below can read private fields such
//! as `remember_due_at`.

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
                // A warning, not an error: the shortcut still switches to the
                // compact layout.
                self.set_toast(reason, Severity::Warn, now);
                Response::redraw()
            }
        }
    }

    /// Switches to the compact overlay after the window was snapped.
    ///
    /// The current line is kept and selected, so typing replaces it while the
    /// arrows and Ctrl+C still reach it. Does not call `on_input_changed`,
    /// which would bump the query generation and re-arm the server check.
    fn enter_overlay(&mut self) -> Response {
        // A fresh summon starts with no history browsing and no drive picker.
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

    /// Requests that the overlay be dismissed, remembering the query first.
    ///
    /// The remember must run before `leave_history`: the gate refuses while
    /// browsing, and leaving history clears that state. The overlay flag is
    /// not changed here; the hotkey thread owns it and reports
    /// [`HotkeyMsg::Dismissed`].
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

    /// Whether the query on the line has settled enough to remember.
    ///
    /// Keeps prefixes typed on the way to a query out of history. A completed
    /// search is not enough: [`crate::config::SEARCH_DEBOUNCE`] is much shorter
    /// than [`crate::config::REMEMBER_DEBOUNCE`], so a pause mid-typing already
    /// reaches [`QueryPhase::Local`]. `Local` cannot be excluded either, since
    /// `on_verify` maps a skipped server check back to it.
    ///
    /// The signal is `remember_due_at`: armed on every keystroke that reaches a
    /// real query and cleared once `REMEMBER_DEBOUNCE` expires.
    pub(super) fn query_settled(&self) -> bool {
        // A recalled entry must not reorder the list it came from.
        if self.history.is_browsing() {
            return false;
        }
        if self.remember_due_at.is_some() {
            return false;
        }
        // Searches start at one character; history needs a longer floor so
        // short prefixes are not remembered.
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

    /// Remembers the query on the line if it has settled.
    ///
    /// Called from the quiet-period tick and on overlay dismissal.
    pub(super) fn remember_if_settled(&mut self) -> Option<Cmd> {
        if !self.query_settled() {
            return None;
        }
        self.remember_now()
    }

    /// Remembers the query on the line regardless of the debounce.
    ///
    /// Used on Enter, which is itself the settle signal. Still refuses while
    /// browsing history, so a recalled entry does not reorder the list.
    pub(super) fn remember_now(&mut self) -> Option<Cmd> {
        if self.history.is_browsing() {
            return None;
        }
        self.remember_due_at = None;
        let line = self.input.text().to_string();
        self.remember(&line)
    }
}
