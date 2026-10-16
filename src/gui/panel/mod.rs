//! The search panel: header, hairline, content, hairline and footer in a
//! fixed-size window. Text comes from [`crate::view`]; this module arranges it.
//!
//! The band heights are constants and only the content band scrolls (an
//! `egui::ScrollArea` holding the whole list), so the window never resizes.
//!
//! The text field always has keyboard focus: Left/Right move the caret, Up/Down
//! move the selection, and Enter activates it. Recent searches are a mode entered
//! only with Up and left with Escape, so search history is never shown unasked.

pub mod actions;
pub mod chip;
pub mod field;
pub mod footer;
pub mod icons;
pub mod list;
pub mod row;

use eframe::egui::{CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::time::SystemTime;

use crate::app::state::pointer::Intent;
use crate::app::state::{AppState, EmptyReason};
use crate::gui::anim::Content;
use crate::gui::theme::{self, Theme};
use crate::gui::window::Backdrop;

/// Returns which body the state calls for.
pub fn body_of(state: &AppState) -> Content {
    if state.picking_share {
        return Content::Shares;
    }
    // Checked before the field, because browsing puts a past search in the field.
    // Delegates to `showing_recent` so there is one source of truth.
    if state.showing_recent() {
        return Content::Recent;
    }
    // Configured aliases, shown for an empty box; see
    // `AppState::showing_aliases`.
    if state.showing_aliases() {
        return Content::Aliases;
    }
    if state.input.text().is_empty() {
        return Content::Empty;
    }
    if state.hits.is_empty() {
        Content::Empty
    } else {
        Content::Results
    }
}

/// Returns why the body is empty, defaulting to `EmptyReason::NoQuery`.
pub(crate) fn empty_reason(state: &AppState) -> EmptyReason {
    state.empty_reason.clone().unwrap_or(EmptyReason::NoQuery)
}

/// Returns the header and footer bands, by which the window may be dragged.
///
/// The content band is excluded so presses there reach the
/// `egui::ScrollArea`. The search box is registered on top of the header
/// and takes its own presses; the padding around it stays draggable.
pub fn handles(rect: Rect) -> (Rect, Rect) {
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), theme::HEADER_H));
    let footer = Rect::from_min_size(
        pos2(rect.left(), rect.bottom() - theme::FOOTER_H),
        vec2(rect.width(), theme::FOOTER_H),
    );
    (header, footer)
}

/// Draws the whole panel and returns the pointer intents it produced.
///
/// One frame can produce several intents, such as a hover and a click.
pub fn show(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    content: Content,
    backdrop: Option<Backdrop>,
    wall: SystemTime,
) -> Vec<Intent> {
    let rect = ui.max_rect();
    paint_surface(ui, theme, rect, backdrop);

    let mut cursor = rect;
    let header = take(&mut cursor, theme::HEADER_H);
    let mut intents = field::show(ui, state, theme, header);

    let upper = take(&mut cursor, theme::DIVIDER);
    let band = take_bottom(&mut cursor, theme::FOOTER_H);
    let lower = take_bottom(&mut cursor, theme::DIVIDER);

    rule(ui, theme, upper);
    rule(ui, theme, lower);

    intents.extend(footer::show(ui, state, theme, band, wall));
    intents.extend(list::show(ui, state, theme, cursor, content, wall));
    // Drawn last so the menu floats above the footer and list.
    intents.extend(actions::show(ui, state, theme, rect));
    intents
}

/// Registers `text` at `rect` in the accessibility tree.
///
/// egui builds the tree from widgets, but this panel paints text directly, so
/// painted strings must be announced explicitly. Uses `Sense::hover()` and a
/// namespaced id so it never takes clicks from real widgets, which report
/// through their own `egui::Response`.
pub(crate) fn announce(
    ui: &Ui,
    rect: Rect,
    what: impl std::hash::Hash + std::fmt::Debug,
    text: &str,
) {
    if text.trim().is_empty() {
        return;
    }
    let response = ui.interact(rect, Id::new(("files-a11y", what)), Sense::hover());
    let text = text.to_owned();
    response.widget_info(|| {
        eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Label, true, &text)
    });
}

