//! Keyboard navigation over results laid out several to a line.
//!
//! Ranks are row-major (rank 0 top left, rank 1 beside it). Up and Down keep
//! the column; Tab and Shift+Tab move within the line. With `columns == 1`
//! every function reduces to plain single-list behaviour.

/// Returns the rank Up or Down lands on from `rank` in a list of `len`.
///
/// Moves one line within the same column and wraps to the other end of that
/// column. Returns `rank` unchanged for an empty list or zero columns.
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
        // rank actually in it.
        column + (len - 1 - column) / columns * columns
    }
}

/// Returns the rank Tab (`forward`) or Shift+Tab lands on, or `None` at the
/// end of the line.
///
/// Does not wrap onto the next line; changing lines is Up and Down's job.
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

    #[test]
    fn one_column_steps_and_wraps_like_a_plain_list() {
        assert_eq!(vertical(0, 5, 1, true), 1);
        assert_eq!(vertical(4, 5, 1, true), 0);
        assert_eq!(vertical(0, 5, 1, false), 4);
        assert_eq!(vertical(3, 5, 1, false), 2);
    }

    #[test]
    fn one_column_has_no_across() {
        assert_eq!(across(2, 5, 1, true), None);
        assert_eq!(across(2, 5, 1, false), None);
    }

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

    #[test]
    fn down_off_the_foot_of_a_column_wraps_to_its_head() {
        assert_eq!(vertical(7, 8, 3, true), 1);
        assert_eq!(vertical(6, 8, 3, true), 0);
        // The third column is a line shorter; its foot is 5.
        assert_eq!(vertical(5, 8, 3, true), 2);
    }

    /// On a short last line, a column's foot is on the line above.
    #[test]
    fn up_off_the_head_of_a_column_wraps_to_its_foot() {
        assert_eq!(vertical(0, 8, 3, false), 6);
        assert_eq!(vertical(1, 8, 3, false), 7);
        assert_eq!(vertical(2, 8, 3, false), 5);
    }

    #[test]
    fn tab_steps_along_the_line_and_holds_at_its_end() {
        assert_eq!(across(3, 8, 3, true), Some(4));
        assert_eq!(across(4, 8, 3, true), Some(5));
        assert_eq!(across(5, 8, 3, true), None);
    }

    #[test]
    fn shift_tab_steps_back_and_holds_at_the_first_column() {
        assert_eq!(across(5, 8, 3, false), Some(4));
        assert_eq!(across(4, 8, 3, false), Some(3));
        assert_eq!(across(3, 8, 3, false), None);
    }

    #[test]
    fn tab_holds_at_the_end_of_a_short_last_line() {
        assert_eq!(across(6, 8, 3, true), Some(7));
        assert_eq!(across(7, 8, 3, true), None);
    }

    #[test]
    fn an_empty_list_goes_nowhere() {
        assert_eq!(vertical(0, 0, 3, true), 0);
        assert_eq!(across(0, 0, 3, true), None);
        assert_eq!(vertical(0, 4, 0, true), 0);
    }
}
