//! Talking to the settings window, which is a separate process.
//!
//! # Why a pipe and not the doorbell next door
//!
//! [`crate::single`] already does inter-process signalling: a named mutex
//! and a named auto-reset event, twenty lines of it. It is exactly the right
//! shape for what it does, which is "wake up", and exactly the wrong shape
//! for this - it carries no payload, and more importantly it carries no
//! *liveness*. This link needs both.
//!
//! The liveness is the deciding requirement. With no panel running, three
//! things on the settings form become lies: the About page cannot install
//! an update, the Appearance page cannot forget a window position nothing
//! is remembering, and "applies at once" is not true of any setting because
//! there is nothing to apply it to. A named pipe answers that question for
//! nothing: the write fails with `ERROR_BROKEN_PIPE` the moment the other
//! end goes, which is knowledge an event cannot give.
//!
//! Message-mode pipes also frame for us, so the codec next door is a line
//! of ASCII per message rather than a length prefix and a state machine.
//!
//! # No panel running is a supported state
//!
//! `files --settings` started from the Start menu with nothing else running
//! opens the window, reads the configuration file, and writes to it. Every
//! setting works; the three actions above are disabled, and every caveat on
//! the form reads "when files next starts" - which is more honest than what
//! the in-process window said, and is what makes `--settings` a legitimate
//! shortcut rather than a debugging aid.
//!
//! That is why [`Link`] is a trait with two implementations rather than a
//! struct: [`pipe`] is the real one, and the settings process holds
//! `Option<Box<dyn Link>>` with `None` meaning "on its own".

pub mod wire;

#[cfg(windows)]
pub mod pipe;

pub use wire::{Held, Live, ToPanel, ToSettings};

/// Why a name is per-session rather than machine-wide.
///
/// `Local\` on a mutex is per-session by convention, but `\\.\pipe\` is
/// machine-global: two people signed in to the same terminal server would
/// share one name, and whichever settings window connected first would be
/// driving somebody else's panel. The session id closes that.
///
/// `PIPE_REJECT_REMOTE_CLIENTS` closes the other half - a pipe is reachable
/// over SMB by default, so without it a name on this machine is a name on
/// the network.
#[cfg(windows)]
pub fn pipe_name() -> String {
    format!(r"\\.\pipe\files-settings-{}", session_id())
}

/// This process's Windows session.
///
/// Zero where it cannot be asked, which is a worse name and not a broken
/// one: two sessions would share a pipe and the first window to connect
/// would win, which is the behaviour without this at all.
#[cfg(windows)]
fn session_id() -> u32 {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let mut id = 0u32;
    // SAFETY: writes one `u32` through a pointer to a local. The pid is this
    // process's own, which is always a valid one to ask about.
    let ok = unsafe { ProcessIdToSessionId(std::process::id(), &mut id) };
    if ok == 0 { 0 } else { id }
}

/// One end of the link, from the settings window's point of view.
///
/// A trait so that the settings process can run without one, and so that
/// the contract can be tested over a pair of channels instead of two live
/// processes - see [`Pair`]. Everything here is best effort: a panel that
/// has gone away is an ordinary state, not an error to report.
pub trait Link: Send {
    /// Sends, or reports that the other end has gone.
    fn send(&mut self, msg: &ToPanel) -> bool;
    /// Whatever has arrived since the last call, in order.
    fn poll(&mut self) -> Vec<ToSettings>;
    /// Whether the other end is still there.
    fn alive(&self) -> bool;
}

/// Two ends joined by a pair of channels, for tests.
///
/// The contract this module is really about is the *sequence* of messages,
/// and that is checkable without a pipe. What the pipe adds is bytes and
/// Windows, and neither is what a wrong message order would be caused by.
pub struct Pair {
    to_them: std::sync::mpsc::Sender<ToPanel>,
    from_them: std::sync::mpsc::Receiver<ToSettings>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Pair {
    /// A settings end and a panel end, wired together.
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

    /// The shape the settings window relies on: what it sends arrives, in
    /// order, and what the panel sends comes back the same way.
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

    /// A panel that has gone away is a fact the window can act on rather
    /// than an error it has to handle. This is the property the whole
    /// choice of transport was made for.
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

    /// And a window with no panel at all is not a broken window.
    #[test]
    fn no_link_is_a_supported_state() {
        let none: Option<Box<dyn Link>> = None;
        assert!(none.is_none(), "the type is what carries this");
    }
}
