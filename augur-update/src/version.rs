//! A deliberately small semantic-version type.
//!
//! Release tags for this project are plain `vMAJOR.MINOR.PATCH`, occasionally
//! with a prerelease suffix. That is the whole grammar the updater has to
//! understand, so it parses it directly rather than pulling in a dependency
//! whose remaining surface would go unused.

use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Prerelease identifier without the leading `-`, e.g. `rc.1`.
    pub pre: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVersionError(String);

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a version number: {}", self.0)
    }
}

impl std::error::Error for ParseVersionError {}

impl Version {
    pub fn parse(raw: &str) -> Result<Self, ParseVersionError> {
        let invalid = || ParseVersionError(raw.to_owned());

        // Tags carry a `v` prefix; `CARGO_PKG_VERSION` does not.
        let text = raw.trim().trim_start_matches(['v', 'V']);
        // Build metadata never participates in ordering, so drop it up front.
        let text = text.split('+').next().unwrap_or_default();

        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) if !pre.is_empty() => (core, Some(pre.to_owned())),
            Some(_) => return Err(invalid()),
            None => (text, None),
        };

        let mut parts = core.split('.');
        let mut next = || -> Result<u64, ParseVersionError> {
            parts
                .next()
                .filter(|part| !part.is_empty())
                .ok_or_else(invalid)?
                .parse()
                .map_err(|_| invalid())
        };
        let major = next()?;
        let minor = next()?;
        let patch = next()?;
        if parts.next().is_some() {
            return Err(invalid());
        }

        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            // A prerelease precedes its own release: 1.2.0-rc.1 < 1.2.0.
            .then(match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Version {
        Version::parse(raw).expect(raw)
    }

    #[test]
    fn parses_tags_and_cargo_versions_identically() {
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert_eq!(v("1.2.3").major, 1);
        assert_eq!(v("1.2.3").minor, 2);
        assert_eq!(v("1.2.3").patch, 3);
        assert!(v("1.2.3").pre.is_none());
    }

    #[test]
    fn keeps_prerelease_and_drops_build_metadata() {
        assert_eq!(v("v1.2.3-rc.1").pre.as_deref(), Some("rc.1"));
        assert!(v("1.2.3-rc.1").is_prerelease());
        assert_eq!(v("1.2.3+build.7"), v("1.2.3"));
        assert_eq!(v("1.2.3-rc.1+build.7").pre.as_deref(), Some("rc.1"));
    }

    #[test]
    fn rejects_malformed_input() {
        for raw in ["", "1", "1.2", "1.2.3.4", "1.2.x", "latest", "v", "1.2.3-"] {
            assert!(Version::parse(raw).is_err(), "should reject {raw:?}");
        }
    }

    #[test]
    fn orders_by_precedence() {
        assert!(v("1.0.1") > v("1.0.0"));
        assert!(v("1.1.0") > v("1.0.99"));
        assert!(v("2.0.0") > v("1.99.99"));
        // A published 1.2.0 must win over its own release candidate, otherwise
        // the updater would strand anyone who tried a prerelease.
        assert!(v("1.2.0") > v("1.2.0-rc.2"));
        assert!(v("1.2.0-rc.2") > v("1.2.0-rc.1"));
        assert_eq!(v("1.2.3"), v("v1.2.3"));
    }

    #[test]
    fn round_trips_through_display() {
        for raw in ["1.2.3", "0.0.1", "10.20.30", "1.2.3-rc.1"] {
            assert_eq!(v(raw).to_string(), raw);
        }
    }
}
