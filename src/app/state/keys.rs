//! Key bindings for the panel.
//!
//! A child module of `state` so it can reach the state's private fields.
//!
//! History is written as entries are recorded rather than at exit, since the
//! window can be closed without a clean shutdown.

use std::sync::Arc;
use std::time::Instant;

use crate::app::key::{Key, KeyEvent};

use super::{AppState, Severity, Urgency, Wrap};
use crate::app::event::{Cmd, Redraw, RefreshTarget, Response};
use crate::config::ViewerKind;

impl AppState {
    pub(super) fn on_key(&mut self, key: KeyEvent, now: Instant) -> Response {
        // Windows reports both press and release; act on presses only.
        if !key.is_press() {
            return Response::none();
        }

        let ctrl = key.mods.ctrl;
        let shift = key.mods.shift;
        let alt = key.mods.alt;

        // A keystroke means the hands left the mouse, so drop any hover
        // highlight left where the pointer was.
        let stale_hover = self.clear_hover();

        let mut leaving = Response::none().with_redraw(stale_hover);

        if self.picking_share {
            match key.key {
                Key::Up => return self.move_share(-1),
                Key::Down => return self.move_share(1),
                Key::Enter => return self.update_chosen_share(now),
                Key::Esc | Key::F(5) => return self.close_shares(),
                Key::Char('q') if ctrl => {
                    self.should_quit = true;
                    return Response::none().with(Cmd::Quit);
                }
                // Any other key closes the picker and is then handled normally.
                _ => {
                    leaving = self.close_shares();
                }
            }
        }

        // The actions menu is drawn on top of the picker and recall list, so it
        // is handled first. Esc closes only the menu, not the panel.
        if self.actions_open {
            match key.key {
                Key::Esc => {
                    self.actions_open = false;
                    return Response::redraw();
                }
                // Ctrl+K's own arm below toggles it; closing here as well would
                // let the toggle reopen it.
                Key::Char('k') if ctrl => {}
                // Any other bound key runs and closes the menu. Bare modifiers
                // are ignored so the menu stays open while a chord is typed.
                Key::Char(_) | Key::Enter | Key::F(_) => self.actions_open = false,
                _ => {}
            }
        }

        // Esc closes the alias list. Checked before recall; the two are
        // mutually exclusive.
        if self.alias_cursor().is_some() && key.key == Key::Esc {
            self.alias_cursor = None;
            return Response::redraw();
        }

        // Esc leaves the recall list rather than dismissing the panel.
        if self.history.is_browsing() && key.key == Key::Esc {
            return self.end_recall();
        }

        let response = match key.key {
            // Ctrl+Q quits. Ctrl+C copies and Esc is pressed too often while
            // searching to be the exit key.
            Key::Char('q') if ctrl => {
                self.should_quit = true;
                Response::none().with(Cmd::Quit)
            }
            Key::Char('c') if ctrl => self.copy_selection(now),
            // Ctrl+X and Shift+Delete arrive from the toolkit as one Cut event,
            // which `gui::input` maps to this. See that module's note.
            Key::Char('x') if ctrl => self.cut_selection(now),
            Key::Char('v') if ctrl => Response::none().with(Cmd::ReadClipboard),
            Key::Char('a') if ctrl => {
                self.input.select_all();
                Response::redraw()
            }
            Key::Char('u') if ctrl => {
                if self.input.is_empty() {
                    Response::none()
                } else {
                    self.input.clear();
                    self.on_input_changed(now, Urgency::Typed)
                }
            }
            Key::Char('w') if ctrl => self.delete_field(now),
            // Must precede the Ctrl+Alt arm, which treats modified characters
            // as text. Delivered via `gui::input::chord`; `tests/bindings.rs`
            // drives that real input layer.
            Key::Char(',') if ctrl && !alt => Response::redraw().with(Cmd::ToggleSettings),

            // Act on the selected row with a one-off viewer, without changing
            // the current viewer.
            Key::Char('d') if ctrl && !alt => self.act_on_selection(ViewerKind::Pdf, now),
            // The first configured viewer, if any.
            Key::Char('e') if ctrl && !alt => {
                if self.settings.viewers.is_empty() {
                    // Reported rather than ignored so the key does not seem broken.
                    self.set_toast(
                        "No viewer configured \u{b7} add a [[viewer]] to config.toml".into(),
                        Severity::Info,
                        now,
                    );
                    Response::redraw()
                } else {
                    self.act_on_selection(ViewerKind::App(0), now)
                }
            }
            Key::Char('o') if ctrl && !alt => self.reveal_selection(now),
            // Lists every action for the row, so the footer need not.
            Key::Char('k') if ctrl && !alt => {
                self.actions_open = !self.actions_open;
                Response::redraw()
            }
            // Home-row arrows. Routed through `on_up`/`on_down` so they behave
            // exactly like Up and Down.
            Key::Char('p') if ctrl && !alt => self.on_up(now),
            Key::Char('n') if ctrl && !alt => self.on_down(),
            // Selects the whole line, like focusing a search box elsewhere; the
            // field already has focus, so this is the same as Ctrl+A.
            Key::Char('l') if ctrl && !alt => {
                self.input.select_all();
                Response::redraw()
            }

            // AltGr arrives as Ctrl+Alt on Windows and types `@`, `{`, `[` and
            // accented letters on many layouts. No binding uses Ctrl+Alt, so
            // these are text. Must precede the next arm.
            Key::Char(c) if ctrl && alt => {
                self.input.insert_char(c);
                self.on_input_changed(now, Urgency::Typed)
            }

            // Any other modified character is an unbound chord, not text.
            Key::Char(_) if ctrl || alt => Response::none(),

            Key::Char(c) => {
                self.input.insert_char(c);
                self.on_input_changed(now, Urgency::Typed)
            }

            Key::Backspace if ctrl || alt => self.delete_field(now),
            Key::Backspace => {
                if !self.input.backspace() {
                    return leaving;
                }
                self.on_input_changed(now, Urgency::Typed)
            }
            Key::Delete => {
                if !self.input.delete() {
                    return leaving;
                }
                self.on_input_changed(now, Urgency::Typed)
            }

            // The text field always has focus, so these are caret keys.
            Key::Left => {
                self.input.move_left(ctrl, shift);
                Response::redraw()
            }
            Key::Right => {
                self.input.move_right(ctrl, shift);
                Response::redraw()
            }
            Key::Home => {
                self.input.move_home(shift);
                Response::redraw()
            }
            Key::End => {
                self.input.move_end(shift);
                Response::redraw()
            }

            // One screenful, which is a different row count per layout.
            Key::PageDown => self.move_selection(self.rows_per_page(), Wrap::Stop),
            Key::PageUp => self.move_selection(-self.rows_per_page(), Wrap::Stop),

            Key::Up => self.on_up(now),
            Key::Down => self.on_down(),
            // Moves across result columns; does nothing in a single column.
            Key::Tab => self.on_tab(!shift),

            Key::Enter => self.on_enter(now),
            Key::Esc => self.on_escape(now),

            // No modifier guard, so a function key reported with Shift still
            // works. F1 is intentionally unbound.
            Key::F(2) => self.toggle_viewer(now),

            Key::F(3) => self.cycle_match_mode(now),
            Key::F(4) => self.cycle_file_type(now),

            Key::F(5) => self.open_shares(),
            _ => Response::none(),
        };

        let mut out = leaving;
        out.merge(response);
        out
    }

