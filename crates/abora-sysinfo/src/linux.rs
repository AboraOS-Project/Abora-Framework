//! Linux backend for [`crate::SystemInfo`].
//!
//! Everything here is Linux-specific and private to this crate. Parsing is
//! split into small, pure functions so it can be unit tested against sample
//! file contents without touching the real machine.

use std::collections::BTreeMap;
use std::time::Duration;

use super::{Architecture, CpuInfo, KernelInfo, MemoryInfo, OsInfo, SysInfoError, SystemInfo};

pub(crate) fn collect() -> Result<SystemInfo, SysInfoError> {
    let hostname = read("/proc/sys/kernel/hostname")?.trim().to_owned();
    if hostname.is_empty() {
        return Err(SysInfoError::Parse {
            what: "/proc/sys/kernel/hostname".to_owned(),
            detail: "hostname is empty".to_owned(),
        });
    }

    let kernel = KernelInfo {
        name: read("/proc/sys/kernel/ostype")
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| "Linux".to_owned()),
        release: read("/proc/sys/kernel/osrelease")?.trim().to_owned(),
        version: read("/proc/sys/kernel/version")
            .unwrap_or_default()
            .trim()
            .to_owned(),
    };

    // Use the target architecture the framework was built for. A future
    // backend can prefer the kernel-reported machine via uname(2).
    let machine = std::env::consts::ARCH.to_owned();
    let architecture = Architecture::from_machine(&machine);

    let uptime = parse_uptime(&read("/proc/uptime")?)?;
    let memory = parse_meminfo(&read("/proc/meminfo")?)?;

    let os = match read("/etc/os-release") {
        Ok(text) => parse_os_release(&text),
        Err(_) => OsInfo::unknown(),
    };
    let cpu = match read("/proc/cpuinfo") {
        Ok(text) => parse_cpuinfo(&text),
        Err(_) => CpuInfo::default(),
    };

    Ok(SystemInfo {
        hostname,
        os,
        kernel,
        architecture,
        machine,
        uptime,
        memory,
        cpu,
    })
}

fn read(path: &str) -> Result<String, SysInfoError> {
    std::fs::read_to_string(path).map_err(|source| SysInfoError::Io {
        path: path.to_owned(),
        source,
    })
}

/// Parse the first field of `/proc/uptime`, which is seconds as a float.
pub(crate) fn parse_uptime(text: &str) -> Result<Duration, SysInfoError> {
    let token = text.split_whitespace().next().ok_or_else(|| SysInfoError::Parse {
        what: "/proc/uptime".to_owned(),
        detail: "empty file".to_owned(),
    })?;
    let secs: f64 = token.parse().map_err(|_| SysInfoError::Parse {
        what: "/proc/uptime".to_owned(),
        detail: format!("`{token}` is not a number"),
    })?;
    if !secs.is_finite() || secs < 0.0 {
        return Err(SysInfoError::Parse {
            what: "/proc/uptime".to_owned(),
            detail: format!("`{token}` is not a valid duration"),
        });
    }
    Ok(Duration::from_secs_f64(secs))
}

/// Parse `/proc/meminfo`. Values are reported in kibibytes.
pub(crate) fn parse_meminfo(text: &str) -> Result<MemoryInfo, SysInfoError> {
    let mut fields: BTreeMap<&str, u64> = BTreeMap::new();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        fields.insert(key.trim(), value);
    }

    let total = fields.get("MemTotal").copied().ok_or_else(|| SysInfoError::Parse {
        what: "/proc/meminfo".to_owned(),
        detail: "MemTotal is missing".to_owned(),
    })?;
    let free = fields.get("MemFree").copied().unwrap_or(0);
    let available = fields.get("MemAvailable").copied().unwrap_or(free);
    let swap_total = fields.get("SwapTotal").copied().unwrap_or(0);
    let swap_free = fields.get("SwapFree").copied().unwrap_or(0);

    const KIB: u64 = 1024;
    Ok(MemoryInfo::from_fields(
        total * KIB,
        available * KIB,
        free * KIB,
        swap_total * KIB,
        swap_free * KIB,
    ))
}

