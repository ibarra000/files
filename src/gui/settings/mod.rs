//! The settings window, drawn.
//!
//! The words are in [`crate::view::settings`], which knows nothing about
//! rectangles; this is the half that knows nothing about wording. The split
//! is the one [`crate::view`]'s module note argues for, and it is why the
//! house-style test can check every line on the form without a font.
//!
//! # A card per setting, and no shading
//!
//! The panel next door is soft-UI: a surface is the colour of its ground and
//! is legible only by the shading at its edges, which `theme::raise` and
//! `theme::press` paint. This window is not, and the difference is deliberate
//! rather than an oversight.
//!
//! A form is a list of small tiles, one per setting, and `raise` costs two
//! feathered shadow shapes plus a fill for every one of them. A page here
//! holds a dozen or more, and the shell asks for a frame every time one is
//! open, so the shading would be several thousand feathered shapes a second
//! to say something a flat fill one step off the ground says for nothing.
//!
//! The panel keeps its shading, where two surfaces earn it.
//!
//! # Two columns, two scrolls
//!
//! A `SidePanel` and a `CentralPanel`, each with a `ScrollArea` of its own,
//! so the nav does not move when a long page is scrolled and a short page
//! does not leave the nav stranded. That is the arrangement Ueli has and it
//! is the only one that behaves when the two columns differ in height, which
//! they almost always do.

pub mod lists;
pub mod measure;
pub mod nav;
pub mod page;
pub mod report;
pub mod widgets;

use eframe::egui;

use crate::config::Settings;
use crate::gui::theme::{self, Theme};
use crate::view::settings::{Page, PageId};

pub use page::Form;

/// Draws the whole window: the list of pages, and the page.
///
/// Takes the current page by value and returns the one that should show
/// next, rather than taking `&mut`, so the caller keeps the only copy. The
/// same reason the window returns a `Clicked` instead of reaching back into
/// the shell.
///
/// Takes the pages already built, which it did not used to. `view::settings::
/// pages` walks the whole configuration and allocates a `Page` for each of
/// the eight, and it was being called twice on the frame the window opened -
/// once here and once in `wants_report` - for one answer each. Now the
/// caller builds it once and both read the same slice.
pub fn show(
    ui: &mut egui::Ui,
    theme: &Theme,
    pages: &[Page],
    settings: &Settings,
    current: PageId,
    report: report::View<'_>,
    form: &mut Form<'_>,
) -> PageId {
    let mut chosen = current;

    egui::containers::Panel::left("files-settings-nav")
        // `exact_size`, not `exact_width`: in 0.36 one `Panel` type serves
        // all four edges and the setter is named for the edge it is on.
        .exact_size(theme::NAV_W)
        // Ueli's is neither, and there is nothing here to reveal.
        .resizable(false)
        .show_separator_line(true)
        .frame(
            egui::Frame::new()
                .fill(opaque(theme.surface))
                .inner_margin(egui::Margin::symmetric(8, 12)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("files-settings-nav-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    chosen = nav::show(ui, theme, current);
                });
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(opaque(theme.surface))
                .inner_margin(theme::CONTENT_PAD),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                // Salted apart from the nav, or the two share an offset and
                // the list jumps every time a long page is scrolled.
                .id_salt("files-settings-content-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let Some(page) = page::find(pages, current) else {
                        return;
                    };
                    page::show(ui, theme, page, settings, report, form);
                });
        });

    chosen
}

/// Whether the page showing needs the `doctor` report taken.
pub fn wants_report(pages: &[Page], current: PageId) -> bool {
    page::find(pages, current).is_some_and(page::wants_report)
}

/// The panel's surface without its transparency.
///
/// This window is a document rather than an overlay: it sits over other
/// programs for minutes at a time, and text on a translucent ground is
/// harder to read the longer you read it.
pub fn opaque(colour: egui::Color32) -> egui::Color32 {
    egui::Color32::from_rgb(colour.r(), colour.g(), colour.b())
}
