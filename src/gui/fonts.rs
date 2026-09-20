//! The system's own typeface.
//!
//! egui ships with a font, and a panel drawn in it looks like a program that
//! brought its own furniture. This one draws in Segoe UI, which is what every
//! other window on this machine is drawn in - loaded from `C:\Windows\Fonts`
//! rather than bundled, because shipping Segoe UI is a licence violation and a
//! Windows without it is a Windows that cannot start.
//!
//! # The families exist whether or not the files do
//!
//! [`crate::gui::theme::font`] asks for `FontFamily::Name("segoe")`, and egui
//! panics on a family it has never been given. So both names are always
//! registered; on a machine with no Segoe UI they simply resolve to egui's own
//! font, and the panel is plain rather than absent.

use std::sync::Arc;

use eframe::egui::{FontData, FontDefinitions, FontFamily};

use crate::gui::theme::icons::{ICON_FAMILY, ICON_FILES};
use crate::gui::theme::{FALLBACK_FILES, FONT_FILES, VARIABLE_FONT_FILES, Weight};

/// Which of the fonts this program would like were actually there.
///
/// Two answers rather than one, because they fail separately and for
/// different reasons: a machine can have Segoe UI and not have Segoe MDL2
/// Assets, and the second is a cosmetic loss where the first is not.
///
/// Both are kept for the diagnostics report. "The text looks wrong" and "the
/// icons are missing" are two support calls, and these are the two answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub text: bool,
    pub icons: bool,
}

/// Installs every family, and says which of them found a file.
pub fn install(ctx: &eframe::egui::Context) -> Found {
    install_with(ctx, true)
}

/// Registers the same two families, bound to a font that is the same
/// everywhere.
///
/// For tests, and for the pictures they compare. [`install`] reads whatever
/// Segoe UI the machine it is running on happens to have - which is the right
/// thing for a panel and the wrong thing for a snapshot, because a picture
/// taken on one machine and compared on another would be comparing two
/// typefaces and reporting it as a layout change.
///
/// "The same everywhere" is `eframe/default_fonts`, which the shipping build
/// does not have: it is 1.41 MB of TTFs for a fallback the panel now reads off
/// the system instead. Without `test-fonts` there is nothing bundled to bind
/// to, and a family bound to nothing lays every string out *zero wide* rather
/// than failing - so this falls through to the system fonts, which are at
/// least fonts. Everything that compares pixels is behind `ui-snapshots`,
/// which implies `test-fonts`, so nothing that measures takes that branch.
pub fn install_bundled(ctx: &eframe::egui::Context) {
    install_with(ctx, !cfg!(feature = "test-fonts"));
}

fn install_with(ctx: &eframe::egui::Context, use_system: bool) -> Found {
    let mut defs = FontDefinitions::default();

    // Whatever egui was going to use, which in the shipping build is nothing at
    // all: with `default_fonts` off, `FontDefinitions::default()` is
    // `::empty()`. Present under `test-fonts`, so a snapshot still has a
    // typeface that is the same on every machine.
    let bundled: Vec<String> = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    // The system's own coverage, loaded once and shared by both weights - two
    // families pointing at the same two entries, not four copies of the
    // files. This is the tail that makes a character Segoe UI has no glyph for
    // come out as a character rather than as a box.
    let mut tail = Vec::new();
    if use_system {
        for (name, path) in FALLBACK_FILES {
            if let Ok(bytes) = std::fs::read(path) {
                defs.font_data
                    .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
                tail.push(name.to_owned());
            }
        }
    }
    tail.extend(bundled.iter().cloned());

    let mut found_all = true;
    for (i, (name, path)) in FONT_FILES.iter().enumerate() {
        let mut stack: Vec<String> = Vec::new();
        // Segoe UI Variable first where the machine has it - it is the same
        // typeface cut to stay crisp at a given size, which is the whole
        // complaint - and the original where it does not. Both are read the
        // same way and registered under the same name, so nothing downstream
        // knows which it got.
        let read = |p: &str| use_system.then(|| std::fs::read(p).ok()).flatten();
        match read(VARIABLE_FONT_FILES[i].1).or_else(|| read(path)) {
            Some(bytes) => {
                defs.font_data
                    .insert((*name).to_owned(), Arc::new(FontData::from_owned(bytes)));
                stack.push((*name).to_owned());
            }
            None => found_all = false,
        }
        stack.extend(tail.iter().cloned());
        defs.families
            .insert(FontFamily::Name((*name).into()), stack);
    }

    // The marks, from whichever of the two icon fonts this Windows has.
    //
    // Registered with no fallback tail, unlike the two families above, and
    // `gui::theme::icons` gives the reason: these are Private Use code points,
    // and a tail would resolve a mark the icon font lacks to an unrelated
    // glyph from `seguisym` rather than to nothing.
    let mut icons: Vec<String> = Vec::new();
    if use_system {
        for path in ICON_FILES {
            if let Ok(bytes) = std::fs::read(path) {
                defs.font_data.insert(
                    ICON_FAMILY.to_owned(),
                    Arc::new(FontData::from_owned(bytes)),
                );
                icons.push(ICON_FAMILY.to_owned());
                break;
            }
        }
    }
    let found_icons = !icons.is_empty();
    // Inserted whether or not a file was read, for the reason the module note
    // gives: egui panics on a family it has never been given, and a family
    // bound to nothing merely lays out to nothing.
    defs.families
        .insert(FontFamily::Name(ICON_FAMILY.into()), icons);

    // egui's own two families, which this panel never asks for and egui itself
    // does - the debug-on-hover overlay, a tooltip, anything reaching
    // `Style::text_styles`. With `default_fonts` they arrived populated; with it
    // gone `FontDefinitions::empty` leaves them *present and empty*, and epaint
    // only panics on a family it has never been given. An empty one lays out
    // silently to zero width, so the failure would have been invisible text and
    // a support call rather than a stack trace. Bound to the regular weight, so
    // whatever reaches them is drawn in the same typeface as everything else.
    let regular = defs
        .families
        .get(&FontFamily::Name(Weight::Regular.family().into()))
        .cloned()
        .unwrap_or_default();
    defs.families
        .insert(FontFamily::Proportional, regular.clone());
    defs.families.insert(FontFamily::Monospace, regular);

    ctx.set_fonts(defs);
    sharpen(ctx);
    Found {
        text: found_all,
        icons: found_icons,
    }
}

