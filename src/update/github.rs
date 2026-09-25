//! Looking for a newer version among a GitHub repository's releases.
//!
//! The release carries the same two files a share folder does: the installer,
//! and the `latest.toml` that `tools/make_msi.ps1 -Release` writes beside it.
//! So this reads the same manifest with the same parser, and the one thing it
//! adds is getting them off the internet. No JSON, and no API: GitHub serves
//! the newest release's assets at a fixed address,
//! `releases/latest/download/<name>`, which is the whole of what is needed.
//!
//! # Why the installer is downloaded when it is found
//!
//! Everything downstream of a look - the About page's button, the tray's menu
//! item, [`super::apply::stage`] - was written for a share, where the
//! installer is a file that is simply there. Fetching it here, on the
//! checker's thread, keeps that true: what comes back is the same
//! [`Found::Available`] a share produces, pointing at a file on this machine,
//! and nothing after it needs to know the network was involved. It also means
//! "Install and restart" is instant rather than a download with no progress.
//!
//! # Why it must name a digest
//!
//! A share is one an administrator controls. A download is bytes from the
//! internet, and the `sha256` in the manifest is the only thing that ties the
//! installer to the release that named it. So a release without one is not
//! offered, and a download that does not match it is deleted. `stage` checks
//! the digest again before anything runs, which is deliberate: this check is
//! about not keeping a bad download, that one is about not installing one.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::http::{Fetch, HttpError};
use super::{Found, MANIFEST_NAME, Version, manifest};

/// The most a manifest may be. It is four lines.
const MANIFEST_LIMIT: u64 = 64 * 1024;

/// The most an installer may be. It is a few megabytes; this is a ceiling
/// against something that is not an installer at all.
const MSI_LIMIT: u64 = 200 * 1024 * 1024;

/// Where downloads go, under the cache folder's `update`.
const DOWNLOAD_DIR: &str = "download";

/// How old a scratch file must be before it is taken for a dead download's.
///
/// A day: an installer over the slowest VPN arrives in minutes, so nothing
/// that old is still being written.
const STALE_SCRATCH: Duration = Duration::from_secs(24 * 60 * 60);

/// `owner/name` on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    owner: String,
    name: String,
}

impl Repo {
    /// `owner/name`, or a `https://github.com/owner/name` address.
    ///
    /// Strict about the characters, because both halves go into a URL this
    /// program builds, and GitHub's own rules - letters, digits, `-`, `_`,
    /// `.` - are narrower than anything that would need escaping.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text
            .strip_prefix("https://github.com/")
            .unwrap_or(text)
            .trim_end_matches('/');
        let text = text.strip_suffix(".git").unwrap_or(text);
        let (owner, name) = text.split_once('/')?;
        let fine = |part: &str| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        (fine(owner) && fine(name)).then(|| Self {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    /// The repository this build came from, off `Cargo.toml`'s `repository`.
    pub fn ours() -> Option<Self> {
        Self::parse(env!("CARGO_PKG_REPOSITORY"))
    }

    /// The newest release's manifest.
    ///
    /// "Latest" is GitHub's: the newest release that is neither a draft nor
    /// a pre-release, so publishing either of those offers nobody anything.
    fn manifest_url(&self) -> String {
        format!(
            "https://github.com/{}/{}/releases/latest/download/{MANIFEST_NAME}",
            self.owner, self.name
        )
    }

    /// An asset of the release tagged for `version`.
    ///
    /// By tag rather than through "latest" again, so the installer is the one
    /// the manifest just read belongs to even if a release is published in
    /// the moment between the two requests.
    fn asset_url(&self, version: Version, file: &str) -> String {
        format!(
            "https://github.com/{}/{}/releases/download/v{version}/{file}",
            self.owner, self.name
        )
    }
}

impl fmt::Display for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Reads the newest release of `repo` and says what it means for this build,
/// downloading its installer into `cache_dir` when it is newer.
pub fn look(repo: &Repo, running: Version, fetch: &dyn Fetch, cache_dir: Option<&Path>) -> Found {
    let unavailable = |detail: String| Found::Unavailable { detail };

    let text = match fetch.get(&repo.manifest_url(), MANIFEST_LIMIT) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return unavailable(format!("The {MANIFEST_NAME} on GitHub is not text")),
        },
        // No release yet, or a release published without a manifest. Both
        // are ordinary for a repository that has only just started
        // publishing, and neither is anything this machine can fix.
        Err(HttpError::Status(404)) => {
            return unavailable(format!(
                "No release on GitHub has a {MANIFEST_NAME} yet, so there is nothing to offer"
            ));
        }
        Err(e) => return unavailable(format!("GitHub could not be reached: {e}")),
    };

