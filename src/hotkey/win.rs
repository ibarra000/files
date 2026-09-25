//! The hotkey thread, the message pump, and the window operations.
//!
//! Everything here runs on one thread named `files-hotkey`, and that is load
//! bearing rather than tidy - see the module doc on [`super`]. Note what is
//! *absent*: there is no `unsafe impl Send` anywhere in this file. `HWND` is a
//! raw pointer and so is neither `Send` nor `Sync`, which means the compiler
//! itself guarantees no window handle ever crosses a thread boundary. The panel
//! publishes its window as an `isize` through [`Panel`] for exactly that
//! reason, and it is rebuilt into an `HWND` here and nowhere else.
//!
//! # What the window being ours changed
//!
//! Most of this file used to be about somebody else's window: finding it,
//! confirming it was really the one on screen, remembering the placement it had
//! before we shrank it, and putting all of that back afterwards - including the
//! case where the terminal had been maximised, and the case where the user
//! closed it while the overlay was up. None of that survives. Our window has no
//! previous placement to preserve, cannot be closed behind our back, and does
//! not need finding.
//!
//! What does survive is the part that was never about terminals:
//!
//! * **Foreground rights belong to the keypress, not to the process.** Windows
//!   grants the right to change the foreground window to the process handling a
//!   `WM_HOTKEY`, and only while it is handling it. So `SetForegroundWindow`
//!   still happens here, on this thread, inside that handler - and the drawing
//!   thread still asks rather than acts.
//! * **`PostThreadMessageW` has nothing to post to until a queue exists.** The
//!   pump forces one into existence *before* publishing its thread id.
//! * **No `WndProc`.** `RegisterHotKey` with a null window delivers `WM_HOTKEY`
//!   as a thread message, so a bare pump receives it and this file defines no
//!   `extern "system"` function at all - which matters because `panic =
//!   "unwind"` is kept deliberately, and unwinding across the foreign boundary
//!   is undefined behaviour. The hazard is removed by construction.

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

/// Take the panel down, asked for by the user - Escape, or the hotkey pressed
/// a second time.
///
/// Distinct from [`WM_APP_HIDE`], and the distinction is the whole reason the
/// exit transition is ever seen: this one *reports* that the panel is going
/// away, and the window is still on screen afterwards. Collapsing the two is
/// how a transition comes to be written and never watched.
const WM_APP_DISMISS: u32 = WM_APP + 3;

/// Put the panel away, asked for by the drawing thread once its exit transition
/// has finished playing.
const WM_APP_HIDE: u32 = WM_APP + 1;

/// Bring the panel up, asked for by something that is not the hotkey: the tray
/// icon, or a second copy of the program being launched.
///
/// Routed through this thread rather than done where it was asked for, because
/// showing the panel means taking the foreground, and this is the thread that
/// does that - see the module note.
const WM_APP_SUMMON: u32 = WM_APP + 2;

/// Any non-negative id will do: the space is per-thread, and this thread
/// registers exactly one.
const HOTKEY_ID: i32 = 1;

/// How long `spawn` waits to hear whether registration worked.
///
/// The path to the answer is three system calls, so this is an upper bound on
/// a thing that takes microseconds, not a guess at how long it might take.
const READY_TIMEOUT: Duration = Duration::from_millis(500);

/// The panel's window, shared with whoever is drawing it.
///
/// An `isize` rather than an `HWND` on purpose. `HWND` is a raw pointer, so it
/// is neither `Send` nor `Sync`, and a struct holding one could not be shared
/// with this thread without an `unsafe impl` that would also switch off the
/// compiler's guarantee that no *other* handle escapes. Passing the bits and
/// rebuilding the pointer in one place keeps that guarantee intact.
///
/// Zero means the window does not exist yet: the thread starts before the
/// toolkit has created anything, and a hotkey pressed in that window of time
/// must do nothing rather than act on a null pointer.
#[derive(Debug)]
pub struct Panel {
    hwnd: AtomicIsize,
    /// Where the user last dragged the panel to, in device pixels, packed as
    /// two `i32`s into one atomic.
    ///
    /// Packed rather than held behind a lock because it is written on every
    /// frame of a drag and read once per summon, and because the two halves are
    /// meaningless apart: a torn read that took the left from one position and
    /// the top from another would put the panel somewhere nobody ever left it.
    /// One atomic makes that unrepresentable rather than unlikely.
    ///
    /// [`NOWHERE`] means nobody has moved it, and the panel is placed.
    at: AtomicI64,
    /// Which edge the panel is pinned to, if any: a [`Dock`] as its index in
    /// [`Dock::ALL`]. Published by the drawing thread every frame, because the
    /// setting can change under it at any time and this thread reads it only
    /// on a keypress.
    dock: AtomicU8,
}

