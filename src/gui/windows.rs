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
//! # Why Diagnostics exists at all
//!
//! `--doctor` already prints everything here. But this program is about to stop
//! having a console, and a tray application whose diagnostics need a command
//! prompt is not integrated with anything - the person who needs them is the
//! one who cannot get it to work, which is the worst moment to ask somebody to
//! open a terminal.

use eframe::egui;

use crate::app::state::AppState;
use crate::config::Settings;
use crate::gui::theme::{self, Theme, Weight};
use crate::view;

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

    /// Draws whichever are open.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &Theme,
        state: &AppState,
        settings: &Settings,
        report: impl Fn() -> String,
    ) {
        if self.help {
            let open = show_one(ctx, theme, Window::Help, |ui| help(ui, theme, state));
            self.help = open;
        }
        if self.settings {
            let open = show_one(ctx, theme, Window::Settings, |ui| {
                self::settings(ui, theme, settings)
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
    }
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

            // The title bar's close button. Without this the window can be
            // closed and immediately reappears, because nothing told the parent
            // it had gone.
            if ctx.input(|i| i.viewport().close_requested()) {
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

fn settings(ui: &mut egui::Ui, theme: &Theme, settings: &Settings) {
    // Read-only, deliberately. Everything here comes from a file the user
    // already owns and can already edit, and a settings window that writes a
    // second copy of the truth is how the file and the window come to disagree.
    // What this adds is knowing *what is in force right now*, which is the
    // question somebody actually has.
    heading(ui, theme, "SHARES");
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
            egui::RichText::new("No shares are configured.")
                .font(theme::font(theme::SIZE_ROW, Weight::Regular))
                .color(theme.tone(view::status::Tone::Warn)),
        );
    }

    heading(ui, theme, "SHORTCUT");
    row(
        ui,
        theme,
        "Summon",
        &settings
            .hotkey
            .bound()
            .map(crate::hotkey::spec::describe)
            .unwrap_or_else(|| "off".into()),
    );

    heading(ui, theme, "OPENING");
    row(ui, theme, "Viewer", &format!("{:?}", settings.viewer));
    if let Some(path) = &settings.pdf_viewer {
        row(ui, theme, "PDF viewer", &path.display().to_string());
    }

    heading(ui, theme, "REMEMBERING");
    row(
        ui,
        theme,
        "Recent codes",
        if settings.history { "on" } else { "off" },
    );
    if let Some(path) = &settings.history_path {
        row(ui, theme, "Stored in", &path.display().to_string());
    }

    heading(ui, theme, "CONFIGURATION FILE");
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
            row(ui, theme, "Path", "none - running on the built-in defaults");
        }
    }
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
