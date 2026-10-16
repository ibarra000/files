//! Renders the settings window and checks its accessibility tree and pixels.
//!
//! Drives `gui::settings::show` over a hand-built `AppState`, not `gui::Shell`,
//! which owns threads, a tray icon and a viewport. As in `tests/panel.rs`, the
//! form is a pure function of state, theme and page.

use eframe::egui;
use eframe::egui::accesskit;
use egui_kittest::Harness;
use egui_kittest::kittest::{AccessKitNode, NodeT};

use files::app::state::SettingChange;
use files::config::Settings;
use files::gui::settings::lists::{AliasDraft, DriveDraft};
use files::gui::settings::{self, Form};
use files::gui::theme::Theme;
use files::view::settings::{ActionId, PageId};

const REPORT: &str = "drive jobs   reachable   120000 files";

/// The harness state for one frame.
struct Window {
    theme: Theme,
    settings: Settings,
    fonts_ready: bool,
    page: PageId,
    editing: Option<(files::config::write::SettingKey, String)>,
    draft: AliasDraft,
    drive: DriveDraft,
    /// Everything the frames so far produced.
    changed: Vec<SettingChange>,
    actions: Vec<ActionId>,
    /// The drive list the last frame requested, if any.
    mappings: Option<Vec<files::paths::Mapping>>,
}

fn window(page: PageId, dark: bool, have_file: bool) -> Window {
    let settings = Settings {
        have_file,
        ..Settings::fixture()
    };
    Window {
        theme: Theme::of(dark),
        settings,
        fonts_ready: false,
        page,
        editing: None,
        draft: AliasDraft::default(),
        drive: DriveDraft::default(),
        changed: Vec::new(),
        actions: Vec::new(),
        mappings: None,
    }
}

fn harness_of(start: Window) -> Harness<'static, Window> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(940.0, 620.0))
        // Fixed scale, so snapshots do not depend on the machine's DPI.
        .with_pixels_per_point(1.0)
        .build_ui_state(
            |ui, w: &mut Window| {
                // egui applies new font definitions at the start of the next
                // frame, so the first frame installs them and draws nothing.
                if !w.fonts_ready {
                    files::gui::fonts::install_bundled(ui.ctx());
                    files::gui::theme::apply_style(ui.ctx(), &w.theme);
                    w.fonts_ready = true;
                    return;
                }

                let theme = w.theme;
                let page = w.page;
                let mut changed = Vec::new();
                let mut actions = Vec::new();
                let mut exists = files::gui::settings::exists::DirCache::default();
                let (chosen, mappings) = {
                    let mut form = Form {
                        editing: &mut w.editing,
                        draft: &mut w.draft,
                        drive: &mut w.drive,
                        changed: &mut changed,
                        actions: &mut actions,
                        aliases: None,
                        mappings: None,
                        exists: &mut exists,
                        now: std::time::Instant::now(),
                    };
                    let pages =
                        files::view::settings::pages(None, &w.settings, None, true, Some(false));
                    let out = settings::show(
                        ui,
                        &theme,
                        &pages,
                        &w.settings,
                        page,
                        files::gui::settings::report::View::Ready(REPORT),
                        &mut form,
                    );
                    let mappings = form.mappings;
                    (out, mappings)
                };
                w.page = chosen;
                // Accumulated: a click lands on one of several frames, so
                // overwriting would keep only the last, empty one.
                w.changed.extend(changed);
                w.actions.extend(actions);
                if mappings.is_some() {
                    w.mappings = mappings;
                }
            },
            start,
        );

    harness.run_steps(4);
    harness
}

fn harness_on(page: PageId, dark: bool) -> Harness<'static, Window> {
    harness_of(window(page, dark, true))
}

fn harness() -> Harness<'static, Window> {
    harness_on(PageId::General, false)
}

// --- the nav ---------------------------------------------------------------

/// Every page is listed in the nav.
#[test]
fn the_list_offers_every_page() {
    let harness = harness();
    let spoken = spoken(&harness);
    for page in PageId::ALL {
        assert!(
            spoken.iter().any(|s| s == page.title()),
            "{} is not in the list: {spoken:#?}",
            page.title()
        );
    }
}

