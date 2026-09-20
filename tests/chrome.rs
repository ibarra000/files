//! What the drag handle rests on, which is not this program but the toolkit.
//!
//! `gui::mod::Shell::ui` makes the panel draggable by interacting with the
//! header and the footer - see `gui::panel::handles` - *before*
//! `gui::panel::show` runs, and relies on the search box and the chips, which
//! are registered afterwards over the same pixels, taking the press instead.
//! Nothing computes where the air around the search box is: it is simply
//! whatever the box did not claim.
//!
//! The handle used to be the whole panel on the same argument, and the
//! argument stopped holding the moment the content band became a scroller: a
//! press in the gap between two rows has to move the list, not carry the
//! window off across the desktop. So the *bands* are named now and the claim
//! about egui is what makes the layering inside them work.
//!
//! It is one line of policy resting on that claim, and the claim is the part
//! worth pinning. If it ever stops holding, clicking in the search box stops
//! moving the caret and starts dragging the window - and nothing else in the
//! suite would notice, because `Shell::ui` owns a window, a tray icon and
//! three threads, and no test drives it.
//!
//! Its own file rather than a module inside `tests/panel.rs`, because it shares
//! none of that file's fixtures: there is no `AppState` here and no panel, only
//! two overlapping rectangles standing in for the two layers.

use eframe::egui::{self, Pos2, Rect, Sense, pos2, vec2};
use egui_kittest::Harness;

/// The header band, and the search box inside it.
const PANEL: Rect = Rect::from_min_max(Pos2::ZERO, pos2(200.0, 200.0));
const ROW: Rect = Rect::from_min_max(pos2(20.0, 80.0), pos2(180.0, 120.0));

/// Somewhere inside the band that the box does not cover: the air above it.
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

/// A click on the box is the box's, even though the handle underneath it
/// covers the same pixel and asked for clicks too.
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

/// And a press in the air around it, which nothing else asked for, reaches
/// the handle - otherwise there would be nothing to drag the panel by at all.
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

/// A drag that starts in the air and travels across the box stays with the
/// handle. Without this, moving the panel any distance would hand the gesture
/// to whatever it passed over on the way.
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