    // --- viewer -----------------------------------------------------------

    /// Cycles the default viewer.
    ///
    /// Redraws and toasts, since only a footer word changes otherwise. Leaves
    /// the query and results alone.
    fn toggle_viewer(&mut self, now: Instant) -> Response {
        let viewers = std::sync::Arc::clone(&self.settings.viewers);
        self.viewer = self.viewer.next(&viewers);
        // `display`, not `name`: `name` is the config spelling, and `Pdf` looks
        // like a typo on screen.
        let name = self.viewer.display(&viewers);

        if self
            .settings
            .can_save(crate::config::write::SettingKey::Viewer)
        {
            // The toast comes from the save result, so it reflects what reached
            // the disk.
            Response::redraw().with(Cmd::SaveViewer(self.viewer.name(&viewers).into()))
        } else {
            // An environment or command-line value outranks the file, so a
            // save would be ignored on the next start.
            self.set_toast(
                format!("Viewer: {name} \u{b7} this session only"),
                Severity::Info,
                now,
            );
            Response::redraw()
        }
    }

    // --- narrowing what is already on screen -------------------------------

    /// Steps the match mode by rewriting the line.
    ///
    /// The query lives only in the line, so it is recalled, copied and shown
    /// with it. Uses `Urgency::Complete` since a deliberate press has nothing
    /// to wait for.
    fn cycle_match_mode(&mut self, now: Instant) -> Response {
        let next = self.query.with_mode(self.query.mode().next());
        self.retype(next.to_line(), now)
    }

