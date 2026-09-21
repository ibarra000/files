//! The panel, drawn, and then read back.
//!
//! The safety net the rewrite dropped. The terminal build rendered into a
//! `TestBackend` buffer and asserted on the characters *and the styles* in it,
//! so a selection highlight that quietly stopped being painted failed a test.
//! When the window replaced it, `gui::panel::show` - the one function that
//! paints the whole panel - arrived with no way to check anything at all, and
//! stayed that way. The toast band went missing through exactly that gap: the
//! messages were still raised, still expired on a timer, and were drawn
//! nowhere, and nothing anywhere could tell.
//!
//! [`egui_kittest`] closes it. It builds a real [`egui::Context`], runs the
//! real `show`, and hands back the accessibility tree - which is not a
//! rendering but is derived from one, and is the same tree Narrator reads. So
//! "is this on screen" becomes a question with an answer, without a GPU.
//!
//! The pictures are behind `--features ui-snapshots`, because rendering them
//! needs an adapter to render *through* and a build machine may not have one.
//!
//! # What this drives, and what it does not
//!
//! `gui::panel::show` and `gui::frame::Frame`, over an `AppState` built by
//! hand. Not `gui::Shell`, which owns threads, a tray icon and a window: the
//! panel is a pure function of state, a theme and a `Content`, and that is
//! exactly the seam worth testing.

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

/// The outer margin `egui_kittest` frames its `Ui` in. Not a property of the
/// panel; the window is grown by it so that it is not one.
const HARNESS_MARGIN: f32 = 8.0;

// --- fixtures --------------------------------------------------------------

fn hit(name: &str) -> Hit {
    Hit {
        path: Arc::from(format!("R:\\11d\\11-D-0704\\{name}").as_str()),
        name: Arc::from(name),
        match_pos: 0,
        index: 0,
    }
}

