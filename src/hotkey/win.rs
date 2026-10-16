//! The Windows hotkey thread (`files-hotkey`): message pump and window calls.
//!
//! See [`super`] for the threading constraints. There is no `unsafe impl Send`
//! here: `HWND` is `!Send`, so the panel's handle crosses threads as an
//! `isize` through [`Panel`] and is rebuilt into an `HWND` only in this file.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicIsize, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, GetLastError, HWND};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HWND_TOPMOST, IsWindow, MSG,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetForegroundWindow, SetWindowPos, ShowWindow, WM_APP, WM_HOTKEY, WM_QUIT, WM_USER,
};

use super::geometry::{self, RectPx};
use super::spec::{Hotkey, HotkeySpec};
use super::{Probe, win_hwnd};
use crossbeam_channel::Sender;

use crate::app::event::{AppEvent, Events, HotkeyMsg};
use crate::config::Dock;

/// Starts the dismissal (Escape or a second hotkey press).
///
/// Distinct from [`WM_APP_HIDE`]: this only reports the dismissal, leaving the
/// window on screen so the drawing thread can play the exit transition.
const WM_APP_DISMISS: u32 = WM_APP + 3;

/// Hides the window; sent by the drawing thread after the exit transition.
const WM_APP_HIDE: u32 = WM_APP + 1;

/// Shows the panel on request from the tray icon or a second instance.
///
/// Routed through this thread because showing the panel takes the foreground.
const WM_APP_SUMMON: u32 = WM_APP + 2;

/// Hotkey id; ids are per thread and this thread registers only one.
const HOTKEY_ID: i32 = 1;

/// Upper bound on how long `spawn` waits for the registration result.
const READY_TIMEOUT: Duration = Duration::from_millis(500);

/// Panel window state shared between the drawing thread and this one.
///
/// The handle is stored as an `isize` because `HWND` is `!Send`. Zero means
/// the window does not exist yet, and a hotkey press then does nothing.
#[derive(Debug)]
pub struct Panel {
    hwnd: AtomicIsize,
    /// The last dragged-to position in device pixels, packed as two `i32`s.
    ///
    /// A single atomic avoids torn reads that mix two positions' halves.
    /// [`NOWHERE`] means no position is remembered.
    at: AtomicI64,
    /// The [`Dock`] setting as its index in [`Dock::ALL`]. Published by the
    /// drawing thread every frame; read here on each summon.
    dock: AtomicU8,
}

/// Sentinel for "no remembered position" in [`Panel::at`].
///
/// `i64::MIN` is the packed form of `(i32::MIN, 0)`; [`COORD_LIMIT`] keeps
/// every real position from packing to it.
const NOWHERE: i64 = i64::MIN;

/// Coordinates are clamped to `-COORD_LIMIT..=COORD_LIMIT` before packing.
///
/// Far beyond any real desktop, and it keeps the packed high half away from
/// `i32::MIN` so no position equals [`NOWHERE`].
const COORD_LIMIT: i32 = 1_000_000;

impl Default for Panel {
    fn default() -> Self {
        Self {
            hwnd: AtomicIsize::new(0),
            at: AtomicI64::new(NOWHERE),
            dock: AtomicU8::new(0),
        }
    }
}

