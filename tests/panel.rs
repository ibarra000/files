//! Renders the panel through `egui_kittest` and asserts on what it draws.
//!
//! Assertions read the accessibility tree, which is derived from the layout
//! pass and needs no GPU. Pixel snapshots are behind `--features ui-snapshots`
//! because they need a GPU adapter.
//!
//! Drives `gui::panel::show` and `gui::frame::Frame` over a hand-built
//! `AppState`, not `gui::Shell`, which owns threads, a tray icon and a window.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;

use files::app::event::{AppEvent, IndexMsg, SearchMsg};
use files::app::key::{Key, KeyEvent, Mods};
use files::app::state::AppState;
use files::config::{ResultLayout, Settings, VISIBLE_ROWS, VISIBLE_ROWS_DETAILED, ViewerKind};
use files::gui::frame::Frame;
use files::gui::theme::{self, Theme};
use files::index::store::{IndexStatus, Origin};
use files::paths::MappingId;
use files::search::matcher::{Hit, SearchOutcome};
use files::search::query::Query;

/// One frame at sixty a second.
const DT: f32 = 1.0 / 60.0;

/// The outer margin `egui_kittest` adds around its `Ui`; the window is grown by
/// it so the panel gets its real size.
const HARNESS_MARGIN: f32 = 8.0;

// --- fixtures --------------------------------------------------------------

fn hit(name: &str) -> Hit {
    Hit {
        path: Arc::from(format!("R:\\abc\\AB-C-0704\\{name}").as_str()),
        name: Arc::from(name),
        match_pos: 0,
        index: 0,
    }
}

/// Types `term` and delivers `hits` as the matcher's result.
///
/// `matched` is the total the index found, which can exceed `hits.len()`
/// because the search is capped.
fn with_results(state: &mut AppState, term: &str, hits: Vec<Hit>, matched: u32, now: Instant) {
    for c in term.chars() {
        state.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
    }
    let epoch = state.query_epoch();
    state.update(
        AppEvent::Search(SearchMsg {
            epoch,
            query: Query::contains(term),
            elapsed: Duration::ZERO,
            result: Ok(SearchOutcome {
                hits,
                matched,
                total: 1_284_551,
                cancelled: false,
                unicode_fallback: false,
            }),
        }),
        now,
    );
}

fn many(n: usize) -> Vec<Hit> {
    (0..n)
        .map(|i| hit(&format!("AB-C-0704-{i:02}.pdf")))
        .collect()
}

/// The panel and everything it is drawn from, carried through the harness.
struct Panel {
    state: AppState,
    frame: Frame,
    theme: Theme,
    wall: SystemTime,
    fonts_ready: bool,
}

impl Panel {
    fn new(state: AppState) -> Self {
        let mut frame = Frame::new();
        frame.motion.summon();
        Self {
            state,
            frame,
            // Pinned so snapshots do not depend on the system theme.
            theme: Theme::light(),
            // Fixed so the status line's age text is stable.
            wall: SystemTime::UNIX_EPOCH,
            fonts_ready: false,
        }
    }
}

/// Builds a harness around the real panel, run past its entrance animation.
///
/// The backdrop is `None`, so the panel paints its own surface.
fn harness(state: AppState) -> Harness<'static, Panel> {
    let mut harness = Harness::builder()
        // Compensates for the harness margin so the panel gets the same
        // rectangle as in the real program.
        .with_size(egui::vec2(
            theme::PANEL_W + HARNESS_MARGIN * 2.0,
            theme::PANEL_H + HARNESS_MARGIN * 2.0,
        ))
        // Fixed scale so snapshots do not depend on the machine's DPI.
        .with_pixels_per_point(1.0)
        .with_step_dt(DT)
        .build_ui_state(
            |ui, panel: &mut Panel| {
                // egui panics on an unregistered font family, so the bundled
                // fonts are installed before any layout. Font definitions take
                // effect at the start of the next frame, so the first frame
                // only installs them and draws nothing.
                if !panel.fonts_ready {
                    files::gui::fonts::install_bundled(ui.ctx());
                    // The results scrollbar is an egui widget and reads the
                    // style; without this it uses egui's defaults.
                    files::gui::theme::apply_style(ui.ctx(), &panel.theme);
                    panel.fonts_ready = true;
                    return;
                }
                let content = panel.frame.advance(&panel.state, DT);
                files::gui::panel::show(ui, &panel.state, &panel.theme, content, None, panel.wall);
            },
            Panel::new(state),
        );

    // Run past the entrance animation.
    harness.run_steps(40);
    harness
}

