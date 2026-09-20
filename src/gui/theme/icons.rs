//! The marks beside a nav entry and inside a small button.
//!
//! Read out of `C:\Windows\Fonts` for exactly the reason the typeface next
//! door is: these are fonts the machine already has, and shipping a copy would
//! be both a licence violation and half a megabyte. Segoe Fluent Icons is
//! Windows 11's set and Segoe MDL2 Assets is Windows 10's; every code point
//! below is one the two share, so one file is read and which one it was is
//! nobody's business afterwards.
//!
//! # An icon is never the only carrier
//!
//! The same rule the palette keeps, applied to marks rather than to colour.
//! Every nav entry has its name beside its icon and every icon button has its
//! name in a tooltip - which is also its accessible name, because a button
//! with no text has nothing else to offer a screen reader. So a machine with
//! no icon font loses decoration and loses nothing else, and [`has_icon`] is
//! what lets the gutter close up rather than standing empty.
//!
//! # Why this family has no fallback tail
//!
//! [`super::FALLBACK_FILES`] is appended to both text families so that a
//! character Segoe UI has not got comes out as a character rather than as a
//! box. Doing the same here would be actively worse than nothing: these are
//! Private Use code points, `seguisym` has its own unrelated glyphs at some of
//! them, and a mark the icon font lacks would resolve to an arbitrary picture
//! instead of to nothing. With no tail, [`has_icon`] is a true answer.

use eframe::egui::{FontFamily, FontId};

/// What the family is registered as. Asked for by [`icon_font`], registered by
/// [`crate::gui::fonts`], and the two have to agree.
pub const ICON_FAMILY: &str = "segoe-icons";

/// Tried in order, first one found wins.
///
/// Windows 11 has both and Windows 10 has only the second. Nothing downstream
/// knows which it got, which is the same arrangement
/// [`super::VARIABLE_FONT_FILES`] has with [`super::FONT_FILES`].
pub const ICON_FILES: [&str; 2] = [
    r"C:\Windows\Fonts\SegoeIcons.ttf",
    r"C:\Windows\Fonts\segmdl2.ttf",
];

/// A mark, by what it means rather than by what it looks like.
///
/// The same bargain [`crate::view::Emphasis`] makes one level up: a caller
/// names a meaning and exactly one place turns it into a picture. It matters
/// more here than it looks, because the alternative is a `char` literal at the
/// call site - and a Private Use code point written inline is unreadable,
/// unsearchable and unverifiable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    // The nav, in the order the pages are in.
    Gear,
    Palette,
    Drive,
    Tag,
    Search,
    Open,
    Info,
    Bug,

    // Inside a button.
    Add,
    Remove,
    Copy,
    Browse,
    Reset,
    External,
    /// Everything that did not fit: the panel's actions menu.
    More,

    // On a result row, chosen by the file's extension. Six marks rather than
    // one per type: a vocabulary somebody has to learn is worse than no
    // vocabulary at all, and these are the distinctions that actually matter
    // to somebody looking for a drawing. See [`crate::gui::panel::icons`].
    FilePdf,
    FileDrawing,
    FileImage,
    FileDocument,
    FileArchive,
    File,
}

impl Icon {
    /// Every one, for the test that proves the machine can draw them.
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

    /// The code point, with the name Microsoft gives it.
    ///
    /// Written as an escape rather than as the character, deliberately: these
    /// are Private Use, so the literal would be a box in every editor that
    /// opens this file and nobody could review it. The name in the comment is
    /// what makes a wrong number findable; the test below is what makes a
    /// wrong number fail.
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

    /// One character, ready to be laid out.
    pub fn text(self) -> String {
        self.ch().to_string()
    }
}

/// Beside a nav entry.
pub const ICON_NAV: f32 = 16.0;
/// Inside a small button on a row.
pub const ICON_INLINE: f32 = 14.0;

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICON_FAMILY.into()))
}

/// Whether this machine can actually draw it.
///
/// Per glyph rather than per font, because Segoe MDL2 Assets is the smaller of
/// the two sets and a machine on Windows 10 may have the file without having
/// the mark.
///
/// A family bound to no data lays every string out zero-wide rather than
/// panicking - see [`crate::gui::fonts`] - so the failure this guards against
/// is a blank gutter and not a crash. But a blank gutter that something
/// reserved room for is worse than one that nothing did, which is what asking
/// first buys.
pub fn has_icon(ctx: &eframe::egui::Context, icon: Icon) -> bool {
    ctx.fonts_mut(|fonts| fonts.has_glyph(&icon_font(ICON_NAV), icon.ch()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::fonts;

    /// Two entries drawn with the same mark are one mark and a wasted slot.
    ///
    /// The same shape as `no_tone_depends_on_colour_alone`, and for the same
    /// reason: a vocabulary whose members are not distinguishable is not a
    /// vocabulary.
    #[test]
    fn every_icon_is_a_different_mark() {
        let mut seen: Vec<char> = Icon::ALL.iter().map(|i| i.ch()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), Icon::ALL.len(), "two icons share a code point");
    }

    /// Every code point above is one the installed font actually has.
    ///
    /// This is the whole verification. A Private Use code point cannot be
    /// checked by reading it, the name in the comment beside it is a claim
    /// rather than a fact, and a wrong number is silent: [`has_icon`] returns
    /// false and the mark simply is not drawn.
    ///
    /// Skipped where there is no icon font to ask - a build agent, or a
    /// machine older than Windows 10 - which is the same accommodation
    /// `the_families_are_registered_even_when_the_files_are_missing` makes.
    ///
    /// What it cannot prove is that `E8EC` is a *tag* rather than some other
    /// perfectly real glyph. That needs an eye, and the picture behind
    /// `ui-snapshots` is where one gets pointed at it.
    #[test]
    fn every_icon_this_program_asks_for_is_one_the_machine_can_draw() {
        let ctx = eframe::egui::Context::default();
        let found = fonts::install(&ctx);
        if !found.icons {
            return;
        }

        // Inside a pass, because egui has no font atlas at all until the
        // first one.
        let mut output = ctx.run_ui(Default::default(), |ui| {
            for icon in Icon::ALL {
                assert!(
                    has_icon(ui.ctx(), icon),
                    "{icon:?} is U+{:04X}, which the icon font has not got",
                    icon.ch() as u32
                );
            }
        });

        // The atlas the pass built is a texture upload nobody here is going to
        // perform, and epaint asserts on dropping unapplied ones.
        output.textures_delta.clear();
    }

    /// With no icon font the family still has to exist, or the first nav entry
    /// drawn takes the process down.
    #[test]
    fn asking_about_an_icon_is_safe_on_a_machine_that_has_none() {
        let ctx = eframe::egui::Context::default();
        fonts::install_bundled(&ctx);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            // The assertion is that this returns at all: egui panics on a
            // family it was never given.
            let _ = has_icon(ui.ctx(), Icon::Gear);
        });
        output.textures_delta.clear();
    }
}
