//! Drawing the pane that says what a result is.
//!
//! The words are [`crate::view::preview`]; this decides where they go, in the
//! two shapes the panel has room for.
//!
//! # A pane and a popup, from one set of lines
//!
//! Both layouts draw the same [`Row`]s in the same order and differ only in the
//! rectangle they are given. That is deliberate: two renderings would be two
//! places for the wording to drift, and the whole reason `view` exists is that
//! the wording is the part worth keeping.
//!
//! * [`Layout::Pane`] gets a fixed column down the right of the panel, present
//!   whether or not anything is under the pointer. Fixed, because a pane that
//!   came and went would change the window's width as the pointer moved.
//! * [`Layout::List`] gets a card drawn *over* the lower rows, only while
//!   something is hovered. Over rather than below, because the panel is the
//!   window: anything drawn past its edge is clipped, and anything that made
//!   the window taller would be the height jitter `gui::frame` refuses.
//!
//! # Why the popup covers the rows it does
//!
//! It is anchored under the row it describes and clipped to the list, so the
//! row itself is never covered by its own description - which would leave
//! somebody reading facts about a file they can no longer see the name of.

use eframe::egui::{Rect, Ui, pos2, vec2};

use crate::app::state::AppState;
use crate::gui::theme::{self, Theme};
use crate::view::Emphasis;
use crate::view::preview::{Line, Row};

/// Height of one line in the pane.
///
/// Tighter than `ROW_H`, because these are lines of prose rather than rows to
/// be clicked: the pane needs to fit nine of them beside eight results.
const LINE_H: f32 = 22.0;

/// The gap under the name, and under the last fact.
const GROUP_GAP: f32 = 8.0;

/// How wide the popup is in [`Layout::List`].
///
/// The pane's width, so the two layouts wrap their text at the same place and a
/// line that fits one fits the other.
pub const POPUP_W: f32 = theme::PREVIEW_W;

/// Which rectangle the pane occupies, given the whole panel.
///
/// Returns `None` in [`Layout::List`], which has no pane - only a popup, which
/// is placed against a row instead.
pub fn pane_rect(panel: Rect, layout: theme::Layout, body: Rect) -> Option<Rect> {
    if !layout.has_pane() {
        return None;
    }
    let left = panel.right() - theme::PREVIEW_W;
    Some(Rect::from_min_max(
        pos2(left, body.top()),
        pos2(panel.right(), body.bottom()),
    ))
}

/// How much width the list gives up to the pane.
pub fn list_inset(layout: theme::Layout) -> f32 {
    if layout.has_pane() {
        theme::PREVIEW_W
    } else {
        0.0
    }
}

/// Draws the pane in [`Layout::Pane`].
pub fn draw_pane(
    ui: &Ui,
    state: &AppState,
    theme: &Theme,
    rect: Rect,
    wall: std::time::SystemTime,
) {
    // A rule rather than a panel of its own: the pane is part of the same
    // surface, and a second background would read as a second window.
    let painter = ui.painter();
    painter.vline(
        rect.left(),
        rect.top()..=rect.bottom(),
        eframe::egui::Stroke::new(1.0, theme.edge),
    );

    let inner = Rect::from_min_max(
        pos2(rect.left() + theme::PAD_X, rect.top() + theme::PAD_Y),
        pos2(rect.right() - theme::PAD_X, rect.bottom()),
    );
    draw_rows(ui, theme, inner, &rows_for(state, wall), "files-preview");
}

/// Draws the popup in [`Layout::List`], if anything is being pointed at.
///
/// `anchor` is the row the pointer is on, and `list` is the band the popup may
/// not leave.
pub fn draw_popup(
    ui: &Ui,
    state: &AppState,
    theme: &Theme,
    anchor: Rect,
    list: Rect,
    wall: std::time::SystemTime,
) {
    // Only for a real answer. The idle line belongs in a pane that is always
    // there; as a popup it would be a card that appears to tell you it has
    // nothing to say.
    let Some(preview) = &state.preview else {
        return;
    };
    let rows = crate::view::preview::rows(preview, wall);
    let wanted = height_of(&rows);

    // Whichever side of the row has more room, and never over the row itself:
    // facts about a file whose name has just been covered up are facts about
    // nothing.
    //
    // The card is shortened to fit rather than pushed over the anchor, which is
    // what an earlier version did - it placed the card above when it would not
    // fit below, clamped that to the top of the list, and on a short list the
    // clamp put it straight back over the row it was describing. `draw_rows`
    // drops whatever then falls off the bottom, and `view::preview` orders the
    // lines so that what goes is what mattered least.
    let room_below = list.bottom() - anchor.bottom();
    let room_above = anchor.top() - list.top();
    let (top, height) = if room_below >= wanted || room_below >= room_above {
        (anchor.bottom(), wanted.min(room_below))
    } else {
        let height = wanted.min(room_above);
        (anchor.top() - height, height)
    };

    // Nothing worth drawing a frame around. One line of a card is a border with
    // a sliver of text in it, which reads as a rendering fault.
    if height < LINE_H + theme::PAD_Y * 2.0 {
        return;
    }

    let left = (anchor.left() + theme::ROW_PAD_X).min(list.right() - POPUP_W);
    let card = Rect::from_min_size(pos2(left, top), vec2(POPUP_W, height));

    let painter = ui.painter();
    theme::raise(painter, theme, card, theme::ROW_RADIUS);
    painter.rect_filled(card, theme::radius(theme::ROW_RADIUS), theme.well);
    painter.rect_stroke(
        card,
        theme::radius(theme::ROW_RADIUS),
        eframe::egui::Stroke::new(1.0, theme.edge),
        eframe::egui::StrokeKind::Inside,
    );

    let inner = card.shrink2(vec2(theme::PAD_X, theme::PAD_Y));
    draw_rows(ui, theme, inner, &rows, "files-preview-popup");
}

