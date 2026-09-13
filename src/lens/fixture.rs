//! A hardcoded screen's worth of text, for building the overlay against.
//!
//! Point 2 of the specification: the overlay is built against a fixed list of
//! rectangles and strings before any recognition exists, because hit-testing,
//! drag selection and cursor swapping are the hard parts and have to work in
//! isolation first.
//!
//! This stays in the tree afterwards. `lens --fixture` is how selection
//! behaviour is exercised by hand without a capture, and it is the only source
//! of a [`BoxMap`] on a machine with no screen - which is most of the machines
//! this crate's tests run on.
//!
//! # The numbers are not arbitrary
//!
//! Glyphs are sixteen pixels tall and ten wide, laid out on a twenty-four pixel
//! leading. That is ordinary Windows UI text at 100%, and it is deliberately at
//! the small end: point 6 warns that a recogniser tested on document scans
//! looks fine and then fails on screen text, because screen glyphs run twelve
//! to sixteen pixels tall where the model expects about forty-eight. A fixture
//! built at scan sizes would hide exactly the problem the upscaler exists to
//! solve.

use crate::lens::overlay::hit::{BoxMap, Line, Word};
use crate::lens::px::{Point, Rect, Screen};

/// The nominal glyph box of the fixture text.
const GLYPH_W: i32 = 10;
const GLYPH_H: i32 = 16;
/// Baseline to baseline. Half again the glyph height, as UI text is set.
const LEADING: i32 = 24;

/// A drawing's title block, as it would be recognised off the screen.
///
/// Shaped like the thing this feature exists for: a job code that is a run of
/// digits and dashes inside a longer line, beside labels that are not worth
/// selecting, with a revision and a date that are. Selecting the code and
/// nothing else is the gesture the whole overlay is for, so it must be possible
/// to test it.
pub fn title_block(origin: Point<Screen>) -> BoxMap {
    rows(
        origin,
        &[
            &["DRAWING", "No.", "11-D-0704"],
            &["TITLE", "GENERAL", "ARRANGEMENT"],
            &["REV", "B", "ISSUED", "2026-04-17"],
            &["SCALE", "1:50", "SHEET", "3", "OF", "12"],
            &["café", "Ω", "\u{2014}", "not", "ASCII"],
        ],
    )
}

/// Two lines far apart, for checking that a drag between distant boxes behaves.
pub fn scattered(origin: Point<Screen>) -> BoxMap {
    let a = Line::new(vec![word(origin, "TOP")]);
    let far = Point::new(origin.x + 600, origin.y + 400);
    let b = Line::new(vec![word(far, "BOTTOM")]);
    BoxMap::new(vec![a, b])
}

/// Lays out rows of words, one row per line, a single space between each.
///
/// Public because the overlay's own tests want fixtures of their own shape, and
/// because writing the arithmetic twice is how the two drift apart.
pub fn rows(origin: Point<Screen>, text: &[&[&str]]) -> BoxMap {
    let lines = text
        .iter()
        .enumerate()
        .map(|(row, words)| {
            let y = origin.y + row as i32 * LEADING;
            let mut x = origin.x;
            let words = words
                .iter()
                .map(|w| {
                    let word = word(Point::new(x, y), w);
                    // One glyph of space between words, so the gap is the same
                    // width as the separator character the caret arithmetic
                    // puts there.
                    x = word.rect.right + GLYPH_W;
                    word
                })
                .collect();
            Line::new(words)
        })
        .collect();
    BoxMap::new(lines)
}

/// One word, sized as if it had been set in the fixture's font.
fn word(at: Point<Screen>, text: &str) -> Word {
    let width = GLYPH_W * text.chars().count() as i32;
    Word::new(Rect::at(at, width, GLYPH_H), text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lens::overlay::hit::Caret;

    fn block() -> BoxMap {
        title_block(Point::new(100, 100))
    }

    #[test]
    fn the_title_block_reads_the_way_it_was_written() {
        let m = block();
        assert_eq!(m.lines[0].text(), "DRAWING No. 11-D-0704");
        assert_eq!(m.lines[2].text(), "REV B ISSUED 2026-04-17");
        assert_eq!(m.lines.len(), 5);
    }

    /// Screen sizes, not scan sizes. A fixture that drifted upwards would hide
    /// the problem point 6 exists to solve.
    #[test]
    fn the_glyphs_are_screen_sized_rather_than_scan_sized() {
        for line in &block().lines {
            assert!(
                (12..=16).contains(&line.rect.height()),
                "a line {} pixels tall is not screen text",
                line.rect.height()
            );
        }
    }

    /// The gesture the whole feature exists for: put the pointer on a job code
    /// in the middle of a line, drag to its end, and get the code alone.
    #[test]
    fn a_job_code_can_be_selected_out_of_the_middle_of_a_line() {
        let m = block();
        let line = &m.lines[0];
        let start = line.x_of("DRAWING No. ".chars().count());
        let end = line.rect.right;

        let a = m.caret_at(Point::new(start, line.rect.top + 8)).unwrap();
        let b = m.nearest_caret(Point::new(end, line.rect.top + 8)).unwrap();
        assert_eq!(m.run_between(a, b), "11-D-0704");
    }

    #[test]
    fn every_caret_in_the_fixture_round_trips_through_its_x() {
        for m in [block(), scattered(Point::new(0, 0))] {
            for line in &m.lines {
                for ch in 0..=line.chars() {
                    assert_eq!(line.caret_for_x(line.x_of(ch)), ch, "{:?}", line.text());
                }
            }
        }
    }

    /// Non-ASCII is in the fixture on purpose: a caret that indexed bytes would
    /// pass every other test here and split a code point on this line.
    #[test]
    fn the_fixture_contains_text_that_is_not_ascii() {
        let m = block();
        let line = m.lines.iter().find(|l| l.text().contains('é')).unwrap();
        assert!(line.text().chars().count() < line.text().len());
    }

    #[test]
    fn a_drag_between_two_distant_boxes_selects_both() {
        let m = scattered(Point::new(0, 0));
        let all = m.run_between(Caret::new(0, 0), Caret::new(1, 6));
        assert_eq!(all, "TOP\nBOTTOM");
    }
}