/// The default settings plus one configured viewer, `sketch`.
fn with_sketch() -> Settings {
    Settings {
        viewers: std::sync::Arc::new(files::config::Viewers::new(vec![
            files::config::ViewerApp::new("sketch", "sketch.exe", &["tif"]),
        ])),
        ..Settings::fixture()
    }
}

fn state() -> (AppState, Instant) {
    let now = Instant::now();
    (AppState::new(with_sketch(), now), now)
}

/// A state with nothing typed and a healthy index, so the status line is
/// empty.
///
/// A fresh `state()` has no index and shows the "No file list yet" notice.
fn quiet_state() -> (AppState, Instant) {
    let (mut s, now) = state();
    let status = IndexStatus {
        origin: Some(Origin::Network),
        entries: 10,
        built_at: Some(SystemTime::UNIX_EPOCH),
        ..Default::default()
    };
    s.update(
        AppEvent::Index(IndexMsg::Status {
            id: MappingId(0),
            status: Arc::new(status),
        }),
        now,
    );
    // The footer's status text is the public way to check for a notice.
    let said = files::view::status::render(&s, SystemTime::UNIX_EPOCH).text;
    assert!(
        said.is_empty(),
        "the fixture has something to say - {said:?} - so it proves nothing"
    );
    (s, now)
}

/// Returns every label and value in the accessibility tree, one per line.
fn on_screen(harness: &Harness<'_, Panel>) -> String {
    let root = harness.root();
    std::iter::once(root)
        .chain(root.children_recursive())
        .flat_map(|node| {
            let accesskit = node.accesskit_node();
            [accesskit.label(), accesskit.value()]
        })
        .flatten()
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

// --- what the panel says ---------------------------------------------------

/// Raises a toast by pressing Enter on a search with no results.
fn toasted(now: Instant) -> AppState {
    let (mut s, _) = state();
    with_results(&mut s, "AB-C-0704", Vec::new(), 0, now);
    s.update(AppEvent::Key(KeyEvent::new(Key::Enter, Mods::NONE)), now);
    assert!(s.toast.is_some(), "the fixture raised no toast");
    s
}

#[test]
fn a_toast_is_actually_on_screen() {
    let h = harness(toasted(Instant::now()));
    let screen = on_screen(&h);
    assert!(
        screen.contains("Nothing to open"),
        "the toast was computed and thrown away:\n{screen}"
    );
}

#[test]
fn a_toast_gives_the_line_back_when_it_expires() {
    let now = Instant::now();
    let mut s = toasted(now);
    s.update(AppEvent::Tick, now + files::app::state::TOAST_LIFETIME);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        !screen.contains("Nothing to open"),
        "the toast outlived its lifetime:\n{screen}"
    );
}

#[test]
fn the_footer_says_how_many_the_search_found() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(300), 300, now);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        screen.contains("300 items"),
        "nothing said how many there were:\n{screen}"
    );
}

/// With no results the footer shows no count and no default-action button.
#[test]
fn the_footer_counts_nothing_when_there_is_nothing_to_count() {
    let (s, _) = state();
    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        !screen.contains("Results"),
        "a caption over a list that is not there:\n{screen}"
    );
    assert!(
        !screen.contains("\u{b7} Enter"),
        "the footer offered to open nothing:\n{screen}"
    );
    // The settings and actions buttons stay; they do not depend on results.
    assert!(screen.contains("Settings"), "{screen}");
}

/// The default-action button names what Enter does for the current viewer.
#[test]
fn the_footer_says_what_enter_will_do() {
    for (viewer, label) in [
        (ViewerKind::Auto, "Open"),
        (ViewerKind::Pdf, "Open as combined PDF"),
        (ViewerKind::App(0), "Open with sketch"),
    ] {
        let (mut s, now) = state();
        with_results(&mut s, "AB-C-0704", many(3), 3, now);
        s.viewer = viewer;
        let h = harness(s);
        let screen = on_screen(&h);
        assert!(
            screen.contains(&format!("{label} \u{b7} Enter")),
            "{viewer:?} is not named on the footer:\n{screen}"
        );
    }
}

