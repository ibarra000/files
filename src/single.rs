//! One copy at a time.
//!
//! A program that lives in the notification area is one people launch again
//! without meaning to - from the Start menu, from a pinned shortcut, from the
//! installer's "run now" checkbox. A second copy is not merely redundant: it
//! would fail to register the global hotkey (the first copy owns it), put a
//! second icon in the tray, and start a second index walk over the same shares.
//!
//! So the second launch does what the person launching it actually wanted, and
//! then gets out of the way: it summons the copy already running and exits.
//!
//! # Why a named event and not a window message
//!
//! The obvious way is `RegisterWindowMessageW` plus a broadcast, and it is the
//! wrong way here for a specific reason: this program's window belongs to a
//! toolkit, so there is no `WndProc` of ours for a broadcast to arrive at, and
//! the thread that *can* act on a summon deliberately owns no window at all
//! (see [`crate::hotkey`]). A named event needs neither. One thread waits on
//! it, and a wait is exactly the shape of the thing already used everywhere
//! else in this program.

#![cfg(windows)]

use std::sync::Arc;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};

/// Per-session rather than machine-wide.
///
/// `Local\` means one copy per Windows session, which is the right unit: two
/// people signed in to the same terminal server each get their own, and each
/// has their own shares mapped and their own history file. A `Global\` name
/// would let the first of them lock the second out of a program they can see
/// in their own tray.
const MUTEX_NAME: &str = r"Local\files-single-instance";
const EVENT_NAME: &str = r"Local\files-summon";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A handle that closes itself.
///
/// Not merely tidiness: the mutex is what says this copy is running, and a
/// leaked handle would keep saying so for as long as the process lived, which
/// is the same thing - but a *closed* one at the wrong moment would let a
/// second copy in.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle this module created and has not closed.
            unsafe { CloseHandle(self.0) };
        }
    }
}

// SAFETY: a Win32 kernel handle is a process-wide token, not a pointer into
// this process's memory, and every use of it below is a call that takes it by
// value. The wait thread genuinely needs to own one.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

/// This process's claim to being the only copy.
pub struct Instance {
    _mutex: Owned,
    _event: Arc<Owned>,
}

/// What happened when this copy tried to claim the name.
pub enum Claim {
    /// This is the only copy. Hold the [`Instance`] for as long as the program
    /// runs.
    Only(Instance),
    /// Another copy has it, and has been asked to show itself. This one should
    /// exit quietly - with a success code, because from the user's point of
    /// view the launch worked.
    AlreadyRunning,
}

/// Claims the name, or hands over to whoever already has it.
///
/// `on_summon` is called on a thread of its own, every time another copy is
/// launched. It is only ever called on the copy that won.
pub fn claim(on_summon: impl Fn() + Send + 'static) -> Claim {
    claim_named(MUTEX_NAME, EVENT_NAME, on_summon)
}

