//! The host-package provider: read-only update checks against the system's `apt`.
//!
//! This is the first real [`UpdateProvider`]. It **never installs anything**: `apply`
//! keeps the trait's default refusal.
//!
//! * `check` runs `apt-get -s upgrade` (a simulation: no root, no changes) and turns each
//!   `Inst` line into an [`AvailableUpdate`]. It reads the package lists apt already has;
//!   refreshing them (`apt-get update`) needs root and is left to the system's own timers.
//! * `reboot_required` looks for Debian/Ubuntu's `/var/run/reboot-required` marker.
//! * `history` and the last-check time come from the [`UpdateStore`].
//!
//! Commands are run directly, never through a shell, with a fixed argument list, `LC_ALL=C`
//! and a timeout. Package names are validated before they are used.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use abora_core::Version;

use crate::store::UpdateStore;
use crate::{
    AvailableUpdate, AvailableUpdates, Channel, RebootStatus, UpdateError, UpdateHistoryEntry,
    UpdateProvider, UpdateStatus,
};

/// How long an `apt-get` simulation may run before it is killed.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// Runs an external program and returns its standard output. A seam for tests.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

/// Runs real programs: no shell, `LC_ALL=C`, killed after `timeout`. On failure the error
/// carries the exit status and the tail of standard error (never standard input or the
/// environment).
pub struct SystemRunner {
    timeout: Duration,
}

impl SystemRunner {
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self::new(COMMAND_TIMEOUT)
    }
}

/// The last `max` bytes of `text`, cut on a character boundary.
pub fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text.trim();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].trim()
}

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let mut child = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run {program}: {e}"))?;
        fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
            std::thread::spawn(move || {
                let mut out = String::new();
                let _ = pipe.read_to_string(&mut out);
                out
            })
        }
        let out_reader = drain(child.stdout.take().expect("stdout was piped"));
        let err_reader = drain(child.stderr.take().expect("stderr was piped"));
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() > self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{program} timed out after {}s",
                        self.timeout.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(format!("waiting for {program}: {e}")),
            }
        };
        let out = out_reader.join().unwrap_or_default();
        let err = err_reader.join().unwrap_or_default();
        if status.success() {
            Ok(out)
        } else if err.trim().is_empty() {
            Err(format!("{program} exited with {status}"))
        } else {
            Err(format!(
                "{program} exited with {status}: {}",
                tail(&err, 600)
            ))
        }
    }
}

/// One package apt would upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageUpgrade {
    pub name: String,
    /// Installed version; `None` when the package is not installed yet (a new dependency).
    pub from: Option<String>,
    pub to: String,
    /// Where it comes from, e.g. `Ubuntu:24.04/noble-updates [amd64]`.
    pub source: String,
}

/// Package names apt accepts: lowercase letters, digits and `+ - . :` (arch qualifier).
pub fn valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.:".contains(c))
}

/// Parse the `Inst` lines of `apt-get -s` output. Anything else is ignored, and so is any
/// `Inst` line that does not have the expected shape or has a suspicious package name.
///
/// ```text
/// Inst libssl3t64 [3.0.13-0ubuntu3.4] (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])
/// Inst newdep (1.2-1 Ubuntu:24.04/noble [amd64])
/// ```
pub fn parse_simulation(output: &str) -> Vec<PackageUpgrade> {
    output.lines().filter_map(parse_inst_line).collect()
}

fn parse_inst_line(line: &str) -> Option<PackageUpgrade> {
    let rest = line.strip_prefix("Inst ")?;
    let (name, rest) = rest.split_once(' ')?;
    if !valid_package_name(name) {
        return None;
    }
    let (from, rest) = match rest.strip_prefix('[') {
        Some(after) => {
            let (from, tail) = after.split_once(']')?;
            (Some(from.to_owned()), tail.trim_start())
        }
        None => (None, rest),
    };
    // apt may add trailing markers after the closing paren (for example ` []`), so stop at the first `)`.
    let inner = rest.strip_prefix('(')?;
    let inner = inner.split_once(')').map_or(inner, |(inside, _)| inside);
    let (to, source) = inner.split_once(' ').unwrap_or((inner, ""));
    if to.is_empty() {
        return None;
    }
    Some(PackageUpgrade {
        name: name.to_owned(),
        from,
        to: to.to_owned(),
        source: source.to_owned(),
    })
}

