//! Draws one result row in the compact layout (icon, name, drive badge; 36
//! points) or the detailed layout (adds the folder below the name; 52 points).
//! Row text comes from [`crate::view::row`].
//!
//! Names are elided at the end, since the job code leads; folders are elided
//! at the start, since the deepest folder distinguishes otherwise identical
//! rows. Compact rows omit the folder, so the full path is always in the
//! tooltip and accessible name. See [`crate::config::ResultLayout`].

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align2, Color32, FontId, Rect, Response, Sense, Ui, Vec2, pos2, vec2};

use crate::config::ResultLayout;
use crate::gui::text;
use crate::gui::theme::{self, Theme, Weight};
use crate::paths::Routes;
use crate::search::matcher::Hit;
use crate::view::row as content;

/// Horizontal padding at a row's edges.
const fn pad_x(layout: ResultLayout) -> f32 {
    match layout {
        ResultLayout::Compact => 8.0,
        ResultLayout::Detailed => 10.0,
    }
}

/// Width reserved for the extension mark, and the gap after it.
const fn icon_slot(layout: ResultLayout) -> (f32, f32) {
    match layout {
        ResultLayout::Compact => (20.0, 8.0),
        ResultLayout::Detailed => (24.0, 10.0),
    }
}

/// Font size of the mark within its slot.
const fn icon_size(layout: ResultLayout) -> f32 {
    match layout {
        ResultLayout::Compact => 16.0,
        ResultLayout::Detailed => 18.0,
    }
}

/// Minimum gap between the name and the drive badge.
const COLUMN_GAP: f32 = 12.0;

/// Horizontal padding inside the badge.
const BADGE_PAD_X: f32 = 4.0;

/// Badge height, matching Fluent's `Badge size="small"`: the badge line box
/// (14) plus one point above and below.
const BADGE_H: f32 = 16.0;
const _: () = assert!(BADGE_H == theme::line_h(theme::SIZE_BADGE) + 1.0 * 2.0);

/// Per-list settings shared by every row.
pub struct Style<'a> {
    pub theme: &'a Theme,
    pub layout: ResultLayout,
    /// Configured drives, for the row's drive badge.
    pub routes: &'a Routes,
    /// Whether the icon font is available, checked once per list via
    /// [`theme::has_icon`].
    pub icons: bool,
    /// Length in bytes of the query term matched in the name.
    pub query_len: usize,
}

/// Draws one result and returns its response.
pub fn show(ui: &mut Ui, style: &Style<'_>, hit: &Hit, selected: bool) -> Response {
    let theme = style.theme;
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), theme::row_h(style.layout)),
        Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return response;
    }

    // Cloned so `ui` is not held borrowed across the layout calls below.
    let painter = ui.painter().clone();

    // Each row paints its own selection (fill plus accent bar) or hover fill.
    if selected {
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

    // The slot is reserved even without Segoe Fluent Icons, so text alignment
    // is the same either way.
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

    // Drive badge at the right end, styled as a Fluent Badge (10 pt Semibold).
    let badge_font = theme::font(theme::SIZE_BADGE, Weight::Semibold);
    let badge = content::drive_of(style.routes, &hit.path);
    let badge_w = badge
        .map(|name| measure(name, &badge_font) + BADGE_PAD_X * 2.0)
        .unwrap_or(0.0);
    if let Some(name) = badge {
        let pill = Rect::from_min_size(
            pos2(
                rect.right() - pad - badge_w,
                rect.center().y - BADGE_H / 2.0,
            ),
            vec2(badge_w, BADGE_H),
        );
        // Capsule shape, as a Fluent badge.
        painter.rect_filled(pill, theme::radius(theme::RADIUS_CIRCULAR), theme.badge_bg);
        painter.text(
            pill.center(),
            Align2::CENTER_CENTER,
            name,
            badge_font,
            theme.badge_fg,
        );
    }

    let text_right = rect.right() - pad - badge_w - if badge_w > 0.0 { COLUMN_GAP } else { 0.0 };
    let text_w = (text_right - text_left).max(0.0);

    // When the row matched by its folder, the folder is drawn at full text
    // strength instead of dimmed, without highlighting specific characters.
    // The accent hue is avoided: on a 12 pt line it falls short of AA on the
    // acrylic path.
    let inherited = hit.is_inherited();
    let folder = content::folder(&hit.path);
    let folder_colour = if inherited { theme.text } else { theme.dim };

    let body = if selected { theme.strong } else { theme.text };
    let name = content::highlight(hit, style.query_len);
    let weight = match style.layout {
        // Semibold so the name stands out above the folder line.
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
    // One line, truncated with an ellipsis. `break_anywhere` is required
    // because job codes have no spaces to break at.
    job.wrap.max_width = text_w;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some(text::ELLIPSIS);
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
            // Both lines are centred as a block, so a row with no folder line
            // is still centred. No gap between lines; `ROW_DETAILED_H` is
            // derived from the two line heights.
            let second = if folder.is_empty() {
                0.0
            } else {
                theme::line_h(theme::SIZE_CAPTION)
            };
            let top = rect.center().y - (name_h + second) / 2.0;
            painter.galley(pos2(text_left, top), galley, body);
            if !folder.is_empty() {
                let shown = text::elide_left(folder, text_w, &|text| measure(text, &folder_font));
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

    // Accessible name: full name, folder and drive, untruncated and regardless
    // of layout. The selected state is reported too.
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

/// Draws the accent bar on the left of a selected row.
///
/// `MARKER_W` wide, 45% of the row height, vertically centred. Shared with the
/// recent-codes, alias and drive lists.
pub fn marker(ui: &Ui, theme: &Theme, row: Rect) {
    let h = row.height() * 0.45;
    let bar = Rect::from_min_size(
        pos2(row.left(), row.center().y - h / 2.0),
        Vec2::new(theme::MARKER_W, h),
    );
    ui.painter()
        .rect_filled(bar, theme::radius(theme::RADIUS_LARGE), theme.accent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_layouts_are_actually_different() {
        let (compact, detailed) = (ResultLayout::Compact, ResultLayout::Detailed);
        assert!(theme::row_h(detailed) > theme::row_h(compact));
        assert!(icon_slot(detailed).0 > icon_slot(compact).0);
        assert!(icon_size(detailed) > icon_size(compact));
        assert!(pad_x(detailed) > pad_x(compact));
    }

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
