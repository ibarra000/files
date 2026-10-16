//! The settings window as a standalone process (`files --settings`), with no
//! index, hotkey, tray icon, or search.
//!
//! It is a separate process rather than a panel viewport because a child of
//! the always-on-top panel would sit behind it and interfere with
//! `hide_on_blur`.
//!
//! This process writes the config file itself, then tells the panel to
//! re-read it; `config::write::save` re-reads before applying, so concurrent
//! writers are safe. It works without a running panel, with panel-only
//! actions disabled.

use std::time::Instant;

use eframe::egui;

use super::{Form, exists, lists, report};
use crate::config::write::{Edit, SettingKey};
use crate::config::{ConfigChoice, Settings};
use crate::gui::theme::{self, Theme};
use crate::ipc::{Held, Link, Live, ToPanel, ToSettings};
use crate::view::settings::{ActionId, PageId};

/// Initial size; fits a 1366x768 screen after the taskbar.
const SIZE: [f32; 2] = [940.0, 700.0];

/// Minimum size, a readability floor; the layout itself works at any width.
const MIN_SIZE: [f32; 2] = [820.0, 560.0];

/// Runs the window until it is closed.
pub fn run(settings: Settings, choice: ConfigChoice, page: Option<PageId>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("files - settings")
            .with_app_id("files-settings")
            .with_icon(crate::gui::icon::window_icon(false))
            // An ordinary document window: caption bar, taskbar button, and
            // not always-on-top.
            .with_decorations(true)
            .with_resizable(true)
            .with_inner_size(SIZE)
            .with_min_inner_size(MIN_SIZE),
        // Unlike the panel, which is placed on every summon, this window
        // keeps its user-chosen position between runs.
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "files-settings",
        options,
        Box::new(move |cc| {
            crate::gui::fonts::install(&cc.egui_ctx);
            let wake = {
                let ctx = cc.egui_ctx.clone();
                move || ctx.request_repaint()
            };
            Ok(Box::new(App::new(settings, choice, page, wake)))
        }),
    )
}

/// The settings window's state.
struct App {
    settings: Settings,
    /// The config file source, so a reload reads the same file.
    choice: ConfigChoice,
    theme: Theme,
    /// Whether the title-bar icon is tinted for a dark caption, once set.
    caption_dark: Option<bool>,
    page: PageId,
    /// State only a running panel knows; default until the panel reports.
    live: Live,
    /// The connection to the panel, if one is running.
    link: Option<Box<dyn Link>>,
    /// The update check for the About page, run locally because the panel's
    /// result may be hours old.
    update: Update,
    report: report::Reporter,
    exists: exists::DirCache,
    /// This frame's form, built in `logic` and drawn in `ui`.
    pages: Vec<crate::view::settings::Page>,
    editing: Option<(SettingKey, String)>,
    draft: lists::AliasDraft,
    drive: lists::DriveDraft,
    /// The last save error, if any.
    problem: Option<String>,
    /// Whether Windows starts files at sign-in; `None` if the registry read
    /// failed. Read on open and after each toggle, not every frame.
    autostart: Option<bool>,
}

/// The update check, run off the frame thread because it reads a network
/// share.
enum Update {
    Running(std::sync::mpsc::Receiver<crate::update::Found>),
    Done(Option<crate::update::Found>),
}

impl App {
    fn new(
        settings: Settings,
        choice: ConfigChoice,
        page: Option<PageId>,
        wake: impl Fn() + Clone + Send + 'static,
    ) -> Self {
        let link: Option<Box<dyn Link>> = {
            #[cfg(windows)]
            {
                crate::ipc::pipe::Client::connect(wake.clone())
                    .map(|c| Box::new(c) as Box<dyn Link>)
            }
            #[cfg(not(windows))]
            {
                let _ = &wake;
                None
            }
        };
        // The system theme is unknown before the context exists, so start
        // light and let `follow_theme` correct it on the first frame.
        let theme = Theme::of(settings.theme.is_dark(false));
        Self {
            update: look_for_update(&settings, wake),
            page: page.unwrap_or(PageId::General),
            theme,
            caption_dark: None,
            settings,
            choice,
            live: Live::default(),
            link,
            report: report::Reporter::default(),
            exists: exists::DirCache::default(),
            pages: Vec::new(),
            editing: None,
            draft: lists::AliasDraft::default(),
            drive: lists::DriveDraft::default(),
            problem: None,
            autostart: crate::autostart::is_on().ok(),
        }
    }

