//! Filesystem error classification.
//!
//! Deliberately kept out of the FFI modules: this is pure, table-driven
//! logic, and it is one of the few parts of the Windows integration that can
//! be tested on a machine without the network drives. The FFI modules stay
//! mechanically thin so excluding them from coverage stays honest.
//!
//! The distinction that matters most is that **`ERROR_FILE_NOT_FOUND` is a
//! success case**. On a volume root it means the directory is empty; on a
//! wildcard query it means nothing matched. The previous implementation
//! collapsed it, along with access denials, into "Path not found".

use std::fmt;

/// Windows error codes this crate reasons about by name.
pub mod code {
    pub const FILE_NOT_FOUND: u32 = 2;
    pub const PATH_NOT_FOUND: u32 = 3;
    pub const ACCESS_DENIED: u32 = 5;
    pub const INVALID_DRIVE: u32 = 15;
    pub const NOT_READY: u32 = 21;
    pub const NO_MORE_FILES: u32 = 18;
    pub const NOT_SUPPORTED: u32 = 50;
    pub const BAD_NETPATH: u32 = 53;
    pub const UNEXP_NET_ERR: u32 = 59;
    pub const NETNAME_DELETED: u32 = 64;
    pub const INVALID_PARAMETER: u32 = 87;
    pub const SEM_TIMEOUT: u32 = 121;
    pub const INVALID_NAME: u32 = 123;
    pub const INVALID_DATA: u32 = 13;
    pub const DIRECTORY: u32 = 267;
    pub const OPERATION_ABORTED: u32 = 995;
    pub const NO_NET_OR_BAD_PATH: u32 = 1203;
    pub const SESSION_CREDENTIAL_CONFLICT: u32 = 1219;
    pub const NETWORK_UNREACHABLE: u32 = 1231;
    pub const LOGON_FAILURE: u32 = 1326;
    pub const SHARING_VIOLATION: u32 = 32;
    pub const NOTIFY_ENUM_DIR: u32 = 1022;
    pub const MORE_DATA: u32 = 234;
}

/// A classified enumeration failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnumError {
    /// The directory exists and nothing matched. Not a failure.
    Empty,
    PathNotFound(u32),
    NotADirectory(u32),
    AccessDenied(u32),
    /// Worth one retry; a mapped drive often reconnects lazily.
    Transient(u32),
    /// This strategy cannot run here. The only condition that advances the
    /// fallback chain.
    Unsupported(u32),
    /// The kernel or server returned a malformed record buffer.
    Corrupt(u32),
    Cancelled,
    TimedOut,
    Other(u32),
}

impl EnumError {
    /// Maps a raw Win32 error code.
    pub fn from_win(raw: u32) -> Self {
        match raw {
            code::FILE_NOT_FOUND | code::NO_MORE_FILES => Self::Empty,
            code::PATH_NOT_FOUND | code::INVALID_DRIVE => Self::PathNotFound(raw),
            code::ACCESS_DENIED
            | code::SESSION_CREDENTIAL_CONFLICT
            | code::LOGON_FAILURE
            | code::SHARING_VIOLATION => Self::AccessDenied(raw),
            code::DIRECTORY => Self::NotADirectory(raw),
            code::NOT_READY
            | code::BAD_NETPATH
            | code::UNEXP_NET_ERR
            | code::NETNAME_DELETED
            | code::SEM_TIMEOUT
            | code::NO_NET_OR_BAD_PATH
            | code::NETWORK_UNREACHABLE => Self::Transient(raw),
            code::INVALID_PARAMETER | code::NOT_SUPPORTED => Self::Unsupported(raw),
            code::INVALID_DATA => Self::Corrupt(raw),
            code::OPERATION_ABORTED => Self::Cancelled,
            _ => Self::Other(raw),
        }
    }

