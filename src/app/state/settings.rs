//! Changing a setting from the window.
//!
//! A child module rather than a free-standing one so it can reach the private
//! fields the live half depends on, exactly as `overlay` does.
//!
//! # Applied here, not on the way back
//!
//! The change takes effect the moment it is made, and only then is it written.
//! The other order looks tidier and is wrong: `%APPDATA%` is very often
//! redirected onto a network path in the domains this runs in, so waiting for
//! the write would mean a switch that moves when an SMB round trip says it
//! may. A control that lags a file server is a control that reads as broken.
//!
//! It also means a failed save is a change that already happened, which is why
//! `OpenMsg::SettingSaveFailed` says "for this session only" rather than
//! implying nothing occurred.
//!
//! # Why only five settings move
//!
//! [`crate::config::Settings`] is cloned into the backend, both workers and
//! every index actor. A field one of them captured cannot be changed
//! underneath it - there would be two truths, and the stale one would win
//! wherever it was being read. Five settings are read where they are used
//! rather than captured at startup, and those are the five that move.
//!
//! The rest are written and wait for the next start, which the form says in
//! so many words. That is a smaller promise than live reload and it is one
//! that can be kept.

use std::time::Instant;

use super::{AppState, Response, Severity};
use crate::config::Settings;
use crate::config::write::SettingKey;

/// A setting the window changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChange {
    pub key: SettingKey,
    pub typed: crate::config::write::Typed,
    /// How the form spells it, carried through so the message that reports
    /// the outcome names the control the user actually touched.
    pub label: &'static str,
}

impl AppState {
    /// Takes a whole new `Settings`, and re-applies whatever is derived
    /// from it.
    ///
    /// The replacement for `apply_live`, which took one `Edit` and matched
    /// exhaustively over [`SettingKey`] so that a key added without a
    /// decision was a compile error rather than a control that silently
    /// did nothing. That guarantee is kept and the mechanism is inverted:
    /// this assigns the whole struct and then asks, key by key, what else
    /// has to happen - and the `match` below is exhaustive for the same
    /// reason the old one was.
    ///
    /// Why the whole struct: the settings window is a separate process, and
    /// the only thing it and the panel both see is the file. It writes,
    /// then says so. Sending the edit instead would mean putting the
    /// configuration schema on the wire and trusting two processes to agree
    /// about what each field means, to save a read of a file that is a few
    /// kilobytes long.
    pub(super) fn on_adopt(&mut self, fresh: Settings, now: Instant) -> Response {
        let was = std::mem::replace(&mut self.settings, fresh);
        for key in SettingKey::ALL {
            self.adopt_one(key, &was);
        }

        // One status per configured drive, or the next report from an
        // actor would be filed against a slot that no longer exists. The
        // drive list is not a `SettingKey` - it is its own array in the
        // file - so it is not covered by the loop above.
        self.resize_statuses();

        let mut response = Response::redraw();

        // The line already on the panel may have just become an alias, or
        // stopped being one, or now matches a different set of drives. Put
        // back through the ordinary path rather than re-resolved on the
        // side, so the expansion, the query and the results move together -
        // a field showing an expansion for an alias that has been removed
        // is exactly the silent disagreement the resolution is written to
        // avoid.
        if !self.input.text().trim().is_empty() {
            response.merge(self.on_input_changed(now, crate::app::state::Urgency::Complete));
        }

        // A toast rather than silence, because a change made in another
        // window is a change nobody watching this one saw happen.
        self.set_toast("Settings updated".into(), Severity::Info, now);
        response
    }

    /// What has to happen beyond the field itself, for one key.
    ///
    /// Exhaustive on purpose: see [`Self::on_adopt`]. Most keys are read
    /// where they are used and need nothing here, and saying so key by key
    /// is what makes the next one somebody adds a decision rather than an
    /// omission.
    fn adopt_one(&mut self, key: SettingKey, was: &Settings) {
        match key {
            // Read at the moment they are used, by something that is about
            // to read them anyway.
            SettingKey::Theme
            | SettingKey::ResultLayout
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
            | SettingKey::UpdateFrom => {}
            // The viewer is two fields, and they are not the same thing:
            // `settings.viewer` is what the form reads back and `self.viewer`
            // is what Enter uses. F2 moves only the second.
            SettingKey::Viewer => self.viewer = self.settings.viewer,
            // Turning the history off throws away what is in memory as
            // well as stopping new entries, or the up arrow would still
            // recall codes from a list the user has just asked not to be
            // kept.
            SettingKey::History => {
                if was.history && !self.settings.history {
                    self.history = crate::history::History::new();
                }
            }
        }
    }
}