/// No remembered position.
///
/// A sentinel rather than an `Option` because the value lives in an atomic, and
/// the pair has to be read in one go - see [`Panel::at`].
///
/// The packing is a bijection, so *some* coordinate maps onto any sentinel that
/// could be chosen. `i64::MIN` is the image of `(i32::MIN, 0)` exactly, which a
/// test caught: remembering that position and then asking what was remembered
/// answered "nothing". [`COORD_LIMIT`] is what makes the sentinel unreachable
/// rather than merely unlikely.
const NOWHERE: i64 = i64::MIN;

/// Furthest from the desktop origin a coordinate is taken seriously.
///
/// A million pixels is some five hundred 4K monitors laid end to end, so no
/// arrangement of real displays reaches it. Clamping to it costs nothing for
/// every position anybody can produce, and buys the one property [`NOWHERE`]
/// needs: the high half of a packed value is now always within a million of
/// zero, and `i32::MIN` - the high half of the sentinel - is not.
///
/// Clamping rather than rejecting because the two end in the same place
/// anyway. [`geometry::place_at`] pulls whatever it is given inside the work
/// area of the nearest monitor, so an absurd coordinate was always going to
/// become an edge; doing it here as well only decides *which* edge.
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

    /// Published by the drawing thread once the toolkit has a window.
    pub fn publish(&self, hwnd: isize) {
        self.hwnd.store(hwnd, Ordering::Release);
    }

    /// Published by the drawing thread when a drag ends, and once at startup
    /// from whatever [`crate::placement::load`] found.
    ///
    /// `None` puts the panel back to being placed by
    /// [`geometry::place`], which is what the Settings window asks for when
    /// somebody wants the default back.
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

    /// Published by the drawing thread whenever it has a frame to spare.
    pub fn set_dock(&self, dock: Dock) {
        let index = Dock::ALL.iter().position(|&d| d == dock).unwrap_or(0);
        self.dock.store(index as u8, Ordering::Release);
    }

    /// Where the next summon will pin the panel.
    pub fn dock(&self) -> Dock {
        let index = usize::from(self.dock.load(Ordering::Acquire));
        Dock::ALL.get(index).copied().unwrap_or_default()
    }

    /// Whether a position is being remembered, for `--doctor` and for the
    /// Settings window.
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

/// The hotkey thread's handle.
///
/// Genuinely joinable, unlike the input thread: this one parks in
/// `GetMessageW`, and a posted `WM_QUIT` is a wakeup that reaches it.
pub struct HotkeyThread {
    tid: u32,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl HotkeyThread {
    /// The user asked for the panel to go away.
    ///
    /// Starts the exit rather than finishing it: the window is still on screen
    /// when this returns, and stays there until the transition has played and
    /// [`Self::hide`] is called. A window that vanished on the keystroke would
    /// have no exit transition, however carefully one was written.
    pub fn dismiss(&self, handing_over: bool) {
        self.post(WM_APP_DISMISS, usize::from(handing_over), 0);
    }

    /// The exit transition has finished; take the window off the screen.
    pub fn hide(&self) {
        self.post(WM_APP_HIDE, 0, 0);
    }

    /// Brings the panel up, as the hotkey would have.
    ///
    /// Idempotent: asking for a panel that is already up does nothing, which is
    /// what somebody clicking the tray icon twice means.
    pub fn summon(&self) {
        self.post(WM_APP_SUMMON, 0, 0);
    }

    fn post(&self, msg: u32, w: usize, l: isize) {
        // SAFETY: posting to a thread id. A dead thread returns zero, which is
        // ignored - there is nothing useful to do about a listener that has
        // already stopped.
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
        // Abandoned rather than waited on, the same answer every other worker
        // gives. It cannot actually happen: a parked GetMessageW is woken by
        // the post above.
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
            // A panic here must not take the window state with it silently.
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
        // Deliberately does not guess which program. Windows claims several
        // chords itself - shift+win+f23, the Copilot key, among them - so
        // "another copy of this one" is a guess that is wrong about as often
        // as it is right, and a wrong guess sends somebody hunting for a
        // process that was never running.
        format!(
            "{chord} is already claimed \u{b7} set hotkey in config.toml to another \
             combination, or to \"off\""
        )
    } else {
        format!("{chord} could not be registered (error {code})")
    }
}

fn pump(hk: Hotkey, tx: &Events, ready: &Sender<Ready>, panel: &Panel) {
    // Force a message queue into existence *before* the thread id is
    // published. A thread that has never called into user32 has no queue, and
    // PostThreadMessageW against it fails with ERROR_INVALID_THREAD_ID - a
    // hide lost that way would leave the panel on screen with no way to shift
    // it but the hotkey.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: a null window filters to this thread's own messages, and
    // PM_NOREMOVE leaves the queue alone. The only effect wanted is the side
    // effect of the queue being created.
    unsafe {
        PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        )
    };

