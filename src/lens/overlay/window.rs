//! The sheet of glass, and the frame loop that drives it.
//!
//! One window covering the whole virtual desktop: transparent, always on top,
//! never activated, and click-through until the modifier is held. Built on
//! `eframe` the same way [`crate::gui`] is, and for the same reason - a
//! transparent window that composites correctly with the desktop is most of
//! what makes this look like part of the operating system rather than a
//! rectangle a program drew.
//!
//! Everything decided here was decided somewhere else. [`Selection`] says
//! whether the mouse is caught and what the cursor is; [`Viewport`] says where
//! a pointer position actually is. This file converts those answers into
//! `WS_EX_TRANSPARENT`, a `CursorIcon` and some painted rectangles, and holds no
//! opinion of its own.

use eframe::egui;

use crate::lens::modifier::{self, Modifier};
use crate::lens::overlay::hit::BoxMap;
use crate::lens::overlay::platform;
use crate::lens::overlay::select::{Cursor, Selection};
use crate::lens::overlay::view::Viewport;
use crate::lens::px::{Point, Rect, Screen};

/// What the user asked for, once a selection exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Point 12.
    Copy(String),
    /// Point 13.
    Search(String),
}

/// Something to do with a finished selection. Called on the frame thread.
pub type OnAction = Box<dyn FnMut(Action)>;

/// Runs the overlay until the user quits, or until `quit_after` elapses.
///
/// The deadline is a safety valve rather than a feature, and it earns its place
/// on the first run of any change to [`platform::dress`]. This window is
/// full-screen, always on top, and one style bit away from catching every click
/// on the desktop; if that bit is ever wrong there is no window to close and no
/// Alt-Tab entry to reach, and the way out is Task Manager. A deadline means a
/// mistake lasts a known number of seconds instead of until somebody works that
/// out.
pub fn run(
    map: BoxMap,
    chord: modifier::Chord,
    quit_after: Option<std::time::Duration>,
    diagnose: bool,
    on_action: OnAction,
) -> eframe::Result<()> {
    // Built here rather than taken from the creation context, because the
    // modifier watcher needs something to wake before there is a window to draw
    // in - the same ordering problem `gui::run` solves the same way.
    let ctx = egui::Context::default();

    let wake = {
        let ctx = ctx.clone();
        std::sync::Arc::new(move || ctx.request_repaint())
    };
    let watcher = modifier::watch(chord, wake).ok().flatten();

    let screen = platform::virtual_screen();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("lens")
            .with_app_id("lens")
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_always_on_top()
            .with_taskbar(false)
            // Placed in physical pixels here and corrected on the first frame,
            // once the real scale factor is known. See `place`.
            .with_position([screen.left as f32, screen.top as f32])
            .with_inner_size([screen.width() as f32, screen.height() as f32]),
        // The DirectComposition swapchain. Without it the window is opaque;
        // see `crate::gpu`.
        wgpu_options: crate::gpu::transparent_config(),
        persist_window: false,
        ..Default::default()
    };

    let build = move |cc: &eframe::CreationContext<'_>| {
        let overlay = Overlay::new(cc, map, watcher, quit_after, diagnose, on_action);
        Ok(Box::new(overlay) as Box<dyn eframe::App>)
    };

    eframe::run_native_ext("lens", options, Some(ctx), Box::new(build))
}

struct Overlay {
    sel: Selection,
    /// `None` when the platform cannot say - off Windows, or with the chord
    /// switched off. The overlay then never arms, which is the honest
    /// behaviour rather than arming permanently.
    watcher: Option<Modifier>,
    screen: Rect<Screen>,
    hwnd: Option<isize>,
    /// The last style bit written, so it is written only on a change.
    catching: bool,
    /// The scale the window was last placed for. Replacing it is how a DPI
    /// change is followed.
    placed_for: Option<f32>,
    on_action: OnAction,
    /// When to close regardless. See [`run`].
    deadline: Option<std::time::Instant>,
    /// Paint a known opaque marker over the left half of the overlay.
    ///
    /// The one experiment that distinguishes "our pixels never reach the
    /// screen" from "they do, and the background behind them is opaque" - the
    /// two hypotheses that look identical from outside.
    diagnose: bool,
    /// Where the action chips were drawn last frame, so a press on one is not
    /// also the start of a drag.
    chips: Vec<(egui::Rect, Action)>,
}

