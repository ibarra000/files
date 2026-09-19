//! Help, Settings and Diagnostics.
//!
//! Three ordinary windows, reached from the tray menu and from `?`. Ordinary is
//! the point: they have a title bar, they can be moved and resized, and they
//! stay where they were put. The panel is the thing that behaves unusually, and
//! it earns that by being summoned and dismissed dozens of times an hour; a
//! document somebody reads once does not.
//!
//! # Immediate rather than deferred viewports
//!
//! egui offers both. A deferred viewport runs its own closure on its own
//! schedule, which means the closure must own everything it draws - so every
//! one of these would need a snapshot of the state, taken each frame, whether
//! or not the window was open. An immediate viewport runs inline within the
//! parent's pass and can simply borrow. For three windows that are shut almost
//! all of the time, that is the difference between paying for them always and
//! paying for them when they are open.
//!
//! ## These windows cannot outlive the panel, and deferring them will not help
//!
//! Worth writing down, because it looks like a bug and the obvious fix does not
//! work. `Windows::show` is called from `eframe::App::ui`, so a window is only
//! re-shown while the panel is being drawn - and dismissing the panel therefore
//! takes every one of these with it.
//!
//! Making them deferred does **not** fix that. `Context::show_viewport_deferred`
//! must still be called "each pass when the child viewport should exist", and
//! `eframe::App::logic`'s own documentation says that while the window is
//! hidden "eframe runs no egui pass at all" and that you "may NOT show any ui"
//! from `logic`. There is nowhere left to call it from. The only way a child
//! could outlive the panel is to stop hiding the panel, which is the one thing
//! a summoned overlay must do.
//!
//! So Escape closes the window that has the keyboard, and the panel taking its
//! children with it is the documented consequence rather than an oversight.
//!
//! ## Which is why the panel does not park while one is open
//!
//! That consequence was tolerable while these were documents somebody read.
//! It is not tolerable for a form somebody fills in: dismissing the panel
//! mid-edit would take the settings window with it and discard whatever was
//! half-typed. `gui::Shell::park` therefore refuses to park while
//! [`Windows::any_open`], which is the one remaining way out named above -
//! stop hiding the panel - taken deliberately and only for as long as a window
//! is up.
//!
//! # Why Diagnostics exists at all
//!
//! `--doctor` already prints everything here. But this program is about to stop
//! having a console, and a tray application whose diagnostics need a command
//! prompt is not integrated with anything - the person who needs them is the
//! one who cannot get it to work, which is the worst moment to ask somebody to
//! open a terminal.

use eframe::egui;

use crate::app::state::{AppState, SettingChange};
use crate::config::Settings;
use crate::config::write::{SettingKey, Typed};
use crate::gui::theme::{self, Theme, Weight};
use crate::view;

/// The label column, so every control in the form starts at one rule rather
/// than stepping in and out with the length of each name.
const KEY_COLUMN: f32 = 190.0;

/// Wide enough for a path worth reading, and for the hint text under an empty
/// box.
const TEXT_WIDTH: f32 = 260.0;

/// Which of the three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Window {
    Help,
    Settings,
    Diagnostics,
}

impl Window {
    fn title(self) -> &'static str {
        match self {
            Self::Help => "files - keyboard shortcuts",
            Self::Settings => "files - settings",
            Self::Diagnostics => "files - diagnostics",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Help => "files-help",
            Self::Settings => "files-settings",
            Self::Diagnostics => "files-diagnostics",
        }
    }

    fn size(self) -> [f32; 2] {
        match self {
            Self::Help => [620.0, 560.0],
            Self::Settings => [620.0, 480.0],
            Self::Diagnostics => [860.0, 620.0],
        }
    }
}

/// Which windows are open, and anything they had to compute to open.
#[derive(Default)]
pub struct Windows {
    help: bool,
    settings: bool,
    diagnostics: bool,
    /// The diagnostic report, taken once when the window is opened.
    ///
    /// Not per frame: `doctor` touches the network shares, and running it sixty
    /// times a second would turn a diagnostics window into a load test against
    /// the thing being diagnosed.
    report: Option<String>,
    /// The text box being typed in, and what is in it.
    ///
    /// Not a second copy of the settings: it exists between the moment a box
    /// takes the keyboard and the moment it gives it up, which is state any
    /// text box has to have. One, because only one can have the keyboard.
    editing: Option<(SettingKey, String)>,
}

