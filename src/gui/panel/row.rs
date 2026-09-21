//! One result, drawn.
//!
//! Ueli's two row layouts, to the point. Compact is an icon, a name and a
//! badge on one line, thirty-six points tall; detailed puts the folder under
//! the name, uses a larger icon and stands fifty-two. What each part *says* is
//! decided in [`crate::view::row`]; this file is only about where the ink
//! goes.
//!
//! # Why the folder loses its front and the name loses its end
//!
//! They are read for opposite reasons. A filename is scanned from the start -
//! the job code is the first thing in it - so the end is what can go. A folder
//! is being used to tell two otherwise identical rows apart, and what tells
//! them apart is the *deepest* part of the path, which is at the end. Eliding
//! them the same way would make one of the two useless.
//!
//! # What a compact row cannot say
//!
//! The folder, and it matters more here than it does in Ueli. Ueli's rows are
//! installed applications with unique names; ours are files, and two drawings
//! called `GA.pdf` in different job folders are the ordinary case. In compact
//! they are two rows that differ only in the drive badge at the end.
//!
//! That is deliberate and it is Ueli's default, which is what was asked for.
//! The full path is on the hover tooltip and in the accessible name of every
//! row whichever layout is on, so nothing is *lost* - and the Appearance page
//! has the switch. See [`crate::config::ResultLayout`].

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align2, Color32, FontId, Rect, Response, Sense, Ui, Vec2, pos2, vec2};

use crate::config::ResultLayout;
use crate::gui::theme::{self, Theme, Weight};
use crate::paths::Routes;
use crate::search::matcher::Hit;
use crate::view::row as content;

/// The character that says something was left out.
const ELLIPSIS: char = '\u{2026}';

/// The air at a row's left and right edges.
///
/// Ueli's `padding`, which is eight on a compact row and ten on a detailed
/// one - the taller row can afford the wider margin and needs it, because its
/// two lines of text make a denser block.
const fn pad_x(layout: ResultLayout) -> f32 {
    match layout {
        ResultLayout::Compact => 8.0,
        ResultLayout::Detailed => 10.0,
    }
}

/// The room kept for the extension mark, and the air after it.
const fn icon_slot(layout: ResultLayout) -> (f32, f32) {
    match layout {
        ResultLayout::Compact => (20.0, 8.0),
        ResultLayout::Detailed => (24.0, 10.0),
    }
}

/// The size the mark itself is set at, inside that slot.
const fn icon_size(layout: ResultLayout) -> f32 {
    match layout {
        ResultLayout::Compact => 16.0,
        ResultLayout::Detailed => 18.0,
    }
}

/// The air between the name and the badge, so a long name and a drive name do
/// not read as one string.
const COLUMN_GAP: f32 = 12.0;

/// The air inside the badge, left and right of its text.
const BADGE_PAD_X: f32 = 6.0;

/// Everything a row needs that is the same for every row in the list.
///
/// Gathered rather than passed one by one: four of these are read once per
/// row for three hundred rows, and a call with seven positional arguments is
/// one where two of the same type eventually get swapped.
pub struct Style<'a> {
    pub theme: &'a Theme,
    pub layout: ResultLayout,
    /// Which drives are configured, for the badge at the end of a row.
    pub routes: &'a Routes,
    /// Whether this machine can draw the extension marks at all.
    ///
    /// Asked once by the list rather than once per row: [`theme::has_icon`]
    /// goes through the font atlas, and the answer is the same for every row.
    pub icons: bool,
    /// How much of the name the query matched.
    pub query_len: usize,
}

