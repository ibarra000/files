//! The settings widgets, drawn, and then read back.
//!
//! The same safety net `tests/panel.rs` describes, stretched over the other
//! window. The argument is the same one and is worth repeating because this
//! window went without it for longer: a control that quietly stops being
//! painted, or quietly stops telling the accessibility tree what it is, fails
//! nothing and nobody notices until somebody using a screen reader reports it.
//!
//! What it drives is the widgets rather than a page, and that is deliberate
//! rather than a shortcut. The switch is the one control in this program that
//! is painted from nothing, so it is the one whose contract with `accesskit`
//! is a claim rather than something egui does on our behalf. This is where
//! that claim is checked.

use eframe::egui;
use eframe::egui::accesskit;
use egui_kittest::Harness;
use egui_kittest::kittest::{AccessKitNode, NodeT};

use files::gui::settings::widgets::{self, Row, width};
use files::gui::theme::Theme;

/// What one frame of the harness is looking at.
struct Form {
    theme: Theme,
    fonts_ready: bool,
    /// The setting being toggled, and how many times it moved.
    on: bool,
    flips: usize,
}

fn harness(dark: bool) -> Harness<'static, Form> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(560.0, 300.0))
        // Points, not pixels: a picture taken at whatever this machine is
        // scaled to is a picture of the machine.
        .with_pixels_per_point(1.0)
        .build_ui_state(
            |ui, form: &mut Form| {
                // The families have to exist before anything is laid out, and
                // new definitions are picked up at the *start* of a frame -
                // so the first frame installs them and draws nothing. Exactly
                // what `tests/panel.rs` does, and for the same reason.
                if !form.fonts_ready {
                    files::gui::fonts::install_bundled(ui.ctx());
                    files::gui::theme::apply_style(ui.ctx(), &form.theme);
                    form.fonts_ready = true;
                    return;
                }

                let theme = form.theme;
                widgets::setting_row(
                    ui,
                    &theme,
                    Row {
                        label: "Recent codes",
                        help: "Keep the codes you search for, so the up arrow brings them \
                               back tomorrow.",
                        caveat: None,
                        enabled: true,
                        control_w: width::SWITCH,
                    },
                    |ui| {
                        if widgets::switch(ui, &theme, &mut form.on, "Recent codes").changed() {
                            form.flips += 1;
                        }
                    },
                );

                widgets::setting_row(
                    ui,
                    &theme,
                    Row {
                        label: "Summon with",
                        help: "The key that brings this window up from wherever you are \
                               working.",
                        caveat: Some("Applies when files next starts"),
                        // A setting something else is holding. Drawn rather
                        // than hidden, which is the behaviour being pinned.
                        enabled: false,
                        control_w: width::SWITCH,
                    },
                    |ui| {
                        let mut held = true;
                        widgets::switch(ui, &theme, &mut held, "Summon with");
                    },
                );
            },
            Form {
                theme: Theme::of(dark),
                fonts_ready: false,
                on: false,
                flips: 0,
            },
        );

    harness.run_steps(4);
    harness
}

/// Everything a row says has to be in the tree, not just what fitted.
///
/// The label is laid out to one line and ellipsised, so the pixels are
/// allowed to lose the end of it. The accessible name is not: somebody
/// reading this window with a screen reader gets the whole label and the
/// whole sentence under it or they get a setting they cannot identify.
#[test]
fn a_setting_reads_out_its_label_and_the_sentence_under_it() {
    let harness = harness(false);
    let spoken = spoken(&harness);
    assert!(
        spoken
            .iter()
            .any(|s| s.contains("Recent codes") && s.contains("up arrow brings them back")),
        "the row did not read out in full: {spoken:#?}"
    );
}

/// The switch is painted from nothing, so what it tells `accesskit` is a
/// claim this file makes rather than something egui does for us.
#[test]
fn a_switch_reports_itself_as_a_checkbox_that_is_off() {
    let harness = harness(false);
    let switches = switches(&harness);
    let node = switches
        .first()
        .expect("the switch is not in the accessibility tree at all");
    assert_eq!(node.label().as_deref(), Some("Recent codes"));
    assert_eq!(
        node.toggled(),
        Some(accesskit::Toggled::False),
        "a switch that is off did not say so"
    );
}

/// And moves when it is clicked, once per click.
#[test]
fn clicking_a_switch_flips_it_exactly_once() {
    let mut harness = harness(false);
    let at = switches(&harness)
        .first()
        .and_then(|n| n.bounding_box())
        .expect("the switch has no rectangle to click");
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(at.x0 as f32 + 4.0, at.y0 as f32 + 4.0),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(at.x0 as f32 + 4.0, at.y0 as f32 + 4.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(4);

    assert!(harness.state().on, "the switch did not move");
    assert_eq!(harness.state().flips, 1, "one click, more than one change");
    assert_eq!(
        switches(&harness).first().unwrap().toggled(),
        Some(accesskit::Toggled::True)
    );
}

/// A setting that cannot be saved is shown and refused rather than hidden,
/// and the line that says why is read out with it.
#[test]
fn a_setting_that_is_held_elsewhere_is_still_on_screen_and_says_so() {
    let harness = harness(false);
    let spoken = spoken(&harness);
    assert!(
        spoken
            .iter()
            .any(|s| s.contains("Summon with") && s.contains("brings this window up")),
        "a pinned setting vanished instead of being refused: {spoken:#?}"
    );

    let switches = switches(&harness);
    let held = switches
        .get(1)
        .expect("the pinned switch is not in the tree");
    assert!(
        held.is_disabled(),
        "a setting nothing can save was offered as though it could be"
    );
}

/// Both palettes, because a window drawn in one and checked in the other is
/// a window half of this office never sees.
#[cfg(feature = "ui-snapshots")]
#[test]
fn looks_right_in_the_light_theme() {
    let mut harness = harness(false);
    harness.snapshot("settings_light");
}

#[cfg(feature = "ui-snapshots")]
#[test]
fn looks_right_in_the_dark_theme() {
    let mut harness = harness(true);
    harness.snapshot("settings_dark");
}

/// Every label the tree carries, in no particular order.
///
/// The tree rather than the pixels, for the reason `tests/panel.rs` gives:
/// it is derived from the same layout pass, it is what a screen reader is
/// handed, and it needs no GPU.
fn spoken(harness: &Harness<'static, Form>) -> Vec<String> {
    nodes(harness)
        .into_iter()
        .filter_map(|n| n.label())
        .collect()
}

/// Every switch, in the order they were drawn in.
fn switches<'a>(harness: &'a Harness<'static, Form>) -> Vec<AccessKitNode<'a>> {
    nodes(harness)
        .into_iter()
        .filter(|n| n.role() == accesskit::Role::CheckBox)
        .collect()
}

fn nodes<'a>(harness: &'a Harness<'static, Form>) -> Vec<AccessKitNode<'a>> {
    let root = harness.root();
    std::iter::once(root.accesskit_node())
        .chain(root.children_recursive().map(|n| n.accesskit_node()))
        .collect()
}