impl Panel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records the panel window handle once the toolkit has created it.
    pub fn publish(&self, hwnd: isize) {
        self.hwnd.store(hwnd, Ordering::Release);
    }

    /// Records the panel position after a drag, or at startup from
    /// [`crate::placement::load`].
    ///
    /// `None` restores the default placement from [`geometry::place`].
    pub fn remember(&self, at: Option<(i32, i32)>) {
        let packed = match at {
            Some((left, top)) => {
                let left = left.clamp(-COORD_LIMIT, COORD_LIMIT);
                let top = top.clamp(-COORD_LIMIT, COORD_LIMIT);
                let packed = ((left as i64) << 32) | (top as u32 as i64);
                debug_assert_ne!(packed, NOWHERE, "a real position packed to the sentinel");
                packed
            }
            None => NOWHERE,
        };
        self.at.store(packed, Ordering::Release);
    }

    /// Records the dock setting; called by the drawing thread each frame.
    pub fn set_dock(&self, dock: Dock) {
        let index = Dock::ALL.iter().position(|&d| d == dock).unwrap_or(0);
        self.dock.store(index as u8, Ordering::Release);
    }

    /// Returns the dock mode the next summon will use.
    pub fn dock(&self) -> Dock {
        let index = usize::from(self.dock.load(Ordering::Acquire));
        Dock::ALL.get(index).copied().unwrap_or_default()
    }

    /// Returns the remembered position, if any, for `--doctor` and Settings.
    pub fn remembered(&self) -> Option<(i32, i32)> {
        self.placed()
    }

    fn get(&self) -> Option<HWND> {
        let bits = self.hwnd.load(Ordering::Acquire);
        (bits != 0).then_some(bits as HWND)
    }

    fn placed(&self) -> Option<(i32, i32)> {
        let packed = self.at.load(Ordering::Acquire);
        if packed == NOWHERE {
            return None;
        }
        Some(((packed >> 32) as i32, packed as u32 as i32))
    }
}

enum Ready {
    Registered { tid: u32 },
    Failed(u32),
}

/// Handle to the hotkey thread.
///
/// Joinable: a posted `WM_QUIT` wakes its `GetMessageW`.
pub struct HotkeyThread {
    tid: u32,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl HotkeyThread {
    /// Starts dismissing the panel.
    ///
    /// The window stays on screen until the exit transition has played and
    /// [`Self::hide`] is called.
    pub fn dismiss(&self, handing_over: bool) {
        self.post(WM_APP_DISMISS, usize::from(handing_over), 0);
    }

    /// Hides the window once the exit transition has finished.
    pub fn hide(&self) {
        self.post(WM_APP_HIDE, 0, 0);
    }

    /// Shows the panel as the hotkey would. Does nothing if it is already up.
    pub fn summon(&self) {
        self.post(WM_APP_SUMMON, 0, 0);
    }

    fn post(&self, msg: u32, w: usize, l: isize) {
        // SAFETY: posting to a thread id. A dead thread returns zero, which is
        // ignored because the listener has already stopped.
        unsafe { PostThreadMessageW(self.tid, msg, w, l) };
    }

    /// Stops the thread, within `budget`.
    pub fn shutdown(&mut self, budget: Duration) -> bool {
        let Some(handle) = self.handle.take() else {
            return true;
        };
        self.post(WM_QUIT, 0, 0);
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if handle.is_finished() {
                return handle.join().is_ok();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // Abandon the thread, as other workers do. Not expected: the WM_QUIT
        // post wakes GetMessageW.
        false
    }
}

impl Drop for HotkeyThread {
    fn drop(&mut self) {
        let _ = self.shutdown(super::SHUTDOWN_BUDGET);
    }
}

pub fn spawn(
    spec: HotkeySpec,
    events: Events,
    panel: Arc<Panel>,
) -> std::io::Result<Option<HotkeyThread>> {
    let Some(hk) = spec.bound() else {
        return Ok(None);
    };

    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Ready>(1);
    let tx = events.clone();
    let handle = std::thread::Builder::new()
        .name("files-hotkey".into())
        .spawn(move || {
            // Report a panic instead of letting the thread die silently.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pump(hk, &tx, &ready_tx, &panel);
            }));
            if let Err(payload) = result {
                let _ = tx.send(AppEvent::ActorDied {
                    actor: "hotkey",
                    detail: crate::util::once::panic_detail(&payload),
                });
            }
        })?;

    match ready_rx.recv_timeout(READY_TIMEOUT) {
        Ok(Ready::Registered { tid }) => {
            // Recorded rather than re-asked later. See `HotkeyMsg::Claimed`.
            let _ = events.send(AppEvent::Hotkey(HotkeyMsg::Claimed));
            Ok(Some(HotkeyThread {
                tid,
                handle: Some(handle),
            }))
        }
        Ok(Ready::Failed(code)) => {
            let _ = events.send(AppEvent::Hotkey(HotkeyMsg::Unavailable {
                reason: registration_detail(hk, code),
            }));
            Ok(None)
        }
        Err(_) => {
            let _ = events.send(AppEvent::Hotkey(HotkeyMsg::Unavailable {
                reason: "The hotkey listener did not start".into(),
            }));
            Ok(None)
        }
    }
}