/// Puts `hits` on screen for `code`, the way the matcher would.
///
/// `matched` is what the index *found*, which is not the same as what came
/// back: the search is capped, and the panel has room for eight rows of
/// whatever survived that. Keeping the two apart is the whole point of the
/// count in the footer.
fn with_results(state: &mut AppState, code: &str, hits: Vec<Hit>, matched: u32, now: Instant) {
    for c in code.chars() {
        state.update(AppEvent::Key(KeyEvent::new(Key::Char(c), Mods::NONE)), now);
    }
    let epoch = state.query_epoch();
    state.update(
        AppEvent::Search(SearchMsg {
            epoch,
            query: Query::contains(code),
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
        .map(|i| hit(&format!("11-D-0704-{i:02}.pdf")))
        .collect()
}

/// The panel and everything it is drawn from, carried through the harness.
struct Panel {
    state: AppState,
    frame: Frame,
    theme: Theme,
    now: Instant,
    wall: SystemTime,
    fonts_ready: bool,
}

impl Panel {
    fn new(state: AppState, now: Instant) -> Self {
        let mut frame = Frame::new();
        frame.motion.summon();
        Self {
            state,
            frame,
            // Pinned rather than followed from the system, so a machine in
            // dark mode and a machine in light mode agree about the pictures.
            theme: Theme::light(),
            now,
            // A fixed wall clock, because the status line renders an age from
            // it and "updated 4s ago" is not a stable snapshot.
            wall: SystemTime::UNIX_EPOCH,
            fonts_ready: false,
        }
    }
}

/// Builds a harness around the real panel, settled and at rest.
///
/// `Backdrop` is `None` - the path a Windows without acrylic takes, and the
/// one where the panel paints its own surface, which is what there is to look
/// at.
fn harness(state: AppState) -> Harness<'static, Panel> {
    let now = Instant::now();
    let mut harness = Harness::builder()
        // The harness frames whatever it is given in an eight-point outer
        // margin. `eframe::App::ui` hands over a `Ui` with no margin at all,
        // so the window is grown to match and the panel gets exactly the
        // rectangle it gets in the real program.
        .with_size(egui::vec2(
            theme::PANEL_W + HARNESS_MARGIN * 2.0,
            theme::PANEL_H + HARNESS_MARGIN * 2.0,
        ))
        // Points, not pixels: the panel is laid out in points and a snapshot
        // taken at whatever the machine's scaling happens to be is a snapshot
        // of the machine.
        .with_pixels_per_point(1.0)
        .with_step_dt(DT)
        .build_ui_state(
            |ui, panel: &mut Panel| {
                // `theme::font` asks for `FontFamily::Name("segoe")` and egui
                // panics on a family it was never given, so the families have
                // to exist before anything is laid out. Bound to egui's own
                // font rather than to whatever Segoe UI this machine has, so
                // the pictures are of the panel and not of the machine.
                //
                // Here rather than before the first frame because new
                // definitions are picked up at the *start* of a frame, and the
                // builder runs one on the way out. So the first frame installs
                // them and draws nothing, and every frame after is the panel.
                if !panel.fonts_ready {
                    files::gui::fonts::install_bundled(ui.ctx());
                    // And the style, which these pictures went without for
                    // as long as they have existed. Nothing the panel paints
                    // by hand reads it - but the scroll-bar down the results
                    // is an egui widget, and without this it was drawn from
                    // egui's defaults: floating, over the content, twice the
                    // width. So the snapshots were of a scrollbar the program
                    // does not ship.
                    files::gui::theme::apply_style(ui.ctx(), &panel.theme);
                    panel.fonts_ready = true;
                    return;
                }
                let content = panel.frame.advance(&panel.state, DT);
                files::gui::panel::show(
                    ui,
                    &panel.state,
                    &panel.theme,
                    content,
                    None,
                    panel.now,
                    panel.wall,
                );
            },
            Panel::new(state, now),
        );

    // Past the entrance, so nothing here is a test of a half-arrived panel.
    harness.run_steps(40);
    harness
}

fn state() -> (AppState, Instant) {
    let now = Instant::now();
    (AppState::new(Settings::default(), now), now)
}

/// A state with nothing at all to say: nothing typed, and an index that has
/// reported in healthy.
///
/// `state()` is not that. A fresh one has no index yet, which is a standing
/// notice - "No file list yet" - and that notice is a real screen somebody
/// sees. This fixture is for the other half.
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
    // Read off the line the footer would draw, which is the only public
    // answer to "has this panel anything to say" now that `is_quiet` has
    // gone with `Content::Quiet`.
    let said = files::view::status::render(&s, now, SystemTime::UNIX_EPOCH).text;
    assert!(
        said.is_empty(),
        "the fixture has something to say - {said:?} - so it proves nothing"
    );
    (s, now)
}

/// Everything the panel put on screen, as one string.
///
/// The accessibility tree rather than the pixels: it is derived from the same
/// layout pass, it is what a screen reader is handed, and it needs no GPU.
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

/// The thirteen messages that were raised, given an expiry, kept on the frame
/// clock so the loop would wake to retire them - and drawn nowhere.
/// Raises a toast the way the program does: Enter on a code with nothing under
/// it. Driven through a real keystroke rather than by reaching into the state,
/// so the path a user takes is the path under test.
fn toasted(now: Instant) -> AppState {
    let (mut s, _) = state();
    with_results(&mut s, "11-D-0704", Vec::new(), 0, now);
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

/// And it goes away on its own, rather than sitting on the status line for the
/// rest of the session.
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

/// Six rows on screen, three hundred results, and every one of them
/// reachable by scrolling. The count is what says there are more than fit.
///
/// This used to read "1-6 of 300", because the panel drew a fixed window over
/// the list and the other 294 could not be got at. The band scrolls now, so
/// the scrollbar answers where in them you are and the footer answers how
/// many there are.
#[test]
fn the_footer_says_how_many_the_code_found() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(300), 300, now);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        screen.contains("300"),
        "nothing said how many there were:\n{screen}"
    );
}

/// And says nothing at all when there is nothing to say, rather than a zero.
#[test]
fn the_footer_counts_nothing_when_there_is_nothing_to_count() {
    let (s, _) = state();
    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        !screen.contains("Results"),
        "a caption over a list that is not there:\n{screen}"
    );
    // And offers no way to open anything, because there is nothing to open.
    // The button that names the default action is what carries that now: it
    // is simply not drawn.
    assert!(
        !screen.contains("\u{b7} Enter"),
        "the footer offered to open nothing:\n{screen}"
    );
    // The gear and the menu are still there. They are about the program
    // rather than about the list, so they do not come and go with one - and
    // between them they are the only thing left advertising F5 and Ctrl+,.
    assert!(screen.contains("Settings"), "{screen}");
}

