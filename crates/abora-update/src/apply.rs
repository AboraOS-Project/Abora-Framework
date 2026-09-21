//! Types and rules shared by `aborad` (which *asks* for an update) and `abora-apply` (the small
//! root helper that *performs* it). See `docs/apply-design.md`.
//!
//! The two never talk directly. `aborad` writes an [`ApplyRequest`] file it owns; the helper
//! reads it, re-checks everything itself, and writes an [`ApplyResult`] file into a directory only
//! root can write. Because the helper never trusts the request, the request carries **no command,
//! path or free-form argument**: only a list of package names, each validated here.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::host::valid_package_name;

/// Most packages one request may name.
pub const MAX_PACKAGES: usize = 2000;
/// A request older than this is ignored, so a stale file can never be replayed later.
pub const MAX_REQUEST_AGE: Duration = Duration::from_secs(10 * 60);
/// Largest request/result file that is read.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Where the helper writes its result (a root-owned directory `aborad` can read).
pub const DEFAULT_RESULT_FILE: &str = "/var/lib/abora-apply/result.json";
/// File name of the request, next to the daemon's state file.
pub const REQUEST_FILE_NAME: &str = "apply-request.json";

/// What `aborad` asks for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApplyRequest {
    pub id: String,
    /// RFC 3339, informational (the helper judges age by the file's modification time).
    pub requested_at: String,
    /// Token name of the caller, for the audit trail.
    pub requested_by: String,
    /// Packages the last check listed. Never anything else.
    pub packages: Vec<String>,
}

/// What the helper reports back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ApplyResult {
    pub id: String,
    pub started_at: String,
    pub finished_at: String,
    pub succeeded: bool,
    /// Packages that were upgraded (empty on failure or when nothing was left to do).
    pub packages: Vec<String>,
    /// Why it failed or was refused, or a note such as "nothing to do". Never secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Whether the helper started a reboot because policy allowed it.
    #[serde(default)]
    pub reboot_started: bool,
}

/// Request ids are 8 to 64 characters of `[A-Za-z0-9-]`.
pub fn valid_request_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Check a request's shape. The helper calls this on whatever it finds on disk.
pub fn validate_request(request: &ApplyRequest) -> Result<(), String> {
    if !valid_request_id(&request.id) {
        return Err("request id is not valid".into());
    }
    if request.packages.is_empty() {
        return Err("request names no packages".into());
    }
    if request.packages.len() > MAX_PACKAGES {
        return Err(format!("request names more than {MAX_PACKAGES} packages"));
    }
    if let Some(bad) = request.packages.iter().find(|p| !valid_package_name(p)) {
        // Deliberately not echoing the value: it came from a file the helper does not trust.
        return Err(format!(
            "request contains an invalid package name ({} bytes)",
            bad.len()
        ));
    }
    Ok(())
}

