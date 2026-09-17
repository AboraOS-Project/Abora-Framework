//! Reusable, platform-agnostic system information for Abora Framework.
//!
//! Cloud and Atlas must never re-implement system detection. This crate is
//! the single place that knows how to read hostname, kernel, architecture,
//! uptime, memory and OS facts from the local machine.
//!
//! # Separation of concerns
//!
//! * The public types in this module ([`SystemInfo`] and friends) are
//!   OS-agnostic and API/UI-facing.
//! * All Linux-specific reading and parsing lives in the private [`linux`]
//!   module behind `cfg(target_os = "linux")`. Porting to another platform
//!   means adding a sibling backend, not touching callers.
//!
//! No `unsafe` and no external binaries: on Linux everything is read from
//! `/proc`, `/etc/os-release` and compile-time target architecture.

#![forbid(unsafe_code)]

use std::fmt;
use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;

/// A complete snapshot of basic system facts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemInfo {
    pub hostname: String,
    pub os: OsInfo,
    pub kernel: KernelInfo,
    pub architecture: Architecture,
    pub machine: String,
    pub uptime: Duration,
    pub memory: MemoryInfo,
    pub cpu: CpuInfo,
}

impl SystemInfo {
    /// Collect information about the local system.
    pub fn collect() -> Result<Self, SysInfoError> {
        #[cfg(target_os = "linux")]
        {
            linux::collect()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(SysInfoError::NotSupported)
        }
    }
}

/// Operating-system identification, derived from `/etc/os-release` where
/// available. Fields are optional because minimal images may omit them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OsInfo {
    pub id: String,
    pub name: Option<String>,
    pub pretty_name: Option<String>,
    pub version: Option<String>,
    pub version_id: Option<String>,
    pub id_like: Vec<String>,
    pub home_url: Option<String>,
}

impl OsInfo {
    /// Placeholder used when `/etc/os-release` is absent.
    pub fn unknown() -> Self {
        Self {
            id: "unknown".to_owned(),
            name: None,
            pretty_name: None,
            version: None,
            version_id: None,
            id_like: Vec::new(),
            home_url: None,
        }
    }
}

/// Kernel identification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KernelInfo {
    /// Kernel name, typically `Linux`.
    pub name: String,
    /// Kernel release, e.g. `6.10.0-14-generic`.
    pub release: String,
    /// Kernel build string.
    pub version: String,
}

/// CPU architecture. Unknown machines are preserved rather than discarded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    X86,
    Aarch64,
    Arm,
    Riscv64,
    Riscv32,
    Powerpc64,
    S390x,
    Other(String),
}

impl Architecture {
    /// Map a `uname -m` style machine string to a known architecture.
    pub fn from_machine(machine: &str) -> Self {
        let m = machine.trim().to_ascii_lowercase();
        match m.as_str() {
            "x86_64" | "amd64" => Architecture::X86_64,
            "i386" | "i486" | "i586" | "i686" | "x86" => Architecture::X86,
            "aarch64" | "arm64" => Architecture::Aarch64,
            "riscv64" => Architecture::Riscv64,
            "riscv32" => Architecture::Riscv32,
            "ppc64" | "ppc64le" | "powerpc64" => Architecture::Powerpc64,
            "s390x" => Architecture::S390x,
            other if other.starts_with("arm") => Architecture::Arm,
            other => Architecture::Other(other.to_owned()),
        }
    }

    /// Canonical `uname -m` string.
    pub fn canonical(&self) -> String {
        match self {
            Architecture::X86_64 => "x86_64".to_owned(),
            Architecture::X86 => "i686".to_owned(),
            Architecture::Aarch64 => "aarch64".to_owned(),
            Architecture::Arm => "arm".to_owned(),
            Architecture::Riscv64 => "riscv64".to_owned(),
            Architecture::Riscv32 => "riscv32".to_owned(),
            Architecture::Powerpc64 => "ppc64".to_owned(),
            Architecture::S390x => "s390x".to_owned(),
            Architecture::Other(s) => s.clone(),
        }
    }
}

impl fmt::Display for Architecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Memory usage in bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
}

impl MemoryInfo {
    pub fn from_fields(
        total_bytes: u64,
        available_bytes: u64,
        free_bytes: u64,
        swap_total_bytes: u64,
        swap_free_bytes: u64,
    ) -> Self {
        Self {
            total_bytes,
            available_bytes,
            free_bytes,
            used_bytes: total_bytes.saturating_sub(available_bytes),
            swap_total_bytes,
            swap_used_bytes: swap_total_bytes.saturating_sub(swap_free_bytes),
        }
    }
}

/// CPU description.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CpuInfo {
    pub model: Option<String>,
    pub physical_cores: Option<usize>,
    pub logical_cores: Option<usize>,
}

/// Errors from system detection.
#[derive(Debug, thiserror::Error)]
pub enum SysInfoError {
    /// Running on a platform without a backend.
    #[error("system information is not supported on this platform")]
    NotSupported,
    /// A required file could not be read.
    #[error("could not read `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// A required value could not be parsed.
    #[error("could not parse {what}: {detail}")]
    Parse { what: String, detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architecture_mapping() {
        assert_eq!(Architecture::from_machine("x86_64"), Architecture::X86_64);
        assert_eq!(Architecture::from_machine("AMD64"), Architecture::X86_64);
        assert_eq!(Architecture::from_machine("aarch64"), Architecture::Aarch64);
        assert_eq!(Architecture::from_machine("armv7l"), Architecture::Arm);
        assert_eq!(Architecture::from_machine("riscv64"), Architecture::Riscv64);
        assert_eq!(
            Architecture::from_machine("weird-cpu"),
            Architecture::Other("weird-cpu".into())
        );
        assert_eq!(Architecture::from_machine("weird-cpu").to_string(), "weird-cpu");
        assert_eq!(Architecture::Aarch64.to_string(), "aarch64");
    }

    #[test]
    fn memory_used_is_total_minus_available() {
        let m = MemoryInfo::from_fields(1000, 250, 100, 500, 100);
        assert_eq!(m.used_bytes, 750);
        assert_eq!(m.swap_used_bytes, 400);
    }

    #[test]
    fn memory_used_saturates_when_inconsistent() {
        let m = MemoryInfo::from_fields(100, 200, 0, 0, 10);
        assert_eq!(m.used_bytes, 0);
        assert_eq!(m.swap_used_bytes, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn collects_real_system_information() {
        let info = SystemInfo::collect().expect("linux collection should succeed");
        assert!(!info.hostname.is_empty());
        assert!(!info.kernel.release.is_empty());
        assert!(info.memory.total_bytes > 0);
        assert!(info.uptime > Duration::ZERO);
        assert!(!info.machine.is_empty());
    }
}