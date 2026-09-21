//! Every ratio the palette claims, asserted.
//!
//! Split out of [`super`] because the file outgrew the size this repository
//! holds a module to, and split *here* rather than anywhere else because
//! these are the least entangled three hundred lines in it: they read the
//! palette and nothing reads them.
//!
//! The arguments for each rule are in the module note above. What follows is
//! mostly the arithmetic. The one thing here that is not about colour is at
//! the foot of the file: the corner ramp, which lives with these because it
//! is the same kind of claim - a number this module publishes that something
//! outside it has to be prevented from getting wrong.

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

/// How much of the desktop can still reach the eye through the panel.
///
/// The compositor path is acrylic - a heavy blur, tinted to roughly 80%
/// toward the system theme's own colour, with a luminosity blend that pulls
/// the result further toward that tint - and then `paint_surface` puts
/// `Theme::wash` over the top at 60%. So at most `0.4 * 0.2` of whatever was
/// behind the window survives to the surface. Eight percent, and that is an
/// upper bound on a bound.
const LEAK: f32 = 0.08;

/// The ground the panel paints, which every ratio below is against.
///
/// This used to be a pair of grounds rather than one, and the change is the
/// whole reason the numbers in this file moved. The surface was translucent
/// at alpha `0xF0`, so the real background was a blur of the desktop, and the
/// honest model was to composite it over pure black *and* pure white and hold
/// every ratio against both ends. That was not paranoia - the first version
/// of this palette passed every ratio over black while the hover tint came to
/// 1.03:1 over white.
///
/// It is also a bound so wide that Fluent's own published values cannot meet
/// it, and that is not a sign the values are wrong. A subtle divider is 1.3:1
/// against its own background in the theme Microsoft ships. No threshold
/// turns 1.3 into 3.
///
/// So the arrangement changed rather than the model getting looser. The panel
/// paints an opaque `NeutralBackground1` where it paints its own ground, and
/// `Theme::wash` over the compositor's acrylic where it does not - which is
/// what Ueli does, and what makes this function able to return one colour.
///
/// What the acrylic path still costs is not swept away with it. It is in
/// [`what_the_acrylic_path_costs`], as a number.
fn base(theme: Theme) -> Color32 {
    opaque(theme.surface)
}

