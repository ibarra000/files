//! Turning a pointer position into a caret, and two carets into a selection.
//!
//! Pure, and the reason point 2 of the specification insists the overlay is
//! built against hardcoded rectangles first: this is the hard part, it has
//! nothing to do with recognition, and every bug in it is reachable from a unit
//! test. Once a [`BoxMap`] exists it makes no difference whether it came from a
//! recogniser or from [`crate::lens::fixture`].
//!
//! # Carets, not boxes
//!
//! A selection is a pair of positions *between* characters, not a set of boxes.
//! That is what makes dragging backwards, dragging past the end of a line, and
//! dragging off the bottom of the window all fall out of the same arithmetic
//! rather than each needing a case.
//!
//! # Characters, from word boxes
//!
//! No recogniser this will be given reports per-character boxes;
//! `Windows.Media.Ocr` reports words, and so do the ONNX models. So a position
//! inside a word is interpolated linearly across its rectangle. That is exactly
//! right for a monospaced screen font and slightly wrong for a proportional
//! one, where it can land the caret one character off inside a long word. The
//! alternative is measuring glyphs we do not have, and being one character out
//! at the point of a drag the user is still adjusting is not a defect anybody
//! can perceive.

use crate::lens::px::{Point, Rect, Screen};

/// How far outside a line box still counts as being on it.
///
/// Recognised line boxes are tight around the ink, so without this the I-beam
/// flickers off in the gap between two lines of a paragraph and a drag loses
/// its head every time it crosses one. Four physical pixels is under half the
/// leading of the smallest screen text worth reading.
pub const GRIP_PX: i32 = 4;

/// One recognised word, and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub rect: Rect<Screen>,
    pub text: String,
}

impl Word {
    pub fn new(rect: Rect<Screen>, text: impl Into<String>) -> Self {
        Self {
            rect,
            text: text.into(),
        }
    }

    /// Characters, not bytes. Every offset in this module is a character
    /// offset, because a byte offset into recognised text would put a caret in
    /// the middle of a multi-byte character the first time somebody selected an
    /// accented name.
    pub fn chars(&self) -> usize {
        self.text.chars().count()
    }
}

/// One recognised line: a run of words that reads left to right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub rect: Rect<Screen>,
    pub words: Vec<Word>,
}

impl Line {
    /// Builds a line and derives its rectangle from the words in it.
    pub fn new(words: Vec<Word>) -> Self {
        let rect = words
            .iter()
            .fold(Rect::new(0, 0, 0, 0), |acc, w| acc.union(w.rect));
        Self { rect, words }
    }

    /// The line as one string, words joined by a single space.
    ///
    /// The join is what every offset here indexes, so it is computed the same
    /// way everywhere rather than being passed around.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, w) in self.words.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&w.text);
        }
        out
    }

    /// How many characters [`Line::text`] has, without building it.
    pub fn chars(&self) -> usize {
        let letters: usize = self.words.iter().map(Word::chars).sum();
        letters + self.words.len().saturating_sub(1)
    }

    /// The character offset at which word `i` starts in [`Line::text`].
    fn word_start(&self, i: usize) -> usize {
        self.words[..i].iter().map(|w| w.chars() + 1).sum()
    }

    /// Where a caret sitting at character offset `ch` would be drawn.
    ///
    /// Clamped, so an offset past the end lands on the right edge rather than
    /// panicking - which matters because a drag head is routinely past the end
    /// of the line it started on.
    pub fn x_of(&self, ch: usize) -> i32 {
        let Some(first) = self.words.first() else {
            return self.rect.left;
        };
        let ch = ch.min(self.chars());
        for (i, w) in self.words.iter().enumerate() {
            let start = self.word_start(i);
            let len = w.chars();
            if ch <= start {
                return w.rect.left;
            }
            // `start + len` is the separating space itself, and it belongs at
            // the right edge of this word - so the range is inclusive, and the
            // offset after it is the next word's start, which the next turn of
            // the loop answers with `ch <= start`.
            if ch <= start + len {
                // Linear across the word. `len` cannot be zero here: a word
                // with no characters would have made `ch <= start` true above.
                let into = (ch - start) as i64;
                let span = w.rect.width() as i64;
                return w.rect.left + (into * span / len as i64) as i32;
            }
        }
        self.words.last().map_or(first.rect.left, |w| w.rect.right)
    }

    /// The character offset nearest to `x`.
    pub fn caret_for_x(&self, x: i32) -> usize {
        if self.words.is_empty() {
            return 0;
        }
        if x <= self.rect.left {
            return 0;
        }
        if x >= self.rect.right {
            return self.chars();
        }
        for (i, w) in self.words.iter().enumerate() {
            let start = self.word_start(i);
            let len = w.chars();
            if x < w.rect.left {
                // In the gap before this word. Whichever edge is nearer.
                let before_end = start.saturating_sub(1);
                let prev_x = self.x_of(before_end);
                return if x - prev_x <= w.rect.left - x {
                    before_end
                } else {
                    start
                };
            }
            if x < w.rect.right {
                let span = w.rect.width().max(1) as i64;
                let into = (x - w.rect.left) as i64;
                // Rounded rather than truncated, so the caret snaps to the
                // nearer side of a character instead of always its left edge.
                let ch = ((into * len as i64 * 2 + span) / (span * 2)) as usize;
                return start + ch.min(len);
            }
        }
        self.chars()
    }
}

