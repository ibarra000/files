//! The panel's end of the settings window.
//!
//! Replaces `gui::windows`, which drew the form inside an immediate
//! viewport of the panel. What is left of that file is this one: three
//! hundred and eighty lines of window state become "start a process, or
//! tell the one that is running", because the window now owns all of it.
//!
//! # What the panel still has to do
//!
//! Four things, and they are the reason this is not simply
//! `Command::spawn`.
//!
//! * **Start at most one.** A second `--settings` would be turned away by
//!   the pipe, silently, and somebody who pressed Ctrl+comma twice would
//!   wonder why. So a window that is already attached is *told* to come
//!   forward instead.
//! * **Toggle.** Ctrl+comma is advertised as show-or-hide and has to do
//!   both. With a window attached it sends `Close`; with none it starts
//!   one.
//! * **Tell it what only the panel knows.** Where the panel has been
//!   dragged to, and whether the global chord was claimed. Two facts, sent
//!   whenever they change.
//! * **Act on what it asks for.** Re-read the file, install an update,
//!   forget the remembered position.
//!
//! # Detached, deliberately
//!
//! The child is started with no handle kept and never waited on. A
//! settings window outliving the panel is a perfectly good state - it has
//! a configuration file and it can write to it - and the pipe breaking is
//! how each finds out about the other. Keeping a `Child` would mean a
//! zombie process entry for as long as the panel ran, to learn nothing the
//! pipe does not already say.

use std::process::Command;

use crate::config::Settings;
use crate::ipc::{Held, Live, ToPanel, ToSettings};
use crate::view::settings::PageId;

/// The panel's link to the settings window.
pub struct PanelLink {
    #[cfg(windows)]
    server: Option<crate::ipc::pipe::Server>,
    /// The last thing sent, so it is not sent again every frame.
    ///
    /// The window is redrawn on every message and the panel produces a
    /// frame whenever anything moves, so without this the link would carry
    /// sixty identical `live` lines a second while somebody dragged the
    /// panel around.
    last: Option<Live>,
}

impl PanelLink {
    /// Starts listening. `wake` is called when the window says something.
    pub fn new(wake: impl Fn() + Clone + Send + 'static) -> Self {
        Self {
            #[cfg(windows)]
            server: crate::ipc::pipe::Server::listen(wake),
            #[cfg(not(windows))]
            _wake: {
                let _ = wake;
            },
            last: None,
        }
    }