    // SAFETY: a null window means the hotkey arrives as a *thread* message,
    // which is what lets this file own no window and define no WndProc.
    if unsafe { RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, hk.mods, hk.vk as u32) } == 0 {
        // SAFETY: no arguments.
        let _ = ready.send(Ready::Failed(unsafe { GetLastError() }));
        return;
    }
    // SAFETY: no arguments.
    let tid = unsafe { GetCurrentThreadId() };
    let _ = ready.send(Ready::Registered { tid });

    let mut summoner = Summoner::default();

    // GetMessageW returns 0 on WM_QUIT and -1 on error; `> 0` covers both,
    // which is the documented idiom and the reason this cannot spin.
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

/// Everything the thread knows, on the thread that knows it.
#[derive(Default)]
struct Summoner {
    /// Whether the panel is up. The single authority; the interface's own idea
    /// of it is downstream of this, never a peer.
    shown: bool,
    /// The window the keyboard came from, as bits.
    ///
    /// Stored so focus can be handed back to whatever somebody was working in.
    /// An `isize` rather than an `HWND` only because the field would otherwise
    /// make this struct un-`Send` for no reason; it never leaves this thread.
    prev: isize,
    /// The panel is going away so a viewer can come up, rather than because
    /// somebody pressed Escape.
    ///
    /// Set when the dismissal is asked for and read when the window is
    /// actually hidden, which are two messages apart. See [`Self::hide`].
    handing_over: bool,
}

impl Summoner {
    fn toggle(&mut self, tx: &Events, panel: &Panel) {
        if self.shown {
            // The hotkey pressed again, which is a dismissal like Escape: no
            // viewer is coming, so the foreground goes back where it was.
            self.dismiss(tx, false);
        } else {
            self.summon(tx, panel);
        }
    }

    /// Reports that the panel is going away, and does nothing else.
    ///
    /// The drawing thread plays the exit transition and asks for
    /// `WM_APP_HIDE` when it has finished. Idempotent, because Escape followed
    /// by the hotkey is an ordinary sequence.
    fn dismiss(&mut self, tx: &Events, handing_over: bool) {
        if !self.shown {
            return;
        }
        self.shown = false;
        // Remembered here because `hide` is posted later, by the shell, and
        // has no way of knowing why the panel is going away.
        self.handing_over = handing_over;
        let _ = tx.send(AppEvent::Hotkey(HotkeyMsg::Dismissed));
    }

    fn summon(&mut self, tx: &Events, panel: &Panel) {
        let Some(hwnd) = panel.get() else {
            // The toolkit has not created the window yet. Nothing to say about
            // it: a keypress in the first few hundred milliseconds of the
            // process's life is not something to warn anybody about.
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

            // Where the keyboard is going back to, captured before we take it.
            // Our own window is excluded: storing it would mean dismissing the
            // panel restored focus to the panel.
            let fg = GetForegroundWindow();
            if !fg.is_null() && fg != hwnd {
                self.prev = fg as isize;
            }

            let dock = panel.dock();
            let rect = place(hwnd, panel.placed(), dock);
            // Before it is shown, so it never appears with the wrong corners.
            crate::gui::window::set_rounded(hwnd as isize, !dock.is_docked());
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                // Shown without being activated, then given the foreground
                // deliberately below. `SWP_SHOWWINDOW` alone leaves the
                // keyboard wherever it was, and a search box that does not
                // take what you type is worse than no search box.
                SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
        }

        self.force_foreground(hwnd);
        let _ = tx.send(AppEvent::Hotkey(HotkeyMsg::Summoned));
    }

