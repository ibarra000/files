//! Loads the system Segoe UI and icon fonts into egui.
//!
//! Fonts are read from `C:\Windows\Fonts` rather than bundled, because Segoe UI
//! may not be redistributed.
//!
//! # Families are always registered
//!
//! [`crate::gui::theme::font`] requests `FontFamily::Name("segoe")` and egui
//! panics on an unregistered family, so every family is registered even when
//! its file is missing; it then falls back to whatever else is available.

use std::sync::Arc;

use eframe::egui::{FontData, FontDefinitions, FontFamily};

use crate::gui::theme::icons::{ICON_FAMILY, ICON_FILES};
use crate::gui::theme::{FALLBACK_FILES, FONT_FILES, Weight};

/// Which of the requested fonts were found, reported in diagnostics.
///
/// Text and icons are tracked separately because they fail independently, and
/// a missing icon font is only cosmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub text: bool,
    pub icons: bool,
}

/// Registers every family from the system fonts and reports which were found.
pub fn install(ctx: &eframe::egui::Context) -> Found {
    install_with(ctx, true)
}

/// Registers the same families bound to fonts that are identical on every
/// machine, for tests and snapshots.
///
/// Requires the `test-fonts` feature, which enables `eframe/default_fonts`
/// (1.41 MB, not in the shipping build). Without it there is nothing bundled,
/// and a family bound to nothing lays out at zero width, so this falls back to
/// the system fonts. `ui-snapshots` implies `test-fonts`, so pixel comparisons
/// never take that fallback.
pub fn install_bundled(ctx: &eframe::egui::Context) {
    install_with(ctx, !cfg!(feature = "test-fonts"));
}

fn install_with(ctx: &eframe::egui::Context, use_system: bool) -> Found {
    let mut defs = FontDefinitions::default();

    // egui's bundled fonts: empty in the shipping build (`default_fonts` off),
    // present under `test-fonts`.
    let bundled: Vec<String> = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    // System fallback fonts for glyphs Segoe UI lacks, loaded once and shared
    // by both weights.
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
    for (name, path) in FONT_FILES.iter() {
        let mut stack: Vec<String> = Vec::new();
        // One file per family; see `FONT_FILES`.
        let read = |p: &str| use_system.then(|| std::fs::read(p).ok()).flatten();
        match read(path) {
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

    // The first icon font present. No fallback tail: icons are Private Use
    // code points, and a fallback would draw an unrelated glyph from
    // `seguisym` instead of nothing (see `gui::theme::icons`).
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
    // Registered even if empty; see the module docs.
    defs.families
        .insert(FontFamily::Name(ICON_FAMILY.into()), icons);

    // egui's own families, used by tooltips, debug overlays and
    // `Style::text_styles`. Without `default_fonts` they exist but are empty,
    // which lays text out invisibly at zero width, so bind them to the regular
    // weight.
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

/// Disables `TextOptions::subpixel_binning`, which epaint documents as making
/// text blurrier.
///
/// Hinting stays on (the default), since it snaps stems to the pixel grid.
fn sharpen(ctx: &eframe::egui::Context) {
    // Both light and dark styles, since the theme can change at runtime.
    ctx.all_styles_mut(|style| {
        style.visuals.text_options.subpixel_binning = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_weight_names_a_distinct_family() {
        let mut names: Vec<_> = Weight::ALL.map(|w| w.family()).to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Weight::ALL.len());
    }

    /// `theme::font` requests these by name, and egui panics on an unregistered
    /// family.
    #[test]
    fn every_family_the_panel_asks_for_is_one_that_gets_registered() {
        for weight in Weight::ALL {
            assert!(
                FONT_FILES.iter().any(|(name, _)| *name == weight.family()),
                "{:?} asks for a family nothing registers",
                weight
            );
        }
    }

    /// Every family must be registered even without Segoe UI. The width check
    /// only applies where a font was found, which is never the case off
    /// Windows.
    #[test]
    fn the_families_are_registered_even_when_the_files_are_missing() {
        let ctx = eframe::egui::Context::default();
        let found = install(&ctx);

        // Inside a pass, because egui has no font atlas until the first one.
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let ctx = ui.ctx();
            for (name, _) in FONT_FILES {
                let id = eframe::egui::FontId::new(14.0, FontFamily::Name(name.into()));
                // An unregistered family panics here.
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

            // The icon family. Laid out rather than queried with `has_glyph`,
            // which tests coverage, not registration.
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

            // egui's built-in families, laid out because present-but-empty
            // would otherwise read as present.
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let id = eframe::egui::FontId::new(14.0, family);
                let _ = ctx.fonts_mut(|fonts| {
                    fonts.layout_no_wrap("x".to_owned(), id, eframe::egui::Color32::WHITE)
                });
            }
        });

        // epaint asserts if unapplied texture uploads are dropped.
        output.textures_delta.clear();
    }
}
