//! The desktop shell.
//!
//! The same engine as the terminal build, drawn in a window of its own. What
//! crosses the boundary is unchanged and was unchanged before this existed:
//! a [`crate::app::App`] holding the state machine and the workers, fed events
//! and pumped once a turn.
//!
//! # Two halves, and why eframe's shape suits this
//!
//! [`eframe::App`] splits a turn into [`eframe::App::logic`] and
//! [`eframe::App::ui`], and - crucially - runs `logic` *even while the window
//! is hidden*, with no egui pass at all. A search tool that lives in the
//! notification area is hidden almost all of the time, so that split is worth
//! having: the index keeps working, a walk that finishes is still recorded,
//! and none of it costs a frame.
//!
//! # Waking
//!
//! The terminal loop blocked on the event channel, so posting an event woke it
//! by construction. Here the toolkit owns the loop and sleeps, so the workers
//! are given an [`crate::app::event::Events`] whose wake is
//! `Context::request_repaint`. Without that, a walk finishing while the window
//! is idle would sit in a channel nobody was waiting on.

pub mod anim;
pub mod fonts;
pub mod frame;
pub mod input;
pub mod overlay;
pub mod row;
pub mod theme;
#[cfg(windows)]
pub mod tray;
pub mod window;
pub mod windows;

use eframe::egui;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::app::App;
use crate::app::event::AppEvent;
use crate::config::Settings;
use crate::index::enumerate::DirSource;
use windows::{Window, Windows};

pub use theme::{PANEL_MAX_H, PANEL_W};

/// What the panel asks the compositor for.
///
/// [`window::Backdrop::Painted`], which is the deliberate answer to something
/// that was measured rather than assumed. The panel is the window - it grows
/// and shrinks to fit its results - and `DWMWA_SYSTEMBACKDROP_TYPE` describes a
/// *region* that does not follow a window as it resizes. On this machine a
/// panel that went from two rows to eight kept the compositor's acrylic over
/// the old rectangle and had none over the rest: two different backgrounds
/// meeting along a horizontal line through the middle of the list. Re-stating
/// the region after the resize lands moves the seam without removing it.
///
/// So the panel paints its own translucent surface instead. It follows the
/// window exactly, because we are the ones drawing it; it can be faded, which
/// acrylic cannot; and it behaves identically in both themes and on every
/// Windows. The cost is a real blur, which is worth less than a panel that is
/// the same colour all the way down.
///
/// The acrylic path is kept, tested and one constant away - it is the right
/// answer for a window that never changes size, which a future help or
/// settings window is.
const WANT_BACKDROP: window::Backdrop = window::Backdrop::Painted;

/// Something outside the frame loop asking for the program's attention.
///
/// The tray icon and a second launch both happen on threads of their own, at
/// moments when a hidden panel has asked for no frames at all. So they post
/// here and then wake the loop - which is [`crate::app::event::Events`] again,
/// for exactly the reason that type exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Show,
    Help,
    Settings,
    Diagnostics,
    Quit,
}