    /// Steps the type filter by rewriting the line.
    fn cycle_file_type(&mut self, now: Instant) -> Response {
        let next = self.query.with_types(self.query.next_types());
        self.retype(next.to_line(), now)
    }

    /// Replaces the line with a composed one and re-runs the query.
    ///
    /// Re-runs even when the line is unchanged, so the key never appears dead.
    fn retype(&mut self, line: String, now: Instant) -> Response {
        self.input.set_text(line);
        self.on_input_changed(now, Urgency::Complete)
    }

    // --- choosing a drive to update ---------------------------------------

    /// Opens the drive picker so F5 refreshes one share rather than all.
    fn open_shares(&mut self) -> Response {
        if self.share_ids().is_empty() {
            return Response::none();
        }
        // Start on the stalest share, so F5 then Enter is the common case.
        self.shares_cursor = self
            .index
            .stalest
            .and_then(|(id, _)| self.share_ids().iter().position(|x| *x == id))
            .unwrap_or(0);
        self.picking_share = true;
        Response::redraw()
    }

    fn close_shares(&mut self) -> Response {
        self.picking_share = false;
        Response::redraw()
    }

    fn move_share(&mut self, delta: isize) -> Response {
        let len = self.share_ids().len();
        if len == 0 {
            return Response::none();
        }
        // Wraps, unlike the results list: with only two drives, clamping
        // would make half of all presses do nothing.
        let len = len as isize;
        self.shares_cursor = ((self.shares_cursor as isize + delta).rem_euclid(len)) as usize;
        Response::redraw()
    }

    fn update_chosen_share(&mut self, now: Instant) -> Response {
        let ids = self.share_ids();
        let Some(&id) = ids.get(self.shares_cursor) else {
            return self.close_shares();
        };
        self.picking_share = false;
        self.set_toast(
            format!("Updating {}\u{2026}", self.settings.routes.label(id)),
            Severity::Info,
            now,
        );
        self.after_refresh(Response::redraw().with(Cmd::RefreshIndex {
            target: RefreshTarget(id),
            force: true,
        }))
    }

    /// Re-runs the query immediately after a refresh request, skipping the
    /// verification debounce so the update visibly takes effect.
    fn after_refresh(&mut self, mut r: Response) -> Response {
        if self.input.chars().count() >= crate::config::MIN_TERM_LEN {
            // Dispatched now, so any armed search debounce would be redundant.
            self.search_due_at = None;
            self.verify_due_at = Some(self.last_frame);
            r = r.with(Cmd::Search {
                query: self.query.clone(),
                epoch: self.query_epoch,
            });
        }
        r
    }

    /// Moves the highlight up the visible list.
    ///
    /// The list is history when nothing is typed and results otherwise.
    fn on_up(&mut self, _now: Instant) -> Response {
        if self.picking_share {
            return self.move_share(-1);
        }
        if self.history.is_browsing() {
            return self.history_older();
        }
        // The only way into recall. History is not shown until Up is
        // pressed on an empty field, so it is not on display to passers-by.
        if self.can_begin_recall() {
            return self.begin_recall();
        }
        if self.alias_cursor().is_some() {
            return self.move_alias(-1);
        }
        // Wraps from top to bottom (within the column when in columns).
        // See `AppState::move_selection`.
        self.move_vertically(false)
    }