impl Overlay {
    fn new(
        cc: &eframe::CreationContext<'_>,
        map: BoxMap,
        watcher: Option<Modifier>,
        quit_after: Option<std::time::Duration>,
        diagnose: bool,
        on_action: OnAction,
    ) -> Self {
        // Deliberately *not* `gui::hwnd_of(cc)`. That handle is not the
        // overlay: measured on a running build, the styles it dressed landed on
        // a 16x16 helper window while the real one kept `WS_EX_APPWINDOW` and
        // no `WS_EX_TRANSPARENT` at all - so the overlay was in the taskbar,
        // took the focus, and swallowed every click on the desktop. The root
        // window is reached through `Frame::winit_window`, which is only
        // available once there are frames, so dressing happens in `logic`.

        // The panel must not paint a background of its own; the window is a
        // sheet of glass and anything opaque in it hides the desktop.
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::TRANSPARENT;
        visuals.window_fill = egui::Color32::TRANSPARENT;
        visuals.extreme_bg_color = egui::Color32::TRANSPARENT;
        cc.egui_ctx.set_visuals(visuals);

        Self {
            sel: Selection::new(map),
            watcher,
            screen: platform::virtual_screen(),
            hwnd: None,
            catching: false,
            placed_for: None,
            on_action,
            deadline: quit_after.map(|d| std::time::Instant::now() + d),
            diagnose,
            chips: Vec::new(),
        }
    }

    /// The window's own geometry, in the units egui works in.
    fn viewport(&self, ctx: &egui::Context) -> Viewport {
        Viewport::new(
            Point::new(self.screen.left, self.screen.top),
            ctx.pixels_per_point(),
        )
    }

    /// Puts the window over the whole virtual desktop.
    ///
    /// Done from inside the frame rather than in the builder because the scale
    /// factor is not known until there is a window to ask, and a window sized
    /// in logical points before that is sized against a guess. Re-run whenever
    /// the scale changes, which is how a drag to a monitor at another DPI is
    /// followed.
    fn place(&mut self, ctx: &egui::Context) {
        let scale = ctx.pixels_per_point();
        if self.placed_for == Some(scale) {
            return;
        }
        self.placed_for = Some(scale);
        let vp = self.viewport(ctx);
        let (w, h) = vp.size_in_points(self.screen.width(), self.screen.height());
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            self.screen.left as f32 / scale,
            self.screen.top as f32 / scale,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
    }

    /// Finds the real overlay window and dresses it, once.
    ///
    /// Verified rather than assumed: `dress` is a write, and a write to the
    /// wrong window looks exactly like a write that worked until somebody reads
    /// the desktop back. If the styles do not take, that is said out loud - an
    /// overlay that is silently not click-through is one that has taken the
    /// machine away from its owner.
    fn dress(&mut self, frame: &mut eframe::Frame) {
        if let Some(hwnd) = self.hwnd {
            // Already found. Re-assert only when the toolkit has undone it,
            // which costs one window-long read on the frames where it has not.
            if !platform::is_dressed(hwnd) {
                platform::dress(hwnd);
                // `dress` does not own the click-through bit, so the current
                // mode has to be put back alongside it.
                platform::set_catches_mouse(hwnd, self.catching);
            }
            return;
        }
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let Some(window) = frame.winit_window() else {
            return;
        };
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(win32) = handle.as_raw() else {
            return;
        };
        let hwnd = win32.hwnd.get();
        self.hwnd = Some(hwnd);
        platform::dress(hwnd);
        // Click-through from the first frame: an overlay that catches the mouse
        // before anybody has held the modifier has taken the desktop away.
        platform::set_catches_mouse(hwnd, false);
        if platform::is_dressed(hwnd) {
            log::debug!("overlay dressed: ex-style 0x{:X}", platform::ex_style(hwnd));
        } else {
            log::error!(
                "the overlay window refused its styles (ex-style 0x{:X}); it may sit in the                  taskbar and catch clicks meant for other programs",
                platform::ex_style(hwnd)
            );
        }
    }

    /// Pushes the click-through bit, on a change only.
    fn sync_click_through(&mut self) {
        let want = self.sel.catches_mouse();
        if want == self.catching {
            return;
        }
        self.catching = want;
        if let Some(hwnd) = self.hwnd {
            platform::set_catches_mouse(hwnd, want);
        }
    }
}