/// Runs the application until the user quits.
pub fn run(settings: Settings, source: Arc<dyn DirSource>) -> eframe::Result<()> {
    // Built here rather than taken from the creation context, because the
    // workers need something to wake *before* there is a window to draw in -
    // and `run_native_ext` exists precisely so the context can outlive that
    // ordering problem.
    let ctx = egui::Context::default();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("files")
            .with_app_id("files")
            // Frameless: the panel paints its own edge. A caption bar on a
            // thing summoned by a hotkey is a thing nobody asked to manage.
            .with_decorations(false)
            // Per-pixel alpha, so the compositor has something to put a
            // backdrop behind.
            .with_transparent(true)
            .with_resizable(false)
            .with_always_on_top()
            // No taskbar button and no Alt-Tab entry. A panel that answers a
            // global hotkey should not also be somewhere you arrive by
            // accident.
            .with_taskbar(false)
            // Hidden until summoned. A tray-resident program that flashes a
            // panel at sign-in is one nobody keeps - and the taskbar bit below
            // is read by the shell when a window is *first shown*, so being
            // hidden here is also what makes `hide_from_taskbar` stick.
            .with_visible(false)
            .with_inner_size([PANEL_W, PANEL_MAX_H]),
        // We place the window ourselves on every summon; restoring the last
        // session's rectangle would fight that.
        persist_window: false,
        // Without this the window is opaque black on Windows whatever
        // `with_transparent` says - wgpu's default DX12 swapchain offers no
        // alpha mode that blends. See [`crate::gpu`], which spells out why.
        wgpu_options: crate::gpu::transparent_config(),
        ..Default::default()
    };

    // Bounded and small: these are user gestures, and a hundred of them queued
    // behind a wedged frame would be a hundred panels' worth of catching up.
    let (requests_tx, requests_rx) = crossbeam_channel::bounded::<Request>(16);
    let post = {
        let ctx = ctx.clone();
        let tx = requests_tx.clone();
        move |request: Request| {
            // Dropped if the queue is full, and the wake only happens when the
            // post did - the same bargain `Events::try_send` makes, and for the
            // same reason: claiming a frame is owed for an event that was
            // discarded is how a loop comes to spin.
            if tx.try_send(request).is_ok() {
                ctx.request_repaint();
            }
        }
    };

    // Before the window, and before the workers. A second copy that has already
    // walked the share and put an icon in the tray has done all of its damage
    // by the time it finds out it is a second copy.
    #[cfg(windows)]
    let _instance = {
        let post = post.clone();
        match crate::single::claim(move || post(Request::Show)) {
            crate::single::Claim::Only(instance) => instance,
            // Not an error. From the user's point of view the launch worked:
            // the panel they wanted is now in front of them.
            crate::single::Claim::AlreadyRunning => return Ok(()),
        }
    };

    let build = {
        let ctx = ctx.clone();
        move |cc: &eframe::CreationContext<'_>| {
            let shell = Shell::new(cc, ctx, settings, source, requests_rx, post)?;
            Ok(Box::new(shell) as Box<dyn eframe::App>)
        }
    };

    eframe::run_native_ext("files", options, Some(ctx), Box::new(build))
}

/// The window handle, as an integer.
///
/// Deliberately not an `HWND`: that type is a raw pointer, so it is neither
/// `Send` nor `Sync`, and keeping the bits rather than the pointer is what
/// lets this be published to the thread that owns the hotkey later without
/// anybody writing an `unsafe impl`. Reconstructing a window from it happens
/// in exactly one module.
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

/// The window, and everything behind it.
struct Shell {
    app: App,
    /// What the compositor agreed to. `None` off Windows, and
    /// [`window::Backdrop::Painted`] on a Windows too old for acrylic - in
    /// which case the panel draws its own background instead of asking for one.
    backdrop: Option<window::Backdrop>,
    /// Kept so the window can be re-dressed when the system theme changes.
    hwnd: Option<isize>,
    /// Whether the real Segoe UI was found. Kept for the diagnostics panel:
    /// "the text looks wrong" is a support call, and this is the answer to it.
    #[allow(dead_code)]
    system_fonts: bool,
    /// The panel's motion, and the size the window was last asked for.
    frame: frame::Frame,
    theme: theme::Theme,
    /// Whether the panel is meant to be up, as of the last frame.
    up: bool,
    /// Gestures from outside the frame loop: the tray, and a second launch.
    requests: crossbeam_channel::Receiver<Request>,
    /// Help, Settings and Diagnostics, which are ordinary windows.
    windows: Windows,
    /// The notification-area icon. Held for the life of the process, because
    /// dropping it takes the icon out of the tray.
    #[cfg(windows)]
    _tray: Option<tray::Tray>,
    /// Whether the window has already been asked to hide for this dismissal.
    parked: bool,
}

