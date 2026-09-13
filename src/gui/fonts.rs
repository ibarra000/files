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
//! panics on a family it has never been given. So all three names are always
//! registered; on a machine with no Segoe UI they simply resolve to egui's own
//! font, and the panel is plain rather than absent.

use std::sync::Arc;

use eframe::egui::{FontData, FontDefinitions, FontFamily};

use crate::gui::theme::FONT_FILES;

/// Installs the three weights, and says whether the real ones were found.
///
/// The answer is kept for the diagnostics panel: "the text looks wrong" is a
/// support call, and "Segoe UI was not found" is the answer to it.
pub fn install(ctx: &eframe::egui::Context) -> bool {
    install_with(ctx, true)
}

/// Registers the same three families, bound to egui's own font only.
///
/// For tests, and for the pictures they compare. [`install`] reads whatever
/// Segoe UI the machine it is running on happens to have - which is the right
/// thing for a panel and the wrong thing for a snapshot, because a picture
/// taken on one machine and compared on another would be comparing two
/// typefaces and reporting it as a layout change.
pub fn install_bundled(ctx: &eframe::egui::Context) {
    install_with(ctx, false);
}

fn install_with(ctx: &eframe::egui::Context, use_system: bool) -> bool {
    let mut defs = FontDefinitions::default();

    // Whatever egui was going to use. Kept as the tail of every family so a
    // character Segoe UI has no glyph for - an emoji in a filename, a script
    // from another alphabet - still comes out as a character rather than as a
    // box.
    let fallback = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut found_all = true;
    for (name, path) in FONT_FILES {
        let mut stack = Vec::new();
        // `None` in bundled mode without looking, which is the same branch a
        // machine with no Segoe UI takes.
        match use_system.then(|| std::fs::read(path).ok()).flatten() {
            Some(bytes) => {
                defs.font_data
                    .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
                stack.push(name.to_owned());
            }
            None => found_all = false,
        }
        stack.extend(fallback.iter().cloned());
        defs.families.insert(FontFamily::Name(name.into()), stack);
    }

    ctx.set_fonts(defs);
    found_all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::theme::Weight;

    /// The three weights the panel draws with, and no more. Two weights that
    /// are drawn the same are one weight and a wasted load.
    #[test]
    fn every_weight_names_a_distinct_family() {
        let mut names: Vec<_> = [Weight::Light, Weight::Regular, Weight::Bold]
            .map(|w| w.family())
            .to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 3);
    }

    /// `theme::font` asks for these by name, and egui panics on a family it was
    /// never given. The two lists have to agree.
    #[test]
    fn every_family_the_panel_asks_for_is_one_that_gets_registered() {
        for weight in [Weight::Light, Weight::Regular, Weight::Bold] {
            assert!(
                FONT_FILES.iter().any(|(name, _)| *name == weight.family()),
                "{:?} asks for a family nothing registers",
                weight
            );
        }
    }

    /// A machine with no Segoe UI must still get all three families, or the
    /// first label drawn takes the process down.
    #[test]
    fn the_families_are_registered_even_when_the_files_are_missing() {
        let ctx = eframe::egui::Context::default();
        install(&ctx);

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
                assert!(width > 0.0, "{name} laid out to nothing");
            }
        });

        // The atlas the pass built is a texture upload nobody here is going to
        // perform, and epaint asserts on dropping unapplied ones rather than
        // letting a real renderer lose a glyph.
        output.textures_delta.clear();
    }
}
