//! The vocabulary the interaction model is written in.
//!
//! Plain data, split out from the transition itself so `mod.rs` stays about
//! behaviour. Nothing here has any logic beyond a predicate or two, and
//! nothing here knows about events, commands or the clock.

use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;

/// How far the current query has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPhase {
    /// Nothing typed.
    Idle,
    TooShort {
        need: usize,
    },
    /// Typed, but it matches no known job-code pattern.
    Unresolvable,
    /// Dispatched to the matcher. Sub-millisecond, so rarely rendered.
    LocalPending,
    /// Showing results from the in-memory index.
    Local,
    /// Local results shown while the server is being consulted.
    Verifying {
        since: Instant,
    },
    /// Confirmed against the server, or proven current by an unchanged
    /// directory stamp.
    Verified {
        took: Duration,
        by_stamp: bool,
    },
    /// Verification failed; local results remain on screen.
    VerifyFailed {
        detail: String,
    },
}

impl QueryPhase {
    pub fn is_verifying(&self) -> bool {
        matches!(self, Self::Verifying { .. })
    }
}

/// Why the result list is empty.
///
/// The results widget takes this rather than an empty slice, so a blank list
/// always carries a reason. The previous implementation could show nothing at
/// all when the drive was unreachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmptyReason {
    NoQuery,
    QueryTooShort { need: usize },
    NoPathPattern,
    NoMatches { searched: u32 },
    IndexUnavailable { detail: String },
    PathNotFound { dir: PathBuf },
    AccessDenied { dir: PathBuf },
    NotSearchedYet,
}

/// What the arrow keys act on.
///
/// Up used to mean one thing because there was only one thing it could mean.
/// Now it has to choose between recalling a code and walking the results, and
/// an explicit focus is how that choice stays predictable instead of being
/// inferred from whichever flags happen to be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// Typing. Up opens recall, Down steps into the results.
    #[default]
    Input,
    /// Walking the result list. Up on the top row goes back to the input.
    Results,
    /// Walking previously used codes.
    History,
}

/// The shape of the results grid.
///
/// Results flow *down* each column and then across, newspaper-style, so Down
/// is simply the next rank and wraps from the foot of one column to the head
/// of the next. Left and Right move by a whole column, which is why navigation
/// needs the column height and therefore needs this at all.
///
/// Derived from the terminal rather than stored, by [`Grid::for_pane`], so a
/// resize cannot leave the selection arithmetic disagreeing with what was
/// drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    columns: usize,
    rows: usize,
}

impl Default for Grid {
    /// What an 80x24 terminal yields, matching `AppState`'s default area. The
    /// state machine answers keys before the first frame is ever drawn, and in
    /// tests it is driven with no terminal at all, so "no geometry seen yet"
    /// has to mean something ordinary rather than nothing.
    fn default() -> Self {
        Self::new(crate::config::GRID_COLUMNS, 14)
    }
}

impl Grid {
    /// Both dimensions are floored at one. A terminal can be short enough that
    /// the results pane has no interior at all, and every navigation step
    /// divides by these - a zero-sized page is a panic, not a layout.
    pub const fn new(columns: usize, rows: usize) -> Self {
        Self {
            columns: if columns == 0 { 1 } else { columns },
            rows: if rows == 0 { 1 } else { rows },
        }
    }

    /// Derives the grid from the results pane, whose border is not usable.
    pub fn for_pane(pane: Rect) -> Self {
        let inner_w = pane.width.saturating_sub(2);
        let inner_h = pane.height.saturating_sub(2);
        let columns = (inner_w / crate::config::MIN_COLUMN_WIDTH) as usize;
        Self::new(columns.min(crate::config::GRID_COLUMNS), inner_h as usize)
    }

    pub const fn columns(self) -> usize {
        self.columns
    }

    pub const fn rows(self) -> usize {
        self.rows
    }

    /// Ranks visible at once.
    pub const fn page_len(self) -> usize {
        self.columns * self.rows
    }

    /// Half-open range of ranks on the page holding `rank`.
    pub fn page(self, rank: usize, len: usize) -> Range<usize> {
        let start = (rank / self.page_len()) * self.page_len();
        start..(start + self.page_len()).min(len)
    }

    /// Column and row of `rank` within its own page, counting down first.
    pub const fn slot(self, rank: usize) -> (usize, usize) {
        let within = rank % self.page_len();
        (within / self.rows, within % self.rows)
    }
}

/// A transient message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// How long a transient message stays on screen.
pub const TOAST_LIFETIME: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    /// The floor that keeps `page_len` from being zero. A short terminal
    /// leaves the results pane no interior at all, and every navigation step
    /// divides by these.
    #[test]
    fn a_grid_with_no_room_still_has_one_cell() {
        let g = Grid::new(0, 0);
        assert_eq!(g.columns(), 1);
        assert_eq!(g.rows(), 1);
        assert_eq!(g.page_len(), 1);
        assert_eq!(g.page(0, 0), 0..0);
    }

    #[test]
    fn a_tiny_pane_collapses_to_a_single_column() {
        // 12x8 is the smallest terminal the render tests exercise.
        assert_eq!(Grid::for_pane(Rect::new(0, 0, 10, 0)).columns(), 1);
        assert_eq!(Grid::for_pane(Rect::new(0, 0, 10, 0)).rows(), 1);
    }

    #[test]
    fn columns_appear_only_when_they_would_be_readable() {
        let width = |w: u16| Grid::for_pane(Rect::new(0, 0, w, 20)).columns();
        // +2 for the border the pane spends before any text is drawn.
        assert_eq!(width(crate::config::MIN_COLUMN_WIDTH + 2), 1);
        assert_eq!(width(crate::config::MIN_COLUMN_WIDTH * 2 + 2), 2);
        assert_eq!(width(crate::config::MIN_COLUMN_WIDTH * 3 + 2), 3);
        assert_eq!(
            width(crate::config::MIN_COLUMN_WIDTH * 10 + 2),
            crate::config::GRID_COLUMNS,
            "never more columns than configured, however wide"
        );
    }

    /// Ranks run down a column before moving across, newspaper-style.
    #[test]
    fn ranks_flow_down_each_column_before_moving_right() {
        let g = Grid::new(3, 4);
        assert_eq!(g.page_len(), 12);
        assert_eq!(g.slot(0), (0, 0), "first rank, top of column one");
        assert_eq!(g.slot(3), (0, 3), "foot of column one");
        assert_eq!(g.slot(4), (1, 0), "wraps to the head of column two");
        assert_eq!(g.slot(11), (2, 3), "foot of the last column");
        assert_eq!(g.slot(12), (0, 0), "and over onto the next page");
    }

    #[test]
    fn a_page_holds_the_rank_that_selected_it() {
        let g = Grid::new(3, 4);
        for rank in [0usize, 5, 11, 12, 23, 24, 99] {
            let page = g.page(rank, 100);
            assert!(
                page.contains(&rank),
                "rank {rank} fell outside its own page {page:?}"
            );
            assert_eq!(page.start % g.page_len(), 0, "pages start on a boundary");
        }
    }

    #[test]
    fn the_last_page_is_clipped_to_the_results_that_exist() {
        let g = Grid::new(3, 4);
        assert_eq!(g.page(12, 14), 12..14, "not 12..24");
    }
}