impl Windows {
    pub fn open(&mut self, which: Window) {
        match which {
            Window::Help => self.help = true,
            Window::Settings => self.settings = true,
            Window::Diagnostics => {
                self.diagnostics = true;
                self.report = None;
            }
        }
    }

    /// Opens the window, or shuts it if it is already up.
    ///
    /// What F1 does, as against what the tray menu does. A menu item named
    /// "Keyboard shortcuts" that closed the window when it was open would be a
    /// menu that lies, so that route still calls [`Self::open`]; a key the help
    /// panel itself describes as "show or hide" has to do both.
    pub fn toggle(&mut self, which: Window) {
        if self.is_open(which) {
            self.close(which);
        } else {
            self.open(which);
        }
    }

    fn close(&mut self, which: Window) {
        match which {
            Window::Help => self.help = false,
            Window::Settings => self.settings = false,
            Window::Diagnostics => self.diagnostics = false,
        }
    }

    pub fn is_open(&self, which: Window) -> bool {
        match which {
            Window::Help => self.help,
            Window::Settings => self.settings,
            Window::Diagnostics => self.diagnostics,
        }
    }

    pub fn any_open(&self) -> bool {
        self.help || self.settings || self.diagnostics
    }

    /// Draws whichever are open, and reports anything that was clicked.
    ///
    /// A return value rather than a callback because these windows are drawn
    /// inline inside the parent's pass: `Shell` is already borrowed for the
    /// frame, and a closure that could reach back into it would not compile.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &Theme,
        state: &AppState,
        settings: &Settings,
        placement: Option<(i32, i32)>,
        report: impl Fn() -> String,
    ) -> Clicked {
        let mut clicked = Clicked::default();
        if self.help {
            let open = show_one(ctx, theme, Window::Help, |ui| help(ui, theme, state));
            self.help = open;
        }
        if self.settings {
            let editing = &mut self.editing;
            let changed = &mut clicked.changed;
            let open = show_one(ctx, theme, Window::Settings, |ui| {
                if self::settings(ui, theme, settings, placement, editing, changed) {
                    clicked.forget_placement = true;
                }
            });
            self.settings = open;
        }
        if self.diagnostics {
            // Computed on the first frame the window is up rather than when the
            // menu item was clicked, so the window appears immediately and the
            // waiting happens with something on screen.
            if self.report.is_none() {
                self.report = Some(report());
            }
            let text = self.report.clone().unwrap_or_default();
            let open = show_one(ctx, theme, Window::Diagnostics, |ui| {
                diagnostics(ui, theme, &text)
            });
            self.diagnostics = open;
        }
        clicked
    }
}

/// What the user pressed in one of these windows, for the caller to act on.
///
/// A struct of one field rather than a bare `bool`, because the Settings window
/// is where a second such button would go and a `bool` return says nothing
/// about which one it was.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clicked {
    pub forget_placement: bool,
    /// Controls the user moved, in the order they moved them.
    pub changed: Vec<SettingChange>,
}

/// One window, returning whether it is still open.
fn show_one(
    ctx: &egui::Context,
    theme: &Theme,
    which: Window,
    mut body: impl FnMut(&mut egui::Ui),
) -> bool {
    let mut open = true;
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of(which.id()),
        egui::ViewportBuilder::default()
            .with_title(which.title())
            .with_inner_size(which.size())
            .with_min_inner_size([420.0, 300.0]),
        |ctx, _class| {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(opaque(theme.surface))
                        .inner_margin(theme::PAD_X),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, &mut body);
                });

            // The title bar's close button, and Escape while this window has
            // the keyboard. Without the first the window closes and
            // immediately reappears, because nothing told the parent it had
            // gone; without the second, the key that shuts every other layer of
            // this program does nothing here.
            //
            // F1 is the third, and it is needed *as well as* the toggle in the
            // shell rather than instead of it. Once this window has the
            // keyboard its keystrokes land in this viewport's input, not the
            // panel's, so `gui::mod::Shell::logic` never sees the press and the
            // toggle there can never fire. Scoped to Help because F1 closing
            // the settings window would be a key doing something unrelated to
            // what it says. The two cannot disagree: whichever of them sees the
            // press, it closes.
            let by_key = ctx.input(|i| {
                i.key_pressed(egui::Key::Escape)
                    || (which == Window::Help && i.key_pressed(egui::Key::F1))
            });
            if by_key || ctx.input(|i| i.viewport().close_requested()) {
                open = false;
            }
        },
    );
    open
}

