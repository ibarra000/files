//! The desktop shell: a window that drives a [`crate::app::App`], feeding it
//! events and pumping it once per turn.
//!
//! # Hidden turns
//!
//! eframe runs [`eframe::App::logic`] even while the window is hidden, with no
//! egui pass. The panel is hidden most of the time, so index work continues
//! there at no frame cost; drawing happens only in [`eframe::App::ui`].
//!
//! # Waking
//!
//! The toolkit owns the event loop and sleeps, so workers are given an
//! [`crate::app::event::Events`] whose wake calls `Context::request_repaint`.
//! Without it, a worker event posted while idle would not be processed.

pub mod anim;
pub mod drag;
pub mod fonts;
pub mod frame;
pub mod icon;
pub mod input;
pub mod link;
pub mod panel;
pub mod settings;
#[cfg(windows)]
pub mod taskbar;
pub mod text;
pub mod theme;
#[cfg(windows)]
pub mod tray;
pub mod window;

use eframe::egui;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::app::App;
use crate::app::event::AppEvent;
use crate::config::Settings;
use crate::index::enumerate::DirSource;
use crate::placement;
use crate::view::settings::PageId;

pub use theme::{PANEL_H, PANEL_W};

// `DWMWA_SYSTEMBACKDROP_TYPE` applies to a region that does not follow a
// resizing window, so the backdrop relies on the panel's fixed size. See
// `window::Material`.

/// A request from outside the frame loop.
///
/// The tray icon and a second launch run on their own threads while a hidden
/// panel requests no frames, so they post here and then wake the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Show,
    Settings,
    Diagnostics,
    InstallUpdate,
    Quit,
    /// The taskbar may have switched between light and dark.
    TaskbarTheme,
}

/// Runs the application until the user quits.
pub fn run(
    settings: Settings,
    choice: crate::config::ConfigChoice,
    source: Arc<dyn DirSource>,
) -> eframe::Result<()> {
    // Created here rather than taken from the creation context, because the
    // workers need a context to wake before the window exists.
    let ctx = egui::Context::default();

    let options = eframe::NativeOptions {
        // Explicit, although `glow` is the only backend compiled in: window
        // transparency depends on it. See the note in `Cargo.toml`.
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("files")
            .with_app_id("files")
            .with_icon(icon::window_icon(false))
            // Frameless: the panel paints its own edge.
            .with_decorations(false)
            // Required: makes winit call `DwmEnableBlurBehindWindow`, which is
            // how the window becomes transparent. See `Cargo.toml`.
            .with_transparent(true)
            .with_resizable(false)
            .with_always_on_top()
            // No taskbar button or Alt-Tab entry.
            .with_taskbar(false)
            // Hidden until summoned. The shell reads the taskbar style when a
            // window is first shown, so this also makes `hide_from_taskbar`
            // take effect.
            .with_visible(false)
            .with_inner_size([PANEL_W, PANEL_H]),
        // The window is positioned on every summon; restoring the last
        // session's rectangle would conflict with that.
        persist_window: false,
        ..Default::default()
    };

    // Small bound: these are user gestures, and a long backlog behind a stalled
    // frame is not worth replaying.
    let (requests_tx, requests_rx) = crossbeam_channel::bounded::<Request>(16);
    let post = {
        let ctx = ctx.clone();
        let tx = requests_tx.clone();
        move |request: Request| {
            // Wake only if the post succeeded, as `Events::try_send` does;
            // waking for a dropped event can make the loop spin.
            if tx.try_send(request).is_ok() {
                ctx.request_repaint();
            }
        }
    };

    // Claimed before the window and workers start, so a second instance exits
    // before doing any work.
    #[cfg(windows)]
    let _instance = {
        let post = post.clone();
        match crate::single::claim(move || post(Request::Show)) {
            crate::single::Claim::Only(instance) => instance,
            // Not an error: the running instance shows its panel.
            crate::single::Claim::AlreadyRunning => return Ok(()),
        }
    };

    let build = {
        let ctx = ctx.clone();
        move |cc: &eframe::CreationContext<'_>| {
            let shell = Shell::new(cc, ctx, settings, choice, source, requests_rx, post)?;
            Ok(Box::new(shell) as Box<dyn eframe::App>)
        }
    };

    eframe::run_native_ext("files", options, Some(ctx), Box::new(build))
}

