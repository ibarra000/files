//! What the mouse does.
//!
//! The program already captured the mouse and then threw every event away,
//! which was the worst of both worlds: capture suppresses the terminal's own
//! drag-to-select, so selecting text was impossible by any means. Capture is
//! kept and the events are now used, which is the half of the trade that
//! actually buys something - clicking a caret into the middle of a code, and
//! dragging over part of it to copy.
//!
//! Holding Shift while dragging is still the way to reach the terminal's own
//! selection, over the whole window rather than just the search box. That
//! needs no code here: terminals handle it themselves, above the application.
//!
//! Hit-testing goes through [`crate::ui::layout`], the same function that
//! placed the widgets, so a click cannot land a row off because two copies of
//! the arithmetic drifted apart.

use std::time::Instant;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::{AppState, Click, Focus};
use crate::app::event::Response;
use crate::config::MULTI_CLICK_WINDOW;
use crate::ui::{Chunks, input_line};

impl AppState {
    pub(super) fn on_mouse(&mut self, event: MouseEvent, now: Instant) -> Response {
        let chunks = self.chunks();
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_press(event, &chunks, now),
            MouseEventKind::Drag(MouseButton::Left) => self.on_drag(event, &chunks),
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging = false;
                Response::none()
            }
            MouseEventKind::ScrollDown => self.on_scroll(1, event, &chunks),
            MouseEventKind::ScrollUp => self.on_scroll(-1, event, &chunks),
            _ => Response::none(),
        }
    }

    fn on_press(&mut self, event: MouseEvent, chunks: &Chunks, now: Instant) -> Response {
        // Clicking anywhere is a decision to stop browsing recall, so the
        // recalled code is kept and searched for - the same rule the keyboard
        // follows.
        let mut leaving = Response::none();
        if self.focus == Focus::History && !within(chunks.results, event.column, event.row) {
            self.leave_history();
            leaving = self.on_input_changed(now);
        }

        if within(chunks.input, event.column, event.row) {
            let extend = event.modifiers.contains(KeyModifiers::SHIFT);
            let clicks = self.click_count(event.column, event.row, now);
            let byte = self.byte_at_column(event.column, chunks);

            match clicks {
                1 => {
                    self.input.set_caret(byte, extend);
                    // Only a single click starts a drag. Extending a
                    // double-click by dragging would need a word-granular
                    // anchor, which is more machinery than the gesture is
                    // worth here.
                    self.dragging = true;
                }
                2 => self.input.select_word_at(byte),
                _ => self.input.select_all(),
            }
            self.focus = Focus::Input;
            leaving.merge(Response::redraw());
            return leaving;
        }

        if within(chunks.results, event.column, event.row) {
            let Some(row) = list_row(event.row, chunks) else {
                return leaving;
            };
            leaving.merge(self.click_row(row, now));
            return leaving;
        }

        leaving
    }

    fn click_row(&mut self, row: usize, now: Instant) -> Response {
        if self.focus == Focus::History {
            let Some(entry) = self.history.select(row) else {
                return Response::none();
            };
            let entry = entry.to_string();
            self.input.set_text(entry);
            return Response::redraw();
        }
        if row >= self.hits.len() {
            return Response::none();
        }
        let _ = now;
        self.focus = Focus::Results;
        self.jump_selection(row)
    }

    fn on_drag(&mut self, event: MouseEvent, chunks: &Chunks) -> Response {
        // Only a drag that began in the search box extends a selection.
        // Without that, dragging across the results would silently select
        // text in a box the pointer never touched.
        if !self.dragging {
            return Response::none();
        }
        let byte = self.byte_at_column(event.column, chunks);
        self.input.set_caret(byte, true);
        Response::redraw()
    }

    fn on_scroll(&mut self, delta: isize, event: MouseEvent, chunks: &Chunks) -> Response {
        if !within(chunks.results, event.column, event.row) {
            return Response::none();
        }
        match self.focus {
            Focus::History => {
                if delta > 0 {
                    self.history_older()
                } else {
                    self.history_newer()
                }
            }
            _ => {
                if self.hits.is_empty() {
                    return Response::none();
                }
                self.focus = Focus::Results;
                self.move_selection(delta)
            }
        }
    }

    /// Turns a screen column into a position in the text.
    ///
    /// Undoes the horizontal scroll by asking the renderer for it rather than
    /// recomputing it, so a click lands where the character it was aimed at is
    /// actually drawn.
    fn byte_at_column(&self, column: u16, chunks: &Chunks) -> usize {
        let relative = column.saturating_sub(chunks.input_text_x()) as usize;
        let scrolled = input_line::offset(&self.input, chunks.input_text_width());
        self.input.byte_at_column(scrolled + relative)
    }

    /// How many times this spot has been clicked in quick succession.
    ///
    /// The terminal reports presses, not clicks, so the count is kept here.
    /// It is capped at three because there is no gesture beyond select-all.
    fn click_count(&mut self, column: u16, row: u16, now: Instant) -> u8 {
        let count = match self.last_click {
            Some(previous)
                if previous.column == column
                    && previous.row == row
                    && now.saturating_duration_since(previous.at) <= MULTI_CLICK_WINDOW =>
            {
                previous.count.saturating_add(1).min(3)
            }
            _ => 1,
        };
        self.last_click = Some(Click {
            at: now,
            column,
            row,
            count,
        });
        count
    }
}

/// Whether a point is inside a rectangle.
///
/// Written out rather than taken from ratatui so the bounds are visible at the
/// one place they matter.
fn within(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Which list entry a screen row refers to, or `None` for the border.
fn list_row(row: u16, chunks: &Chunks) -> Option<usize> {
    // The list is never scrolled in practice - it is capped at MAX_RESULTS and
    // the pane is taller than that on any usable terminal - so the first entry
    // is always the first row inside the border.
    row.checked_sub(chunks.first_row_y()).map(usize::from)
}
