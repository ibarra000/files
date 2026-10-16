//! Single-instance guard: a second launch summons the running copy and exits.
//!
//! A second copy would fail to register the hotkey, add a second tray icon,
//! and re-index the same shares. Summons use a named event rather than a
//! broadcast window message because the window belongs to the toolkit (no
//! `WndProc` of ours) and the summon thread owns no window (see
//! [`crate::hotkey`]).

#![cfg(windows)]

use std::sync::Arc;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};

/// Session-scoped names (`Local\`), so each user on a terminal server gets
/// their own instance.
const MUTEX_NAME: &str = r"Local\files-single-instance";
const EVENT_NAME: &str = r"Local\files-summon";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A kernel handle closed on drop. A null handle is ignored.
///
/// Closing the mutex handle early would let a second copy start.
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
// this process's memory, and every use below passes it by value. The wait
// thread needs to own one.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

/// This process's claim to being the only copy.
pub struct Instance {
    _mutex: Owned,
    _event: Arc<Owned>,
}

/// The result of trying to become the only instance.
pub enum Claim {
    /// This is the only copy. Hold the [`Instance`] for the program's lifetime.
    Only(Instance),
    /// Another copy is running and has been summoned. Exit with a success code.
    AlreadyRunning,
}

/// Claims single-instance ownership, or summons the copy that already has it.
///
/// `on_summon` runs on a dedicated thread each time another copy is launched.
pub fn claim(on_summon: impl Fn() + Send + 'static) -> Claim {
    claim_named(MUTEX_NAME, EVENT_NAME, on_summon)
}

/// [`claim`] with explicit object names, so parallel tests can use their own.
fn claim_named(mutex_name: &str, event_name: &str, on_summon: impl Fn() + Send + 'static) -> Claim {
    let name = wide(mutex_name);
    // SAFETY: a named mutex with default security. The name is a live
    // NUL-terminated wide string for the duration of the call.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: no arguments; reads this thread's last error.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    if mutex.is_null() {
        // Mutex creation failed: run without single-instance protection
        // rather than refusing to start.
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
        // Detached: the wait is uninterruptible and ends with the process.
        // The `Arc` keeps the handle alive while the thread can use it.
        let _ = std::thread::Builder::new()
            .name("files-single".into())
            .spawn(move || {
                loop {
                    // SAFETY: waits on a handle this module created and holds
                    // an `Arc` to, so it cannot be closed underneath the wait.
                    let woken = unsafe { WaitForSingleObject(waiter.0, INFINITE) };
                    if woken != 0 {
                        // WAIT_OBJECT_0 is zero; anything else means the
                        // handle is invalid, and retrying would spin forever.
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

/// Signals the running copy to show itself. Best effort; failures are ignored.
fn summon_the_other(event_name: &str) {
    let name = wide(event_name);
    // EVENT_MODIFY_STATE (0x0002) is the minimum access needed to signal.
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

    /// Returns mutex and event names unique to one test.
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

        // Releasing the instance frees the name.
        drop(first);
        drop(second);
        let third = claim_named(&m, &e, || {});
        assert!(
            matches!(third, Claim::Only(_)),
            "the name stayed claimed after the holder exited"
        );
    }

    #[test]
    fn launching_a_second_copy_summons_the_first() {
        static SUMMONS: AtomicUsize = AtomicUsize::new(0);
        SUMMONS.store(0, Ordering::SeqCst);

        let (m, e) = names("summon");
        let _first = claim_named(&m, &e, || {
            SUMMONS.fetch_add(1, Ordering::SeqCst);
        });
        assert!(matches!(claim_named(&m, &e, || {}), Claim::AlreadyRunning));

        // The summon arrives on another thread; poll with a deadline.
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
