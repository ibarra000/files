//! The programs somebody has named to open their files with.
//!
//! Nothing here is built in. This program used to know about exactly one
//! viewer, `avwin.exe`, and offered it to everybody - on F2, on `Ctrl+E`, in
//! the menu and as the home for drawings in PDF mode - although most machines
//! have never had it installed. A viewer is the user's to name now, in
//! `[[viewer]]` tables in `config.toml`, and the program offers exactly the
//! ones they wrote down, in the order they wrote them.
//!
//! # What `extensions` means
//!
//! The types PDF mode hands to this program rather than trying to assemble
//! them. PDF mode treats a code as naming a document and merges its pages, and
//! a drawing is the thing it cannot merge - there is no Rust that reads DWG.
//! A viewer that claims `dwg` is where those go; without one they go to
//! whatever Windows has registered for them. `Auto` never reads this list: it
//! hands every file to Windows, which is what it means.

use std::sync::Arc;

/// One program, as the configuration names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerApp {
    /// What F2, the menu and `viewer = ` call it.
    pub name: Arc<str>,
    /// A bare name looked up on `PATH`, or a full path.
    pub program: Arc<str>,
    /// Lower case, no dot.
    pub extensions: Box<[Box<str>]>,
}

impl ViewerApp {
    pub fn new(name: &str, program: &str, extensions: &[&str]) -> Self {
        Self {
            name: name.trim().into(),
            program: program.trim().into(),
            extensions: extensions
                .iter()
                .map(|e| Box::from(e.trim().trim_start_matches('.').to_ascii_lowercase()))
                .collect(),
        }
    }

    /// Whether PDF mode should send a file with extension `ext` here.
    pub fn claims(&self, ext: &str) -> bool {
        self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
    }
}

/// The configured viewers. Immutable once loaded, shared by every thread.
///
/// A slice in configuration order, like [`crate::alias::Aliases`]: the order
/// somebody wrote them is the order F2 visits them, and the first one is the
/// one `Ctrl+E` opens.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Viewers {
    entries: Box<[ViewerApp]>,
}

impl Viewers {
    pub fn new(entries: Vec<ViewerApp>) -> Self {
        Self {
            entries: entries.into_boxed_slice(),
        }
    }

    pub fn all(&self) -> &[ViewerApp] {
        &self.entries
    }

    pub fn get(&self, index: usize) -> Option<&ViewerApp> {
        self.entries.get(index)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The position of the viewer called `name`, case aside.
    pub fn find(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        self.entries
            .iter()
            .position(|v| v.name.eq_ignore_ascii_case(name))
    }

    /// The first viewer that claims files with extension `ext`.
    pub fn claiming(&self, ext: &str) -> Option<usize> {
        self.entries.iter().position(|v| v.claims(ext))
    }

    /// The names, for an error message that has to say what was expected.
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|v| v.name.as_ref()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, program: &str, extensions: &[&str]) -> ViewerApp {
        ViewerApp::new(name, program, extensions)
    }

    #[test]
    fn a_viewer_is_found_by_name_whatever_its_case() {
        let v = Viewers::new(vec![
            app("avwin", "avwin.exe", &["dwg"]),
            app("Notes", "notepad.exe", &[]),
        ]);
        assert_eq!(v.find("AVWIN"), Some(0));
        assert_eq!(v.find(" notes "), Some(1));
        assert_eq!(v.find("sumatra"), None);
    }

    #[test]
    fn the_first_viewer_to_claim_an_extension_wins() {
        let v = Viewers::new(vec![
            app("a", "a.exe", &["txt"]),
            app("b", "b.exe", &["dwg", "txt"]),
        ]);
        assert_eq!(v.claiming("txt"), Some(0));
        assert_eq!(v.claiming("DWG"), Some(1));
        assert_eq!(v.claiming("pdf"), None);
    }

    #[test]
    fn no_viewers_claim_nothing() {
        let v = Viewers::default();
        assert!(v.is_empty());
        assert_eq!(v.claiming("dwg"), None);
        assert_eq!(v.find("avwin"), None);
    }
}