/// Parse `/etc/os-release` (or `/usr/lib/os-release`).
pub(crate) fn parse_os_release(text: &str) -> OsInfo {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key.trim().to_owned(), unquote(value.trim()));
        }
    }

    let get = |key: &str| map.get(key).cloned();

    OsInfo {
        id: get("ID").unwrap_or_else(|| "unknown".to_owned()),
        name: get("NAME"),
        pretty_name: get("PRETTY_NAME"),
        version: get("VERSION"),
        version_id: get("VERSION_ID"),
        id_like: get("ID_LIKE")
            .map(|v| v.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default(),
        home_url: get("HOME_URL"),
    }
}

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let inner = &value[1..value.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(escaped) => out.push(escaped),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

/// Parse `/proc/cpuinfo`.
pub(crate) fn parse_cpuinfo(text: &str) -> CpuInfo {
    let mut model: Option<String> = None;
    let mut logical_cores = 0usize;
    let mut physical_cores: Option<usize> = None;

    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "processor" => logical_cores += 1,
            "model name" | "Model" | "Hardware" | "cpu model" => {
                if model.is_none() && !value.is_empty() {
                    model = Some(value.to_owned());
                }
            }
            "cpu cores" => {
                if physical_cores.is_none() {
                    physical_cores = value.parse::<usize>().ok();
                }
            }
            _ => {}
        }
    }

    CpuInfo {
        model,
        physical_cores,
        logical_cores: (logical_cores > 0).then_some(logical_cores),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uptime() {
        let d = parse_uptime("12345.67 98765.43\n").unwrap();
        assert_eq!(d.as_secs(), 12345);
        assert!(parse_uptime("").is_err());
        assert!(parse_uptime("abc 1").is_err());
        assert!(parse_uptime("-5 1").is_err());
    }

    #[test]
    fn parses_meminfo() {
        let sample = "\
MemTotal:       16384000 kB
MemFree:         2048000 kB
MemAvailable:    8192000 kB
Buffers:          100000 kB
SwapTotal:       2097152 kB
SwapFree:        1048576 kB
";
        let m = parse_meminfo(sample).unwrap();
        assert_eq!(m.total_bytes, 16_384_000 * 1024);
        assert_eq!(m.available_bytes, 8_192_000 * 1024);
        assert_eq!(m.free_bytes, 2_048_000 * 1024);
        assert_eq!(m.used_bytes, (16_384_000 - 8_192_000) * 1024);
        assert_eq!(m.swap_total_bytes, 2_097_152 * 1024);
        assert_eq!(m.swap_used_bytes, 1_048_576 * 1024);
    }

    #[test]
    fn meminfo_available_falls_back_to_free() {
        let sample = "MemTotal: 1000 kB\nMemFree: 400 kB\n";
        let m = parse_meminfo(sample).unwrap();
        assert_eq!(m.available_bytes, 400 * 1024);
        assert_eq!(m.swap_total_bytes, 0);
    }

    #[test]
    fn meminfo_requires_total() {
        assert!(parse_meminfo("MemFree: 1 kB\n").is_err());
    }

    #[test]
    fn parses_os_release_quoted_and_unquoted() {
        let sample = r#"
NAME="Pop!_OS"
VERSION="24.04 LTS (Jammy)"
ID=pop
ID_LIKE="ubuntu debian"
VERSION_ID="24.04"
PRETTY_NAME="Pop!_OS 24.04 LTS"
HOME_URL="https://pop.system76.com"
ANSI_COLOR='38;2;48;167;212'
# a comment
"#;
        let os = parse_os_release(sample);
        assert_eq!(os.id, "pop");
        assert_eq!(os.name.as_deref(), Some("Pop!_OS"));
        assert_eq!(os.pretty_name.as_deref(), Some("Pop!_OS 24.04 LTS"));
        assert_eq!(os.version_id.as_deref(), Some("24.04"));
        assert_eq!(os.id_like, vec!["ubuntu".to_owned(), "debian".to_owned()]);
        assert_eq!(os.home_url.as_deref(), Some("https://pop.system76.com"));
    }

    #[test]
    fn parses_escaped_os_release_values() {
        let sample = "NAME=\"A \\\"quoted\\\" name\"\nID=test\n";
        let os = parse_os_release(sample);
        assert_eq!(os.name.as_deref(), Some("A \"quoted\" name"));
    }

    #[test]
    fn unknown_os_when_fields_missing() {
        let os = parse_os_release("PRETTY_NAME=Nothing\n");
        assert_eq!(os.id, "unknown");
        assert!(os.id_like.is_empty());
    }

    #[test]
    fn parses_cpuinfo_x86() {
        let sample = "\
processor\t: 0
model name\t: AMD Ryzen 9 5900X 12-Core Processor
cpu cores\t: 12

processor\t: 1
model name\t: AMD Ryzen 9 5900X 12-Core Processor
cpu cores\t: 12
";
        let cpu = parse_cpuinfo(sample);
        assert_eq!(cpu.logical_cores, Some(2));
        assert_eq!(cpu.physical_cores, Some(12));
        assert_eq!(
            cpu.model.as_deref(),
            Some("AMD Ryzen 9 5900X 12-Core Processor")
        );
    }

    #[test]
    fn parses_cpuinfo_arm_without_counts() {
        let sample = "Hardware\t: BCM2835\n";
        let cpu = parse_cpuinfo(sample);
        assert_eq!(cpu.model.as_deref(), Some("BCM2835"));
        assert_eq!(cpu.logical_cores, None);
        assert_eq!(cpu.physical_cores, None);
    }
}