//! Comparing this build against a published one.
//!
//! Three numbers, and deliberately no more. A general semantic-version parser
//! handles pre-release tags and build metadata, whose ordering rules are
//! subtle and whose only use here would be to let somebody publish something
//! this could not compare - so the crate that implements them is not worth
//! carrying, and the syntax they need is not worth accepting.

use std::fmt;

/// A released version, as `major.minor.patch`.
///
/// `Ord` is derived, and the field order is what makes that correct: Rust
/// compares a struct field by field in declaration order, which is exactly
/// precedence for a version number.
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

    /// What this executable is.
    ///
    /// Read from the same `Cargo.toml` field the installer reads, so the
    /// running program, the MSI and the manifest cannot claim three different
    /// things. Falls back to zero rather than panicking: a version that
    /// somehow would not parse should disable updating, not stop the program.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or_default()
    }

    /// Exactly three dot-separated numbers, and nothing else.
    ///
    /// A trailing `-beta` or `+build` is refused rather than ignored.
    /// Ignoring it would make `0.3.0-beta` and `0.3.0` compare equal, so a
    /// machine on the release would be offered the beta and told it was the
    /// same version.
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

    /// Refused rather than ignored: ignoring the tag would make a pre-release
    /// compare equal to the release it precedes.
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

    /// The case that matters most: an unchanged share must not offer an
    /// update every time it is read.
    #[test]
    fn the_same_version_is_not_newer_than_itself() {
        let running = Version::new(0, 2, 0);
        assert!(!running.is_newer_than(running));
    }

    /// A share rolled back to an older build must not be installed over a
    /// newer one - the installer refuses a downgrade anyway, and offering it
    /// would be a prompt that cannot be satisfied.
    #[test]
    fn an_earlier_version_is_not_newer() {
        let running = Version::new(0, 3, 0);
        for older in ["0.2.9", "0.1.0", "0.3.0"] {
            assert!(!Version::parse(older).unwrap().is_newer_than(running));
        }
    }

    /// Ordering is by field, not by the string: 10 is after 9.
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

    /// The build has to be able to say what it is, or nothing can be compared
    /// against it.
    #[test]
    fn this_build_knows_its_own_version() {
        assert_ne!(Version::current(), Version::default());
        assert_eq!(
            Version::current(),
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
        );
    }
}