/// Paints the panel background.
///
/// With a compositor backdrop, a translucent wash over the acrylic gives a
/// predictable ground for the contrast checks in `theme::tests`; otherwise the
/// fill is the opaque surface colour.
///
/// The fill is square with no outline. DWM rounds and outlines the window
/// (`DWMWCP_ROUND`) in device pixels; a rounded fill in egui points would
/// misalign whenever `pixels_per_point` differs from the monitor scale.
fn paint_surface(ui: &Ui, theme: &Theme, rect: Rect, backdrop: Option<Backdrop>) {
    let fill = if backdrop == Some(Backdrop::Compositor) {
        theme.wash
    } else {
        theme.surface
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
}

fn take(cursor: &mut Rect, height: f32) -> Rect {
    let taken = Rect::from_min_size(
        cursor.min,
        vec2(cursor.width(), height.min(cursor.height())),
    );
    cursor.min.y = taken.max.y;
    taken
}

fn take_bottom(cursor: &mut Rect, height: f32) -> Rect {
    let height = height.min(cursor.height());
    let taken = Rect::from_min_size(
        pos2(cursor.left(), cursor.bottom() - height),
        vec2(cursor.width(), height),
    );
    cursor.max.y = taken.min.y;
    taken
}

/// Draws the edge-to-edge hairline between two bands.
fn rule(ui: &Ui, theme: &Theme, band: Rect) {
    ui.painter().hline(
        band.x_range(),
        band.center().y,
        Stroke::new(theme::DIVIDER, theme.edge),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::AppEvent;
    use crate::app::key::{Key, KeyEvent, Mods};
    use crate::app::state::pointer::Intent;
    use crate::config::Settings;
    use crate::search::matcher::Hit;
    use std::sync::Arc;
    use std::time::Instant;

    /// Selects `row` through the same intent a click sends.
    fn select(state: &mut AppState, row: usize) {
        state.update(AppEvent::Intent(Intent::Select(row)), Instant::now());
    }

    fn state() -> AppState {
        AppState::new(Settings::fixture(), Instant::now())
    }

    /// Reports a healthy index, clearing the "No file list yet" notice.
    fn settle_index(state: &mut AppState) {
        let status = crate::index::store::IndexStatus {
            origin: Some(crate::index::store::Origin::Network),
            entries: 10,
            built_at: Some(SystemTime::UNIX_EPOCH),
            ..Default::default()
        };
        state.update(
            crate::app::event::AppEvent::Index(crate::app::event::IndexMsg::Status {
                id: crate::paths::MappingId(0),
                status: std::sync::Arc::new(status),
            }),
            Instant::now(),
        );
    }

    fn with_hits(n: usize) -> AppState {
        let mut state = state();
        state.input.set_text("AB-C");
        state.hits = (0..n)
            .map(|i| Hit {
                path: Arc::from(format!(r"R:\projects\AB-C-070{i}.pdf").as_str()),
                name: Arc::from(format!("AB-C-070{i}.pdf").as_str()),
                match_pos: 0,
                index: i as u32,
            })
            .collect();
        state
    }

    #[test]
    fn an_empty_field_with_no_history_shows_the_first_run_block() {
        assert_eq!(body_of(&state()), Content::Empty);
    }

    #[test]
    fn the_recent_searches_appear_only_after_the_up_arrow() {
        let mut state = state();
        state.history.record("AB-C-0704");

        // Search history is not shown until Up is pressed.
        assert_eq!(body_of(&state), Content::Empty);

        state.update(
            AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)),
            std::time::Instant::now(),
        );
        assert_eq!(body_of(&state), Content::Recent);
    }

    #[test]
    fn typing_something_that_matches_shows_the_results() {
        assert_eq!(body_of(&with_hits(3)), Content::Results);
    }

    #[test]
    fn typing_something_that_matches_nothing_shows_why() {
        let mut state = state();
        state.input.set_text("zzzz");
        assert_eq!(body_of(&state), Content::Empty);
    }

    #[test]
    fn the_drive_picker_replaces_whatever_was_there() {
        let mut state = with_hits(3);
        assert_eq!(body_of(&state), Content::Results);

        state.picking_share = true;
        assert_eq!(body_of(&state), Content::Shares);
    }

    /// The whole result list is in the body; only `MAX_RESULTS` caps it.
    #[test]
    fn a_long_result_list_is_all_of_it() {
        for n in [0, 1, 7, 8, 9, 400] {
            let state = with_hits(n);
            assert_eq!(state.hits.len(), n);
            assert_eq!(
                body_of(&state),
                if n == 0 {
                    Content::Empty
                } else {
                    Content::Results
                },
                "{n} hits"
            );
        }
    }

    /// `view::empty` returns no block for `NoQuery`, so the first screen is
    /// blank.
    #[test]
    fn an_untouched_panel_has_nothing_in_its_body() {
        let mut state = state();
        settle_index(&mut state);
        assert_eq!(body_of(&state), Content::Empty);
        assert!(
            crate::view::empty::view(&empty_reason(&state), state.input.text()).is_none(),
            "the first screen has something on it"
        );
    }

    /// An empty box shows configured aliases, or nothing if there are none.
    #[test]
    fn an_empty_box_shows_the_shortcuts_and_only_if_there_are_any() {
        let mut state = state();
        settle_index(&mut state);
        assert_eq!(body_of(&state), Content::Empty, "nothing is configured");

        state.settings.aliases =
            std::sync::Arc::new(crate::alias::Aliases::new(vec![crate::alias::Alias {
                name: "pw".into(),
                code: "AB-C-0704".into(),
                note: None,
            }]));
        assert_eq!(body_of(&state), Content::Aliases);
    }

    /// Showing aliases does not expose recent searches; Up is still required.
    #[test]
    fn the_shortcuts_do_not_bring_the_recent_searches_with_them() {
        let mut state = state();
        settle_index(&mut state);
        state.settings.aliases =
            std::sync::Arc::new(crate::alias::Aliases::new(vec![crate::alias::Alias {
                name: "pw".into(),
                code: "AB-C-0704".into(),
                note: None,
            }]));
        state.history.record("XY-Z-1234");

        assert_eq!(body_of(&state), Content::Aliases);
        state.update(
            AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)),
            Instant::now(),
        );
        assert_eq!(body_of(&state), Content::Recent, "Up is still the way in");
    }

    /// A toast still reaches the footer while the body is empty.
    #[test]
    fn something_to_say_is_not_lost() {
        let mut state = state();
        settle_index(&mut state);

        state.update(
            crate::app::event::AppEvent::Clipboard(crate::app::event::ClipboardMsg::Copied {
                chars: 9,
            }),
            Instant::now(),
        );
        assert!(state.toast.is_some(), "the fixture raised no toast");
        assert_eq!(body_of(&state), Content::Empty);
    }

    #[test]
    fn selecting_a_row_leaves_the_body_alone() {
        let mut state = with_hits(300);
        assert_eq!(body_of(&state), Content::Results);
        select(&mut state, 0);
        assert_eq!(body_of(&state), Content::Results);
        select(&mut state, 299);
        assert_eq!(body_of(&state), Content::Results);
    }

    #[test]
    fn the_bands_partition_the_panel() {
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_W, theme::PANEL_H));
        let mut cursor = panel;
        let header = take(&mut cursor, theme::HEADER_H);
        let upper = take(&mut cursor, theme::DIVIDER);
        let footer = take_bottom(&mut cursor, theme::FOOTER_H);
        let lower = take_bottom(&mut cursor, theme::DIVIDER);

        assert_eq!(header.top(), panel.top());
        assert_eq!(upper.top(), header.bottom());
        assert_eq!(cursor.top(), upper.bottom());
        assert_eq!(cursor.bottom(), lower.top());
        assert_eq!(lower.bottom(), footer.top());
        assert_eq!(footer.bottom(), panel.bottom());
        assert_eq!(
            cursor.height(),
            theme::CONTENT_H,
            "the content band is not what the constant says it is"
        );
    }

    #[test]
    fn the_drag_handles_are_the_header_and_the_footer() {
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_W, theme::PANEL_H));
        let (header, footer) = handles(panel);

        assert!(header.contains(pos2(300.0, 5.0)), "the header is not one");
        assert!(
            footer.contains(pos2(300.0, theme::PANEL_H - 5.0)),
            "the footer is not one"
        );

        // The middle of the list, which belongs to the scroller.
        let middle = pos2(300.0, theme::PANEL_H / 2.0);
        assert!(
            !header.contains(middle) && !footer.contains(middle),
            "a press in the list would carry the window off"
        );
    }
}
