//! The page list in the left column. It has a fixed set of entries, so it is
//! neither collapsible nor resizable.
//!
//! Focus selects: arrowing through the list changes the page, matching the
//! Windows settings app. An entry that gains focus is scrolled into view, and
//! so is the current page on arrival, since the window can open on the last
//! page.

use eframe::egui;

use super::widgets;
use crate::gui::theme::{self, Icon, Theme};
use crate::view::settings::PageId;

const HEADING: &str = "Settings";

/// Space between the heading and the first pill, in points. Wide enough that
/// the heading reads as covering the whole list, not labelling the first entry.
const HEADING_GAP: f32 = 8.0;

/// Space between adjacent pills, in points.
const PILL_GAP: f32 = 2.0;

/// Returns the icon for a page. Kept here because [`crate::view::settings`]
/// does not deal in appearance.
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

/// Draws the list and returns the page to show next.
pub fn show(ui: &mut egui::Ui, theme: &Theme, current: PageId) -> PageId {
    let mut chosen = current;

    // Checked once for the whole list so all labels share the same gutter.
    let icons = theme::has_icon(ui.ctx(), Icon::Gear);

    // Scroll to the current page only on arrival; scrolling every frame
    // would pin the nav in place.
    let seen_id = egui::Id::new("files-settings-nav-seen");
    let arrived = ui.data(|d| d.get_temp::<PageId>(seen_id)) != Some(current);
    ui.data_mut(|d| d.insert_temp(seen_id, current));

    widgets::group_heading(ui, theme, HEADING);
    ui.add_space(HEADING_GAP);

    // Tighter than tile spacing so the pills read as one list.
    ui.spacing_mut().item_spacing.y = PILL_GAP;

    for page in PageId::ALL {
        let response =
            widgets::nav_item(ui, theme, icon(page), page.title(), page == current, icons);
        // egui turns Up/Down into focus moves, so `gained_focus` provides
        // keyboard navigation.
        if response.clicked() || response.gained_focus() {
            chosen = page;
            // `None` scrolls the minimum distance, and not at all when the
            // entry is already visible.
            response.scroll_to_me(None);
        } else if page == current && arrived {
            response.scroll_to_me(None);
        }
    }

    chosen
}
