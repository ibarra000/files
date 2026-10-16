//! The panel's side of the link to the separate settings-window process.
//!
//! Responsibilities:
//!
//! * **Start at most one.** If a window is attached it is told to come forward
//!   instead; a second `--settings` process would be rejected by the pipe.
//! * **Toggle.** Ctrl+comma sends `Close` to an attached window, or starts one.
//! * **Forward panel-only state**: the dragged position and whether the global
//!   hotkey was registered, sent when they change.
//! * **Act on requests**: reload the file, install an update, forget the
//!   remembered position.
//!
//! The child is spawned detached and never waited on. The settings window may
//! outlive the panel; each side detects the other's exit when the pipe breaks.

use std::process::Command;

use crate::config::Settings;
use crate::ipc::{Held, Live, ToPanel, ToSettings};
use crate::view::settings::PageId;

/// The panel's link to the settings window.
pub struct PanelLink {
    #[cfg(windows)]
    server: Option<crate::ipc::pipe::Server>,
    /// The last `Live` sent, so unchanged state is not resent every frame
    /// (for example while the panel is dragged).
    last: Option<Live>,
}

impl PanelLink {
    /// Starts listening. `wake` is called when the window sends a message.
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

    /// Opens the window on `page`, or switches an open window to it.
    ///
    /// Used by the tray menu, whose items name specific pages.
    pub fn open_at(&mut self, page: PageId, settings: &Settings) {
        if self.open() {
            self.send(&ToSettings::Show(page));
        } else {
            self.start(Some(page), settings);
        }
    }

    /// Opens the window, or closes it if attached. Used by Ctrl+comma.
    ///
    /// Names no page, so a reopened window returns to its last page.
    pub fn toggle(&mut self, settings: &Settings) {
        if self.open() {
            self.send(&ToSettings::Close);
        } else {
            self.start(None, settings);
        }
    }

    /// Sends the panel position and hotkey status if they changed.
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

    /// Notifies the window that the configuration file changed.
    pub fn reload(&mut self) {
        self.send(&ToSettings::Reload);
    }

    /// Notifies the window that the panel is exiting.
    pub fn exiting(&mut self) {
        self.send(&ToSettings::Exiting);
    }

    /// Returns the messages received from the window since the last call.
    pub fn poll(&mut self) -> Vec<ToPanel> {
        #[cfg(windows)]
        {
            // Clear the last-sent state when the window detaches, so the next
            // window receives a full `Live`.
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

    /// Spawns `files --settings` detached.
    fn start(&mut self, page: Option<PageId>, settings: &Settings) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut command = Command::new(exe);
        command.arg("--settings");
        if let Some(page) = page {
            command.arg("--page").arg(page.slug());
        }
        // Pass on which settings are pinned by this process's command-line
        // flags; otherwise the window would offer to save values that the
        // flags override. See `config::Pin`.
        if settings.cli_pinned != 0 {
            command.arg("--pinned").arg(settings.cli_pinned.to_string());
        }
        // Pass on a non-default `--config` path, which the window cannot know.
        if let Some(path) = config_arg(settings) {
            command.arg("--config").arg(path);
        }
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Detached; see the module docs.
        let _ = command.spawn();
    }
}

/// Returns the panel's configuration path if it is not the default one.
fn config_arg(settings: &Settings) -> Option<std::path::PathBuf> {
    let source = settings.routes.source();
    let path = source.path()?;
    let default = crate::config::file::default_config_path();
    (Some(path) != default.as_deref()).then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `open()` reports a window only once one has connected, not when one was
    /// requested.
    #[test]
    fn asking_for_a_window_does_not_make_one_exist() {
        let mut link = PanelLink::new(|| {});
        assert!(!link.open());
        // A real window is not spawned in a unit test.
        link.send(&ToSettings::Close);
        assert!(!link.open());
    }

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

    /// Detaching clears the last-sent state so the next window gets everything.
    #[test]
    fn a_new_window_is_told_everything_again() {
        let mut link = PanelLink::new(|| {});
        link.live(Some((10, 20)), &None);
        assert!(link.last.is_some());
        // `poll` detects the missing window.
        let _ = link.poll();
        assert!(link.last.is_none(), "the record outlived the window");
    }

    #[test]
    fn the_default_configuration_file_is_not_named_on_the_command_line() {
        let settings = Settings::fixture();
        assert_eq!(config_arg(&settings), None);
    }
}
