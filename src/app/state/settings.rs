//! Adopting changed settings into the running panel.
//!
//! A child module of `state` so it can reach private fields, as `overlay` does.
//!
//! A change takes effect before it is written: `%APPDATA%` is often redirected
//! to a network path, and a control must not wait on an SMB round trip. A
//! failed save therefore still applies for the session, which is what
//! `OpenMsg::SettingSaveFailed` reports.
//!
//! [`crate::config::Settings`] is also cloned into the backend, the workers
//! and every index actor. Fields they captured at startup cannot change under
//! them and take effect on the next start.

use std::time::Instant;

use super::{AppState, Response, Severity};
use crate::config::Settings;
use crate::config::write::SettingKey;

/// A setting the window changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChange {
    pub key: SettingKey,
    pub typed: crate::config::write::Typed,
    /// The form's label, used to name the control in outcome messages.
    pub label: &'static str,
}

impl AppState {
    /// Replaces `Settings` wholesale and re-applies whatever derives from it.
    ///
    /// The settings window is a separate process that shares only the config
    /// file, so it writes the file and the panel re-reads it whole.
    pub(super) fn on_adopt(&mut self, fresh: Settings, now: Instant) -> Response {
        let was = std::mem::replace(&mut self.settings, fresh);
        for key in SettingKey::ALL {
            self.adopt_one(key, &was);
        }

        // One status per configured drive, so actor reports land in a valid
        // slot. The drive list is not a `SettingKey`, so the loop skips it.
        self.resize_statuses();

        // Probe results for the old viewer list are stale; the caller, which
        // may do I/O, probes again.
        if self.settings.viewers != was.viewers {
            self.viewers_missing = Box::default();
        }

        let mut response = Response::redraw();

        // The current line may have gained or lost an alias or match a
        // different set of drives. Re-run it through the normal path so the
        // expansion, query and results stay consistent.
        if !self.input.text().trim().is_empty() {
            response.merge(self.on_input_changed(now, crate::app::state::Urgency::Complete));
        }

        // The change was made in another window, so announce it here.
        self.set_toast("Settings updated".into(), Severity::Info, now);
        response
    }

    /// Applies the side effects of one changed key beyond the field itself.
    ///
    /// The match is exhaustive so a new `SettingKey` forces a decision here.
    fn adopt_one(&mut self, key: SettingKey, was: &Settings) {
        match key {
            // Read where they are used; nothing to do.
            SettingKey::Theme
            | SettingKey::ResultLayout
            | SettingKey::Dock
            | SettingKey::Columns
            | SettingKey::Backdrop
            | SettingKey::HideOnBlur
            | SettingKey::HideAfterOpening
            | SettingKey::HideOnEscape
            | SettingKey::HideExtensions
            | SettingKey::HideSystemFiles
            | SettingKey::PdfReadOnly
            | SettingKey::DevMode
            | SettingKey::LiveUpdates
            | SettingKey::StaleNotices
            | SettingKey::PdfViewer
            | SettingKey::IndexLog
            | SettingKey::Hotkey
            | SettingKey::UpdateFrom
            | SettingKey::CheckForUpdates => {}
            // `settings.viewer` is what the form reads back; `self.viewer` is
            // what Enter uses. F2 changes only the latter.
            SettingKey::Viewer => self.viewer = self.settings.viewer.within(&self.settings.viewers),
            // Turning history off also clears the in-memory list so Up cannot
            // recall entries the user asked not to keep.
            SettingKey::History => {
                if was.history && !self.settings.history {
                    self.history = crate::history::History::new();
                }
            }
        }
    }
}
