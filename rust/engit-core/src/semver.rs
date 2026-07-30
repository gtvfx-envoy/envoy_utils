//! Semantic version parsing, validation, and ordering.
//!
//! This ports `py/engit/_semver.py`, preserving the accepted tag format:
//! `vMAJOR.MINOR.PATCH[-PRERELEASE]`.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use regex::Regex;

use crate::error::{EngitError, Result};

fn semver_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();

    REGEX.get_or_init(|| {
        Regex::new(
            r"^v?(?P<major>0|[1-9]\d*)\.(?P<minor>0|[1-9]\d*)\.(?P<patch>0|[1-9]\d*)(?:-(?P<prerelease>[a-zA-Z][a-zA-Z0-9]*(?:\.\d+)?))?$",
        )
        .expect("semver regex must compile")
    })
}

/// Immutable semantic version with optional prerelease identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemVer {
    /// Breaking change increment.
    pub major: u64,
    /// Backwards-compatible feature increment.
    pub minor: u64,
    /// Backwards-compatible bug-fix increment.
    pub patch: u64,
    /// Optional prerelease identifier such as `alpha` or `alpha.3`.
    pub prerelease: Option<String>,
}

impl SemVer {
    /// Parse a version string with or without a leading `v`.
    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        let Some(captures) = semver_regex().captures(trimmed) else {
            return Err(EngitError::SemVer(format!(
                "'{value}' is not a valid semantic version. Expected \
MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-LABEL[.N] (e.g. 1.2.3, v1.2.3, \
1.2.3-alpha, v0.0.1-alpha.3)."
            )));
        };

        Ok(Self {
            major: captures["major"]
                .parse()
                .expect("regex-validated major must parse"),
            minor: captures["minor"]
                .parse()
                .expect("regex-validated minor must parse"),
            patch: captures["patch"]
                .parse()
                .expect("regex-validated patch must parse"),
            prerelease: captures
                .name("prerelease")
                .map(|value| value.as_str().to_string()),
        })
    }

    /// Return the prerelease label without the numeric suffix.
    pub fn prerelease_label(&self) -> Option<&str> {
        self.prerelease
            .as_deref()
            .map(|value| value.split('.').next().unwrap_or(value))
    }

    /// Return the numeric prerelease suffix, if present.
    pub fn prerelease_number(&self) -> Option<u64> {
        let prerelease = self.prerelease.as_deref()?;
        let (_, number) = prerelease.split_once('.')?;

        number.parse().ok()
    }

    /// Return a copy with `major` incremented and lower parts reset.
    pub fn bump_major(&self) -> Self {
        Self {
            major: self.major + 1,
            minor: 0,
            patch: 0,
            prerelease: None,
        }
    }

    /// Return a copy with `minor` incremented and lower parts reset.
    pub fn bump_minor(&self) -> Self {
        Self {
            major: self.major,
            minor: self.minor + 1,
            patch: 0,
            prerelease: None,
        }
    }

    /// Return a copy with `patch` incremented and prerelease cleared.
    pub fn bump_patch(&self) -> Self {
        Self {
            major: self.major,
            minor: self.minor,
            patch: self.patch + 1,
            prerelease: None,
        }
    }

    /// Render the version as a git tag string with a leading `v`.
    pub fn to_tag(&self) -> String {
        let base = format!("v{}.{}.{}", self.major, self.minor, self.patch);

        match &self.prerelease {
            Some(prerelease) => format!("{base}-{prerelease}"),
            None => base,
        }
    }
}

impl fmt::Display for SemVer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(prerelease) = &self.prerelease {
            write!(formatter, "-{prerelease}")?;
        }
        Ok(())
    }
}

impl FromStr for SemVer {
    type Err = EngitError;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| {
                compare_prerelease(self.prerelease.as_deref(), other.prerelease.as_deref())
            })
    }
}

fn compare_prerelease(left: Option<&str>, right: Option<&str>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => {
            let left_parts = split_prerelease(left);
            let right_parts = split_prerelease(right);

            left_parts
                .0
                .cmp(right_parts.0)
                .then_with(|| match (left_parts.1, right_parts.1) {
                    (None, None) => Ordering::Equal,
                    (None, Some(_)) => Ordering::Less,
                    (Some(_), None) => Ordering::Greater,
                    (Some(left_number), Some(right_number)) => left_number.cmp(&right_number),
                })
        }
    }
}

fn split_prerelease(value: &str) -> (&str, Option<u64>) {
    match value.split_once('.') {
        Some((label, number)) => (label, number.parse().ok()),
        None => (value, None),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::SemVer;
    use crate::error::EngitError;

    #[test]
    fn parse_accepts_stable_and_prerelease_tags() {
        let stable = SemVer::parse("v1.2.3").expect("stable version should parse");
        let prerelease = SemVer::parse("1.2.3-alpha.4").expect("prerelease version should parse");

        assert_eq!(stable.major, 1);
        assert_eq!(stable.minor, 2);
        assert_eq!(stable.patch, 3);
        assert_eq!(stable.prerelease, None);

        assert_eq!(prerelease.prerelease.as_deref(), Some("alpha.4"));
        assert_eq!(prerelease.prerelease_label(), Some("alpha"));
        assert_eq!(prerelease.prerelease_number(), Some(4));
    }

    #[test]
    fn parse_rejects_invalid_versions() {
        let error = SemVer::parse("1.2").expect_err("invalid version should fail");

        assert!(matches!(error, EngitError::SemVer(_)));
    }

    #[test]
    fn bump_helpers_reset_lower_parts() {
        let version = SemVer::parse("1.2.3-alpha.2").expect("version should parse");

        assert_eq!(version.bump_major().to_string(), "2.0.0");
        assert_eq!(version.bump_minor().to_string(), "1.3.0");
        assert_eq!(version.bump_patch().to_string(), "1.2.4");
    }

    #[test]
    fn display_and_to_tag_match_python_behavior() {
        let stable = SemVer::parse("v1.2.3").expect("stable version should parse");
        let prerelease = SemVer::parse("1.2.3-alpha").expect("prerelease should parse");

        assert_eq!(stable.to_string(), "1.2.3");
        assert_eq!(stable.to_tag(), "v1.2.3");
        assert_eq!(prerelease.to_string(), "1.2.3-alpha");
        assert_eq!(prerelease.to_tag(), "v1.2.3-alpha");
    }

    #[test]
    fn ordering_places_stable_after_matching_prerelease() {
        let stable = SemVer::from_str("1.2.3").expect("stable version should parse");
        let prerelease =
            SemVer::from_str("1.2.3-alpha.1").expect("prerelease version should parse");
        let older = SemVer::from_str("1.2.2").expect("older version should parse");

        assert!(stable > prerelease);
        assert!(prerelease > older);
        assert!(SemVer::from_str("1.2.3-alpha.2").expect("version should parse") > prerelease);
    }
}