/// The body of [`claim`], with the names as parameters.
///
/// Split only so the tests can each use a name of their own. They share a
/// process and the runner runs them in parallel, so two tests claiming
/// one name between them would be one test asserting that the *other* test
/// is not running.
fn claim_named(mutex_name: &str, event_name: &str, on_summon: impl Fn() + Send + 'static) -> Claim {
    let name = wide(mutex_name);
    // SAFETY: a named mutex with default security. The name is a live
    // NUL-terminated wide string for the duration of the call.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: no arguments; reads this thread's last error.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    if mutex.is_null() {
        // The name could not be claimed at all, which is not a reason to
        // refuse to run: without the mutex this copy simply does not get the
        // single-instance behaviour, and a search tool that will not start
        // because of a naming service is worse than two of it.
        return Claim::Only(Instance {
            _mutex: Owned(std::ptr::null_mut()),
            _event: Arc::new(Owned(std::ptr::null_mut())),
        });
    }
    let mutex = Owned(mutex);

    if existed {
        summon_the_other(event_name);
        return Claim::AlreadyRunning;
    }

    // Auto-reset, so each launch is one summon: a manual-reset event left
    // signalled would summon the panel in a tight loop.
    let event_name = wide(event_name);
    // SAFETY: a named auto-reset event, initially unsignalled, with default
    // security and a live name for the duration of the call.
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
    let event = Arc::new(Owned(event));

    if !event.0.is_null() {
        let waiter = Arc::clone(&event);
        // Detached: the wait is uninterruptible, and there is nothing useful to
        // do at shutdown but let the process take it down. The handle is kept
        // alive by the `Arc` for exactly as long as the thread can use it.
        let _ = std::thread::Builder::new()
            .name("files-single".into())
            .spawn(move || {
                loop {
                    // SAFETY: waits on a handle this module created and holds
                    // an `Arc` to, so it cannot be closed underneath the wait.
                    let woken = unsafe { WaitForSingleObject(waiter.0, INFINITE) };
                    if woken != 0 {
                        // WAIT_OBJECT_0 is zero; anything else means the handle
                        // went away, and spinning on a dead one would be a
                        // thread at a hundred per cent for the life of the
                        // process.
                        return;
                    }
                    on_summon();
                }
            });
    }

    Claim::Only(Instance {
        _mutex: mutex,
        _event: event,
    })
}

/// Tells the copy that is already running to show itself.
///
/// Best effort throughout. Every failure here means the second copy exits
/// without the first one coming forward, which is a shortcut that did nothing -
/// annoying, and much better than two copies fighting over one hotkey.
fn summon_the_other(event_name: &str) {
    let name = wide(event_name);
    // EVENT_MODIFY_STATE (0x0002) is all that is needed to signal it, and is
    // the least this can ask for.
    // SAFETY: opens an existing named event; a name nothing owns returns null,
    // which is checked.
    unsafe {
        let event = OpenEventW(0x0002, 0, name.as_ptr());
        if !event.is_null() {
            SetEvent(event);
            CloseHandle(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// The first copy wins and the second is turned away. Anything else is two
    /// index walks over the same share.
    /// A name nothing else in the suite uses.
    fn names(tag: &str) -> (String, String) {
        (
            format!(r"Local\files-test-{tag}-mutex"),
            format!(r"Local\files-test-{tag}-event"),
        )
    }

    #[test]
    fn the_second_copy_does_not_get_the_name() {
        let (m, e) = names("exclusive");
        let first = claim_named(&m, &e, || {});
        assert!(matches!(first, Claim::Only(_)));

        let second = claim_named(&m, &e, || {});
        assert!(
            matches!(second, Claim::AlreadyRunning),
            "two copies both believed they were the only one"
        );

        // ...and once the first lets go, the name is available again, or a
        // crash would lock the user out until they signed out.
        drop(first);
        drop(second);
        let third = claim_named(&m, &e, || {});
        assert!(
            matches!(third, Claim::Only(_)),
            "the name stayed claimed after the holder exited"
        );
    }

    /// The point of the exercise: launching it again brings the copy that is
    /// running to the front, rather than doing nothing at all.
    #[test]
    fn launching_a_second_copy_summons_the_first() {
        static SUMMONS: AtomicUsize = AtomicUsize::new(0);
        SUMMONS.store(0, Ordering::SeqCst);

        let (m, e) = names("summon");
        let _first = claim_named(&m, &e, || {
            SUMMONS.fetch_add(1, Ordering::SeqCst);
        });
        assert!(matches!(claim_named(&m, &e, || {}), Claim::AlreadyRunning));

        // The wait is on another thread, so this is a poll rather than an
        // assertion on the next line - but a bounded one, because a test that
        // waits forever for a thing that never happens is not a test.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && SUMMONS.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            SUMMONS.load(Ordering::SeqCst),
            1,
            "the running copy was never told to show itself"
        );
    }
}