/// A position between two characters of the map.
///
/// Ordered by line and then by offset, which is reading order, which is what
/// makes a backwards drag nothing more than a swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Caret {
    pub line: usize,
    pub ch: usize,
}

impl Caret {
    pub const fn new(line: usize, ch: usize) -> Self {
        Self { line, ch }
    }
}

/// Everything readable on screen right now, in reading order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoxMap {
    pub lines: Vec<Line>,
}

impl BoxMap {
    /// Builds a map, putting the lines into reading order.
    ///
    /// Top to bottom, then left to right within a band the height of the line
    /// itself. That is right for ordinary prose, for a form, and for a table
    /// read row-wise; it is wrong for newspaper columns, which need a layout
    /// analysis this does not attempt. Getting that wrong costs the order of a
    /// multi-line selection and nothing else - each line still reads correctly
    /// on its own.
    pub fn new(mut lines: Vec<Line>) -> Self {
        lines.retain(|l| !l.rect.is_empty() && !l.words.is_empty());
        lines.sort_by(|a, b| {
            let band = a.rect.height().max(b.rect.height()).max(1) / 2;
            if (a.rect.top - b.rect.top).abs() <= band {
                a.rect.left.cmp(&b.rect.left)
            } else {
                a.rect.top.cmp(&b.rect.top)
            }
        });
        Self { lines }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The line under `p`, allowing [`GRIP_PX`] of slack.
    ///
    /// This is what decides whether the I-beam is shown, and per point 3 it
    /// answers from detection alone - there is nothing here that needs a word
    /// to have been recognised.
    pub fn line_at(&self, p: Point<Screen>) -> Option<usize> {
        let grip = i64::from(GRIP_PX) * i64::from(GRIP_PX);
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.rect.distance2(p) <= grip)
            .min_by_key(|(_, l)| l.rect.distance2(p))
            .map(|(i, _)| i)
    }

    /// Whether the cursor should be an I-beam here.
    pub fn is_text_at(&self, p: Point<Screen>) -> bool {
        self.line_at(p).is_some()
    }

    /// The caret under `p`, or `None` when `p` is not on any line.
    ///
    /// For deciding where a drag *starts*. A press on empty space should begin
    /// no selection at all, rather than silently snapping to the nearest word
    /// somewhere else on the screen.
    pub fn caret_at(&self, p: Point<Screen>) -> Option<Caret> {
        let line = self.line_at(p)?;
        Some(Caret::new(line, self.lines[line].caret_for_x(p.x)))
    }