/// With no viewer configured, neither "Open with" nor `Ctrl+E` appears.
#[test]
fn with_no_viewers_the_menu_offers_none() {
    let now = Instant::now();
    let mut s = AppState::new(Settings::fixture(), now);
    with_results(&mut s, "AB-C-0704", many(3), 3, now);
    s.actions_open = true;

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(!screen.contains("Open with"), "{screen}");
    assert!(!screen.contains("Ctrl E"), "{screen}");
    assert!(screen.contains("Open as combined PDF"), "{screen}");
}

#[test]
fn the_actions_menu_lists_everything_else() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(3), 3, now);
    s.actions_open = true;

    let h = harness(s);
    let screen = on_screen(&h);
    for offered in [
        "Open \u{b7} Enter",
        "Open as combined PDF \u{b7} Ctrl D",
        "Open with sketch \u{b7} Ctrl E",
        "Show in Explorer \u{b7} Ctrl O",
        "Copy path \u{b7} Ctrl C",
        "Copy name",
        "Refresh drive \u{b7} F5",
        "Settings \u{b7} Ctrl ,",
    ] {
        assert!(
            screen.contains(offered),
            "the menu does not offer {offered:?}:\n{screen}"
        );
    }
}

/// With no results the menu omits file actions but keeps program actions.
#[test]
fn the_actions_menu_with_nothing_selected_offers_no_file() {
    let (mut s, _) = state();
    s.actions_open = true;

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(!screen.contains("Copy path"), "{screen}");
    assert!(!screen.contains("Show in Explorer"), "{screen}");
    assert!(screen.contains("Refresh drive"), "{screen}");
}

/// Selecting a viewer that is not installed shows a notice naming the
/// executable and the F2 fix.
#[test]
fn a_missing_viewer_is_reported_before_it_is_needed() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(3), 3, now);
    s.viewer = ViewerKind::App(0);
    s.set_viewers_missing(Box::new([true]));

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(screen.contains("sketch.exe"), "{screen}");
    assert!(screen.contains("F2"), "and says how to fix it:\n{screen}");
}

/// Browsing history shows the search and the "Recent searches" heading, and the
/// status line stays silent.
#[test]
fn browsing_the_recent_searches_puts_the_search_on_the_line() {
    let (mut s, now) = state();
    s.seed_history(vec![
        "AB-C-0704".into(),
        "P12345-001".into(),
        "XY-Z-1".into(),
    ]);
    // The first Up selects the newest search, the second the one before it.
    s.update(AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)), now);
    s.update(AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)), now);
    assert!(s.history.is_browsing(), "the fixture is not browsing");

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(screen.contains("P12345-001"), "{screen}");
    assert!(screen.contains("Recent searches"), "{screen}");
    assert!(
        !screen.contains("Searches you used before"),
        "the status line is still narrating:\n{screen}"
    );
}

#[test]
fn the_first_screen_is_a_search_box_and_nothing_else() {
    let (s, _) = quiet_state();
    let h = harness(s);
    let screen = on_screen(&h);

    assert!(screen.contains("Search"), "no placeholder:\n{screen}");
    for gone in [
        "Type a search term",
        "AB-C-0704",
        "Recent searches",
        "Viewer:",
        "Quit",
        "Esc",
    ] {
        assert!(
            !screen.contains(gone),
            "{gone:?} is still on the first screen:\n{screen}"
        );
    }
}

/// A standing notice shows the footer even with nothing typed.
#[test]
fn something_worth_saying_brings_the_footer_back() {
    // A fresh state has no index, which raises a standing notice.
    let (s, _) = state();
    let h = harness(s);
    let screen = on_screen(&h);

    assert!(
        screen.contains("No file list yet"),
        "a notice was swallowed by the quiet panel:\n{screen}"
    );
}

/// Detailed rows are taller, so fewer fit on screen.
///
/// Rows outside the clip rectangle are culled before layout, so the count in
/// the accessibility tree is the count on screen.
#[test]
fn detailed_rows_fit_fewer_to_a_screen() {
    let drawn = |layout| {
        let (mut s, now) = state();
        s.settings.result_layout = layout;
        with_results(&mut s, "AB-C-0704", many(300), 300, now);
        let h = harness(s);
        let root = h.root();
        root.children_recursive()
            .filter(|node| {
                node.accesskit_node()
                    .label()
                    .is_some_and(|l| l.contains(".pdf, in "))
            })
            .count()
    };

    let compact = drawn(ResultLayout::Compact);
    let detailed = drawn(ResultLayout::Detailed);
    assert!(compact >= VISIBLE_ROWS, "only {compact} compact rows");
    assert!(
        detailed >= VISIBLE_ROWS_DETAILED,
        "only {detailed} detailed rows"
    );
    assert!(
        detailed < compact,
        "{detailed} detailed rows against {compact} compact ones - the          setting changed nothing"
    );
}