    let manifest = match manifest::parse(&text) {
        Ok(manifest) => manifest,
        Err(problem) => return unavailable(problem.detail()),
    };
    if !manifest.version.is_newer_than(running) {
        return Found::UpToDate;
    }

    let Some(sha256) = manifest.sha256.clone() else {
        return unavailable(format!(
            "Version {} on GitHub names no sha256, so its installer cannot be checked and is \
             not offered",
            manifest.version
        ));
    };
    let Some(cache_dir) = cache_dir else {
        return unavailable("There is no cache folder to download the installer into".into());
    };

    let dir = cache_dir.join(super::apply::STAGE_DIR).join(DOWNLOAD_DIR);
    let msi = dir.join(&manifest.msi);
    let msi_present = fetch_installer(repo, &manifest, &sha256, &dir, &msi, fetch);
    Found::Available {
        manifest,
        msi,
        msi_present,
    }
}

/// Makes sure the installer is at `msi` and matches `sha256`. Whether it is.
///
/// Already there and right is kept, so every look after the first costs one
/// small request. Anything else is fetched to a scratch name and renamed into
/// place only once it has been checked, so a half-finished download is never
/// at the name the installer is looked for under.
fn fetch_installer(
    repo: &Repo,
    manifest: &manifest::Manifest,
    sha256: &str,
    dir: &Path,
    msi: &Path,
    fetch: &dyn Fetch,
) -> bool {
    let matches = |path: &Path| super::apply::sha256_of(path).is_ok_and(|d| d == sha256);
    if matches(msi) {
        return true;
    }
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    sweep_stale(dir);
    let part = scratch_for(manifest, dir);
    let url = repo.asset_url(manifest.version, &manifest.msi);
    let fetched = fetch.download(&url, &part, MSI_LIMIT).is_ok() && matches(&part);
    let placed = fetched && std::fs::rename(&part, msi).is_ok();
    if !placed {
        let _ = std::fs::remove_file(&part);
    }
    // Another attempt may have renamed its copy into place first, which is as
    // good as this one having done it.
    placed || matches(msi)
}

/// A scratch file no other download is using.
///
/// The process id, because the panel and the settings window both look. And
/// a count, because one process can look twice at once: "Check now" pressed
/// again before the first check answers is a second thread in the settings
/// window, and two downloads into one scratch file truncate - or delete -
/// each other halfway through.
fn scratch_for(manifest: &manifest::Manifest, dir: &Path) -> std::path::PathBuf {
    static ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let attempt = ATTEMPT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        "{}.{}.{attempt}.part",
        manifest.msi,
        std::process::id()
    ))
}