    /// Writes or deletes the `Run` value, then reads back the result. Errors
    /// go to the same message bar as save failures.
    fn start_with_windows(&mut self, on: bool) {
        let done = if on {
            crate::autostart::enable()
        } else {
            crate::autostart::disable()
        };
        self.problem = done
            .err()
            .map(|err| format!("Start with Windows \u{b7} {err}"));
        self.autostart = crate::autostart::is_on().ok();
    }

    /// Returns whether a panel is connected.
    fn panel(&self) -> bool {
        self.link.as_ref().is_some_and(|l| l.alive())
    }

    fn tell(&mut self, msg: ToPanel) {
        if let Some(link) = self.link.as_mut() {
            link.send(&msg);
        }
    }

    /// Reloads settings from the config file, which is the source of truth
    /// for both processes.
    fn reload(&mut self) {
        let pinned = self.settings.cli_pinned;
        if let Ok(mut fresh) = Settings::load(&self.choice) {
            // Command-line pins belong to the process, not the file.
            fresh.cli_pinned = pinned;
            self.settings = fresh;
        }
    }

    /// Tracks the system theme each frame, re-applying the style only when
    /// the effective theme changes.
    fn follow_theme(&mut self, ctx: &egui::Context) {
        // The caption follows the Windows app theme, not this setting.
        let caption_dark = ctx.theme() == egui::Theme::Dark;
        if self.caption_dark != Some(caption_dark) {
            self.caption_dark = Some(caption_dark);
            ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(
                crate::gui::icon::window_icon(caption_dark),
            )));
        }

        let dark = self
            .settings
            .theme
            .is_dark(ctx.theme() == egui::Theme::Dark);
        if dark == self.theme.dark {
            return;
        }
        self.theme = Theme::of(dark);
        theme::apply_style(ctx, &self.theme);
    }

    /// Saves edits, reloads, and notifies the panel.
    fn save(&mut self, edits: &[Edit]) {
        let Some(path) = self.choice.path() else {
            self.problem = Some("There is no configuration file to save to.".into());
            return;
        };
        match crate::config::write::save(&path, edits) {
            Ok(()) => {
                self.problem = None;
                self.reload();
                self.tell(ToPanel::Changed);
            }
            Err(err) => self.problem = Some(err.detail()),
        }
    }
}

/// Starts the update check, or returns `Done(None)` if no source is set.
fn look_for_update(settings: &Settings, wake: impl Fn() + Send + 'static) -> Update {
    let Some(source) = crate::update::Source::of(settings) else {
        return Update::Done(None);
    };
    let cache_dir = settings.cache_dir.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("files-settings-update".to_owned())
        .spawn(move || {
            let found = crate::update::look_at(
                &source,
                crate::update::Version::current(),
                cache_dir.as_deref(),
            );
            let _ = tx.send(found);
            wake();
        });
    if spawned.is_err() {
        return Update::Done(None);
    }
    Update::Running(rx)
}

impl eframe::App for App {
    /// Opaque, unlike the panel.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = self.theme.surface;
        [
            c.r() as f32 / 255.0,
            c.g() as f32 / 255.0,
            c.b() as f32 / 255.0,
            1.0,
        ]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();