/// Turns off the one default that epaint itself warns makes text blurry.
///
/// `TextOptions::subpixel_binning` renders each glyph at up to four fractional
/// horizontal offsets for more even spacing, and its own documentation adds:
/// *"It also lead to text looking more blurry."* On a panel whose whole content
/// is short strings of digits and dashes read at a glance, even spacing is
/// worth nothing and sharpness is worth everything.
///
/// Hinting is left on, which is the default: it is what snaps a stem to the
/// pixel grid, and turning it off would undo this.
fn sharpen(ctx: &eframe::egui::Context) {
    // Both themes: egui keeps a `Style` per light/dark and this panel switches
    // between them at runtime, so setting only the active one would sharpen the
    // text until somebody changed theme.
    ctx.all_styles_mut(|style| {
        style.visuals.text_options.subpixel_binning = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two weights the panel draws with, and no more. Two weights that
    /// are drawn the same are one weight and a wasted load.
    #[test]
    fn every_weight_names_a_distinct_family() {
        let mut names: Vec<_> = [Weight::Regular, Weight::Bold].map(|w| w.family()).to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 2);
    }

    /// `theme::font` asks for these by name, and egui panics on a family it was
    /// never given. The two lists have to agree.
    #[test]
    fn every_family_the_panel_asks_for_is_one_that_gets_registered() {
        for weight in [Weight::Regular, Weight::Bold] {
            assert!(
                FONT_FILES.iter().any(|(name, _)| *name == weight.family()),
                "{:?} asks for a family nothing registers",
                weight
            );
        }
    }

    /// A machine with no Segoe UI must still get both families, or the
    /// first label drawn takes the process down.
    ///
    /// Two different failures, and this covers both. `epaint` panics on a
    /// family it was never given, and lays out a family bound to *nothing*
    /// silently at zero width - so the width assertion below is made only where
    /// there was something to measure with. Off Windows there never is, and
    /// that is correct; a machine that found Segoe UI and still measures
    /// nothing has a family pointing at font data nobody inserted.
    #[test]
    fn the_families_are_registered_even_when_the_files_are_missing() {
        let ctx = eframe::egui::Context::default();
        let found = install(&ctx);

        // Inside a pass, because egui has no font atlas at all until the first
        // one - which is also why `install` is called from the constructor
        // rather than lazily at the first label.
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            for (name, _) in FONT_FILES {
                let id = eframe::egui::FontId::new(14.0, FontFamily::Name(name.into()));
                // Laying anything out is the assertion: a family egui was never
                // given panics inside egui rather than returning an error.
                let width = ctx
                    .fonts_mut(|fonts| {
                        fonts.layout_no_wrap(
                            "11-D-0704".to_owned(),
                            id,
                            eframe::egui::Color32::WHITE,
                        )
                    })
                    .rect
                    .width();
                if found.text {
                    assert!(width > 0.0, "{name} laid out to nothing");
                }
            }

            // And the marks, which are a third family asked for by name and
            // are the newest way for this to go wrong. Laid out rather than
            // asked about, because `has_glyph` answers a question about
            // coverage and this one is about whether the family exists at all.
            let mark = ctx
                .fonts_mut(|fonts| {
                    fonts.layout_no_wrap(
                        crate::gui::theme::Icon::Gear.text(),
                        crate::gui::theme::icon_font(16.0),
                        eframe::egui::Color32::WHITE,
                    )
                })
                .rect
                .width();
            if found.icons {
                assert!(mark > 0.0, "{ICON_FAMILY} laid out to nothing");
            }

            // And egui's own two, which this panel never asks for and egui
            // does. Laid out rather than read back, because present-and-empty
            // is the failure being checked for and it reads as present.
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let id = eframe::egui::FontId::new(14.0, family);
                let _ = ctx.fonts_mut(|fonts| {
                    fonts.layout_no_wrap("x".to_owned(), id, eframe::egui::Color32::WHITE)
                });
            }
        });

        // The atlas the pass built is a texture upload nobody here is going to
        // perform, and epaint asserts on dropping unapplied ones rather than
        // letting a real renderer lose a glyph.
        output.textures_delta.clear();
    }
}
