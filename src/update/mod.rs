//! Checking for a newer version.
//!
//! [`Source::of`] picks the share named by `update_from` if set, otherwise the
//! GitHub repository this build came from (unless `check_for_updates` is off).
//! Both publish `latest.toml` beside the installer and yield the same
//! [`Found`]. A configured share takes precedence so an administrator controls
//! when machines upgrade. See [`github`] and [`http`] for the GitHub path.
//!
//! A share is read with `std::fs`, needing no HTTP or TLS stack and no extra
//! credentials; an unreachable share is reported quietly.
//!
//! Nothing is installed automatically and nothing is checked at startup, so
//! the window never waits on an SMB round trip.

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

/// File name of the manifest in the update folder.
pub const MANIFEST_NAME: &str = "latest.toml";

/// Result of checking for an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A newer version, and where its installer is.
    Available {
        manifest: Manifest,
        msi: PathBuf,
        /// Whether the installer exists, checked once when the manifest is
        /// read so the About page does not `stat` a network share every frame.
        msi_present: bool,
    },
    /// Nothing newer than this build.
    UpToDate,
    /// The source could not be read or its contents were unusable. Shown as a
    /// quiet status line; an unreachable share is the ordinary off-site case.
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
    /// Returns the configured update source, if any.
    ///
    /// A configured share wins. Otherwise GitHub, from `Cargo.toml`'s
    /// `repository` or `FILES_UPDATE_GITHUB` (for testing against a scratch
    /// repository).
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

    /// Returns a description for the About page and `--doctor`.
    pub fn describe(&self) -> String {
        match self {
            Self::Folder(folder) => folder.display().to_string(),
            Self::GitHub(repo) => format!("GitHub \u{b7} {repo}"),
        }
    }

    /// Whether the source is on the internet rather than a share.
    pub const fn is_remote(&self) -> bool {
        matches!(self, Self::GitHub(_))
    }
}

/// Checks `source` against the running version.
///
/// `cache_dir` receives a downloaded GitHub installer; a share's installer is
/// used in place.
pub fn look_at(source: &Source, running: Version, cache_dir: Option<&Path>) -> Found {
    match source {
        Source::Folder(folder) => look(folder, running),
        Source::GitHub(repo) => github::look(repo, running, &http::WinHttp, cache_dir),
    }
}

/// Reads the manifest in `folder` and compares it with `running`.
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

    // `manifest::parse` has verified `msi` is a bare file name.
    let msi = folder.join(&manifest.msi);
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

    #[test]
    fn a_configured_share_is_looked_in_rather_than_github() {
        let settings = Settings {
            update_from: Some(PathBuf::from(r"\\server\software\files")),
            ..Settings::fixture()
        };
        assert!(matches!(Source::of(&settings), Some(Source::Folder(_))));
    }

    #[test]
    fn with_no_share_github_is_looked_at_by_default() {
        let source = Source::of(&Settings::fixture());
        assert!(matches!(source, Some(Source::GitHub(_))), "{source:?}");
        assert!(source.unwrap().describe().starts_with("GitHub"));
    }

    #[test]
    fn turning_the_check_off_looks_nowhere() {
        let settings = Settings {
            check_for_updates: false,
            ..Settings::fixture()
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

    /// The installer refuses downgrades, so an older version is not offered.
    #[test]
    fn an_older_published_version_is_up_to_date() {
        let dir = folder(PUBLISHED);
        assert_eq!(look(dir.path(), Version::new(1, 0, 0)), Found::UpToDate);
    }

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

    #[test]
    fn the_installer_is_always_inside_the_folder_that_named_it() {
        let dir = folder(PUBLISHED);
        let Found::Available { msi, .. } = look(dir.path(), Version::new(0, 2, 0)) else {
            panic!("expected an update");
        };
        assert_eq!(msi.parent(), Some(dir.path()));
    }
}
