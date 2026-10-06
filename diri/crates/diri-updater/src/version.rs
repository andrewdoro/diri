//! Dotted numeric versions, compared the way a release feed needs them.
//!
//! Deliberately not a semver implementation: diri ships `MAJOR.MINOR.PATCH`
//! and macOS reports `15`, `15.5`, or `15.5.1`. One prerelease form is
//! ordered: `X.Y.Z-nightly.N`, the nightly channel's builds, where `N` is a
//! UTC `YYYYMMDDHHMM` stamp. A nightly sorts *before* the stable `X.Y.Z` it
//! becomes when promoted (semver precedence), and nightlies sort by `N`.
//! Anything else after a `-` or `+` (other prerelease or build metadata) is
//! ignored rather than ordered, because guessing at arbitrary prerelease
//! precedence would be a silent source of wrong update decisions.

use std::cmp::Ordering;
use std::fmt;

const NIGHTLY_PREFIX: &str = "nightly.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// The `N` of a `-nightly.N` build; `None` for a stable release.
    pub nightly: Option<u64>,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            nightly: None,
        }
    }

    pub const fn nightly(major: u32, minor: u32, patch: u32, build: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            nightly: Some(build),
        }
    }

    pub const fn is_nightly(self) -> bool {
        self.nightly.is_some()
    }

    /// Parses `"0.4.2"`, `"15.5"`, `"3"`, or `"0.9.4-nightly.202610070417"`.
    /// Returns `None` for anything with a non-numeric or empty component, so a
    /// malformed feed entry is skipped instead of being treated as version 0.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('v');
        let (core, suffix) = match text.find(['-', '+']) {
            Some(index) => (&text[..index], &text[index..]),
            None => (text, ""),
        };
        let core = core.trim();
        if core.is_empty() {
            return None;
        }
        let mut parts = core.split('.');
        let mut component = || -> Option<u32> {
            match parts.next() {
                Some(part) => part.trim().parse().ok(),
                None => Some(0),
            }
        };
        let major = component()?;
        let minor = component()?;
        let patch = component()?;
        if parts.next().is_some() {
            return None;
        }
        // Only a prerelease (`-`) names a nightly; build metadata (`+`) never does.
        let nightly = suffix
            .strip_prefix('-')
            .and_then(|pre| pre.split('+').next())
            .and_then(|pre| pre.strip_prefix(NIGHTLY_PREFIX))
            .filter(|build| !build.is_empty() && build.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|build| build.parse().ok());
        Some(Self {
            major,
            minor,
            patch,
            nightly,
        })
    }

    pub fn is_newer_than(self, other: Self) -> bool {
        self.cmp(&other) == Ordering::Greater
    }
}

// Written out rather than derived: a derived `Option` order puts `None` first,
// which would rank every nightly above the release it leads up to.
impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.nightly, other.nightly) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(mine), Some(theirs)) => mine.cmp(&theirs),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(build) = self.nightly {
            write!(formatter, "-{NIGHTLY_PREFIX}{build}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_to_three_components() {
        assert_eq!(Version::parse("0.4.2"), Some(Version::new(0, 4, 2)));
        assert_eq!(Version::parse("15.5"), Some(Version::new(15, 5, 0)));
        assert_eq!(Version::parse("16"), Some(Version::new(16, 0, 0)));
        assert_eq!(Version::parse(" v1.2.3 "), Some(Version::new(1, 2, 3)));
    }

    #[test]
    fn ignores_prerelease_and_build_metadata() {
        assert_eq!(Version::parse("1.2.3-beta.1"), Some(Version::new(1, 2, 3)));
        assert_eq!(Version::parse("1.2.3+197"), Some(Version::new(1, 2, 3)));
    }

    #[test]
    fn rejects_garbage_instead_of_defaulting_to_zero() {
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("latest"), None);
        assert_eq!(Version::parse("1.x.3"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("-1.0.0"), None);
        assert_eq!(Version::parse("-nightly.1"), None);
    }

    #[test]
    fn orders_by_component_significance() {
        assert!(Version::new(0, 5, 0).is_newer_than(Version::new(0, 4, 9)));
        assert!(Version::new(1, 0, 0).is_newer_than(Version::new(0, 99, 99)));
        assert!(Version::new(0, 4, 10).is_newer_than(Version::new(0, 4, 9)));
        assert!(!Version::new(0, 4, 2).is_newer_than(Version::new(0, 4, 2)));
        assert!(!Version::new(0, 4, 1).is_newer_than(Version::new(0, 4, 2)));
    }

    #[test]
    fn parses_nightly_builds() {
        let nightly = Version::parse("0.9.4-nightly.202610070417").unwrap();
        assert_eq!(nightly, Version::nightly(0, 9, 4, 202_610_070_417));
        assert!(nightly.is_nightly());
        assert!(!Version::new(0, 9, 4).is_nightly());
        assert_eq!(
            Version::parse("0.9.4-nightly.20261007+5"),
            Some(Version::nightly(0, 9, 4, 20_261_007))
        );
        // Not the nightly form: the suffix is ignored, as for any prerelease.
        assert_eq!(Version::parse("0.9.4-nightly"), Some(Version::new(0, 9, 4)));
        assert_eq!(
            Version::parse("0.9.4-nightly.x1"),
            Some(Version::new(0, 9, 4))
        );
        assert_eq!(
            Version::parse("0.9.4+nightly.1"),
            Some(Version::new(0, 9, 4))
        );
    }

    #[test]
    fn a_nightly_precedes_its_release_and_follows_the_previous_one() {
        let older = Version::parse("0.9.4-nightly.202610060417").unwrap();
        let newer = Version::parse("0.9.4-nightly.202610070417").unwrap();
        let stable = Version::new(0, 9, 4);
        assert!(newer.is_newer_than(older));
        assert!(stable.is_newer_than(newer));
        assert!(older.is_newer_than(Version::new(0, 9, 3)));
        assert!(Version::nightly(0, 9, 5, 1).is_newer_than(stable));
        assert!(!older.is_newer_than(older));
        assert_ne!(older, stable);
    }

    #[test]
    fn nightly_display_round_trips() {
        let text = "0.9.4-nightly.202610070417";
        assert_eq!(Version::parse(text).unwrap().to_string(), text);
    }

    #[test]
    fn displays_all_three_components() {
        assert_eq!(Version::new(15, 5, 0).to_string(), "15.5.0");
    }
}