    /// Puts the panel away and hands the keyboard back.
    fn hide(&mut self, panel: &Panel) {
        self.shown = false;
        let Some(hwnd) = panel.get() else {
            return;
        };
        // SAFETY: handles owned by this process, checked live; a stale `prev`
        // is rejected by `IsWindow` rather than dereferenced.
        unsafe {
            ShowWindow(hwnd, SW_HIDE);

            // Back to whatever was in front before. Hiding a window usually
            // does this by itself, but "usually" means the case it misses is
            // the one where somebody typed a code, dismissed the panel, and
            // found their keystrokes going to the desktop.
            //
            // Except when a viewer is on its way up. Then the window that was
            // in front before is precisely the wrong answer: handing it the
            // foreground is a race against the viewer, and it is a race the
            // viewer loses, because it is still starting. That is most of why
            // a drawing used to open *behind* the window somebody opened it
            // from. Leaving the foreground alone lets the viewer take it with
            // the right this process granted in `dispatch`.
            let prev = self.prev as HWND;
            if !self.handing_over && !prev.is_null() && IsWindow(prev) != 0 {
                SetForegroundWindow(prev);
            }
        }
        self.prev = 0;
        self.handing_over = false;
    }

    /// Takes the foreground, including from a window belonging to somebody
    /// else's thread.
    ///
    /// `SetForegroundWindow` is refused across threads unless the input queues
    /// are attached, and this is the documented way to arrange that. It is
    /// allowed here at all only because this thread is handling `WM_HOTKEY`.
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

/// Where to put the panel, and how big to make it.
///
/// The size is the panel's own - [`crate::gui::PANEL_W`] by
/// [`crate::gui::PANEL_H`] points - in this monitor's pixels. It used to be
/// read back off the window, which was right while nothing but the drawing
/// thread ever sized it. Docking does: a panel that was a bar along the bottom
/// last time is a bar's width now, and asking the window how big it is would
/// float a bar. So both sizes are worked out from the points every summon.
///
/// Docked, it is the width of the work area and the panel's height, against
/// whichever edge - on the monitor the window is on, for the reason below, and
/// with any remembered position left alone for when it floats again.
///
/// `at` is the position the user dragged the panel to, if they ever did. Note
/// which monitor each branch asks about, because it is the difference between
/// the feature working and appearing to work: the default placement asks about
/// the monitor the *window* is on, while a remembered position asks about the
/// monitor that *position* is on. They agree on every summon but the first
/// after a cold start - at which point the window is still wherever the toolkit
/// created it, and asking about the window would quietly drag a position saved
/// on the second screen back onto the first.
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

/// The panel's size in points, in the pixels of the monitor the window is on.
///
/// `GetDpiForWindow` answers for the window's current monitor, which is the
/// one it is about to be placed on in every case but a remembered position on
/// another screen - and there Windows sends the window its new DPI as it
/// arrives, and the toolkit resizes to match.
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
    // What the caller already knows beats what this can find out, and the
    // reason is in `super::Probe`: `RegisterHotKey` is per-thread, so asking
    // again from inside a process whose own hotkey thread holds the chord
    // reports it as taken - by itself.
    let registered = match known {
        Some(answer) => spec.bound().map(|_| answer),
        None => spec.bound().map(|hk| {
            // SAFETY: registered and immediately released, on whatever thread
            // the caller is on. Asking the question must not leave the key
            // claimed.
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
    // The window is this program's own, so there is nothing to find and nothing
    // that can go wrong in finding it. What is still worth reporting is where
    // the panel would land, which is the answer somebody with two monitors
    // actually wants from `--doctor`.
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

    /// The packing is the one piece of arithmetic in this file that a test can
    /// reach, and getting it wrong would not crash - it would put the panel
    /// somewhere plausible and wrong. A sign-extended `top` is the likely
    /// mistake, which is why the negative cases are here.
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

    /// The sentinel is the image of `(i32::MIN, 0)` under a bijective packing,
    /// so without [`COORD_LIMIT`] that exact position reads back as "never
    /// moved". This is the test that found it.
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

    /// And a coordinate no desktop can produce is brought back to one that can,
    /// rather than stored as it stands.
    #[test]
    fn a_coordinate_no_desktop_could_produce_is_brought_back_in_range() {
        let panel = Panel::default();
        panel.remember(Some((i32::MIN, i32::MAX)));
        assert_eq!(panel.placed(), Some((-COORD_LIMIT, COORD_LIMIT)));
    }

    /// Forgetting has to work from any position.
    #[test]
    fn a_position_can_always_be_forgotten() {
        let panel = Panel::default();
        panel.remember(Some((i32::MIN, 0)));
        assert!(panel.placed().is_some());

        panel.remember(None);
        assert_eq!(panel.placed(), None);
    }

    /// Every corner of a desktop far larger than any real one survives intact.
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