/// A compact row does not draw its folder but still exposes it to screen
/// readers.
#[test]
fn a_compact_row_still_names_the_folder_it_is_in() {
    let (mut s, now) = state();
    s.settings.result_layout = ResultLayout::Compact;
    with_results(&mut s, "AB-C-0704", many(3), 3, now);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        screen.contains(r"AB-C-0704-00.pdf, in R:\abc\AB-C-0704"),
        "a compact row told a reader less than it knows:\n{screen}"
    );
}

#[test]
fn the_results_are_captioned() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(3), 3, now);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(screen.contains("Results"), "no caption:\n{screen}");
}

/// Moving the selection scrolls it into view, by the minimum distance.
///
/// Rows outside the clip rectangle are culled, so presence in the
/// accessibility tree means the row is on screen.
#[test]
fn walking_down_a_long_list_carries_the_view_with_it() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(300), 300, now);

    let mut h = harness(s);
    assert!(
        !on_screen(&h).contains("AB-C-0704-50.pdf"),
        "the fixture starts with row fifty already on screen"
    );

    for _ in 0..50 {
        h.state_mut()
            .state
            .update(AppEvent::Key(KeyEvent::new(Key::Down, Mods::NONE)), now);
    }
    h.run_steps(2);

    let screen = on_screen(&h);
    assert!(
        screen.contains("AB-C-0704-50.pdf"),
        "the selection walked off the bottom and the view stayed put:\n{screen}"
    );
    // Nearest-edge scrolling leaves the row at the bottom with its
    // predecessor still visible.
    assert!(
        screen.contains("AB-C-0704-49.pdf"),
        "it scrolled further than it had to:\n{screen}"
    );
}

#[test]
fn the_pointer_is_an_i_beam_over_the_search_box_and_nowhere_else() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(10), 10, now);
    let mut h = harness(s);

    h.hover_at(egui::pos2(
        HARNESS_MARGIN + theme::PANEL_W / 2.0,
        HARNESS_MARGIN + theme::HEADER_H / 2.0,
    ));
    h.run_steps(2);
    assert_eq!(
        h.output().platform_output.cursor_icon,
        egui::CursorIcon::Text,
        "no I-beam over the box"
    );

    h.hover_at(egui::pos2(
        HARNESS_MARGIN + theme::PANEL_W / 2.0,
        HARNESS_MARGIN + theme::HEADER_H + theme::CONTENT_H / 2.0,
    ));
    h.run_steps(2);
    assert_ne!(
        h.output().platform_output.cursor_icon,
        egui::CursorIcon::Text,
        "the I-beam followed the pointer onto the list"
    );
}

#[test]
fn the_wheel_scrolls_the_list_and_leaves_the_selection_alone() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(300), 300, now);

    let mut h = harness(s);
    let before = h.state().state.selected_row();
    assert!(before.is_some(), "the fixture selected nothing");

    // egui routes wheel events to the scroller under the pointer, so hover
    // over the list first.
    h.hover_at(egui::pos2(
        HARNESS_MARGIN + theme::PANEL_W / 2.0,
        HARNESS_MARGIN + theme::HEADER_H + theme::CONTENT_H / 2.0,
    ));
    h.run_steps(1);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -400.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(4);

    let screen = on_screen(&h);
    assert!(
        !screen.contains("AB-C-0704-00.pdf"),
        "the wheel moved nothing:\n{screen}"
    );
    assert_eq!(
        h.state().state.selected_row(),
        before,
        "the wheel moved the selection"
    );
}

// --- what the panel does not do --------------------------------------------

