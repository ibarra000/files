//! The actions menu: a card above the footer's `⋮` button listing each action
//! with an optional mark, a label and its keyboard shortcut.
//!
//! The card is a hand-painted `egui::Area` rather than an `egui::Popup`, because
//! open/closed state belongs to [`crate::app::state::AppState::actions_open`]
//! (Escape closes the menu before the panel). Click-outside dismissal is
//! therefore handled explicitly in [`show`].

use eframe::egui::{Align2, Area, Id, Order, Rect, Sense, Ui, pos2, vec2};

use super::chip;
use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::theme::{self, Icon, Theme, Weight};
use crate::view::actions::{Action, ActionId};

/// Height of one menu row.
const ROW_H: f32 = 28.0;

/// Padding inside the card.
const CARD_PAD: f32 = 4.0;

/// Horizontal padding inside a row.
const ROW_PAD_X: f32 = 8.0;

/// Width reserved for a row's mark, and the gap after it.
const ICON_SLOT: f32 = 16.0;
const ICON_GAP: f32 = 10.0;

/// Minimum gap between a label and its shortcut.
const LABEL_GAP: f32 = 24.0;

/// Maximum card width, so a long label cannot push the menu off the panel.
const MAX_W: f32 = 320.0;

/// Draws the menu, if it is open, and reports what the pointer did to it.
pub fn show(ui: &Ui, state: &AppState, theme: &Theme, panel: Rect) -> Vec<Intent> {
    if !state.actions_open {
        return Vec::new();
    }
    let entries = crate::view::actions::actions(state);
    if entries.is_empty() {
        return Vec::new();
    }

    let mut intents = Vec::new();
    let card = measure(ui, theme, &entries, panel);

    // `Order::Foreground` so the card sits above the list rows; on a shared
    // layer a click would go to whichever widget was registered last.
    let response = Area::new(Id::new("files-actions"))
        .order(Order::Foreground)
        .fixed_pos(card.min)
        .show(ui.ctx(), |ui| {
            ui.set_min_size(card.size());
            ui.set_max_size(card.size());
            paint(ui, theme, &entries, card, &mut intents);
        })
        .response;

    // A press outside closes the menu. The `⋮` button is excluded, otherwise
    // a press on it would close the menu here and reopen it in the footer.
    let clicked_away = ui.input(|i| i.pointer.any_pressed())
        && ui
            .ctx()
            .pointer_interact_pos()
            .is_some_and(|at| !card.contains(at) && !more_button(panel).contains(at));
    if clicked_away && !response.contains_pointer() {
        intents.push(Intent::ShowActions(false));
    }

    intents
}

/// Returns the `⋮` button's rect.
///
/// Recomputed from the footer's layout constants; keep in sync with
/// `footer.rs`.
fn more_button(panel: Rect) -> Rect {
    const BUTTON: f32 = 24.0;
    Rect::from_min_size(
        pos2(
            panel.right() - theme::FOOTER_PAD - BUTTON,
            panel.bottom() - theme::FOOTER_H / 2.0 - BUTTON / 2.0,
        ),
        vec2(BUTTON, BUTTON),
    )
}

/// Returns the card's rect: above the `⋮` button, right-aligned to it, and
/// clamped to the panel (not the screen), since the panel is frameless.
fn measure(ui: &Ui, theme: &Theme, entries: &[Action], panel: Rect) -> Rect {
    let painter = ui.painter();
    let label_font = theme::font(theme::SIZE_CAPTION, Weight::Semibold);

    let marks = entries.iter().any(|a| icon_of(a.id).is_some());
    let gutter = if marks { ICON_SLOT + ICON_GAP } else { 0.0 };

    let mut widest: f32 = 0.0;
    for action in entries {
        let label = painter
            .layout_no_wrap(
                action.description.to_string(),
                label_font.clone(),
                theme.text,
            )
            .rect
            .width();
        let key = action
            .shortcut
            .map(|s| LABEL_GAP + chip::width(painter, s))
            .unwrap_or(0.0);
        widest = widest.max(gutter + label + key);
    }

    let w = (ROW_PAD_X * 2.0 + widest).min(MAX_W);
    let h = CARD_PAD * 2.0 + ROW_H * entries.len() as f32;
    let button = more_button(panel);
    let left = (button.right() - w).max(panel.left() + theme::FOOTER_PAD);
    let top = (button.top() - theme::FOOTER_PAD - h).max(panel.top());
    Rect::from_min_size(pos2(left, top), vec2(w, h))
}

