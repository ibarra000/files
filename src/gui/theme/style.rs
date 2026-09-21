//! How the toolkit's own widgets are dressed.
//!
//! Split out of [`super`] for the reason its sibling `tests` was: the file
//! outgrew the size this repository holds a module to. The palette is next
//! door; this is the one place that hands it to egui.

use eframe::egui::Color32;
use eframe::egui::epaint::Shadow;

use super::{
    PANEL_RADIUS, SIZE_CHIP, SIZE_HEADLINE, SIZE_ROW, SIZE_SMALL, Theme, Weight, font, radius,
};
use crate::view::status::Tone;

/// A control's corner. Windows 11 rounds a button and a text box by four.
pub const CONTROL_RADIUS: u8 = 4;

/// The tile a setting is drawn on, and the padding inside it.
pub const CARD_RADIUS: u8 = 4;
pub const CARD_PAD: f32 = 12.0;

/// Between one row and the next: two settings, or two results.
///
/// Five, which is what Ueli gives a list of results and what a stack of
/// setting tiles wanted independently. Rows that touch read as a table; five
/// points of ground showing between them reads as a list of things, and is
/// what makes a rounded corner visible at all.
pub const ROW_GAP: f32 = 5.0;
/// Between one group of settings and the next. Large, and that is the point:
/// it is the only thing separating two groups, because a group heading is
/// smaller than the labels under it.
pub const GROUP_GAP: f32 = 40.0;
/// The margin the settings window keeps around its content.
pub const CONTENT_PAD: f32 = 20.0;
/// How wide the list of pages is.
pub const NAV_W: f32 = 260.0;

/// The shortest a control may be, so a row's right-hand column does not change
/// height with what is in it.
pub const CONTROL_H: f32 = 28.0;

