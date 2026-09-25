//! Finding out that a newer version exists.
//!
//! Two places to look, and one is chosen by [`Source::of`]: a share, if
//! `update_from` names one, and otherwise the GitHub repository this build
//! came from - unless `check_for_updates` is off. Both publish the same
//! `latest.toml` beside the same installer, so both answer with the same
//! [`Found`], and nothing after the look knows which it was.
//!
//! # Why a share is still the first choice
//!
//! The rest of this note is the argument for the share, and it still holds
//! where there is one: an office that runs its own update folder decides when
//! its machines move to a new version, and GitHub would take that decision
//! away. GitHub is for everybody else - the laptop with no share to read,
//! which before this was never told about a new version at all. See
//! [`github`] for what it does differently, and [`http`] for why it needs no
//! new dependency.
//!
//! # Why a file share rather than a URL
//!
//! This program already lives in an SMB world: every drive it searches is one,
//! and every machine it runs on is on the domain that serves them. So the
//! update folder is one more share, read with `std::fs` like anything else.
//!
//! That buys a great deal for a feature this small. No HTTP client and no TLS
//! stack - the two largest dependencies this would otherwise take, into a
//! crate whose manifest argues for each one it has. No certificate to renew,
//! no server to run, and no second set of credentials: whoever may read the
//! share may take the update, which is a decision an administrator already
//! made with tools they already use.
//!
//! What it costs is a laptop off the domain, which finds the share missing and
//! says so quietly. That is the right failure for a machine that also cannot
//! reach any of the drives it searches.
//!
//! # What this module will not do
//!
//! It does not install anything on its own, and it does not check at startup.
//! An update that arrives unasked is one that interrupts somebody mid-search,
//! and a check in the startup path is an SMB round trip - or an SMB timeout -
//! in front of a window somebody is waiting for.

pub mod apply;
pub mod check;
pub mod github;
pub mod http;
pub mod manifest;
pub mod version;

pub use manifest::Manifest;
pub use version::Version;

use std::path::{Path, PathBuf};

use crate::config::Settings;

/// What the manifest is called, in the configured folder.
pub const MANIFEST_NAME: &str = "latest.toml";

/// What a look at the update folder found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A newer version, and where its installer is.
    Available {
        manifest: Manifest,
        msi: PathBuf,
        /// Whether that installer is actually on disk, answered once here
        /// rather than at every reader.
        ///
        /// The About page asks this twice a frame - once to decide whether
        /// to warn and once to decide whether to offer the button - and the
        /// settings window is redrawn continuously while it is open. That
        /// is two `stat` calls per frame against a network share, which is
        /// two round trips per frame on a laptop over a VPN. It is answered
        /// when the manifest is read, which is once every four hours, and
        /// being a few hours stale is exactly as wrong as the manifest
        /// beside it.
        msi_present: bool,
    },
    /// The folder was read and holds nothing newer than this build.
    UpToDate,
    /// The folder could not be read, or what it held could not be used.
    ///
    /// Not an error anybody is made to act on: a share that is unreachable
    /// because the laptop is at home is the ordinary case, and the only honest
    /// response is a quiet line rather than a dialog.
    Unavailable { detail: String },
}

/// Where updates come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A share folder holding `latest.toml` and the installer.
    Folder(PathBuf),
    /// The releases of a GitHub repository.
    GitHub(github::Repo),
}

impl Source {
    /// Where these settings say to look, if anywhere.
    ///
    /// A share wins when there is one, because somebody configured it on
    /// purpose. Otherwise GitHub, from `Cargo.toml`'s `repository` - or from
    /// `FILES_UPDATE_GITHUB`, which exists so a release can be tried against
    /// a scratch repository before it is published to the real one.
    pub fn of(settings: &Settings) -> Option<Self> {
        if let Some(folder) = &settings.update_from {
            return Some(Self::Folder(folder.clone()));
        }
        if !settings.check_for_updates {
            return None;
        }
        let repo = crate::config::env_str("FILES_UPDATE_GITHUB")
            .and_then(|r| github::Repo::parse(&r))
            .or_else(github::Repo::ours)?;
        Some(Self::GitHub(repo))
    }

    /// Where, in words, for the About page and `--doctor`.
    pub fn describe(&self) -> String {
        match self {
            Self::Folder(folder) => folder.display().to_string(),
            Self::GitHub(repo) => format!("GitHub \u{b7} {repo}"),
        }
    }

    /// Whether this is the internet rather than a share.
    pub const fn is_remote(&self) -> bool {
        matches!(self, Self::GitHub(_))
    }
}