/// The panel's surface without its transparency.
///
/// These windows are documents rather than overlays: they sit over other
/// programs for minutes at a time, and text on a translucent ground is harder
/// to read the longer you read it.
fn opaque(colour: egui::Color32) -> egui::Color32 {
    egui::Color32::from_rgb(colour.r(), colour.g(), colour.b())
}

fn heading(ui: &mut egui::Ui, theme: &Theme, text: &str) {
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(text)
            .font(theme::font(theme::SIZE_SMALL, Weight::Bold))
            .color(theme.dim),
    );
    ui.add_space(4.0);
}

fn row(ui: &mut egui::Ui, theme: &Theme, key: &str, what: &str) {
    ui.horizontal(|ui| {
        // A fixed key column, so the descriptions line up in one rule rather
        // than stepping in and out with the length of each key name.
        ui.allocate_ui_with_layout(
            egui::vec2(130.0, 22.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(key)
                        .font(theme::font(theme::SIZE_SMALL, Weight::Bold))
                        .color(theme.accent),
                );
            },
        );
        ui.label(
            egui::RichText::new(what)
                .font(theme::font(theme::SIZE_ROW, Weight::Regular))
                .color(theme.text),
        );
    });
}

fn help(ui: &mut egui::Ui, theme: &Theme, state: &AppState) {
    for entry in view::help::rows(state.viewer) {
        match entry {
            view::help::Row::Blank => ui.add_space(6.0),
            view::help::Row::Heading(text) => heading(ui, theme, text),
            view::help::Row::Entry(key, what) => row(ui, theme, key, what),
        }
    }
}

/// Returns whether the remembered window position was asked to be forgotten.
fn settings(
    ui: &mut egui::Ui,
    theme: &Theme,
    settings: &Settings,
    placement: Option<(i32, i32)>,
    editing: &mut Option<(SettingKey, String)>,
    changed: &mut Vec<SettingChange>,
) -> bool {
    // This window used to be read-only, on the grounds that a settings window
    // writing a second copy of the truth is how the file and the window come
    // to disagree. That objection is right and is answered rather than
    // overruled: there is no second copy. Every control below is drawn from
    // the live `Settings` each frame, and a change is written through
    // `config::write`, which reads the file back before it keeps the result.
    // The only state held here is the text of the box being typed in, which
    // any text box has to have.
    for section in view::settings::sections(settings) {
        heading(ui, theme, section.heading);
        for row in &section.rows {
            control(ui, theme, row, editing, changed);
        }
    }

    // Still read-only, and the one thing here that is. A mistyped share path
    // is the single configuration error with no symptom - the search simply
    // finds nothing and the code looks like a job with no files - so it is
    // worth more care than a text box in a list.
    heading(ui, theme, "Drives");
    for mapping in settings.routes.enabled() {
        row(
            ui,
            theme,
            &mapping.name,
            &format!("{} ({:?})", mapping.path.display(), mapping.kind),
        );
    }
    if settings.routes.enabled().count() == 0 {
        ui.label(
            egui::RichText::new("No drives are configured.")
                .font(theme::font(theme::SIZE_ROW, Weight::Regular))
                .color(theme.tone(view::status::Tone::Warn)),
        );
    }

    if let Some(path) = &settings.history_path {
        heading(ui, theme, "Remembering");
        row(ui, theme, "Stored in", &path.display().to_string());
    }

    // The one control in this window that writes anything, and it is not a
    // contradiction of the note at the top: a window position is runtime state
    // the mouse produced, not a line in a file the user maintains. There is
    // nothing here for the file and the window to disagree about.
    let mut forget = false;
    heading(ui, theme, "Where the panel appears");
    match placement {
        Some((left, top)) => {
            row(
                ui,
                theme,
                "Position",
                &format!("where you left it, {left},{top}"),
            );
            ui.add_space(8.0);
            forget = ui.button("Forget the remembered position").clicked();
        }
        None => {
            row(
                ui,
                theme,
                "Position",
                "chosen by the program \u{b7} drag the panel to move it",
            );
        }
    }

    heading(ui, theme, "Updates");
    row(
        ui,
        theme,
        "This version",
        &crate::update::Version::current().to_string(),
    );
    match &settings.update_from {
        Some(folder) => row(ui, theme, "Looking in", &folder.display().to_string()),
        None => row(
            ui,
            theme,
            "Looking in",
            "Nowhere \u{b7} set update_from to be told about new versions",
        ),
    }

    heading(ui, theme, "Configuration file");
    let config = crate::config::file::default_config_path();
    match &config {
        Some(path) => {
            row(ui, theme, "Path", &path.display().to_string());
            ui.add_space(8.0);
            if ui.button("Open the configuration file").clicked() {
                // Whatever the user has registered for .toml, which is what
                // "open" means everywhere else on this machine.
                #[cfg(windows)]
                let _ = crate::open::shell_open(&path.to_string_lossy());
            }
        }
        None => {
            row(
                ui,
                theme,
                "Path",
                "None \u{b7} running on the built-in defaults",
            );
        }
    }
    forget
}