/// F2 changes what Enter does, and the footer says so in words.
///
/// It used to be a chip reading `Viewer: PDF`, at `Priority::Normal` - so at
/// the shipped width it was the first thing the hint bar dropped, and the one
/// key whose whole job is to change a mode gave no sign of which mode it was
/// in. It is the label on the button Enter runs now, which cannot be dropped
/// because it is the button.
#[test]
fn the_footer_says_what_enter_will_do() {
    for (viewer, label) in [
        (ViewerKind::Auto, "Open"),
        (ViewerKind::Pdf, "Open as one document"),
        (ViewerKind::Avwin, "Open with avwin"),
    ] {
        let (mut s, now) = state();
        with_results(&mut s, "11-D-0704", many(3), 3, now);
        s.viewer = viewer;
        let h = harness(s);
        let screen = on_screen(&h);
        assert!(
            screen.contains(&format!("{label} \u{b7} Enter")),
            "{viewer:?} is not named on the footer:\n{screen}"
        );
    }
}

/// And Ctrl+K opens the rest of them, which is the whole reason the footer
/// can be two buttons rather than a row of chips.
#[test]
fn the_actions_menu_lists_everything_else() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(3), 3, now);
    s.actions_open = true;

    let h = harness(s);
    let screen = on_screen(&h);
    for offered in [
        "Open \u{b7} Enter",
        "Open as one document \u{b7} Ctrl D",
        "Open with avwin \u{b7} Ctrl E",
        "Show it in Explorer \u{b7} Ctrl O",
        "Copy the path \u{b7} Ctrl C",
        "Copy the name",
        "Read a drive again \u{b7} F5",
        "Settings \u{b7} Ctrl ,",
    ] {
        assert!(
            screen.contains(offered),
            "the menu does not offer {offered:?}:\n{screen}"
        );
    }
}

/// With nothing found it offers nothing to do with a file, and still offers
/// the two that are about the program.
#[test]
fn the_actions_menu_with_nothing_selected_offers_no_file() {
    let (mut s, _) = state();
    s.actions_open = true;

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(!screen.contains("Copy the path"), "{screen}");
    assert!(!screen.contains("Show it in Explorer"), "{screen}");
    assert!(screen.contains("Read a drive again"), "{screen}");
}

/// Choosing the viewer that is not installed used to fail silently, at the
/// moment somebody wanted a drawing. The probe was taken at startup and
/// discarded.
#[test]
fn a_missing_avwin_is_reported_before_it_is_needed() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(3), 3, now);
    s.viewer = ViewerKind::Avwin;
    s.set_avwin_missing(true);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(screen.contains("avwin.exe"), "{screen}");
    assert!(screen.contains("F2"), "and says how to fix it:\n{screen}");
}

/// Browsing a long list of remembered codes without knowing where you are in
/// it is what the terminal build's " History (2 of 3) " title existed to
/// prevent.
#[test]
fn browsing_the_recent_codes_says_where_you_are_in_them() {
    let (mut s, now) = state();
    s.seed_history(vec![
        "11-D-0704".into(),
        "P12345-001".into(),
        "22-A-1".into(),
    ]);
    // Twice: the first Up steps onto the newest code, the second onto the one
    // behind it. A Down from the newest leaves browsing altogether.
    s.update(AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)), now);
    s.update(AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)), now);
    assert!(s.history.is_browsing(), "the fixture is not browsing");

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(screen.contains("Codes you used before"), "{screen}");
    assert!(
        screen.contains("of 3"),
        "no position in the list:\n{screen}"
    );
}

