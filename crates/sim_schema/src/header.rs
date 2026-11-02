//! `FileHeader`: the identity block every SABER file carries (subsystems §5.4, §10.4), and the
//! semantic versions it is checked with.

use serde::{Deserialize, Serialize};

/// `FileHeader::magic` of every SABER file.
pub const MAGIC: &str = "SABER";
/// `FileHeader::format` of a scenario file (`*.scn.ron`, IB-01).
pub const SCENE_FORMAT: &str = "scenario";
/// The scenario schema this build writes and reads (readers accept the same major, §10.4).
pub const SCENE_SCHEMA_VERSION: Semver = Semver::new(1, 0, 0);
/// `FileHeader::format` of a requirement set (`*.req.ron`, IB-03).
pub const REQ_FORMAT: &str = "requirements";
/// The requirement-set schema this build writes and reads.
pub const REQ_SCHEMA_VERSION: Semver = Semver::new(1, 0, 0);

/// Identity of a file: what it is, which schema it follows, which build wrote it.
///
/// Only `magic`, `format` and `schema_version` are required, so a hand-written file stays short;
/// writers fill in the rest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileHeader {
    /// Always [`MAGIC`].
    pub magic: String,
    /// What the file holds, for example [`SCENE_FORMAT`].
    pub format: String,
    /// Schema version, `major.minor.patch`.
    pub schema_version: String,
    /// `saber_version` of the writer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub saber_version: String,
    /// `build_id` of the writer (git sha, rustc, target, features; step 3a).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub build_id: String,
    /// Campaign the file belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign_id: Option<String>,
    /// `scene_hash` as the writer computed it. Informational in a scenario file: readers recompute
    /// it, and a differing value only means the file was edited after export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_hash: Option<u64>,
    /// `req_set_hash` of the requirement set the file was produced with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub req_set_hash: Option<u64>,
    /// Creation time, RFC 3339 UTC. Never read by the kernel (no wall clock in a run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_utc: Option<String>,
}

impl FileHeader {
    /// A header for a file of `format` at `schema_version`, written by this `saber_version`.
    pub fn new(format: &str, schema_version: Semver, saber_version: &str) -> Self {
        Self {
            magic: MAGIC.to_owned(),
            format: format.to_owned(),
            schema_version: schema_version.to_string(),
            saber_version: saber_version.to_owned(),
            build_id: String::new(),
            campaign_id: None,
            scene_hash: None,
            req_set_hash: None,
            created_utc: None,
        }
    }

    /// Check magic and format and that the schema version is readable by a reader of `supported`.
    pub fn check(&self, format: &str, supported: Semver) -> Result<(), HeaderError> {
        if self.magic != MAGIC {
            return Err(HeaderError::Magic(self.magic.clone()));
        }
        if self.format != format {
            return Err(HeaderError::Format {
                found: self.format.clone(),
                expected: format.to_owned(),
            });
        }
        let found = Semver::parse(&self.schema_version)
            .ok_or_else(|| HeaderError::BadVersion(self.schema_version.clone()))?;
        if found.major != supported.major {
            return Err(HeaderError::Unsupported { found, supported });
        }
        Ok(())
    }
}

/// Why a header was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// Not a SABER file.
    Magic(String),
    /// A SABER file of another kind.
    Format {
        /// The file's format.
        found: String,
        /// The format the reader wanted.
        expected: String,
    },
    /// `schema_version` is not `major.minor.patch`.
    BadVersion(String),
    /// A schema major this reader does not read.
    Unsupported {
        /// The file's schema version.
        found: Semver,
        /// The reader's schema version.
        supported: Semver,
    },
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeaderError::Magic(m) => write!(f, "magic is {m:?}, not {MAGIC:?}"),
            HeaderError::Format { found, expected } => {
                write!(f, "format is {found:?}, expected {expected:?}")
            }
            HeaderError::BadVersion(v) => {
                write!(f, "schema_version {v:?} is not major.minor.patch")
            }
            HeaderError::Unsupported { found, supported } => {
                write!(f, "schema version {found} is not readable by {supported}")
            }
        }
    }
}

impl std::error::Error for HeaderError {}

/// A `major.minor.patch` version (no pre-release or build suffixes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Semver {
    /// Incompatible changes.
    pub major: u64,
    /// Compatible additions.
    pub minor: u64,
    /// Fixes.
    pub patch: u64,
}

impl Semver {
    /// A version.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse `major.minor.patch`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.trim().split('.');
        let mut next = || -> Option<u64> {
            let p = parts.next()?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let v = Self::new(next()?, next()?, next()?);
        parts.next().is_none().then_some(v)
    }

    /// Whether a binary at `self` satisfies a file that needs `required`, by Cargo's caret rule:
    /// same major and at least the required version, and below 1.0 the same minor too.
    pub fn satisfies(self, required: Semver) -> bool {
        let same_line = if required.major == 0 {
            self.major == 0 && self.minor == required.minor
        } else {
            self.major == required.major
        };
        same_line && self >= required
    }
}

impl std::fmt::Display for Semver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parse_and_caret_rule() {
        assert_eq!(Semver::parse("1.2.3"), Some(Semver::new(1, 2, 3)));
        for bad in ["1.2", "1.2.3.4", "1.x.3", "", "1..3", "-1.0.0", "1.0.0-rc1"] {
            assert_eq!(Semver::parse(bad), None, "{bad}");
        }
        let v = |s| Semver::parse(s).unwrap();
        assert!(v("1.4.0").satisfies(v("1.2.9")));
        assert!(!v("1.2.0").satisfies(v("1.2.1")));
        assert!(!v("2.0.0").satisfies(v("1.0.0")));
        assert!(v("0.3.4").satisfies(v("0.3.1")));
        assert!(!v("0.4.0").satisfies(v("0.3.1")));
    }

    #[test]
    fn header_checks() {
        let h = FileHeader::new(SCENE_FORMAT, SCENE_SCHEMA_VERSION, "0.0.0");
        assert_eq!(h.check(SCENE_FORMAT, SCENE_SCHEMA_VERSION), Ok(()));
        let mut bad = h.clone();
        bad.magic = "SABERS".into();
        assert!(matches!(
            bad.check(SCENE_FORMAT, SCENE_SCHEMA_VERSION),
            Err(HeaderError::Magic(_))
        ));
        let mut bad = h.clone();
        bad.format = "requirements".into();
        assert!(matches!(
            bad.check(SCENE_FORMAT, SCENE_SCHEMA_VERSION),
            Err(HeaderError::Format { .. })
        ));
        let mut newer_minor = h.clone();
        newer_minor.schema_version = "1.7.0".into();
        assert_eq!(
            newer_minor.check(SCENE_FORMAT, SCENE_SCHEMA_VERSION),
            Ok(())
        );
        let mut next_major = h;
        next_major.schema_version = "2.0.0".into();
        assert!(matches!(
            next_major.check(SCENE_FORMAT, SCENE_SCHEMA_VERSION),
            Err(HeaderError::Unsupported { .. })
        ));
    }
}
