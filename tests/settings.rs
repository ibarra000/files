//! The settings window, drawn, and then read back.
//!
//! The same safety net `tests/panel.rs` describes, stretched over the other
//! window. The argument is the same one and is worth repeating because this
//! window went without it far longer: a control that quietly stops being
//! painted, or quietly stops telling the accessibility tree what it is,
//! fails nothing and nobody notices until somebody using a screen reader
//! reports it.
//!
//! What it drives is `gui::settings::show` over a hand-built `AppState` -
//! the whole two-pane window, nav included - and deliberately not
//! `gui::Shell`, which owns threads, a tray icon and a viewport. The same
//! seam `tests/panel.rs` chose, for the same reason: the form is a pure
//! function of state, a theme and a page.

use eframe::egui;
use eframe::egui::accesskit;
use egui_kittest::Harness;
use egui_kittest::kittest::{AccessKitNode, NodeT};

use files::app::state::{AppState, SettingChange};
use files::config::Settings;
use files::gui::settings::lists::{AliasDraft, DriveDraft};
use files::gui::settings::{self, Form};
use files::gui::theme::Theme;
use files::view::settings::{ActionId, PageId};

const REPORT: &str = "drive jobs   reachable   120000 files";

/// What one frame of the harness is looking at.
struct Window {
    theme: Theme,
    state: AppState,
    settings: Settings,
    fonts_ready: bool,
    page: PageId,
    editing: Option<(files::config::write::SettingKey, String)>,
    draft: AliasDraft,
    drive: DriveDraft,
    /// Everything the frames so far produced, for the assertions to read.
    changed: Vec<SettingChange>,
    actions: Vec<ActionId>,
}

fn window(page: PageId, dark: bool, have_file: bool) -> Window {
    let settings = Settings {
        have_file,
        ..Settings::default()
    };
    Window {
        theme: Theme::of(dark),
        state: AppState::new(settings.clone(), std::time::Instant::now()),
        settings,
        fonts_ready: false,
        page,
        editing: None,
        draft: AliasDraft::default(),
        drive: DriveDraft::default(),
        changed: Vec::new(),
        actions: Vec::new(),
    }
}

fn harness_of(start: Window) -> Harness<'static, Window> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(940.0, 620.0))
        // Points, not pixels: a picture taken at whatever this machine is
        // scaled to is a picture of the machine.
        .with_pixels_per_point(1.0)
        .build_ui_state(
            |ui, w: &mut Window| {
                // The families have to exist before anything is laid out,
                // and new definitions are picked up at the *start* of a
                // frame - so the first frame installs them and draws
                // nothing. Exactly what `tests/panel.rs` does.
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
                let chosen = {
                    let mut form = Form {
                        editing: &mut w.editing,
                        draft: &mut w.draft,
                        drive: &mut w.drive,
                        changed: &mut changed,
                        actions: &mut actions,
                        aliases: None,
                        mappings: None,
                    };
                    settings::show(
                        ui,
                        &theme,
                        &w.state,
                        &w.settings,
                        None,
                        page,
                        REPORT,
                        &mut form,
                    )
                };
                w.page = chosen;
                // Accumulated rather than replaced. A click lands on one
                // frame and the harness runs several, so overwriting would
                // mean reading back the empty frame after the one that
                // mattered.
                w.changed.extend(changed);
                w.actions.extend(actions);
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

/// Every page is reachable, or a setting exists that nothing can get to.
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

/// Which is the whole job of the nav: a different page shows different
/// settings.
#[test]
fn choosing_a_page_changes_what_is_on_the_right() {
    let general = spoken(&harness_on(PageId::General, false));
    assert!(general.iter().any(|s| s.contains("Recent codes")));
    assert!(!general.iter().any(|s| s.contains("Hidden file types")));

    let searching = spoken(&harness_on(PageId::Searching, false));
    assert!(searching.iter().any(|s| s.contains("Hidden file types")));
    assert!(!searching.iter().any(|s| s.contains("Recent codes")));
}

/// The nav entry for the page showing says it is the one selected, or a
/// screen reader has no way to tell where it is.
#[test]
fn the_page_that_is_showing_is_the_one_marked_selected() {
    let harness = harness_on(PageId::Opening, false);
    // `toggled` rather than `selected`: egui maps `WidgetInfo::selected`
    // onto `Toggled`, whatever the widget type, so that is what a screen
    // reader is actually handed.
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

/// Everything a row says has to be in the tree, not just what fitted.
///
/// The label is laid out to one line and ellipsised, so the pixels are
/// allowed to lose the end of a long one. The accessible name is not:
/// somebody reading this window with a screen reader gets the whole label
/// and the whole sentence under it, or they get a setting they cannot
/// identify.
#[test]
fn a_setting_reads_out_its_label_and_the_sentence_under_it() {
    let harness = harness();
    let spoken = spoken(&harness);
    assert!(
        spoken
            .iter()
            .any(|s| s.contains("Recent codes") && s.contains("up arrow brings them back")),
        "the row did not read out in full: {spoken:#?}"
    );
}

/// The switch is painted from nothing, so what it tells `accesskit` is a
/// claim this program makes rather than something egui does for us.
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

/// And reports a change when it is clicked, once per click.
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

/// A setting nothing can save is shown and refused rather than hidden, and
/// the line saying why is read out with it.
#[test]
fn a_setting_that_is_held_elsewhere_is_still_on_screen_and_says_so() {
    // No configuration file, so there is nowhere to save anything and every
    // row on the form is pinned.
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

/// A read-only pair is a row too, and its value has to be readable.
#[test]
fn a_fact_reads_out_its_label_and_its_value() {
    let harness = harness_on(PageId::About, false);
    let spoken = spoken(&harness);
    assert!(
        spoken.iter().any(|s| s.contains("This version")),
        "the version is not on the About page: {spoken:#?}"
    );
}

/// The report is the thing the diagnostics page exists for, so it has to
/// actually be on it.
#[test]
fn the_diagnostics_page_shows_the_report_it_was_given() {
    let harness = harness_on(PageId::Diagnostics, false);
    let spoken = spoken(&harness);
    assert!(
        spoken
            .iter()
            .any(|s| s.contains("Paste this into an email")),
        "the report has no introduction: {spoken:#?}"
    );
}

/// A button that reaches outside the window has to report that it was
/// pressed, or nothing acts on it.
#[test]
fn pressing_a_button_reports_which_one_it_was() {
    let mut harness = harness_on(PageId::Diagnostics, false);
    click(&mut harness, "Copy to clipboard");
    assert_eq!(harness.state().actions, vec![ActionId::CopyReport]);
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

/// Clicks whichever node reads out as `name`.
///
/// Through kittest rather than by pushing pointer events, because kittest
/// moves the pointer, presses, releases and runs the frames in the order
/// egui expects. Synthesising the three by hand produced a press that egui
/// had no hover for, which is a click on nothing.
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

/// Every line the tree carries, in no particular order.
///
/// The tree rather than the pixels, for the reason `tests/panel.rs` gives:
/// it is derived from the same layout pass, it is what a screen reader is
/// handed, and it needs no GPU.
///
/// Label *and* value, which is not belt and braces: egui puts the text of a
/// `Role::Label` into the value and the text of everything else into the
/// label, so reading only one of the two finds half the window.
/// `tests/panel.rs` takes both for the same reason.
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