    /// Maps a raw Win32 error code from a *directory open*.
    ///
    /// The classification is context-dependent, and getting it wrong is easy.
    /// `ERROR_FILE_NOT_FOUND` from an enumeration means "no more entries" or
    /// "the pattern matched nothing" - an answer. From `CreateFileW` it means
    /// the directory itself does not exist, which is a genuine failure the
    /// user needs told about.
    pub fn from_win_open(raw: u32) -> Self {
        match raw {
            code::FILE_NOT_FOUND => Self::PathNotFound(raw),
            other => Self::from_win(other),
        }
    }

    /// Maps a `std::io::Error`, preferring the raw OS code when present.
    pub fn from_io(e: &std::io::Error) -> Self {
        if let Some(raw) = e.raw_os_error() {
            return Self::from_win(raw as u32);
        }
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::PathNotFound(code::PATH_NOT_FOUND),
            std::io::ErrorKind::PermissionDenied => Self::AccessDenied(code::ACCESS_DENIED),
            std::io::ErrorKind::TimedOut => Self::TimedOut,
            _ => Self::Other(0),
        }
    }

    /// Whether the fallback chain should advance past the current strategy.
    ///
    /// Only genuine "this API cannot work here" answers qualify. Retrying an
    /// access denial or a missing path through a different API just multiplies
    /// latency on an operation that has already given its answer.
    pub fn should_try_next_strategy(self) -> bool {
        matches!(self, Self::Unsupported(_) | Self::Corrupt(_))
    }

    /// Whether one retry after a short delay is worthwhile.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Transient(_) | Self::TimedOut)
    }

    /// Whether this means "no data, but nothing went wrong".
    pub fn is_empty_result(self) -> bool {
        matches!(self, Self::Empty)
    }

    /// The raw OS code, when there was one.
    pub fn raw(self) -> Option<u32> {
        match self {
            Self::Empty | Self::Cancelled | Self::TimedOut => None,
            Self::PathNotFound(c)
            | Self::NotADirectory(c)
            | Self::AccessDenied(c)
            | Self::Transient(c)
            | Self::Unsupported(c)
            | Self::Corrupt(c)
            | Self::Other(c) => Some(c),
        }
    }

    /// A message for the status line, naming the drive involved.
    ///
    /// Always carries the raw code: an error nobody can diagnose is worse
    /// than an ugly one.
    pub fn describe(self, target: &str) -> String {
        let detail = |s: String| match self.raw() {
            Some(c) => format!("{s} (os error {c})"),
            None => s,
        };
        match self {
            Self::Empty => format!("{target} is empty"),
            Self::PathNotFound(code::INVALID_DRIVE) => detail(format!("{target} is not mapped")),
            Self::PathNotFound(_) => detail(format!("{target} not found")),
            Self::NotADirectory(_) => detail(format!("{target} is a file, not a folder")),
            Self::AccessDenied(code::LOGON_FAILURE) => {
                detail(format!("credentials rejected for {target}"))
            }
            Self::AccessDenied(_) => detail(format!("access denied to {target}")),
            Self::Transient(code::NOT_READY) => detail(format!("{target} is not ready")),
            Self::Transient(_) => detail(format!("{target} unreachable")),
            Self::Unsupported(_) => detail(format!("{target} rejected this request")),
            Self::Corrupt(_) => detail(format!("{target} returned malformed directory data")),
            Self::Cancelled => "cancelled".to_string(),
            Self::TimedOut => format!("timed out reading {target}"),
            Self::Other(_) => detail(format!("could not read {target}")),
        }
    }
}

impl fmt::Display for EnumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe("the drive"))
    }
}