/// The lines for whatever is under the pointer, or the idle sentence.
fn rows_for(state: &AppState, wall: std::time::SystemTime) -> Vec<Row> {
    match &state.preview {
        Some(preview) => crate::view::preview::rows(preview, wall),
        None => crate::view::preview::idle(),
    }
}

/// How tall a set of rows is, including the gaps between the groups.
fn height_of(rows: &[Row]) -> f32 {
    let lines = rows.len() as f32 * LINE_H;
    lines + gaps(rows) + theme::PAD_Y * 2.0
}

fn gaps(rows: &[Row]) -> f32 {
    rows.windows(2)
        .filter(|pair| group_of(pair[0].line) != group_of(pair[1].line))
        .count() as f32
        * GROUP_GAP
}

/// Which visual group a line belongs to.
///
/// The gap goes between groups rather than after a fixed line, so a pane with
/// no page set does not carry the gap that would have separated one.
fn group_of(line: Line) -> u8 {
    match line {
        Line::Name => 0,
        Line::Fact => 1,
        Line::Heading | Line::Page => 2,
        Line::Quiet => 3,
    }
}

fn emphasis_of(line: Line) -> Emphasis {
    match line {
        Line::Name => Emphasis::Strong,
        Line::Fact => Emphasis::Body,
        Line::Heading => Emphasis::Accent,
        Line::Page | Line::Quiet => Emphasis::Dim,
    }
}

fn size_of(line: Line) -> f32 {
    match line {
        Line::Name => theme::SIZE_ROW,
        _ => theme::SIZE_SMALL,
    }
}

fn draw_rows(ui: &Ui, theme: &Theme, inner: Rect, rows: &[Row], id: &'static str) {
    let painter = ui.painter().with_clip_rect(inner);
    let mut y = inner.top();

    for (i, row) in rows.iter().enumerate() {
        if y + LINE_H > inner.bottom() {
            // Out of room. Dropped silently rather than clipped mid-glyph:
            // `view::preview` already orders the lines by how much they settle
            // the question, so what falls off the bottom is what mattered
            // least.
            break;
        }
        if i > 0 && group_of(rows[i - 1].line) != group_of(row.line) {
            y += GROUP_GAP;
        }

        let emphasis = emphasis_of(row.line);
        let font = theme::font(size_of(row.line), theme.weight(emphasis));
        // Laid out first so a long file name is ellipsised rather than running
        // out of the pane - which on a `.pdf` is exactly where the part that
        // distinguishes two rows lives.
        let mut job = eframe::egui::text::LayoutJob::single_section(
            row.text.clone(),
            eframe::egui::TextFormat {
                font_id: font,
                color: theme.emphasis(emphasis),
                ..Default::default()
            },
        );
        job.wrap.max_width = inner.width();
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('\u{2026}');
        let galley = painter.layout_job(job);
        painter.galley(pos2(inner.left(), y), galley, theme.text);

        // The panel is painted rather than built from widgets, so nothing here
        // is in the accessibility tree unless it is put there. See
        // `overlay::announce`.
        let line_rect = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), LINE_H));
        let response = ui.interact(
            line_rect,
            eframe::egui::Id::new((id, i)),
            eframe::egui::Sense::hover(),
        );
        let text = row.text.clone();
        response.widget_info(|| {
            eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Label, true, &text)
        });

        y += LINE_H;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::preview::Line;

    fn row(line: Line) -> Row {
        Row {
            line,
            text: "x".into(),
        }
    }

    /// The gap is between groups, so a pane without a page set does not carry
    /// the space that would have separated one.
    #[test]
    fn a_pane_without_pages_is_shorter_than_one_with_them() {
        let bare = [row(Line::Name), row(Line::Fact), row(Line::Quiet)];
        let with_pages = [
            row(Line::Name),
            row(Line::Fact),
            row(Line::Heading),
            row(Line::Page),
            row(Line::Quiet),
        ];
        assert!(height_of(&bare) < height_of(&with_pages));
    }

    /// Two lines of the same group sit against each other.
    #[test]
    fn lines_of_one_group_are_not_separated() {
        let one = [row(Line::Fact)];
        let two = [row(Line::Fact), row(Line::Fact)];
        assert_eq!(height_of(&two) - height_of(&one), LINE_H);
    }

    #[test]
    fn the_pane_sits_against_the_right_edge_of_the_panel() {
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_WIDE_W, 400.0));
        let body = Rect::from_min_max(pos2(0.0, 64.0), pos2(theme::PANEL_WIDE_W, 356.0));

        let pane = pane_rect(panel, theme::Layout::Pane, body).expect("a pane");
        assert_eq!(pane.right(), panel.right());
        assert_eq!(pane.width(), theme::PREVIEW_W);
        assert_eq!(pane.top(), body.top());
    }

    /// The list layout has no pane at all, and gives up no width to one.
    #[test]
    fn the_list_layout_has_no_pane() {
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_W, 400.0));
        assert_eq!(pane_rect(panel, theme::Layout::List, panel), None);
        assert_eq!(list_inset(theme::Layout::List), 0.0);
        assert_eq!(list_inset(theme::Layout::Pane), theme::PREVIEW_W);
    }
}
