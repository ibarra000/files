//! The wording for the alias and drive lists, which have several values per
//! entry and so cannot be [`super::Row`]s.
//!
//! Each struct's `lines()` enumerates every string so [`super::Page::prose`]
//! includes all of them in the house-style check.

/// The alias list's wording.
pub struct AliasWords {
    pub help: &'static str,
    pub name_hint: &'static str,
    pub term_hint: &'static str,
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
            self.term_hint,
            self.note_hint,
            self.add,
            self.remove,
            self.empty,
        ]
    }
}

pub const ALIASES: AliasWords = AliasWords {
    help: "A short name for a frequent search. Typing the name shows that search's \
           results immediately.",
    name_hint: "Short name",
    term_hint: "Search",
    note_hint: "Note (optional)",
    add: "Add",
    remove: "Remove",
    empty: "None yet.",
};

/// The drive list's wording.
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
    /// The confirmation asked before a drive is removed.
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
    help: "Drives to search. Changes take effect the next time files starts. Editing a \
           drive removes comments from the configuration file.",
    name_hint: "Name",
    path_hint: "Folder or drive",
    add: "Add",
    remove: "Remove",
    // `on_drives` replaces the routing table before the save can be refused,
    // so an empty list can appear briefly even though such a file will not
    // load.
    empty: "None \u{b7} add a drive to start searching.",
    not_there: "Not reachable right now \u{b7} expected when off the network",
    needs_name: "a drive needs a name",
    needs_path: "a drive needs a folder to search",
    name_taken: "that name is already a drive",
    // The window's only confirmation: removal loses the path and the
    // surrounding comments in the configuration file.
    confirm: "Remove this drive? Its files are not affected, but the path must be \
              entered again to add it back.",
    confirm_go: "Remove drive",
    confirm_keep: "Cancel",
};
