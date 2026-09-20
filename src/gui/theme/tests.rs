//! Every ratio the palette claims, asserted.
//!
//! Split out of [`super`] because the file outgrew the size this repository
//! holds a module to, and split *here* rather than anywhere else because
//! these are the least entangled three hundred lines in it: they read the
//! palette and nothing reads them.
//!
//! The arguments for each rule are in the module note above. What follows is
//! only the arithmetic.

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

/// What the panel's translucent surface actually resolves to.
///
/// Contrast is a property of what reaches the eye, and behind the panel is
/// a blur of the desktop, not a known colour. Composited over the darkest
/// and the lightest thing it could possibly sit on, so the ratios below
/// hold over a black terminal *and* a white spreadsheet rather than over a
/// convenient average.
///
/// This is not hypothetical: the first version of this palette used opaque
/// row fills, and they passed every ratio over black while the hover tint
/// came to 1.03:1 - invisible - over white.
fn grounds(theme: Theme) -> [(&'static str, Color32); 2] {
    [
        ("over black", over(theme.surface, Color32::BLACK)),
        ("over white", over(theme.surface, Color32::WHITE)),
    ]
}

fn both() -> [(&'static str, Theme); 2] {
    [("dark", Theme::dark()), ("light", Theme::light())]
}

/// Body text at AAA, which is the bar for a tool read for hours a day by
/// people of every age. This is the rule the original complaint was about.
#[test]
fn body_text_is_legible_to_the_aaa_standard() {
    for (name, theme) in both() {
        for (ground, bg) in grounds(theme) {
            for (role, fg) in [
                ("text", theme.text),
                ("strong", theme.strong),
                ("input", theme.input),
            ] {
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= 7.0,
                    "{name}/{role} {ground}: {ratio:.2}:1, want 7:1"
                );
            }
        }
    }
}

/// Secondary text - folders, the status line, hint labels - at AA. It is
/// smaller and it is supporting, but it is still text somebody reads.
#[test]
fn secondary_text_is_legible_to_the_aa_standard() {
    for (name, theme) in both() {
        for (ground, bg) in grounds(theme) {
            for (role, fg) in [
                ("dim", theme.dim),
                ("accent", theme.accent),
                ("match_run", theme.match_run),
            ] {
                let ratio = contrast(fg, bg);
                assert!(
                    ratio >= 4.5,
                    "{name}/{role} {ground}: {ratio:.2}:1, want 4.5:1"
                );
            }
        }
    }
}

/// A tinted row must not cost the text on it its legibility - and the
/// selected row is the one the user is looking hardest at.
#[test]
fn text_stays_legible_on_every_row_background() {
    for (name, theme) in both() {
        for (ground, surface) in grounds(theme) {
            for (role, fill, fg) in [
                ("selection", theme.selection, theme.strong),
                ("hover", theme.hover, theme.text),
                ("chip", theme.chip_bg, theme.chip_fg),
                ("text_selection", theme.text_selection, theme.strong),
            ] {
                let ratio = contrast(fg, over(fill, surface));
                assert!(
                    ratio >= 4.5,
                    "{name}/{role} {ground}: {ratio:.2}:1, want 4.5:1"
                );
            }
        }
    }
}

/// The code being typed is the biggest text on the panel and sits in a
/// recessed well rather than on the panel itself, so the well is a text
/// ground and is held to the AAA ratio the rest of the body text is.
#[test]
fn the_code_is_legible_in_the_well_it_is_typed_into() {
    for (name, theme) in both() {
        for (ground, surface) in grounds(theme) {
            let well = over(theme.well, surface);
            for (role, fg) in [("input", theme.input), ("dim", theme.dim)] {
                let want = if role == "input" { 7.0 } else { 4.5 };
                let ratio = contrast(fg, well);
                assert!(
                    ratio >= want,
                    "{name}/{role} {ground}: {ratio:.2}:1 in the well, want {want}:1"
                );
            }
        }
    }
}

/// The settings window is opaque, so its grounds are known colours rather
/// than a composite over an unknown desktop.
///
/// That is the whole difference between this group of tests and the ones
/// above it, and it is worth being explicit about: `grounds` exists
/// because the panel is translucent and its real background is a blur of
/// whatever was behind it. A document window has no such problem, and
/// pretending it does would hold these colours to a ratio against black
/// that nothing will ever draw them over.
fn opaque(c: Color32) -> Color32 {
    Color32::from_rgb(c.r(), c.g(), c.b())
}

/// A setting's label and its sentence of help both sit on the tile, not on
/// the window, so the tile is a text ground and answers as one.
#[test]
fn a_setting_is_legible_on_the_tile_it_is_drawn_on() {
    for (name, theme) in both() {
        let card = theme.card;
        for (role, fg, want) in [
            ("text", theme.text, 7.0),
            ("strong", theme.strong, 7.0),
            ("dim", theme.dim, 4.5),
            ("accent", theme.accent, 4.5),
            ("good", theme.good, 4.5),
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

/// And on whatever a control is filled with, which is a different colour
/// again and is where the value actually is.
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

/// A filled accent carries text of its own - a primary button's label, and
/// the knob of a switch that is on.
#[test]
fn the_filled_accent_carries_what_is_drawn_on_it() {
    for (name, theme) in both() {
        let ratio = contrast(theme.accent_fg, theme.accent_fill);
        assert!(ratio >= 7.0, "{name}: accent {ratio:.2}:1, want 7:1");
    }
}

/// A tile that is not a shade off the window is not a tile, and one that
/// is a long way off it is a second window inside the first. The same rule
/// the well answers to, in the other direction.
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

/// The tile and the trough are the two surfaces a setting is made of, and
/// a text box sits in the second one *inside* the first. If they resolve
/// to the same colour the box stops looking like a box.
#[test]
fn a_text_box_is_visibly_sunk_into_the_tile_around_it() {
    for (name, theme) in both() {
        let well = opaque(theme.well);
        let ratio = contrast(well, theme.card);
        assert!(
            ratio >= 1.15,
            "{name}: the well is {ratio:.3}:1 against the card"
        );
    }
}

/// A control's outline and the bar beside the selected page are boundaries
/// rather than text, so WCAG 1.4.11 and its 3:1 - but they are the only
/// thing saying where a control ends.
#[test]
fn a_control_has_an_edge_you_can_see() {
    for (name, theme) in both() {
        for (role, colour, ground) in [
            ("border on the card", theme.faint, theme.card),
            ("border on a control", theme.faint, theme.control),
            ("accent on the card", theme.accent_fill, theme.card),
            (
                "accent on the window",
                theme.accent_fill,
                opaque(theme.surface),
            ),
        ] {
            let ratio = contrast(colour, ground);
            assert!(ratio >= 3.0, "{name}/{role}: {ratio:.2}:1, want 3:1");
        }
    }
}

/// Both of egui's styles, because this program picks its palette from the
/// configuration and Windows picks egui's.
///
/// The two disagree the moment somebody sets `theme = "light"` on a
/// machine in dark mode, and a style written into only the active one is a
/// window that is dressed until the system theme moves. The same argument
/// [`crate::gui::fonts::sharpen`] makes, and it is here as a test because
/// `all_styles_mut` is one word away from `style_mut`.
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

/// The scroll-bar track and a text box are two different surfaces, and
/// egui will make them one if `text_edit_bg_color` is left unset.
///
/// Worth a test rather than a comment because the failure is quiet: the
/// box still works, it just stops looking like a box.
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

/// `weak_bg_fill` is a button; `bg_fill` is a scroll handle and a slider
/// rail. Setting them together is the easy mistake, and it puts a scroll
/// grip the same shade as a text box.
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

/// A widget that grows under the pointer no longer lines up with the one
/// beside it, and this window is a column of tiles that have to.
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

/// A recessed surface that is not recessed is a flat panel with extra
/// drawing in it, and one that is recessed too far is a hole.
#[test]
fn the_well_is_a_shade_off_the_panel_and_no_more() {
    for (name, theme) in both() {
        for (ground, surface) in grounds(theme) {
            let well = over(theme.well, surface);
            let ratio = contrast(well, surface);
            assert!(
                (1.02..1.35).contains(&ratio),
                "{name} {ground}: the well is {ratio:.3}:1 against the panel"
            );
        }
    }
}

/// The highlight has to survive the selection: a match drawn in amber on a
/// tinted row is exactly where a hue stops carrying and nobody notices,
/// because the unselected rows still look right.
#[test]
fn a_matched_run_is_legible_on_the_row_it_is_most_likely_to_be_on() {
    for (name, theme) in both() {
        for (ground, surface) in grounds(theme) {
            let ratio = contrast(theme.match_run, over(theme.selection, surface));
            assert!(
                ratio >= 4.5,
                "{name} {ground}: the match run is {ratio:.2}:1 on the                      selected row"
            );
        }
    }
}

/// Rules and borders are not text, so they answer to WCAG 1.4.11 rather
/// than 1.4.3 - but they must still be visible, or the panel has no edge.
#[test]
fn rules_and_borders_are_visible_without_being_text() {
    for (name, theme) in both() {
        for (ground, bg) in grounds(theme) {
            let ratio = contrast(theme.faint, bg);
            assert!(
                ratio >= 3.0,
                "{name}/faint {ground}: {ratio:.2}:1, want 3:1"
            );
            assert!(
                ratio < contrast(theme.dim, bg),
                "{name}/faint {ground} is no fainter than dim, so one of \
                 them is pointless"
            );
        }
    }
}

/// Enter opens the selection. A hover that looked like a selection would
/// get a file opened by accident - the mistake this codebase has already
/// made once, when mono mode drew them identically.
#[test]
fn the_selected_row_is_never_confusable_with_the_hovered_one() {
    for (name, theme) in both() {
        assert_ne!(
            theme.selection, theme.hover,
            "{name}: the selected and hovered rows are drawn the same"
        );
        for (ground, bg) in grounds(theme) {
            let selected = contrast(over(theme.selection, bg), bg);
            let hovered = contrast(over(theme.hover, bg), bg);
            assert!(
                selected > hovered,
                "{name} {ground}: hover ({hovered:.2}) stands out at least \
                 as much as the selection ({selected:.2})"
            );
            assert!(
                hovered > 1.05,
                "{name} {ground}: the hover tint is invisible at \
                 {hovered:.2}:1"
            );
        }
    }
}

/// Every tone must read, including the one that says a drive is
/// unreachable - which is the line that has to land.
#[test]
fn every_tone_is_legible_on_every_ground() {
    for (name, theme) in both() {
        for (ground, bg) in grounds(theme) {
            for tone in [Tone::Normal, Tone::Busy, Tone::Good, Tone::Warn, Tone::Bad] {
                let ratio = contrast(theme.tone(tone), bg);
                assert!(
                    ratio >= 4.5,
                    "{name}/{tone:?} {ground}: {ratio:.2}:1, want 4.5:1"
                );
            }
        }
    }
}

/// Roughly one man in twelve cannot tell the green from the red.
#[test]
fn no_tone_depends_on_colour_alone() {
    let mut seen: Vec<char> = TONE_GLYPH.to_vec();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), TONE_GLYPH.len(), "two tones share a glyph");
}

/// The mapping from meaning to colour is total, which is what lets
/// `src/view/` name emphasis without naming a palette.
#[test]
fn every_emphasis_has_a_distinct_appearance() {
    for (name, theme) in both() {
        // Appearance, not colour. In greyscale two emphases can sit a
        // couple of values apart and satisfy a colour-only check while
        // being the same thing to anybody not looking for the difference,
        // so the weight is half the identity.
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
        let mut colours: Vec<_> = looks.iter().map(|(c, _)| *c).collect();
        colours.sort();
        colours.dedup();
        assert_eq!(
            colours.len(),
            4,
            "{name}: two emphases are the same colour, so one says nothing"
        );

        looks.sort();
        looks.dedup();
        assert_eq!(looks.len(), 4, "{name}: two emphases are drawn identically");
        assert_eq!(
            theme.emphasis(Emphasis::Tone(Tone::Bad)),
            theme.tone(Tone::Bad)
        );
    }
}

/// A hover is a tint over a row, not a repaint of it.
#[test]
fn fading_a_colour_scales_it_rather_than_replacing_it() {
    let opaque = Color32::from_rgb(0x80, 0x40, 0x20);
    assert_eq!(faded(opaque, 1.0), opaque);
    assert_eq!(faded(opaque, 0.0), Color32::TRANSPARENT);
    assert!(faded(opaque, 0.5).a() < opaque.a());
    // Out-of-range input is clamped rather than wrapping into nonsense.
    assert_eq!(faded(opaque, 2.0), opaque);
    assert_eq!(faded(opaque, -1.0), Color32::TRANSPARENT);
}

/// The panel is a sheet over whatever is behind it, on both paths: with a
/// compositor backdrop *and* with the fill it paints itself.
#[test]
fn the_surface_is_translucent_in_both_themes() {
    for (name, theme) in both() {
        assert!(
            theme.surface.a() < 255,
            "{name}: an opaque panel reads as a dialog, not an overlay"
        );
        assert!(
            theme.surface.a() > 200,
            "{name}: too transparent to read text on"
        );
    }
}

/// The five bands have to add up to the window.
///
/// Said out loud, with the numbers in the failure message. The content band
/// is what is left over after the other four, so an arithmetic slip here is
/// a scroller that is the wrong height rather than anything that fails to
/// compile.
#[test]
fn the_bands_add_up_to_the_panel() {
    let bands = HEADER_H + DIVIDER + CONTENT_H + DIVIDER + FOOTER_H;
    assert_eq!(
        bands, PANEL_H,
        "the bands come to {bands}pt, not {PANEL_H}pt"
    );
}

/// A full list, under its heading, has to fit the content band.
///
/// The same arithmetic the `const` assertion beside `MAX_ROWS` runs. The
/// window used to be created tall enough for whatever the row count asked
/// for; the row count is now what has to fit the window, and this is the
/// direction that can be got wrong.
#[test]
fn a_full_list_fits_the_content_band() {
    let wanted = BAND_PAD * 2.0 + HEADING_H + ROW_H * MAX_ROWS as f32;
    assert!(
        wanted <= CONTENT_H,
        "{MAX_ROWS} rows want {wanted}pt of a {CONTENT_H}pt band"
    );
}

/// And they have to fit *well*. A row count that left a hundred points of
/// nothing under the list would mean `PageDown` moved less than a screen,
/// which is the one thing the number is still load-bearing for.
#[test]
fn a_full_list_very_nearly_fills_the_content_band() {
    let wanted = BAND_PAD * 2.0 + HEADING_H + ROW_H * MAX_ROWS as f32;
    assert!(
        CONTENT_H - wanted < ROW_H,
        "another row would fit: {wanted}pt of {CONTENT_H}pt used"
    );
}