/// Dresses egui's own widgets in this palette.
///
/// # Why there was no such function until now
///
/// Because until now nothing needed one. The panel does not use a single egui
/// widget: `gui::panel` and `gui::row` between them call
/// `ui.interact` and then paint, and every colour they use comes from
/// [`Theme`] directly. A `Style` would have configured nothing.
///
/// The settings window is the opposite. It is a form, it is made of buttons,
/// boxes and drop-downs, and until this function existed every one of them was
/// drawn in egui's stock blue-grey - which is why a window in this program did
/// not look like this program.
///
/// # It is safe to apply to everything
///
/// Applied to the whole context rather than scoped to the settings viewport,
/// and that is sound precisely because of the paragraph above: there is no
/// egui widget on the panel for this to reach. `tests/panel.rs` and the image
/// snapshots beside it are the standing proof. Scoping it would mean applying
/// it inside the child viewport's own pass, which is a second place to keep in
/// step for no benefit anybody can see.
///
/// Both themes, like [`crate::gui::fonts::sharpen`]: egui keeps a `Style` per
/// light and dark and this program switches between them at runtime, so
/// setting only the active one would dress the window until somebody changed
/// theme.
pub fn apply_style(ctx: &eframe::egui::Context, theme: &Theme) {
    use eframe::egui::{Margin, Stroke, TextStyle, Vec2};

    let solid = |c: Color32| Color32::from_rgb(c.r(), c.g(), c.b());
    let hairline = Stroke::new(1.0, theme.faint);

    // A control's states, which differ in the fill and in how firm the outline
    // is. The text on all of them is the same colour: a button whose label
    // brightens under the pointer is a button that looks disabled until you
    // touch it.
    //
    // `weak_bg_fill` and `bg_fill` are two different jobs and are given two
    // different colours, which is the one thing about this struct that is easy
    // to get wrong. `weak_bg_fill` is what a button and a drop-down paint.
    // `bg_fill` is the slider rail, the *scroll-bar handle* and the box of a
    // checkbox - so filling it with the control colour puts a scroll handle the
    // same shade as a text box, which reads as a gutter rather than as a grip.
    //
    // The checkbox is the third of those and this deliberately does not suit
    // it: the settings window draws switches, by hand, and calls `ui.checkbox`
    // nowhere. Anybody who reaches for one will get a grey-filled box, and this
    // paragraph is why.
    let widget = |fill: Color32, grip: Color32, stroke: Stroke| {
        eframe::egui::style::WidgetVisuals {
            weak_bg_fill: fill,
            bg_fill: grip,
            bg_stroke: stroke,
            corner_radius: radius(CONTROL_RADIUS),
            fg_stroke: Stroke::new(1.0, theme.text),
            // Zero, deliberately. egui's default grows a hovered widget by a
            // point; in a column of tiles that have to line up, that reads as
            // jitter rather than as feedback.
            expansion: 0.0,
        }
    };

    ctx.all_styles_mut(|style| {
        // Widget text in the same typeface and at the same sizes as everything
        // this program writes by hand. Without this a button is 14 pt and the
        // label beside it is 15, which reads as a mistake because it is one.
        style.text_styles = [
            (TextStyle::Small, font(SIZE_CHIP, Weight::Regular)),
            (TextStyle::Body, font(SIZE_ROW, Weight::Regular)),
            (TextStyle::Button, font(SIZE_ROW, Weight::Regular)),
            (TextStyle::Heading, font(SIZE_HEADLINE, Weight::Semibold)),
            (TextStyle::Monospace, font(SIZE_SMALL, Weight::Regular)),
        ]
        .into();

        let spacing = &mut style.spacing;
        spacing.item_spacing = Vec2::new(8.0, ROW_GAP);
        spacing.button_padding = Vec2::new(12.0, 6.0);
        spacing.interact_size = Vec2::new(40.0, CONTROL_H);
        spacing.menu_margin = Margin::same(4);
        spacing.window_margin = Margin::same(CARD_PAD as i8);
        spacing.combo_width = 140.0;
        spacing.text_edit_width = 240.0;

        // Ueli's scrollbar, which is thinner than egui's and always there. A
        // form this long needs a position indicator; it does not need a
        // twelve-point gutter taken out of the reading column to get one.
        let scroll = &mut spacing.scroll;
        scroll.bar_width = 6.0;
        scroll.bar_inner_margin = 4.0;
        scroll.bar_outer_margin = 0.0;
        scroll.floating = false;

        let v = &mut style.visuals;
        v.dark_mode = theme.dark;
        // Opaque, unlike the panel's. These windows are documents: they sit
        // over other programs for minutes at a time, and text on a translucent
        // ground is harder to read the longer you read it.
        v.panel_fill = solid(theme.surface);
        v.window_fill = solid(theme.surface);
        v.window_stroke = Stroke::new(1.0, solid(theme.edge));
        v.window_corner_radius = radius(PANEL_RADIUS);
        v.menu_corner_radius = radius(CONTROL_RADIUS);
        v.faint_bg_color = theme.card;
        // The scroll-bar *track*, which is what this field actually is - the
        // text box below only falls back to it when `text_edit_bg_color` is
        // unset, and it is not. A track the colour of a tile is a groove in
        // the page; a track the colour of the well would be a black stripe
        // down the edge of a light window.
        v.extreme_bg_color = theme.card;
        // What a text box is sunk into. The same trough the search field uses,
        // so a box in the settings window and the one on the panel are the
        // same idea rather than two.
        v.text_edit_bg_color = Some(solid(theme.well));
        v.code_bg_color = solid(theme.well);
        // Selected *text*, which is what this is: egui uses it inside a
        // `TextEdit` and behind a selectable label. The window's own "this
        // one" is `accent_fill`, painted by hand where it is meant.
        v.selection = eframe::egui::style::Selection {
            bg_fill: theme.text_selection,
            stroke: Stroke::new(1.0, theme.strong),
        };
        v.text_cursor.stroke = Stroke::new(1.0, theme.caret);
        // A pinned setting is drawn disabled rather than hidden, so its value
        // has to stay readable while it is refused. egui's default fades
        // harder than that.
        v.disabled_alpha = 0.45;
        v.striped = false;
        v.warn_fg_color = theme.tone(Tone::Warn);
        v.error_fg_color = theme.tone(Tone::Bad);
        v.hyperlink_color = theme.accent;
        v.button_frame = true;
        // Left to egui elsewhere, off here: a window this program draws has no
        // shadow of its own, because the compositor already gives it one.
        v.window_shadow = Shadow::NONE;
        v.popup_shadow = Shadow::default();

        let w = &mut v.widgets;
        // A label, a group heading, a panel separator: not interactive, so not
        // outlined in anything firmer than a rule. Its `bg_stroke` is what
        // `SidePanel` draws its dividing line with.
        w.noninteractive = widget(theme.card, theme.faint, Stroke::new(1.0, theme.edge));
        w.inactive = widget(theme.control, theme.faint, hairline);
        w.hovered = widget(theme.control_hover, theme.dim, Stroke::new(1.0, theme.dim));
        w.active = widget(
            theme.control_active,
            theme.dim,
            Stroke::new(1.0, theme.accent),
        );
        w.open = widget(theme.control_active, theme.dim, hairline);

        // Re-asserted rather than assumed. `gui::fonts::sharpen` turns this
        // off once at startup, and this function runs after it and again on
        // every theme change; it mutates a `Style` in place rather than
        // replacing one, so the setting does survive - but "does survive"
        // is a property of how this is written, and writing it down here
        // costs a line and outlives whoever knew.
        style.visuals.text_options.subpixel_binning = false;
    });
}