    fn on_down(&mut self) -> Response {
        if self.picking_share {
            return self.move_share(1);
        }
        // Down never starts recall, only steps within it; otherwise Down on an
        // empty field would toggle a code in and out.
        if self.history.is_browsing() {
            return self.history_newer();
        }
        // On an empty field Down enters the alias list, as Up enters history.
        if self.showing_aliases() {
            return self.move_alias(1);
        }
        if self.hits.is_empty() {
            return Response::none();
        }
        // Steps rather than landing on row 0, which is already highlighted.
        self.move_vertically(true)
    }

    /// Moves across result columns. Single lists (picker, history, aliases)
    /// ignore Tab.
    fn on_tab(&mut self, forward: bool) -> Response {
        if self.picking_share || self.history.is_browsing() || self.showing_aliases() {
            return Response::none();
        }
        self.move_across(forward)
    }

    /// Steps through the aliases, entering from the near end. Wraps.
    fn move_alias(&mut self, delta: isize) -> Response {
        let len = self.settings.aliases.len() as isize;
        if len == 0 {
            return Response::none();
        }
        let next = match self.alias_cursor {
            None if delta > 0 => 0,
            None => len - 1,
            Some(at) => (at as isize + delta).rem_euclid(len),
        };
        self.alias_cursor = Some(next as usize);
        Response::redraw()
    }

    /// Puts the alias under the cursor into the box and searches.
    ///
    /// Inserts the alias name, not its expansion, so the box holds what the
    /// user could have typed.
    pub(super) fn accept_alias(&mut self, rank: usize, now: Instant) -> Response {
        let Some(alias) = self.settings.aliases.all().get(rank) else {
            return Response::none();
        };
        let name = alias.name.to_string();
        self.alias_cursor = None;
        self.input.set_text(name);
        self.on_input_changed(now, Urgency::Complete)
    }

    /// Drops a hover highlight the pointer has moved on from.
    fn clear_hover(&mut self) -> Redraw {
        if self.hovered.take().is_some() {
            Redraw::Yes
        } else {
            Redraw::No
        }
    }

    // --- recall -----------------------------------------------------------

    /// True while history is being browsed.
    ///
    /// Only Up starts browsing. Stays true while an entry is previewed in the
    /// field, so check this before checking whether the field is empty.
    pub fn showing_recent(&self) -> bool {
        self.history.is_browsing()
    }

    /// Whether Up would open history rather than move a selection.
    ///
    /// Separate from `showing_recent` so the renderer and hint bar ask the
    /// same question.
    fn can_begin_recall(&self) -> bool {
        !self.history.is_browsing() && self.input.is_empty() && !self.history.is_empty()
    }

    /// Steps onto the most recent entry, as the first Up does.
    fn begin_recall(&mut self) -> Response {
        let Some(entry) = self.history.begin().map(str::to_string) else {
            // Nothing to recall: do nothing and draw nothing.
            return Response::none();
        };
        self.input.set_text(entry);
        // Browsing must not search or reorder the list, so stand down every
        // pending deadline; taking an entry re-arms them. `stand_down` also
        // clears `enter_pending`, so an Enter pressed just before Up cannot
        // later open a row under the recalled entry.
        self.stand_down();
        Response::redraw()
    }

