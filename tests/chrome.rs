//! What the drag handle rests on, which is not this program but the toolkit.
//!
//! `gui::mod::Shell::ui` makes the panel draggable by interacting with
//! `ui.max_rect()` *before* `gui::overlay::show` runs, and relies on the field,
//! the rows and the chips - registered afterwards, over the same pixels -
//! taking the press instead. That is the whole definition of "empty chrome":
//! nothing computes where the gaps are, the gaps are simply whatever nothing
//! else claimed.
//!
//! So the feature is one line of policy resting on one claim about egui, and
//! the claim is the part worth pinning. If it ever stops holding, clicking a
//! result stops selecting it and starts dragging the window instead - and
//! nothing else in the suite would notice, because `Shell::ui` owns a window,
//! a tray icon and three threads, and no test drives it.
//!
//! Its own file rather than a module inside `tests/panel.rs`, because it shares
//! none of that file's fixtures: there is no `AppState` here and no panel, only
//! two overlapping rectangles standing in for the two layers.

use eframe::egui::{self, Pos2, Rect, Sense, pos2, vec2};
use egui_kittest::Harness;

/// The panel, and a result row somewhere in the middle of it.
const PANEL: Rect = Rect::from_min_max(Pos2::ZERO, pos2(200.0, 200.0));
const ROW: Rect = Rect::from_min_max(pos2(20.0, 80.0), pos2(180.0, 120.0));

/// Somewhere inside the panel that the row does not cover: the margin above it.
const GAP: Pos2 = pos2(100.0, 20.0);

/// Which of the two layers took the pointer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Claimed {
    chrome: bool,
    row: bool,
}

/// Two overlapping interactions, registered in the order `Shell::ui` uses when
/// no modifier is held: the handle underneath, everything else on top.
fn harness() -> Harness<'static, Claimed> {
    let mut harness = Harness::builder()
        .with_size(vec2(200.0, 200.0))
        .with_pixels_per_point(1.0)
        .build_ui_state(
            |ui, claimed: &mut Claimed| {
                let chrome = ui.interact(
                    PANEL,
                    egui::Id::new("files-chrome"),
                    Sense::click_and_drag(),
                );
                let row = ui.interact(ROW, egui::Id::new("a-row"), Sense::click());

                // Latched rather than assigned. A click is reported on the
                // frame the button comes back up and a drag on the frames in
                // between, so a plain assignment would be cleared by whichever
                // frame the harness happened to stop on.
                claimed.chrome |= chrome.clicked() || chrome.dragged();
                claimed.row |= row.clicked();
            },
            Claimed::default(),
        );
    harness.run();
    harness
}

/// A click on the row is the row's, even though the handle underneath it covers
/// the same pixel and asked for clicks too.
#[test]
fn a_widget_registered_later_takes_the_press_from_the_one_beneath_it() {
    let mut harness = harness();
    let at = ROW.center();

    harness.hover_at(at);
    harness.step();
    harness.drag_at(at);
    harness.step();
    harness.drop_at(at);
    harness.step();

    let claimed = *harness.state();
    assert!(
        claimed.row,
        "the row did not get its own click: {claimed:?}"
    );
    assert!(
        !claimed.chrome,
        "the drag handle swallowed a click meant for a result: {claimed:?}"
    );
}

/// And a press in the margin, which no row asked for, reaches the handle -
/// otherwise there would be nothing to drag the panel by at all.
#[test]
fn a_press_on_the_gaps_between_widgets_reaches_the_drag_handle() {
    assert!(!ROW.contains(GAP), "the fixture is not in a gap");

    let mut harness = harness();
    harness.hover_at(GAP);
    harness.step();
    harness.drag_at(GAP);
    harness.step();
    // Far enough to be a drag rather than a click, which is what the panel is
    // moved by.
    harness.hover_at(GAP + vec2(0.0, 30.0));
    harness.step();

    let claimed = *harness.state();
    assert!(
        claimed.chrome,
        "the chrome cannot be grabbed anywhere: {claimed:?}"
    );
    assert!(
        !claimed.row,
        "a press in the margin reached a row: {claimed:?}"
    );
}

/// A drag that starts in a gap and travels across a row stays with the handle.
/// Without this, moving the panel any distance would hand the gesture to
/// whatever it passed over on the way.
#[test]
fn a_drag_that_crosses_a_row_stays_with_the_handle_that_started_it() {
    let mut harness = harness();

    harness.hover_at(GAP);
    harness.step();
    harness.drag_at(GAP);
    harness.step();
    // Through the middle of the row, and out the far side.
    for y in [60.0, 100.0, 140.0] {
        harness.hover_at(pos2(GAP.x, y));
        harness.step();
    }
    harness.drop_at(pos2(GAP.x, 140.0));
    harness.step();

    let claimed = *harness.state();
    assert!(
        claimed.chrome,
        "the drag was dropped mid-gesture: {claimed:?}"
    );
    assert!(
        !claimed.row,
        "a row claimed a drag that was already in flight: {claimed:?}"
    );
}