        let inbox: Vec<ToSettings> = self.link.as_mut().map(|l| l.poll()).unwrap_or_default();
        for msg in inbox {
            match msg {
                ToSettings::Show(page) => {
                    self.page = page;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                ToSettings::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                ToSettings::Live(live) => self.live = live,
                ToSettings::Reload => self.reload(),
                // `panel()` turns false when the pipe breaks, which updates
                // the form's caveats.
                ToSettings::Exiting => {}
            }
        }

        if let Update::Running(rx) = &self.update
            && let Ok(found) = rx.try_recv()
        {
            self.update = Update::Done(Some(found));
        }

        self.follow_theme(ctx);
        self.report.poll(now);
        self.exists.forget_stale(now);

        let pages = crate::view::settings::pages(
            self.update_found(),
            &self.settings,
            self.live.placement,
            self.panel(),
            self.autostart,
        );
        self.report.wanted(
            super::wants_report(&pages, self.page),
            &self.settings,
            self.hotkey_claim(),
            now,
            {
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            },
        );

        // Stashed for `ui`. In eframe 0.36, `logic` runs every frame even
        // when nothing is drawn; `ui` receives the root `Ui`.
        self.pages = pages;
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let ctx = ui.ctx().clone();
        let pages = std::mem::take(&mut self.pages);

        let mut changed = Vec::new();
        let mut actions = Vec::new();
        let aliases;
        let mappings;
        let chosen = {
            {
                let mut form = Form {
                    editing: &mut self.editing,
                    draft: &mut self.draft,
                    drive: &mut self.drive,
                    changed: &mut changed,
                    actions: &mut actions,
                    aliases: None,
                    mappings: None,
                    exists: &mut self.exists,
                    now,
                };
                // Save failures are shown here, since the panel may not be
                // running to show a toast.
                if let Some(problem) = &self.problem {
                    super::widgets::message_bar(
                        ui,
                        &self.theme,
                        crate::view::status::Tone::Bad,
                        &format!("Not saved \u{b7} {problem}"),
                    );
                    ui.add_space(8.0);
                }
                let chosen = super::show(
                    ui,
                    &self.theme,
                    &pages,
                    &self.settings,
                    self.page,
                    self.report.view(),
                    &mut form,
                );
                aliases = form.aliases;
                mappings = form.mappings;
                chosen
            }
        };
        self.page = chosen;

        // Edits are saved in order, before button actions run.
        let edits: Vec<Edit> = changed
            .into_iter()
            .filter(|c| self.settings.can_save(c.key))
            .map(|c| Edit::from_typed(c.key, c.typed))
            .chain(aliases.map(Edit::Aliases))
            .chain(mappings.map(Edit::Mappings))
            .collect();
        if !edits.is_empty() {
            self.save(&edits);
        }

        for action in actions {
            match action {
                // Disabled on the form without a panel; `tell` is a second
                // guard for the frame where a panel is exiting.
                ActionId::ForgetPlacement => self.tell(ToPanel::ForgetPlacement),
                ActionId::InstallUpdate => self.tell(ToPanel::InstallUpdate),
                ActionId::CheckForUpdates => {
                    let ctx = ctx.clone();
                    self.update = look_for_update(&self.settings, move || ctx.request_repaint());
                }
                ActionId::OpenConfigFile =>
                {
                    #[cfg(windows)]
                    if let Some(path) = self.choice.path() {
                        let _ = crate::open::shell_open(&path.to_string_lossy());
                    }
                }
                ActionId::RefreshReport => {
                    let claim = self.hotkey_claim();
                    let ctx = ctx.clone();
                    self.report
                        .refresh(&self.settings, claim, move || ctx.request_repaint());
                }
                // Handled in `gui::settings::page`, where the text is at hand.
                ActionId::CopyReport => {}
                // The `Run` value is per-user, and this process runs as the
                // signed-in user.
                ActionId::StartWithWindows => self.start_with_windows(true),
                ActionId::StopStartingWithWindows => self.start_with_windows(false),
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            self.tell(ToPanel::Closing);
        }
    }
}

impl App {
    fn update_found(&self) -> Option<&crate::update::Found> {
        match &self.update {
            Update::Done(found) => found.as_ref(),
            Update::Running(_) => None,
        }
    }

    /// Returns the panel's hotkey registration status in the form `doctor`
    /// expects.
    fn hotkey_claim(&self) -> Option<Result<(), String>> {
        match &self.live.hotkey {
            // No panel report; `doctor` probes Windows directly, which is
            // accurate because this process holds no hotkey. See
            // `hotkey::Probe`.
            Held::Unknown => None,
            Held::Yes => Some(Ok(())),
            Held::No(why) => Some(Err(why.clone())),
        }
    }
}
