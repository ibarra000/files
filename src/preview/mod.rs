//! What a result *is*, before anybody opens it.
//!
//! A [`crate::search::matcher::Hit`] carries a path, a name and where the match
//! fell. That is everything the list needs to draw a row and nothing a person
//! needs to decide between two rows. On a share where a drawing set is stored
//! one file per page, `11-D-0704_Page3.pdf` and `11-D-0704_Page30.pdf` differ
//! by one character and by nothing else on screen, and the only way to tell
//! which was wanted is to open one and look.
//!
//! So this answers the question the list cannot: how big, how old, which share,
//! which folder, and - for a document stored as pages - how many pages there
//! are and what they are called.
//!
//! # Facts, and deliberately not a picture
//!
//! A rendered first page was considered and is not here. Nothing in Rust reads
//! a PDF into pixels, so a thumbnail means running somebody else's program,
//! which means a configured command, an environment variable, a cache
//! directory, a timeout and a decoder dependency. That exact apparatus was
//! built for DWG conversion and deleted again in `93b5dc4`, on the grounds that
//! it was a great deal of machinery spent reaching a program most machines have
//! not got. Nothing about pointing it at a rasteriser instead changes that
//! arithmetic, so the seam is left clean rather than re-cut.
//!
//! What is here costs one `metadata` call and a sweep of an index already in
//! memory, and it works the same on a `.dwg` as on a `.pdf` - which a
//! rasteriser would not have.
//!
//! # Why the worker, for so little work
//!
//! Because `metadata` is a round trip to an SMB share, and the drawing shares
//! this searches are reached over a VPN as often as over the LAN. Thirty
//! milliseconds is nothing on a thread of its own and a stutter in every
//! keystroke on the drawing thread. See [`worker`].

pub mod worker;

use std::sync::Arc;
use std::time::SystemTime;

use crate::index::snapshot::Snapshot;
use crate::search::pages;
use crate::util::cancel::CancelToken;

/// How many sibling pages are named before the rest become a count.
///
/// Four, which is what the pane has room for under the facts without pushing
/// the folder off the bottom. The count that follows is the part that matters -
/// "13 pages" is the fact somebody is checking - and the names are there to
/// confirm the set is the one they meant.
pub const NAMED_PAGES: usize = 4;

/// What is known about one file.
///
/// Every field is optional because every field comes from somewhere that can be
/// unavailable: a share that has gone away, a file deleted between the search
/// and the pointer reaching it, an index that has not been built. A preview
/// missing its size is worth drawing; a preview that refuses to appear because
/// one call failed is not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub bytes: Option<u64>,
    pub modified: Option<SystemTime>,
    /// Which configured share it came from, by name.
    pub share: Option<Box<str>>,
    /// The folder it sits in, already stripped of the file name.
    pub folder: Option<Box<str>>,
    /// Whether the file was there to be asked about.
    ///
    /// Distinct from every field being `None`, and the distinction is the
    /// useful one: "this is gone" is an answer, and "the share did not respond"
    /// is a different answer that should not be drawn as the first.
    pub missing: bool,
}

/// The pages of the document this file belongs to, when it belongs to one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pages {
    /// How many there are in total, including any this did not name.
    pub total: usize,
    /// The first few, in merge order.
    pub named: Vec<Arc<str>>,
    /// The sweep hit its ceiling, so `total` is a floor rather than a count.
    pub capped: bool,
}

/// Everything known about the file under the pointer.
///
/// Keyed by path rather than by row, following `selected_path`: `apply_hits`
/// replaces the result list wholesale, so a rank is stale the instant results
/// land and an answer addressed to one would be drawn against the wrong file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub path: Arc<str>,
    pub name: Arc<str>,
    pub facts: Facts,
    /// `None` when the file is not part of a multi-page document, which is the
    /// ordinary case for everything that is not a drawing set.
    pub pages: Option<Pages>,
}

impl Preview {
    /// A preview for a file nothing could be learned about.
    ///
    /// Drawn rather than withheld: the name and the folder come from the hit
    /// itself and are worth having on their own, and a pane that empties
    /// whenever a share is slow is a pane nobody trusts.
    pub fn unknown(path: Arc<str>, name: Arc<str>) -> Self {
        Self {
            path,
            name,
            facts: Facts::default(),
            pages: None,
        }
    }
}

/// What a request needs to be answered, gathered where the answer is drawn.
///
/// The query comes along because a page set is defined against the *code*, not
/// against the file: `pages::collect` asks which files are pages of the thing
/// that was searched for, and the file under the pointer is one of them rather
/// than the definition of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub path: Arc<str>,
    pub name: Arc<str>,
    pub query: String,
}