/// Best-effort semantic version from a Debian version like `2:1.4.7-3ubuntu2`: the epoch is
/// dropped and the leading numeric `a.b.c` parts are used, anything unparseable is `0.0.0`.
/// The exact Debian string is always kept in the update's summary.
fn version_from_debian(version: &str) -> Version {
    let no_epoch = version.split_once(':').map_or(version, |(_, v)| v);
    let mut parts = no_epoch
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("")
        .split('.');
    let mut next = || {
        parts
            .next()
            .and_then(|p| p.parse::<u64>().ok())
            .unwrap_or(0)
    };
    Version::new(next(), next(), next())
}

/// The outcome of the last `check`, kept in memory for `status`.
#[derive(Debug, Clone)]
enum LastCheck {
    Updates(Vec<String>),
    Failed(String),
}

/// Read-only provider backed by the host's package manager (apt).
pub struct HostPackageProvider {
    store: Arc<UpdateStore>,
    runner: Box<dyn CommandRunner>,
    reboot_marker: PathBuf,
    now: fn() -> String,
    last: Mutex<Option<LastCheck>>,
}

impl HostPackageProvider {
    /// A provider using the real system: `apt-get`, `/var/run/reboot-required` and the wall clock.
    pub fn new(store: Arc<UpdateStore>) -> Self {
        Self::with_parts(
            store,
            Box::new(SystemRunner::default()),
            "/var/run/reboot-required".into(),
            abora_log::rfc3339_now,
        )
    }

    /// Full control over the parts, for tests.
    pub fn with_parts(
        store: Arc<UpdateStore>,
        runner: Box<dyn CommandRunner>,
        reboot_marker: PathBuf,
        now: fn() -> String,
    ) -> Self {
        Self {
            store,
            runner,
            reboot_marker,
            now,
            last: Mutex::new(None),
        }
    }

    fn set_last(&self, value: LastCheck) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
    }
}

fn store_err(e: crate::store::StoreError) -> UpdateError {
    UpdateError::Source(e.to_string())
}

impl UpdateProvider for HostPackageProvider {
    /// Host packages have no channels: the `channel` argument is only echoed back.
    fn check(&self, channel: &Channel) -> Result<AvailableUpdates, UpdateError> {
        let output = match self
            .runner
            .run("apt-get", &["-s", "-o", "Debug::NoLocking=1", "upgrade"])
        {
            Ok(out) => out,
            Err(message) => {
                self.set_last(LastCheck::Failed(message.clone()));
                return Err(UpdateError::Source(message));
            }
        };
        let upgrades = parse_simulation(&output);
        self.set_last(LastCheck::Updates(
            upgrades
                .iter()
                .map(|u| format!("{} {}", u.name, u.to))
                .collect(),
        ));
        self.store.record_check((self.now)()).map_err(store_err)?;

        let updates = upgrades
            .into_iter()
            .map(|u| AvailableUpdate {
                channel: channel.clone(),
                version: version_from_debian(&u.to),
                summary: match &u.from {
                    Some(from) => format!("{}: {from} -> {} ({})", u.name, u.to, u.source),
                    None => format!("{}: new, {} ({})", u.name, u.to, u.source),
                },
                component: u.name,
                release_notes: None,
                // apt's simulation does not report download sizes.
                size_bytes: 0,
                released_at: None,
                checksums: None,
            })
            .collect();
        Ok(AvailableUpdates {
            channel: channel.clone(),
            updates,
        })
    }

