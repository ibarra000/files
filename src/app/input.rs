//! The editable search line: text, caret and selection.
//!
//! Pure functions of the struct, with no clock or I/O.
//!
//! # Indices
//!
//! `caret` and `anchor` are **byte** offsets derived from `char_indices`, so
//! they always lie on a character boundary and slicing cannot panic.
//!
//! *Columns* are character counts, used for on-screen placement. Convert
//! explicitly with [`Input::column_of`] and [`Input::byte_at_column`].

use std::ops::Deref;

/// The most text the line will hold.
///
/// Bounds the cost of each edit after a pathological paste.
pub const MAX_LEN: usize = 4096;

/// The search text, with a caret and an optional selection anchor.
///
/// The selection spans `anchor` and `caret` in either order; the anchor is
/// where the selection started, not necessarily the lower bound.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    text: String,
    caret: usize,
    anchor: Option<usize>,
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The selected range as `(lo, hi)`, or `None` when nothing is selected.
    ///
    /// An anchor equal to the caret is an empty selection, reported as `None`
    /// so callers never have to special-case a zero-width range.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.caret {
            return None;
        }
        Some((anchor.min(self.caret), anchor.max(self.caret)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        let (lo, hi) = self.selection()?;
        self.text.get(lo..hi)
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub fn select_all(&mut self) {
        if self.text.is_empty() {
            self.anchor = None;
            return;
        }
        self.anchor = Some(0);
        self.caret = self.text.len();
    }

    /// Replaces the whole line, as a history recall does.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.truncate_to_limit();
        self.caret = self.text.len();
        self.anchor = None;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
        self.anchor = None;
    }

    // --- editing ----------------------------------------------------------

    /// Inserts one character, replacing the selection if there is one.
    ///
    /// Returns whether the text changed, so the caller knows whether to run
    /// the query pipeline.
    pub fn insert_char(&mut self, c: char) -> bool {
        let mut buf = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut buf))
    }

    /// Inserts text at the caret, replacing the selection if there is one.
    pub fn insert_str(&mut self, s: &str) -> bool {
        let had_selection = self.delete_selection();
        if s.is_empty() {
            return had_selection;
        }
        if self.text.len() + s.len() > MAX_LEN {
            // Insert what fits rather than refusing the whole paste.
            let room = MAX_LEN.saturating_sub(self.text.len());
            let fits = floor_boundary(s, room);
            if fits == 0 {
                return had_selection;
            }
            self.text.insert_str(self.caret, &s[..fits]);
            self.caret += fits;
        } else {
            self.text.insert_str(self.caret, s);
            self.caret += s.len();
        }
        self.anchor = None;
        true
    }

    /// Deletes the selection. Returns whether anything was removed.
    pub fn delete_selection(&mut self) -> bool {
        let Some((lo, hi)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.text.replace_range(lo..hi, "");
        self.caret = lo;
        self.anchor = None;
        true
    }

    /// Backspace: the selection if there is one, otherwise one character.
    pub fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let Some((start, _)) = self.char_before(self.caret) else {
            return false;
        };
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        true
    }

    /// Delete: the selection if there is one, otherwise the character ahead.
    pub fn delete(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let Some((end, _)) = self.char_at(self.caret) else {
            return false;
        };
        self.text.replace_range(self.caret..end, "");
        true
    }

    /// Deletes back to the start of the previous field, as Ctrl+W does.
    pub fn delete_prev_field(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let start = self.prev_field_start(self.caret);
        if start == self.caret {
            return false;
        }
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        true
    }

    // --- motion -----------------------------------------------------------

    pub fn move_left(&mut self, by_field: bool, extend: bool) {
        // Without `extend`, collapse an existing selection to its near edge,
        // as standard text fields do.
        if !extend && let Some((lo, _)) = self.selection() {
            self.caret = lo;
            self.anchor = None;
            return;
        }
        self.begin_move(extend);
        self.caret = if by_field {
            self.prev_field_start(self.caret)
        } else {
            self.char_before(self.caret).map_or(0, |(i, _)| i)
        };
    }

    pub fn move_right(&mut self, by_field: bool, extend: bool) {
        if !extend && let Some((_, hi)) = self.selection() {
            self.caret = hi;
            self.anchor = None;
            return;
        }
        self.begin_move(extend);
        self.caret = if by_field {
            self.next_field_end(self.caret)
        } else {
            self.char_at(self.caret).map_or(self.text.len(), |(i, _)| i)
        };
    }

    pub fn move_home(&mut self, extend: bool) {
        self.begin_move(extend);
        self.caret = 0;
    }

    pub fn move_end(&mut self, extend: bool) {
        self.begin_move(extend);
        self.caret = self.text.len();
    }

    /// Places the caret at a byte offset, clamped to a character boundary.
    pub fn set_caret(&mut self, byte: usize, extend: bool) {
        self.begin_move(extend);
        self.caret = self.clamp_boundary(byte);
    }

    /// Selects the run of non-whitespace around `byte`.
    ///
    /// Splits on whitespace only, so a double-click on `AB-C-0704` selects the
    /// whole search rather than `AB`.
    pub fn select_word_at(&mut self, byte: usize) {
        let at = self.clamp_boundary(byte);
        let mut lo = at;
        while let Some((start, c)) = self.char_before(lo) {
            if c.is_whitespace() {
                break;
            }
            lo = start;
        }
        let mut hi = at;
        while let Some((end, c)) = self.char_at(hi) {
            if c.is_whitespace() {
                break;
            }
            hi = end;
        }
        if lo == hi {
            self.anchor = None;
        } else {
            self.anchor = Some(lo);
            self.caret = hi;
        }
    }

    /// Sets the anchor for an extending move, and drops it otherwise.
    fn begin_move(&mut self, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
    }

    // --- columns ----------------------------------------------------------

    /// The character count before `byte`, which is the column it renders at.
    pub fn column_of(&self, byte: usize) -> usize {
        let byte = self.clamp_boundary(byte);
        self.text[..byte].chars().count()
    }

    pub fn caret_column(&self) -> usize {
        self.column_of(self.caret)
    }

    /// The byte offset of a column, saturating at the end of the text.
    pub fn byte_at_column(&self, column: usize) -> usize {
        self.text
            .char_indices()
            .nth(column)
            .map_or(self.text.len(), |(i, _)| i)
    }

    pub fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    // --- boundaries -------------------------------------------------------

    /// The largest character boundary at or below `i`.
    fn clamp_boundary(&self, i: usize) -> usize {
        let mut i = i.min(self.text.len());
        while i > 0 && !self.text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// The character ending at `i`, as `(its start, the character)`.
    fn char_before(&self, i: usize) -> Option<(usize, char)> {
        let i = self.clamp_boundary(i);
        self.text[..i].char_indices().next_back()
    }

    /// The character starting at `i`, as `(its end, the character)`.
    fn char_at(&self, i: usize) -> Option<(usize, char)> {
        let i = self.clamp_boundary(i);
        self.text[i..].chars().next().map(|c| (i + c.len_utf8(), c))
    }

    fn prev_field_start(&self, from: usize) -> usize {
        let mut i = self.clamp_boundary(from);
        while let Some((start, c)) = self.char_before(i) {
            if is_field_char(c) {
                break;
            }
            i = start;
        }
        while let Some((start, c)) = self.char_before(i) {
            if !is_field_char(c) {
                break;
            }
            i = start;
        }
        i
    }

    fn next_field_end(&self, from: usize) -> usize {
        let mut i = self.clamp_boundary(from);
        while let Some((end, c)) = self.char_at(i) {
            if is_field_char(c) {
                break;
            }
            i = end;
        }
        while let Some((end, c)) = self.char_at(i) {
            if !is_field_char(c) {
                break;
            }
            i = end;
        }
        i
    }

    fn truncate_to_limit(&mut self) {
        if self.text.len() > MAX_LEN {
            let cut = floor_boundary(&self.text, MAX_LEN);
            self.text.truncate(cut);
        }
    }
}