    /// The caret nearest `p`, wherever `p` is.
    ///
    /// For where a drag currently *is*. Once a drag has begun the head must
    /// always resolve to something, or the selection would flicker away every
    /// time the pointer crossed the gap between two lines, and dragging off the
    /// bottom of the window would not extend to the end.
    ///
    /// **The row is chosen before the column, and that is not the same as
    /// choosing the nearest box.** Straight-line distance picks whichever
    /// rectangle happens to be closest, so dragging to the bottom-right corner
    /// of the desktop selected to the end of the *widest* line rather than the
    /// last one - a short final line loses to a long one above it, because its
    /// right edge is further away. Every text control on the machine resolves
    /// the row first and the offset within it second, and so does this.
    pub fn nearest_caret(&self, p: Point<Screen>) -> Option<Caret> {
        let (i, line) = self.lines.iter().enumerate().min_by_key(|(_, l)| {
            let dy = if p.y < l.rect.top {
                i64::from(l.rect.top - p.y)
            } else if p.y >= l.rect.bottom {
                i64::from(p.y - l.rect.bottom + 1)
            } else {
                0
            };
            // Horizontal distance breaks ties only, which is what puts the
            // caret on the right one of two lines set side by side.
            let dx = if p.x < l.rect.left {
                i64::from(l.rect.left - p.x)
            } else if p.x >= l.rect.right {
                i64::from(p.x - l.rect.right + 1)
            } else {
                0
            };
            (dy, dx)
        })?;
        Some(Caret::new(i, line.caret_for_x(p.x)))
    }

    /// The selected text between two carets, in reading order.
    ///
    /// Lines are joined with a newline rather than a space: the text goes to
    /// the clipboard and into a search box, and a paragraph flattened onto one
    /// line is a worse answer than one that keeps its shape.
    pub fn run_between(&self, a: Caret, b: Caret) -> String {
        let (from, to) = order(a, b);
        if self.lines.is_empty() {
            return String::new();
        }
        let last = self.lines.len() - 1;
        let (from, to) = (clamp(from, self, last), clamp(to, self, last));

        if from == to {
            return String::new();
        }
        if from.line == to.line {
            return slice(&self.lines[from.line].text(), from.ch, to.ch);
        }

        let mut out = String::new();
        for line in from.line..=to.line {
            if line > from.line {
                out.push('\n');
            }
            let text = self.lines[line].text();
            let piece = if line == from.line {
                slice(&text, from.ch, self.lines[line].chars())
            } else if line == to.line {
                slice(&text, 0, to.ch)
            } else {
                text
            };
            out.push_str(&piece);
        }
        out
    }

    /// The rectangles to paint over the selection: at most one per line.
    pub fn highlight(&self, a: Caret, b: Caret) -> Vec<Rect<Screen>> {
        let (from, to) = order(a, b);
        if self.lines.is_empty() || from == to {
            return Vec::new();
        }
        let last = self.lines.len() - 1;
        let (from, to) = (clamp(from, self, last), clamp(to, self, last));

        (from.line..=to.line)
            .filter_map(|i| {
                let line = self.lines.get(i)?;
                let left = if i == from.line {
                    line.x_of(from.ch)
                } else {
                    line.rect.left
                };
                let right = if i == to.line {
                    line.x_of(to.ch)
                } else {
                    line.rect.right
                };
                let rect = Rect::new(
                    left.min(right),
                    line.rect.top,
                    left.max(right),
                    line.rect.bottom,
                );
                (!rect.is_empty()).then_some(rect)
            })
            .collect()
    }
}

fn order(a: Caret, b: Caret) -> (Caret, Caret) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Holds a caret inside the map, whatever a stale or extreme one claims.
fn clamp(c: Caret, map: &BoxMap, last: usize) -> Caret {
    let line = c.line.min(last);
    Caret::new(line, c.ch.min(map.lines[line].chars()))
}