/// One setting, drawn as whatever kind of control it needs.
///
/// A control whose value cannot be saved is drawn disabled rather than hidden.
/// Hiding it would answer "why can I not change the theme?" with silence; this
/// way the setting is visible, its value is visible, and the line underneath
/// names what is holding it.
fn control(
    ui: &mut egui::Ui,
    theme: &Theme,
    row: &view::settings::Row,
    editing: &mut Option<(SettingKey, String)>,
    changed: &mut Vec<SettingChange>,
) {
    use view::settings::Field;

    let mut push = |typed| {
        changed.push(SettingChange {
            key: row.key,
            typed,
            label: row.label,
        })
    };

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(KEY_COLUMN, 22.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(row.label)
                        .font(theme::font(theme::SIZE_SMALL, Weight::Bold))
                        .color(theme.accent),
                );
            },
        );

        ui.add_enabled_ui(row.pin.is_none(), |ui| match &row.field {
            // Every option at once rather than a drop-down. There are two or
            // three of them, they have to be read to be chosen between, and a
            // menu that has to be opened to see what is in it is a menu that
            // hides the answer to the question the window was opened to ask.
            Field::Choice { options, current } => {
                for (i, option) in options.iter().enumerate() {
                    if ui.selectable_label(i == *current, option.label).clicked() && i != *current {
                        push(Typed::Text(option.value.to_string()));
                    }
                }
            }
            Field::Toggle { on } => {
                let mut value = *on;
                if ui.checkbox(&mut value, "").changed() {
                    push(Typed::Flag(value));
                }
            }
            // Committed when the box gives up the keyboard, which is both
            // Enter and clicking away. Not per keystroke: every character of a
            // path would otherwise be a write to a file on a network share,
            // and half of them would name a program that does not exist yet.
            Field::Text { value, placeholder } => {
                let mut text = match &editing {
                    Some((key, buffer)) if *key == row.key => buffer.clone(),
                    _ => value.clone(),
                };
                let response = ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .hint_text(*placeholder)
                        .desired_width(TEXT_WIDTH),
                );
                if response.lost_focus() {
                    if text.trim() != value.trim() {
                        push(Typed::Text(text));
                    }
                    *editing = None;
                } else if response.has_focus() {
                    *editing = Some((row.key, text));
                }
            }
        });
    });

    // The sentence that says what the setting does, then whatever caveat it
    // carries. Indented under the control rather than beside it, because at
    // this width a sentence beside a checkbox is a sentence three words wide.
    ui.horizontal(|ui| {
        ui.add_space(KEY_COLUMN);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(row.help)
                    .font(theme::font(theme::SIZE_SMALL, Weight::Regular))
                    .color(theme.dim),
            );
            if let Some(caveat) = row.caveat() {
                ui.label(
                    egui::RichText::new(caveat)
                        .font(theme::font(theme::SIZE_SMALL, Weight::Regular))
                        .color(theme.tone(view::status::Tone::Warn)),
                );
            }
        });
    });
}