/// Reads what the filesystem knows about `path`.
///
/// Separate from [`describe`] so the expensive half can be skipped in a test:
/// everything below this line is arithmetic on values, and only this touches a
/// share.
pub fn facts_of(path: &str) -> Facts {
    match std::fs::metadata(path) {
        Ok(meta) => Facts {
            bytes: Some(meta.len()),
            modified: meta.modified().ok(),
            share: None,
            folder: None,
            missing: false,
        },
        Err(e) => Facts {
            missing: e.kind() == std::io::ErrorKind::NotFound,
            ..Facts::default()
        },
    }
}

/// The pages of `query`'s document, if `path` is one of them.
///
/// `None` rather than an empty group when the file stands alone, because the
/// two mean different things on screen: no pages is "this is one file", and a
/// group of one would read as "a document of one page", which is a claim about
/// a naming scheme this file does not follow.
///
/// The membership test is the load-bearing half. `pages::collect` answers for
/// the *code*, so without checking that `path` is in the result, typing
/// `11-D-0704` and hovering an unrelated `notes.txt` in the same folder would
/// tell you it was page one of a thirteen-page drawing set.
pub fn pages_of(snap: &Snapshot, path: &str, query: &str, cancel: &CancelToken) -> Option<Pages> {
    let group = pages::collect(snap, query, cancel);
    if group.cancelled || group.len() < 2 || !group.contains_path(path) {
        return None;
    }
    Some(Pages {
        total: group.len(),
        named: group
            .pages
            .iter()
            .take(NAMED_PAGES)
            .map(|p| Arc::clone(&p.name))
            .collect(),
        capped: group.capped,
    })
}

/// Which share a path belongs to, and the folder inside it.
///
/// Both come from the configuration rather than from the filesystem, so this
/// still answers for a share that has gone offline - which is exactly when
/// somebody is looking at the pane wondering where the file was supposed to be.
pub fn locate(routes: &crate::paths::Routes, path: &str) -> (Option<Box<str>>, Option<Box<str>>) {
    let folder = crate::view::row::folder(path);
    let folder = (!folder.is_empty()).then(|| folder.into());

    let share = routes
        .enabled()
        .find(|m| crate::util::winpath::contains(&m.path, std::path::Path::new(path)))
        .map(|m| m.name.clone());

    (share, folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::{MappingKind, Routes};

    #[test]
    fn a_file_that_is_not_there_is_reported_as_missing_rather_than_unknown() {
        let facts = facts_of(r"Z:\no\such\file-9f3a.pdf");
        assert!(facts.missing, "a deleted file should say so");
        assert_eq!(facts.bytes, None);
    }

    #[test]
    fn a_real_file_reports_its_size() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("11-D-0704.pdf");
        std::fs::write(&path, b"1234567890").expect("the write succeeded");

        let facts = facts_of(&path.to_string_lossy());
        assert!(!facts.missing);
        assert_eq!(facts.bytes, Some(10));
        assert!(facts.modified.is_some(), "no modification time");
    }

    #[test]
    fn a_path_is_placed_in_the_share_that_contains_it() {
        let routes = Routes::single("jobs", r"R:\".into(), MappingKind::Tree);
        let (share, folder) = locate(&routes, r"R:\11d\11-D-0704\11-D-0704.pdf");

        assert_eq!(share.as_deref(), Some("jobs"));
        assert_eq!(folder.as_deref(), Some(r"R:\11d\11-D-0704"));
    }

    /// A path from a share nobody configured still names its folder. The pane
    /// is drawn from a hit that already exists, so refusing to describe it
    /// because the routing table has moved on would blank the pane at exactly
    /// the moment somebody is asking why.
    #[test]
    fn a_path_outside_every_share_still_names_its_folder() {
        let routes = Routes::single("jobs", r"R:\".into(), MappingKind::Tree);
        let (share, folder) = locate(&routes, r"V:\elsewhere\thing.pdf");

        assert_eq!(share, None);
        assert_eq!(folder.as_deref(), Some(r"V:\elsewhere"));
    }

    #[test]
    fn a_preview_for_a_file_nothing_is_known_about_still_carries_its_name() {
        let preview = Preview::unknown("R:\\a\\b.pdf".into(), "b.pdf".into());
        assert_eq!(&*preview.name, "b.pdf");
        assert_eq!(preview.pages, None);
        assert!(!preview.facts.missing, "unknown is not the same as gone");
    }
}
