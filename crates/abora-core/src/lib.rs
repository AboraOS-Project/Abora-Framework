//! Foundational types and constants shared across Abora Framework.
//!
//! Every other framework crate should be able to depend on this crate
//! without creating cycles. It contains no platform-specific code.

#![forbid(unsafe_code)]

use std::fmt;

/// Human-readable name of the framework.
pub const FRAMEWORK_NAME: &str = "Abora Framework";

/// Version of the framework, sourced from the workspace `Cargo.toml`.
pub const FRAMEWORK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Name of the framework daemon binary.
pub const DAEMON_NAME: &str = "aborad";

/// Name of the framework command line client.
pub const CLI_NAME: &str = "abora";

/// Current API version identifier, e.g. `v1`.
pub const API_VERSION: &str = "v1";

/// Path prefix under which the current API version is served.
pub const API_PATH_PREFIX: &str = "/api/v1";

/// Default address the daemon binds to. Loopback-only by default.
pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:7360";

/// Default configuration file path (Linux).
pub const DEFAULT_CONFIG_PATH: &str = "/etc/abora/abora.toml";

/// Structured, comparable version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Option<String>,
    pub build: Option<String>,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            prerelease: None,
            build: None,
        }
    }

    /// Parse a `major.minor.patch`-style version string.
    ///
    /// Optional `-prerelease` and `+build` suffixes are preserved as
    /// opaque strings. Returns an error for empty or malformed input.
    pub fn parse(input: &str) -> Result<Self, String> {
        let (rest, build) = match input.split_once('+') {
            Some((head, build)) => (head, Some(build.to_owned())),
            None => (input, None),
        };
        let (rest, prerelease) = match rest.split_once('-') {
            Some((head, pre)) => (head, Some(pre.to_owned())),
            None => (rest, None),
        };

        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() != 3 {
            return Err(format!("expected <major>.<minor>.<patch>, got `{input}`"));
        }

        let mut nums = [0u64; 3];
        for (slot, part) in nums.iter_mut().zip(parts.iter()) {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("invalid version number component `{part}`"));
            }
            *slot = part
                .parse::<u64>()
                .map_err(|_| format!("invalid version number component `{part}`"))?;
        }

        Ok(Self {
            major: nums[0],
            minor: nums[1],
            patch: nums[2],
            prerelease,
            build,
        })
    }

    /// The version this framework binary was built from.
    pub fn current() -> Self {
        Self::parse(FRAMEWORK_VERSION).unwrap_or_else(|_| Self::new(0, 0, 0))
    }
}

impl Default for Version {
    fn default() -> Self {
        Self::current()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.prerelease {
            write!(f, "-{pre}")?;
        }
        if let Some(build) = &self.build {
            write!(f, "+{build}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_version() {
        let v = Version::parse("1.2.3-alpha.1+build.7").unwrap();
        assert_eq!(
            v,
            Version {
                major: 1,
                minor: 2,
                patch: 3,
                prerelease: Some("alpha.1".into()),
                build: Some("build.7".into()),
            }
        );
        assert_eq!(v.to_string(), "1.2.3-alpha.1+build.7");
    }

    #[test]
    fn parse_simple_version() {
        let v = Version::parse("0.1.0").unwrap();
        assert_eq!(v.to_string(), "0.1.0");
        assert!(v.prerelease.is_none());
        assert!(v.build.is_none());
    }

    #[test]
    fn reject_malformed_versions() {
        assert!(Version::parse("").is_err());
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.x").is_err());
        assert!(Version::parse("1..3").is_err());
    }

    #[test]
    fn current_is_valid_semver_shape() {
        let v = Version::current();
        assert_eq!(v.to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn serializes_to_plain_object() {
        let v = Version::new(1, 2, 3);
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["major"], 1);
        assert_eq!(json["minor"], 2);
        assert_eq!(json["patch"], 3);
    }
}