/// Returns the window handle as an integer.
///
/// `HWND` is a raw pointer and so neither `Send` nor `Sync`; an integer can be
/// passed to the hotkey thread without an `unsafe impl`. It is converted back
/// to a window in one module only.
#[cfg(windows)]
pub(crate) fn hwnd_of(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
pub(crate) fn hwnd_of(_cc: &eframe::CreationContext<'_>) -> Option<isize> {
    None
}

/// The panel window and the application it drives.
struct Shell {
    app: App,
    /// The backdrop the compositor accepted. `None` off Windows, and
    /// [`window::Backdrop::Painted`] where acrylic is unsupported, in which
    /// case the panel paints its own background.
    backdrop: Option<window::Backdrop>,
    /// Kept so the window can be restyled when the system theme changes.
    hwnd: Option<isize>,
    /// Whether Segoe UI was found, for diagnostics.
    #[allow(dead_code)]
    system_fonts: fonts::Found,
    /// The panel's animation state.
    frame: frame::Frame,
    theme: theme::Theme,
    /// Whether the panel was meant to be visible as of the last frame.
    up: bool,
    /// Requests from the tray and from a second launch.
    requests: crossbeam_channel::Receiver<Request>,
    /// The connection to the settings window, which is a separate process.
    link: link::PanelLink,
    /// The configuration file this run uses.
    ///
    /// Needed to re-read the file after the settings window rewrites it;
    /// `Settings` does not record which choice produced it.
    config: crate::config::ConfigChoice,
    /// The notification-area icon. Dropping it removes the icon from the tray.
    #[cfg(windows)]
    tray: Option<tray::Tray>,
    /// Whether the window has already been asked to hide for this dismissal.
    parked: bool,
    /// The in-progress drag of the panel, if any.
    drag: drag::Drag,
    /// The thread that owns `%APPDATA%\files\window.txt`.
    ///
    /// Owned here rather than by [`crate::app::actors::Actors`] because a
    /// window position is a pixel coordinate, and `AppState` holds no
    /// coordinates. See [`crate::app::state::pointer`].
    placement: Option<placement::Writer>,
}

impl Shell {
    fn new(
        cc: &eframe::CreationContext<'_>,
        ctx: egui::Context,
        settings: Settings,
        choice: crate::config::ConfigChoice,
        source: Arc<dyn DirSource>,
        requests: crossbeam_channel::Receiver<Request>,
        post: impl Fn(Request) + Clone + Send + Sync + 'static,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // The configured theme; `system` follows Windows.
        let dark = settings
            .theme
            .is_dark(cc.egui_ctx.theme() == egui::Theme::Dark);

        // Styled before anything is drawn and before the window is first
        // shown, because the shell reads the taskbar style only once.
        let hwnd = hwnd_of(cc);
        let backdrop = hwnd.map(|hwnd| {
            window::hide_from_taskbar(hwnd);
            window::apply(hwnd, dark, settings.backdrop, !settings.dock.is_docked())
        });
        let system_fonts = fonts::install(&cc.egui_ctx);
        // After the fonts: the style refers to the families `fonts::install`
        // registers.
        theme::apply_style(&cc.egui_ctx, &theme::Theme::of(dark));

        // Called by workers after posting. See the module docs.
        let wake = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let placement_path = settings.placement_path.clone();
        let app = App::start(settings, source, wake)?;

        // The hotkey thread started in `App::start` and waits for the window
        // to summon.
        if let Some(hwnd) = hwnd {
            app.actors.panel.publish(hwnd);
        }

        // Publish the saved position before the first summon, because the
        // hotkey thread reads it on a keypress without coordination.
        //
        // A writer that fails to start is not fatal; dragging still works but
        // the position is not saved.
        let placement = placement_path.and_then(|path| {
            app.actors.panel.remember(placement::load(&path));
            placement::spawn_writer(path).ok()
        });
        // The dock setting, also read by the first summon.
        app.actors.panel.set_dock(app.state.settings.dock);

        // Built on the event loop's thread, where the tray icon's hidden
        // window must live for its messages to be pumped.
        //
        // A tray icon the shell refuses is not fatal; the hotkey still works.
        #[cfg(windows)]
        let tray = tray::Tray::new({
            let post = post.clone();
            move |action| {
                post(match action {
                    tray::TrayAction::Show => Request::Show,
                    tray::TrayAction::Settings => Request::Settings,
                    tray::TrayAction::Diagnostics => Request::Diagnostics,
                    tray::TrayAction::InstallUpdate => Request::InstallUpdate,
                    tray::TrayAction::Quit => Request::Quit,
                });
            }
        })
        .ok();
        #[cfg(windows)]
        if tray.is_some()
            && let Err(err) = taskbar::watch(move || post(Request::TaskbarTheme))
        {
            log::warn!("the tray icon will not follow the taskbar theme: {err}");
        }

        Ok(Self {
            app,
            backdrop,
            hwnd,
            system_fonts,
            frame: frame::Frame::new(),
            theme: theme::Theme::of(dark),
            up: false,
            parked: true,
            requests,
            config: choice,
            link: link::PanelLink::new({
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            }),
            #[cfg(windows)]
            tray,
            drag: drag::Drag::new(),
            placement,
        })
    }

    /// Switches between light and dark when the effective theme changes,
    /// including the window frame.
    fn follow_theme(&mut self, ctx: &egui::Context) {
        let dark = self
            .app
            .state
            .settings
            .theme
            .is_dark(ctx.theme() == egui::Theme::Dark);
        if dark == self.theme.dark {
            return;
        }
        self.theme = theme::Theme::of(dark);
        // Widgets take colours from egui's style, not `self.theme`. Applied
        // only on change, because cloning the style every frame is wasteful.
        theme::apply_style(ctx, &self.theme);
        if let Some(hwnd) = self.hwnd {
            let settings = &self.app.state.settings;
            self.backdrop = Some(window::apply(
                hwnd,
                dark,
                settings.backdrop,
                !settings.dock.is_docked(),
            ));
        }
    }

    /// Handles all pending requests from the tray and a second launch.
    ///
    /// Drained in one turn, as [`crate::app::App::pump`] drains events.
    fn serve_requests(&mut self, now: Instant) {
        while let Ok(request) = self.requests.try_recv() {
            match request {
                // Via the hotkey thread: showing the panel takes the
                // foreground, which Windows allows only from that thread.
                Request::Show => self.app.actors.summon_overlay(),
                // A menu item always opens a named page; only
                // `Cmd::ToggleSettings` resumes the last one.
                Request::Settings => self.link.open_at(PageId::General, &self.app.state.settings),
                Request::Diagnostics => self
                    .link
                    .open_at(PageId::Diagnostics, &self.app.state.settings),
                Request::InstallUpdate => self.install_update(now),
                Request::Quit => self.app.state.should_quit = true,
                Request::TaskbarTheme =>
                {
                    #[cfg(windows)]
                    if let Some(tray) = self.tray.as_mut() {
                        tray.follow_taskbar();
                    }
                }
            }
        }
    }

    /// Keeps the tray's install item in step with the update checker.
    ///
    /// Offered only when the installer is on disk. Called every frame; it is
    /// cheap unless the result changed.
    #[cfg(windows)]
    fn follow_update(&mut self) {
        let Some(tray) = self.tray.as_mut() else {
            return;
        };
        match &self.app.state.update {
            Some(crate::update::Found::Available {
                manifest,
                msi_present: true,
                ..
            }) => tray.offer_update(manifest.version),
            _ => tray.withdraw_update(),
        }
    }

    /// Starts the entrance or exit animation when visibility changes.
    ///
    /// The state machine owns visibility (`overlay_up`, set from
    /// [`crate::app::event::HotkeyMsg`]); this only relays the change to the
    /// animator, so the two cannot disagree.
    fn follow_overlay(&mut self) {
        let up = self.app.state.overlay_up;
        if up == self.up {
            return;
        }
        self.up = up;
        if up {
            self.parked = false;
            self.frame.motion.summon();
        } else {
            self.frame.motion.dismiss();
        }
    }

    /// Stages the update, hands it to the helper, and requests a quit.
    ///
    /// Lives here rather than in the state machine because every step is I/O.
    /// The quit itself goes through the normal route.
    fn install_update(&mut self, now: Instant) {
        let state = &self.app.state;
        let Some(crate::update::Found::Available { manifest, msi, .. }) = &state.update else {
            return;
        };

        let staged =
            crate::update::apply::stage(manifest, msi, state.settings.cache_dir.as_deref())
                .and_then(|staged| {
                    crate::update::apply::hand_over(&staged)?;
                    Ok(staged)
                });

        match staged {
            Ok(_) => {
                // The normal quit stops the workers and writes the index, as
                // Ctrl+Q does. The helper waits for this process to exit.
                self.app.state.should_quit = true;
            }
            // Nothing was installed or closed; report the failure.
            Err(problem) => self.app.feed(
                AppEvent::Open(crate::app::event::OpenMsg::SettingSaveFailed {
                    label: "The update",
                    detail: problem.detail(),
                }),
                now,
            ),
        }
    }

    /// Hides the window once the exit animation has finished.
    ///
    /// Deferred to the end of the exit so the animation is visible. The hotkey
    /// thread does the hiding, because returning the keyboard to the previous
    /// window is a foreground change.
    fn park(&mut self) {
        if !self.frame.motion.is_hidden() || self.parked {
            return;
        }
        self.parked = true;
        self.app.actors.dismiss_overlay();
    }

    /// Moves the panel with the pointer and saves where it was left.
    ///
    /// `response` covers this frame's drag handle: the header and footer, or
    /// the whole panel with Alt held. Whether it was registered before or after
    /// [`panel::show`] sets precedence, because egui gives a press to the last
    /// widget registered at that point. See [`panel::handles`].
    ///
    /// The arithmetic lives in [`drag::Drag`] so it can be tested.
    fn follow_drag(&mut self, ctx: &egui::Context, response: &egui::Response) {
        if response.drag_started() {
            // The press position, not `hover_pos`, which differs once the
            // pointer leaves the panel.
            let grab = response.interact_pointer_pos();
            let outer = ctx.input(|i| i.viewport().outer_rect.map(|r| r.min));
            if let (Some(grab), Some(outer)) = (grab, outer) {
                self.drag.begin(grab, outer);
            }
        }

        if response.dragged()
            && let Some(pointer) = ctx.pointer_latest_pos()
            && let Some(at) = self.drag.update(pointer)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
        }

        if response.drag_stopped()
            && let Some(at) = self.drag.end()
        {
            self.moved_to(ctx, at);
        }
    }

    /// Records where a drag left the panel, in device pixels.
    ///
    /// Takes points (egui's unit) and stores pixels (the unit of
    /// `SetWindowPos` and `GetMonitorInfoW`), converting once here. See
    /// [`crate::hotkey::geometry::RectPx`].
    fn moved_to(&mut self, ctx: &egui::Context, at: egui::Pos2) {
        let scale = ctx.pixels_per_point();
        let px = ((at.x * scale).round() as i32, (at.y * scale).round() as i32);

        self.app.actors.panel.remember(Some(px));
        if let Some(writer) = &self.placement {
            writer.store(px);
        }
    }

    /// Re-reads the configuration file after the settings window wrote it.
    ///
    /// The whole file is reloaded, since the file is the only state the two
    /// processes share. `AppState::adopt` is exhaustive over `SettingKey`, so
    /// a new key without handling fails to compile.
    fn adopt_config(&mut self, now: Instant) {
        let choice = self.config.clone();
        let Ok(mut fresh) = crate::config::Settings::load(&choice) else {
            // A file that fails to parse was hand-edited; keep the loaded
            // settings. The next start reports the error.
            return;
        };
        // Command-line pins belong to this process, not to the file.
        fresh.cli_pinned = self.app.state.settings.cli_pinned;
        // Probed here because the state machine does no I/O, and only when
        // the viewer list changed.
        let probe = (fresh.viewers != self.app.state.settings.viewers)
            .then(|| crate::open::probe_viewers(&fresh.viewers));
        self.app.feed(AppEvent::Adopt(Box::new(fresh)), now);
        if let Some(missing) = probe {
            self.app.state.set_viewers_missing(missing);
        }
    }

    /// Returns the panel to automatic placement.
    ///
    /// Clears both the in-memory slot (read by the next summon) and the file
    /// (read by the next session).
    fn forget_placement(&mut self) {
        self.app.actors.panel.remember(None);
        if let Some(writer) = &self.placement {
            writer.forget();
        }
    }
}