impl Shell {
    fn new(
        cc: &eframe::CreationContext<'_>,
        ctx: egui::Context,
        settings: Settings,
        source: Arc<dyn DirSource>,
        requests: crossbeam_channel::Receiver<Request>,
        post: impl Fn(Request) + Clone + Send + Sync + 'static,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // What the configuration asked for, which is usually not what Windows
        // is doing. `system` is still available and still means this.
        let dark = settings
            .theme
            .is_dark(cc.egui_ctx.theme() == egui::Theme::Dark);

        // Dressed before anything is drawn, and - for the taskbar bit - before
        // the window is ever shown, because the shell reads that one once.
        let hwnd = hwnd_of(cc);
        let backdrop = hwnd.map(|hwnd| {
            window::hide_from_taskbar(hwnd);
            window::apply(hwnd, dark, WANT_BACKDROP)
        });
        let system_fonts = fonts::install(&cc.egui_ctx);

        // What the workers call after posting. See the module note.
        let wake = {
            let ctx = ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let app = App::start(settings, source, wake)?;

        // The hotkey thread has been running since `App::start`, waiting to be
        // told which window to summon. This is that.
        if let Some(hwnd) = hwnd {
            app.actors.panel.publish(hwnd);
        }

        // Built on the event loop's own thread, which is where the tray icon's
        // hidden window has to live for its messages to be pumped at all.
        //
        // A tray icon the shell refuses is not a reason to fail: the hotkey
        // still works, and a search tool that will not start because the
        // notification area is full is a worse tool than one without an icon.
        #[cfg(windows)]
        let _tray = tray::Tray::new(move |action| {
            post(match action {
                tray::TrayAction::Show => Request::Show,
                tray::TrayAction::Help => Request::Help,
                tray::TrayAction::Settings => Request::Settings,
                tray::TrayAction::Diagnostics => Request::Diagnostics,
                tray::TrayAction::Quit => Request::Quit,
            });
        })
        .ok();

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
            windows: Windows::default(),
            #[cfg(windows)]
            _tray,
        })
    }

    /// Follows the system between light and dark, window frame included.
    ///
    /// Watched rather than read once: somebody who switches at dusk should not
    /// have to restart the program, and a panel in the wrong theme over a
    /// desktop in the right one is the most obvious way for a window to look
    /// like it does not belong here.
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
        if let Some(hwnd) = self.hwnd {
            self.backdrop = Some(window::apply(hwnd, dark, WANT_BACKDROP));
        }
    }

    /// Does what the tray icon, or a second launch, asked for.
    ///
    /// Drained rather than taken one at a time, for the same reason
    /// [`crate::app::App::pump`] drains: two clicks that arrived between frames
    /// are one turn's worth of work, not two frames' worth.
    fn serve_requests(&mut self) {
        while let Ok(request) = self.requests.try_recv() {
            match request {
                // Through the hotkey thread, because showing the panel means
                // taking the foreground and that is the one thread Windows will
                // accept it from.
                Request::Show => self.app.actors.summon_overlay(),
                Request::Help => self.windows.open(Window::Help),
                Request::Settings => self.windows.open(Window::Settings),
                Request::Diagnostics => self.windows.open(Window::Diagnostics),
                Request::Quit => self.app.state.should_quit = true,
            }
        }
    }

    /// Starts and finishes the entrance and the exit.
    ///
    /// The state machine is the authority on whether the panel is up -
    /// `LayoutMode` is set from [`crate::app::event::HotkeyMsg`], which the
    /// hotkey thread sends because it is the thread Windows will take a
    /// foreground change from. This only notices the change and tells the
    /// animator, which is why a summon and a dismiss cannot get out of step
    /// with what the rest of the program thinks is happening.
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

    /// Asks for the window to be put away, once there is nothing left to see.
    ///
    /// Deferred to the end of the exit transition rather than done when Escape
    /// was pressed, because a window that vanishes on the keystroke has no exit
    /// transition however carefully one was written. The hotkey thread does the
    /// hiding: it also has to hand the keyboard back to whatever the panel took
    /// it from, and that is a foreground change.
    fn park(&mut self) {
        if !self.frame.motion.is_hidden() || self.parked {
            return;
        }
        self.parked = true;
        self.app.actors.dismiss_overlay();
    }

    /// Keeps the window the size the animator asked for.
    ///
    /// The entrance is the *window* arriving, not the content moving inside
    /// it, and that is forced rather than chosen: [`overlay::show`] paints into
    /// `ui.max_rect()`, so the window's height is the panel's height and there
    /// is nowhere else for a transition to happen. Driving the window means the
    /// edge, the surface and the content are one object at every instant.
    ///
    /// The decision itself is [`frame::Frame::resize`], which is pure and is
    /// therefore checkable: a viewport command is a round trip to the window
    /// system and a swapchain reconfigure behind it, and how many of them one
    /// keystroke costs is a number a test can hold us to.
    fn resize(&mut self, ctx: &egui::Context, visual: &anim::Visual) {
        let acrylic = self.backdrop == Some(window::Backdrop::Acrylic);
        if let Some(size) = self.frame.resize(visual, acrylic) {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
    }
}