/// A keystroke keeps the previous results on screen until new ones arrive.
#[test]
fn a_keystroke_does_not_blank_the_result_list() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-070", many(6), 6, now);

    let mut h = harness(s);
    let before = on_screen(&h);
    assert!(before.contains("AB-C-0704-00.pdf"), "{before}");

    // One more character, then a frame drawn before any search result.
    h.state_mut().state.update(
        AppEvent::Key(KeyEvent::new(Key::Char('4'), Mods::NONE)),
        now,
    );
    h.run_steps(1);

    let during = on_screen(&h);
    assert!(
        during.contains("AB-C-0704-00.pdf"),
        "the list went away while the next one was in flight:\n{during}"
    );
    assert!(
        !during.contains("Searching\u{2026}"),
        "and the empty state took its place:\n{during}"
    );
}

/// A pasted string longer than the field stays within the panel's bounds.
#[test]
fn a_very_long_search_stays_inside_the_panel() {
    let long = "AB-C-0704-".repeat(30);
    let (mut s, now) = state();
    s.update(AppEvent::Paste(long.clone()), now);
    assert_eq!(s.input.caret(), long.len(), "the caret is at the end of it");

    let h = harness(s);

    // The field's accessible value is the full text regardless of clipping,
    // so check node bounds instead.
    let panel = egui::Rect::from_min_size(
        egui::pos2(HARNESS_MARGIN, HARNESS_MARGIN),
        egui::vec2(theme::PANEL_W, theme::PANEL_H),
    );
    let root = h.root();
    for node in root.children_recursive() {
        let bounds = node.accesskit_node().bounding_box();
        let Some(bounds) = bounds else { continue };
        assert!(
            bounds.x1 <= panel.max.x as f64 + 1.0,
            "something reaches {:.0}pt, past the panel's {:.0}pt edge",
            bounds.x1,
            panel.max.x
        );
    }
    assert!(on_screen(&h).contains(&long), "{}", on_screen(&h));
}

/// Every drawn result row starts inside the content band.
#[test]
fn a_full_list_of_results_does_not_run_into_the_footer() {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(300), 300, now);

    let h = harness(s);
    // The content band in harness coordinates. Only row tops are checked: the
    // row straddling the bottom edge is legitimately clipped by the scroller.
    let content_top = HARNESS_MARGIN + theme::HEADER_H + theme::DIVIDER;
    let content_bottom = content_top + theme::CONTENT_H;

    let root = h.root();
    let mut seen = 0;
    for node in root.children_recursive() {
        let Some(bounds) = node.accesskit_node().bounding_box() else {
            continue;
        };
        let label = node.accesskit_node().label().unwrap_or_default();
        if !label.contains(".pdf, in ") {
            continue;
        }
        seen += 1;
        assert!(
            bounds.y0 >= content_top as f64 - 1.0 && bounds.y0 < content_bottom as f64,
            "{label} starts at {:.0}pt, outside the band {content_top:.0}-{content_bottom:.0}pt",
            bounds.y0
        );
    }
    // Guards against the loop passing vacuously. Rows past the clip are
    // culled, so only a lower bound is asserted.
    assert!(
        seen >= files::config::VISIBLE_ROWS,
        "only {seen} rows were drawn, of a screenful of {}",
        files::config::VISIBLE_ROWS
    );
}

/// Returns each result row's label and bounds from the accessibility tree.
fn result_boxes(h: &Harness<'_, Panel>) -> Vec<(String, egui::Rect)> {
    h.root()
        .children_recursive()
        .filter_map(|node| {
            let label = node.accesskit_node().label().unwrap_or_default();
            let b = node.accesskit_node().bounding_box()?;
            label.contains(".pdf, in ").then(|| {
                let rect = egui::Rect::from_min_max(
                    egui::pos2(b.x0 as f32, b.y0 as f32),
                    egui::pos2(b.x1 as f32, b.y1 as f32),
                );
                (label, rect)
            })
        })
        .collect()
}

/// Rows start at one of two left edges, fill left to right, and do not
/// overlap the neighbouring column.
#[test]
fn two_columns_lay_the_results_out_side_by_side() {
    let (mut s, now) = state();
    s.settings.columns = 2;
    with_results(&mut s, "AB-C-0704", many(20), 20, now);
    let h = harness(s);
    let boxes = result_boxes(&h);
    assert!(boxes.len() >= 4, "only {} rows were drawn", boxes.len());

    let mut lefts: Vec<f32> = boxes.iter().map(|(_, r)| r.left().round()).collect();
    lefts.sort_by(f32::total_cmp);
    lefts.dedup();
    assert_eq!(lefts.len(), 2, "rows start at {lefts:?}");

    let at = |name: &str| {
        boxes
            .iter()
            .find(|(label, _)| label.starts_with(name))
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{name} is not on screen"))
    };
    let first = at("AB-C-0704-00.pdf");
    let second = at("AB-C-0704-01.pdf");
    let third = at("AB-C-0704-02.pdf");
    assert_eq!(
        first.top(),
        second.top(),
        "the second is not beside the first"
    );
    assert!(
        second.left() > first.right(),
        "the first reaches into the second column"
    );
    assert_eq!(
        third.left(),
        first.left(),
        "the third did not start the next line"
    );
    assert!(
        third.top() > first.bottom(),
        "the next line overlaps the first"
    );
}