fn registration_detail(hk: Hotkey, code: u32) -> String {
    let chord = super::spec::describe(hk);
    if code == ERROR_HOTKEY_ALREADY_REGISTERED {
        // Does not name the owner: Windows itself claims several chords (such
        // as shift+win+f23 for the Copilot key), so any guess would mislead.
        format!(
            "{chord} is already claimed \u{b7} set hotkey in config.toml to another \
             combination, or to \"off\""
        )
    } else {
        format!("{chord} could not be registered (error {code})")
    }
}

fn pump(hk: Hotkey, tx: &Events, ready: &Sender<Ready>, panel: &Panel) {
    // Create the message queue before publishing the thread id; until then
    // PostThreadMessageW fails with ERROR_INVALID_THREAD_ID and a hide would
    // be lost.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: a null window filters to this thread's own messages, and
    // PM_NOREMOVE leaves the queue alone. Called only to create the queue.
    unsafe {
        PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        )
    };

    // SAFETY: a null window means the hotkey arrives as a thread message, so
    // no window or WndProc is needed.
    if unsafe { RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, hk.mods, hk.vk as u32) } == 0 {
        // SAFETY: no arguments.
        let _ = ready.send(Ready::Failed(unsafe { GetLastError() }));
        return;
    }
    // SAFETY: no arguments.
    let tid = unsafe { GetCurrentThreadId() };
    let _ = ready.send(Ready::Registered { tid });

    let mut summoner = Summoner::default();

    // GetMessageW returns 0 on WM_QUIT and -1 on error; `> 0` exits on both.
    // SAFETY: `msg` is a live local; a null window filter means this thread's
    // messages, which includes the thread-targeted WM_HOTKEY.
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        match msg.message {
            WM_HOTKEY if msg.wParam as i32 == HOTKEY_ID => summoner.toggle(tx, panel),
            WM_APP_DISMISS => summoner.dismiss(tx, msg.wParam != 0),
            WM_APP_HIDE => summoner.hide(panel),
            WM_APP_SUMMON if !summoner.shown => summoner.summon(tx, panel),
            _ => {}
        }
    }

    // SAFETY: unregistering the id this thread registered.
    unsafe { UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID) };
}

/// Panel visibility state, owned by the hotkey thread.
#[derive(Default)]
struct Summoner {
    /// Whether the panel is up. Authoritative; the UI's view follows it.
    shown: bool,
    /// The previously focused window as bits, restored on hide.
    prev: isize,
    /// The panel is closing so a viewer can take the foreground.
    ///
    /// Set on dismiss and read on hide, which arrive as separate messages.
    /// See [`Self::hide`].
    handing_over: bool,
}

impl Summoner {
    fn toggle(&mut self, tx: &Events, panel: &Panel) {
        if self.shown {
            // A second press dismisses like Escape and restores the previous
            // foreground window.
            self.dismiss(tx, false);
        } else {
            self.summon(tx, panel);
        }
    }

    /// Reports that the panel is closing, without hiding it.
    ///
    /// The drawing thread plays the exit transition, then posts `WM_APP_HIDE`.
    /// Idempotent.
    fn dismiss(&mut self, tx: &Events, handing_over: bool) {
        if !self.shown {
            return;
        }
        self.shown = false;
        // Stored for `hide`, which arrives later without this context.
        self.handing_over = handing_over;
        let _ = tx.send(AppEvent::Hotkey(HotkeyMsg::Dismissed));
    }

