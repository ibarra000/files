//! The settings window, as its own program.
//!
//! `files --settings` runs this and nothing else: no index, no hotkey, no
//! tray icon, no search. It is a form over a configuration file, and that
//! is all the process is.
//!
//! # Why a process and not a viewport
//!
//! It used to be an immediate viewport of the panel, and three of the
//! reported faults were that arrangement rather than anything drawn in it.
//!
//! The panel is `always_on_top`, because a thing summoned by a global
//! hotkey has to arrive in front of whatever you were doing. A child
//! viewport of an always-on-top window is behind it, so the panel floated
//! over the settings window. `park()` then refused to dismiss the panel
//! while a window was open - which is what `any_open()` was for - so it
//! could not be got out of the way either, and `hide_on_blur` stopped
//! meaning what the form said it meant: opening the settings window
//! suppressed exactly the blur that should have dismissed the panel.
//!
//! As a process none of that exists. The window is an ordinary top-level
//! window with a caption bar, a taskbar button, and no relationship to the
//! panel's z-order at all. This is what Ueli does.
//!
//! # With no panel running
//!
//! Supported, and not a fallback. Every setting saves, because this process
//! writes the file itself - see [`Editor::save`]. What needs a panel is
//! three things, and each is disabled with a reason rather than failing
//! when pressed.
//!
//! # Who writes the file
//!
//! This process does, always, and then tells the panel to re-read it.
//!
//! The alternative - sending each edit across and having the panel perform
//! the write - would put the whole configuration schema on the wire and
//! then on the wire again in the other direction, to avoid a race that
//! `config::write::save` already handles: it re-reads the file before
//! applying, precisely because "another instance - or the user, in an
//! editor - may have changed a mapping since". Two writers is a case that
//! module was written for.

use std::time::Instant;

use eframe::egui;

use super::{Form, exists, lists, report};
use crate::config::write::{Edit, SettingKey};
use crate::config::{ConfigChoice, Settings};
use crate::gui::theme::{self, Theme};
use crate::ipc::{Held, Link, Live, ToPanel, ToSettings};
use crate::view::settings::{ActionId, PageId};

/// Big enough for the nav and a reading column, short enough to open whole
/// on a 1366 by 768 laptop once the taskbar has taken its share.
///
/// Ueli uses 1000 by 800; the 800 is the part not to copy, because this
/// configuration follows people onto laptops by design.
const SIZE: [f32; 2] = [940.0, 700.0];

/// The smallest this window may be made. See the note in [`super::measure`]:
/// nothing breaks at any width now, so this is a readability floor rather
/// than a damage limit.
const MIN_SIZE: [f32; 2] = [820.0, 560.0];

