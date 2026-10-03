//! The Nix provider: updates for NixOS systems built from a flake.
//!
//! A Framework server on NixOS is described by a flake (by default `/etc/nixos`). Its
//! `flake.lock` pins every input to a revision; "an update" means an input whose upstream has
//! moved on. This provider:
//!
//! * `check` reads `flake.lock`, and for every root input that tracks a branch (a `github`,
//!   `gitlab`, `git` or `tarball` input) asks `nix flake metadata --refresh` what that branch
//!   points to now. Inputs whose revision changed are reported as updates. Nothing is written:
//!   the lock file is only read, and no root is needed.
//! * `reboot_required` compares the kernel the machine booted (`/run/booted-system/kernel`)
//!   with the kernel of the activated system (`/run/current-system/kernel`).
//! * Applying is done by the root helper `abora-apply` (`nix flake update <inputs>` followed by
//!   `nixos-rebuild switch`, restoring the old lock file if the rebuild fails); see
//!   `docs/apply-design.md`.
//!
//! As with the apt provider, commands run directly (no shell), with fixed argument lists, and
//! input names are validated before they are used.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use abora_core::Version;

use crate::host::{CommandRunner, SystemRunner};
use crate::store::UpdateStore;
use crate::{
    AvailableUpdate, AvailableUpdates, Channel, RebootStatus, UpdateError, UpdateHistoryEntry,
    UpdateProvider, UpdateStatus,
};

/// Default location of the system flake on NixOS.
pub const DEFAULT_FLAKE_DIR: &str = "/etc/nixos";
/// Flake commands need these features on Nix versions where they are still experimental.
pub const NIX_FEATURES: [&str; 2] = ["--extra-experimental-features", "nix-command flakes"];

/// Flake input names: what `nix flake update <input>` accepts and nothing a shell or an option
/// parser could misread.
pub fn valid_input_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// One root input of the system flake that follows a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedInput {
    /// The name the flake gives it (`nixpkgs`, `anix`, ...).
    pub name: String,
    /// Flake reference of the branch it follows, e.g. `github:NixOS/nixpkgs/nixos-unstable`.
    pub url: String,
    /// Locked revision (or `narHash` for tarballs, which have no revision).
    pub locked: String,
}

/// An input whose branch has moved past the locked revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputUpdate {
    pub name: String,
    pub url: String,
    pub from: String,
    pub to: String,
    /// Upstream commit time (Unix seconds), when Nix reports it.
    pub last_modified: Option<u64>,
}

fn str_field<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(serde_json::Value::as_str)
}

/// The flake reference an input's `original` entry follows, if it is one this provider can
/// refresh (branches of github/gitlab/git repositories and tarballs). Inputs pinned to a fixed
/// revision, local paths and indirect (registry) references are left alone.
fn tracked_url(original: &serde_json::Value) -> Option<String> {
    if str_field(original, "rev").is_some() {
        return None; // pinned on purpose
    }
    // Names, branches and URLs come from a file on disk; only well-formed ones are ever passed on.
    let name_ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let ref_ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && !s.contains("..")
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    };
    let url_ok = |s: &str, schemes: &[&str]| {
        schemes.iter().any(|scheme| s.starts_with(scheme))
            && !s.chars().any(|c| c.is_whitespace() || c.is_control())
    };
    let reference = str_field(original, "ref");
    if reference.is_some_and(|r| !ref_ok(r)) {
        return None;
    }
    let url = match str_field(original, "type")? {
        kind @ ("github" | "gitlab") => {
            let owner = str_field(original, "owner").filter(|s| name_ok(s))?;
            let repo = str_field(original, "repo").filter(|s| name_ok(s))?;
            match reference {
                Some(r) => format!("{kind}:{owner}/{repo}/{r}"),
                None => format!("{kind}:{owner}/{repo}"),
            }
        }
        "git" => {
            let url = str_field(original, "url")
                .filter(|u| url_ok(u, &["https://", "http://", "ssh://", "git://"]))?;
            match reference {
                Some(r) => format!("git+{url}?ref={r}"),
                None => format!("git+{url}"),
            }
        }
        "tarball" => str_field(original, "url")
            .filter(|u| url_ok(u, &["https://", "http://"]))?
            .to_owned(),
        _ => return None,
    };
    Some(url)
}

