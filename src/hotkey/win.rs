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
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, GetLastError, HWND};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetMessageW, GetWindowRect, GetWindowThreadProcessId, HWND_TOPMOST,
    IsWindow, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SW_HIDE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow, WM_APP, WM_HOTKEY, WM_QUIT,
    WM_USER,
};

use super::geometry::{self, RectPx};
use super::spec::{Hotkey, HotkeySpec};
use super::{Probe, win_hwnd};
use crossbeam_channel::Sender;

use crate::app::event::{AppEvent, Events, HotkeyMsg};

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
#[derive(Debug, Default)]
pub struct Panel {
    hwnd: AtomicIsize,
}

impl Panel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Published by the drawing thread once the toolkit has a window.
    pub fn publish(&self, hwnd: isize) {
        self.hwnd.store(hwnd, Ordering::Release);
    }

    fn get(&self) -> Option<HWND> {
        let bits = self.hwnd.load(Ordering::Acquire);
        (bits != 0).then_some(bits as HWND)
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
        Ok(Ready::Registered { tid }) => Ok(Some(HotkeyThread {
            tid,
            handle: Some(handle),
        })),
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

            let rect = place(hwnd);
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

/// Where to put the panel, at the size it currently is.
///
/// The size is read back off the window rather than passed in, because the
/// drawing thread owns it and reads of a value that is animating would be a
/// second opinion about it. Reading it here means the placement is exactly
/// right for the panel as it is at the instant it appears.
fn place(hwnd: HWND) -> RectPx {
    let work = win_hwnd::work_area(hwnd).unwrap_or(RectPx::new(0, 0, 1920, 1080));

    // SAFETY: `rect` is a live local; the call reports failure by return value.
    let want = unsafe {
        let mut rect = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) != 0 {
            (rect.right - rect.left, rect.bottom - rect.top)
        } else {
            // Clamped up to `MIN_PX` by `place`, so a window whose size could
            // not be read still lands somewhere it can be seen and dismissed.
            (0, 0)
        }
    };

    geometry::place(work, want)
}

pub fn probe(spec: HotkeySpec) -> Probe {
    let chord = spec.bound().map(super::spec::describe);
    let registered = spec.bound().map(|hk| {
        // SAFETY: registered and immediately released, on whatever thread the
        // caller is on. Asking the question must not leave the key claimed.
        unsafe {
            if RegisterHotKey(std::ptr::null_mut(), HOTKEY_ID, hk.mods, hk.vk as u32) == 0 {
                let code = GetLastError();
                Err(registration_detail(hk, code))
            } else {
                UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID);
                Ok(())
            }
        }
    });
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
