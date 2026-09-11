//! Drawing the search line.
//!
//! Two things the previous one-line `Paragraph` could not do. It had no way to
//! show a selection, because a `Paragraph` carries a single style for the
//! whole string; and it had no horizontal scroll, so a code longer than the
//! box simply ran through the border and the terminal caret was placed
//! somewhere outside the widget entirely.
//!
//! Both are handled here as pure functions of the line and the width
//! available, so the scroll arithmetic can be tested without a terminal.
//!
//! # Columns
//!
//! A column is a character, following the convention the rest of the crate
//! already uses to place things on screen. That is wrong for full-width CJK
//! text, which is equally wrong everywhere else in this program - job codes
//! are ASCII, and fixing it properly means a width table and a dependency.
//! The arithmetic is at least confined to this file.

use ratatui::text::{Line, Span};

use super::theme;
use crate::app::input::Input;

/// What to draw, and where the caret goes within the inner area.
pub struct View {
    pub line: Line<'static>,
    /// Column of the caret, relative to the inner area's left edge. Always
    /// inside `0..width`, so the caller can place the terminal cursor without
    /// clamping it again.
    pub caret_column: u16,
}

/// How far the line is scrolled, in columns.
///
/// Derived rather than stored - a remembered offset is how a text field ends
/// up scrolled somewhere the caret is not - and shared with mouse handling,
/// which has to undo exactly this to turn a click back into a position in the
/// text.
pub fn offset(input: &Input, width: u16) -> usize {
    // A zero-width inner area is possible in a very narrow terminal; treating
    // it as one column keeps the arithmetic honest without a special case in
    // every expression.
    let width = width.max(1) as usize;
    input.caret_column().saturating_sub(width.saturating_sub(1))
}

/// Builds the visible portion of the line, with the selection highlighted.
pub fn view(input: &Input, width: u16) -> View {
    let width = width.max(1) as usize;
    let caret_column = input.caret_column();
    let offset = offset(input, width as u16);

    let text = input.text();
    let start = input.byte_at_column(offset);
    let end = input.byte_at_column(offset + width);
    let Some(visible) = text.get(start..end) else {
        // Unreachable while the offsets come from `byte_at_column`, which
        // only ever returns boundaries. Degrading rather than slicing blind
        // is the same choice the results rows make.
        return View {
            line: Line::from(Span::styled(text.to_string(), theme::input())),
            caret_column: 0,
        };
    };

    let spans = match input.selection() {
        Some((lo, hi)) => {
            let lo = lo.clamp(start, end);
            let hi = hi.clamp(start, end);
            split(visible, lo - start, hi - start)
        }
        None => vec![Span::styled(visible.to_string(), theme::input())],
    };

    View {
        line: Line::from(spans),
        caret_column: (caret_column - offset) as u16,
    }
}

/// Prefix, highlighted run, suffix - the same shape the result rows use.
fn split(visible: &str, lo: usize, hi: usize) -> Vec<Span<'static>> {
    if lo >= hi || !visible.is_char_boundary(lo) || !visible.is_char_boundary(hi) {
        return vec![Span::styled(visible.to_string(), theme::input())];
    }
    let mut spans = Vec::with_capacity(3);
    if lo > 0 {
        spans.push(Span::styled(visible[..lo].to_string(), theme::input()));
    }
    spans.push(Span::styled(
        visible[lo..hi].to_string(),
        theme::text_selection(),
    ));
    if hi < visible.len() {
        spans.push(Span::styled(visible[hi..].to_string(), theme::input()));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> Input {
        Input::from(text)
    }

    fn rendered(view: &View) -> String {
        view.line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_short_code_is_shown_whole_with_the_caret_after_it() {
        let view = view(&input("11-D-0704"), 40);
        assert_eq!(rendered(&view), "11-D-0704");
        assert_eq!(view.caret_column, 9);
    }

    #[test]
    fn an_empty_line_renders_with_the_caret_at_the_left() {
        let view = view(&Input::new(), 40);
        assert_eq!(rendered(&view), "");
        assert_eq!(view.caret_column, 0);
    }

    #[test]
    fn a_code_longer_than_the_box_scrolls_to_keep_the_caret_visible() {
        // The old renderer ran the text through the border and placed the
        // terminal caret outside the widget.
        let long = "A".repeat(50);
        let view = view(&input(&long), 10);
        assert_eq!(rendered(&view).chars().count(), 10);
        assert!(
            view.caret_column < 10,
            "the caret must land inside the box, got {}",
            view.caret_column
        );
    }

    #[test]
    fn moving_back_through_a_long_code_scrolls_the_view_with_the_caret() {
        let mut line = input(&"ABCDEFGHIJKLMNOPQRSTUVWXYZ".repeat(2));
        for _ in 0..40 {
            line.move_left(false, false);
        }
        let view = view(&line, 10);
        assert!(view.caret_column < 10);
        let shown = rendered(&view);
        assert_eq!(shown.chars().count(), 10);
        assert!(
            line.text().contains(&shown),
            "the visible run must be a slice of the line"
        );
    }

    #[test]
    fn the_selected_run_is_a_span_of_its_own() {
        let mut line = input("11-D-0704");
        line.move_left(false, true);
        line.move_left(false, true);

        let view = view(&line, 40);
        assert_eq!(rendered(&view), "11-D-0704");

        let selected: String = view
            .line
            .spans
            .iter()
            .filter(|s| s.style == theme::text_selection())
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(selected, "04");
    }

    #[test]
    fn a_selection_running_off_the_left_edge_is_clipped_not_panicked() {
        let mut line = input(&"A".repeat(50));
        line.select_all();
        let view = view(&line, 10);
        assert_eq!(rendered(&view).chars().count(), 10);
        assert!(
            view.line
                .spans
                .iter()
                .any(|s| s.style == theme::text_selection())
        );
    }

    #[test]
    fn selecting_everything_highlights_everything() {
        let mut line = input("11-D");
        line.select_all();
        let view = view(&line, 40);
        assert_eq!(view.line.spans.len(), 1);
        assert_eq!(view.line.spans[0].style, theme::text_selection());
    }

    #[test]
    fn a_selection_in_the_middle_produces_three_spans() {
        let mut line = input("abcdef");
        line.move_home(false);
        line.move_right(false, false);
        line.move_right(false, true);
        line.move_right(false, true);
        let view = view(&line, 40);
        assert_eq!(view.line.spans.len(), 3);
        assert_eq!(view.line.spans[1].content.as_ref(), "bc");
    }

    #[test]
    fn multi_byte_text_never_splits_a_character() {
        // Slicing a scrolled view at a byte offset derived from a column is
        // exactly where a panic would hide.
        let line = input(&"Ωé".repeat(20));
        for width in 1..30u16 {
            let view = view(&line, width);
            assert!(view.caret_column < width.max(1));
        }
    }

    #[test]
    fn a_one_column_box_still_renders() {
        let view = view(&input("abc"), 1);
        assert_eq!(view.caret_column, 0);
    }
}