/// Runs the window until it is closed.
pub fn run(settings: Settings, choice: ConfigChoice, page: Option<PageId>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("files - settings")
            .with_app_id("files-settings")
            // Everything the panel is not. A caption bar, because this is a
            // document window somebody keeps open and moves about; a
            // taskbar button, because it is a place you come back to; and
            // no always-on-top, because it is not summoned.
            .with_decorations(true)
            .with_resizable(true)
            .with_inner_size(SIZE)
            .with_min_inner_size(MIN_SIZE),
        // Remembered between runs, unlike the panel's. The panel is placed
        // by the program on every summon; this is a window somebody has
        // put where they want it.
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

/// The window, and everything it is drawn from.
struct App {
    settings: Settings,
    /// Which file the settings came from, so a re-read comes from the same
    /// one. `--config` is a flag on *this* process as well as on the panel.
    choice: ConfigChoice,
    theme: Theme,
    page: PageId,
    /// The facts only a running panel knows. Default until one says.
    live: Live,
    /// The panel, if there is one.
    link: Option<Box<dyn Link>>,
    /// What the About page found, computed here rather than carried across.
    ///
    /// The panel has an answer of its own, four hours stale by design, and
    /// sending it would have meant a fifth field on the wire to be more
    /// wrong than a fresh look. `update::look` is a directory read.
    update: Update,
    report: report::Reporter,
    exists: exists::DirCache,
    /// This frame's form, built in `logic` and drawn in `ui`.
    pages: Vec<crate::view::settings::Page>,
    editing: Option<(SettingKey, String)>,
    draft: lists::AliasDraft,
    drive: lists::DriveDraft,
    /// What the last save did, if it failed.
    problem: Option<String>,
    /// Whether Windows starts files at sign-in. `None` when the registry
    /// would not say. Read when the window opens and again after every flip
    /// of the switch, rather than every frame: nothing else this window does
    /// moves it, and a flip reads back what actually landed.
    autostart: Option<bool>,
}

/// The update check, off the frame thread for the same reason the
/// diagnostics are: it reads a network share.
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
        // The system's answer is not available before the context exists,
        // so `System` starts light and `follow_theme` corrects it on the
        // first frame - exactly as the panel does.
        let theme = Theme::of(settings.theme.is_dark(false));
        Self {
            update: look_for_update(&settings, wake),
            page: page.unwrap_or(PageId::General),
            theme,
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

    /// Writes or deletes the `Run` value, then reads back what landed.
    ///
    /// A failure is said on the message bar where a failed save is, because
    /// it is the same news: the switch moved and Windows did not.
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

    /// Whether there is a panel to act on.
    fn panel(&self) -> bool {
        self.link.as_ref().is_some_and(|l| l.alive())
    }

    fn tell(&mut self, msg: ToPanel) {
        if let Some(link) = self.link.as_mut() {
            link.send(&msg);
        }
    }

    /// Reads the configuration file again, keeping nothing of what is here.
    ///
    /// The file is the truth, and both processes can read it - which is
    /// most of why the wire carries so little.
    fn reload(&mut self) {
        let pinned = self.settings.cli_pinned;
        if let Ok(mut fresh) = Settings::load(&self.choice) {
            // Command-line pins are a property of the *processes*, not of
            // the file, so they do not survive a re-read on their own.
            fresh.cli_pinned = pinned;
            self.settings = fresh;
        }
    }

    /// Follows the system theme, where the setting says to.
    ///
    /// Every frame, like the panel's: `ctx.theme()` is winit's answer and
    /// moves when Windows does. The style is re-applied only when the
    /// answer changes, because a `Style` is several hundred bytes cloned
    /// and the theme moves about twice a day.
    fn follow_theme(&mut self, ctx: &egui::Context) {
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

    /// Writes an edit, and tells the panel the file moved.
    fn save(&mut self, edits: &[Edit]) {
        let Some(path) = self.choice.path() else {
            self.problem = Some("There is no configuration file to write to.".into());
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

/// Starts the update check, or reports that there is nothing to check.
fn look_for_update(settings: &Settings, wake: impl Fn() + Send + 'static) -> Update {
    let Some(folder) = settings.update_from.clone() else {
        return Update::Done(None);
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("files-settings-update".to_owned())
        .spawn(move || {
            let found = crate::update::look(&folder, crate::update::Version::current());
            let _ = tx.send(found);
            wake();
        });
    if spawned.is_err() {
        return Update::Done(None);
    }
    Update::Running(rx)
}

impl eframe::App for App {
    /// Opaque. This window is a document rather than an overlay.
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

        // Whatever the panel has said. `Close` and `Exiting` are the two
        // that change what this process does rather than what it draws.
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
                // Nothing to do but notice. `panel()` goes false on its own
                // when the pipe breaks, and every caveat on the form reads
                // from that.
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

        // Stashed for `ui` below, which is where this frame's drawing
        // happens. The split is eframe 0.36's: `logic` runs once per frame
        // whether or not anything is drawn, and `ui` is handed the root
        // `Ui`. Everything above here is the part that has to run even
        // while the window is hidden behind something.
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
                // A save that did not land, said where the setting is
                // rather than in a toast on a panel that may not be
                // running. The change is already in force for this
                // session - the form drew it the moment it moved - so the
                // wording has to say what did and did not happen.
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

        // In the order they were moved, and before the buttons: a setting
        // is what the window is for and a button is what somebody does
        // afterwards.
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
                // Both of these need the panel, and both are disabled on
                // the form when there is not one - this is the second
                // guard, because a form and a link can disagree for one
                // frame while a panel is exiting.
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
                // Handled where it is pressed, because it needs the report
                // text and nothing else. See `gui::settings::page`.
                ActionId::CopyReport => {}
                // Here rather than in the panel: the `Run` value is the
                // signed-in user's, and this process is running as them.
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

    /// What the panel said about the chord, in the shape `doctor` wants.
    fn hotkey_claim(&self) -> Option<Result<(), String>> {
        match &self.live.hotkey {
            // No panel has spoken, so this process may ask Windows itself -
            // and unlike the panel it holds no chord, so the answer is
            // honest. See `hotkey::Probe`.
            Held::Unknown => None,
            Held::Yes => Some(Ok(())),
            Held::No(why) => Some(Err(why.clone())),
        }
    }
}
