//! `major.minor.patch` versions for update comparison.
//!
//! Pre-release tags and build metadata are not supported, which avoids a
//! semver dependency and its subtle ordering rules.

use std::fmt;

/// A released version, as `major.minor.patch`.
///
/// The derived `Ord` relies on the field declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Returns this build's version from `Cargo.toml`, the same source the
    /// installer uses. Falls back to `0.0.0` rather than panicking.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or_default()
    }

    /// Parses exactly three dot-separated numbers.
    ///
    /// A `-beta` or `+build` suffix is rejected, since ignoring it would make
    /// a pre-release compare equal to its release.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let mut next = || parts.next()?.parse::<u32>().ok();
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// Whether `self` is worth installing over `running`.
    pub fn is_newer_than(self, running: Self) -> bool {
        self > running
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_three_numbers() {
        assert_eq!(Version::parse("0.3.0"), Some(Version::new(0, 3, 0)));
        assert_eq!(Version::parse("12.4.117"), Some(Version::new(12, 4, 117)));
    }

    #[test]
    fn surrounding_space_is_not_a_syntax_error() {
        assert_eq!(Version::parse("  1.2.3\n"), Some(Version::new(1, 2, 3)));
    }

    #[test]
    fn anything_that_is_not_three_numbers_is_refused() {
        for text in [
            "0.3",
            "0.3.0.1",
            "0.3.0-beta",
            "0.3.0+build7",
            "v0.3.0",
            "",
            "x.y.z",
            "0.3.",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?} should be refused");
        }
    }

    #[test]
    fn a_later_version_is_newer() {
        let running = Version::new(0, 2, 0);
        for newer in ["0.2.1", "0.3.0", "1.0.0"] {
            let newer = Version::parse(newer).unwrap();
            assert!(newer.is_newer_than(running), "{newer} should be newer");
        }
    }

    #[test]
    fn the_same_version_is_not_newer_than_itself() {
        let running = Version::new(0, 2, 0);
        assert!(!running.is_newer_than(running));
    }

    #[test]
    fn an_earlier_version_is_not_newer() {
        let running = Version::new(0, 3, 0);
        for older in ["0.2.9", "0.1.0", "0.3.0"] {
            assert!(!Version::parse(older).unwrap().is_newer_than(running));
        }
    }

    /// Ordering is numeric per field, not lexical.
    #[test]
    fn a_two_digit_part_sorts_after_a_one_digit_part() {
        let nine = Version::parse("0.9.0").unwrap();
        let ten = Version::parse("0.10.0").unwrap();
        assert!(ten.is_newer_than(nine), "0.10.0 should beat 0.9.0");
    }

    #[test]
    fn a_version_round_trips_through_its_own_spelling() {
        for text in ["0.0.0", "0.2.0", "1.20.300"] {
            let parsed = Version::parse(text).unwrap();
            assert_eq!(parsed.to_string(), text);
        }
    }

    #[test]
    fn this_build_knows_its_own_version() {
        assert_ne!(Version::current(), Version::default());
        assert_eq!(
            Version::current(),
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
        );
    }
}
