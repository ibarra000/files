//! The list of pages down the left.
//!
//! Always open, never collapsible, and not resizable. Ueli's is the same and
//! the reason is the same: a nav with nine fixed entries has nothing to
//! reveal, so a control that hides it is a control that can only put the
//! window into a state nobody wanted.
//!
//! # Focus selects
//!
//! Arrowing down the list moves the page, rather than moving a dotted
//! outline that then needs Enter. It is what Ueli does, it is what the
//! Windows settings app does, and it is the behaviour that makes the nav
//! usable without a mouse at all.

use eframe::egui;

use super::widgets;
use crate::gui::theme::{self, Icon, Theme};
use crate::view::settings::PageId;

/// The heading over the list.
///
/// One section rather than Ueli's two, because the second of Ueli's was the
/// extensions and this program has none.
const HEADING: &str = "Settings";

/// Which mark a page is drawn with.
///
/// Here rather than in [`crate::view::settings`], which names meanings and
/// never appearances. A page id is the meaning; this is the picture.
const fn icon(page: PageId) -> Icon {
    match page {
        PageId::General => Icon::Gear,
        PageId::Appearance => Icon::Palette,
        PageId::Drives => Icon::Drive,
        PageId::Aliases => Icon::Tag,
        PageId::Searching => Icon::Search,
        PageId::Opening => Icon::Open,
        PageId::About => Icon::Info,
        PageId::Diagnostics => Icon::Bug,
    }
}

/// Draws the list and reports which page should now be showing.
pub fn show(ui: &mut egui::Ui, theme: &Theme, current: PageId) -> PageId {
    let mut chosen = current;

    // Asked once rather than per entry. The answer cannot differ between
    // them - the font is either registered or it is not - and the gutter
    // has to close up for the whole list or for none of it, or the labels
    // would not line up.
    let icons = theme::has_icon(ui.ctx(), Icon::Gear);

    widgets::group_heading(ui, theme, HEADING);
    ui.add_space(2.0);

    for page in PageId::ALL {
        let response =
            widgets::nav_item(ui, theme, icon(page), page.title(), page == current, icons);
        // `gained_focus` as well as `clicked`, which is what makes arrowing
        // down the list actually move the page. egui already turns a bare
        // Up or Down into a focus move, so this is the whole of keyboard
        // navigation.
        if response.clicked() || response.gained_focus() {
            chosen = page;
        }
    }

    chosen
}