/// Cleans text arriving from outside the program.
///
/// Removes control characters from pasted text and trims it.
///
/// A newline in the query would be invisible yet make the match fail.
/// Multi-line text collapses to its content rather than being refused.
pub fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LEN)
        .collect();
    cleaned.trim().to_string()
}

/// Fields are runs of alphanumerics, so Ctrl+Left steps `AB-C-0704` one
/// dash-delimited field at a time.
fn is_field_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// The largest character boundary at or below `at`.
fn floor_boundary(s: &str, at: usize) -> usize {
    let mut at = at.min(s.len());
    while at > 0 && !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Lets the line be read directly as a `&str`.
impl Deref for Input {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl PartialEq<&str> for Input {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl PartialEq<str> for Input {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl From<&str> for Input {
    fn from(s: &str) -> Self {
        let mut input = Self::new();
        input.set_text(s);
        input
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Input {
        let mut input = Input::new();
        for c in s.chars() {
            input.insert_char(c);
        }
        input
    }

    #[test]
    fn typing_leaves_the_caret_at_the_end() {
        let input = typed("AB-C-0704");
        assert_eq!(input.text(), "AB-C-0704");
        assert_eq!(input.caret(), input.text().len());
        assert!(!input.has_selection());
    }

    #[test]
    fn a_character_can_be_inserted_in_the_middle() {
        let mut input = typed("AB-C-074");
        input.move_left(false, false);
        assert!(input.insert_char('0'));
        assert_eq!(input.text(), "AB-C-0704");
    }

    #[test]
    fn backspace_at_the_start_reports_no_change() {
        let mut input = typed("ab");
        input.move_home(false);
        assert!(!input.backspace());
        assert_eq!(input.text(), "ab");
    }

    #[test]
    fn delete_removes_the_character_ahead_and_leaves_the_caret_put() {
        let mut input = typed("abc");
        input.move_home(false);
        assert!(input.delete());
        assert_eq!(input.text(), "bc");
        assert_eq!(input.caret(), 0);
        input.move_end(false);
        assert!(!input.delete(), "nothing ahead of the end");
    }

    #[test]
    fn shift_left_extends_a_selection_the_caret_started() {
        let mut input = typed("AB-C-0704");
        input.move_left(false, true);
        input.move_left(false, true);
        assert_eq!(input.selected_text(), Some("04"));
    }

    #[test]
    fn an_unextended_move_collapses_the_selection_to_its_near_edge() {
        let mut input = typed("abcdef");
        input.move_left(false, true);
        input.move_left(false, true);
        assert_eq!(input.selected_text(), Some("ef"));

        input.move_left(false, false);
        assert!(!input.has_selection());
        assert_eq!(input.caret(), 4, "collapses to the start of the run");
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut input = typed("AB-C-0704");
        input.select_all();
        input.insert_char('X');
        assert_eq!(input.text(), "X");
        assert!(!input.has_selection());
    }

    #[test]
    fn backspace_over_a_selection_removes_the_whole_run() {
        let mut input = typed("AB-C-0704");
        input.move_left(false, true);
        input.move_left(false, true);
        assert!(input.backspace());
        assert_eq!(input.text(), "AB-C-07");
        assert!(!input.has_selection());
    }

    #[test]
    fn field_motion_steps_between_the_dashes_of_a_search() {
        let mut input = typed("AB-C-0704");
        input.move_left(true, false);
        assert_eq!(input.caret(), 5, "start of 0704");
        input.move_left(true, false);
        assert_eq!(input.caret(), 3, "start of C");
        input.move_left(true, false);
        assert_eq!(input.caret(), 0);
        input.move_left(true, false);
        assert_eq!(input.caret(), 0, "clamped, not wrapped");

        input.move_right(true, false);
        assert_eq!(input.caret(), 2, "end of AB");
    }

    #[test]
    fn ctrl_w_deletes_one_field_not_the_whole_search() {
        let mut input = typed("AB-C-0704");
        assert!(input.delete_prev_field());
        assert_eq!(input.text(), "AB-C-");
        assert!(input.delete_prev_field());
        assert_eq!(input.text(), "AB-");
    }

    #[test]
    fn a_double_click_selects_the_whole_search_not_one_field() {
        let mut input = typed("AB-C-0704");
        input.select_word_at(4);
        assert_eq!(input.selected_text(), Some("AB-C-0704"));
    }

    #[test]
    fn a_double_click_stops_at_whitespace() {
        let mut input = typed("ab cd ef");
        input.select_word_at(4);
        assert_eq!(input.selected_text(), Some("cd"));
    }

    #[test]
    fn selecting_everything_in_an_empty_line_selects_nothing() {
        let mut input = Input::new();
        input.select_all();
        assert!(!input.has_selection());
        assert_eq!(input.selected_text(), None);
    }

    #[test]
    fn the_caret_never_splits_a_multi_byte_character() {
        // A caret mid-character would panic the next time the line is sliced.
        let mut input = typed("café-Ω");
        for _ in 0..10 {
            input.move_left(false, false);
            assert!(input.text().is_char_boundary(input.caret()));
        }
        for _ in 0..10 {
            input.move_right(false, false);
            assert!(input.text().is_char_boundary(input.caret()));
        }
        input.set_caret(3, false);
        assert!(input.text().is_char_boundary(input.caret()));
    }

    #[test]
    fn a_click_past_the_end_lands_at_the_end() {
        let mut input = typed("abc");
        let byte = input.byte_at_column(99);
        input.set_caret(byte, false);
        assert_eq!(input.caret(), 3);
    }

    #[test]
    fn columns_and_bytes_agree_across_multi_byte_text() {
        let input = typed("aΩb");
        assert_eq!(input.char_len(), 3);
        assert_eq!(input.byte_at_column(0), 0);
        assert_eq!(input.byte_at_column(1), 1);
        assert_eq!(input.byte_at_column(2), 3);
        assert_eq!(input.column_of(3), 2);
        assert_eq!(input.caret_column(), 3);
    }

    #[test]
    fn an_over_long_paste_is_truncated_rather_than_refused() {
        let mut input = Input::new();
        let huge = "x".repeat(MAX_LEN * 2);
        input.insert_str(&huge);
        assert_eq!(input.text().len(), MAX_LEN);
        assert!(!input.insert_str("more"), "no room left, so no change");
    }

    #[test]
    fn recalling_a_history_entry_replaces_the_line_and_drops_the_selection() {
        let mut input = typed("abc");
        input.select_all();
        input.set_text("AB-C-0704");
        assert_eq!(input.text(), "AB-C-0704");
        assert_eq!(input.caret(), 9);
        assert!(!input.has_selection());
    }

    #[test]
    fn an_empty_selection_is_reported_as_no_selection() {
        let mut input = typed("abc");
        input.move_left(false, true);
        input.move_right(false, true);
        assert_eq!(input.selection(), None, "anchor met the caret again");
    }

    #[test]
    fn a_pasted_search_loses_its_newline_and_keeps_its_content() {
        assert_eq!(sanitize("AB-C-0704\r\n"), "AB-C-0704");
        assert_eq!(sanitize("  AB-C-0704  "), "AB-C-0704");
        assert_eq!(sanitize("AB-C\n0704"), "AB-C0704");
        assert_eq!(sanitize("\u{7}\u{1b}"), "");
    }

    #[test]
    fn it_still_reads_as_a_string_at_the_old_call_sites() {
        let input = typed("AB-C-0704");
        assert_eq!(input, "AB-C-0704");
        assert_eq!(input.chars().count(), 9);
        assert!(!input.is_empty());
    }
}
