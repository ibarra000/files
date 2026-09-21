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
//!
//! Two things follow from it that are easy to leave out. An entry that gains
//! focus has to be scrolled to, because in a short window the arrow key can
//! otherwise select a page that is off the bottom of the list - the page
//! changes, the nav does not move, and nothing on screen says why. And the
//! *current* page has to be scrolled to on arrival, because the window can
//! be opened straight onto Diagnostics, which is the eighth of eight.

use eframe::egui;

use super::widgets;
use crate::gui::theme::{self, Icon, Theme};
use crate::view::settings::PageId;

/// The heading over the list.
///
/// One section rather than Ueli's two, because the second of Ueli's was the
/// extensions and this program has none.
const HEADING: &str = "Settings";

/// The air between the heading and the first pill.
///
/// Eight rather than two. The heading is a twelve-point caption and the pill
/// under it is a thirty-two point target; two points apart they read as a
/// label attached to the first entry rather than as a heading over all of
/// them.
const HEADING_GAP: f32 = 8.0;

/// And between one pill and the next. See [`show`].
const PILL_GAP: f32 = 2.0;

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

    // Whether the window has drawn this page before. The reveal below has to
    // happen once, on arrival, and not on every frame - a `scroll_to_me`
    // asked for every frame is a nav that cannot be scrolled away from.
    let seen_id = egui::Id::new("files-settings-nav-seen");
    let arrived = ui.data(|d| d.get_temp::<PageId>(seen_id)) != Some(current);
    ui.data_mut(|d| d.insert_temp(seen_id, current));

    widgets::group_heading(ui, theme, HEADING);
    ui.add_space(HEADING_GAP);

    // Two points between pills, not the five a list of setting tiles takes.
    // These are thirty-two points tall and touch the same fill on hover, so
    // five points of window showing between them reads as eight separate
    // objects rather than as one list of eight.
    ui.spacing_mut().item_spacing.y = PILL_GAP;

    for page in PageId::ALL {
        let response =
            widgets::nav_item(ui, theme, icon(page), page.title(), page == current, icons);
        // `gained_focus` as well as `clicked`, which is what makes arrowing
        // down the list actually move the page. egui already turns a bare
        // Up or Down into a focus move, so this is the whole of keyboard
        // navigation.
        if response.clicked() || response.gained_focus() {
            chosen = page;
            // `None` is `block: "nearest"`, the same policy the panel's
            // result list uses: move by the least it takes, and do nothing
            // at all when the entry is already on screen.
            response.scroll_to_me(None);
        } else if page == current && arrived {
            response.scroll_to_me(None);
        }
    }

    chosen
}