/// Draws one result and reports what the pointer did to it.
pub fn show(ui: &mut Ui, style: &Style<'_>, hit: &Hit, selected: bool) -> Response {
    let theme = style.theme;
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::row_h(style.layout)),
        Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }

    // Cloned rather than borrowed: the layout calls below go through the same
    // `Ui`, and a live `&Painter` would keep it borrowed across them.
    let painter = ui.painter().clone();

    // Its own, now. The panel used to paint the highlight from a y it had
    // measured, because it slid between rows and a row only knows about
    // itself. Nothing slides any more, and the list is a scroller - so the
    // panel no longer knows where a row is either, and the row is the only
    // thing that does.
    //
    // Raised, then washed. The shading is what says "this one", and the wash
    // is what says which one - the pair is legible where either alone would
    // not be, which is the point of shading a monochrome panel.
    if selected {
        theme::raise(&painter, theme, rect, theme::RADIUS_MEDIUM);
        painter.rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.selection);
        marker(ui, theme, rect);
    } else if response.hovered() {
        painter.rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.hover);
    }

    let measure = |text: &str, font: &FontId| {
        painter
            .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
            .rect
            .width()
    };

    let pad = pad_x(style.layout);
    let (slot, gap) = icon_slot(style.layout);

    // The mark, centred in its slot. The slot is kept whether or not the
    // machine can draw into it: a row that shifts twenty points left on a
    // Windows without Segoe Fluent Icons is a different layout, not a
    // degraded one.
    if style.icons {
        painter.text(
            pos2(rect.left() + pad + slot / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            super::icons::of(&hit.path).text(),
            theme::icon_font(icon_size(style.layout)),
            theme.dim,
        );
    }
    let text_left = rect.left() + pad + slot + gap;

    // The drive, at the far end, in a pill. A badge rather than more text:
    // it is a fact *about* the row rather than part of what the row is, and
    // in the detailed layout there is a folder on the line below that would
    // otherwise read as the same kind of thing.
    let badge_font = theme::font(theme::SIZE_CAPTION, Weight::Regular);
    let badge = content::drive_of(style.routes, &hit.path);
    let badge_w = badge
        .map(|name| measure(name, &badge_font) + BADGE_PAD_X * 2.0)
        .unwrap_or(0.0);
    if let Some(name) = badge {
        let pill = Rect::from_min_size(
            pos2(
                rect.right() - pad - badge_w,
                rect.center().y - theme::SIZE_CAPTION,
            ),
            vec2(badge_w, theme::SIZE_CAPTION * 2.0),
        );
        theme::cap(&painter, theme, pill, theme::RADIUS_MEDIUM);
        painter.text(
            pill.center(),
            Align2::CENTER_CENTER,
            name,
            badge_font,
            theme.dim,
        );
    }

    let text_right = rect.right() - pad - badge_w - if badge_w > 0.0 { COLUMN_GAP } else { 0.0 };
    let text_w = (text_right - text_left).max(0.0);

    // A row that is here because its *folder* matched has nothing in its own
    // name to pick out, so the explanation moves to the other line: the
    // folder is drawn in the accent colour rather than dimmed. That says
    // "this one is about the folder" without claiming which characters
    // matched - which is the claim `match_pos` explicitly declines to make.
    let inherited = hit.is_inherited();
    let folder = content::folder(&hit.path);
    let folder_colour = if inherited { theme.accent } else { theme.dim };

    let body = if selected { theme.strong } else { theme.text };
    let name = content::highlight(hit, style.query_len);
    let weight = match style.layout {
        // Semibold, as Ueli sets it: with a second line under it the name has
        // to be the one the eye lands on.
        ResultLayout::Detailed => Weight::Semibold,
        ResultLayout::Compact => Weight::Regular,
    };
    let mut job = LayoutJob::default();
    let mut push = |text: &str, colour: Color32, weight: Weight| {
        if text.is_empty() {
            return;
        }
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: theme::font(theme::SIZE_BODY, weight),
                color: colour,
                ..Default::default()
            },
        );
    };
    push(name.before, body, weight);
    push(name.matched, theme.match_run, Weight::Semibold);
    push(name.after, body, weight);
    // One line, truncated with an ellipsis. `break_anywhere` because a job
    // code has no spaces to break at, so without it a long name wraps to
    // nothing rather than truncating.
    job.wrap.max_width = text_w;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some(ELLIPSIS);
    let galley = painter.layout_job(job);

    match style.layout {
        ResultLayout::Compact => {
            painter.galley(
                pos2(text_left, rect.center().y - galley.rect.height() / 2.0),
                galley,
                body,
            );
        }
        ResultLayout::Detailed => {
            let folder_font = theme::font(theme::SIZE_CAPTION, Weight::Regular);
            let name_h = galley.rect.height();
            // The two lines stacked and centred as a block, rather than each
            // pinned to an edge of the row: a file at the root of a drive has
            // no second line, and pinning would leave it sitting high in its
            // own rectangle beside neighbours that do not.
            // No gap between the two: Ueli stacks them in a flex column
            // with none, and the two published line heights are exactly what
            // `ROW_DETAILED_H` was derived from.
            let second = if folder.is_empty() {
                0.0
            } else {
                theme::line_h(theme::SIZE_CAPTION)
            };
            let top = rect.center().y - (name_h + second) / 2.0;
            painter.galley(pos2(text_left, top), galley, body);
            if !folder.is_empty() {
                let shown = elide_left(folder, text_w, &|text| measure(text, &folder_font));
                painter.text(
                    pos2(text_left, top + name_h),
                    Align2::LEFT_TOP,
                    shown,
                    folder_font,
                    folder_colour,
                );
            }
        }
    }

    // The name, the folder and the drive, as one label - whichever of them
    // the layout actually drew. The row is already a widget, so unlike the
    // painted parts of the panel it can say what it is itself, and it says
    // the *whole* thing rather than what fitted, because a reader has no
    // column width to be elided to.
    //
    // Whole in the other sense too: a compact row does not draw the folder,
    // and a reader who was given only what was drawn would be worse off than
    // somebody looking at the tooltip. What a row *is* does not depend on how
    // much room it was given to say so.
    //
    // Selected rather than merely present: which row Enter would take is the
    // single most useful fact about this list, and colour alone does not
    // carry it to a magnifier.
    let mut label = hit.name.to_string();
    if !folder.is_empty() {
        label.push_str(&format!(", in {folder}"));
    }
    if let Some(drive) = badge {
        label.push_str(&format!(", on {drive}"));
    }
    response.widget_info(|| {
        eframe::egui::WidgetInfo::selected(eframe::egui::WidgetType::Button, true, selected, &label)
    });

    response.on_hover_text(hit.path.as_ref())
}

