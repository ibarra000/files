//! Pins the egui layering behaviour the panel's drag handle relies on.
//!
//! `Shell::ui` registers the header and footer drag handles (see
//! `gui::panel::handles`) before `gui::panel::show` runs, and relies on widgets
//! registered later over the same pixels taking the press. If that stops
//! holding, clicks in the search box drag the window instead, and no other test
//! would notice because nothing drives `Shell::ui`.

use eframe::egui::{self, Pos2, Rect, Sense, pos2, vec2};
use egui_kittest::Harness;

/// The header band and the search box inside it.
const PANEL: Rect = Rect::from_min_max(Pos2::ZERO, pos2(200.0, 200.0));
const ROW: Rect = Rect::from_min_max(pos2(20.0, 80.0), pos2(180.0, 120.0));

/// A point inside the band that the box does not cover.
const GAP: Pos2 = pos2(100.0, 20.0);

/// Which of the two layers took the pointer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Claimed {
    chrome: bool,
    row: bool,
}

/// Two overlapping interactions, registered in the order `Shell::ui` uses: the
/// handle first, the widget on top.
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

                // Latched: clicks and drags are reported on different frames, so
                // plain assignment would depend on which frame the harness ends on.
                claimed.chrome |= chrome.clicked() || chrome.dragged();
                claimed.row |= row.clicked();
            },
            Claimed::default(),
        );
    harness.run();
    harness
}

/// A click on the box goes to the box, not to the handle beneath it.
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

/// A press outside any widget reaches the handle.
#[test]
fn a_press_on_the_gaps_between_widgets_reaches_the_drag_handle() {
    assert!(!ROW.contains(GAP), "the fixture is not in a gap");

    let mut harness = harness();
    harness.hover_at(GAP);
    harness.step();
    harness.drag_at(GAP);
    harness.step();
    // Far enough to register as a drag rather than a click.
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

/// A drag that starts on the handle stays with it while crossing a widget.
#[test]
fn a_drag_that_crosses_a_row_stays_with_the_handle_that_started_it() {
    let mut harness = harness();

    harness.hover_at(GAP);
    harness.step();
    harness.drag_at(GAP);
    harness.step();
    // Through the row and out the far side.
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
