//! The words around the two lists, which cannot be [`super::Row`]s.
//!
//! An alias and a drive each have several values and a button, so neither
//! has a single field to put on a row. Their wording lives here for the
//! reason the rest of this module lives where it does: it is prose, and
//! prose is what the house style is checked on.
//!
//! # A struct rather than a `pub mod` of constants
//!
//! These were two modules of loose `pub const`s, and the test that checked
//! them named all eighteen by hand. That test checked the eighteen it named.
//! A nineteenth would have been added, shipped and never looked at.
//!
//! One value each, with a `lines()` that enumerates it, so
//! [`super::Page::prose`] picks the whole set up whether or not anybody
//! remembered.

/// What the alias list says, above the list itself.
pub struct AliasWords {
    pub help: &'static str,
    pub name_hint: &'static str,
    pub code_hint: &'static str,
    pub note_hint: &'static str,
    pub add: &'static str,
    pub remove: &'static str,
    pub empty: &'static str,
}

impl AliasWords {
    pub const fn lines(&self) -> [&'static str; 7] {
        [
            self.help,
            self.name_hint,
            self.code_hint,
            self.note_hint,
            self.add,
            self.remove,
            self.empty,
        ]
    }
}

pub const ALIASES: AliasWords = AliasWords {
    help: "A short name for a code you open often. Type the name and you get the results \
           for the code, on the second keystroke.",
    name_hint: "Short name",
    code_hint: "What to search for",
    note_hint: "What it is (optional)",
    add: "Add",
    remove: "Remove",
    empty: "None yet.",
};

/// And what the drive list says.
pub struct DriveWords {
    pub help: &'static str,
    pub name_hint: &'static str,
    pub path_hint: &'static str,
    pub add: &'static str,
    pub remove: &'static str,
    pub empty: &'static str,
    pub not_there: &'static str,
    pub needs_name: &'static str,
    pub needs_path: &'static str,
    pub name_taken: &'static str,
    /// Asked before a drive is taken off the list.
    pub confirm: &'static str,
    pub confirm_go: &'static str,
    pub confirm_keep: &'static str,
}

impl DriveWords {
    pub const fn lines(&self) -> [&'static str; 13] {
        [
            self.help,
            self.name_hint,
            self.path_hint,
            self.add,
            self.remove,
            self.empty,
            self.not_there,
            self.needs_name,
            self.needs_path,
            self.name_taken,
            self.confirm,
            self.confirm_go,
            self.confirm_keep,
        ]
    }
}

pub const DRIVES: DriveWords = DriveWords {
    help: "Where to search. A change here takes effect when files next starts, and the \
           comments in the configuration file are lost when a drive is edited.",
    name_hint: "Name",
    path_hint: "Folder or drive",
    add: "Add",
    remove: "Remove",
    // New, and needed because the window can now show a state the loader
    // would never produce: `on_drives` replaces the routing table as soon as
    // the last drive is removed, before the save can be refused, so an empty
    // list can be on screen for a moment even though a configuration with no
    // drives will not load.
    empty: "None \u{b7} searching has nothing to look at.",
    not_there: "This is not there right now \u{b7} fine on a laptop, worth checking otherwise",
    needs_name: "a drive needs a name",
    needs_path: "a drive needs a folder to search",
    name_taken: "that name is already a drive",
    // The one question this window asks. A form that confirms everything
    // teaches people to dismiss the question without reading it, so this is
    // spent on the one action that cannot be undone: the path is the part
    // nobody remembers, and rewriting the drive list costs the comments
    // around it in the configuration file.
    confirm: "Stop searching this drive? The folder it points at is not touched, but \
              getting it back means typing the path again.",
    confirm_go: "Remove it",
    confirm_keep: "Keep it",
};