fn diagnostics(ui: &mut egui::Ui, theme: &Theme, report: &str) {
    ui.horizontal(|ui| {
        if ui.button("Copy to clipboard").clicked() {
            ui.ctx().copy_text(report.to_owned());
        }
        ui.label(
            egui::RichText::new("Paste this into an email if you are asking for help.")
                .font(theme::font(theme::SIZE_SMALL, Weight::Regular))
                .color(theme.dim),
        );
    });
    ui.add_space(8.0);

    // Monospaced, because the report lines things up in columns and a
    // proportional font would take that apart.
    ui.label(
        egui::RichText::new(report)
            .monospace()
            .size(theme::SIZE_SMALL)
            .color(theme.text),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opening one must not open the others, and closing one must not close
    /// them - which is the whole of what this struct is for.
    #[test]
    fn the_windows_open_and_close_independently() {
        let mut windows = Windows::default();
        assert!(!windows.any_open());

        windows.open(Window::Help);
        assert!(windows.is_open(Window::Help));
        assert!(!windows.is_open(Window::Settings));
        assert!(!windows.is_open(Window::Diagnostics));
        assert!(windows.any_open());

        windows.open(Window::Settings);
        assert!(windows.is_open(Window::Help), "opening one closed another");
    }

    /// What F1 does. It used to only ever open, so a second press was a no-op
    /// and the window could be shut by nothing but Escape or its title bar -
    /// while the help panel it displays promised "show or hide this list of
    /// keys" throughout.
    #[test]
    fn toggling_a_window_opens_it_and_then_shuts_it() {
        let mut windows = Windows::default();

        windows.toggle(Window::Help);
        assert!(
            windows.is_open(Window::Help),
            "the first press did not open"
        );

        windows.toggle(Window::Help);
        assert!(
            !windows.is_open(Window::Help),
            "the second press did not shut"
        );
        assert!(!windows.any_open());
    }

    /// The tray menu opens; only the key toggles. A menu item that closed the
    /// window it names would be a menu that lies.
    #[test]
    fn opening_an_already_open_window_leaves_it_open() {
        let mut windows = Windows::default();
        windows.open(Window::Help);
        windows.open(Window::Help);
        assert!(windows.is_open(Window::Help));
    }

    #[test]
    fn toggling_one_window_does_not_touch_the_others() {
        let mut windows = Windows::default();
        windows.open(Window::Settings);
        windows.open(Window::Diagnostics);

        windows.toggle(Window::Help);
        windows.toggle(Window::Help);

        assert!(windows.is_open(Window::Settings), "settings was shut too");
        assert!(
            windows.is_open(Window::Diagnostics),
            "diagnostics was shut too"
        );
    }

    /// `doctor` touches the network shares, so the report is taken once per
    /// opening rather than once per frame. Re-opening must take a fresh one, or
    /// somebody who fixed a drive and looked again would see the old answer.
    #[test]
    fn re_opening_diagnostics_discards_the_previous_report() {
        let mut windows = Windows::default();
        windows.open(Window::Diagnostics);
        windows.report = Some("stale".into());

        windows.open(Window::Diagnostics);
        assert_eq!(
            windows.report, None,
            "a second opening would have shown the first one's answer"
        );
    }

    /// The three are separate windows, so nothing about them may collide -
    /// least of all the viewport id, which is what egui keys their position and
    /// size on.
    #[test]
    fn each_window_is_distinguishable_from_the_others() {
        let all = [Window::Help, Window::Settings, Window::Diagnostics];
        for field in [Window::id, Window::title] {
            let mut seen: Vec<_> = all.iter().map(|w| field(*w)).collect();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), all.len(), "two windows share a name");
        }
    }
}
