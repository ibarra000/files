//! Checking a GitHub repository's latest release for a newer version.
//!
//! A release carries the same `latest.toml` and installer as a share folder
//! (written by `tools/make_msi.ps1 -Release`), fetched from the fixed
//! `releases/latest/download/<name>` address; no API or JSON is involved.
//!
//! The installer is downloaded during the check, so the result is the same
//! [`Found::Available`] a share yields, pointing at a local file, and
//! downstream code (including [`super::apply::stage`]) is unchanged.
//!
//! A release must name a `sha256`; otherwise it is not offered. A mismatched
//! download is deleted here, and `stage` checks the digest again before
//! installing.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::http::{Fetch, HttpError};
use super::{Found, MANIFEST_NAME, Version, manifest};

/// Maximum manifest size in bytes.
const MANIFEST_LIMIT: u64 = 64 * 1024;

/// Maximum installer size in bytes; a sanity ceiling well above the real size.
const MSI_LIMIT: u64 = 200 * 1024 * 1024;

/// Download subdirectory under the cache's `update` folder.
const DOWNLOAD_DIR: &str = "download";

/// Age after which a `.part` file is assumed abandoned; real downloads finish
/// in minutes.
const STALE_SCRATCH: Duration = Duration::from_secs(24 * 60 * 60);

/// `owner/name` on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    owner: String,
    name: String,
}

impl Repo {
    /// Parses `owner/name` or a `https://github.com/owner/name` address.
    ///
    /// Accepts only GitHub's allowed characters (letters, digits, `-`, `_`,
    /// `.`), so both parts are safe to put into a URL unescaped.
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

    /// Returns the repository from `Cargo.toml`'s `repository` field.
    pub fn ours() -> Option<Self> {
        Self::parse(env!("CARGO_PKG_REPOSITORY"))
    }

    /// URL of the latest release's manifest. GitHub's "latest" excludes drafts
    /// and pre-releases.
    fn manifest_url(&self) -> String {
        format!(
            "https://github.com/{}/{}/releases/latest/download/{MANIFEST_NAME}",
            self.owner, self.name
        )
    }

    /// URL of an asset of the release tagged `v{version}`.
    ///
    /// Uses the tag rather than "latest" so the installer matches the manifest
    /// even if a release is published between the two requests.
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

/// Checks the latest release of `repo` against `running`, downloading its
/// installer into `cache_dir` when it is newer.
pub fn look(repo: &Repo, running: Version, fetch: &dyn Fetch, cache_dir: Option<&Path>) -> Found {
    let unavailable = |detail: String| Found::Unavailable { detail };

    let text = match fetch.get(&repo.manifest_url(), MANIFEST_LIMIT) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return unavailable(format!("The {MANIFEST_NAME} on GitHub is not text")),
        },
        // No release yet, or a release without a manifest.
        Err(HttpError::Status(404)) => {
            return unavailable(format!("No GitHub release includes {MANIFEST_NAME} yet"));
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
        return unavailable("No cache folder is available for the installer download".into());
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

/// Ensures the installer at `msi` matches `sha256`, downloading it if needed.
/// Returns whether it does.
///
/// A matching file is kept. Otherwise the download goes to a scratch name and
/// is renamed into place only after verification.
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
    // A concurrent attempt may have placed a valid copy first.
    placed || matches(msi)
}

/// Returns a scratch path unique to this download.
///
/// Includes the process id (two processes may check) and a counter (one
/// process may run two checks at once), so concurrent downloads never share a
/// file.
fn scratch_for(manifest: &manifest::Manifest, dir: &Path) -> std::path::PathBuf {
    static ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let attempt = ATTEMPT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        "{}.{}.{attempt}.part",
        manifest.msi,
        std::process::id()
    ))
}

/// Deletes `.part` files older than [`STALE_SCRATCH`], left by interrupted
/// downloads. Newer ones may belong to a download still in progress.
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

    /// A fake network: canned responses per URL, with every request recorded.
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

    #[test]
    fn this_build_names_its_own_repository() {
        assert!(
            Repo::ours().is_some(),
            "Cargo.toml's repository is not a GitHub one"
        );
    }

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

    #[test]
    fn an_installer_already_downloaded_is_not_fetched_twice() {
        let cache = tempfile::tempdir().unwrap();
        let fake = published("0.3.0", INSTALLER);
        look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));
        look(&repo(), Version::new(0, 2, 0), &fake, Some(cache.path()));

        let downloads = fake.asked().iter().filter(|u| u.ends_with(".msi")).count();
        assert_eq!(downloads, 1, "asked for {:?}", fake.asked());
    }

    #[test]
    fn two_looks_at_once_in_one_process_do_not_share_a_scratch_file() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = manifest::parse(&manifest("0.3.0", None)).unwrap();
        let first = scratch_for(&manifest, dir.path());
        let second = scratch_for(&manifest, dir.path());
        assert_ne!(first, second, "two attempts were given one scratch file");
    }

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

    #[test]
    fn no_release_yet_is_said_quietly() {
        let fake = Fake::default();
        match look(&repo(), Version::new(0, 2, 0), &fake, None) {
            Found::Unavailable { detail } => {
                assert!(detail.contains("No GitHub release"), "{detail}")
            }
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