    /// Takes the entry being previewed and searches for it.
    pub(super) fn accept_recall(&mut self, now: Instant) -> Response {
        self.leave_history();
        self.on_input_changed(now, Urgency::Complete)
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
            // Past the newest entry: leave recall.
            None => self.end_recall(),
        }
    }

    /// Leaves recall and empties the field.
    ///
    /// Recall only starts from an empty field, so empty is how it was found.
    /// Not an edit, so `on_input_changed` is not called and the phase and
    /// results are left as they were.
    pub(super) fn end_recall(&mut self) -> Response {
        self.history.accept();
        self.input.clear();
        Response::redraw()
    }

    // --- clipboard and escape ---------------------------------------------

    /// Copies the text selected in the search box, or else the path of the
    /// selected row.
    ///
    /// A text selection is the more recent, explicit intent. `view::actions`
    /// advertises Ctrl+C for "Copy the path" only while there is no text
    /// selection.
    fn copy_selection(&mut self, now: Instant) -> Response {
        if let Some(text) = self.input.selected_text() {
            return Response::none().with(Cmd::Copy(text.to_string()));
        }
        if let Some(hit) = self.selected_hit().or_else(|| self.hits.first()) {
            return Response::none().with(Cmd::Copy(hit.path.to_string()));
        }
        // Report it, so the key does not appear to be ignored.
        self.set_toast("Nothing to copy".into(), Severity::Info, now);
        Response::redraw()
    }

    /// Copies the path of the selected row, regardless of the text selection.
    ///
    /// Used by the actions menu, where a synthetic Ctrl+C would copy the text
    /// selection instead.
    pub(super) fn copy_path(&mut self, now: Instant) -> Response {
        let Some(hit) = self.selected_hit().or_else(|| self.hits.first()) else {
            self.set_toast("Nothing to copy".into(), Severity::Info, now);
            return Response::redraw();
        };
        Response::none().with(Cmd::Copy(hit.path.to_string()))
    }

    /// Copies the name of the selected row. Menu-only; it has no key.
    pub(super) fn copy_name(&mut self, now: Instant) -> Response {
        let Some(hit) = self.selected_hit().or_else(|| self.hits.first()) else {
            self.set_toast("Nothing to copy".into(), Severity::Info, now);
            return Response::redraw();
        };
        Response::none().with(Cmd::Copy(hit.name.to_string()))
    }

    /// Opens the row the cursor is on with a viewer other than the current
    /// one, for this press only.
    pub(super) fn act_on_selection(&mut self, viewer: ViewerKind, now: Instant) -> Response {
        if self.hits.is_empty() {
            return Response::none();
        }
        self.open_with(viewer, now)
    }

    /// Opens Explorer with the selected row selected.
    pub(super) fn reveal_selection(&mut self, now: Instant) -> Response {
        let Some(hit) = self.selected_hit().or_else(|| self.hits.first()) else {
            self.set_toast("Nothing to show".into(), Severity::Info, now);
            return Response::redraw();
        };
        let mut response = Response::none().with(Cmd::Reveal(Arc::clone(&hit.path)));
        // Dismisses on the same terms as an open, since Explorer will appear
        // where the panel is. Mirrors `view::actions::Action::hides`, checked by
        // `every_hiding_action_hides` in `tests/overlay.rs`.
        if self.overlay_up && self.settings.hide_after_opening {
            response.merge(self.request_dismiss());
        }
        response
    }

    /// Copies the selection, then removes it.
    ///
    /// `Cmd::Copy` carries the text by value, so ordering it after the edit is
    /// safe.
    fn cut_selection(&mut self, now: Instant) -> Response {
        let Some(text) = self.input.selected_text().map(str::to_string) else {
            self.set_toast("Nothing selected to cut".into(), Severity::Info, now);
            return Response::redraw();
        };
        self.input.delete_selection();
        self.on_input_changed(now, Urgency::Typed)
            .with(Cmd::Copy(text))
    }

    fn delete_field(&mut self, now: Instant) -> Response {
        if !self.input.delete_prev_field() {
            return Response::none();
        }
        self.on_input_changed(now, Urgency::Typed)
    }

    fn on_escape(&mut self, now: Instant) -> Response {
        // In the overlay, Esc dismisses on the first press regardless of what
        // is selected: summoning selects the line, so a clearing first press
        // would look like nothing happened, and clearing runs
        // `on_input_changed`, discarding the settled query before it could be
        // remembered. The drive picker is intercepted earlier in `on_key`.
        // Controlled by `hide_on_escape`; when off, Esc falls through to
        // clearing.
        if self.overlay_up && self.settings.hide_on_escape {
            return self.request_dismiss();
        }

        // Not in the overlay, or `hide_on_escape` is off: Esc clears the
        // selection, then the line.
        if self.input.has_selection() {
            self.input.clear_selection();
            return Response::redraw();
        }
        if self.input.is_empty() {
            return Response::none();
        }
        self.input.clear();
        self.on_input_changed(now, Urgency::Typed)
    }
}
