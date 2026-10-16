//! The `latest.toml` update manifest, published beside the installer:
//!
//! ```toml
//! version = "0.3.0"
//! msi     = "files-0.3.0-x64.msi"
//! sha256  = "9f2c…"            # optional
//! notes   = "Scrollable results, editable settings."
//! ```
//!
//! `tools/make_msi.ps1` writes it from the same `Cargo.toml` version it builds.
//!
//! The digest detects truncated or half-written copies. It is not
//! authentication: whoever can write the MSI can write the manifest. Share
//! permissions control who can publish.

use crate::update::version::Version;

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: Version,
    /// The installer's bare file name in the manifest's folder (see
    /// [`Problem::MsiIsNotABareName`]).
    pub msi: String,
    pub sha256: Option<String>,
    pub notes: Option<String>,
}

/// Why a manifest could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    NotToml(String),
    MissingVersion,
    BadVersion(String),
    MissingMsi,
    /// The name contains a separator, drive or `..`. It is joined onto the
    /// update folder and passed to `msiexec`, so it must not escape that
    /// folder.
    MsiIsNotABareName(String),
    BadDigest(String),
}

impl Problem {
    pub fn detail(&self) -> String {
        match self {
            Self::NotToml(e) => format!("the update manifest could not be read: {e}"),
            Self::MissingVersion => "the update manifest names no version".into(),
            Self::BadVersion(v) => format!("{v:?} is not a valid version"),
            Self::MissingMsi => "the update manifest names no installer".into(),
            Self::MsiIsNotABareName(name) => {
                format!("{name:?} is not a file name in the update folder")
            }
            Self::BadDigest(d) => format!("{d:?} is not a SHA-256 digest"),
        }
    }
}

/// Reads a manifest.
pub fn parse(text: &str) -> Result<Manifest, Problem> {
    let doc: toml_edit::ImDocument<String> = toml_edit::ImDocument::parse(text.to_string())
        .map_err(|e| Problem::NotToml(e.to_string()))?;

    let string = |key: &str| {
        doc.get(key)
            .and_then(toml_edit::Item::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };

    let raw = string("version").ok_or(Problem::MissingVersion)?;
    let version = Version::parse(raw).ok_or_else(|| Problem::BadVersion(raw.to_string()))?;

    let msi = string("msi").ok_or(Problem::MissingMsi)?;
    if !is_bare_name(msi) {
        return Err(Problem::MsiIsNotABareName(msi.to_string()));
    }

    let sha256 = match string("sha256") {
        Some(digest) if is_digest(digest) => Some(digest.to_ascii_lowercase()),
        Some(digest) => return Err(Problem::BadDigest(digest.to_string())),
        None => None,
    };

    Ok(Manifest {
        version,
        msi: msi.to_string(),
        sha256,
        notes: string("notes").map(str::to_string),
    })
}

/// Whether `name` is a plain file name within the update folder.
///
/// Rejects anything outside a conservative set of safe shapes.
fn is_bare_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        // Windows strips a trailing dot or space, so such a name would open a
        // different file.
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name.chars().any(|c| c.is_control())
}