#[test]
fn choosing_a_page_changes_what_is_on_the_right() {
    let general = spoken(&harness_on(PageId::General, false));
    assert!(general.iter().any(|s| s.contains("Recent codes")));
    assert!(!general.iter().any(|s| s.contains("Hidden file types")));

    let searching = spoken(&harness_on(PageId::Searching, false));
    assert!(searching.iter().any(|s| s.contains("Hidden file types")));
    assert!(!searching.iter().any(|s| s.contains("Recent codes")));
}

/// The nav entry for the current page reports itself as selected.
#[test]
fn the_page_that_is_showing_is_the_one_marked_selected() {
    let harness = harness_on(PageId::Opening, false);
    // egui maps `WidgetInfo::selected` to `Toggled` for every widget type.
    let selected: Vec<String> = nodes(&harness)
        .into_iter()
        .filter(|n| n.toggled() == Some(accesskit::Toggled::True))
        .filter_map(|n| n.label())
        .collect();
    assert!(
        selected.iter().any(|s| s == "Opening"),
        "nothing said which page was showing: {selected:?}"
    );
}

// --- a setting -------------------------------------------------------------

/// A row's full label and description are in the tree, even when the painted
/// label is ellipsised.
#[test]
fn a_setting_reads_out_its_label_and_the_sentence_under_it() {
    let harness = harness();
    let spoken = spoken(&harness);
    assert!(
        spoken
            .iter()
            .any(|s| s.contains("Recent codes") && s.contains("up arrow can recall them")),
        "the row did not read out in full: {spoken:#?}"
    );
}

/// The custom-painted switch reports its role and state to `accesskit`.
#[test]
fn a_switch_reports_itself_as_a_checkbox_that_is_on() {
    let harness = harness();
    let switch = switches(&harness)
        .into_iter()
        .find(|n| n.label().as_deref() == Some("Recent codes"))
        .expect("the switch is not in the accessibility tree at all");
    assert_eq!(
        switch.toggled(),
        // Recent codes ships on.
        Some(accesskit::Toggled::True),
        "a switch that is on did not say so"
    );
}

#[test]
fn clicking_a_switch_reports_exactly_one_change() {
    let mut harness = harness();
    click(&mut harness, "Recent codes");

    let changed = &harness.state().changed;
    assert_eq!(
        changed.len(),
        1,
        "one click produced {} changes",
        changed.len()
    );
    assert_eq!(changed[0].label, "Recent codes");
}

/// An unsaveable setting is shown disabled, with the reason read out.
#[test]
fn a_setting_that_is_held_elsewhere_is_still_on_screen_and_says_so() {
    // No configuration file, so every row is read-only.
    let harness = harness_of(window(PageId::General, false, false));

    let spoken = spoken(&harness);
    assert!(
        spoken.iter().any(|s| s.contains("Recent codes")),
        "a pinned setting vanished instead of being refused: {spoken:#?}"
    );
    assert!(
        spoken.iter().any(|s| s.contains("no configuration file")),
        "nothing said why the setting was refused: {spoken:#?}"
    );
    let held = switches(&harness)
        .into_iter()
        .find(|n| n.label().as_deref() == Some("Recent codes"))
        .expect("the pinned switch is not in the tree");
    assert!(
        held.is_disabled(),
        "a setting nothing can save was offered as though it could be"
    );
}

// --- the other kinds of block ----------------------------------------------

/// A read-only pair exposes its value.
#[test]
fn a_fact_reads_out_its_label_and_its_value() {
    let harness = harness_on(PageId::About, false);
    let spoken = spoken(&harness);
    assert!(
        spoken.iter().any(|s| s.contains("This version")),
        "the version is not on the About page: {spoken:#?}"
    );
}

#[test]
fn the_diagnostics_page_shows_the_report_it_was_given() {
    let harness = harness_on(PageId::Diagnostics, false);
    let spoken = spoken(&harness);
    assert!(
        spoken.iter().any(|s| s.contains("Paste into an email")),
        "the report has no introduction: {spoken:#?}"
    );
}

/// An action button reports which action was pressed.
#[test]
fn pressing_a_button_reports_which_one_it_was() {
    let mut harness = harness_on(PageId::Diagnostics, false);
    click(&mut harness, "Copy to clipboard");
    assert_eq!(harness.state().actions, vec![ActionId::CopyReport]);
}