/// The two extremes the acrylic path can reach, on top of everything else.
fn through_acrylic(theme: Theme) -> [(&'static str, Color32); 2] {
    let base = base(theme);
    [
        ("over black", over(faded(Color32::BLACK, LEAK), base)),
        ("over white", over(faded(Color32::WHITE, LEAK), base)),
    ]
}

/// A colour with whatever transparency it has removed.
fn opaque(c: Color32) -> Color32 {
    Color32::from_rgb(c.r(), c.g(), c.b())
}

fn both() -> [(&'static str, Theme); 2] {
    [("dark", Theme::dark()), ("light", Theme::light())]
}

/// Body text at AAA, which is the bar for a tool read for hours a day by
/// people of every age. This is the rule the original complaint was about.
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

/// Secondary text - folders, the status line, hint labels - at AA. It is
/// smaller and it is supporting, but it is still text somebody reads.
#[test]
fn secondary_text_is_legible_to_the_aa_standard() {
    for (name, theme) in both() {
        let bg = base(theme);
        // `accent` is not here, and used to be. It is brand blue, it is no
        // longer drawn as small text on the panel at all - the two runs that
        // used to be accented are full-strength and Semibold now - and it
        // answers for itself as a *bar* in `a_boundary_is_as_loud_as_its_job`
        // and as text on the settings window's tile.
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

/// A tinted row must not cost the text on it its legibility - and the
/// selected row is the one the user is looking hardest at.
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

/// The code being typed is the biggest text on the panel and sits in a
/// recessed well rather than on the panel itself, so the well is a text
/// ground and is held to the AAA ratio the rest of the body text is.
#[test]
fn the_code_is_legible_in_the_well_it_is_typed_into() {
    for (name, theme) in both() {
        // One ground, not three: the well is opaque, so nothing behind the
        // panel reaches the text typed into it.
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
///
/// AA, not AAA. This asked 7:1 when the accent was a near-white fill with
/// near-black on it, which is a pair that can reach it; brand blue is not,
/// and no brand blue is. White on `#0f6cbd` is 5.4:1 and white on `#115ea3`
/// is 6.7:1, and Microsoft ships both of those as the text-on-accent pairing
/// for a primary button. 7:1 was a bar the old palette happened to clear
/// rather than one this pair was ever going to.
#[test]
fn the_filled_accent_carries_what_is_drawn_on_it() {
    for (name, theme) in both() {
        let ratio = contrast(theme.accent_fg, theme.accent_fill);
        assert!(ratio >= 4.5, "{name}: accent {ratio:.2}:1, want 4.5:1");
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

/// A text box on a tile has to be findable, by one means or another.
///
/// This used to assert the only means there was: the trough is a step darker
/// than the tile, 1.15:1 or better. Fluent does not do that. A filled input
/// and the card around it are both `NeutralBackground3` - the *same colour*,
/// 1.000:1 - and what makes it a box is a one-pixel rule along the bottom at
/// `NeutralStrokeAccessible`, which is why that token exists and why it is in
/// the palette.
///
/// So the assertion is the disjunction rather than the old half of it: either
/// the fill differs enough to see, or the rule does. Written as an `or` on
/// purpose - a later change that puts a step back into the fill should not
/// have to come back here and re-argue the point.
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

/// Every boundary is as loud as its job, and no louder.
///
/// This replaces a test that held all three of them to WCAG 1.4.11's 3:1.
/// That is the right bar for a boundary you have to be able to *find* - the
/// outline that says where a control ends - and the wrong bar for a rule
/// between two bands, which is a hint about structure. Fluent agrees: it
/// publishes three stroke tokens precisely so that the three jobs are three
/// values, and asserting 3:1 of `NeutralStroke2` is asserting that Microsoft
/// got their own divider wrong.
///
/// So what is asserted is the *ordering*, which is the real claim - a
/// divider quieter than a control outline quieter than the rule that makes a
/// box a box - plus 3:1 on the one that has to carry it.
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
        // A rule nobody can see at all is not a quiet rule, it is a missing
        // one.
        assert!(
            edge > 1.05,
            "{name}: the band rule is invisible at {edge:.3}:1"
        );
    }
    // And the mark that says which page, on every ground it is drawn over.
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
        let ratio = contrast(opaque(theme.well), base(theme));
        assert!(
            (1.02..1.35).contains(&ratio),
            "{name}: the well is {ratio:.3}:1 against the panel"
        );
    }
}

/// The highlight has to survive the selection: a match drawn in amber on a
/// tinted row is exactly where a hue stops carrying and nobody notices,
/// because the unselected rows still look right.
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

/// Enter opens the selection. A hover that looked like a selection would get
/// a file opened by accident - the mistake this codebase has already made
/// once, when mono mode drew the two identically.
///
/// # Why this no longer asks the selection to be louder
///
/// It used to, and it was the right rule for a palette we were picking. In
/// stock Fluent dark the hovered row is `#3d3d3d` and the selected row is
/// `#383838`: the hover is a shade *louder*, 1.34:1 against the ground
/// against the selection's 1.24:1. That is not a mistake to be corrected on
/// the way past - it is what `NeutralBackground1Hover` and
/// `NeutralBackground1Selected` are, and every Fluent list in Windows behaves
/// this way.
///
/// What actually distinguishes them, there and here, is the three-point brand
/// bar down the left edge, which only the selected row has. So the assertion
/// moves onto the bar: the two fills differ, both are visible, and the bar is
/// legible on the selected one. That is a weaker guarantee than "brighter
/// means selected" and it is worth saying so plainly - a user who reads
/// brightness rather than the bar now has it backwards in the dark theme.
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
        // The one signal only the selected row carries.
        let bar = contrast(theme.accent, theme.selection);
        assert!(
            bar >= 3.0,
            "{name}: the accent bar is {bar:.2}:1 on the selected row, and it \
             is the only thing telling it from a hovered one"
        );
    }
}

/// Every tone must read, including the one that says a drive is
/// unreachable - which is the line that has to land.
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

/// What the acrylic path costs, as a number rather than as a shrug.
///
/// Every other ratio in this file is against the ground the panel paints,
/// because that is a colour the palette owns. On the compositor path it does
/// not own it: `Theme::wash` goes over DWM's acrylic, and up to [`LEAK`] of
/// whatever was behind the window survives to the surface. So the grounds
/// drift, by a bounded amount, in a direction nobody controls.
///
/// This is the test that says how far. It is deliberately not a pass/fail
/// restatement of the tests above at a lower bar - those bars mean something
/// and moving them would make them mean less. It is a floor, with the real
/// numbers in the failure message, so that a future change to the palette or
/// to the wash cannot quietly make the translucent path worse than it is
/// today.
///
/// What it records, and this is the honest summary: body text stays AAA
/// everywhere. Secondary text - a folder, the status line, a group caption -
/// drops from about 5:1 to about 4:1 at the extreme, which is under AA. That
/// is a real cost of copying Fluent's neutrals exactly onto a translucent
/// window, it is a cost Ueli pays too, and the lever if it ever needs
/// paying down is the wash, not the type.
///
/// Row fills are not asserted here at all, and the reason is worth writing
/// down because it looks like an omission. A row's fill is an opaque
/// neutral and the ground under it drifts, so at one particular desktop
/// brightness the two cross and the fill says nothing. In the light theme
/// that point is exact: a selected row is `#ebebeb` and the ground reaches
/// `#ebebeb`, 1.000:1. Ueli has the identical property for the identical
/// reason. What marks the row there and here is the accent bar, which is
/// asserted where it belongs, in
/// `the_selected_row_is_never_confusable_with_the_hovered_one`.
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
        // Colour alone is deliberately *not* asserted, and used to be. Body
        // and Strong are both `NeutralForeground1` in Fluent - the difference
        // between `body1` and `body1Strong` is 400 against 600 and nothing
        // else - so a colour-only check would now fail against a theme that
        // is behaving exactly as published.
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

/// The panel is a sheet where the compositor can make it one, and a solid
/// window where it cannot.
///
/// The inverse of what this used to assert. The surface was translucent on
/// both paths so that the painted fallback still read as an overlay; it is
/// now opaque, and the sheet is `wash` over the compositor's acrylic - which
/// is Ueli's arrangement and is what lets every ratio above be arithmetic
/// against a known ground. The trade is in `paint_surface`.
#[test]
fn the_panel_is_a_wash_over_acrylic_and_a_solid_window_without_it() {
    for (name, theme) in both() {
        assert_eq!(
            theme.surface.a(),
            255,
            "{name}: the painted ground is the one colour the palette is \
             allowed to be certain of"
        );
        // Sixty percent, which is Ueli's number. Enough that the desktop is
        // still there and little enough that `LEAK` holds.
        assert_eq!(theme.wash.a(), 0x99, "{name}: the wash is not 60%");
        // And `LEAK` is derived from it rather than guessed: what gets past
        // the wash, times what gets past acrylic's 80% tint.
        let past = (1.0 - theme.wash.a() as f32 / 255.0) * 0.2;
        assert!(
            past <= LEAK,
            "{name}: {past:.3} of the desktop gets through, and `LEAK`              claims {LEAK}"
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

/// A full page of rows, under its caption, has to fit the content band.
///
/// The same arithmetic the `const` assertions beside the row heights run,
/// said out loud with the numbers in the failure message, and for both
/// layouts - because each has a page size of its own and the taller rows are
/// the ones that would overrun.
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

/// No corner this program paints is a window corner.
///
/// Three things used to draw the panel's edge and none of them agreed with
/// the others about where it was. DWM clips the window at `DWMWCP_ROUND`, in
/// device pixels, at whatever radius the running build of Windows uses. This
/// module published eight, and `paint_surface` filled and stroked at eight
/// *egui points*. A point is a device pixel only while `pixels_per_point` is
/// exactly the monitor's scale - which it is not under a zoom factor, not in
/// the frames either side of a `WM_DPICHANGED`, and not while the window
/// straddles two monitors scaled differently. Wherever those two curves
/// parted company the difference showed as a bad edge.
///
/// The panel no longer paints a corner at all, and the ramp is now cut short
/// of the number that would let anything paint one by accident. Eight is
/// Windows', DWM is the only thing that has it, and this is the test that
/// keeps it that way.
///
/// `RADIUS_CIRCULAR` is exempt and is checked separately. A window corner is
/// a specific radius on a large rectangle; a circular badge is a sixteen-point
/// pill with its ends rounded off. Nobody has ever mistaken one for the other,
/// and a test that could not tell them apart would be a test that banned
/// pills.
#[test]
fn nothing_this_program_paints_is_rounded_like_a_window() {
    /// What Windows 11 gives an ordinary window, and Fluent calls
    /// `borderRadiusXLarge`.
    const WINDOW_CORNER: u8 = 8;
    for r in [RADIUS_SMALL, RADIUS_MEDIUM, RADIUS_LARGE] {
        assert!(
            r < WINDOW_CORNER,
            "{r} is a window corner, and this program does not draw those"
        );
    }
}

/// The ramp is Fluent's, and it is a ramp: three rungs, all different, in
/// order. A duplicate would be the state this replaced, where four constants
/// with four names all held 4.
#[test]
fn the_three_rungs_are_three_different_numbers() {
    let ramp = [RADIUS_SMALL, RADIUS_MEDIUM, RADIUS_LARGE];
    assert_eq!(ramp, [2, 4, 6], "these are Fluent's published radii");
    assert!(ramp.windows(2).all(|w| w[0] < w[1]), "out of order");
}