    /// Status as of the last `check`. Before any check has run there is nothing honest to say,
    /// so that is reported as a conflict rather than "up to date".
    fn status(&self) -> Result<UpdateStatus, UpdateError> {
        match self.last.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            None => Err(UpdateError::Conflict("no update check has run yet".into())),
            Some(LastCheck::Failed(message)) => Ok(UpdateStatus::Error { message }),
            Some(LastCheck::Updates(v)) if v.is_empty() => Ok(UpdateStatus::UpToDate),
            Some(LastCheck::Updates(versions)) => Ok(UpdateStatus::UpdateAvailable { versions }),
        }
    }

    fn reboot_required(&self) -> Result<RebootStatus, UpdateError> {
        let current = self.store.reboot();
        if !self.reboot_marker.exists() {
            if current.required {
                self.store
                    .set_reboot(RebootStatus::default())
                    .map_err(store_err)?;
            }
            return Ok(RebootStatus::default());
        }
        if current.required {
            return Ok(current);
        }
        // First time we see the marker: remember when, and why if the system says.
        let packages = std::fs::read_to_string(self.reboot_marker.with_extension("pkgs"))
            .map(|s| s.lines().take(5).collect::<Vec<_>>().join(", "))
            .ok()
            .filter(|s| !s.is_empty());
        let status = RebootStatus {
            required: true,
            reason: Some(match packages {
                Some(p) => format!("updated packages need a reboot: {p}"),
                None => "the system reports that a reboot is required".into(),
            }),
            pending_since: Some((self.now)()),
        };
        self.store.set_reboot(status.clone()).map_err(store_err)?;
        Ok(status)
    }

    fn history(&self) -> Result<Vec<UpdateHistoryEntry>, UpdateError> {
        Ok(self.store.history())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
NOTE: This is only a simulation!
Reading package lists...
The following packages will be upgraded:
  libssl3t64 openssl
Inst libssl3t64 [3.0.13-0ubuntu3.4] (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates, Ubuntu:24.04/noble-security [amd64])
Inst openssl [3.0.13-0ubuntu3.4] (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])
Inst newdep (2:1.2-1 Ubuntu:24.04/noble [amd64])
Inst libc6-dev [2.39-0ubuntu8.7] (2.39-0ubuntu8.9 Ubuntu:24.04/noble-updates [amd64]) []
Conf libssl3t64 (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])
Inst ../etc/passwd [1] (2 x)
Inst -evil [1] (2 x)
Inst broken line without parens
";

    struct Fake(Result<String, String>);
    impl CommandRunner for Fake {
        fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
            assert_eq!(program, "apt-get");
            assert_eq!(
                args,
                ["-s", "-o", "Debug::NoLocking=1", "upgrade"],
                "fixed, read-only arguments"
            );
            self.0.clone()
        }
    }

    fn provider(
        result: Result<String, String>,
        marker: &str,
    ) -> (HostPackageProvider, Arc<UpdateStore>) {
        let store = Arc::new(UpdateStore::in_memory());
        let p = HostPackageProvider::with_parts(
            store.clone(),
            Box::new(Fake(result)),
            marker.into(),
            || "2026-09-21T04:00:00Z".into(),
        );
        (p, store)
    }

    #[test]
    fn parses_inst_lines_and_ignores_everything_else() {
        let up = parse_simulation(SAMPLE);
        let names: Vec<_> = up.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(
            names,
            ["libssl3t64", "openssl", "newdep", "libc6-dev"],
            "Conf, bad names and broken lines are skipped"
        );
        assert_eq!(
            up[3].source, "Ubuntu:24.04/noble-updates [amd64]",
            "trailing markers are not part of the source"
        );
        assert_eq!(up[0].from.as_deref(), Some("3.0.13-0ubuntu3.4"));
        assert_eq!(up[0].to, "3.0.13-0ubuntu3.5");
        assert!(up[0].source.contains("noble-security"));
        assert_eq!(
            up[2].from, None,
            "a new dependency has no installed version"
        );
        assert_eq!(up[2].to, "2:1.2-1");
    }

    #[test]
    fn debian_versions_map_to_best_effort_semver() {
        assert_eq!(
            version_from_debian("3.0.13-0ubuntu3.5"),
            Version::new(3, 0, 13)
        );
        assert_eq!(version_from_debian("2:1.2-1"), Version::new(1, 2, 0));
        assert_eq!(version_from_debian("1.4.7+dfsg-3"), Version::new(1, 4, 7));
        assert_eq!(version_from_debian("weird"), Version::new(0, 0, 0));
    }

    #[test]
    fn check_reports_updates_and_status_and_records_the_time() {
        let (p, store) = provider(Ok(SAMPLE.into()), "/nonexistent/reboot-required");
        assert!(
            matches!(p.status(), Err(UpdateError::Conflict(_))),
            "no claim before the first check"
        );

        let found = p.check(&Channel::Stable).unwrap();
        assert_eq!(found.updates.len(), 4);
        assert!(found.updates[0]
            .summary
            .starts_with("libssl3t64: 3.0.13-0ubuntu3.4 -> 3.0.13-0ubuntu3.5"));
        assert_eq!(found.updates[0].component, "libssl3t64");
        assert_eq!(store.last_check().as_deref(), Some("2026-09-21T04:00:00Z"));
        match p.status().unwrap() {
            UpdateStatus::UpdateAvailable { versions } => {
                assert_eq!(versions[1], "openssl 3.0.13-0ubuntu3.5")
            }
            other => panic!("expected UpdateAvailable, got {other:?}"),
        }
    }

    #[test]
    fn nothing_to_upgrade_is_up_to_date() {
        let (p, _) = provider(
            Ok("Reading package lists...\n0 upgraded, 0 newly installed\n".into()),
            "/nonexistent/x",
        );
        assert!(p.check(&Channel::Stable).unwrap().updates.is_empty());
        assert_eq!(p.status().unwrap(), UpdateStatus::UpToDate);
    }

    #[test]
    fn a_failed_check_is_an_error_status_and_does_not_touch_the_store() {
        let (p, store) = provider(
            Err("apt-get exited with exit status: 100".into()),
            "/nonexistent/x",
        );
        assert!(matches!(
            p.check(&Channel::Stable),
            Err(UpdateError::Source(_))
        ));
        assert!(matches!(p.status().unwrap(), UpdateStatus::Error { .. }));
        assert_eq!(store.last_check(), None);
    }

    #[test]
    fn reboot_marker_is_noticed_remembered_and_cleared() {
        let dir = std::env::temp_dir().join(format!("abora-host-reboot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("reboot-required");
        let (p, store) = provider(Ok(String::new()), marker.to_str().unwrap());

        assert!(!p.reboot_required().unwrap().required);

        std::fs::write(&marker, "*** System restart required ***\n").unwrap();
        std::fs::write(dir.join("reboot-required.pkgs"), "linux-image-6.8\nlibc6\n").unwrap();
        let status = p.reboot_required().unwrap();
        assert!(status.required);
        assert_eq!(
            status.pending_since.as_deref(),
            Some("2026-09-21T04:00:00Z")
        );
        assert!(status.reason.unwrap().contains("linux-image-6.8, libc6"));
        assert!(store.reboot().required, "remembered in the store");

        std::fs::remove_file(&marker).unwrap();
        assert!(!p.reboot_required().unwrap().required);
        assert!(!store.reboot().required, "cleared once the marker is gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_is_refused() {
        let (p, _) = provider(Ok(SAMPLE.into()), "/nonexistent/x");
        let update = p.check(&Channel::Stable).unwrap().updates.remove(0);
        assert!(matches!(
            p.apply(&update),
            Err(UpdateError::NotImplemented(_))
        ));
    }

    #[test]
    fn real_apt_smoke_test() {
        if Command::new("apt-get").arg("--version").output().is_err() {
            return; // not a Debian-family system
        }
        let store = Arc::new(UpdateStore::in_memory());
        let p = HostPackageProvider::new(store);
        // Either a list (possibly empty) or a reported failure: never a panic or a hang.
        match p.check(&Channel::Stable) {
            Ok(found) => assert!(found
                .updates
                .iter()
                .all(|u| valid_package_name(&u.component))),
            Err(UpdateError::Source(_)) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