#[test]
fn three_columns_show_more_results_than_one() {
    let drawn = |columns| {
        let (mut s, now) = state();
        s.settings.columns = columns;
        with_results(&mut s, "AB-C-0704", many(60), 60, now);
        result_boxes(&harness(s)).len()
    };
    let (one, three) = (drawn(1), drawn(3));
    assert!(
        three >= one * 3,
        "one column drew {one} and three drew {three}"
    );
}

// --- the pictures ----------------------------------------------------------

/// Defines a pixel snapshot test of the panel built from the given state.
///
/// One snapshot per test: `egui_kittest` collects results per harness and
/// cannot merge two harnesses in one test.
macro_rules! snapshot {
    ($name:ident, $build:expr) => {
        #[test]
        #[cfg_attr(not(feature = "ui-snapshots"), ignore = "needs a GPU adapter")]
        fn $name() {
            let build: fn() -> AppState = $build;
            harness(build()).snapshot(stringify!($name));
        }
    };
}

snapshot!(looks_right_with_results, || {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(6), 47, now);
    s
});

// The first screen: a search box and no footer. Uses `quiet_state` because a
// fresh state shows the missing-index notice.
snapshot!(looks_right_when_quiet, || {
    let (s, _) = quiet_state();
    s
});

snapshot!(looks_right_with_a_toast, || toasted(Instant::now()));

snapshot!(looks_right_showing_recent_searches, || {
    let (mut s, now) = state();
    s.seed_history(vec!["AB-C-0704".into(), "P12345-001".into()]);
    // Recent searches appear only while browsing, so press Up.
    s.update(AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)), now);
    assert!(s.history.is_browsing(), "the fixture is not browsing");
    s
});

// The drive picker, including its hover and text-selection colours.
snapshot!(looks_right_picking_a_drive, || {
    let (mut s, now) = state();
    s.update(AppEvent::Key(KeyEvent::new(Key::F(5), Mods::NONE)), now);
    assert!(s.picking_share, "the fixture did not open the picker");
    s
});

// More results than fit on screen.
snapshot!(looks_right_with_more_than_it_can_show, || {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(300), 300, now);
    s
});

snapshot!(looks_right_in_two_columns, || {
    let (mut s, now) = state();
    s.settings.columns = 2;
    with_results(&mut s, "AB-C-0704", many(300), 300, now);
    s.update(AppEvent::Key(KeyEvent::new(Key::Tab, Mods::NONE)), now);
    s
});

snapshot!(looks_right_in_three_columns, || {
    let (mut s, now) = state();
    s.settings.columns = 3;
    with_results(&mut s, "AB-C-0704", many(300), 300, now);
    s
});

// An empty search box with configured shortcuts listed below it.
snapshot!(looks_right_showing_the_shortcuts, || {
    let (mut s, _) = quiet_state();
    s.settings.aliases = Arc::new(files::alias::Aliases::new(vec![
        files::alias::Alias {
            name: "pw".into(),
            code: "AB-C-0704".into(),
            note: Some("the pump house".into()),
        },
        files::alias::Alias {
            name: "gd".into(),
            code: "XY-Z-1234".into(),
            note: None,
        },
    ]));
    s
});

// The actions menu, which lists every action with its shortcut.
snapshot!(looks_right_with_the_actions_menu_open, || {
    let (mut s, now) = state();
    with_results(&mut s, "AB-C-0704", many(6), 47, now);
    s.actions_open = true;
    s
});

// The detailed row layout: the folder under the name and a larger icon.
snapshot!(looks_right_with_detailed_rows, || {
    let (mut s, now) = state();
    s.settings.result_layout = ResultLayout::Detailed;
    with_results(&mut s, "AB-C-0704", many(300), 300, now);
    s
});
