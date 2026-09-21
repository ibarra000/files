//! The footer: what the program knows, and what Enter will do.
//!
//! Ueli's arrangement. A subtle gear on the left; on the right a subtle
//! button labelled with the *default action's description* and an `↵` chip, a
//! vertical divider, and a `⋮` that opens everything else. Between them,
//! whatever the program has to say.
//!
//! # What went, and why it is not a loss
//!
//! A row of key-hint chips, fitted from the right, dropping the
//! lowest-priority hint whenever the line got tight. It was a good answer to
//! the question it was asked, and the question was wrong: it listed which
//! keys were live somewhere on the panel, which is not what somebody about to
//! press Enter wants to know.
//!
//! Three things it could not do, and this can:
//!
//! * Say what Enter will actually do. `Enter open` is true of every launcher
//!   ever written; `Open as one document` is the state `F2` left the program
//!   in, which is the fact the viewer chip existed to carry and - at
//!   `Priority::Normal` and six hundred points wide - was the first thing
//!   dropped.
//! * Offer the keys it names. A chip was a hint you could click, and only
//!   four of them were wired; a menu row is the action.
//! * Hold more than fits. `Ctrl+O`, `Ctrl+D`, `Ctrl+E` and the two copies
//!   would have needed five more chips on a line that was already dropping
//!   them.
//!
//! What is genuinely gone is the *advertisement* of `Ctrl+Q` and `Esc`, which
//! were listed and deliberately not clickable. Escape is the key everybody
//! tries first, and `Ctrl+Q` is on the tray icon's menu, which is where a
//! thing that ends the session belongs.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Align2, Color32, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::time::SystemTime;

use super::chip;
use crate::app::state::AppState;
use crate::app::state::pointer::Intent;
use crate::gui::theme::{self, Icon, Theme, Weight};
use crate::view::actions::ActionId;
use crate::view::{self, Emphasis};

/// A square button holding one mark.
const BUTTON: f32 = 24.0;

/// The air between two things on this line.
const GAP: f32 = 8.0;

/// The air inside the button that names the default action.
const LABEL_PAD_X: f32 = 8.0;

/// Room kept for the status line, whether or not it has anything to say.
///
/// Wide enough for the longest transient - `Checking the drive…` - with a
/// warning longer than that left to truncate, which is what truncation is
/// for. Fixed rather than fitted: a line that reflowed every time the phase
/// changed would move under somebody reading it.
const STATUS_RESERVE: f32 = 120.0;

/// The gap between the result count and the status prose beside it.
const COUNT_GAP: f32 = 12.0;

pub fn show(
    ui: &mut Ui,
    state: &AppState,
    theme: &Theme,
    rect: Rect,
    wall: SystemTime,
) -> Vec<Intent> {
    let mut intents = Vec::new();

    // The gear, first and on the left, exactly where Ueli has it. It is the
    // only control on this line that is about the program rather than about
    // the row the cursor is on.
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

    // And the two on the right, laid out from the edge inwards.
    let more = Rect::from_min_size(
        pos2(
            rect.right() - theme::FOOTER_PAD - BUTTON,
            rect.center().y - BUTTON / 2.0,
        ),
        vec2(BUTTON, BUTTON),
    );
    if icon_button(ui, theme, more, Icon::More, "Everything else \u{b7} Ctrl+K").clicked() {
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

/// The button that says what Enter will do, with its `↵` beside it.
///
/// Returns its left edge, so the status line knows where it has to stop.
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
        .layout_no_wrap(action.description.to_owned(), font.clone(), theme.text)
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

    // The description is the accessible name, and the key is said in words
    // beside it: `^ ↵` is a pair of pictures, and a reader handed those would
    // say "circumflex".
    let spoken = match action.shortcut {
        Some(key) => format!("{} \u{b7} {}", action.description, chip::spoken(key)),
        None => action.description.to_owned(),
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

/// A button with no frame until it is touched, holding one mark.
///
/// The tooltip is also the accessible name, because a button with no text has
/// nothing else to offer a screen reader - the same rule the settings
/// window's `subtle_icon_button` keeps, applied to a painted panel.
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

/// The tone glyph, the count, and whatever the program has to say.
fn status(ui: &mut Ui, state: &AppState, theme: &Theme, band: Rect, right: f32, wall: SystemTime) {
    let painter = ui.painter().clone();
    let line = view::status::render(state, wall);
    let font = theme::font(theme::SIZE_CAPTION, Weight::Regular);

    let mut left = band.left() + theme::FOOTER_PAD + BUTTON + GAP;

    // The glyph, then the words. Colour alone does not carry the difference
    // between "updated" and "unreachable" for about one man in twelve.
    //
    // Only when there is something to say. A standing dot beside an empty
    // line is a mark that means nothing, and on the quiet panel it would be
    // the only thing on the footer besides the gear.
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

    // The reserved slot: drawn before the prose and outside its wrap width,
    // so it is the one part of this line that truncation cannot reach.
    //
    // Strong rather than dim. How much a code found is something somebody is
    // looking for, not a note about the state of the program.
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

    // Whatever is left, truncated rather than overlapped: a status line
    // running under the button that says what Enter does is unreadable, and
    // the button is the part somebody is about to act on.
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
    // The untruncated text, deliberately. What is painted may be ellipsised
    // to fit; what is *said* has no width to fit into, and a reader that got
    // the same abbreviation as the screen would be worse off than one that
    // got the sentence.
    super::announce(
        ui,
        Rect::from_min_max(pos2(left, band.top()), pos2(stop, band.bottom())),
        "status",
        &line.text,
    );
}