fn paint(ui: &mut Ui, theme: &Theme, entries: &[Action], card: Rect, intents: &mut Vec<Intent>) {
    let painter = ui.painter().clone();
    let radius = theme::radius(theme::RADIUS_MEDIUM);
    // Window surface, not a tile: key caps (`NeutralBackground5`) on
    // `NeutralBackground3` are 1.07:1 in the light theme.
    painter.rect_filled(card, radius, theme.surface);
    painter.rect_stroke(
        card,
        radius,
        eframe::egui::Stroke::new(1.0, theme.edge),
        eframe::egui::StrokeKind::Inside,
    );

    let label_font = theme::font(theme::SIZE_CAPTION, Weight::Semibold);
    let marks = entries.iter().any(|a| icon_of(a.id).is_some());
    let gutter = if marks { ICON_SLOT + ICON_GAP } else { 0.0 };

    for (i, action) in entries.iter().enumerate() {
        let row = Rect::from_min_size(
            pos2(
                card.left() + CARD_PAD,
                card.top() + CARD_PAD + i as f32 * ROW_H,
            ),
            vec2(card.width() - CARD_PAD * 2.0, ROW_H),
        );
        let response = ui.interact(row, Id::new(("files-action", i)), Sense::click());
        let spoken = match action.shortcut {
            Some(key) => format!("{} \u{b7} {}", action.description, chip::spoken(key)),
            None => action.description.to_string(),
        };
        response.widget_info(|| {
            eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Button, true, &spoken)
        });
        if response.hovered() {
            painter.rect_filled(row, theme::radius(theme::RADIUS_MEDIUM), theme.hover);
        }
        if response.clicked() {
            intents.push(Intent::Act(action.id));
        }

        if let Some(icon) = icon_of(action.id)
            && theme::has_icon(ui.ctx(), icon)
        {
            painter.text(
                pos2(row.left() + ROW_PAD_X + ICON_SLOT / 2.0, row.center().y),
                Align2::CENTER_CENTER,
                icon.text(),
                theme::icon_font(theme::ICON_INLINE),
                theme.dim,
            );
        }
        painter.text(
            pos2(row.left() + ROW_PAD_X + gutter, row.center().y),
            Align2::LEFT_CENTER,
            action.description.as_ref(),
            label_font.clone(),
            theme.text,
        );
        if let Some(key) = action.shortcut {
            let w = chip::width(&painter, key);
            chip::draw(
                &painter,
                theme,
                key,
                pos2(row.right() - ROW_PAD_X - w, row.center().y),
            );
        }
    }
}

/// Returns the mark drawn beside an action's row.
const fn icon_of(id: ActionId) -> Option<Icon> {
    match id {
        ActionId::Open
        | ActionId::OpenAsDocument
        | ActionId::OpenWith(_)
        | ActionId::OpenWithWindows => Some(Icon::Open),
        ActionId::Reveal => Some(Icon::Browse),
        ActionId::CopyPath | ActionId::CopyName => Some(Icon::Copy),
        ActionId::Refresh => Some(Icon::Reset),
        ActionId::Settings => Some(Icon::Gear),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action has a mark, so the gutter never sits empty.
    #[test]
    fn every_action_has_a_mark() {
        for id in [
            ActionId::Open,
            ActionId::OpenAsDocument,
            ActionId::OpenWith(0),
            ActionId::OpenWithWindows,
            ActionId::Reveal,
            ActionId::CopyPath,
            ActionId::CopyName,
            ActionId::Refresh,
            ActionId::Settings,
        ] {
            assert!(icon_of(id).is_some(), "{id:?} has no mark");
        }
    }

    #[test]
    fn the_button_is_where_the_footer_puts_it() {
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(theme::PANEL_W, theme::PANEL_H));
        let button = more_button(panel);
        assert!(panel.contains(button.center()));
        assert_eq!(button.right(), theme::PANEL_W - theme::FOOTER_PAD);
        assert!(
            button.bottom() < panel.bottom(),
            "the button hangs off the foot of the panel"
        );
    }
}
