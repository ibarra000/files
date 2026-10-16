//! The footer: a settings button on the left, the status line in the middle,
//! and on the right a button naming the default (Enter) action with its
//! shortcut chip, a divider, and the `⋮` actions-menu button.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align2, Color32, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::time::SystemTime;

use super::chip;
use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::theme::{self, Icon, Theme, Weight};
use crate::view::actions::ActionId;
use crate::view::{self, Emphasis};

/// Side length of a square icon button.
const BUTTON: f32 = 24.0;

/// Gap between footer items.
const GAP: f32 = 8.0;

/// Horizontal padding inside the default-action button.
const LABEL_PAD_X: f32 = 8.0;

/// Minimum width reserved for the status line.
///
/// Fits the longest transient message (`Checking the drive…`); longer text is
/// truncated. Fixed so the layout does not reflow as the phase changes.
const STATUS_RESERVE: f32 = 120.0;

/// Gap between the result count and the status text.
const COUNT_GAP: f32 = 12.0;

pub fn show(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    rect: Rect,
    wall: SystemTime,
) -> Vec<Intent> {
    let mut intents = Vec::new();

    // Settings button, on the left.
    let gear = Rect::from_min_size(
        pos2(
            rect.left() + theme::FOOTER_PAD,
            rect.center().y - BUTTON / 2.0,
        ),
        vec2(BUTTON, BUTTON),
    );
    if icon_button(ui, theme, gear, Icon::Gear, "Settings").clicked() {
        intents.push(Intent::Act(ActionId::Settings));
    }

    // Right-hand controls, laid out from the edge inwards.
    let more = Rect::from_min_size(
        pos2(
            rect.right() - theme::FOOTER_PAD - BUTTON,
            rect.center().y - BUTTON / 2.0,
        ),
        vec2(BUTTON, BUTTON),
    );
    if icon_button(ui, theme, more, Icon::More, "More actions \u{b7} Ctrl+K").clicked() {
        intents.push(Intent::ShowActions(!state.actions_open));
    }

    let mut right = more.left() - GAP;
    if let Some(action) = view::actions::default_action(state) {
        let divider = right - 1.0;
        ui.painter().vline(
            divider,
            (rect.center().y - 8.0)..=(rect.center().y + 8.0),
            Stroke::new(1.0, theme.stroke),
        );
        right = divider - GAP;

        right = default_button(ui, theme, action, rect, right, &mut intents);
    }

    status(ui, state, theme, rect, right, wall);
    intents
}

/// Draws the button naming the default action, with its shortcut chip.
///
/// Returns its left edge less `GAP`, where the status line must stop.
fn default_button(
    ui: &mut Ui,
    theme: &Theme,
    action: view::actions::Action,
    band: Rect,
    right: f32,
    intents: &mut Vec<Intent>,
) -> f32 {
    let painter = ui.painter().clone();
    let font = theme::font(theme::SIZE_CAPTION, Weight::Regular);
    let label_w = painter
        .layout_no_wrap(action.description.to_string(), font.clone(), theme.text)
        .rect
        .width();
    let key_w = action
        .shortcut
        .map(|s| chip::width(&painter, s) + GAP)
        .unwrap_or(0.0);

    let width = LABEL_PAD_X * 2.0 + label_w + key_w;
    let rect = Rect::from_min_size(
        pos2(right - width, band.center().y - BUTTON / 2.0),
        vec2(width, BUTTON),
    );

    // The accessible name spells out key names rather than chip glyphs.
    let spoken = match action.shortcut {
        Some(key) => format!("{} \u{b7} {}", action.description, chip::spoken(key)),
        None => action.description.to_string(),
    };
    let response = ui.interact(rect, Id::new("files-default-action"), Sense::click());
    let name = spoken.clone();
    response.widget_info(|| {
        eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Button, true, &name)
    });

    if response.hovered() {
        painter.rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.hover);
    }
    painter.text(
        pos2(rect.left() + LABEL_PAD_X, rect.center().y),
        Align2::LEFT_CENTER,
        action.description,
        font,
        if response.hovered() {
            theme.strong
        } else {
            theme.text
        },
    );
    if let Some(key) = action.shortcut {
        chip::draw(
            &painter,
            theme,
            key,
            pos2(rect.left() + LABEL_PAD_X + label_w + GAP, rect.center().y),
        );
    }
    if response.clicked() {
        intents.push(Intent::Act(action.id));
    }
    rect.left() - GAP
}

/// Draws a frameless icon button that fills on hover.
///
/// The tooltip doubles as the accessible name, as in the settings window's
/// `subtle_icon_button`.
fn icon_button(
    ui: &mut Ui,
    theme: &Theme,
    rect: Rect,
    icon: Icon,
    tooltip: &str,
) -> eframe::egui::Response {
    let response = ui.interact(rect, Id::new(("files-footer", tooltip)), Sense::click());
    let name = tooltip.to_owned();
    response.widget_info(|| {
        eframe::egui::WidgetInfo::labeled(eframe::egui::WidgetType::Button, true, &name)
    });

    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, theme::radius(theme::RADIUS_MEDIUM), theme.hover);
    }
    if theme::has_icon(ui.ctx(), icon) {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon.text(),
            theme::icon_font(theme::ICON_NAV),
            if response.hovered() {
                theme.strong
            } else {
                theme.dim
            },
        );
    }
    response.on_hover_text(tooltip)
}

/// Draws the status line: tone glyph, result count and status text.
fn status(ui: &mut Ui, state: &AppState, theme: &Theme, band: Rect, right: f32, wall: SystemTime) {
    let painter = ui.painter().clone();
    let line = view::status::render(state, wall);
    let font = theme::font(theme::SIZE_CAPTION, Weight::Regular);

    let mut left = band.left() + theme::FOOTER_PAD + BUTTON + GAP;

    // A tone glyph accompanies the colour for colour-blind users. Drawn only
    // when there is status text.
    if !line.text.is_empty() {
        painter.text(
            pos2(left, band.center().y),
            Align2::LEFT_CENTER,
            theme.glyph(line.tone),
            theme::font(theme::SIZE_CAPTION, theme.weight(Emphasis::Tone(line.tone))),
            theme.tone(line.tone),
        );
    }
    left += 16.0;

    // Result count, drawn outside the status text's wrap width so it is never
    // truncated.
    let found = view::status::found(state);
    if !found.is_empty() {
        let width = painter
            .layout_no_wrap(found.clone(), font.clone(), Color32::WHITE)
            .rect
            .width();
        painter.text(
            pos2(left, band.center().y),
            Align2::LEFT_CENTER,
            &found,
            font.clone(),
            theme.text,
        );
        super::announce(
            ui,
            Rect::from_min_size(pos2(left, band.top()), vec2(width, band.height())),
            "found",
            &found,
        );
        left += width + COUNT_GAP;
    }

    // Status text, ellipsised so it never runs under the default-action button.
    let stop = (right - GAP).max(left + STATUS_RESERVE.min(right - left).max(0.0));
    let mut job = LayoutJob::single_section(
        line.text.clone(),
        TextFormat {
            font_id: font,
            color: theme.dim,
            ..Default::default()
        },
    );
    job.wrap.max_width = (stop - left).max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('\u{2026}');
    let galley = painter.layout_job(job);
    painter.galley(
        pos2(left, band.center().y - galley.rect.height() / 2.0),
        galley,
        theme.dim,
    );
    // Announce the full, untruncated text.
    super::announce(
        ui,
        Rect::from_min_max(pos2(left, band.top()), pos2(stop, band.bottom())),
        "status",
        &line.text,
    );
}