impl eframe::App for Overlay {
    /// Fully transparent. Anything opaque and the desktop is hidden behind a
    /// grey sheet, which is the single most obvious way this can go wrong.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.dress(frame);
        if let Some(deadline) = self.deadline {
            if std::time::Instant::now() >= deadline {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            // The overlay is otherwise idle - nothing moves unless the pointer
            // does - so without a repaint request the deadline would be checked
            // only when something else happened to wake the toolkit.
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.place(ctx);
        if let Some(watcher) = &self.watcher {
            self.sel.modifier(watcher.is_held());
        }
        self.sync_click_through();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let vp = self.viewport(&ctx);

        // --- input ---------------------------------------------------------
        //
        // Only while armed. With the window click-through, egui is told nothing
        // anyway, but reading it regardless would let a stale pointer position
        // move a selection the user can no longer see.
        if self.sel.is_armed() {
            let (pos, pressed, released) = ctx.input(|i| {
                (
                    i.pointer.latest_pos(),
                    i.pointer.primary_pressed(),
                    i.pointer.primary_released(),
                )
            });

            if let Some(pos) = pos {
                let on_chip = self.chips.iter().find(|(r, _)| r.contains(pos));
                if pressed && let Some((_, action)) = on_chip {
                    // A press on a chip is that chip, and emphatically not the
                    // start of a new drag - which would clear the selection the
                    // chip is offering to act on.
                    let action = action.clone();
                    (self.on_action)(action);
                } else {
                    let screen = vp.to_screen(pos.x, pos.y);
                    if pressed {
                        self.sel.press(screen);
                    } else {
                        self.sel.pointer_moved(screen);
                    }
                    if released {
                        self.sel.release();
                    }
                }
            }

            ctx.set_cursor_icon(match self.sel.cursor() {
                Cursor::IBeam => egui::CursorIcon::Text,
                Cursor::Arrow => egui::CursorIcon::Default,
            });
        }

        // --- paint ---------------------------------------------------------
        let painter = ui.painter();

        if self.diagnose {
            // Opaque magenta over the left half, nothing over the right. Two
            // readings off one screenshot, needing no "before" capture and no
            // assumption that the desktop underneath held still.
            let full = ui.max_rect();
            let half = egui::Rect::from_min_max(full.min, egui::pos2(full.center().x, full.max.y));
            painter.rect_filled(half, 0.0, egui::Color32::from_rgb(255, 0, 255));
        }
        let to_rect = |r: Rect<Screen>| {
            let (l, t, right, b) = vp.to_points(r);
            egui::Rect::from_min_max(egui::pos2(l, t), egui::pos2(right, b))
        };

        for rect in self.sel.highlight() {
            painter.rect_filled(to_rect(rect), 2.0, SELECTION);
        }

        self.chips.clear();
        if self.sel.has_selection() {
            let anchor = self
                .sel
                .highlight()
                .last()
                .map(|r| to_rect(*r))
                .unwrap_or(egui::Rect::NOTHING);
            self.chips = paint_chips(painter, &ctx, anchor, &self.sel.text());
        }
    }
}

/// The selection wash. Blue, translucent, and the same blue every other text
/// control on the machine uses - a selection that does not look like a
/// selection is a selection nobody recognises.
const SELECTION: egui::Color32 = egui::Color32::from_rgba_premultiplied(0x2C, 0x5A, 0x8C, 0x80);
const CHIP_BG: egui::Color32 = egui::Color32::from_rgba_premultiplied(0x14, 0x18, 0x20, 0xE8);
const CHIP_FG: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xF0, 0xFF);

/// Draws the action chips under a selection, and reports where they went.
///
/// Below the selection rather than above it, and offset by a few points, so the
/// chips never sit over the text the user is still reading.
fn paint_chips(
    painter: &egui::Painter,
    ctx: &egui::Context,
    anchor: egui::Rect,
    text: &str,
) -> Vec<(egui::Rect, Action)> {
    const PAD: f32 = 6.0;
    const GAP: f32 = 4.0;

    let mut out = Vec::new();
    let mut x = anchor.left();
    let y = anchor.bottom() + GAP;

    for (label, action) in [
        ("Copy", Action::Copy(text.to_owned())),
        ("Search", Action::Search(text.to_owned())),
    ] {
        let galley =
            painter.layout_no_wrap(label.to_owned(), egui::FontId::proportional(13.0), CHIP_FG);
        let size = galley.size() + egui::vec2(PAD * 2.0, PAD);
        let rect = egui::Rect::from_min_size(egui::pos2(x, y), size);
        painter.rect_filled(rect, 4.0, CHIP_BG);
        painter.galley(rect.min + egui::vec2(PAD, PAD / 2.0), galley, CHIP_FG);
        out.push((rect, action));
        x = rect.right() + GAP;
    }

    // Keeps the borrow checker honest about `ctx` being used - and is worth
    // doing anyway: the chips are new geometry, so the frame they appear on is
    // a frame that has to be drawn.
    ctx.request_repaint();
    out
}
