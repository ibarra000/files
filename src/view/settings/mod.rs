//! What the settings window offers, and what it says about it.
//!
//! The shape of the form and every word on it, as plain data. No colour, no
//! rectangle and no widget - for the reason the rest of this module gives, and
//! for one more that is particular to this screen: a form is mostly prose, and
//! prose is the thing the house style is checked on.
//!
//! # The window does not hold a second copy of the truth
//!
//! That was the stated reason the settings window was read-only, and it is a
//! good one: a window with its own idea of the settings is a window that comes
//! to disagree with the file. Nothing here is a buffer. [`sections`] is a pure
//! function of the live [`Settings`], called every frame, so what is on screen is
//! what is in force. A change is written through [`crate::config::write`] and
//! read back, or it is refused with a reason - there is no third state in
//! which the window believes something the file does not.
//!
//! # Saying what will not stick, and what will not stick yet
//!
//! Two different disappointments, and they are kept apart.
//!
//! [`Row::pin`] means the value cannot be *saved*: the environment or a flag
//! outranks the file, so writing it would report a success and change nothing
//! at the next start. That is [`crate::config::Pin`], and it names what is
//! holding the setting so it can be found and undone.
//!
//! [`Row::needs_restart`] means the value saves perfectly well but nothing
//! reads it again until the program starts. `Settings` is cloned into the
//! backend, every index actor and both workers, so a field one of them
//! captured cannot be changed underneath it. Five settings are read where they
//! are used rather than captured, and those apply immediately; the rest say so
//! rather than appearing to work.
//!
//! # Pages, and why the shape is data too
//!
//! The form used to be one scrolling column of headings, and `sections`
//! returned them. It is now a list of pages with a nav down the left, which
//! is a fact about the *form* rather than about the drawing of it: which page
//! a setting is on is a decision about where somebody will look for it, and
//! that belongs beside the sentence explaining it rather than in a renderer.
//!
//! So [`pages`] returns the whole registry, [`Page::prose`] walks it, and
//! [`check_prose`] holds every word in it to the house style. The version
//! before this kept the wording for the two lists in loose constants and
//! hand-listed all eighteen of them in the test that checked them - which
//! checked the eighteen it named, and would not have checked a nineteenth.

use crate::config::write::SettingKey;
use crate::config::{Pin, Settings};

mod lists;
mod pages;
mod shape;
#[cfg(test)]
mod tests;

pub use lists::{ALIASES, AliasWords, DRIVES, DriveWords};
pub use pages::pages;
pub use shape::{Action, ActionId, Block, Fact, Group, Page, PageId, Switch, check_prose, prose};

/// One option in a [`Field::Choice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// What is written to the file.
    pub value: &'static str,
    /// What the window shows.
    pub label: &'static str,
}

/// What kind of control a setting needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    /// One of a fixed set, where the set is short enough to show at once.
    Choice {
        options: &'static [Choice],
        /// Index into `options`. Always valid: it is derived from the live
        /// setting, which is already one of them.
        current: usize,
    },
    Toggle {
        on: bool,
    },
    /// Free text, because the value is a path, a chord or a list and no fixed
    /// set could hold it.
    Text {
        value: String,
        /// Shown when the value is empty, and it says what *not* setting this
        /// does rather than repeating the label.
        placeholder: &'static str,
    },
}

/// One setting, with everything the window has to say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: SettingKey,
    pub label: &'static str,
    /// One sentence on what it does. Whole sentences: this is body prose.
    pub help: &'static str,
    pub field: Field,
    /// Why this cannot be written back, if it cannot.
    pub pin: Option<Pin>,
    /// Whether a change waits for the next start.
    pub needs_restart: bool,
}

impl Row {
    /// What to say under the control, or nothing when there is nothing to say.
    ///
    /// A pin outranks a restart notice, and deliberately: a setting that will
    /// not be saved at all is not also worth telling somebody it would have
    /// applied at the next start.
    pub fn caveat(&self) -> Option<String> {
        match (&self.pin, self.needs_restart) {
            (Some(pin), _) => Some(pin.detail()),
            (None, true) => Some("Applies when files next starts".into()),
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
    Choice {
        value: "acrylic",
        label: "Acrylic",
    },
    Choice {
        value: "mica",
        label: "Mica",
    },
    Choice {
        value: "tabbed",
        label: "Tabbed",
    },
    Choice {
        value: "none",
        label: "None",
    },
];

pub(super) const LAYOUTS: &[Choice] = &[
    Choice {
        value: "compact",
        label: "Compact",
    },
    Choice {
        value: "detailed",
        label: "Detailed",
    },
];

pub(super) const DOCKS: &[Choice] = &[
    Choice {
        value: "free",
        label: "Floating",
    },
    Choice {
        value: "top",
        label: "Along the top",
    },
    Choice {
        value: "bottom",
        label: "Along the bottom",
    },
];

pub(super) const THEMES: &[Choice] = &[
    Choice {
        value: "light",
        label: "Light",
    },
    Choice {
        value: "dark",
        label: "Dark",
    },
    Choice {
        value: "system",
        label: "Follow Windows",
    },
];

pub(super) const VIEWERS: &[Choice] = &[
    Choice {
        value: "auto",
        label: "Automatic",
    },
    Choice {
        value: "pdf",
        label: "PDF",
    },
    Choice {
        value: "avwin",
        label: "avwin",
    },
];

/// Where `options` holds `value`, or zero.
///
/// Zero rather than a panic: the value came out of a `Settings` whose every
/// variant is in the list above, so the fallback is unreachable - and a
/// settings window that refuses to open is a poor way to report that somebody
/// added a fourth theme and forgot this file.
pub(super) fn index_of(options: &[Choice], value: &str) -> usize {
    options.iter().position(|o| o.value == value).unwrap_or(0)
}

/// One setting, with its pin and its restart notice filled in from the key.
///
/// Asked of the key rather than written out at each call site, so that this
/// and the module that does the applying cannot come to disagree about which
/// settings wait.
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
