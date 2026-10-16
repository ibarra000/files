//! Renders the settings window. The wording lives in
//! [`crate::view::settings`]; this module handles layout only, so wording can
//! be tested without a font.
//!
//! Setting cards use a flat fill rather than `theme::raise` shading: a page
//! holds a dozen or more cards, and the feathered shadows would cost thousands
//! of shapes per second while the window is open.
//!
//! The nav and the page each have their own `ScrollArea`, so scrolling a long
//! page does not move the nav.

pub mod app;
pub mod exists;
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

/// Draws the nav and the current page, and returns the page to show next.
///
/// The caller builds `pages` once per frame and shares it with
/// [`wants_report`].
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
        // egui 0.36 uses one `Panel` type for all edges, so the setter is
        // `exact_size` rather than `exact_width`.
        .exact_size(theme::NAV_W)
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
                // A distinct salt from the nav; otherwise both share one
                // scroll offset.
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

/// Returns whether the current page needs the `doctor` report.
pub fn wants_report(pages: &[Page], current: PageId) -> bool {
    page::find(pages, current).is_some_and(page::wants_report)
}

/// Returns `colour` with full opacity.
///
/// The settings window stays open for long reads, where a translucent
/// background hurts legibility.
pub fn opaque(colour: egui::Color32) -> egui::Color32 {
    egui::Color32::from_rgb(colour.r(), colour.g(), colour.b())
}
