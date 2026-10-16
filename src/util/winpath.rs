//! Windows path shapes, as pure string manipulation.
//!
//! Several Win32 calls require a volume root rather than an arbitrary
//! directory, and fail quietly otherwise:
//!
//! * `GetVolumeInformationW` requires a root path with a trailing separator.
//!   Otherwise it fails with `ERROR_DIR_NOT_ROOT`, and the volume serial (the
//!   persisted index's identity check) becomes unavailable.
//! * `GetDriveTypeW` returns `DRIVE_NO_ROOT_DIR` for a non-root, so network
//!   shares are not recognised.
//! * `WNetGetConnectionW` wants a bare device name (`V:`), not a path.
//!
//! Parsed by hand because `std::path::Component::Prefix` is never produced on
//! non-Windows hosts; this keeps behaviour and tests identical everywhere.

use std::path::{Path, PathBuf};

/// Strips a `\\?\` verbatim prefix, which `CreateFileW` accepts but the
/// volume APIs do not.
fn strip_verbatim(s: &str) -> &str {
    s.strip_prefix(r"\\?\").unwrap_or(s)
}

#[inline]
fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// The drive letter of `path` as a device name, e.g. `V:`.
///
/// `None` for UNC paths and anything without a `X:` prefix.
pub fn drive_letter_of(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    let s = strip_verbatim(&s);
    let mut chars = s.chars();
    let letter = chars.next()?;
    if !letter.is_ascii_alphabetic() || chars.next()? != ':' {
        return None;
    }
    Some(format!("{}:", letter.to_ascii_uppercase()))
}

/// The volume root containing `path`, with the trailing separator the volume
/// APIs require.
///
/// `V:\shared\files` and `V:\` both yield `V:\`;
/// `\\server\share\folder` yields `\\server\share\`.
pub fn volume_root_of(path: &Path) -> Option<PathBuf> {
    let s = path.to_string_lossy();
    let s = strip_verbatim(&s);

    if let Some(letter) = drive_letter_of(Path::new(s)) {
        return Some(PathBuf::from(format!("{letter}\\")));
    }

    // UNC: \\server\share -> \\server\share\
    let rest = s.strip_prefix(r"\\").or_else(|| s.strip_prefix("//"))?;
    let mut parts = rest.split(is_sep).filter(|p| !p.is_empty());
    let server = parts.next()?;
    let share = parts.next()?;
    Some(PathBuf::from(format!(r"\\{server}\{share}\")))
}

/// True when `path` is a volume root - `V:\`, `V:`, or `\\server\share`.
///
/// A volume root must keep its trailing separator when handed to
/// `CreateFileW`; any deeper directory must not have one.
pub fn is_volume_root(path: &Path) -> bool {
    match volume_root_of(path) {
        Some(root) => {
            let a = path.to_string_lossy();
            let a = strip_verbatim(&a);
            let a = a.trim_end_matches(is_sep);
            let b = root.to_string_lossy();
            let b = b.trim_end_matches(is_sep);
            a.eq_ignore_ascii_case(b)
        }
        None => false,
    }
}

/// Canonical spelling of a configured root.
///
/// A volume root keeps its trailing separator; anything deeper loses it.
/// Applied when configuration is loaded, so `CreateFileW` never receives a
/// non-root path with a trailing backslash.
pub fn normalise_root(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().into_owned();
    if s.is_empty() {
        return PathBuf::new();
    }
    if is_volume_root(path) {
        return volume_root_of(path).unwrap_or_else(|| PathBuf::from(s));
    }
    let trimmed = s.trim_end_matches(is_sep);
    PathBuf::from(if trimmed.is_empty() {
        s.as_str()
    } else {
        trimmed
    })
}

/// A stable identifier for a directory, used to name its cache files.
///
/// Case- and separator-insensitive, so `V:\shared\files` and
/// `v:/Shared/FILES/` identify the same index rather than two.
pub fn path_key(path: &Path) -> u64 {
    let s = path.to_string_lossy();
    let s = strip_verbatim(&s);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for c in s.trim_end_matches(is_sep).chars() {
        let c = if c == '/' {
            '\\'
        } else {
            c.to_ascii_lowercase()
        };
        let mut buf = [0u8; 4];
        for &b in c.encode_utf8(&mut buf).as_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    h
}

/// Whether two configured directories denote the same place.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

/// Whether `path` is `root` itself or lies beneath it.
///
/// Compared component by component rather than as a string prefix, which
/// would call `V:\\projects2` a child of `V:\\projects`. Case-insensitive, because
/// the file systems this runs against are.
pub fn contains(root: &Path, path: &Path) -> bool {
    let root = normalise_root(root);
    let mut want = root.components();
    let mut have = path.components();
    loop {
        match (want.next(), have.next()) {
            (None, _) => return true,
            (Some(_), None) => return false,
            (Some(a), Some(b)) => {
                let (a, b) = (a.as_os_str(), b.as_os_str());
                if !a.eq_ignore_ascii_case(b) {
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_the_drive_letter() {
        assert_eq!(drive_letter_of(Path::new(r"V:\")).as_deref(), Some("V:"));
        assert_eq!(
            drive_letter_of(Path::new(r"V:\shared\files")).as_deref(),
            Some("V:")
        );
        assert_eq!(drive_letter_of(Path::new("v:")).as_deref(), Some("V:"));
        assert_eq!(
            drive_letter_of(Path::new(r"\\?\V:\Documents")).as_deref(),
            Some("V:")
        );
    }

    #[test]
    fn a_unc_path_has_no_drive_letter() {
        assert_eq!(drive_letter_of(Path::new(r"\\server\share\x")), None);
        assert_eq!(drive_letter_of(Path::new("relative\\thing")), None);
        assert_eq!(drive_letter_of(Path::new("")), None);
    }

    /// The volume APIs need `V:\`, not the configured directory.
    #[test]
    fn finds_the_volume_root_of_a_subdirectory() {
        assert_eq!(
            volume_root_of(Path::new(r"V:\shared\files")),
            Some(PathBuf::from(r"V:\"))
        );
        assert_eq!(
            volume_root_of(Path::new(r"V:\shared\files\")),
            Some(PathBuf::from(r"V:\"))
        );
        assert_eq!(
            volume_root_of(Path::new(r"V:\")),
            Some(PathBuf::from(r"V:\"))
        );
        assert_eq!(volume_root_of(Path::new("v:")), Some(PathBuf::from(r"V:\")));
    }

    #[test]
    fn finds_the_volume_root_of_a_unc_path() {
        assert_eq!(
            volume_root_of(Path::new(r"\\fileserv\projects\sub\dir")),
            Some(PathBuf::from(r"\\fileserv\projects\"))
        );
        assert_eq!(
            volume_root_of(Path::new(r"\\fileserv\projects")),
            Some(PathBuf::from(r"\\fileserv\projects\"))
        );
    }

    #[test]
    fn a_path_with_no_volume_has_no_root() {
        assert_eq!(volume_root_of(Path::new("relative\\thing")), None);
        assert_eq!(volume_root_of(Path::new("")), None);
        assert_eq!(volume_root_of(Path::new(r"\\onlyserver")), None);
    }

    #[test]
    fn recognises_volume_roots() {
        assert!(is_volume_root(Path::new(r"V:\")));
        assert!(is_volume_root(Path::new("V:")));
        assert!(is_volume_root(Path::new(r"\\fileserv\projects")));
        assert!(is_volume_root(Path::new(r"\\fileserv\projects\")));

        assert!(!is_volume_root(Path::new(r"V:\Documents")));
        assert!(!is_volume_root(Path::new(r"V:\shared\files")));
        assert!(!is_volume_root(Path::new("relative")));
    }

    #[test]
    fn normalises_roots_for_createfile() {
        assert_eq!(normalise_root(Path::new(r"V:\")), PathBuf::from(r"V:\"));
        assert_eq!(normalise_root(Path::new("V:")), PathBuf::from(r"V:\"));
        assert_eq!(
            normalise_root(Path::new(r"V:\shared\files\")),
            PathBuf::from(r"V:\shared\files")
        );
        assert_eq!(
            normalise_root(Path::new(r"V:\shared\files")),
            PathBuf::from(r"V:\shared\files")
        );
        assert_eq!(
            normalise_root(Path::new(r"\\fileserv\projects\sub\")),
            PathBuf::from(r"\\fileserv\projects\sub")
        );
    }

    #[test]
    fn normalising_an_empty_path_is_harmless() {
        assert_eq!(normalise_root(Path::new("")), PathBuf::new());
    }

    #[test]
    fn the_path_key_ignores_case_and_separator_spelling() {
        let a = path_key(Path::new(r"V:\shared\files"));
        assert_eq!(a, path_key(Path::new(r"v:\SHARED\Files")));
        assert_eq!(a, path_key(Path::new(r"V:/shared/files/")));
        assert_eq!(a, path_key(Path::new(r"\\?\V:\shared\files")));
    }

    #[test]
    fn different_directories_get_different_keys() {
        assert_ne!(
            path_key(Path::new(r"V:\shared\files")),
            path_key(Path::new(r"V:\Documents\other"))
        );
        // A root and its subdirectory must not collide, or a stale index
        // would be served.
        assert_ne!(
            path_key(Path::new(r"V:\")),
            path_key(Path::new(r"V:\shared\files"))
        );
    }

    #[test]
    fn same_dir_agrees_with_the_key() {
        assert!(same_dir(
            Path::new(r"V:\shared\files"),
            Path::new(r"v:/Shared/FILES/")
        ));
        assert!(!same_dir(Path::new(r"V:\"), Path::new(r"V:\shared\files")));
    }
}
