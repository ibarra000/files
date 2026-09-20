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

use crate::app::key::{Key, KeyEvent};

use super::{AppState, Severity, Urgency};
use crate::app::event::{Cmd, Redraw, RefreshTarget, Response};
use crate::config::VISIBLE_ROWS;

impl AppState {
    pub(super) fn on_key(&mut self, key: KeyEvent, now: Instant) -> Response {
        // Windows reports both press and release as key events; without this
        // guard every keystroke is handled twice.
        if !key.is_press() {
            return Response::none();
        }

        let ctrl = key.mods.ctrl;
        let shift = key.mods.shift;
        let alt = key.mods.alt;

        // Recall owns the arrows, Enter and Esc while it is up. Any other key
        // is someone going back to editing, so the recalled code is kept and
        // put through the query pipeline - otherwise recalling a code and then
        // pressing Left would leave it on screen having never been searched
        // for.
        // A keystroke means the hands have left the mouse, so a hover
        // highlight from wherever the pointer was left is no longer telling
        // the truth. No terminal reports the pointer leaving the window, so
        // this is the only moment that can honestly clear it.
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
                // Anything else hands control back, as help and recall do.
                _ => {
                    leaving = self.close_shares();
                }
            }
        }

        // Escape peels the recall list the way it peels the drive picker, and
        // for the reason `on_escape` gives for that exception: "a list that
        // cannot be shut without taking the panel with it would be a trap."
        // The status line has been promising "Esc to go back" throughout;
        // until now nothing kept the promise and the whole window closed.
        if self.history.is_browsing() && key.key == Key::Esc {
            return self.end_recall();
        }

        let response = match key.key {
            // The one key that leaves. Ctrl+Q rather than Ctrl+C because Ctrl+C
            // copies a selection here, and rather than Esc because Esc is
            // reached for constantly while searching - two taps of it used to
            // end the session by accident, which is what this binding exists
            // to replace. Raw mode means Ctrl+C never raises SIGINT either, so
            // without this there is no way out except closing the window.
            Key::Char('q') if ctrl => {
                self.should_quit = true;
                Response::none().with(Cmd::Quit)
            }
            Key::Char('c') if ctrl => self.copy_selection(now),
            // Never arrives as a `Char('x')` from a keyboard: the toolkit
            // turns Ctrl+X and Shift+Delete into one event and `gui::input`
            // turns that back into this. See that module's note.
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
            // What this operating system uses for settings everywhere else.
            // F1 to F5 are all spoken for, and a sixth function key would be
            // one nobody guesses. Above the Ctrl+Alt arm, so it is reached at
            // all: everything below treats a modified character as text.
            Key::Char(',') if ctrl && !alt => Response::redraw().with(Cmd::ToggleSettings),

            // AltGr arrives as Ctrl+Alt on Windows, and on a German, Polish
            // or French layout that is how `@`, `{`, `[` and the accented
            // letters are typed. This program has no Ctrl+Alt binding at all,
            // so anything reaching here is a character - and the guard below,
            // which exists for Ctrl+W, was making a whole class of them
            // untypable with nothing on screen to say why.
            Key::Char(c) if ctrl && alt => {
                self.input.insert_char(c);
                self.on_input_changed(now, Urgency::Typed)
            }

            // Every other modified key is a binding this program does not
            // have, not text. Without this the catch-all below turned Ctrl+W
            // into a literal `w` in the search box.
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

            // The text field always has the keyboard, so these are always
            // caret keys. This is the whole of what "the overlay is simpler"
            // cashes out to: there is no mode to be in, and no way to be
            // confused about what an arrow will do.
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

            // A listful at a time. The list is at most a screenful, so this
            // reaches either end in one press - which is what makes it worth
            // having at all now that there is no grid to page through.
            Key::PageDown => self.move_selection(VISIBLE_ROWS as isize),
            Key::PageUp => self.move_selection(-(VISIBLE_ROWS as isize)),

            Key::Up => self.on_up(now),
            Key::Down => self.on_down(),

            Key::Enter => self.on_enter(now),
            Key::Esc => self.on_escape(now),

            // No modifier guard, matching the F5 arm below: some terminals
            // report a function key with SHIFT set, and requiring NONE would
            // make this intermittently dead. An unmodified letter is not
            // available for a toggle - every `Char` falls through into the
            // search box.
            // F1 is the one key every program has meant "help" for thirty
            // years, and it was unbound. Everything the hint bar cannot fit
            // lives behind it, which is what lets that bar stay short.
            Key::F(1) => Response::redraw().with(Cmd::ToggleHelp),

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

    /// Switches between opening whole documents and opening one file.
    ///
    /// Redraws *and* toasts, and both are needed. The help line changes, but
    /// nothing else on screen moves, so without the message the only evidence
    /// that a mode changed is a word in the footer - and someone who pressed
    /// the key by accident would have no idea what they had done.
    ///
    /// Deliberately touches neither the query epoch nor the results: choosing
    /// a different program to open a file with is not a reason to search
    /// again.
    fn toggle_viewer(&mut self, now: Instant) -> Response {
        self.viewer = self.viewer.next();
        // `display`, not `name`: the latter is the spelling the config file
        // round-trips, and `Pdf` reads as a typo on screen.
        let name = self.viewer.display();

        if self
            .settings
            .can_save(crate::config::write::SettingKey::Viewer)
        {
            // The toast is raised by the save reporting back, so that what the
            // user reads is what actually reached the disk.
            Response::redraw().with(Cmd::SaveViewer(self.viewer))
        } else {
            // The environment or the command line outranks the file, so
            // writing it would report a save the next start ignores. Saying so
            // is better than saving into the void.
            self.set_toast(
                format!("Viewer: {name} \u{b7} this session only"),
                Severity::Info,
                now,
            );
            Response::redraw()
        }
    }

    // --- narrowing what is already on screen -------------------------------

    /// Steps the match mode, and rewrites the line to say so.
    ///
    /// The line is edited rather than a filter being held beside it, and that
    /// is the whole design: the query lives in exactly one place, so it is
    /// recalled with the line, copied with the line, and visible without a
    /// chip anybody has to notice. What will run is always what is on screen.
    ///
    /// `Urgency::Complete`, not `Typed`: this is a deliberate press rather
    /// than a character on the way to a longer code, so there is nothing to
    /// wait for. The same argument the paste path makes.
    fn cycle_match_mode(&mut self, now: Instant) -> Response {
        let next = self.query.with_mode(self.query.mode().next());
        self.retype(next.to_line(), now)
    }

    /// Steps the type filter, and rewrites the line to say so.
    fn cycle_file_type(&mut self, now: Instant) -> Response {
        let next = self.query.with_types(self.query.next_types());
        self.retype(next.to_line(), now)
    }

    /// Replaces the line with one this program composed.
    ///
    /// Not a no-op guard on an unchanged line: `next_types` can legitimately
    /// return what is already there when the line was hand-typed, and a key
    /// that silently does nothing is worse than one that re-runs a search
    /// costing a millisecond.
    fn retype(&mut self, line: String, now: Instant) -> Response {
        self.input.set_text(line);
        self.on_input_changed(now, Urgency::Complete)
    }

    // --- arrows -----------------------------------------------------------

    // --- choosing a drive to update ---------------------------------------

    /// Opens the update list rather than updating everything.
    ///
    /// `F5` used to re-read every share immediately. Almost every press wanted
    /// one of them, and across a few hundred people the difference is between
    /// a handful of passes and several hundred simultaneous ones over a share
    /// of three hundred thousand folders. So the key asks.
    fn open_shares(&mut self) -> Response {
        if self.share_ids().is_empty() {
            return Response::none();
        }
        // Land on whichever share has the strongest reason to be updated, so
        // the common case is F5 then Enter.
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
        // Wrapped rather than clamped, unlike the results list. That list is
        // long enough that its ends are a useful place to stop; this one is as
        // long as somebody has drives, which is often two - and with two, half
        // of every press of an arrow key would do nothing at all.
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

    /// Re-runs the query against whatever the refresh brings back, without
    /// waiting out the verification debounce. Asking for an update and then
    /// watching a stale list for another third of a second reads as the
    /// update having done nothing.
    fn after_refresh(&mut self, mut r: Response) -> Response {
        if self.input.chars().count() >= crate::config::MIN_QUERY_LEN {
            // Dispatched here and now, so any debounce armed by the keystroke
            // that opened the picker would only be a second, redundant run.
            self.search_due_at = None;
            self.verify_due_at = Some(self.last_frame);
            r = r.with(Cmd::Search {
                query: self.query.clone(),
                epoch: self.query_epoch,
            });
        }
        r
    }

    /// Up always moves the selection. Always.
    ///
    /// Which list it moves in follows from what is on screen rather than from
    /// a mode: with nothing typed the body is the codes used before, and with
    /// something typed it is the files that matched. Nobody has to know which,
    /// because the answer to "what will this arrow do" is the same either way -
    /// *move the highlight up the list you can see*.
    fn on_up(&mut self, _now: Instant) -> Response {
        if self.picking_share {
            return self.move_share(-1);
        }
        if self.history.is_browsing() {
            return self.history_older();
        }
        // The only door into recall. An empty field is a *precondition* rather
        // than a description of what is on screen: the body shows the first-run
        // block until this key is pressed, which is what keeps a list of
        // somebody's job codes from appearing in front of whoever walks past.
        if self.can_begin_recall() {
            return self.begin_recall();
        }
        match self.selected_row() {
            // The top holds rather than wrapping. There is nowhere to hand
            // focus back to any more - the field never lost it - so a further
            // Up is simply a key that has run out of list.
            Some(0) | None => Response::none(),
            Some(_) => self.move_selection(-1),
        }
    }

    fn on_down(&mut self) -> Response {
        if self.picking_share {
            return self.move_share(1);
        }
        // Never *starts* recall, which is the asymmetry that matters. Down
        // used to call `begin_recall` too, so pressing it on an empty field put
        // the code just cleared straight back - and pressing it again cleared
        // it, and again put it back. One key, two states, forever.
        if self.history.is_browsing() {
            return self.history_newer();
        }
        if self.hits.is_empty() {
            return Response::none();
        }
        // Moves rather than landing on row 0: the top row is already
        // highlighted before the first Down is pressed, so stepping onto it
        // would look like the key did nothing.
        self.move_selection(1)
    }

    /// Drops a hover highlight the pointer has moved on from.
    fn clear_hover(&mut self) -> Redraw {
        if self.hovered.take().is_some() {
            Redraw::Yes
        } else {
            Redraw::No
        }
    }

    // --- help ---------------------------------------------------------------
    // --- recall -----------------------------------------------------------

    /// True when the codes used before are on screen.
    ///
    /// Exactly "the Up arrow started browsing", and nothing else. The list is
    /// not a view of an empty field any more: it used to appear on its own
    /// whenever nothing was typed, which meant every summon of an empty panel
    /// opened onto somebody's search history with nobody having asked for it.
    ///
    /// It stays true while a code is being *previewed* - stepping onto an entry
    /// puts it in the field - which is why this is asked before anything looks
    /// at whether the field is empty.
    pub fn showing_recent(&self) -> bool {
        self.history.is_browsing()
    }

    /// Whether Up would open the recall list rather than move a selection.
    ///
    /// Its own predicate rather than a second meaning for `showing_recent`.
    /// One function answering both "is the list on screen" and "may it be
    /// opened" is what let the renderer and the hint bar disagree about it.
    fn can_begin_recall(&self) -> bool {
        !self.history.is_browsing() && self.input.is_empty() && !self.history.is_empty()
    }

    /// Steps onto the most recent code, which is what the first Up does.
    ///
    /// The first arrow lands on the newest code rather than stepping past it:
    /// beginning and then stepping would skip one, which is the sort of thing
    /// nobody reports and everybody works around.
    fn begin_recall(&mut self) -> Response {
        let Some(entry) = self.history.begin().map(str::to_string) else {
            // An arrow with nowhere to go does nothing, rather than something
            // surprising - and draws no frame for it.
            return Response::none();
        };
        self.input.set_text(entry);
        // Browsing must not search, and must not rewrite the list being
        // browsed. Stepping through twenty codes would otherwise be twenty
        // server round trips, twenty matcher runs over the whole index and
        // twenty recall entries for codes nobody has chosen yet - so every
        // pending deadline is stood down, and re-armed when an entry is
        // actually taken. Browsing does not go through `on_input_changed`, so
        // nothing else would clear them.
        // `stand_down` rather than three assignments: it also clears
        // `enter_pending`, and Enter inside `SEARCH_DEBOUNCE` followed by Up
        // otherwise left it armed - so when the matcher answered it opened a
        // row under a recalled code nobody had searched for.
        self.stand_down();
        Response::redraw()
    }

    /// Takes the code being previewed and searches for it.
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
            // Past the newest entry, which is the way out of recall by arrow.
            None => self.end_recall(),
        }
    }

    /// Leaves recall, putting the field back the way recall found it.
    ///
    /// Empty, always: `on_up` only starts browsing from an empty field, so
    /// there is nothing else it could have found - which is the whole of what
    /// `History`'s draft used to hold and the reason it could be deleted.
    ///
    /// And it *stays* empty. This used to be followed by a Down that called
    /// `begin_recall` again and put the code straight back.
    ///
    /// Nothing is searched for: clearing a field that was already clear is not
    /// an edit, so `on_input_changed` is deliberately not called and the phase,
    /// the empty reason and the (already empty) result list are left exactly as
    /// browsing found them.
    pub(super) fn end_recall(&mut self) -> Response {
        self.history.accept();
        self.input.clear();
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
                self.set_toast("Nothing selected to copy".into(), Severity::Info, now);
                Response::redraw()
            }
        }
    }

    /// Copy, then remove what was copied.
    ///
    /// The copy is ordered *after* the edit in the response rather than issued
    /// first, which costs nothing: `Cmd::Copy` carries the text by value, so
    /// the clipboard thread is not reading a field the edit has already
    /// changed.
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
        // In the overlay Escape closes, on the first press and from wherever
        // the focus happens to be. It does not peel.
        //
        // Two reasons, and neither is taste. A window covering somebody's work
        // has to be dismissable without them working out which layer they are
        // on - and the top layer here is usually one they did not create,
        // because summoning selects the code so the next keystroke replaces
        // it, which would make the first Escape do nothing visible at all.
        //
        // The other is that "Escape clears, a second Escape closes" is
        // actively broken: clearing runs `on_input_changed`, which bumps the
        // generation, drops the debounce deadline and puts the phase back to
        // `Idle`, so the code and the fact that it had settled are both gone
        // before a second press could commit it. The people who reach for
        // Escape would be exactly the ones whose codes were never remembered.
        //
        // The drive picker is the one exception, and it does not arrive here:
        // `on_key` intercepts Escape while it is up and closes it instead. It
        // advertises that in its own key hints, and a list that cannot be shut
        // without taking the panel with it would be a trap.
        if self.overlay_up {
            return self.request_dismiss();
        }

        // Not summoned - which now only happens in a test, since the panel is
        // only ever on screen because something summoned it. Escape still
        // undoes rather than quitting.
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
