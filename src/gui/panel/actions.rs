//! Everything that did not fit on the footer, behind one key.
//!
//! A card floating above the `⋮` button, right-aligned to it, exactly where
//! Ueli puts its own. Each row is an optional mark, a label, and the keyboard
//! shortcut for the same thing pushed to the right - so the menu is also the
//! only place the program teaches its own chords.
//!
//! # Why it is drawn rather than built out of a `Popup`
//!
//! egui has one, and it owns the question of whether the menu is open. That
//! question belongs to [`crate::app::state::AppState::actions_open`], because
//! Escape has to close the menu rather than the panel, and which key means
//! what is the state machine's business - a popup that egui could close on
//! its own would be a second opinion about a state Escape is already deciding
//! against.
//!
//! So the card is an [`egui::Area`] painted by hand, and the one behaviour a
//! `Popup` would have supplied - a press outside shuts it - is the explicit
//! check at the bottom of [`show`].

use eframe::egui::{Align2, Area, Color32, Id, Order, Rect, Sense, Ui, pos2, vec2};

use super::chip;
use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::theme::{self, Icon, Theme, Weight};
use crate::view::actions::{Action, ActionId};

/// One row of the menu.
const ROW_H: f32 = 28.0;

/// The air inside the card.
const CARD_PAD: f32 = 4.0;

/// The air inside a row, left and right.
const ROW_PAD_X: f32 = 8.0;

/// The room kept for a row's mark, and the air after it.
const ICON_SLOT: f32 = 16.0;
const ICON_GAP: f32 = 10.0;

/// The air between the widest label and the shortcut beside it.
const LABEL_GAP: f32 = 24.0;

/// The card's widest, so a long description does not push the menu off the
/// panel it is floating over.
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

    // Its own layer, so it is drawn over the list and takes presses from it.
    // `Order::Foreground` rather than `Order::Middle`: the rows underneath
    // are registered every frame, and a menu that shared their layer would
    // hand a click to whichever was added last.
    let response = Area::new(Id::new("files-actions"))
        .order(Order::Foreground)
        .fixed_pos(card.min)
        .show(ui.ctx(), |ui| {
            ui.set_min_size(card.size());
            ui.set_max_size(card.size());
            paint(ui, theme, &entries, card, &mut intents);
        })
        .response;

    // A press anywhere else shuts it, which is the one thing `egui::Popup`
    // would have done for us. Checked against the card *and* the button that
    // opened it: without the second, a click on `⋮` while the menu is up
    // would close it here and reopen it in the footer on the same press.
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

/// Where the `⋮` is, so a press on it is not also a press outside the menu.
///
/// Recomputed rather than passed down from the footer, because the footer
/// draws before this and the two would otherwise have to agree through a
/// return value threaded across the whole panel. It is arithmetic off the
/// same three constants.
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

/// Where the card goes, and how big it is.
///
/// Above the button and right-aligned to it, which is Ueli's `RectAlign`
/// `TOP_END`. Clamped to the panel rather than to the screen: the panel is
/// frameless and a menu hanging off its edge would have no surface to sit on.
fn measure(ui: &Ui, theme: &Theme, entries: &[Action], panel: Rect) -> Rect {
    let painter = ui.painter();
    let label_font = theme::font(theme::SIZE_CHIP, Weight::Bold);

    let marks = entries.iter().any(|a| icon_of(a.id).is_some());
    let gutter = if marks { ICON_SLOT + ICON_GAP } else { 0.0 };

    let mut widest: f32 = 0.0;
    for action in entries {
        let label = painter
            .layout_no_wrap(
                action.description.to_owned(),
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
    let radius = theme::radius(theme::PANEL_RADIUS);
    // Opaque, unlike the panel under it. A translucent menu over a
    // translucent panel over somebody's drawing is three layers of ground
    // under twelve-point text.
    painter.rect_filled(card, radius, opaque(theme.card));
    painter.rect_stroke(
        card,
        radius,
        eframe::egui::Stroke::new(1.0, theme.edge),
        eframe::egui::StrokeKind::Inside,
    );

    let label_font = theme::font(theme::SIZE_CHIP, Weight::Bold);
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
            None => action.description.to_owned(),
        };
        response.widget_info(|| {
            eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Button, true, &spoken)
        });
        if response.hovered() {
            painter.rect_filled(row, theme::radius(theme::ROW_RADIUS), theme.hover);
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
            action.description,
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

/// The mark beside a row, where one says anything the label does not.
///
/// Deliberately not one per row. A menu where every line has a small grey
/// shape in front of it is a menu read as a picture book; the marks here are
/// the three that are recognised without being read, and the rest of the
/// gutter is the alignment they buy.
const fn icon_of(id: ActionId) -> Option<Icon> {
    match id {
        ActionId::Open
        | ActionId::OpenAsDocument
        | ActionId::OpenInAvwin
        | ActionId::OpenWithWindows => Some(Icon::Open),
        ActionId::Reveal => Some(Icon::Browse),
        ActionId::CopyPath | ActionId::CopyName => Some(Icon::Copy),
        ActionId::Refresh => Some(Icon::Reset),
        ActionId::Settings => Some(Icon::Gear),
    }
}

/// The card's fill, with whatever transparency the palette gave it removed.
fn opaque(c: Color32) -> Color32 {
    Color32::from_rgb(c.r(), c.g(), c.b())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row has a mark, so the gutter is never a column of air beside
    /// one line that happens to have one.
    #[test]
    fn every_action_has_a_mark() {
        for id in [
            ActionId::Open,
            ActionId::OpenAsDocument,
            ActionId::OpenInAvwin,
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

    /// The menu is anchored above its button and right-aligned to it, which
    /// is what keeps it over the panel rather than off the side of it.
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
