//! Icon glyphs for nav entries, buttons and result rows.
//!
//! Loaded from `C:\Windows\Fonts` (Segoe Fluent Icons on Windows 11, Segoe MDL2
//! Assets on Windows 10) rather than bundled, for licensing and size. Every
//! code point below exists in both fonts.
//!
//! Icons are never the only carrier of meaning: nav entries show a label and
//! icon buttons carry a tooltip that doubles as the accessible name. Without an
//! icon font the UI loses decoration only, and [`has_icon`] lets the gutter
//! collapse.
//!
//! This family has no [`super::FALLBACK_FILES`] tail. These are Private Use code
//! points, and a fallback font such as `seguisym` maps unrelated glyphs to some
//! of them, so a missing icon must resolve to nothing. That also keeps
//! [`has_icon`] accurate.

use eframe::egui::{FontFamily, FontId};

/// The registered family name. Must match the registration in
/// [`crate::gui::fonts`].
pub const ICON_FAMILY: &str = "segoe-icons";

/// Candidate icon fonts, tried in order; the first found is used.
///
/// Windows 11 has both; Windows 10 has only the second.
pub const ICON_FILES: [&str; 2] = [
    r"C:\Windows\Fonts\SegoeIcons.ttf",
    r"C:\Windows\Fonts\segmdl2.ttf",
];

/// An icon named by meaning; [`Icon::ch`] is the only mapping to a code point.
///
/// Keeps unreadable Private Use literals out of call sites, as
/// [`crate::view::Emphasis`] does for colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    // Nav entries, in page order.
    Gear,
    Palette,
    Drive,
    Tag,
    Search,
    Open,
    Info,
    Bug,

    // Buttons.
    Add,
    Remove,
    Copy,
    Browse,
    Reset,
    External,
    /// The panel's actions menu.
    More,

    // Result rows, chosen by file extension. Kept to six broad categories. See
    // [`crate::gui::panel::icons`].
    FilePdf,
    FileDrawing,
    FileImage,
    FileDocument,
    FileArchive,
    File,
}

impl Icon {
    /// Every variant, for tests.
    pub const ALL: [Self; 21] = [
        Self::Gear,
        Self::Palette,
        Self::Drive,
        Self::Tag,
        Self::Search,
        Self::Open,
        Self::Info,
        Self::Bug,
        Self::Add,
        Self::Remove,
        Self::Copy,
        Self::Browse,
        Self::Reset,
        Self::External,
        Self::More,
        Self::FilePdf,
        Self::FileDrawing,
        Self::FileImage,
        Self::FileDocument,
        Self::FileArchive,
        Self::File,
    ];

    /// Returns the code point; the trailing comment is Microsoft's glyph name.
    ///
    /// Written as escapes because Private Use characters render as boxes in
    /// editors. The test below verifies each one exists in the installed font.
    pub const fn ch(self) -> char {
        match self {
            Self::Gear => '\u{E713}',     // Setting
            Self::Palette => '\u{E790}',  // Color
            Self::Drive => '\u{E8B7}',    // Folder
            Self::Tag => '\u{E8EC}',      // Tag
            Self::Search => '\u{E721}',   // Search
            Self::Open => '\u{E8E5}',     // OpenFile
            Self::Info => '\u{E946}',     // Info
            Self::Bug => '\u{EBE8}',      // Bug
            Self::Add => '\u{E710}',      // Add
            Self::Remove => '\u{E711}',   // Cancel
            Self::Copy => '\u{E8C8}',     // Copy
            Self::Browse => '\u{E838}',   // FolderOpen
            Self::Reset => '\u{E7A7}',    // Undo
            Self::External => '\u{E8A7}', // OpenInNewWindow
            Self::More => '\u{E712}',     // More

            Self::FilePdf => '\u{EA90}',      // PDF
            Self::FileDrawing => '\u{EB3C}',  // Design
            Self::FileImage => '\u{E91B}',    // Photo
            Self::FileDocument => '\u{E8A5}', // Document
            Self::FileArchive => '\u{E7B8}',  // Package
            Self::File => '\u{E7C3}',         // Page
        }
    }

    /// Returns the glyph as a one-character string.
    pub fn text(self) -> String {
        self.ch().to_string()
    }
}

/// Icon size beside a nav entry.
pub const ICON_NAV: f32 = 16.0;
/// Icon size inside a small row button.
pub const ICON_INLINE: f32 = 14.0;

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICON_FAMILY.into()))
}

/// Returns whether the loaded icon font contains this glyph.
///
/// Checked per glyph because Segoe MDL2 Assets is the smaller set. A family
/// with no font data lays text out zero-wide rather than panicking (see
/// [`crate::gui::fonts`]), so this prevents reserving space for a blank icon.
pub fn has_icon(ctx: &eframe::egui::Context, icon: Icon) -> bool {
    ctx.fonts_mut(|fonts| fonts.has_glyph(&icon_font(ICON_NAV), icon.ch()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::fonts;

    #[test]
    fn every_icon_is_a_different_mark() {
        let mut seen: Vec<char> = Icon::ALL.iter().map(|i| i.ch()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), Icon::ALL.len(), "two icons share a code point");
    }

    /// Verifies every code point exists in the installed icon font.
    ///
    /// Skipped when no icon font is installed (for example on a build agent).
    /// It cannot verify that a glyph looks right; the `ui-snapshots` images
    /// cover that.
    #[test]
    fn every_icon_this_program_asks_for_is_one_the_machine_can_draw() {
        let ctx = eframe::egui::Context::default();
        let found = fonts::install(&ctx);
        if !found.icons {
            return;
        }

        // egui builds its font atlas during the first pass.
        let mut output = ctx.run_ui(Default::default(), |ui| {
            for icon in Icon::ALL {
                assert!(
                    has_icon(ui.ctx(), icon),
                    "{icon:?} is U+{:04X}, which the icon font has not got",
                    icon.ch() as u32
                );
            }
        });

        // epaint asserts if unapplied texture uploads are dropped.
        output.textures_delta.clear();
    }

    /// The family must be registered even without an icon font, or the first
    /// nav entry drawn panics.
    #[test]
    fn asking_about_an_icon_is_safe_on_a_machine_that_has_none() {
        let ctx = eframe::egui::Context::default();
        fonts::install_bundled(&ctx);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            // Passes if this returns: egui panics on an unregistered family.
            let _ = has_icon(ui.ctx(), Icon::Gear);
        });
        output.textures_delta.clear();
    }
}
