//! Changing a setting from the window.
//!
//! A child module rather than a free-standing one so it can reach the private
//! fields the live half depends on, exactly as `overlay` and `preview` do.
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
//! # Why only four settings move
//!
//! [`crate::config::Settings`] is cloned into the backend, both workers and
//! every index actor. A field one of them captured cannot be changed
//! underneath it - there would be two truths, and the stale one would win
//! wherever it was being read. Four settings are read where they are used
//! rather than captured at startup, and those are the four that move.
//!
//! The rest are written and wait for the next start, which the form says in
//! so many words. That is a smaller promise than live reload and it is one
//! that can be kept.

use std::time::Instant;

use super::{AppState, Cmd, Response};
use crate::config::write::{Edit, Scalar, SettingKey};
use crate::config::{ThemeChoice, ViewerKind};

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
    /// The alias list was changed in the window.
    ///
    /// Applied at once, and it can be: the table is read from `self.settings`
    /// on the keystroke that needs it, so nothing captured a copy at startup.
    /// Saved whole rather than by entry, because that is how the window edits
    /// it - see [`crate::config::write::Edit::Aliases`].
    pub(super) fn on_aliases(&mut self, list: Vec<crate::alias::Alias>, now: Instant) -> Response {
        self.settings.aliases = std::sync::Arc::new(crate::alias::Aliases::new(list.clone()));

        let mut response = Response::redraw().with(Cmd::SaveSetting {
            edit: Edit::Aliases(list),
            label: "Aliases",
        });

        // The line already on the panel may have just become an alias, or
        // stopped being one. Put back through the ordinary path rather than
        // re-resolved on the side, so the expansion, the query and the results
        // move together - a field showing an expansion for an alias that has
        // been removed is exactly the silent disagreement the resolution is
        // written to avoid.
        if !self.input.text().trim().is_empty() {
            response.merge(self.on_input_changed(now, crate::app::state::Urgency::Complete));
        }
        response
    }

    pub(super) fn on_setting(&mut self, change: SettingChange, _now: Instant) -> Response {
        let SettingChange { key, typed, label } = change;
        let edit = Edit::from_typed(key, typed);

        self.apply_live(&edit);

        // Asked before the command is emitted rather than discovered by the
        // writer, so that a pinned setting costs no thread and no round trip.
        // The window already names what is holding it, so saying it again in
        // a toast every time a control moves would be nagging.
        if !self.settings.can_save(key) {
            return Response::redraw();
        }
        Response::redraw().with(Cmd::SaveSetting { edit, label })
    }

    /// Applies the settings nothing else is holding a copy of.
    ///
    /// Exhaustive over [`SettingKey`] on purpose: a key added without a
    /// decision here is a compile error rather than a control that silently
    /// does nothing until the program is restarted.
    fn apply_live(&mut self, edit: &Edit) {
        let Edit::Set { key, value } = edit else {
            // Every key that can be unset is one that waits for a restart
            // anyway, so there is nothing to do now.
            return;
        };
        match key {
            SettingKey::Theme => {
                if let Scalar::Str(name) = value
                    && let Some(theme) = ThemeChoice::parse(name)
                {
                    // `gui::Shell::follow_theme` reads this every frame and
                    // repaints when it moves, so there is nothing to notify.
                    self.settings.theme = theme;
                }
            }
            SettingKey::Viewer => {
                if let Scalar::Str(name) = value
                    && let Some(viewer) = ViewerKind::parse(name)
                {
                    // Both, and they are not the same thing: `settings.viewer`
                    // is what the form reads back, and `self.viewer` is what
                    // Enter uses. F2 moves only the second, which is why the
                    // form would otherwise show a stale answer.
                    self.settings.viewer = viewer;
                    self.viewer = viewer;
                }
            }
            SettingKey::History => {
                if let Scalar::Bool(on) = value {
                    self.settings.history = *on;
                }
            }
            SettingKey::StaleNotices => {
                if let Scalar::Bool(on) = value {
                    self.settings.stale_notices = *on;
                }
            }
            // Captured at startup by something that cannot be told. The form
            // says these apply when files next starts.
            SettingKey::Hotkey
            | SettingKey::LiveUpdates
            | SettingKey::PdfViewer
            | SettingKey::HideExtensions
            | SettingKey::HideSystemFiles => {}
        }
    }
}