impl eframe::App for Shell {
    /// Fully transparent, so the compositor backdrop shows through.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let wall = SystemTime::now();

        self.follow_theme(ctx);
        // Every frame: one atomic store, and the settings window can change it
        // at any time. The hotkey thread applies it on the next summon.
        self.app.actors.panel.set_dock(self.app.state.settings.dock);

        // Input first, so a character typed this frame is searched this frame.
        // All events pass through, including focus loss.
        for event in ctx.input(input::translate) {
            self.app.feed(event, now);
        }

        // The returned redraw request is ignored: `ui` runs after every
        // `logic` while visible, and there is nothing to draw while hidden.
        let _ = self.app.pump(now);

        self.serve_requests(now);
        #[cfg(windows)]
        self.follow_update();
        self.follow_overlay();

        // `next_deadline` is anchored on the last frame, so a turn without
        // drawing must still record one; otherwise the age readout's deadline
        // stays in the past and the loop spins.
        self.app.state.note_frame(now, wall);

        if self.app.should_quit() {
            // Notify an open settings window before closing, so it disables
            // its panel-dependent controls immediately.
            self.link.exiting();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Repaint at the earlier of the animation tick and the next deadline.
        // With neither, no repaint is requested and the process sleeps until
        // the hotkey or a worker wakes it. Capping at `ANIMATION_TICK` alone
        // would let a due search wait up to an extra tick.
        let animating = self
            .frame
            .motion
            .is_animating()
            .then_some(crate::config::ANIMATION_TICK);
        let deadline = self
            .app
            .next_deadline()
            .map(|due| due.saturating_duration_since(now));

        if let Some(wait) = [animating, deadline].into_iter().flatten().min() {
            // At least 1 ms: `request_repaint_after(ZERO)` repaints
            // immediately, so a past deadline would pin a core.
            ctx.request_repaint_after(wait.max(Duration::from_millis(1)));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let wall = SystemTime::now();

        // Advance state, then time, then decide what to draw, in the order
        // `frame` defines so the tests use the same one.
        let dt = ui.input(|i| i.stable_dt);
        let content = self.frame.advance(&self.app.state, dt);

        self.park();

        // Messages from the settings window, each needing this process.
        for msg in self.link.poll() {
            match msg {
                crate::ipc::ToPanel::Changed => self.adopt_config(now),
                crate::ipc::ToPanel::InstallUpdate => self.install_update(now),
                crate::ipc::ToPanel::ForgetPlacement => self.forget_placement(),
                // Already noted by `link.open()`.
                crate::ipc::ToPanel::Closing => {}
            }
        }

        // The panel also writes the configuration file (F2, F5 and window
        // toggles via `Cmd::SaveSetting`). The settings window is told only
        // after a write succeeds.
        if self.app.take_saved() {
            self.link.reload();
        }

        // Live state that only this process knows.
        self.link.live(
            self.app.actors.panel.remembered(),
            &self.app.state.hotkey_claim,
        );

        // Drag handles. The header and footer are registered before the panel,
        // so its controls take their own presses. With Alt held the whole
        // panel is registered after, so it takes every press.
        //
        // No dragging while docked: the next summon would undo the move.
        let docked = self.app.state.settings.dock.is_docked();
        let alt = ui.input(|i| i.modifiers.alt);
        let sense = egui::Sense::click_and_drag();
        let (header, footer) = panel::handles(ui.max_rect());
        let chrome = (!alt && !docked).then(|| {
            let top = ui.interact(header, egui::Id::new("files-chrome-header"), sense);
            let bottom = ui.interact(footer, egui::Id::new("files-chrome-footer"), sense);
            top.union(bottom)
        });

        let intents = panel::show(
            ui,
            &self.app.state,
            &self.theme,
            content,
            self.backdrop,
            wall,
        );

        if !docked {
            let chrome = match chrome {
                Some(chrome) => chrome,
                None => ui.interact(ui.max_rect(), egui::Id::new("files-chrome-all"), sense),
            };
            self.follow_drag(&ui.ctx().clone(), &chrome);
        }

        // Fed directly rather than sent through the channel, which would delay
        // them a frame and make hover lag the pointer.
        //
        // Dropped during a drag, so the pointer crossing rows does not move the
        // selection.
        for intent in intents {
            if self.drag.is_dragging() {
                break;
            }
            self.app.feed(AppEvent::Intent(intent), now);
        }
        let requested = self.app.take_window_requests();
        if requested.settings {
            self.link.toggle(&self.app.state.settings);
        }
        let _ = self.app.pump(now);
    }
}
