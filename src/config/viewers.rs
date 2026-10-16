//! User-configured viewer programs, from `[[viewer]]` tables in `config.toml`.
//!
//! No viewer is built in; the program offers exactly the configured ones, in
//! configuration order.
//!
//! # `extensions`
//!
//! The file types PDF mode sends to the viewer instead of merging them, such as
//! `tif`, which PDF mode cannot read. Unclaimed types go to the Windows
//! association. `Auto` ignores this list and always uses Windows.

use std::sync::Arc;

/// One configured viewer program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerApp {
    /// The display name used by F2, the menu and `viewer = `.
    pub name: Arc<str>,
    /// A bare name looked up on `PATH`, or a full path.
    pub program: Arc<str>,
    /// Claimed extensions, lowercase without the dot.
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

/// The configured viewers, immutable once loaded and shared across threads.
///
/// Kept in configuration order: F2 cycles in that order and `Ctrl+E` opens the
/// first.
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

    /// Returns the index of the viewer named `name`, ignoring ASCII case.
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

    /// Returns the viewer names, for error messages.
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
            app("sketch", "sketch.exe", &["tif"]),
            app("Notes", "notepad.exe", &[]),
        ]);
        assert_eq!(v.find("SKETCH"), Some(0));
        assert_eq!(v.find(" notes "), Some(1));
        assert_eq!(v.find("sumatra"), None);
    }

    #[test]
    fn the_first_viewer_to_claim_an_extension_wins() {
        let v = Viewers::new(vec![
            app("a", "a.exe", &["txt"]),
            app("b", "b.exe", &["tif", "txt"]),
        ]);
        assert_eq!(v.claiming("txt"), Some(0));
        assert_eq!(v.claiming("TIF"), Some(1));
        assert_eq!(v.claiming("pdf"), None);
    }

    #[test]
    fn no_viewers_claim_nothing() {
        let v = Viewers::default();
        assert!(v.is_empty());
        assert_eq!(v.claiming("tif"), None);
        assert_eq!(v.find("sketch"), None);
    }
}
