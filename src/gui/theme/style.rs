//! Styling for egui's built-in widgets, derived from the palette in [`super`].

use eframe::egui::Color32;
use eframe::egui::epaint::Shadow;

use super::{RADIUS_MEDIUM, SCROLL_W, SIZE_BODY, SIZE_CAPTION, Theme, Weight, font, radius};
use crate::view::status::Tone;

/// Padding inside a settings tile. Its corner radius is [`super::RADIUS_MEDIUM`].
pub const CARD_PAD: f32 = 12.0;

/// Vertical gap between adjacent rows (settings tiles or results).
///
/// Large enough that rows read as separate items and rounded corners show.
pub const ROW_GAP: f32 = 5.0;
/// Gap between settings groups. It is the main group separator, since group
/// headings are no larger than the labels under them.
pub const GROUP_GAP: f32 = 40.0;
/// Margin around the settings window content.
pub const CONTENT_PAD: f32 = 20.0;
/// Width of the settings nav column.
pub const NAV_W: f32 = 260.0;

/// Minimum control height, so a row's height does not depend on its control.
pub const CONTROL_H: f32 = 28.0;

/// Applies the palette to egui's built-in widgets.
///
/// Only the settings window uses egui widgets; the panel paints everything by
/// hand from [`Theme`]. Applying the style to the whole context is therefore
/// safe, and `tests/panel.rs` with its image snapshots guards that.
///
/// Sets both the light and dark styles, as `crate::gui::fonts::sharpen` does,
/// because the theme can switch at runtime.
pub fn apply_style(ctx: &eframe::egui::Context, theme: &Theme) {
    use eframe::egui::{Margin, Stroke, TextStyle, Vec2};

    let hairline = Stroke::new(1.0, theme.stroke);

    // Widget states differ in fill and outline; text colour stays constant so
    // idle buttons do not look disabled.
    //
    // `weak_bg_fill` is the button and combo box fill. `bg_fill` is the slider
    // rail, scroll-bar handle and checkbox box, so it gets the grip colour to
    // keep the scroll handle distinct from text boxes. Checkboxes are not used
    // (the settings window draws its own switches) and would render grey.
    let widget = |fill: Color32, grip: Color32, stroke: Stroke| {
        eframe::egui::style::WidgetVisuals {
            weak_bg_fill: fill,
            bg_fill: grip,
            bg_stroke: stroke,
            corner_radius: radius(RADIUS_MEDIUM),
            fg_stroke: Stroke::new(1.0, theme.text),
            // egui's default grows hovered widgets by a point, which makes
            // aligned tiles jitter.
            expansion: 0.0,
        }
    };

    ctx.all_styles_mut(|style| {
        // Match the typeface and sizes of hand-drawn text.
        style.text_styles = [
            (TextStyle::Small, font(SIZE_CAPTION, Weight::Regular)),
            (TextStyle::Body, font(SIZE_BODY, Weight::Regular)),
            (TextStyle::Button, font(SIZE_BODY, Weight::Regular)),
            // Fluent `body1Strong`: body size, heavier weight.
            (TextStyle::Heading, font(SIZE_BODY, Weight::Semibold)),
            (TextStyle::Monospace, font(SIZE_CAPTION, Weight::Regular)),
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

        // A thin, always-visible scroll bar. The 4 pt inner margin suits the
        // settings window; the panel overrides it to zero because the results
        // band has its own padding. See `gui::panel::list`.
        let scroll = &mut spacing.scroll;
        scroll.bar_width = SCROLL_W;
        scroll.bar_inner_margin = 4.0;
        scroll.bar_outer_margin = 0.0;
        scroll.floating = false;

        let v = &mut style.visuals;
        v.dark_mode = theme.dark;
        // Opaque, unlike the panel, because long-lived text windows read
        // better on a solid background.
        v.panel_fill = theme.surface;
        v.window_fill = theme.surface;
        v.window_stroke = Stroke::new(1.0, theme.edge);
        v.window_corner_radius = radius(RADIUS_MEDIUM);
        v.menu_corner_radius = radius(RADIUS_MEDIUM);
        v.faint_bg_color = theme.card;
        // The scroll-bar track (`NeutralBackground4`). Text boxes use
        // `text_edit_bg_color` below; the two must differ or egui draws them
        // as one surface, which a test checks.
        v.extreme_bg_color = theme.track;
        // The same well as the panel's search field.
        v.text_edit_bg_color = Some(theme.well);
        v.code_bg_color = theme.well;
        // Selected text in a `TextEdit` or selectable label. Selected items use
        // `accent_fill`, painted by hand.
        v.selection = eframe::egui::style::Selection {
            bg_fill: theme.text_selection,
            stroke: Stroke::new(1.0, theme.strong),
        };
        v.text_cursor.stroke = Stroke::new(1.0, theme.caret);
        // Pinned settings are drawn disabled and must stay readable; egui's
        // default fades too much.
        v.disabled_alpha = 0.45;
        v.striped = false;
        v.warn_fg_color = theme.tone(Tone::Warn);
        v.error_fg_color = theme.tone(Tone::Bad);
        v.hyperlink_color = theme.accent;
        v.button_frame = true;
        // The compositor already shadows top-level windows.
        v.window_shadow = Shadow::NONE;
        v.popup_shadow = Shadow::default();

        let w = &mut v.widgets;
        // Non-interactive elements. `bg_stroke` is also the `SidePanel`
        // divider.
        w.noninteractive = widget(theme.card, theme.grip, Stroke::new(1.0, theme.edge));
        w.inactive = widget(theme.control, theme.grip, hairline);
        w.hovered = widget(theme.control_hover, theme.dim, Stroke::new(1.0, theme.dim));
        w.active = widget(
            theme.control_active,
            theme.dim,
            Stroke::new(1.0, theme.accent),
        );
        w.open = widget(theme.control_active, theme.dim, hairline);

        // Also set by `gui::fonts::sharpen` at startup; re-asserted here so it
        // does not depend on this function mutating the style in place.
        style.visuals.text_options.subpixel_binning = false;
    });
}