    /// Whether a settings window is attached.
    pub fn open(&self) -> bool {
        #[cfg(windows)]
        {
            self.server.as_ref().is_some_and(|s| s.connected())
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Opens the window on a named page, or brings the open one to it.
    ///
    /// What the tray menu does. A menu item named "Diagnostics" has to land
    /// on the diagnostics, not on wherever the window was last left.
    pub fn open_at(&mut self, page: PageId, settings: &Settings) {
        if self.open() {
            self.send(&ToSettings::Show(page));
        } else {
            self.start(Some(page), settings);
        }
    }

    /// Opens the window, or shuts it if it is already up.
    ///
    /// What Ctrl+comma does, as against what the tray menu does. A key
    /// advertised as "show or hide" has to do both; it names no page,
    /// because a toggle should come back to where you were.
    pub fn toggle(&mut self, settings: &Settings) {
        if self.open() {
            self.send(&ToSettings::Close);
        } else {
            self.start(None, settings);
        }
    }

    /// Tells the window the two facts only this process has, if they moved.
    pub fn live(&mut self, placement: Option<(i32, i32)>, hotkey: &Option<Result<(), String>>) {
        let live = Live {
            placement,
            hotkey: match hotkey {
                None => Held::Unknown,
                Some(Ok(())) => Held::Yes,
                Some(Err(why)) => Held::No(why.clone()),
            },
        };
        if self.last.as_ref() == Some(&live) {
            return;
        }
        self.send(&ToSettings::Live(live.clone()));
        self.last = Some(live);
    }

    /// The configuration file has changed underneath the window.
    pub fn reload(&mut self) {
        self.send(&ToSettings::Reload);
    }

    /// The panel is going away.
    pub fn exiting(&mut self) {
        self.send(&ToSettings::Exiting);
    }

    /// Whatever the window has asked for since the last frame.
    pub fn poll(&mut self) -> Vec<ToPanel> {
        #[cfg(windows)]
        {
            // A window that has gone takes the memory of what it was last
            // told with it, so the next one gets a fresh `Live` rather than
            // nothing.
            if !self.open() {
                self.last = None;
            }
            self.server.as_ref().map(|s| s.poll()).unwrap_or_default()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    }

    fn send(&mut self, msg: &ToSettings) {
        #[cfg(windows)]
        if let Some(server) = self.server.as_ref() {
            server.send(msg);
        }
        #[cfg(not(windows))]
        {
            let _ = msg;
        }
    }

    /// Starts `files --settings`, and forgets about it.
    fn start(&mut self, page: Option<PageId>, settings: &Settings) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut command = Command::new(exe);
        command.arg("--settings");
        if let Some(page) = page {
            command.arg("--page").arg(page.slug());
        }
        // Which settings a flag on *this* process is holding. Without it
        // the window would have a second, wrong view of what is
        // overridden - `files --viewer pdf` would give a settings window
        // that offers to save the viewer, reports success, and changes
        // nothing at all. See `config::Pin`.
        if settings.cli_pinned != 0 {
            command.arg("--pinned").arg(settings.cli_pinned.to_string());
        }
        // And the same configuration file, because `--config` is a flag on
        // this process and the window has no way to know about it.
        if let Some(path) = config_arg(settings) {
            command.arg("--config").arg(path);
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Detached: see the module note. The handle is dropped immediately
        // and nothing ever waits on it.
        let _ = command.spawn();
    }
}

/// The file the panel is running from, where it is not the default one.
///
/// `--config` is not passed on when it names the default path, because the
/// window would find the same file anyway and a line of arguments that says
/// nothing is a line somebody has to read.
fn config_arg(settings: &Settings) -> Option<std::path::PathBuf> {
    let source = settings.routes.source();
    let path = source.path()?;
    let default = crate::config::file::default_config_path();
    (Some(path) != default.as_deref()).then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no window attached, a toggle starts one and `open` says so
    /// only once it has connected - which it has not, in a test with no
    /// real window to start.
    ///
    /// The value here is small and specific: `open()` must not claim a
    /// window exists merely because one was asked for. Everything that
    /// follows from a wrong answer - a `Show` sent into nothing, a second
    /// process never started - is worse than the honest "not yet".
    #[test]
    fn asking_for_a_window_does_not_make_one_exist() {
        let mut link = PanelLink::new(|| {});
        assert!(!link.open());
        // Deliberately not started: spawning a real window in a unit test
        // is a test of the build rather than of this.
        link.send(&ToSettings::Close);
        assert!(!link.open());
    }

    /// The same facts twice are one message.
    ///
    /// Without this the panel would carry sixty identical lines a second
    /// while somebody dragged it, which is the kind of thing that is
    /// invisible until a profiler is pointed at it.
    #[test]
    fn the_same_live_facts_are_not_sent_twice() {
        let mut link = PanelLink::new(|| {});
        link.live(Some((10, 20)), &Some(Ok(())));
        let first = link.last.clone();
        assert!(first.is_some());

        link.live(Some((10, 20)), &Some(Ok(())));
        assert_eq!(link.last, first, "the record moved on an identical send");

        link.live(Some((11, 20)), &Some(Ok(())));
        assert_ne!(link.last, first, "a moved panel was not reported");
    }

    /// And a window that goes away forgets what it was told, so the next
    /// one is not left with nothing because the last one had the same
    /// facts.
    #[test]
    fn a_new_window_is_told_everything_again() {
        let mut link = PanelLink::new(|| {});
        link.live(Some((10, 20)), &None);
        assert!(link.last.is_some());
        // `poll` is where the panel notices, and with no window attached
        // it notices every frame.
        let _ = link.poll();
        assert!(link.last.is_none(), "the record outlived the window");
    }

    /// The default path is not passed on, because the window finds it
    /// anyway and an argument that says nothing is one somebody has to
    /// read.
    #[test]
    fn the_default_configuration_file_is_not_named_on_the_command_line() {
        let settings = Settings::default();
        assert_eq!(config_arg(&settings), None);
    }
}
