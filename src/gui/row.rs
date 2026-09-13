//! One result, drawn.
//!
//! The name on the left with the matched run picked out, the folder on the
//! right dimmed and truncated from its *front*. What each of those says is
//! decided in [`crate::view::row`]; this file is only about where the ink goes.
//!
//! # Why the folder loses its front and the name loses its end
//!
//! They are read for opposite reasons. A filename is scanned from the start -
//! the job code is the first thing in it - so the end is what can go. A folder
//! is being used to tell two otherwise identical rows apart, and what tells
//! them apart is the *deepest* part of the path, which is at the end. Eliding
//! them the same way would make one of the two columns useless.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align2, Color32, FontId, Rect, Response, Sense, Ui, Vec2, pos2, vec2};

use crate::gui::theme::{self, Theme, Weight};
use crate::search::matcher::Hit;
use crate::view::row as content;

/// The character that says something was left out.
const ELLIPSIS: char = '\u{2026}';

/// The most of a row the folder column may take.
///
/// The filename is what the user is looking for; the folder only ever breaks a
/// tie between two of them. Past about a third, a deep path starts pushing the
/// thing being searched for off its own row.
const FOLDER_SHARE: f32 = 0.38;

/// The gap between the name and the folder, so a long name and a long path do
/// not read as one string.
const COLUMN_GAP: f32 = 20.0;

/// Draws one result and reports what the pointer did to it.
pub fn show(
    ui: &mut Ui,
    theme: &Theme,
    hit: &Hit,
    query_len: usize,
    selected: bool,
    alpha: f32,
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), theme::ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    // Cloned rather than borrowed: the layout calls below go through the same
    // `Ui`, and a live `&Painter` would keep it borrowed across them.
    let painter = ui.painter().clone();
    let fade = |c: Color32| theme::faded(c, alpha);

    // The selection is drawn by the panel, which animates it between rows; a
    // row painting its own would fight that and win, since it draws later.
    if !selected && response.hovered() {
        painter.rect_filled(rect, theme::radius(theme::ROW_RADIUS), fade(theme.hover));
    }

    let inner = rect.shrink2(vec2(theme::ROW_PAD_X, 0.0));
    let folder_font = theme::font(theme::SIZE_SMALL, Weight::Regular);

    // A row that is here because its *folder* matched has nothing in its own
    // name to pick out, so the explanation moves to the other column: the
    // folder is drawn in the accent colour rather than dimmed. That says "this
    // one is about the folder" without claiming which characters matched -
    // which is the claim `match_pos` explicitly declines to make.
    let inherited = hit.is_inherited();
    let folder = content::folder(&hit.path);
    let folder_colour = if inherited { theme.accent } else { theme.dim };

    let measure = |text: &str, font: &FontId| {
        painter
            .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
            .rect
            .width()
    };

    // The folder takes what it needs up to its share, and the name gets the
    // rest - so a shallow path does not leave a column of empty space beside a
    // filename that had to be truncated.
    let folder_cap = (inner.width() * FOLDER_SHARE).max(0.0);
    let folder_w = if folder.is_empty() {
        0.0
    } else {
        measure(folder, &folder_font).min(folder_cap)
    };
    let name_w = (inner.width() - folder_w - COLUMN_GAP).max(0.0);

    let name = content::highlight(hit, query_len);
    let mut job = LayoutJob::default();
    let mut push = |text: &str, colour: Color32, weight: Weight| {
        if text.is_empty() {
            return;
        }
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: theme::font(theme::SIZE_ROW, weight),
                color: fade(colour),
                ..Default::default()
            },
        );
    };
    let body = if selected { theme.strong } else { theme.text };
    push(name.before, body, Weight::Regular);
    push(name.matched, theme.match_run, Weight::Bold);
    push(name.after, body, Weight::Regular);
    // One line, truncated with an ellipsis. `break_anywhere` because a job code
    // has no spaces to break at, so without it a long name wraps to nothing
    // rather than truncating.
    job.wrap.max_width = name_w;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some(ELLIPSIS);

    let galley = painter.layout_job(job);
    painter.galley(
        pos2(inner.left(), rect.center().y - galley.rect.height() / 2.0),
        galley,
        fade(body),
    );

    if !folder.is_empty() {
        let shown = elide_left(folder, folder_w, &|text| measure(text, &folder_font));
        painter.text(
            pos2(inner.right(), rect.center().y),
            Align2::RIGHT_CENTER,
            shown,
            folder_font,
            fade(folder_colour),
        );
    }

    // The name and the folder, as one label. The row is already a widget, so
    // unlike the painted parts of the panel it can say what it is itself -
    // and it says the *whole* thing rather than what fitted, because a reader
    // has no column width to be elided to.
    //
    // Selected rather than merely present: which row Enter would take is the
    // single most useful fact about this list, and colour alone does not carry
    // it to a magnifier.
    let label = if folder.is_empty() {
        hit.name.to_string()
    } else {
        format!("{}, in {folder}", hit.name)
    };
    response.widget_info(|| {
        eframe::egui::WidgetInfo::selected(eframe::egui::WidgetType::Button, true, selected, &label)
    });

    response.on_hover_text(hit.path.as_ref())
}

/// The accent bar down the left of the selected row.
///
/// Drawn by the panel rather than by the row, because it slides between rows
/// and a row only knows about itself.
pub fn marker(ui: &Ui, theme: &Theme, row: Rect, alpha: f32) {
    let bar = Rect::from_min_size(
        pos2(row.left(), row.center().y - theme::ROW_H * 0.3),
        Vec2::new(theme::MARKER_W, theme::ROW_H * 0.6),
    );
    ui.painter()
        .rect_filled(bar, theme::radius(2), theme::faded(theme.accent, alpha));
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
    // at most eight rows are ever on screen, and this only runs at all for the
    // ones that did not fit - so the simple loop is also the fast one.
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
}