/// Deletes scratch files a crash left behind.
///
/// Every finished download is renamed or removed, so a `.part` file that
/// outlives its download is one whose process died mid-way - and without
/// this, each one would sit in the cache for good, up to [`MSI_LIMIT`] apiece.
/// Only old ones, so a download still under way in the other process is
/// never pulled out from under it.
fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let scratch = path.extension().is_some_and(|e| e == "part");
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > STALE_SCRATCH);
        if scratch && old {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A network made of a table: each URL answers with its bytes, or a
    /// status, and every request is written down.
    #[derive(Default)]
    struct Fake {
        answers: HashMap<String, Result<Vec<u8>, HttpError>>,
        asked: RefCell<Vec<String>>,
    }

    impl Fake {
        fn answer(mut self, url: String, with: Result<&[u8], HttpError>) -> Self {
            self.answers.insert(url, with.map(<[u8]>::to_vec));
            self
        }

        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }

    impl Fetch for Fake {
        fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>, HttpError> {
            self.asked.borrow_mut().push(url.to_string());
            let body = self
                .answers
                .get(url)
                .cloned()
                .unwrap_or(Err(HttpError::Status(404)))?;
            if body.len() as u64 > limit {
                return Err(HttpError::TooLarge(limit));
            }
            Ok(body)
        }

        fn download(&self, url: &str, to: &Path, limit: u64) -> Result<(), HttpError> {
            let body = self.get(url, limit)?;
            std::fs::write(to, body).map_err(|e| HttpError::Failed(e.to_string()))
        }
    }

    const INSTALLER: &[u8] = b"pretend this is an msi";

    fn repo() -> Repo {
        Repo::parse("owner/files").unwrap()
    }

    fn digest_of(bytes: &[u8]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        std::fs::write(&path, bytes).unwrap();
        super::super::apply::sha256_of(&path).unwrap()
    }

    fn manifest(version: &str, sha: Option<&str>) -> String {
        let mut text = format!("version = \"{version}\"\nmsi = \"files-{version}-x64.msi\"\n");
        if let Some(sha) = sha {
            text.push_str(&format!("sha256 = \"{sha}\"\n"));
        }
        text
    }

    /// A release that has the manifest and the installer it names.
    fn published(version: &str, installer: &[u8]) -> Fake {
        let v = Version::parse(version).unwrap();
        let text = manifest(version, Some(&digest_of(INSTALLER)));
        Fake::default()
            .answer(repo().manifest_url(), Ok(text.as_bytes()))
            .answer(
                repo().asset_url(v, &format!("files-{version}-x64.msi")),
                Ok(installer),
            )
    }

    #[test]
    fn a_repository_is_read_from_a_name_or_an_address() {
        let want = Repo::parse("ibarra000/files").unwrap();
        assert_eq!(want.to_string(), "ibarra000/files");
        assert_eq!(
            Repo::parse("https://github.com/ibarra000/files"),
            Some(want.clone())
        );
        assert_eq!(
            Repo::parse("https://github.com/ibarra000/files.git"),
            Some(want)
        );
    }

    /// Both halves go into an address this program builds.
    #[test]
    fn a_repository_name_that_would_need_escaping_is_refused() {
        for bad in [
            "owner",
            "owner/",
            "/files",
            "own er/files",
            "owner/../x",
            "a/b?c",
            "../..",
        ] {
            assert_eq!(Repo::parse(bad), None, "{bad:?} was accepted");
        }
    }

    /// The build knows where it came from.
    #[test]
    fn this_build_names_its_own_repository() {
        assert!(
            Repo::ours().is_some(),
            "Cargo.toml's repository is not a GitHub one"
        );
    }

    /// The whole feature: newer, fetched, checked, and on disk.
    #[test]
    fn a_newer_release_is_offered_with_its_installer_already_downloaded() {
        let cache = tempfile::tempdir().unwrap();
        let fake = published("0.3.0", INSTALLER);

        let found = look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));
        let Found::Available {
            manifest,
            msi,
            msi_present,
        } = found
        else {
            panic!("expected an update, got {found:?}");
        };
        assert_eq!(manifest.version, Version::new(0, 3, 0));
        assert!(msi_present);
        assert_eq!(std::fs::read(&msi).unwrap(), INSTALLER);
        assert!(
            msi.starts_with(cache.path()),
            "{msi:?} is outside the cache"
        );
    }

    /// Four hours later nothing is downloaded again.
    #[test]
    fn an_installer_already_downloaded_is_not_fetched_twice() {
        let cache = tempfile::tempdir().unwrap();
        let fake = published("0.3.0", INSTALLER);
        look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));
        look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));

        let downloads = fake.asked().iter().filter(|u| u.ends_with(".msi")).count();
        assert_eq!(downloads, 1, "asked for {:?}", fake.asked());
    }

    /// "Check now" pressed twice in the settings window is two looks in one
    /// process, at once. Each needs a scratch file of its own, or one
    /// truncates - or deletes - the other's download halfway through.
    #[test]
    fn two_looks_at_once_in_one_process_do_not_share_a_scratch_file() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = manifest::parse(&manifest("0.3.0", None)).unwrap();
        let first = scratch_for(&manifest, dir.path());
        let second = scratch_for(&manifest, dir.path());
        assert_ne!(first, second, "two attempts were given one scratch file");
    }

    /// A download a crash cut short is swept up by the next look, rather than
    /// left in the cache for good.
    #[test]
    fn a_scratch_file_left_by_a_dead_download_is_swept_up() {
        let cache = tempfile::tempdir().unwrap();
        let dir = cache.path().join("update").join(DOWNLOAD_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let orphan = dir.join("files-0.2.9-x64.msi.4242.0.part");
        std::fs::write(&orphan, b"half an installer").unwrap();
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&orphan)
            .unwrap()
            .set_modified(two_days_ago)
            .unwrap();

        let fake = published("0.3.0", INSTALLER);
        look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));
        assert!(!orphan.exists(), "the dead download is still there");
    }

    /// And one that is still being written - by the other process - is not.
    #[test]
    fn a_scratch_file_still_being_written_is_left_alone() {
        let cache = tempfile::tempdir().unwrap();
        let dir = cache.path().join("update").join(DOWNLOAD_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("files-0.3.0-x64.msi.4242.0.part");
        std::fs::write(&live, b"arriving").unwrap();

        let fake = published("0.3.0", INSTALLER);
        look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));
        assert!(live.exists(), "a download in progress was swept away");
    }

    /// The installer is fetched by the tag the manifest names, not through
    /// "latest" a second time.
    #[test]
    fn the_installer_comes_from_the_release_the_manifest_belongs_to() {
        let cache = tempfile::tempdir().unwrap();
        let fake = published("0.3.0", INSTALLER);
        look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));
        assert!(
            fake.asked()
                .iter()
                .any(|u| u.ends_with("/releases/download/v0.3.0/files-0.3.0-x64.msi")),
            "asked for {:?}",
            fake.asked()
        );
    }

    /// Bytes that are not what the manifest promised are not kept.
    #[test]
    fn a_download_that_does_not_match_its_digest_is_not_kept() {
        let cache = tempfile::tempdir().unwrap();
        let fake = published("0.3.0", b"something else entirely");

        let found = look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));
        let Found::Available {
            msi, msi_present, ..
        } = found
        else {
            panic!("expected the update to be named, got {found:?}");
        };
        assert!(!msi_present, "a bad download was offered for installing");
        assert!(
            !msi.exists(),
            "a bad download was left where the installer goes"
        );
        let dir = msi.parent().unwrap();
        let left: Vec<_> = std::fs::read_dir(dir).unwrap().collect();
        assert!(left.is_empty(), "scratch files were left behind: {left:?}");
    }

    /// Without a digest there is nothing to check a download against.
    #[test]
    fn a_release_without_a_digest_is_not_offered() {
        let cache = tempfile::tempdir().unwrap();
        let text = manifest("0.3.0", None);
        let fake = Fake::default().answer(repo().manifest_url(), Ok(text.as_bytes()));

        match look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path())) {
            Found::Unavailable { detail } => assert!(detail.contains("sha256"), "{detail}"),
            other => panic!("expected nothing offered, got {other:?}"),
        }
        assert!(fake.asked().iter().all(|u| !u.ends_with(".msi")));
    }

    #[test]
    fn the_same_version_is_up_to_date_and_downloads_nothing() {
        let cache = tempfile::tempdir().unwrap();
        let fake = published("0.3.0", INSTALLER);
        assert_eq!(
            look(&repo(), Version::new(0, 3, 0), &fake, Some(cache.path())),
            Found::UpToDate
        );
        assert_eq!(fake.asked().len(), 1);
    }

    /// Where the repository is today: public, with no release yet.
    #[test]
    fn no_release_yet_is_said_quietly() {
        let fake = Fake::default();
        match look(&repo(), Version::new(0, 2, 0), &fake, None) {
            Found::Unavailable { detail } => assert!(detail.contains("No release"), "{detail}"),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[test]
    fn no_network_is_said_quietly() {
        let fake = Fake::default().answer(
            repo().manifest_url(),
            Err(HttpError::Failed("the name could not be looked up".into())),
        );
        match look(&repo(), Version::new(0, 2, 0), &fake, None) {
            Found::Unavailable { detail } => {
                assert!(detail.contains("could not be reached"), "{detail}");
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
    }
}
