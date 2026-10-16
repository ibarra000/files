//! Link between the panel and the settings window, which runs as a separate
//! process.
//!
//! A named pipe is used rather than the event in [`crate::single`] because the
//! link needs a payload and liveness: a write fails with `ERROR_BROKEN_PIPE`
//! as soon as the panel exits, so the settings window can disable actions
//! that need it (installing updates, forgetting the placement, live apply).
//! Message-mode pipes also provide framing, so [`wire`] encodes one ASCII line
//! per message.
//!
//! The settings window can also run with no panel. It then holds `None` as its
//! `Option<Box<dyn Link>>`, edits the configuration file directly, and labels
//! changes as taking effect on next start.

// Win32 FFI: ProcessIdToSessionId, for the per-session pipe name.
#![allow(unsafe_code)]

pub mod wire;

#[cfg(windows)]
pub mod pipe;

pub use wire::{Held, Live, ToPanel, ToSettings};

/// Returns the pipe name for this Windows session.
///
/// Pipe names are machine-global, so the session id keeps users on a shared
/// terminal server apart. Remote access over SMB is blocked separately with
/// `PIPE_REJECT_REMOTE_CLIENTS`.
#[cfg(windows)]
pub fn pipe_name() -> String {
    format!(r"\\.\pipe\files-settings-{}", session_id())
}

/// Returns this process's Windows session id, or 0 if the query fails.
///
/// With 0, sessions share one pipe name and the first client to connect wins.
#[cfg(windows)]
fn session_id() -> u32 {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let mut id = 0u32;
    // SAFETY: writes one `u32` through a pointer to a local. The pid is this
    // process's own, which is always a valid one to ask about.
    let ok = unsafe { ProcessIdToSessionId(std::process::id(), &mut id) };
    if ok == 0 { 0 } else { id }
}

/// The settings window's end of the link.
///
/// A trait so the protocol can be tested over channels (see [`Pair`]). All
/// operations are best effort; a panel that has exited is an ordinary state.
pub trait Link: Send {
    /// Sends `msg`; returns `false` if the other end has gone.
    fn send(&mut self, msg: &ToPanel) -> bool;
    /// Returns messages received since the last call, in order.
    fn poll(&mut self) -> Vec<ToSettings>;
    /// Whether the other end is still there.
    fn alive(&self) -> bool;
}

/// An in-memory [`Link`] over channels, for testing message sequences.
pub struct Pair {
    to_them: std::sync::mpsc::Sender<ToPanel>,
    from_them: std::sync::mpsc::Receiver<ToSettings>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Pair {
    /// Returns the settings end plus the panel end's sender, receiver and
    /// liveness flag.
    #[allow(clippy::type_complexity)]
    pub fn new() -> (
        Self,
        std::sync::mpsc::Sender<ToSettings>,
        std::sync::mpsc::Receiver<ToPanel>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let (to_them, they_get) = std::sync::mpsc::channel();
        let (they_send, from_them) = std::sync::mpsc::channel();
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let end = Self {
            to_them,
            from_them,
            alive: std::sync::Arc::clone(&alive),
        };
        (end, they_send, they_get, alive)
    }
}

impl Link for Pair {
    fn send(&mut self, msg: &ToPanel) -> bool {
        self.alive() && self.to_them.send(msg.clone()).is_ok()
    }

    fn poll(&mut self) -> Vec<ToSettings> {
        self.from_them.try_iter().collect()
    }

    fn alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn messages_cross_in_the_order_they_were_sent() {
        let (mut link, panel_sends, panel_gets, _alive) = Pair::new();

        assert!(link.send(&ToPanel::Changed));
        assert!(link.send(&ToPanel::ForgetPlacement));
        assert_eq!(panel_gets.try_recv(), Ok(ToPanel::Changed));
        assert_eq!(panel_gets.try_recv(), Ok(ToPanel::ForgetPlacement));

        panel_sends.send(ToSettings::Reload).unwrap();
        panel_sends.send(ToSettings::Close).unwrap();
        assert_eq!(link.poll(), vec![ToSettings::Reload, ToSettings::Close]);
        assert_eq!(link.poll(), vec![], "a message was delivered twice");
    }

    #[test]
    fn a_dead_panel_is_visible_rather_than_a_failed_write() {
        let (mut link, _sends, _gets, alive) = Pair::new();
        assert!(link.alive());

        alive.store(false, Ordering::Relaxed);
        assert!(!link.alive());
        assert!(
            !link.send(&ToPanel::InstallUpdate),
            "an install was sent to a panel that had exited"
        );
    }

    #[test]
    fn no_link_is_a_supported_state() {
        let none: Option<Box<dyn Link>> = None;
        assert!(none.is_none(), "the type is what carries this");
    }
}