/// The fixed `apt-get` arguments that upgrade exactly `packages`, and nothing else.
/// `--only-upgrade` never installs a package that is not already installed, `--no-remove` never
/// removes one, and `--force-confold` keeps the administrator's edited configuration files.
pub fn install_args(packages: &[String]) -> Vec<String> {
    let mut args: Vec<String> = [
        "-y",
        "--no-remove",
        "-o",
        "Dpkg::Options::=--force-confold",
        "-o",
        "Dpkg::Options::=--force-confdef",
        "install",
        "--only-upgrade",
        "--",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    args.extend(packages.iter().cloned());
    args
}

/// Write `bytes` to `path` atomically (temp file, sync, rename) with the given Unix mode.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = path.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Read a small regular file. A symlink, a directory or an oversized file is refused, and
/// `max_age` (if given) refuses a file last modified longer ago than that.
pub fn read_regular_file(path: &Path, max_age: Option<Duration>) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is larger than {MAX_FILE_BYTES} bytes",
            path.display()
        ));
    }
    if let Some(max_age) = max_age {
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            // A modification time in the future is treated as brand new.
            .unwrap_or(Duration::ZERO);
        if age > max_age {
            return Err(format!(
                "{} is older than {} minutes",
                path.display(),
                max_age.as_secs() / 60
            ));
        }
    }
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(packages: &[&str]) -> ApplyRequest {
        ApplyRequest {
            id: "req-0123abcd".into(),
            requested_at: "2026-09-21T02:00:00Z".into(),
            requested_by: "ops".into(),
            packages: packages.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("abora-apply-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_good_request_validates() {
        assert!(validate_request(&request(&[
            "openssl",
            "libc6",
            "g++-13",
            "libssl3t64:amd64"
        ]))
        .is_ok());
    }

    #[test]
    fn bad_requests_are_refused() {
        assert!(validate_request(&request(&[])).is_err());
        for bad in ["-evil", "../etc", "a b", "A", "x;rm", "", "a\nb", "--yes"] {
            assert!(validate_request(&request(&["ok", bad])).is_err(), "{bad:?}");
        }
        let mut r = request(&["ok"]);
        r.id = "short".into();
        assert!(validate_request(&r).is_err());
        r.id = "has space in it".into();
        assert!(validate_request(&r).is_err());
        let many: Vec<String> = (0..=MAX_PACKAGES).map(|i| format!("pkg{i}")).collect();
        let mut r = request(&["ok"]);
        r.packages = many;
        assert!(validate_request(&r).is_err());
    }

    #[test]
    fn the_error_does_not_echo_untrusted_input() {
        let err = validate_request(&request(&["$(reboot)"])).unwrap_err();
        assert!(!err.contains("reboot"), "{err}");
    }

    #[test]
    fn install_args_are_fixed_and_end_options_before_packages() {
        let args = install_args(&["openssl".into(), "libc6".into()]);
        let dashdash = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(&args[dashdash + 1..], ["openssl", "libc6"]);
        for needed in ["-y", "--no-remove", "--only-upgrade", "install"] {
            assert!(args[..dashdash].contains(&needed.to_owned()), "{needed}");
        }
        assert!(!args
            .iter()
            .any(|a| a.contains("dist-upgrade") || a.contains("remove") && a != "--no-remove"));
    }

    #[test]
    fn atomic_write_is_private_and_leaves_no_temp_file() {
        let dir = scratch("atomic");
        let path = dir.join("nested").join("r.json");
        write_atomic(&path, b"{}", 0o600).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_directories_are_not_read() {
        let dir = scratch("nolink");
        let real = dir.join("real.json");
        fs::write(&real, b"{}").unwrap();
        std::os::unix::fs::symlink(&real, dir.join("link.json")).unwrap();
        assert!(read_regular_file(&real, None).is_ok());
        assert!(read_regular_file(&dir.join("link.json"), None)
            .unwrap_err()
            .contains("not a regular file"));
        assert!(read_regular_file(&dir, None).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_and_stale_files_are_refused() {
        let dir = scratch("limits");
        let big = dir.join("big.json");
        fs::write(&big, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert!(read_regular_file(&big, None)
            .unwrap_err()
            .contains("larger"));
        let fresh = dir.join("fresh.json");
        fs::write(&fresh, b"{}").unwrap();
        assert!(read_regular_file(&fresh, Some(MAX_REQUEST_AGE)).is_ok());
        // Make it look 20 minutes old: now it is stale.
        let old = SystemTime::now() - Duration::from_secs(20 * 60);
        fs::OpenOptions::new()
            .write(true)
            .open(&fresh)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(read_regular_file(&fresh, Some(MAX_REQUEST_AGE))
            .unwrap_err()
            .contains("older than"));
        assert!(
            read_regular_file(&fresh, None).is_ok(),
            "no age limit, no complaint"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_and_result_round_trip_as_json() {
        let r = request(&["openssl"]);
        assert_eq!(
            serde_json::from_str::<ApplyRequest>(&serde_json::to_string(&r).unwrap()).unwrap(),
            r
        );
        let res = ApplyResult {
            id: r.id.clone(),
            started_at: "a".into(),
            finished_at: "b".into(),
            succeeded: false,
            packages: vec![],
            note: Some("boom".into()),
            reboot_started: false,
        };
        assert_eq!(
            serde_json::from_str::<ApplyResult>(&serde_json::to_string(&res).unwrap()).unwrap(),
            res
        );
    }
}
