//! The settings form as plain data: its pages, controls and every word on
//! them, with no appearance.
//!
//! # No second copy of the settings
//!
//! [`pages`] is a pure function of the live [`Settings`], called every frame,
//! so the screen always shows what is in force. A change is written through
//! [`crate::config::write`] and read back, or refused with a reason.
//!
//! # Pins and restarts
//!
//! [`Row::pin`] means the value cannot be saved because the environment or a
//! flag outranks the file ([`crate::config::Pin`] names the source).
//! [`Row::needs_restart`] means the value saves but is only read at startup,
//! because `Settings` is cloned into the backend, index actors and workers.
//!
//! [`Page::prose`] enumerates every word on the form and [`check_prose`]
//! holds it to the house style.

use crate::config::write::SettingKey;
use std::borrow::Cow;

use crate::config::{Pin, Settings, ViewerKind};

mod lists;
mod pages;
mod shape;
#[cfg(test)]
mod tests;

pub use lists::{ALIASES, AliasWords, DRIVES, DriveWords};
pub use pages::pages;
pub use shape::{Action, ActionId, Block, Fact, Group, Page, PageId, Switch, check_prose, prose};

/// One option in a [`Field::Choice`].
///
/// As with [`Fact::value`], a borrowed label is held to the house style and an
/// owned one (a configured viewer name) keeps its spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The value written to the file.
    pub value: Cow<'static, str>,
    /// The label the window shows.
    pub label: Cow<'static, str>,
}

impl Choice {
    const fn fixed(value: &'static str, label: &'static str) -> Self {
        Self {
            value: Cow::Borrowed(value),
            label: Cow::Borrowed(label),
        }
    }
}

/// The kind of control a setting needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    /// One of a short fixed set, all shown at once.
    Choice {
        /// Borrowed for a fixed set; owned when it comes from the
        /// configuration, as with viewers.
        options: Cow<'static, [Choice]>,
        /// Index into `options`, derived from the live setting.
        current: usize,
    },
    Toggle {
        on: bool,
    },
    /// Free text, for paths, chords and lists.
    Text {
        value: String,
        /// Shown when empty; describes the effect of leaving it unset.
        placeholder: &'static str,
    },
}

/// One setting and everything the window says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: SettingKey,
    pub label: &'static str,
    /// One whole sentence on what the setting does.
    pub help: &'static str,
    pub field: Field,
    /// Why the setting cannot be saved, if it cannot.
    pub pin: Option<Pin>,
    /// Whether a change waits for the next start.
    pub needs_restart: bool,
}

impl Row {
    /// Returns the note shown under the control, if any.
    ///
    /// A pin outranks a restart notice, since a pinned value is never saved.
    pub fn caveat(&self) -> Option<String> {
        match (&self.pin, self.needs_restart) {
            (Some(pin), _) => Some(pin.detail()),
            (None, true) => Some("Takes effect after files restarts".into()),
            (None, false) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: &'static str,
    pub rows: Vec<Row>,
}

pub(super) const BACKDROPS: &[Choice] = &[
    Choice::fixed("acrylic", "Acrylic"),
    Choice::fixed("mica", "Mica"),
    Choice::fixed("tabbed", "Tabbed"),
    Choice::fixed("none", "None"),
];

pub(super) const LAYOUTS: &[Choice] = &[
    Choice::fixed("compact", "Compact"),
    Choice::fixed("detailed", "Detailed"),
];

pub(super) const DOCKS: &[Choice] = &[
    Choice::fixed("free", "Floating"),
    Choice::fixed("top", "Top"),
    Choice::fixed("bottom", "Bottom"),
];

pub(super) const COLUMNS: &[Choice] = &[
    Choice::fixed("1", "One"),
    Choice::fixed("2", "Two"),
    Choice::fixed("3", "Three"),
];

pub(super) const THEMES: &[Choice] = &[
    Choice::fixed("light", "Light"),
    Choice::fixed("dark", "Dark"),
    Choice::fixed("system", "Follow Windows"),
];

/// Returns the `Enter` viewer modes: the two built-in ones, then each
/// configured viewer in order.
pub(super) fn viewer_choices(settings: &Settings) -> Cow<'static, [Choice]> {
    let fixed = [
        Choice::fixed("auto", "Automatic"),
        Choice::fixed("pdf", "PDF"),
    ];
    let configured = settings.viewers.all().iter().map(|v| Choice {
        value: Cow::Owned(v.name.to_string()),
        label: Cow::Owned(v.name.to_string()),
    });
    Cow::Owned(fixed.into_iter().chain(configured).collect())
}

/// Returns the index of the [`viewer_choices`] entry in force.
pub(super) fn viewer_index(settings: &Settings) -> usize {
    match settings.viewer.within(&settings.viewers) {
        ViewerKind::Auto => 0,
        ViewerKind::Pdf => 1,
        ViewerKind::App(i) => 2 + i,
    }
}

/// Returns the index of `value` in `options`, or zero.
///
/// The fallback is unreachable while every variant is listed above; it avoids
/// a panic if a new variant is added without updating this file.
pub(super) fn index_of(options: &[Choice], value: &str) -> usize {
    options.iter().position(|o| o.value == value).unwrap_or(0)
}

/// Builds a row, deriving its pin and restart notice from the key so they
/// agree with the code that applies settings.
pub(super) fn row(
    settings: &Settings,
    key: SettingKey,
    label: &'static str,
    help: &'static str,
    field: Field,
) -> Row {
    Row {
        key,
        label,
        help,
        field,
        pin: settings.pin(key),
        needs_restart: !key.applies_at_once(),
    }
}
