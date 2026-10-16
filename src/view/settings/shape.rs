//! The structure of the settings window: pages, groups and the blocks they
//! hold.
//!
//! Only page titles are worded here; the rest is in [`super::pages`] and
//! [`super::lists`].

use std::borrow::Cow;

use super::{ALIASES, DRIVES, Field, Row};
use crate::view::status::Tone;
use crate::view::{Emphasis, style};

/// One entry in the navigation list.
///
/// The renderer chooses each page's icon; see `gui::theme::icons`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageId {
    General,
    Appearance,
    Drives,
    Aliases,
    Searching,
    Opening,
    About,
    Diagnostics,
}

impl PageId {
    /// Every page, in navigation order.
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::Appearance,
        Self::Drives,
        Self::Aliases,
        Self::Searching,
        Self::Opening,
        Self::About,
        Self::Diagnostics,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Drives => "Drives",
            Self::Aliases => "Aliases",
            Self::Searching => "Searching",
            Self::Opening => "Opening",
            Self::About => "About",
            Self::Diagnostics => "Diagnostics",
        }
    }

    /// A stable identifier for egui ids; never shown.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Drives => "drives",
            Self::Aliases => "aliases",
            Self::Searching => "searching",
            Self::Opening => "opening",
            Self::About => "about",
            Self::Diagnostics => "diagnostics",
        }
    }
}

/// One screen of the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub id: PageId,
    pub groups: Vec<Group>,
}

/// A run of blocks with an optional heading.
///
/// A page's only group usually has no heading, to avoid repeating the page
/// title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub heading: Option<&'static str>,
    pub blocks: Vec<Block>,
}

/// One block within a group.
///
/// An enum so [`Page::prose`] can match it exhaustively; a new variant
/// without prose handling fails to compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Rows(Vec<Row>),
    Facts(Vec<Fact>),
    Actions(Vec<Action>),
    /// Switches for things Windows keeps rather than the configuration file.
    /// See [`Switch`].
    Switches(Vec<Switch>),
    /// The alias list. Wording is [`ALIASES`]; the renderer reads entries
    /// from the live `Settings` rather than a copy here.
    Aliases,
    /// The drive list, handled like [`Block::Aliases`]. See [`DRIVES`].
    Drives,
    /// The `--doctor` report: read-only and monospaced. Taken once per page
    /// opening because it touches the drives.
    Report {
        intro: &'static str,
    },
}

/// A read-only labelled value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub label: &'static str,
    /// A borrowed value is program-written text, held to the house style. An
    /// owned value (a path, version or drive name) keeps its own spelling and
    /// is excluded by [`Page::prose`].
    pub value: Cow<'static, str>,
    pub emphasis: Emphasis,
}

impl Fact {
    pub fn new(label: &'static str, value: impl Into<Cow<'static, str>>) -> Self {
        Self {
            label,
            value: value.into(),
            emphasis: Emphasis::Body,
        }
    }

    pub fn toned(label: &'static str, value: impl Into<Cow<'static, str>>, tone: Tone) -> Self {
        Self {
            label,
            value: value.into(),
            emphasis: Emphasis::Tone(tone),
        }
    }
}

/// A button that does something rather than setting something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: ActionId,
    pub label: &'static str,
    /// An optional sentence shown under the button.
    pub help: Option<&'static str>,
    /// Disabled when pressing it could only report a problem.
    pub enabled: bool,
}

impl Action {
    pub fn new(id: ActionId, label: &'static str) -> Self {
        Self {
            id,
            label,
            help: None,
            enabled: true,
        }
    }

    pub fn saying(mut self, help: &'static str) -> Self {
        self.help = Some(help);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// A switch whose state lives outside `config.toml`, such as the "Start with
/// Windows" registry value that the installer and Task Manager also write.
///
/// Drawn like a [`Row`] but acts like a button: the shell receives the
/// [`ActionId`] for the other state, then reads the state back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    pub label: &'static str,
    pub help: &'static str,
    pub on: bool,
    pub turn_on: ActionId,
    pub turn_off: ActionId,
}

impl Switch {
    pub fn new(
        label: &'static str,
        help: &'static str,
        on: bool,
        turn_on: ActionId,
        turn_off: ActionId,
    ) -> Self {
        Self {
            label,
            help,
            on,
            turn_on,
            turn_off,
        }
    }

    /// Returns the action that moves the switch to the other state.
    pub fn flipped(&self) -> ActionId {
        if self.on { self.turn_off } else { self.turn_on }
    }
}

/// Identifies a button for the shell to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    OpenConfigFile,
    ForgetPlacement,
    CheckForUpdates,
    InstallUpdate,
    CopyReport,
    /// Re-runs the report now. Reports are cached for a minute so switching
    /// pages does not repeat a multi-second network probe.
    RefreshReport,
    /// Writes the `Run` value, so signing in starts files hidden in the tray.
    StartWithWindows,
    /// Deletes the `Run` value.
    StopStartingWithWindows,
}

impl Page {
    /// Returns every program-written string on this page.
    ///
    /// Exhaustive over [`Block`]. Configured values are excluded by their
    /// `Cow::Owned` type; see [`Fact::value`].
    pub fn prose(&self) -> Vec<Cow<'static, str>> {
        let mut out: Vec<Cow<'static, str>> = vec![Cow::Borrowed(self.id.title())];
        for group in &self.groups {
            out.extend(group.heading.map(Cow::Borrowed));
            for block in &group.blocks {
                match block {
                    Block::Rows(rows) => {
                        for row in rows {
                            out.push(Cow::Borrowed(row.label));
                            out.push(Cow::Borrowed(row.help));
                            out.extend(row.caveat().map(Cow::Owned));
                            match &row.field {
                                Field::Choice { options, .. } => {
                                    // Skip owned labels, which come from the configuration.
                                    out.extend(options.iter().filter_map(|o| match o.label {
                                        Cow::Borrowed(text) => Some(Cow::Borrowed(text)),
                                        Cow::Owned(_) => None,
                                    }));
                                }
                                Field::Text { placeholder, .. } => {
                                    out.push(Cow::Borrowed(*placeholder));
                                }
                                Field::Toggle { .. } => {}
                            }
                        }
                    }
                    Block::Facts(facts) => {
                        for fact in facts {
                            out.push(Cow::Borrowed(fact.label));
                            if let Cow::Borrowed(text) = fact.value {
                                out.push(Cow::Borrowed(text));
                            }
                        }
                    }
                    Block::Actions(actions) => {
                        for action in actions {
                            out.push(Cow::Borrowed(action.label));
                            out.extend(action.help.map(Cow::Borrowed));
                        }
                    }
                    Block::Switches(switches) => {
                        for switch in switches {
                            out.push(Cow::Borrowed(switch.label));
                            out.push(Cow::Borrowed(switch.help));
                        }
                    }
                    Block::Aliases => out.extend(ALIASES.lines().map(Cow::Borrowed)),
                    Block::Drives => out.extend(DRIVES.lines().map(Cow::Borrowed)),
                    Block::Report { intro } => out.push(Cow::Borrowed(*intro)),
                }
            }
        }
        out
    }
}

/// Returns every program-written string on every page.
pub fn prose(pages: &[Page]) -> Vec<Cow<'static, str>> {
    pages.iter().flat_map(Page::prose).collect()
}

/// Asserts the house style over every word on every page.
pub fn check_prose(pages: &[Page]) {
    let lines = prose(pages);
    style::check_all(
        "the settings pages",
        lines.iter().map(Cow::as_ref),
        style::Slot::Body,
    );
}