/// Looks wherever `source` says, and says what it means for this build.
///
/// `cache_dir` is where a GitHub installer is downloaded to; a share's is
/// read where it stands.
pub fn look_at(source: &Source, running: Version, cache_dir: Option<&Path>) -> Found {
    match source {
        Source::Folder(folder) => look(folder, running),
        Source::GitHub(repo) => github::look(repo, running, &http::WinHttp, cache_dir),
    }
}

/// Reads the manifest in `folder` and says what it means for this build.
///
/// Takes the running version rather than reading it, so the decision is
/// testable without being the version this happens to be compiled as.
pub fn look(folder: &Path, running: Version) -> Found {
    let path = folder.join(MANIFEST_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            return Found::Unavailable {
                detail: format!("{} could not be read: {e}", path.display()),
            };
        }
    };

    let manifest = match manifest::parse(&text) {
        Ok(manifest) => manifest,
        Err(problem) => {
            return Found::Unavailable {
                detail: problem.detail(),
            };
        }
    };

    if !manifest.version.is_newer_than(running) {
        return Found::UpToDate;
    }

    // Joined here rather than by the caller, so the one place that has already
    // proved the name is a bare one is the place that builds the path from it.
    let msi = folder.join(&manifest.msi);
    // Asked once, here, rather than at every reader on every frame. See the
    // field's own note.
    let msi_present = msi.is_file();
    Found::Available {
        manifest,
        msi,
        msi_present,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A share somebody configured outranks the internet.
    #[test]
    fn a_configured_share_is_looked_in_rather_than_github() {
        let settings = Settings {
            update_from: Some(PathBuf::from(r"\\server\software\files")),
            ..Settings::default()
        };
        assert!(matches!(Source::of(&settings), Some(Source::Folder(_))));
    }

    /// Out of the box, the repository this build came from.
    #[test]
    fn with_no_share_github_is_looked_at_by_default() {
        let source = Source::of(&Settings::default());
        assert!(matches!(source, Some(Source::GitHub(_))), "{source:?}");
        assert!(source.unwrap().describe().starts_with("GitHub"));
    }

    /// And turning it off means nothing is looked at at all.
    #[test]
    fn turning_the_check_off_looks_nowhere() {
        let settings = Settings {
            check_for_updates: false,
            ..Settings::default()
        };
        assert_eq!(Source::of(&settings), None);
    }

    fn folder(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_NAME), manifest).unwrap();
        dir
    }

    const PUBLISHED: &str = "version = \"0.3.0\"\nmsi = \"files-0.3.0-x64.msi\"\n";

    #[test]
    fn a_newer_version_is_offered_with_the_installer_beside_it() {
        let dir = folder(PUBLISHED);
        match look(dir.path(), Version::new(0, 2, 0)) {
            Found::Available { manifest, msi, .. } => {
                assert_eq!(manifest.version, Version::new(0, 3, 0));
                assert_eq!(msi, dir.path().join("files-0.3.0-x64.msi"));
            }
            other => panic!("expected an update, got {other:?}"),
        }
    }

    #[test]
    fn the_same_version_is_up_to_date() {
        let dir = folder(PUBLISHED);
        assert_eq!(look(dir.path(), Version::new(0, 3, 0)), Found::UpToDate);
    }

    /// A share rolled back must not offer to install backwards - the
    /// installer refuses a downgrade, so the prompt could not be satisfied.
    #[test]
    fn an_older_published_version_is_up_to_date() {
        let dir = folder(PUBLISHED);
        assert_eq!(look(dir.path(), Version::new(1, 0, 0)), Found::UpToDate);
    }

    /// The ordinary case for a laptop at home, and it must be quiet.
    #[test]
    fn a_folder_that_is_not_there_is_unavailable_rather_than_an_error() {
        let found = look(
            Path::new(r"C:\definitely-not-here-7713"),
            Version::new(0, 2, 0),
        );
        match found {
            Found::Unavailable { detail } => assert!(!detail.is_empty()),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[test]
    fn a_manifest_that_cannot_be_used_says_why() {
        let dir = folder("version = \"0.3\"\nmsi = \"a.msi\"\n");
        match look(dir.path(), Version::new(0, 2, 0)) {
            Found::Unavailable { detail } => assert!(detail.contains("0.3"), "{detail}"),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    /// The installer path is built from a name the parser has already proved
    /// cannot leave the folder.
    #[test]
    fn the_installer_is_always_inside_the_folder_that_named_it() {
        let dir = folder(PUBLISHED);
        let Found::Available { msi, .. } = look(dir.path(), Version::new(0, 2, 0)) else {
            panic!("expected an update");
        };
        assert_eq!(msi.parent(), Some(dir.path()));
    }
}