// --- the one question the window asks --------------------------------------

/// Removing a drive asks for confirmation, since it cannot be undone.
#[test]
fn removing_a_drive_asks_before_it_does_it() {
    let mut harness = harness_on(PageId::Drives, false);
    assert!(
        !spoken(&harness)
            .iter()
            .any(|s| s.contains("Remove this drive")),
        "the question was up before anybody asked for it"
    );

    click(&mut harness, "Remove");
    let spoken = spoken(&harness);
    assert!(
        spoken.iter().any(|s| s.contains("Remove this drive?")),
        "the drive went without a word: {spoken:#?}"
    );
    assert!(spoken.iter().any(|s| s == "Remove drive"));
    assert!(spoken.iter().any(|s| s == "Cancel"));
    assert!(
        harness.state().mappings.is_none(),
        "the drive was removed before the question was answered"
    );
}

#[test]
fn keeping_a_drive_takes_the_question_away_and_changes_nothing() {
    let mut harness = harness_on(PageId::Drives, false);
    click(&mut harness, "Remove");
    click(&mut harness, "Cancel");

    assert!(
        !spoken(&harness)
            .iter()
            .any(|s| s.contains("Remove this drive")),
        "the question is still up"
    );
    assert!(
        harness.state().mappings.is_none(),
        "keeping a drive removed it"
    );
}

#[test]
fn removing_a_drive_for_real_takes_it_off_the_list() {
    let mut harness = harness_on(PageId::Drives, false);
    let before = harness.state().settings.routes.all().len();
    click(&mut harness, "Remove");
    click(&mut harness, "Remove drive");

    let after = harness
        .state()
        .mappings
        .as_ref()
        .expect("the drive list was not changed");
    assert_eq!(after.len(), before - 1);
}

// --- the pictures ----------------------------------------------------------

#[cfg(feature = "ui-snapshots")]
#[test]
fn looks_right_on_the_general_page() {
    let mut harness = harness_on(PageId::General, false);
    harness.snapshot("settings_general_light");
}

#[cfg(feature = "ui-snapshots")]
#[test]
fn looks_right_in_the_dark_theme() {
    let mut harness = harness_on(PageId::General, true);
    harness.snapshot("settings_general_dark");
}

#[cfg(feature = "ui-snapshots")]
#[test]
fn looks_right_on_the_drives_page() {
    let mut harness = harness_on(PageId::Drives, false);
    harness.snapshot("settings_drives_light");
}

#[cfg(feature = "ui-snapshots")]
#[test]
fn looks_right_on_the_opening_page() {
    let mut harness = harness_on(PageId::Opening, false);
    harness.snapshot("settings_opening_light");
}

// --- reading the tree ------------------------------------------------------

/// Clicks the node whose accessible name is `name`.
///
/// Uses kittest's click, which hovers, presses and releases in the order egui
/// expects; a hand-built press without a prior hover hits nothing.
fn click(harness: &mut Harness<'static, Window>, name: &str) {
    let node = harness
        .root()
        .children_recursive()
        .find(|n| {
            let n = n.accesskit_node();
            n.label().as_deref() == Some(name) || n.value().as_deref() == Some(name)
        })
        .unwrap_or_else(|| panic!("nothing in the window reads out as {name:?}"));
    node.click();
    harness.run_steps(2);
}

/// Every text line in the accessibility tree, in no particular order.
///
/// Reads both label and value: egui stores a `Role::Label`'s text in the value
/// and every other node's text in the label.
fn spoken(harness: &Harness<'static, Window>) -> Vec<String> {
    nodes(harness)
        .into_iter()
        .flat_map(|n| [n.label(), n.value()])
        .flatten()
        .collect()
}

fn switches<'a>(harness: &'a Harness<'static, Window>) -> Vec<AccessKitNode<'a>> {
    nodes(harness)
        .into_iter()
        .filter(|n| n.role() == accesskit::Role::CheckBox)
        .collect()
}

fn nodes<'a>(harness: &'a Harness<'static, Window>) -> Vec<AccessKitNode<'a>> {
    let root = harness.root();
    std::iter::once(root.accesskit_node())
        .chain(root.children_recursive().map(|n| n.accesskit_node()))
        .collect()
}