    fn summon(&mut self, tx: &Events, panel: &Panel) {
        let Some(hwnd) = panel.get() else {
            // The window does not exist yet (early startup); ignore the press.
            return;
        };
        self.shown = true;

        // SAFETY: every call below takes a window handle this process owns,
        // checked live, plus locals for the out parameters. Each reports
        // failure by return value rather than by writing through a bad
        // pointer.
        unsafe {
            if IsWindow(hwnd) == 0 {
                return;
            }

            // Capture the window to restore focus to, excluding the panel.
            let fg = GetForegroundWindow();
            if !fg.is_null() && fg != hwnd {
                self.prev = fg as isize;
            }

            let dock = panel.dock();
            let rect = place(hwnd, panel.placed(), dock);
            // Set before showing, so the window never appears with the wrong
            // corners.
            crate::gui::window::set_rounded(hwnd as isize, !dock.is_docked());
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                // Show without activating; `force_foreground` below takes the
                // keyboard, which `SWP_SHOWWINDOW` alone does not.
                SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
        }

        self.force_foreground(hwnd);
        let _ = tx.send(AppEvent::Hotkey(HotkeyMsg::Summoned));
    }

    /// Hides the panel and restores the previous foreground window.
    fn hide(&mut self, panel: &Panel) {
        self.shown = false;
        let Some(hwnd) = panel.get() else {
            return;
        };
        // SAFETY: handles owned by this process, checked live; a stale `prev`
        // is rejected by `IsWindow` rather than dereferenced.
        unsafe {
            ShowWindow(hwnd, SW_HIDE);

            // Restore the previous foreground explicitly; hiding alone does not
            // always do it, and keystrokes can then go to the desktop.
            //
            // Skip this when a viewer is starting: it would win the race and
            // open the viewer behind. The viewer instead takes the foreground
            // via `AllowSetForegroundWindow` (see `src/open/launch.rs`).
            let prev = self.prev as HWND;
            if !self.handing_over && !prev.is_null() && IsWindow(prev) != 0 {
                SetForegroundWindow(prev);
            }
        }
        self.prev = 0;
        self.handing_over = false;
    }

    /// Takes the foreground, including from another thread's window.
    ///
    /// Attaches input queues, since `SetForegroundWindow` is otherwise refused
    /// across threads. Only permitted while handling `WM_HOTKEY`.
    fn force_foreground(&self, hwnd: HWND) {
        // SAFETY: attach and detach are paired unconditionally, and both take
        // thread ids obtained from live windows.
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_null() {
                SetForegroundWindow(hwnd);
                return;
            }
            let other = GetWindowThreadProcessId(fg, std::ptr::null_mut());
            let mine = GetCurrentThreadId();
            if other == 0 || other == mine {
                SetForegroundWindow(hwnd);
                return;
            }
            AttachThreadInput(mine, other, 1);
            SetForegroundWindow(hwnd);
            AttachThreadInput(mine, other, 0);
        }
    }
}

/// Returns the panel rectangle for this summon.
///
/// The size is computed from [`crate::gui::PANEL_W`] by
/// [`crate::gui::PANEL_H`] points every time, not read from the window, since
/// a previous docked summon may have resized it.
///
/// A remembered position `at` uses the work area of the monitor containing
/// `at`, not the window's monitor: on the first summon the window may still
/// be on another screen. Docked and default placements use the window's
/// monitor.
fn place(hwnd: HWND, at: Option<(i32, i32)>, dock: Dock) -> RectPx {
    let want = natural_px(hwnd);

    let edge = match dock {
        Dock::Free => None,
        Dock::Top => Some(geometry::Edge::Top),
        Dock::Bottom => Some(geometry::Edge::Bottom),
    };
    if let Some(edge) = edge {
        let work = win_hwnd::work_area(hwnd).unwrap_or(RectPx::new(0, 0, 1920, 1080));
        return geometry::place_docked(work, want.1, edge);
    }

    match at {
        Some(at) => {
            let work = win_hwnd::work_area_at(at).unwrap_or(RectPx::new(0, 0, 1920, 1080));
            geometry::place_at(work, want, at)
        }
        None => {
            let work = win_hwnd::work_area(hwnd).unwrap_or(RectPx::new(0, 0, 1920, 1080));
            geometry::place(work, want)
        }
    }
}

