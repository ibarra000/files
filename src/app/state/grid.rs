//! Moving through results laid out several to a line.
//!
//! Row-major - rank 0 top left, rank 1 beside it - so the best match is still
//! the first thing read and the list still scrolls by whole lines. Up and Down
//! stay in the column they are in; Tab and Shift+Tab step along the line and
//! keep to it. Being a column over is therefore always a matter of being in
//! the same place one column over, which is the promise the keys make.
//!
//! Pure arithmetic over ranks, so it can be tested without a state machine,
//! and so that one column is visibly the list it always was: every function
//! here with `columns == 1` is the single-column behaviour exactly.

/// Where Up or Down lands, from `rank` in a list of `len`.
///
/// One line, in the same column. Off either end it wraps to the other end of
/// *that column*, which with one column is the whole list's wrap - Up from the
/// first row to the last - and with more is the same idea kept to a column,
/// so wrapping never moves the highlight sideways.
///
/// # Panics
///
/// Never: callers hold `rank < len`, and an empty list or zero columns is
/// answered with `rank` rather than a division by zero.
pub fn vertical(rank: usize, len: usize, columns: usize, down: bool) -> usize {
    if len == 0 || columns == 0 {
        return rank;
    }
    let column = rank % columns;
    if down {
        let next = rank + columns;
        if next < len { next } else { column }
    } else if rank >= columns {
        rank - columns
    } else {
        // The last line may be short, so the foot of a column is the last
        // rank in it rather than the last line's slot for it.
        column + (len - 1 - column) / columns * columns
    }
}

/// Where Tab (`forward`) or Shift+Tab lands, or `None` at the end of the line.
///
/// Held at the ends rather than wrapped onto the next line: the next line is
/// Down's to reach, and a Tab that sometimes moved down would make it a key
/// whose effect depends on which column you happen to be in.
pub fn across(rank: usize, len: usize, columns: usize, forward: bool) -> Option<usize> {
    if columns <= 1 || rank >= len {
        return None;
    }
    let column = rank % columns;
    if forward {
        (column + 1 < columns && rank + 1 < len).then_some(rank + 1)
    } else {
        (column > 0).then(|| rank - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One column is the list it always was: a step, and a wrap at each end.
    #[test]
    fn one_column_steps_and_wraps_like_a_plain_list() {
        assert_eq!(vertical(0, 5, 1, true), 1);
        assert_eq!(vertical(4, 5, 1, true), 0);
        assert_eq!(vertical(0, 5, 1, false), 4);
        assert_eq!(vertical(3, 5, 1, false), 2);
    }

    /// And Tab has nowhere to go in it.
    #[test]
    fn one_column_has_no_across() {
        assert_eq!(across(2, 5, 1, true), None);
        assert_eq!(across(2, 5, 1, false), None);
    }

    /// Down and Up move a line and keep the column.
    ///
    /// ```text
    ///  0  1  2
    ///  3  4  5
    ///  6  7
    /// ```
    #[test]
    fn up_and_down_keep_to_their_column() {
        assert_eq!(vertical(1, 8, 3, true), 4);
        assert_eq!(vertical(4, 8, 3, true), 7);
        assert_eq!(vertical(7, 8, 3, false), 4);
        assert_eq!(vertical(4, 8, 3, false), 1);
    }

    /// Off the foot of a column is its head, not the head of the list.
    #[test]
    fn down_off_the_foot_of_a_column_wraps_to_its_head() {
        assert_eq!(vertical(7, 8, 3, true), 1);
        assert_eq!(vertical(6, 8, 3, true), 0);
        // The third column is a line shorter; its foot is 5.
        assert_eq!(vertical(5, 8, 3, true), 2);
    }

    /// And off the head is its foot - which, on a short last line, is the
    /// line above.
    #[test]
    fn up_off_the_head_of_a_column_wraps_to_its_foot() {
        assert_eq!(vertical(0, 8, 3, false), 6);
        assert_eq!(vertical(1, 8, 3, false), 7);
        assert_eq!(vertical(2, 8, 3, false), 5);
    }

    /// Tab keeps the line: the cell beside, and nothing past the last one.
    #[test]
    fn tab_steps_along_the_line_and_holds_at_its_end() {
        assert_eq!(across(3, 8, 3, true), Some(4));
        assert_eq!(across(4, 8, 3, true), Some(5));
        assert_eq!(across(5, 8, 3, true), None);
    }

    /// Shift+Tab is the same in the other direction.
    #[test]
    fn shift_tab_steps_back_and_holds_at_the_first_column() {
        assert_eq!(across(5, 8, 3, false), Some(4));
        assert_eq!(across(4, 8, 3, false), Some(3));
        assert_eq!(across(3, 8, 3, false), None);
    }

    /// A short last line has an end of its own.
    #[test]
    fn tab_holds_at_the_end_of_a_short_last_line() {
        assert_eq!(across(6, 8, 3, true), Some(7));
        assert_eq!(across(7, 8, 3, true), None);
    }

    /// Nothing to move through is not a crash.
    #[test]
    fn an_empty_list_goes_nowhere() {
        assert_eq!(vertical(0, 0, 3, true), 0);
        assert_eq!(across(0, 0, 3, true), None);
        assert_eq!(vertical(0, 4, 0, true), 0);
    }
}