/// The root inputs of a flake lock file that follow a branch, in lock-file order of their names.
pub fn tracked_inputs(lock_json: &str) -> Result<Vec<TrackedInput>, String> {
    let lock: serde_json::Value = serde_json::from_str(lock_json)
        .map_err(|e| format!("flake.lock is not valid JSON: {e}"))?;
    let nodes = lock
        .get("nodes")
        .and_then(serde_json::Value::as_object)
        .ok_or("flake.lock has no nodes")?;
    let root_name = str_field(&lock, "root").unwrap_or("root");
    let root = nodes.get(root_name).ok_or("flake.lock has no root node")?;
    let Some(inputs) = root.get("inputs").and_then(serde_json::Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut tracked = Vec::new();
    for (name, target) in inputs {
        // An array is a `follows` path: that input is pinned by another one.
        let Some(node_name) = target.as_str() else {
            continue;
        };
        if !valid_input_name(name) {
            continue;
        }
        let Some(node) = nodes.get(node_name) else {
            continue;
        };
        let (Some(original), Some(locked)) = (node.get("original"), node.get("locked")) else {
            continue;
        };
        let Some(url) = tracked_url(original) else {
            continue;
        };
        let Some(locked) = str_field(locked, "rev").or_else(|| str_field(locked, "narHash")) else {
            continue;
        };
        tracked.push(TrackedInput {
            name: name.clone(),
            url,
            locked: locked.to_owned(),
        });
    }
    Ok(tracked)
}

/// Ask Nix what `input` points to now. Returns `(revision or narHash, lastModified)`.
fn upstream(
    runner: &dyn CommandRunner,
    input: &TrackedInput,
) -> Result<(String, Option<u64>), String> {
    let mut args: Vec<&str> = NIX_FEATURES.to_vec();
    args.extend([
        "flake",
        "metadata",
        "--json",
        "--refresh",
        input.url.as_str(),
    ]);
    let out = runner.run("nix", &args)?;
    let meta: serde_json::Value = serde_json::from_str(&out)
        .map_err(|e| format!("nix flake metadata for `{}` was not JSON: {e}", input.name))?;
    let locked = meta.get("locked").unwrap_or(&serde_json::Value::Null);
    let current = str_field(locked, "rev")
        .or_else(|| str_field(&meta, "revision"))
        .or_else(|| str_field(locked, "narHash"))
        .ok_or_else(|| format!("nix reported no revision for `{}`", input.name))?;
    let last_modified = locked
        .get("lastModified")
        .or_else(|| meta.get("lastModified"))
        .and_then(serde_json::Value::as_u64);
    Ok((current.to_owned(), last_modified))
}

/// Read `<flake_dir>/flake.lock` and report every tracked input whose upstream has moved.
/// Inputs that cannot be refreshed (offline, removed upstream) make the whole check fail, so a
/// partial answer is never reported as "up to date".
pub fn pending_inputs(
    runner: &dyn CommandRunner,
    flake_dir: &Path,
) -> Result<Vec<InputUpdate>, String> {
    let lock_path = flake_dir.join("flake.lock");
    let lock = std::fs::read_to_string(&lock_path)
        .map_err(|e| format!("could not read {}: {e}", lock_path.display()))?;
    let mut updates = Vec::new();
    for input in tracked_inputs(&lock)? {
        let (current, last_modified) = upstream(runner, &input)?;
        if current != input.locked {
            updates.push(InputUpdate {
                name: input.name,
                url: input.url,
                from: input.locked,
                to: current,
                last_modified,
            });
        }
    }
    Ok(updates)
}

/// A short form of a revision for summaries (`1a2b3c4`), or the hash part of a narHash.
fn short(rev: &str) -> String {
    let rev = rev.split_once('-').map_or(rev, |(_, h)| h);
    rev.chars().take(7).collect()
}

/// Upstream commit time as a date-shaped version (`2026.10.3`), so newer is bigger.
fn version_from_time(seconds: Option<u64>) -> Version {
    let Some(seconds) = seconds else {
        return Version::new(0, 0, 0);
    };
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Version::new(year as u64, month as u64, day as u64)
}

/// Is the activated system's kernel different from the one the machine booted?
pub fn kernel_changed(booted: &Path, current: &Path) -> bool {
    match (
        std::fs::canonicalize(booted),
        std::fs::canonicalize(current),
    ) {
        (Ok(b), Ok(c)) => b != c,
        _ => false,
    }
}

enum LastCheck {
    Updates(Vec<String>),
    Failed(String),
}

/// Provider for NixOS systems built from a flake.
pub struct NixProvider {
    store: Arc<UpdateStore>,
    runner: Box<dyn CommandRunner>,
    flake_dir: PathBuf,
    booted_kernel: PathBuf,
    current_kernel: PathBuf,
    now: fn() -> String,
    last: Mutex<Option<LastCheck>>,
}

impl NixProvider {
    /// The real system: `nix`, the given flake, `/run/{booted,current}-system/kernel`, the clock.
    pub fn new(store: Arc<UpdateStore>, flake_dir: PathBuf) -> Self {
        Self::with_parts(
            store,
            Box::new(SystemRunner::default()),
            flake_dir,
            PathBuf::from("/run/booted-system/kernel"),
            PathBuf::from("/run/current-system/kernel"),
            abora_log::rfc3339_now,
        )
    }

    /// Full control over the parts, for tests.
    pub fn with_parts(
        store: Arc<UpdateStore>,
        runner: Box<dyn CommandRunner>,
        flake_dir: PathBuf,
        booted_kernel: PathBuf,
        current_kernel: PathBuf,
        now: fn() -> String,
    ) -> Self {
        Self {
            store,
            runner,
            flake_dir,
            booted_kernel,
            current_kernel,
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

impl UpdateProvider for NixProvider {
    /// A flake follows its inputs' branches, not Framework channels: `channel` is echoed back.
    fn check(&self, channel: &Channel) -> Result<AvailableUpdates, UpdateError> {
        let pending = match pending_inputs(self.runner.as_ref(), &self.flake_dir) {
            Ok(p) => p,
            Err(message) => {
                self.set_last(LastCheck::Failed(message.clone()));
                return Err(UpdateError::Source(message));
            }
        };
        self.set_last(LastCheck::Updates(
            pending
                .iter()
                .map(|u| format!("{} {}", u.name, short(&u.to)))
                .collect(),
        ));
        self.store.record_check((self.now)()).map_err(store_err)?;
        let updates = pending
            .into_iter()
            .map(|u| AvailableUpdate {
                channel: channel.clone(),
                version: version_from_time(u.last_modified),
                summary: format!(
                    "{}: {} -> {} ({})",
                    u.name,
                    short(&u.from),
                    short(&u.to),
                    u.url
                ),
                component: u.name,
                release_notes: None,
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

    fn status(&self) -> Result<UpdateStatus, UpdateError> {
        match &*self.last.lock().unwrap_or_else(|e| e.into_inner()) {
            None => Err(UpdateError::Conflict("no update check has run yet".into())),
            Some(LastCheck::Failed(message)) => Ok(UpdateStatus::Error {
                message: message.clone(),
            }),
            Some(LastCheck::Updates(v)) if v.is_empty() => Ok(UpdateStatus::UpToDate),
            Some(LastCheck::Updates(versions)) => Ok(UpdateStatus::UpdateAvailable {
                versions: versions.clone(),
            }),
        }
    }

    fn reboot_required(&self) -> Result<RebootStatus, UpdateError> {
        let current = self.store.reboot();
        if !kernel_changed(&self.booted_kernel, &self.current_kernel) {
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
        let status = RebootStatus {
            required: true,
            reason: Some("the activated system has a different kernel than the one running".into()),
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
    use std::sync::Mutex as StdMutex;

    pub(crate) const LOCK: &str = r#"{
  "nodes": {
    "anix": {
      "locked": { "lastModified": 1759000000, "narHash": "sha256-a", "owner": "AnimatedGTVR", "repo": "Abora-OS", "rev": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "type": "github" },
      "original": { "owner": "AnimatedGTVR", "ref": "edge", "repo": "Abora-OS", "type": "github" }
    },
    "nixpkgs": {
      "locked": { "lastModified": 1759100000, "narHash": "sha256-n", "owner": "NixOS", "repo": "nixpkgs", "rev": "1111111111111111111111111111111111111111", "type": "github" },
      "original": { "owner": "NixOS", "ref": "nixos-unstable", "repo": "nixpkgs", "type": "github" }
    },
    "pinned": {
      "locked": { "owner": "x", "repo": "y", "rev": "2222222222222222222222222222222222222222", "type": "github" },
      "original": { "owner": "x", "repo": "y", "rev": "2222222222222222222222222222222222222222", "type": "github" }
    },
    "local": { "locked": { "path": "/srv/x", "type": "path", "narHash": "sha256-l" }, "original": { "path": "/srv/x", "type": "path" } },
    "tools": {
      "locked": { "narHash": "sha256-t", "rev": "3333333333333333333333333333333333333333", "type": "git", "url": "https://example.org/tools.git" },
      "original": { "ref": "main", "type": "git", "url": "https://example.org/tools.git" }
    },
    "root": { "inputs": { "anix": "anix", "nixpkgs": "nixpkgs", "pinned": "pinned", "local": "local", "tools": "tools", "followed": ["anix", "nixpkgs"], "-evil": "nixpkgs" } }
  },
  "root": "root",
  "version": 7
}"#;

    #[test]
    fn tracked_inputs_follow_branches_only() {
        let inputs = tracked_inputs(LOCK).unwrap();
        let names: Vec<_> = inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            ["anix", "nixpkgs", "tools"],
            "pinned, path, follows and bad names are skipped"
        );
        assert_eq!(inputs[1].url, "github:NixOS/nixpkgs/nixos-unstable");
        assert_eq!(inputs[2].url, "git+https://example.org/tools.git?ref=main");
        assert_eq!(inputs[0].locked, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

    #[test]
    fn hostile_or_broken_lock_files_are_refused_or_ignored() {
        assert!(tracked_inputs("not json").is_err());
        assert!(tracked_inputs("{}").is_err());
        let odd = r#"{"nodes":{"root":{"inputs":{"x":"x"}},"x":{"locked":{"rev":"1"},"original":{"type":"git","url":"--upload-pack=evil"}}},"root":"root"}"#;
        assert!(
            tracked_inputs(odd).unwrap().is_empty(),
            "an option-shaped URL is never used"
        );
        let spaced = r#"{"nodes":{"root":{"inputs":{"x":"x"}},"x":{"locked":{"rev":"1"},"original":{"type":"tarball","url":"https://a b"}}},"root":"root"}"#;
        assert!(tracked_inputs(spaced).unwrap().is_empty());
        for original in [
            r#"{"type":"github","owner":"-x","repo":"y"}"#,
            r#"{"type":"github","owner":"x","repo":"y","ref":"--impure"}"#,
            r#"{"type":"github","owner":"x","repo":"y","ref":"../../etc"}"#,
            r#"{"type":"git","url":"file:///etc/passwd"}"#,
            r#"{"type":"git","url":"ext::sh -c touch%20/tmp/x"}"#,
        ] {
            let lock = format!(
                r#"{{"nodes":{{"root":{{"inputs":{{"x":"x"}}}},"x":{{"locked":{{"rev":"1"}},"original":{original}}}}},"root":"root"}}"#
            );
            assert!(
                tracked_inputs(&lock).unwrap().is_empty(),
                "{original} must not be used"
            );
        }
    }

    #[test]
    fn input_names_are_validated() {
        assert!(
            valid_input_name("nixpkgs")
                && valid_input_name("home-manager")
                && valid_input_name("a_b2")
        );
        for bad in ["", "-x", "a b", "a/b", "a;b", &"x".repeat(65)] {
            assert!(!valid_input_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn dates_become_versions() {
        assert_eq!(version_from_time(Some(0)), Version::new(1970, 1, 1));
        assert_eq!(
            version_from_time(Some(1_759_449_600)),
            Version::new(2025, 10, 3)
        );
        assert_eq!(
            version_from_time(Some(951_782_400)),
            Version::new(2000, 2, 29)
        );
        assert_eq!(version_from_time(None), Version::new(0, 0, 0));
    }

    /// Answers `nix flake metadata` from a table of URL -> (rev, lastModified).
    pub(crate) struct FakeNix {
        pub answers: Vec<(&'static str, &'static str, u64)>,
        pub calls: Arc<StdMutex<Vec<Vec<String>>>>,
        pub fail: Option<String>,
    }

    impl CommandRunner for FakeNix {
        fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().map(|a| (*a).to_owned()));
            self.calls.lock().unwrap().push(call);
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            assert_eq!(program, "nix");
            assert!(args.contains(&"metadata") && args.contains(&"--refresh"));
            let url = *args.last().unwrap();
            let (_, rev, modified) = self
                .answers
                .iter()
                .find(|(u, _, _)| *u == url)
                .unwrap_or_else(|| panic!("unexpected url {url}"));
            Ok(format!(
                r#"{{"locked":{{"rev":"{rev}","lastModified":{modified}}}}}"#
            ))
        }
    }

    fn flake_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("abora-nix-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flake.lock"), LOCK).unwrap();
        dir
    }

    fn provider(dir: PathBuf, fake: FakeNix) -> NixProvider {
        NixProvider::with_parts(
            Arc::new(UpdateStore::in_memory()),
            Box::new(fake),
            dir,
            PathBuf::from("/nonexistent/booted"),
            PathBuf::from("/nonexistent/current"),
            || "2026-10-03T00:00:00Z".to_owned(),
        )
    }

    #[test]
    fn check_reports_inputs_that_moved() {
        let dir = flake_dir("moved");
        let fake = FakeNix {
            answers: vec![
                (
                    "github:AnimatedGTVR/Abora-OS/edge",
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    1759000000,
                ),
                (
                    "github:NixOS/nixpkgs/nixos-unstable",
                    "4444444444444444444444444444444444444444",
                    1759449600,
                ),
                (
                    "git+https://example.org/tools.git?ref=main",
                    "3333333333333333333333333333333333333333",
                    1,
                ),
            ],
            calls: Default::default(),
            fail: None,
        };
        let calls = fake.calls.clone();
        let p = provider(dir.clone(), fake);
        assert!(
            matches!(p.status(), Err(UpdateError::Conflict(_))),
            "nothing honest to say before a check"
        );
        let found = p.check(&Channel::Stable).unwrap();
        assert_eq!(found.updates.len(), 1, "only nixpkgs moved");
        let u = &found.updates[0];
        assert_eq!(u.component, "nixpkgs");
        assert_eq!(u.version, Version::new(2025, 10, 3));
        assert_eq!(
            u.summary,
            "nixpkgs: 1111111 -> 4444444 (github:NixOS/nixpkgs/nixos-unstable)"
        );
        assert!(matches!(
            p.status(),
            Ok(UpdateStatus::UpdateAvailable { .. })
        ));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert!(calls
            .iter()
            .all(|c| c[1] == "--extra-experimental-features" && c.contains(&"--json".to_owned())));
        assert!(p.store.last_check().is_some(), "the check time is recorded");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_refresh_is_an_error_not_up_to_date() {
        let dir = flake_dir("offline");
        let p = provider(
            dir.clone(),
            FakeNix {
                answers: vec![],
                calls: Default::default(),
                fail: Some("nix exited with exit status: 1: unable to download".into()),
            },
        );
        assert!(p.check(&Channel::Stable).is_err());
        assert!(matches!(p.status(), Ok(UpdateStatus::Error { .. })));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_lock_file_is_reported() {
        let p = provider(
            PathBuf::from("/nonexistent/flake"),
            FakeNix {
                answers: vec![],
                calls: Default::default(),
                fail: None,
            },
        );
        let err = p.check(&Channel::Stable).unwrap_err();
        assert!(matches!(err, UpdateError::Source(m) if m.contains("flake.lock")));
    }

    #[cfg(unix)]
    #[test]
    fn reboot_is_required_when_the_kernel_changed() {
        let dir = std::env::temp_dir().join(format!("abora-nix-kernel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("k1"), "a").unwrap();
        std::fs::write(dir.join("k2"), "b").unwrap();
        let link = |name: &str, target: &str| {
            let _ = std::fs::remove_file(dir.join(name));
            std::os::unix::fs::symlink(dir.join(target), dir.join(name)).unwrap();
        };
        link("booted", "k1");
        link("current", "k1");
        let p = NixProvider::with_parts(
            Arc::new(UpdateStore::in_memory()),
            Box::new(FakeNix {
                answers: vec![],
                calls: Default::default(),
                fail: None,
            }),
            dir.clone(),
            dir.join("booted"),
            dir.join("current"),
            || "2026-10-03T00:00:00Z".to_owned(),
        );
        assert!(!p.reboot_required().unwrap().required);
        link("current", "k2");
        let status = p.reboot_required().unwrap();
        assert!(status.required && status.reason.unwrap().contains("kernel"));
        link("current", "k1");
        assert!(
            !p.reboot_required().unwrap().required,
            "cleared once they match again"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
