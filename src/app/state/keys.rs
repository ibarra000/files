//! What each key does.
//!
//! A child module of `state` rather than a sibling, so it can still reach the
//! private fields of the state it drives while they stay private to the rest
//! of the crate.
//!
//! # There is no key that quits
//!
//! Ctrl+C copies, because that is what Ctrl+C means everywhere else, and Esc
//! clears the line and stops there. The program is closed the way every other
//! windowed program is closed. That has one consequence worth knowing about:
//! there is no clean shutdown to flush anything at, which is why history is
//! written as it is recorded rather than on the way out.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::{AppState, Focus, Severity};
use crate::app::event::{Cmd, Response};
use crate::config::MIN_QUERY_LEN;

impl AppState {
    pub(super) fn on_key(&mut self, key: KeyEvent, now: Instant) -> Response {
        // Windows reports both press and release as key events; without this
        // guard every keystroke is handled twice.
        if key.kind != KeyEventKind::Press {
            return Response::none();
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        // Recall owns the arrows, Enter and Esc while it is up. Any other key
        // is someone going back to editing, so the recalled code is kept and
        // put through the query pipeline - otherwise recalling a code and then
        // pressing Left would leave it on screen having never been searched
        // for.
        let mut leaving = Response::none();
        if self.focus == Focus::History {
            match key.code {
                KeyCode::Up => return self.history_older(),
                KeyCode::Down => return self.history_newer(),
                KeyCode::Enter => return self.accept_history(now),
                KeyCode::Esc => return self.cancel_history(now),
                _ => {
                    self.leave_history();
                    leaving = self.on_input_changed(now);
                }
            }
        }

        let response = match key.code {
            KeyCode::Char('c') if ctrl => self.copy_selection(now),
            KeyCode::Char('v') if ctrl => Response::none().with(Cmd::ReadClipboard),
            KeyCode::Char('a') if ctrl => {
                self.input.select_all();
                Response::redraw()
            }
            KeyCode::Char('u') if ctrl => {
                if self.input.is_empty() {
                    Response::none()
                } else {
                    self.input.clear();
                    self.on_input_changed(now)
                }
            }
            KeyCode::Char('w') if ctrl => self.delete_field(now),

            // Every other modified key is a binding this program does not
            // have, not text. Without this the catch-all below turned Ctrl+W
            // into a literal `w` in the search box.
            KeyCode::Char(_) if ctrl || alt => Response::none(),

            KeyCode::Char(c) => {
                self.input.insert_char(c);
                self.on_input_changed(now)
            }

            KeyCode::Backspace if ctrl || alt => self.delete_field(now),
            KeyCode::Backspace => {
                if !self.input.backspace() {
                    return leaving;
                }
                self.on_input_changed(now)
            }
            KeyCode::Delete => {
                if !self.input.delete() {
                    return leaving;
                }
                self.on_input_changed(now)
            }

            KeyCode::Left => {
                self.focus = Focus::Input;
                self.input.move_left(ctrl, shift);
                Response::redraw()
            }
            KeyCode::Right => {
                self.focus = Focus::Input;
                self.input.move_right(ctrl, shift);
                Response::redraw()
            }
            KeyCode::Home => {
                if self.focus == Focus::Results {
                    self.jump_selection(0)
                } else {
                    self.input.move_home(shift);
                    Response::redraw()
                }
            }
            KeyCode::End => {
                if self.focus == Focus::Results {
                    self.jump_selection(self.hits.len().saturating_sub(1))
                } else {
                    self.input.move_end(shift);
                    Response::redraw()
                }
            }

            KeyCode::Up => self.on_up(now),
            KeyCode::Down => self.on_down(),

            KeyCode::Enter => self.on_enter(now),
            KeyCode::Esc => self.on_escape(now),

            KeyCode::F(5) => {
                self.set_toast("refreshing index...".into(), Severity::Info, now);
                let mut r = Response::redraw().with(Cmd::RefreshIndex { force: true });
                // F5 also forces an immediate re-verification, bypassing the
                // debounce.
                if self.input.chars().count() >= MIN_QUERY_LEN {
                    self.verify_due_at = Some(now);
                    r = r.with(Cmd::Search {
                        query: self.input.text().to_string(),
                        epoch: self.query_epoch,
                    });
                }
                r
            }
            _ => Response::none(),
        };

        let mut out = leaving;
        out.merge(response);
        out
    }

    // --- arrows -----------------------------------------------------------

    fn on_up(&mut self, now: Instant) -> Response {
        match self.focus {
            Focus::History => self.history_older(),
            Focus::Results => match self.selected_row() {
                // Off the top of the list is a return to the search box, not
                // a wrap to the bottom. Wrapping there would put the selection
                // as far from where someone was looking as it is possible to
                // get.
                Some(0) | None => {
                    self.focus = Focus::Input;
                    self.selection_pinned = false;
                    Response::redraw()
                }
                Some(_) => self.move_selection(-1),
            },
            Focus::Input => self.open_history(now),
        }
    }

    fn on_down(&mut self) -> Response {
        match self.focus {
            Focus::History => self.history_newer(),
            Focus::Results => self.move_selection(1),
            Focus::Input => {
                if self.hits.is_empty() {
                    return Response::none();
                }
                self.focus = Focus::Results;
                self.jump_selection(0)
            }
        }
    }

    // --- recall -----------------------------------------------------------

    fn open_history(&mut self, now: Instant) -> Response {
        let draft = self.input.text().to_string();
        let Some(entry) = self.history.begin(&draft) else {
            self.set_toast("no previous codes yet".into(), Severity::Info, now);
            return Response::redraw();
        };
        let entry = entry.to_string();
        self.focus = Focus::History;
        self.input.set_text(entry);

        // Browsing must not search. Stepping through twenty codes would
        // otherwise be twenty server round trips for codes nobody has chosen
        // yet, so the pending work for the code being replaced is stood down
        // and re-armed when an entry is actually taken.
        self.verify_due_at = None;
        self.prefetch_due_at = None;
        Response::redraw()
    }

    pub(super) fn history_older(&mut self) -> Response {
        if let Some(entry) = self.history.older() {
            let entry = entry.to_string();
            self.input.set_text(entry);
        }
        Response::redraw()
    }

    pub(super) fn history_newer(&mut self) -> Response {
        match self.history.newer() {
            Some(entry) => {
                let entry = entry.to_string();
                self.input.set_text(entry);
                Response::redraw()
            }
            // Past the newest entry, so the half-typed code comes back.
            None => self.restore_draft(),
        }
    }

    fn accept_history(&mut self, now: Instant) -> Response {
        self.leave_history();
        self.on_input_changed(now)
    }

    fn cancel_history(&mut self, now: Instant) -> Response {
        let draft = self.history.cancel().unwrap_or_default();
        self.focus = Focus::Input;
        self.input.set_text(draft);
        self.on_input_changed(now)
    }

    fn restore_draft(&mut self) -> Response {
        let draft = self.history.cancel().unwrap_or_default();
        self.focus = Focus::Input;
        self.input.set_text(draft);
        Response::redraw()
    }

    // --- clipboard and escape ---------------------------------------------

    fn copy_selection(&mut self, now: Instant) -> Response {
        match self.input.selected_text() {
            Some(text) => Response::none().with(Cmd::Copy(text.to_string())),
            None => {
                // Nothing selected is worth saying: this is the key that used
                // to quit, so silence here reads as "the program ignored me"
                // to anyone expecting the old behaviour.
                self.set_toast("nothing selected to copy".into(), Severity::Info, now);
                Response::redraw()
            }
        }
    }

    fn delete_field(&mut self, now: Instant) -> Response {
        if !self.input.delete_prev_field() {
            return Response::none();
        }
        self.on_input_changed(now)
    }

    fn on_escape(&mut self, now: Instant) -> Response {
        // Escape never quits. It undoes, one layer at a time.
        if self.input.has_selection() {
            self.input.clear_selection();
            return Response::redraw();
        }
        if self.focus == Focus::Results {
            self.focus = Focus::Input;
            self.selection_pinned = false;
            return Response::redraw();
        }
        if self.input.is_empty() {
            return Response::none();
        }
        self.input.clear();
        self.on_input_changed(now)
    }
}
