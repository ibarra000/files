//! Tests for the palette's contrast ratios, panel band layout and corner radii.
//!
//! The rationale for each rule is in the parent module docs.

use super::*;

/// WCAG relative luminance.
fn luminance(c: Color32) -> f64 {
    fn channel(v: u8) -> f64 {
        let v = v as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG contrast ratio, 1.0 to 21.0.
fn contrast(a: Color32, b: Color32) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// Premultiplied source-over: what `top` looks like painted on `bottom`.
fn over(top: Color32, bottom: Color32) -> Color32 {
    let a = top.a() as f32 / 255.0;
    let mix = |t: u8, b: u8| (t as f32 + b as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(
        mix(top.r(), bottom.r()),
        mix(top.g(), bottom.g()),
        mix(top.b(), bottom.b()),
    )
}

/// Upper bound on the fraction of the desktop visible through the panel.
///
/// Acrylic tints roughly 80% toward the system colour, then `paint_surface`
/// lays `Theme::wash` over it at 60%, so at most `0.4 * 0.2` survives.
const LEAK: f32 = 0.08;

/// The opaque `NeutralBackground1` ground that most ratios are measured against.
///
/// The acrylic path, where `Theme::wash` covers the compositor backdrop, is
/// measured separately in [`what_the_acrylic_path_costs`].
fn base(theme: Theme) -> Color32 {
    opaque(theme.surface)
}

/// The panel ground with the maximum desktop leak of black and of white.
fn through_acrylic(theme: Theme) -> [(&'static str, Color32); 2] {
    let base = base(theme);
    [
        ("over black", over(faded(Color32::BLACK, LEAK), base)),
        ("over white", over(faded(Color32::WHITE, LEAK), base)),
    ]
}

/// Returns the colour with alpha discarded.
fn opaque(c: Color32) -> Color32 {
    Color32::from_rgb(c.r(), c.g(), c.b())
}

fn both() -> [(&'static str, Theme); 2] {
    [("dark", Theme::dark()), ("light", Theme::light())]
}

/// Body text meets WCAG AAA (7:1).
#[test]
fn body_text_is_legible_to_the_aaa_standard() {
    for (name, theme) in both() {
        let bg = base(theme);
        for (role, fg) in [
            ("text", theme.text),
            ("strong", theme.strong),
            ("input", theme.input),
        ] {
            let ratio = contrast(fg, bg);
            assert!(ratio >= 7.0, "{name}/{role}: {ratio:.2}:1, want 7:1");
        }
    }
}

/// Secondary text (folders, status line, hints) meets WCAG AA (4.5:1).
#[test]
fn secondary_text_is_legible_to_the_aa_standard() {
    for (name, theme) in both() {
        let bg = base(theme);
        // `accent` is not drawn as text on the panel; it is checked as a bar in
        // `a_boundary_is_as_loud_as_its_job` and as text on the settings tile.
        for (role, fg) in [
            ("dim", theme.dim),
            ("caption", theme.caption),
            ("match_run", theme.match_run),
        ] {
            let ratio = contrast(fg, bg);
            assert!(ratio >= 4.5, "{name}/{role}: {ratio:.2}:1, want 4.5:1");
        }
    }
}

/// Text on tinted row and chip backgrounds meets AA.
#[test]
fn text_stays_legible_on_every_row_background() {
    for (name, theme) in both() {
        {
            for (role, fill, fg) in [
                ("selection", theme.selection, theme.strong),
                ("hover", theme.hover, theme.text),
                ("badge", theme.badge_bg, theme.badge_fg),
                ("key cap", theme.key_bg, theme.key_fg),
                ("text_selection", theme.text_selection, theme.strong),
            ] {
                let ratio = contrast(fg, over(fill, base(theme)));
                assert!(ratio >= 4.5, "{name}/{role}: {ratio:.2}:1, want 4.5:1");
            }
        }
    }
}

/// Text in the search field's well meets the same bars as on the panel.
#[test]
fn the_code_is_legible_in_the_well_it_is_typed_into() {
    for (name, theme) in both() {
        // The well is opaque, so no desktop leak applies.
        let well = opaque(theme.well);
        for (role, fg, want) in [
            ("input", theme.input, 7.0),
            ("dim", theme.dim, 4.5),
            ("caption", theme.caption, 4.5),
        ] {
            let ratio = contrast(fg, well);
            assert!(
                ratio >= want,
                "{name}/{role}: {ratio:.2}:1 in the well, want {want}:1"
            );
        }
    }
}

/// Settings text is legible on the tile it is drawn on.
#[test]
fn a_setting_is_legible_on_the_tile_it_is_drawn_on() {
    for (name, theme) in both() {
        let card = theme.card;
        for (role, fg, want) in [
            ("text", theme.text, 7.0),
            ("strong", theme.strong, 7.0),
            ("dim", theme.dim, 4.5),
            ("accent", theme.accent, 4.5),
            ("warn", theme.tone(Tone::Warn), 4.5),
            ("bad", theme.tone(Tone::Bad), 4.5),
        ] {
            let ratio = contrast(fg, card);
            assert!(
                ratio >= want,
                "{name}/{role} on the card: {ratio:.2}:1, want {want}:1"
            );
        }
    }
}

/// Control values are legible on every control fill state.
#[test]
fn a_value_is_legible_in_the_control_that_holds_it() {
    for (name, theme) in both() {
        for (state, fill) in [
            ("control", theme.control),
            ("hover", theme.control_hover),
            ("active", theme.control_active),
        ] {
            for (role, fg, want) in [("text", theme.text, 7.0), ("dim", theme.dim, 4.5)] {
                let ratio = contrast(fg, fill);
                assert!(
                    ratio >= want,
                    "{name}/{role} on {state}: {ratio:.2}:1, want {want}:1"
                );
            }
        }
    }
}

/// Content on the accent fill (primary button labels, switch knobs) meets AA.
///
/// AA rather than AAA because Fluent's brand blue cannot reach 7:1: white on
/// `#0f6cbd` is 5.4:1 and on `#115ea3` is 6.7:1.
#[test]
fn the_filled_accent_carries_what_is_drawn_on_it() {
    for (name, theme) in both() {
        let ratio = contrast(theme.accent_fg, theme.accent_fill);
        assert!(ratio >= 4.5, "{name}: accent {ratio:.2}:1, want 4.5:1");
    }
}

/// The settings tile differs from the window by a visible but small step.
#[test]
fn a_tile_is_a_shade_off_the_window_and_no_more() {
    for (name, theme) in both() {
        let window = opaque(theme.surface);
        let ratio = contrast(theme.card, window);
        assert!(
            (1.02..1.35).contains(&ratio),
            "{name}: the card is {ratio:.3}:1 against the window"
        );
    }
}

/// A text box is distinguishable from its tile by fill or by its bottom rule.
///
/// In Fluent a filled input and its card are both `NeutralBackground3`
/// (1.000:1); the box is marked by a one-pixel `NeutralStrokeAccessible` rule.
/// Either signal satisfies the test.
#[test]
fn a_text_box_is_findable_on_the_tile_it_sits_on() {
    for (name, theme) in both() {
        let fill = contrast(opaque(theme.well), theme.card);
        let rule = contrast(theme.accessible, opaque(theme.well));
        assert!(
            fill >= 1.15 || rule >= 3.0,
            "{name}: the well is {fill:.3}:1 against the card and its rule is \
             {rule:.2}:1 on it, so the box is neither sunk nor outlined"
        );
    }
}

/// Stroke contrast is ordered by role: divider < control outline < accessible.
///
/// Only the accessible stroke must meet WCAG 1.4.11 (3:1); Fluent's divider
/// (`NeutralStroke2`) is intentionally fainter. The accent bar must reach 3:1
/// on every ground it is drawn over.
#[test]
fn a_boundary_is_as_loud_as_its_job() {
    for (name, theme) in both() {
        let bg = base(theme);
        let edge = contrast(theme.edge, bg);
        let stroke = contrast(theme.stroke, bg);
        let accessible = contrast(theme.accessible, bg);
        assert!(
            edge < stroke && stroke < accessible,
            "{name}: the three strokes are {edge:.2} / {stroke:.2} / \
             {accessible:.2}, which is not an order"
        );
        assert!(
            accessible >= 3.0,
            "{name}: the accessible stroke is {accessible:.2}:1, and it is \
             the one WCAG 1.4.11 is about"
        );
        // The divider must still be visible.
        assert!(
            edge > 1.05,
            "{name}: the band rule is invisible at {edge:.3}:1"
        );
    }
    // The accent bar, on every ground it is drawn over.
    for (name, theme) in both() {
        for (where_, bg) in [
            ("the window", base(theme)),
            ("a tile", theme.card),
            ("a selected row", theme.selection),
        ] {
            let ratio = contrast(theme.accent, bg);
            assert!(
                ratio >= 3.0,
                "{name}: the accent bar is {ratio:.2}:1 on {where_}"
            );
        }
    }
}

/// `apply_style` writes both egui styles, since the configured theme can differ
/// from the system theme egui follows.
#[test]
fn a_style_is_written_into_both_of_the_toolkits_themes() {
    let ctx = eframe::egui::Context::default();
    let theme = Theme::dark();
    apply_style(&ctx, &theme);

    for which in [eframe::egui::Theme::Light, eframe::egui::Theme::Dark] {
        let style = ctx.style_of(which);
        assert_eq!(
            style.visuals.panel_fill,
            Color32::from_rgb(theme.surface.r(), theme.surface.g(), theme.surface.b()),
            "{which:?} was left undressed"
        );
        assert!(
            !style.visuals.text_options.subpixel_binning,
            "{which:?} lost the sharpening that `fonts::sharpen` set"
        );
    }
}

/// The scroll track and text boxes use different colours; egui merges them if
/// `text_edit_bg_color` is unset.
#[test]
fn a_text_box_and_a_scroll_track_are_not_the_same_colour() {
    let ctx = eframe::egui::Context::default();
    for theme in [Theme::dark(), Theme::light()] {
        apply_style(&ctx, &theme);
        let v = &ctx.style_of(eframe::egui::Theme::Dark).visuals;
        assert_eq!(
            v.text_edit_bg_color,
            Some(opaque(theme.well)),
            "a text box is not sunk into the well"
        );
        assert_ne!(
            v.extreme_bg_color,
            v.text_edit_bg_color.unwrap(),
            "the scroll track and the text box resolved to one colour"
        );
    }
}

/// `bg_fill` (scroll handle, slider rail) differs from `weak_bg_fill` (button).
#[test]
fn a_scroll_handle_is_not_the_colour_of_a_button() {
    let ctx = eframe::egui::Context::default();
    for theme in [Theme::dark(), Theme::light()] {
        apply_style(&ctx, &theme);
        let w = &ctx.style_of(eframe::egui::Theme::Dark).visuals.widgets;
        for (state, v) in [
            ("inactive", &w.inactive),
            ("hovered", &w.hovered),
            ("active", &w.active),
            ("open", &w.open),
        ] {
            assert_ne!(
                v.bg_fill, v.weak_bg_fill,
                "{state}: the handle and the button share a fill"
            );
        }
    }
}

/// Widgets do not expand on hover, so aligned tiles stay aligned.
#[test]
fn nothing_swells_when_the_pointer_is_over_it() {
    let ctx = eframe::egui::Context::default();
    apply_style(&ctx, &Theme::light());
    let w = &ctx.style_of(eframe::egui::Theme::Dark).visuals.widgets;
    for (state, v) in [
        ("noninteractive", &w.noninteractive),
        ("inactive", &w.inactive),
        ("hovered", &w.hovered),
        ("active", &w.active),
        ("open", &w.open),
    ] {
        assert_eq!(v.expansion, 0.0, "{state} expands");
    }
}

/// The well differs from the panel by a visible but small step.
#[test]
fn the_well_is_a_shade_off_the_panel_and_no_more() {
    for (name, theme) in both() {
        let ratio = contrast(opaque(theme.well), base(theme));
        assert!(
            (1.02..1.35).contains(&ratio),
            "{name}: the well is {ratio:.3}:1 against the panel"
        );
    }
}

/// Match highlighting stays legible on the selected row.
#[test]
fn a_matched_run_is_legible_on_the_row_it_is_most_likely_to_be_on() {
    for (name, theme) in both() {
        let ratio = contrast(theme.match_run, theme.selection);
        assert!(
            ratio >= 4.5,
            "{name}: the match run is {ratio:.2}:1 on the selected row"
        );
    }
}

/// The selected row is distinguishable from a hovered one, since Enter opens
/// the selection.
///
/// In Fluent dark, hover (`NeutralBackground1Hover`, `#3d3d3d`, 1.34:1) is
/// brighter than selection (`NeutralBackground1Selected`, `#383838`, 1.24:1),
/// so the distinguishing signal is the accent bar on the selected row. The
/// fills must differ and be visible, and the bar must reach 3:1.
#[test]
fn the_selected_row_is_never_confusable_with_the_hovered_one() {
    for (name, theme) in both() {
        assert_ne!(
            theme.selection, theme.hover,
            "{name}: the selected and hovered rows are drawn the same"
        );
        let bg = base(theme);
        for (role, fill) in [("selection", theme.selection), ("hover", theme.hover)] {
            let ratio = contrast(fill, bg);
            assert!(ratio > 1.05, "{name}/{role}: invisible at {ratio:.3}:1");
        }
        // Only the selected row has the accent bar.
        let bar = contrast(theme.accent, theme.selection);
        assert!(
            bar >= 3.0,
            "{name}: the accent bar is {bar:.2}:1 on the selected row, and it \
             is the only thing telling it from a hovered one"
        );
    }
}

/// Every status tone meets AA on the panel ground.
#[test]
fn every_tone_is_legible_on_every_ground() {
    for (name, theme) in both() {
        let bg = base(theme);
        for tone in [Tone::Normal, Tone::Busy, Tone::Good, Tone::Warn, Tone::Bad] {
            let ratio = contrast(theme.tone(tone), bg);
            assert!(ratio >= 4.5, "{name}/{tone:?}: {ratio:.2}:1, want 4.5:1");
        }
    }
}

/// Records contrast floors on the acrylic path, where up to [`LEAK`] of the
/// desktop shows through `Theme::wash`.
///
/// A regression guard, not a standard: body text stays AAA, while secondary
/// text drops from about 5:1 to about 4:1 at the extremes (below AA). To raise
/// it, adjust the wash rather than the text colours.
///
/// Row fills are not checked: on light theme a selected row (`#ebebeb`) can
/// reach 1.000:1 against the drifted ground. The accent bar distinguishes it
/// and is checked in `the_selected_row_is_never_confusable_with_the_hovered_one`.
#[test]
fn what_the_acrylic_path_costs() {
    for (name, theme) in both() {
        for (ground, bg) in through_acrylic(theme) {
            for (role, fg, floor) in [
                ("text", theme.text, 7.0),
                ("input", theme.input, 7.0),
                ("dim", theme.dim, 3.9),
                ("caption", theme.caption, 3.9),
                ("accent", theme.accent, 3.9),
            ] {
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= floor,
                    "{name}/{role} {ground}: {ratio:.2}:1, and the recorded \
                     floor is {floor}:1 - the acrylic path got worse"
                );
            }
        }
    }
}

/// Each tone has a distinct glyph, so status is not conveyed by colour alone.
#[test]
fn no_tone_depends_on_colour_alone() {
    let mut seen: Vec<char> = TONE_GLYPH.to_vec();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), TONE_GLYPH.len(), "two tones share a glyph");
}

/// Each `Emphasis` maps to a distinct colour and weight combination.
#[test]
fn every_emphasis_has_a_distinct_appearance() {
    for (name, theme) in both() {
        // Compared on colour and weight together.
        let mut looks = vec![];
        for emphasis in [
            Emphasis::Body,
            Emphasis::Dim,
            Emphasis::Strong,
            Emphasis::Accent,
        ] {
            looks.push((
                theme.emphasis(emphasis).to_array(),
                theme.weight(emphasis) as u8,
            ));
        }
        // Colour alone is not unique: Body and Strong are both
        // `NeutralForeground1`, differing only in weight (400 vs 600).
        looks.sort();
        looks.dedup();
        assert_eq!(looks.len(), 4, "{name}: two emphases are drawn identically");
        assert_eq!(
            theme.emphasis(Emphasis::Tone(Tone::Bad)),
            theme.tone(Tone::Bad)
        );
    }
}

/// `faded` scales alpha and clamps out-of-range factors.
#[test]
fn fading_a_colour_scales_it_rather_than_replacing_it() {
    let opaque = Color32::from_rgb(0x80, 0x40, 0x20);
    assert_eq!(faded(opaque, 1.0), opaque);
    assert_eq!(faded(opaque, 0.0), Color32::TRANSPARENT);
    assert!(faded(opaque, 0.5).a() < opaque.a());
    // Out-of-range input is clamped.
    assert_eq!(faded(opaque, 2.0), opaque);
    assert_eq!(faded(opaque, -1.0), Color32::TRANSPARENT);
}

/// The painted surface is opaque and the acrylic wash is 60%, consistent
/// with [`LEAK`]. See `paint_surface`.
#[test]
fn the_panel_is_a_wash_over_acrylic_and_a_solid_window_without_it() {
    for (name, theme) in both() {
        assert_eq!(
            theme.surface.a(),
            255,
            "{name}: the painted ground is the one colour the palette is \
             allowed to be certain of"
        );
        assert_eq!(theme.wash.a(), 0x99, "{name}: the wash is not 60%");
        // What passes the wash, times what passes acrylic's 80% tint.
        let past = (1.0 - theme.wash.a() as f32 / 255.0) * 0.2;
        assert!(
            past <= LEAK,
            "{name}: {past:.3} of the desktop gets through, and `LEAK`              claims {LEAK}"
        );
    }
}

/// The five panel bands sum to `PANEL_H`.
#[test]
fn the_bands_add_up_to_the_panel() {
    let bands = HEADER_H + DIVIDER + CONTENT_H + DIVIDER + FOOTER_H;
    assert_eq!(
        bands, PANEL_H,
        "the bands come to {bands}pt, not {PANEL_H}pt"
    );
}

/// A full page of rows plus its heading fits the content band, with less than
/// one row to spare, for both layouts.
#[test]
fn a_full_page_fits_the_content_band() {
    for (layout, rows) in [
        (ResultLayout::Compact, crate::config::VISIBLE_ROWS),
        (ResultLayout::Detailed, crate::config::VISIBLE_ROWS_DETAILED),
    ] {
        let wanted = BAND_PAD * 2.0 + HEADING_H + row_pitch(layout) * rows as f32 - ROW_GAP;
        assert!(
            wanted <= CONTENT_H,
            "{rows} {layout:?} rows want {wanted}pt of a {CONTENT_H}pt band"
        );
        assert!(
            CONTENT_H - wanted < row_h(layout),
            "another {layout:?} row would fit: {wanted}pt of {CONTENT_H}pt used"
        );
    }
}

// -- corners ----------------------------------------------------------------

/// No painted radius reaches the window corner radius.
///
/// DWM rounds the window (`DWMWCP_ROUND`) in device pixels, while egui paints
/// in points; the two diverge under zoom, during `WM_DPICHANGED`, or across
/// mixed-DPI monitors. Only DWM draws the window corner. `RADIUS_CIRCULAR`
/// (pill shapes) is exempt.
#[test]
fn nothing_this_program_paints_is_rounded_like_a_window() {
    /// Windows 11 window corner radius; Fluent `borderRadiusXLarge`.
    const WINDOW_CORNER: u8 = 8;
    for r in [RADIUS_SMALL, RADIUS_MEDIUM, RADIUS_LARGE] {
        assert!(
            r < WINDOW_CORNER,
            "{r} is a window corner, and this program does not draw those"
        );
    }
}

/// The radius ramp matches Fluent's published values in ascending order.
#[test]
fn the_three_rungs_are_three_different_numbers() {
    let ramp = [RADIUS_SMALL, RADIUS_MEDIUM, RADIUS_LARGE];
    assert_eq!(ramp, [2, 4, 6], "these are Fluent's published radii");
    assert!(ramp.windows(2).all(|w| w[0] < w[1]), "out of order");
}