impl std::error::Error for EnumError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_not_found_is_an_empty_result_not_a_failure() {
        // On a volume root this means "empty directory"; on a wildcard query
        // it means "nothing matched". Both are answers.
        assert_eq!(EnumError::from_win(code::FILE_NOT_FOUND), EnumError::Empty);
        assert_eq!(EnumError::from_win(code::NO_MORE_FILES), EnumError::Empty);
        assert!(EnumError::from_win(2).is_empty_result());
    }

    #[test]
    fn distinguishes_a_missing_path_from_a_denied_one() {
        // The previous implementation reported both as "Path not found",
        // because Path::exists() returns false on ERROR_ACCESS_DENIED.
        assert!(matches!(EnumError::from_win(3), EnumError::PathNotFound(_)));
        assert!(matches!(EnumError::from_win(5), EnumError::AccessDenied(_)));
    }

    #[test]
    fn classifies_the_network_codes_as_transient() {
        for c in [21, 53, 59, 64, 121, 1203, 1231] {
            assert!(
                matches!(EnumError::from_win(c), EnumError::Transient(_)),
                "code {c} should be transient"
            );
            assert!(EnumError::from_win(c).is_retryable());
        }
    }

    #[test]
    fn classifies_credential_problems_as_access_denied() {
        for c in [5, 1219, 1326, 32] {
            assert!(
                matches!(EnumError::from_win(c), EnumError::AccessDenied(_)),
                "code {c} should be access denied"
            );
        }
    }

    #[test]
    fn an_unmapped_drive_is_a_missing_path() {
        assert!(matches!(
            EnumError::from_win(15),
            EnumError::PathNotFound(15)
        ));
    }

    #[test]
    fn only_unsupported_and_corrupt_advance_the_fallback_chain() {
        assert!(EnumError::from_win(87).should_try_next_strategy());
        assert!(EnumError::from_win(50).should_try_next_strategy());
        assert!(EnumError::from_win(13).should_try_next_strategy());

        for c in [2, 3, 5, 15, 53, 267, 1326] {
            assert!(
                !EnumError::from_win(c).should_try_next_strategy(),
                "code {c} is an answer, not a reason to retry via another API"
            );
        }
    }

    #[test]
    fn unknown_codes_round_trip_without_being_swallowed() {
        let e = EnumError::from_win(9999);
        assert_eq!(e, EnumError::Other(9999));
        assert_eq!(e.raw(), Some(9999));
        assert!(e.describe("V:\\").contains("9999"));
    }

    #[test]
    fn messages_name_the_drive_and_carry_the_raw_code() {
        assert_eq!(
            EnumError::from_win(53).describe("V:\\"),
            "V:\\ unreachable (os error 53)"
        );
        assert_eq!(
            EnumError::from_win(5).describe("R:\\ab1234"),
            "access denied to R:\\ab1234 (os error 5)"
        );
        assert_eq!(
            EnumError::from_win(15).describe("V:\\"),
            "V:\\ is not mapped (os error 15)"
        );
        assert_eq!(
            EnumError::from_win(1326).describe("V:\\"),
            "credentials rejected for V:\\ (os error 1326)"
        );
    }

    #[test]
    fn codeless_variants_omit_the_os_error_suffix() {
        assert_eq!(EnumError::Cancelled.raw(), None);
        assert!(!EnumError::TimedOut.describe("V:\\").contains("os error"));
    }

    /// The same code means different things depending on which call produced
    /// it, and conflating them is how "the folder is empty" and "the folder
    /// does not exist" become indistinguishable.
    #[test]
    fn file_not_found_means_missing_when_it_comes_from_an_open() {
        assert_eq!(EnumError::from_win(code::FILE_NOT_FOUND), EnumError::Empty);
        assert_eq!(
            EnumError::from_win_open(code::FILE_NOT_FOUND),
            EnumError::PathNotFound(code::FILE_NOT_FOUND)
        );
    }

    #[test]
    fn the_open_classifier_agrees_with_the_general_one_elsewhere() {
        for c in [3, 5, 15, 21, 53, 87, 267, 1326] {
            assert_eq!(
                EnumError::from_win_open(c),
                EnumError::from_win(c),
                "code {c}"
            );
        }
    }

    #[test]
    fn maps_io_errors_through_the_raw_code_when_available() {
        let io = std::io::Error::from_raw_os_error(53);
        assert!(matches!(EnumError::from_io(&io), EnumError::Transient(53)));
    }

    #[test]
    fn maps_io_errors_by_kind_when_there_is_no_raw_code() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        assert!(matches!(
            EnumError::from_io(&io),
            EnumError::AccessDenied(_)
        ));
    }
}