/// Returns the panel size in device pixels for the window's current monitor.
///
/// If the window moves to a monitor with a different DPI, Windows notifies the
/// toolkit, which resizes to match.
fn natural_px(hwnd: HWND) -> (i32, i32) {
    // SAFETY: a window this process owns, checked live by the caller. Zero
    // means the call failed, and is replaced by the unscaled 96.
    let dpi = match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        dpi => dpi,
    };
    let scale = dpi as f32 / 96.0;
    (
        (crate::gui::PANEL_W * scale).round() as i32,
        (crate::gui::PANEL_H * scale).round() as i32,
    )
}

pub fn probe(spec: HotkeySpec, known: Option<Result<(), String>>) -> Probe {
    let chord = spec.bound().map(super::spec::describe);
    // Prefer the caller's known result; see `super::Probe` for why a fresh
    // registration is wrong inside the running program.
    let registered = match known {
        Some(answer) => spec.bound().map(|_| answer),
        None => spec.bound().map(|hk| {
            // SAFETY: registered and immediately released on the caller's
            // thread, so the key is never left claimed.
            unsafe {
                if RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, hk.mods, hk.vk as u32) == 0 {
                    let code = GetLastError();
                    Err(registration_detail(hk, code))
                } else {
                    UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
                    Ok(())
                }
            }
        }),
    };
    // Report where the default placement would land; a null window resolves
    // to the nearest monitor.
    let window = win_hwnd::work_area(std::ptr::null_mut()).map(|work| {
        let rect = geometry::place(work, (720, 300));
        Ok(format!(
            "its own window, placed at {},{} on a {}x{} work area",
            rect.left,
            rect.top,
            work.width(),
            work.height()
        ))
    });
    Probe {
        chord,
        supported: true,
        registered,
        window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Negative cases guard against sign-extending `top` when packing.
    #[test]
    fn a_remembered_position_survives_the_round_trip() {
        let panel = Panel::default();
        assert_eq!(panel.placed(), None, "a fresh panel remembers nothing");

        for at in [
            (0, 0),
            (1234, 56),
            // A monitor to the left of, or above, the primary one.
            (-1920, 200),
            (100, -1080),
            (-1920, -1080),
            (COORD_LIMIT, -COORD_LIMIT),
        ] {
            panel.remember(Some(at));
            assert_eq!(panel.placed(), Some(at), "{at:?} did not survive");
        }
    }

    /// Without [`COORD_LIMIT`], `(i32::MIN, 0)` would pack to [`NOWHERE`].
    #[test]
    fn no_position_is_mistaken_for_no_position_at_all() {
        let panel = Panel::default();
        for at in [
            (i32::MIN, 0),
            (i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX),
            (i32::MIN, i32::MAX),
        ] {
            panel.remember(Some(at));
            assert!(
                panel.placed().is_some(),
                "{at:?} was mistaken for no position at all"
            );
        }
    }

    #[test]
    fn a_coordinate_no_desktop_could_produce_is_brought_back_in_range() {
        let panel = Panel::default();
        panel.remember(Some((i32::MIN, i32::MAX)));
        assert_eq!(panel.placed(), Some((-COORD_LIMIT, COORD_LIMIT)));
    }

    #[test]
    fn a_position_can_always_be_forgotten() {
        let panel = Panel::default();
        panel.remember(Some((i32::MIN, 0)));
        assert!(panel.placed().is_some());

        panel.remember(None);
        assert_eq!(panel.placed(), None);
    }

    #[test]
    fn an_ordinary_desktop_coordinate_is_never_clamped() {
        let panel = Panel::default();
        for left in [-100_000, -1, 0, 1, 100_000] {
            for top in [-100_000, -1, 0, 1, 100_000] {
                panel.remember(Some((left, top)));
                assert_eq!(panel.placed(), Some((left, top)), "{left},{top} moved");
            }
        }
    }
}