/// The first screen anybody sees is a search box and nothing else.
///
/// It used to teach three things - what to type, what a code looks like, and
/// that there was more to find - across five lines of body text and four
/// chips. That is a page of instructions in front of somebody who summoned a
/// search box to search, every time they summon it.
#[test]
fn the_first_screen_is_a_search_box_and_nothing_else() {
    let (s, _) = quiet_state();
    let h = harness(s);
    let screen = on_screen(&h);

    assert!(screen.contains("Search"), "no placeholder:\n{screen}");
    for gone in [
        "Type a job code",
        "11-D-0704",
        "Recent codes",
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

/// A standing notice brings the footer back, because that is what the footer
/// is for. The panel is quiet when there is nothing to say - not when nothing
/// may be said.
#[test]
fn something_worth_saying_brings_the_footer_back() {
    // No index yet, which is the one standing notice a fresh start has.
    let (s, _) = state();
    let h = harness(s);
    let screen = on_screen(&h);

    assert!(
        screen.contains("No file list yet"),
        "a notice was swallowed by the quiet panel:\n{screen}"
    );
}

/// The taller rows show fewer of themselves, which is the whole of what the
/// setting does to the list.
///
/// Counted off the accessibility tree rather than off the arithmetic: the
/// rows past the clip rectangle are culled before they are laid out, so how
/// many are in the tree *is* how many are on screen.
#[test]
fn detailed_rows_fit_fewer_to_a_screen() {
    let drawn = |layout| {
        let (mut s, now) = state();
        s.settings.result_layout = layout;
        with_results(&mut s, "11-D-0704", many(300), 300, now);
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

/// Whichever layout is on, a row says the whole truth about itself to a
/// reader. Compact does not draw the folder, and a screen reader that was
/// given only what was drawn would be worse off than the tooltip.
#[test]
fn a_compact_row_still_names_the_folder_it_is_in() {
    let (mut s, now) = state();
    s.settings.result_layout = ResultLayout::Compact;
    with_results(&mut s, "11-D-0704", many(3), 3, now);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        screen.contains(r"11-D-0704-00.pdf, in R:\11d\11-D-0704"),
        "a compact row told a reader less than it knows:\n{screen}"
    );
}

/// The list is a group with a caption over it, which is Ueli's shape.
#[test]
fn the_results_are_captioned() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(3), 3, now);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(screen.contains("Results"), "no caption:\n{screen}");
}

/// A row the arrows walked to is brought into view, by as little as it takes.
///
/// The whole reason the content band is a scroller. The panel used to draw a
/// twelve-row window that the state machine moved; the rows past it were not
/// laid out at all, and a selection beyond the window left the highlight
/// frozen on the last row while `Enter` opened a file that was not on screen.
///
/// Read off the accessibility tree rather than off a scroll offset, because
/// a row outside the clip rectangle is culled before it is given a label -
/// so its presence in the tree *is* the claim that it is on screen.
#[test]
fn walking_down_a_long_list_carries_the_view_with_it() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(300), 300, now);

    let mut h = harness(s);
    assert!(
        !on_screen(&h).contains("11-D-0704-50.pdf"),
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
        screen.contains("11-D-0704-50.pdf"),
        "the selection walked off the bottom and the view stayed put:\n{screen}"
    );
    // And by as little as it takes: `block: "nearest"` puts the row at the
    // foot of the band, so the ones just above it are still there. A scroller
    // that centred the selection would have thrown them away.
    assert!(
        screen.contains("11-D-0704-49.pdf"),
        "it scrolled further than it had to:\n{screen}"
    );
}

/// And the wheel moves the view without moving the selection, which is the
/// reason it was unbound in the first place: it used to walk the cursor
/// through somebody's results whenever a hand rested on the mouse.
#[test]
fn the_wheel_scrolls_the_list_and_leaves_the_selection_alone() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(300), 300, now);

    let mut h = harness(s);
    let before = h.state().state.selected_row();
    assert!(before.is_some(), "the fixture selected nothing");

    // The pointer over the middle of the list, then a long way down it.
    // egui hands a wheel event to whichever scroller is under the pointer, so
    // the hover is not decoration.
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
        !screen.contains("11-D-0704-00.pdf"),
        "the wheel moved nothing:\n{screen}"
    );
    assert_eq!(
        h.state().state.selected_row(),
        before,
        "the wheel moved the selection"
    );
}

// --- what the panel does not do --------------------------------------------

/// The jitter budget, checked through a real layout pass rather than through
/// `measure`. A keystroke must not take the results off the screen.
#[test]
fn a_keystroke_does_not_blank_the_result_list() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-070", many(6), 6, now);

    let mut h = harness(s);
    let before = on_screen(&h);
    assert!(before.contains("11-D-0704-00.pdf"), "{before}");

    // One more character, and the frame that is drawn before the matcher can
    // possibly have answered.
    h.state_mut().state.update(
        AppEvent::Key(KeyEvent::new(Key::Char('4'), Mods::NONE)),
        now,
    );
    h.run_steps(1);

    let during = on_screen(&h);
    assert!(
        during.contains("11-D-0704-00.pdf"),
        "the list went away while the next one was in flight:\n{during}"
    );
    assert!(
        !during.contains("Looking for it\u{2026}"),
        "and the empty state took its place:\n{during}"
    );
}