fn is_digest(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows PowerShell 5.1 writes UTF-8 with a byte-order mark.
    #[test]
    fn a_manifest_with_a_byte_order_mark_is_still_read() {
        let text = "\u{feff}version = \"0.3.0\"\nmsi = \"files-0.3.0-x64.msi\"\n";
        assert_eq!(parse(text).map(|m| m.version), Ok(Version::new(0, 3, 0)));
    }

    const FULL: &str = r#"
version = "0.3.0"
msi     = "files-0.3.0-x64.msi"
sha256  = "9f2c1d4e5a6b7c8d9e0f1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f"
notes   = "Scrollable results."
"#;

    #[test]
    fn reads_a_published_release() {
        let m = parse(FULL).unwrap();
        assert_eq!(m.version, Version::new(0, 3, 0));
        assert_eq!(m.msi, "files-0.3.0-x64.msi");
        assert_eq!(m.notes.as_deref(), Some("Scrollable results."));
        assert!(m.sha256.is_some());
    }

    #[test]
    fn the_digest_and_the_notes_are_both_optional() {
        let m = parse("version = \"1.0.0\"\nmsi = \"a.msi\"\n").unwrap();
        assert_eq!(m.sha256, None);
        assert_eq!(m.notes, None);
    }

    /// Unknown keys are ignored so older builds can still read newer manifests.
    #[test]
    fn a_key_from_the_future_is_ignored_rather_than_refused() {
        let m = parse("version = \"1.0.0\"\nmsi = \"a.msi\"\nsignature = \"x\"\n").unwrap();
        assert_eq!(m.version, Version::new(1, 0, 0));
    }

    #[test]
    fn a_manifest_without_the_essentials_is_refused() {
        assert_eq!(parse("msi = \"a.msi\"\n"), Err(Problem::MissingVersion));
        assert_eq!(parse("version = \"1.0.0\"\n"), Err(Problem::MissingMsi));
        assert!(matches!(parse("{{{"), Err(Problem::NotToml(_))));
    }

    #[test]
    fn a_version_this_cannot_compare_is_refused_rather_than_guessed_at() {
        assert_eq!(
            parse("version = \"0.3\"\nmsi = \"a.msi\"\n"),
            Err(Problem::BadVersion("0.3".into()))
        );
    }

    #[test]
    fn an_installer_name_that_would_leave_the_update_folder_is_refused() {
        for name in [
            r"..\..\windows\system32\evil.msi",
            "sub/dir.msi",
            r"sub\dir.msi",
            r"C:\evil.msi",
            r"\\other\share\evil.msi",
            "..",
            ".",
            "trailing.msi.",
        ] {
            let text = format!("version = \"1.0.0\"\nmsi = {name:?}\n");
            assert!(
                matches!(parse(&text), Err(Problem::MsiIsNotABareName(_))),
                "{name:?} should be refused"
            );
        }
    }

    #[test]
    fn an_ordinary_installer_name_is_accepted() {
        for name in ["files-0.3.0-x64.msi", "files.msi", "a b.msi"] {
            let text = format!("version = \"1.0.0\"\nmsi = {name:?}\n");
            assert_eq!(parse(&text).unwrap().msi, name, "{name:?} should be fine");
        }
    }

    /// Values are trimmed before validation.
    #[test]
    fn space_around_a_value_is_not_part_of_it() {
        let m = parse("version = \"  1.0.0 \"\nmsi = \"  files.msi  \"\n").unwrap();
        assert_eq!(m.version, Version::new(1, 0, 0));
        assert_eq!(m.msi, "files.msi");
    }

    #[test]
    fn a_digest_that_is_not_one_is_refused() {
        for digest in ["abc", &"z".repeat(64), &"a".repeat(63)] {
            let text = format!("version = \"1.0.0\"\nmsi = \"a.msi\"\nsha256 = {digest:?}\n");
            assert!(
                matches!(parse(&text), Err(Problem::BadDigest(_))),
                "{digest:?} should be refused"
            );
        }
    }

    #[test]
    fn a_digest_is_compared_in_one_case() {
        let upper = "9F2C1D4E5A6B7C8D9E0F1A2B3C4D5E6F708192A3B4C5D6E7F8091A2B3C4D5E6F";
        let text = format!("version = \"1.0.0\"\nmsi = \"a.msi\"\nsha256 = {upper:?}\n");
        assert_eq!(
            parse(&text).unwrap().sha256.as_deref(),
            Some(upper.to_ascii_lowercase().as_str())
        );
    }

    #[test]
    fn every_problem_explains_itself() {
        for problem in [
            Problem::NotToml("x".into()),
            Problem::MissingVersion,
            Problem::BadVersion("0.3".into()),
            Problem::MissingMsi,
            Problem::MsiIsNotABareName("../x".into()),
            Problem::BadDigest("z".into()),
        ] {
            assert!(!problem.detail().is_empty());
        }
    }
}