impl eframe::App for Shell {
    /// Fully transparent: the window is a sheet of glass the panel is painted
    /// on. Anything opaque here and the compositor's backdrop is invisible.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let wall = SystemTime::now();

        self.follow_theme(ctx);

        // Keystrokes first, so a character typed this frame is searched for on
        // this frame rather than on the next one.
        for event in ctx.input(input::translate) {
            self.app.feed(event, now);
        }

        // Immediate mode: `ui` runs after every `logic` the window is visible
        // for, so a redraw is already granted rather than requested. While
        // hidden there is nothing to draw and nothing worth waking for. The
        // value is still what the state-machine tests assert on; it is only
        // here that it has already been satisfied.
        let _ = self.app.pump(now);

        self.serve_requests();
        self.follow_overlay();

        // Every deadline in `next_deadline` is anchored on the last frame, so a
        // turn that does not draw still has to say a turn happened - or the age
        // readout's deadline stays permanently in the past and this becomes a
        // spin rather than a sleep. While the window is hidden nothing is on
        // screen for that readout to be wrong about, so calling it here is
        // honest as well as necessary.
        self.app.state.note_frame(now, wall);

        if self.app.should_quit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // A transition owes a frame immediately; a deadline owes one later.
        // With neither there is no request at all, and the process parks until
        // the hotkey or a worker wakes it - which is the whole of the claim
        // that sitting in the notification area costs nothing.
        if self.frame.motion.is_animating() || self.windows.any_open() {
            ctx.request_repaint();
        } else if let Some(deadline) = self.app.next_deadline() {
            // A floor of a millisecond: `request_repaint_after(ZERO)` means
            // "again immediately", and a deadline already in the past would pin
            // a core.
            let wait = deadline
                .saturating_duration_since(now)
                .max(Duration::from_millis(1));
            ctx.request_repaint_after(wait);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let wall = SystemTime::now();

        // Told what the world looks like, then how much time has passed, then
        // asked what to draw. One order, in one place - and that place is
        // `frame`, so the tests take the same one.
        let dt = ui.input(|i| i.stable_dt);
        let visual = self.frame.advance(&self.app.state, dt);

        self.resize(&ui.ctx().clone(), &visual);
        self.park();

        // Before the panel, so an auxiliary window that wants the keyboard is
        // not fighting a panel that also does.
        {
            let context = ui.ctx().clone();
            let settings = self.app.state.settings.clone();
            let theme = self.theme;
            self.windows
                .show(&context, &theme, &self.app.state, &settings, || {
                    report(&settings)
                });
        }

        let intents = overlay::show(
            ui,
            &self.app.state,
            &self.theme,
            &visual,
            self.backdrop,
            now,
            wall,
        );

        // Fed rather than sent: these were produced on the drawing thread, and
        // a send would go round the channel to arrive one frame later - which
        // for a hover is a highlight that trails the pointer.
        for intent in intents {
            self.app.feed(AppEvent::Intent(intent), now);
        }
        if self.app.take_help_request() {
            self.windows.open(Window::Help);
        }
        let _ = self.app.pump(now);
    }
}

/// The `--doctor` report, as text.
///
/// The same function the console build runs, rendered into a string instead of
/// onto a terminal - so the window and the command line cannot come to disagree
/// about what the program thinks is wrong with itself.
fn report(settings: &Settings) -> String {
    let source = crate::app::actors::default_source(settings);
    let mut out = Vec::new();
    crate::doctor::doctor(settings, source, &mut out);
    String::from_utf8_lossy(&out).into_owned()
}
