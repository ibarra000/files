//! The shape of the window: pages, groups, and the blocks they hold.
//!
//! The layer above [`super::Row`], added when the form stopped being one long
//! column. It carries no wording of its own beyond the page titles - the
//! words are in [`super::pages`] and [`super::lists`] - and it carries no
//! appearance at all, which is the promise the whole of [`crate::view`]
//! makes.

use std::borrow::Cow;

use super::{ALIASES, DRIVES, Field, Row};
use crate::view::status::Tone;
use crate::view::{Emphasis, style};

/// One entry in the list down the left.
///
/// An id rather than a title and a picture, so this module keeps its promise
/// to name meanings and never appearances. The renderer decides which mark a
/// page is drawn with; see `gui::theme::icons`.
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
    /// In the order they are listed, which is roughly the order somebody
    /// meets them: how the program behaves, what it looks like, where it
    /// searches, what it opens, and finally what it is.
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

    /// For an egui id, and for nothing anybody reads.
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

/// A titled run of blocks.
///
/// The heading is optional, and that is not laziness. A page with a single
/// group would otherwise repeat its own name straight back at the reader,
/// which is what the old one-column form did every time a section held one
/// row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub heading: Option<&'static str>,
    pub blocks: Vec<Block>,
}

/// What a group is made of.
///
/// An enum rather than a handful of optional fields, so a group cannot be in
/// a state nobody drew - and so [`Page::prose`] can be exhaustive over it,
/// which is the point.
///
/// The old form kept the wording for its lists in loose `pub mod` constants
/// and hand-listed all eighteen of them in one test. That test checked the
/// eighteen it named and would not have checked the nineteenth. This way the
/// compiler asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Rows(Vec<Row>),
    Facts(Vec<Fact>),
    Actions(Vec<Action>),
    /// The alias list. The words are [`ALIASES`]; the entries are read off
    /// the live `Settings` by whoever is drawing, because a copy here would
    /// be the second version of the truth this module exists to not hold.
    Aliases,
    /// And the drives, for the same reason. See [`DRIVES`].
    Drives,
    /// The `--doctor` report: read-only, monospaced, and taken once per
    /// opening of the page because it touches the drives.
    Report {
        intro: &'static str,
    },
}

/// Something read out and not changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub label: &'static str,
    /// `Cow` for the reason [`crate::view::Run`] uses one, and with a second
    /// job here.
    ///
    /// A borrowed value is a line this program wrote and is held to the
    /// house style. An owned one is a value read out of the configuration -
    /// a path, a version, a drive name - and keeps whatever spelling it was
    /// given. That is the exception `style::starts_capitalised` documents in
    /// prose, made mechanical: see [`Page::prose`].
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
    /// A sentence under the button, where pressing it deserves one.
    pub help: Option<&'static str>,
    /// A button that reports a problem when pressed is a button that should
    /// not have been pressable.
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

/// Which button, for the shell to act on.
///
/// An id rather than a callback, so this module stays a description of the
/// form rather than a participant in it. The same reason the renderer hands
/// its caller a `Clicked` instead of reaching back into the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionId {
    OpenConfigFile,
    ForgetPlacement,
    CheckForUpdates,
    InstallUpdate,
    CopyReport,
    /// Take the reading again now.
    ///
    /// An answer is kept for a minute, so that stepping to another page and
    /// back does not re-run a multi-second network probe. This is the button
    /// for the case the minute is wrong: somebody who has just plugged a
    /// drive in wants the new answer, not the one from forty seconds ago.
    RefreshReport,
}

impl Page {
    /// Every word on this page that this program wrote.
    ///
    /// Exhaustive over [`Block`], so a block added without prose is a
    /// compile error rather than a corner of the form nobody checks. Values
    /// read out of the configuration are left out by construction rather
    /// than by a filter somebody has to remember: see [`Fact::value`].
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
                                    out.extend(options.iter().map(|o| Cow::Borrowed(o.label)));
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
                    Block::Aliases => out.extend(ALIASES.lines().map(Cow::Borrowed)),
                    Block::Drives => out.extend(DRIVES.lines().map(Cow::Borrowed)),
                    Block::Report { intro } => out.push(Cow::Borrowed(*intro)),
                }
            }
        }
        out
    }
}

/// The whole corpus, for the house-style check.
pub fn prose(pages: &[Page]) -> Vec<Cow<'static, str>> {
    pages.iter().flat_map(Page::prose).collect()
}

/// Asserts the house style over every word on every page.
///
/// Here rather than in the test module because it is the thing the page
/// registry exists to make possible, and because `style::check_all` is
/// itself outside `cfg(test)` for the same reason: the check belongs where
/// the strings are.
pub fn check_prose(pages: &[Page]) {
    let lines = prose(pages);
    style::check_all(
        "the settings pages",
        lines.iter().map(Cow::as_ref),
        style::Slot::Body,
    );
}