/// A pasted string longer than the field must not run out of the panel, taking
/// the caret with it. The terminal build had a whole module for this and it has
/// no successor.
#[test]
fn a_very_long_code_stays_inside_the_panel() {
    let long = "11-D-0704-".repeat(30);
    let (mut s, now) = state();
    s.update(AppEvent::Paste(long.clone()), now);
    assert_eq!(s.input.caret(), long.len(), "the caret is at the end of it");

    let h = harness(s);

    // The field announces the whole code however little of it is on screen -
    // a reader has no column width to be truncated to - so the tree is not
    // where the overflow would show. What can be checked is that every node
    // the panel laid out is inside the panel, which is what stopped being
    // true when a pasted code ran off the edge and took the caret with it.
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

/// The bands never move, so nothing that belongs in one may be drawn in
/// another. A row overlapping the footer is a row with the status line written
/// through it.
#[test]
fn a_full_list_of_results_does_not_run_into_the_footer() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(300), 300, now);

    let h = harness(s);
    // The band the rows are given, in the harness's coordinates. Arithmetic
    // off the fixed panel: the window is always four hundred points tall, so
    // the footer is always in the same place and a row that *starts* below
    // the band is a row the scroller never clipped.
    //
    // Where a row *ends* is deliberately not asserted. The list is a scroller
    // now, and the row straddling its bottom edge is cut off by the clip
    // rectangle rather than by arithmetic - which is what a scroller is. The
    // failure this guards against is the one that actually happened: rows
    // picking up three points of spacing each and a full list standing
    // twenty-four points taller than the band it was given.
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
    // Without this the loop can pass by matching nothing at all, which is how
    // the first version of this test passed while the rows really did overlap.
    // A screenful, at least: the rows past the clip rectangle are culled
    // before they are laid out, so the exact number is the scroller's
    // business rather than this test's.
    assert!(
        seen >= files::config::VISIBLE_ROWS,
        "only {seen} rows were drawn, of a screenful of {}",
        files::config::VISIBLE_ROWS
    );
}

// --- the pictures ----------------------------------------------------------

/// Everything above says what is on the panel. This says what it looks like -
/// which is the half that catches a rule drawn in the wrong place, a highlight
/// that stopped being painted, and a column that overlaps its neighbour.
/// One test per picture. `egui_kittest` collects the results of a run so it
/// can update them all at once, and two harnesses in one test have two
/// collections and no way to merge them - so a snapshot per test is what the
/// library is built for, and it also means a failure names itself.
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
    with_results(&mut s, "11-D-0704", many(6), 47, now);
    s
});

// The first screen, and the whole of it: a search box, nothing under it, and
// no footer. Built from `quiet_state` rather than `state` because a fresh one
// has no index yet and therefore has something to say - which is a real screen
// somebody sees, and a different one from this.
snapshot!(looks_right_when_quiet, || {
    let (s, _) = quiet_state();
    s
});

snapshot!(looks_right_with_a_toast, || toasted(Instant::now()));

snapshot!(looks_right_showing_recent_codes, || {
    let (mut s, now) = state();
    s.seed_history(vec!["11-D-0704".into(), "P12345-001".into()]);
    // Pressed, not merely seeded. The list used to be what an empty field
    // showed, so this fixture photographed it without touching a key - which
    // now photographs the first-run block and duplicates the picture above.
    s.update(AppEvent::Key(KeyEvent::new(Key::Up, Mods::NONE)), now);
    assert!(s.history.is_browsing(), "the fixture is not browsing");
    s
});

// The drive picker: the one body that had never been photographed, along with
// the hover and text-selection colours it is the only place to see.
snapshot!(looks_right_picking_a_drive, || {
    let (mut s, now) = state();
    s.update(AppEvent::Key(KeyEvent::new(Key::F(5), Mods::NONE)), now);
    assert!(s.picking_share, "the fixture did not open the picker");
    s
});

// More results than the panel has room for, which is the state the count in
// the footer exists for.
snapshot!(looks_right_with_more_than_it_can_show, || {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(300), 300, now);
    s
});

// What an empty box shows when there is anything to show on it: Ueli's
// favourites, which here are the shortcuts somebody configured.
snapshot!(looks_right_showing_the_shortcuts, || {
    let (mut s, _) = quiet_state();
    s.settings.aliases = Arc::new(files::alias::Aliases::new(vec![
        files::alias::Alias {
            name: "pw".into(),
            code: "11-D-0704".into(),
            note: Some("the pump house".into()),
        },
        files::alias::Alias {
            name: "gd".into(),
            code: "22-A-1234".into(),
            note: None,
        },
    ]));
    s
});

// The one key that replaced the whole hint bar, and the only place the
// program teaches its own chords now.
snapshot!(looks_right_with_the_actions_menu_open, || {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(6), 47, now);
    s.actions_open = true;
    s
});

// The other row layout: the folder under the name, a larger mark, and four of
// them where there were six. The one setting on the Appearance page whose
// effect cannot be described in words as well as it can be shown.
snapshot!(looks_right_with_detailed_rows, || {
    let (mut s, now) = state();
    s.settings.result_layout = ResultLayout::Detailed;
    with_results(&mut s, "11-D-0704", many(300), 300, now);
    s
});
