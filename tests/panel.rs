//! The panel, drawn, and then read back.
//!
//! The safety net the rewrite dropped. The terminal build rendered into a
//! `TestBackend` buffer and asserted on the characters *and the styles* in it,
//! so a selection highlight that quietly stopped being painted failed a test.
//! When the window replaced it, `gui::overlay::show` - the one function that
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
//! `gui::overlay::show` and `gui::frame::Frame`, over an `AppState` built by
//! hand. Not `gui::Shell`, which owns threads, a tray icon and a window: the
//! panel is a pure function of state, a theme and a `Visual`, and that is
//! exactly the seam worth testing.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;

use files::app::event::{AppEvent, SearchMsg};
use files::app::key::{Key, KeyEvent, Mods};
use files::app::state::AppState;
use files::config::{Settings, VISIBLE_ROWS, ViewerKind};
use files::gui::frame::Frame;
use files::gui::theme::{self, Theme};
use files::search::matcher::{Hit, SearchOutcome};

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
            query: code.to_string(),
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
            theme::PANEL_MAX_H + HARNESS_MARGIN * 2.0,
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
                    panel.fonts_ready = true;
                    return;
                }
                let visual = panel.frame.advance(&panel.state, DT);
                panel.frame.resize(&visual, false);
                files::gui::overlay::show(
                    ui,
                    &panel.state,
                    &panel.theme,
                    &visual,
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
        screen.contains("nothing to open"),
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
        !screen.contains("nothing to open"),
        "the toast outlived its lifetime:\n{screen}"
    );
}

/// Twelve rows, three hundred results. The rest used to be unreachable and
/// unmentioned; they are reachable now, and this says where in them you are.
#[test]
fn the_footer_says_which_of_the_results_are_on_screen() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(300), 300, now);

    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        screen.contains(&format!("1-{VISIBLE_ROWS} of 300")),
        "nothing said where in the list this is:\n{screen}"
    );
}

/// And says nothing at all when there is nothing to say, rather than a zero.
#[test]
fn the_footer_counts_nothing_when_there_is_nothing_to_count() {
    let (s, _) = state();
    let h = harness(s);
    let screen = on_screen(&h);
    assert!(
        !screen.contains(" of "),
        "a count of nothing:
{screen}"
    );
    // The viewer is still there: it is a fact about the program rather than
    // about the list, so it does not come and go with one.
    assert!(screen.contains("viewer: pdf"), "{screen}");
}

/// F2 changes what Enter does. With the confirming toast drawn nowhere and no
/// label anywhere on the panel, pressing it produced no visible effect at all.
#[test]
fn the_footer_names_the_viewer_enter_will_use() {
    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(3), 3, now);

    let h = harness(s);
    assert!(on_screen(&h).contains("viewer: pdf"), "{}", on_screen(&h));

    let (mut s, now) = state();
    with_results(&mut s, "11-D-0704", many(3), 3, now);
    s.viewer = ViewerKind::Avwin;
    let h = harness(s);
    assert!(on_screen(&h).contains("viewer: avwin"), "{}", on_screen(&h));
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

/// The first screen anybody sees has to teach three things: what to type, what
/// one looks like, and that there is more to find.
#[test]
fn the_first_screen_teaches_what_to_type() {
    let (s, _) = state();
    let h = harness(s);
    let screen = on_screen(&h);
    assert!(screen.contains("Type a job code"), "{screen}");
    assert!(screen.contains("11-D-0704"), "no example:\n{screen}");
    assert!(
        screen.contains("F1"),
        "nothing points at the keys:\n{screen}"
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
        !during.contains("Searching\u{2026}"),
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
        egui::vec2(theme::PANEL_W, theme::PANEL_MAX_H),
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
    // The band the rows are given, in the harness's coordinates. Taken from
    // the same `measure` the animator is driven by, so this asserts that what
    // is *drawn* agrees with what was *measured* - which is exactly what
    // stopped being true when the rows picked up three points of spacing each
    // and a full list stood twenty-four points taller than its band.
    let measured = files::gui::overlay::measure(&h.state().state);
    let footer_top = HARNESS_MARGIN + measured.height - theme::PAD_Y - theme::FOOTER_H;

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
            bounds.y1 <= footer_top as f64 + 1.0,
            "{label} reaches {:.0}pt, past the footer at {footer_top:.0}pt",
            bounds.y1
        );
    }
    // Without this the loop can pass by matching nothing at all, which is how
    // the first version of this test passed while the rows really did overlap.
    assert_eq!(seen, theme::MAX_ROWS, "the rows were not found");
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

snapshot!(looks_right_when_empty, || {
    let (s, _) = state();
    s
});

snapshot!(looks_right_with_a_toast, || toasted(Instant::now()));

snapshot!(looks_right_showing_recent_codes, || {
    let (mut s, _) = state();
    s.seed_history(vec!["11-D-0704".into(), "P12345-001".into()]);
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