/// The accent bar down the left of the selected row.
///
/// Ueli's is three points wide, forty-five per cent of the row's height, and
/// vertically centred at its left edge. Shared rather than inlined above,
/// because the recall list and the drive picker are the same kind of list and
/// have to mark their row the same way.
pub fn marker(ui: &Ui, theme: &Theme, row: Rect) {
    let h = row.height() * 0.45;
    let bar = Rect::from_min_size(
        pos2(row.left(), row.center().y - h / 2.0),
        Vec2::new(theme::MARKER_W, h),
    );
    ui.painter()
        .rect_filled(bar, theme::radius(theme::RADIUS_LARGE), theme.accent);
}

/// Drops characters from the front until what is left fits, marking the cut
/// with a leading ellipsis.
///
/// Takes a measuring function rather than a font, so the policy - how much is
/// dropped, and whether the ellipsis is accounted for - can be checked without
/// a font atlas.
fn elide_left(text: &str, max_w: f32, measure: &impl Fn(&str) -> f32) -> String {
    if measure(text) <= max_w {
        return text.to_owned();
    }

    // One character at a time from the front. A path has at most a few hundred,
    // at most a handful of rows are ever on screen, and this only runs at all
    // for the ones that did not fit - so the simple loop is also the fast one.
    let mut start = 0;
    while start < text.len() {
        // `char_indices` rather than byte arithmetic: a share name can hold
        // characters that are not one byte long, and slicing inside one panics.
        let next = text[start..]
            .char_indices()
            .nth(1)
            .map_or(text.len(), |(offset, _)| start + offset);
        let candidate = format!("{ELLIPSIS}{}", &text[next..]);
        if measure(&candidate) <= max_w {
            return candidate;
        }
        start = next;
    }

    // Not even the ellipsis fits. Returning it anyway would draw outside the
    // column; an empty string is at least honest about having no room.
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font where every character is one unit wide, so the assertions below
    /// are about the policy rather than about Segoe UI's metrics.
    fn monospaced(text: &str) -> f32 {
        text.chars().count() as f32
    }

    #[test]
    fn a_folder_that_fits_is_left_alone() {
        let path = r"R:\11d\11-D-0704";
        assert_eq!(elide_left(path, 100.0, &monospaced), path);
        // Exactly the available width is still fitting.
        assert_eq!(
            elide_left(path, monospaced(path), &monospaced),
            path,
            "a folder was truncated to make room for nothing"
        );
    }

    /// The end of the path is what tells two rows apart, so the end is what
    /// survives.
    #[test]
    fn a_long_folder_loses_its_front_and_keeps_its_tail() {
        let path = r"R:\jobs\2024\11d\11-D-0704";
        let shown = elide_left(path, 12.0, &monospaced);

        assert!(monospaced(&shown) <= 12.0, "{shown:?} does not fit");
        assert!(shown.starts_with(ELLIPSIS), "{shown:?} hides nothing");
        assert!(
            path.ends_with(shown.trim_start_matches(ELLIPSIS)),
            "{shown:?} is not a tail of the path"
        );
        assert!(shown.ends_with("11-D-0704"), "{shown:?} lost the job code");
    }

    /// It must drop as little as it can get away with - an elision that
    /// overshoots throws away the very part it was keeping.
    #[test]
    fn no_more_is_dropped_than_has_to_be() {
        let path = r"R:\jobs\2024\11d\11-D-0704";
        for width in 4..=26 {
            let shown = elide_left(path, width as f32, &monospaced);
            assert!(monospaced(&shown) <= width as f32, "{width}: {shown:?}");
            if !shown.starts_with(ELLIPSIS) {
                continue;
            }
            // Putting one more character back must overflow, or the elision
            // was greedier than it needed to be.
            let tail = shown.chars().count() - 1;
            let total = path.chars().count();
            if tail >= total {
                continue;
            }
            let candidate: String = std::iter::once(ELLIPSIS)
                .chain(path.chars().skip(total - tail - 1))
                .collect();
            assert!(
                monospaced(&candidate) > width as f32,
                "{width}: {shown:?} dropped more than it had to"
            );
        }
    }

    /// Slicing inside a character is a panic, and share names are not all
    /// ASCII.
    #[test]
    fn eliding_a_path_with_wide_characters_does_not_panic() {
        let path = "R:\\Zeichnungen\\Prüfung\\日本語のフォルダ\\11-D-0704";
        for width in 0..=40 {
            let shown = elide_left(path, width as f32, &monospaced);
            assert!(monospaced(&shown) <= width as f32, "{width}: {shown:?}");
        }
    }

    /// A column with no room draws nothing rather than spilling an ellipsis
    /// into the filename beside it.
    #[test]
    fn a_column_too_narrow_for_anything_draws_nothing() {
        assert_eq!(elide_left(r"R:\jobs", 0.0, &monospaced), "");
        assert_eq!(elide_left(r"R:\jobs", 0.5, &monospaced), "");
    }

    #[test]
    fn an_empty_folder_stays_empty() {
        assert_eq!(elide_left("", 0.0, &monospaced), "");
    }

    /// The two layouts have to differ in more than their height, or the
    /// setting is a row that is taller for no reason.
    #[test]
    fn the_two_layouts_are_actually_different() {
        let (compact, detailed) = (ResultLayout::Compact, ResultLayout::Detailed);
        assert!(theme::row_h(detailed) > theme::row_h(compact));
        assert!(icon_slot(detailed).0 > icon_slot(compact).0);
        assert!(icon_size(detailed) > icon_size(compact));
        assert!(pad_x(detailed) > pad_x(compact));
    }

    /// The mark's slot has to hold the mark, or the glyph spills into the
    /// name beside it.
    #[test]
    fn the_mark_fits_the_room_kept_for_it() {
        for layout in ResultLayout::ALL {
            assert!(
                icon_size(layout) <= icon_slot(layout).0,
                "{layout:?}: a {}pt mark in a {}pt slot",
                icon_size(layout),
                icon_slot(layout).0
            );
        }
    }
}