/// `text[from..to]` in characters rather than bytes.
fn slice(text: &str, from: usize, to: usize) -> String {
    text.chars()
        .skip(from)
        .take(to.saturating_sub(from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two lines of ten-pixel characters, laid out the way screen text is:
    ///
    /// ```text
    ///   y 0..16   "JOB 11-D-0704"   x 100..
    ///   y 20..36  "REV B"           x 100..
    /// ```
    fn map() -> BoxMap {
        let w = |x: i32, y: i32, text: &str| {
            Word::new(
                Rect::new(x, y, x + 10 * text.chars().count() as i32, y + 16),
                text,
            )
        };
        BoxMap::new(vec![
            Line::new(vec![w(100, 0, "JOB"), w(140, 0, "11-D-0704")]),
            Line::new(vec![w(100, 20, "REV"), w(140, 20, "B")]),
        ])
    }

    #[test]
    fn a_line_reads_as_its_words_joined_by_spaces() {
        let m = map();
        assert_eq!(m.lines[0].text(), "JOB 11-D-0704");
        assert_eq!(m.lines[0].chars(), 13);
        assert_eq!(m.lines[1].text(), "REV B");
        assert_eq!(m.lines[1].chars(), 5);
    }

    /// The count must agree with the string, or every offset in the file is
    /// one out on any line with more than one word.
    #[test]
    fn the_character_count_agrees_with_the_text() {
        for line in &map().lines {
            assert_eq!(line.chars(), line.text().chars().count());
        }
    }

    #[test]
    fn lines_are_put_into_reading_order_however_they_arrive() {
        let w = |x: i32, y: i32, t: &str| Word::new(Rect::new(x, y, x + 30, y + 16), t);
        let m = BoxMap::new(vec![
            Line::new(vec![w(0, 40, "third")]),
            Line::new(vec![w(50, 0, "second")]),
            Line::new(vec![w(0, 0, "first")]),
        ]);
        let order: Vec<_> = m.lines.iter().map(|l| l.text()).collect();
        assert_eq!(order, ["first", "second", "third"]);
    }

    #[test]
    fn an_empty_or_wordless_line_is_dropped_rather_than_kept() {
        let m = BoxMap::new(vec![
            Line::new(vec![]),
            Line::new(vec![Word::new(Rect::new(0, 0, 0, 0), "")]),
            Line::new(vec![Word::new(Rect::new(0, 0, 10, 16), "a")]),
        ]);
        assert_eq!(m.lines.len(), 1);
    }

    #[test]
    fn the_i_beam_shows_over_text_and_not_beside_it() {
        let m = map();
        assert!(m.is_text_at(Point::new(105, 8)), "over the first word");
        assert!(m.is_text_at(Point::new(145, 28)), "over the second line");
        assert!(!m.is_text_at(Point::new(50, 8)), "left of everything");
        assert!(!m.is_text_at(Point::new(105, 200)), "far below");
        // The second line stops at x=150 where the first runs to 230, so this
        // is beside the text rather than on it - and the I-beam must not
        // appear over the empty space to the right of a short line.
        assert!(!m.is_text_at(Point::new(200, 28)), "right of a short line");
    }

    /// The grip. Without it the I-beam drops out in the gap between two lines
    /// of a paragraph, which reads as the feature flickering.
    #[test]
    fn the_gap_between_two_lines_still_counts_as_text() {
        let m = map();
        // y=18 is between the first line (ends 16) and the second (starts 20).
        assert!(m.is_text_at(Point::new(105, 18)));
        // But a long way below the last line is not text.
        assert!(!m.is_text_at(Point::new(105, 60)));
    }

    #[test]
    fn a_press_on_empty_space_starts_no_selection() {
        let m = map();
        assert_eq!(m.caret_at(Point::new(1000, 1000)), None);
        assert!(m.caret_at(Point::new(105, 8)).is_some());
    }

    /// But a drag already under way always has a head, or it would flicker.
    #[test]
    fn a_drag_head_resolves_anywhere_on_the_desktop() {
        let m = map();
        assert!(m.nearest_caret(Point::new(-5000, -5000)).is_some());
        assert!(m.nearest_caret(Point::new(5000, 5000)).is_some());
        assert_eq!(BoxMap::default().nearest_caret(Point::new(0, 0)), None);
    }

    /// Dragging off the bottom right extends to the very end of the text,
    /// which is what every other text control on the machine does.
    #[test]
    fn dragging_past_the_end_selects_to_the_end() {
        let m = map();
        let start = m.caret_at(Point::new(100, 8)).unwrap();
        let head = m.nearest_caret(Point::new(9999, 9999)).unwrap();
        assert_eq!(m.run_between(start, head), "JOB 11-D-0704\nREV B");
    }

    #[test]
    fn a_caret_at_the_left_edge_is_offset_zero_and_at_the_right_is_the_end() {
        let line = &map().lines[0];
        assert_eq!(line.caret_for_x(line.rect.left), 0);
        assert_eq!(line.caret_for_x(line.rect.left - 100), 0);
        assert_eq!(line.caret_for_x(line.rect.right), line.chars());
        assert_eq!(line.caret_for_x(line.rect.right + 100), line.chars());
    }

    /// The round trip that keeps a drag stable: the caret you get by clicking
    /// where a caret is drawn is that same caret.
    #[test]
    fn a_caret_position_round_trips_through_its_x() {
        let m = map();
        for (i, line) in m.lines.iter().enumerate() {
            for ch in 0..=line.chars() {
                let x = line.x_of(ch);
                let back = line.caret_for_x(x);
                assert_eq!(
                    back, ch,
                    "line {i} offset {ch} drew at x={x} and came back as {back}"
                );
            }
        }
    }

    #[test]
    fn selecting_one_word_gives_that_word() {
        let m = map();
        let a = Caret::new(0, 0);
        let b = Caret::new(0, 3);
        assert_eq!(m.run_between(a, b), "JOB");
    }

    /// A backwards drag is the same selection as a forwards one. This is the
    /// whole reason a selection is a pair of carets rather than a set of boxes.
    #[test]
    fn dragging_backwards_selects_the_same_text() {
        let m = map();
        let a = Caret::new(0, 4);
        let b = Caret::new(1, 3);
        assert_eq!(m.run_between(a, b), m.run_between(b, a));
        assert_eq!(m.run_between(b, a), "11-D-0704\nREV");
    }

    #[test]
    fn a_selection_that_has_not_moved_is_empty() {
        let m = map();
        let a = Caret::new(0, 5);
        assert_eq!(m.run_between(a, a), "");
        assert!(m.highlight(a, a).is_empty());
    }

    /// Lines keep their shape rather than being flattened, because the text
    /// goes to a clipboard and a search box.
    #[test]
    fn a_multi_line_selection_is_joined_with_newlines() {
        let m = map();
        let all = m.run_between(Caret::new(0, 0), Caret::new(1, 5));
        assert_eq!(all, "JOB 11-D-0704\nREV B");
    }

    #[test]
    fn a_highlight_covers_one_rectangle_per_line() {
        let m = map();
        let rects = m.highlight(Caret::new(0, 0), Caret::new(1, 5));
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].top, 0);
        assert_eq!(rects[1].top, 20);
        for r in rects {
            assert!(!r.is_empty());
        }
    }

    /// A stale caret from a previous capture must not panic the paint pass -
    /// point 9 means the box map is replaced under the selection routinely.
    #[test]
    fn a_caret_beyond_the_map_is_clamped_rather_than_fatal() {
        let m = map();
        let wild = Caret::new(99, 99);
        assert_eq!(
            m.run_between(Caret::new(0, 0), wild),
            "JOB 11-D-0704\nREV B"
        );
        assert_eq!(m.highlight(Caret::new(0, 0), wild).len(), 2);
    }

    #[test]
    fn an_empty_map_answers_everything_without_panicking() {
        let m = BoxMap::default();
        assert!(m.is_empty());
        assert_eq!(m.caret_at(Point::new(0, 0)), None);
        assert_eq!(m.run_between(Caret::new(0, 0), Caret::new(3, 3)), "");
        assert!(m.highlight(Caret::new(0, 0), Caret::new(3, 3)).is_empty());
    }

    /// Offsets are in characters, so a name with an accent in it selects the
    /// way it looks rather than splitting a code point.
    #[test]
    fn offsets_are_characters_not_bytes() {
        let m = BoxMap::new(vec![Line::new(vec![Word::new(
            Rect::new(0, 0, 60, 16),
            "café Ω",
        )])]);
        assert_eq!(m.lines[0].chars(), 6);
        assert_eq!(m.run_between(Caret::new(0, 0), Caret::new(0, 4)), "café");
        assert_eq!(m.run_between(Caret::new(0, 5), Caret::new(0, 6)), "Ω");
    }
}
