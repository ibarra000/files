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

use super::{AppState, Cmd, Response, Severity};
use crate::config::write::{Edit, Scalar, SettingKey};
use crate::config::{Settings, ThemeChoice, ViewerKind};

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

    /// The drive list was changed in the window.
    ///
    /// Written, and applied to nothing. An index actor per drive is started
    /// once at launch with its own copy of the settings; telling one to become
    /// a different drive is a far larger thing than editing a list, and doing
    /// it badly would mean a search answered from a cache keyed to a path
    /// nobody is searching any more.
    ///
    /// The routing table *is* replaced, so the window shows what it just
    /// wrote rather than what it wrote over - and the form says in so many
    /// words that searching will not change until the next start. Every
    /// consumer that matters holds its own clone, so this moves the display
    /// and nothing else.
    pub(super) fn on_drives(&mut self, list: Vec<crate::paths::Mapping>) -> Response {
        // Renumbered, because an id is a position: removing the first of three
        // would otherwise leave ids 1 and 2 in a list whose slots are 0 and 1,
        // and `statuses` is indexed by exactly that.
        let renumbered: Vec<_> = list
            .iter()
            .enumerate()
            .map(|(i, m)| crate::paths::Mapping {
                id: crate::paths::MappingId(i as u16),
                ..m.clone()
            })
            .collect();

        self.settings.routes = std::sync::Arc::new(crate::paths::Routes::new(
            renumbered.clone(),
            self.settings.routes.source().clone(),
        ));
        // One status per configured drive, or the next report from an actor
        // would be filed against a slot that no longer exists.
        self.resize_statuses();

        Response::redraw().with(Cmd::SaveSetting {
            edit: Edit::Mappings(renumbered),
            label: "Drives",
        })
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
        // A toast rather than silence, because a change made in another
        // window is a change nobody watching this one saw happen.
        self.set_toast("Settings updated".into(), Severity::Info, now);
        Response::redraw()
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
            SettingKey::ResultLayout => {
                if let Scalar::Str(name) = value
                    && let Some(layout) = crate::config::ResultLayout::parse(name)
                {
                    // Read on the frame the list is drawn, and by nothing
                    // else - so the next one is already the new shape.
                    self.settings.result_layout = layout;
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
            // Gates rendering and nothing else, so nothing has captured it and
            // it moves at once. The next message drawn carries its detail, or
            // stops carrying it.
            SettingKey::DevMode => {
                if let Scalar::Bool(on) = value {
                    self.settings.dev_mode = *on;
                }
            }
            // Read on the keystroke that opens something, or on the frame
            // the window loses focus, off the `Settings` this struct owns -
            // so the next one sees the new answer.
            SettingKey::HideOnBlur => {
                if let Scalar::Bool(on) = value {
                    self.settings.hide_on_blur = *on;
                }
            }
            SettingKey::HideAfterOpening => {
                if let Scalar::Bool(on) = value {
                    self.settings.hide_after_opening = *on;
                }
            }
            SettingKey::HideOnEscape => {
                if let Scalar::Bool(on) = value {
                    self.settings.hide_on_escape = *on;
                }
            }
            // Deliberately not applied, which is a stronger statement than
            // "captured at startup" and is the reason this arm is written
            // out on its own.
            //
            // Nothing here holds a copy of `update_from`, so moving it would
            // work. What holds a copy is the update checker's *thread*: the
            // folder is moved into it at spawn, and the thread is only
            // spawned at all when the setting was set at boot. So applying
            // this live would make the About page read `Looking in <the new
            // folder>` above a status the old folder produced, beside a
            // "Check now" button wired to the old thread - or to nothing, if
            // there was no folder at boot. Three controls, all lying.
            //
            // Leaving it alone keeps `settings.update_from` equal to the
            // folder the checker is actually reading, for as long as this
            // process runs. Anybody tempted to make this live has to move
            // the thread first.
            SettingKey::UpdateFrom => {}
            // Captured at startup by something that cannot be told. The form
            // says these apply when files next starts.
            // The material is set on the window handle once, by
            // `window::apply`. The shell re-applies it when the theme moves
            // and nowhere else, so a change here would not reach the
            // compositor until the next start.
            SettingKey::Backdrop
            | SettingKey::Hotkey
            | SettingKey::LiveUpdates
            | SettingKey::PdfViewer
            | SettingKey::PdfReadOnly
            | SettingKey::IndexLog
            | SettingKey::HideExtensions
            | SettingKey::HideSystemFiles => {}
        }
    }
}
