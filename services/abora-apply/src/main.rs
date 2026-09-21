//! `abora-apply`: the small root helper that installs host package updates.
//!
//! `aborad` is unprivileged and sandboxed, so it cannot install packages. It writes a request file
//! and systemd starts this helper (`abora-apply.path` watches that file). The helper **trusts
//! nothing in the request**: it checks the file itself (a regular file, small, recent), validates
//! every package name, re-evaluates the maintenance policy from the config file, re-runs the apt
//! simulation to see what is really upgradable, and only then runs a fixed `apt-get` command line
//! for the intersection. The request cannot carry a command, a path or an option.
//!
//! The outcome is written to a root-owned result file that `aborad` reads and turns into a history
//! entry. A reboot is started only if the reboot policy allows it, and only after the result is on
//! disk. See `docs/apply-design.md`.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use abora_config::schedule::{local_time, permissions, LocalTime};
use abora_config::Config;
use abora_update::apply::{
    install_args, read_regular_file, validate_request, write_atomic, ApplyRequest, ApplyResult,
    MAX_FILE_BYTES, MAX_REQUEST_AGE, REQUEST_FILE_NAME,
};
use abora_update::host::{parse_simulation, tail, CommandRunner, SystemRunner};

/// How long one `apt-get install` may take.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_CONFIG: &str = "/etc/abora/abora.toml";
const REBOOT_MARKER: &str = "/var/run/reboot-required";

/// Everything the helper depends on, so tests can supply fakes.
struct Ctx {
    request_path: PathBuf,
    config: Config,
    runner: Box<dyn CommandRunner>,
    now: fn() -> String,
    local_time: fn() -> Option<LocalTime>,
    reboot_marker: PathBuf,
}

/// What to do after the result is written.
struct Outcome {
    result: ApplyResult,
    reboot: bool,
}

fn refuse(id: &str, started: String, now: &str, note: impl Into<String>) -> Outcome {
    Outcome {
        result: ApplyResult {
            id: id.to_owned(),
            started_at: started,
            finished_at: now.to_owned(),
            succeeded: false,
            packages: Vec::new(),
            note: Some(note.into()),
            reboot_started: false,
        },
        reboot: false,
    }
}

/// Handle whatever request is waiting. `None` means there is no request (not an error: systemd also
/// starts the helper when the request file is deleted).
fn process(ctx: &Ctx) -> Option<Outcome> {
    if fs::symlink_metadata(&ctx.request_path).is_err() {
        return None;
    }
    let started = (ctx.now)();
    let fail = |id: &str, note: String| refuse(id, started.clone(), &(ctx.now)(), note);

    let bytes = match read_regular_file(&ctx.request_path, Some(MAX_REQUEST_AGE)) {
        Ok(b) => b,
        Err(e) => return Some(fail("invalid", format!("request refused: {e}"))),
    };
    let request: ApplyRequest = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(_) => return Some(fail("invalid", "request refused: not valid JSON".into())),
    };
    if let Err(e) = validate_request(&request) {
        // An id that failed validation is not echoed back.
        let id = if abora_update::apply::valid_request_id(&request.id) {
            request.id.as_str()
        } else {
            "invalid"
        };
        return Some(fail(id, format!("request refused: {e}")));
    }
    let id = request.id.as_str();

    // Policy is re-evaluated here from the config file: aborad's opinion is not trusted.
    let perms = permissions(&ctx.config, (ctx.local_time)());
    if !perms.apply_permitted {
        return Some(fail(
            id,
            "refused: updates may only be applied inside a maintenance window ([maintenance] enabled, and the \
             local time inside a window)"
                .into(),
        ));
    }

    // What is really upgradable right now.
    let simulation = match ctx
        .runner
        .run("apt-get", &["-s", "-o", "Debug::NoLocking=1", "upgrade"])
    {
        Ok(out) => out,
        Err(e) => return Some(fail(id, format!("could not check what is upgradable: {e}"))),
    };
    let upgradable: HashSet<String> = parse_simulation(&simulation)
        .into_iter()
        .map(|u| u.name)
        .collect();
    let (todo, gone): (Vec<String>, Vec<String>) = request
        .packages
        .iter()
        .cloned()
        .partition(|p| upgradable.contains(p));

    if todo.is_empty() {
        let mut outcome = fail(id, String::new());
        outcome.result.succeeded = true;
        outcome.result.note =
            Some("nothing to do: none of the requested packages is upgradable any more".into());
        return Some(outcome);
    }

    let args = install_args(&todo);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = ctx.runner.run("apt-get", &arg_refs) {
        return Some(fail(id, format!("apt-get failed: {e}")));
    }

    let reboot = ctx.reboot_marker.exists() && perms.reboot_permitted;
    let mut note = if gone.is_empty() {
        None
    } else {
        Some(format!(
            "{} requested package(s) were no longer upgradable and were skipped",
            gone.len()
        ))
    };
    if ctx.reboot_marker.exists() && !reboot {
        let extra = "a reboot is needed but policy does not allow one now";
        note = Some(note.map_or_else(|| extra.to_owned(), |n| format!("{n}; {extra}")));
    }
    Some(Outcome {
        result: ApplyResult {
            id: id.to_owned(),
            started_at: started,
            finished_at: (ctx.now)(),
            succeeded: true,
            packages: todo,
            note,
            reboot_started: reboot,
        },
        reboot,
    })
}

struct Args {
    config: PathBuf,
    /// Overrides `[updates] apply_result_file`.
    result_file: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        config: DEFAULT_CONFIG.into(),
        result_file: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--config" => args.config = it.next().ok_or("--config needs a path")?.into(),
            "--result-file" => {
                args.result_file = Some(it.next().ok_or("--result-file needs a path")?.into())
            }
            "--help" | "-h" => {
                println!("usage: abora-apply [--config PATH] [--result-file PATH]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(args)
}

fn load_config(path: &PathBuf) -> Result<Config, String> {
    let meta = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{} is unreasonably large", path.display()));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Config::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let config = load_config(&args.config)?;
    let result_file = args
        .result_file
        .clone()
        .unwrap_or_else(|| config.updates.apply_result_file.clone());
    let request_path = config
        .updates
        .state_file
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(REQUEST_FILE_NAME);
    let ctx = Ctx {
        request_path,
        config,
        runner: Box::new(SystemRunner::new(INSTALL_TIMEOUT)),
        now: abora_log::rfc3339_now,
        local_time,
        reboot_marker: PathBuf::from(REBOOT_MARKER),
    };

    let Some(outcome) = process(&ctx) else {
        eprintln!("abora-apply: no request waiting");
        return Ok(());
    };
    let r = &outcome.result;
    eprintln!(
        "abora-apply: request {} {}: {} package(s){}",
        r.id,
        if r.succeeded {
            "succeeded"
        } else {
            "did not run"
        },
        r.packages.len(),
        r.note
            .as_deref()
            .map(|n| format!(" ({})", tail(n, 300)))
            .unwrap_or_default()
    );

    // Result first (aborad turns it into a history entry), then remove the request, then reboot.
    let json = serde_json::to_vec_pretty(r).expect("result serializes");
    write_atomic(&result_file, &json, 0o644)
        .map_err(|e| format!("could not write {}: {e}", result_file.display()))?;
    let _ = fs::remove_file(&ctx.request_path);
    if outcome.reboot {
        eprintln!("abora-apply: policy allows a reboot; rebooting");
        ctx.runner
            .run("systemctl", &["reboot"])
            .map_err(|e| format!("reboot failed: {e}"))?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("abora-apply: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use abora_update::apply::write_atomic as write;
    use abora_update::{MaintenanceWindow, RebootPolicy, Weekday};
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    const SIM: &str = "\
Inst openssl [3.0.13-0ubuntu3.4] (3.0.13-0ubuntu3.5 Ubuntu:24.04/noble-updates [amd64])
Inst libc6 [2.39-0ubuntu8.7] (2.39-0ubuntu8.9 Ubuntu:24.04/noble-security [amd64])
";

    /// Records every command and answers from a script.
    #[derive(Clone, Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        install_error: Option<String>,
    }

    impl CommandRunner for Fake {
        fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().map(|s| (*s).to_owned()));
            self.calls.lock().unwrap().push(call);
            if args.contains(&"-s") {
                return Ok(SIM.into());
            }
            if args.contains(&"install") {
                if let Some(e) = &self.install_error {
                    return Err(e.clone());
                }
            }
            Ok(String::new())
        }
    }

    fn sunday_3am() -> Option<LocalTime> {
        Some(LocalTime {
            weekday: Weekday::Sunday,
            minutes: 3 * 60,
        })
    }
    fn sunday_noon() -> Option<LocalTime> {
        Some(LocalTime {
            weekday: Weekday::Sunday,
            minutes: 12 * 60,
        })
    }

    fn config(policy: RebootPolicy) -> Config {
        let mut c = Config::default();
        c.updates.reboot_policy = policy;
        c.maintenance.enabled = true;
        c.maintenance.windows = vec![MaintenanceWindow {
            days: vec![Weekday::Sunday],
            start: "02:00".into(),
            end: "04:00".into(),
            max_duration_minutes: None,
        }];
        c
    }

    struct Env {
        dir: PathBuf,
        ctx: Ctx,
        fake: Fake,
    }

    fn env(name: &str, local: fn() -> Option<LocalTime>, policy: RebootPolicy, fake: Fake) -> Env {
        let dir =
            std::env::temp_dir().join(format!("abora-apply-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let ctx = Ctx {
            request_path: dir.join(REQUEST_FILE_NAME),
            config: config(policy),
            runner: Box::new(fake.clone()),
            now: || "2026-09-21T03:00:00Z".to_owned(),
            local_time: local,
            reboot_marker: dir.join("reboot-required"),
        };
        Env { dir, ctx, fake }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn put_request(env: &Env, packages: &[&str]) {
        let req = ApplyRequest {
            id: "req-0123abcd".into(),
            requested_at: "2026-09-21T02:59:00Z".into(),
            requested_by: "ops".into(),
            packages: packages.iter().map(|s| (*s).to_owned()).collect(),
        };
        write(
            &env.ctx.request_path,
            &serde_json::to_vec(&req).unwrap(),
            0o600,
        )
        .unwrap();
    }

    fn installs(env: &Env) -> Vec<Vec<String>> {
        env.fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.iter().any(|a| a == "install"))
            .cloned()
            .collect()
    }

    #[test]
    fn no_request_means_nothing_to_do() {
        let e = env("none", sunday_3am, RebootPolicy::Ask, Fake::default());
        assert!(process(&e.ctx).is_none());
        assert!(
            e.fake.calls.lock().unwrap().is_empty(),
            "no command may run"
        );
    }

    #[test]
    fn a_valid_request_in_the_window_upgrades_exactly_the_listed_packages() {
        let e = env("happy", sunday_3am, RebootPolicy::Ask, Fake::default());
        put_request(&e, &["openssl", "libc6"]);
        let out = process(&e.ctx).unwrap();
        assert!(out.result.succeeded, "{:?}", out.result);
        assert_eq!(out.result.packages, ["openssl", "libc6"]);
        assert!(!out.reboot);
        let installs = installs(&e);
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0][0], "apt-get");
        assert_eq!(&installs[0][installs[0].len() - 2..], ["openssl", "libc6"]);
        assert!(
            installs[0].contains(&"--only-upgrade".to_owned())
                && installs[0].contains(&"--no-remove".to_owned())
        );
    }

    #[test]
    fn outside_the_window_nothing_is_installed() {
        let e = env("outside", sunday_noon, RebootPolicy::Ask, Fake::default());
        put_request(&e, &["openssl"]);
        let out = process(&e.ctx).unwrap();
        assert!(!out.result.succeeded);
        assert!(out.result.note.unwrap().contains("maintenance window"));
        assert!(
            e.fake.calls.lock().unwrap().is_empty(),
            "not even the simulation runs"
        );
    }

    #[test]
    fn unknown_local_time_installs_nothing() {
        let e = env("notime", || None, RebootPolicy::Always, Fake::default());
        put_request(&e, &["openssl"]);
        assert!(!process(&e.ctx).unwrap().result.succeeded);
        assert!(installs(&e).is_empty());
    }

    #[test]
    fn a_hostile_request_is_refused_before_any_command_runs() {
        for bad in [
            &["openssl", "--yes"][..],
            &["$(reboot)"],
            &["../../etc/passwd"],
            &["a b"],
        ] {
            let e = env("hostile", sunday_3am, RebootPolicy::Ask, Fake::default());
            put_request(&e, bad);
            let out = process(&e.ctx).unwrap();
            assert!(!out.result.succeeded, "{bad:?}");
            assert!(
                e.fake.calls.lock().unwrap().is_empty(),
                "{bad:?}: no command may run"
            );
            assert!(
                !out.result.note.unwrap().contains("reboot"),
                "untrusted text is not echoed"
            );
        }
    }

    #[test]
    fn garbage_json_is_refused() {
        let e = env("garbage", sunday_3am, RebootPolicy::Ask, Fake::default());
        write(&e.ctx.request_path, b"{ not json", 0o600).unwrap();
        let out = process(&e.ctx).unwrap();
        assert!(!out.result.succeeded);
        assert_eq!(out.result.id, "invalid");
        assert!(e.fake.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_stale_request_is_not_replayed() {
        let e = env("stale", sunday_3am, RebootPolicy::Ask, Fake::default());
        put_request(&e, &["openssl"]);
        let old = SystemTime::now() - Duration::from_secs(30 * 60);
        fs::OpenOptions::new()
            .write(true)
            .open(&e.ctx.request_path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let out = process(&e.ctx).unwrap();
        assert!(!out.result.succeeded);
        assert!(out.result.note.unwrap().contains("older than"));
        assert!(e.fake.calls.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_request_is_refused() {
        let e = env("symlink", sunday_3am, RebootPolicy::Ask, Fake::default());
        let target = e.dir.join("elsewhere.json");
        fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, &e.ctx.request_path).unwrap();
        let out = process(&e.ctx).unwrap();
        assert!(!out.result.succeeded);
        assert!(out.result.note.unwrap().contains("not a regular file"));
    }

    #[test]
    fn packages_that_are_no_longer_upgradable_are_skipped() {
        let e = env("skip", sunday_3am, RebootPolicy::Ask, Fake::default());
        put_request(&e, &["openssl", "vim-tiny"]);
        let out = process(&e.ctx).unwrap();
        assert!(out.result.succeeded);
        assert_eq!(out.result.packages, ["openssl"]);
        assert!(out.result.note.unwrap().contains("skipped"));
        assert!(!installs(&e)[0].contains(&"vim-tiny".to_owned()));
    }

    #[test]
    fn if_none_are_upgradable_it_succeeds_without_installing() {
        let e = env("none-left", sunday_3am, RebootPolicy::Ask, Fake::default());
        put_request(&e, &["vim-tiny"]);
        let out = process(&e.ctx).unwrap();
        assert!(out.result.succeeded && out.result.packages.is_empty());
        assert!(out.result.note.unwrap().contains("nothing to do"));
        assert!(installs(&e).is_empty());
    }

    #[test]
    fn an_install_failure_is_reported_and_never_reboots() {
        let fake = Fake {
            install_error: Some(
                "apt-get exited with exit status: 100: E: dpkg was interrupted".into(),
            ),
            ..Fake::default()
        };
        let e = env("fail", sunday_3am, RebootPolicy::Always, fake);
        fs::write(&e.ctx.reboot_marker, b"").unwrap();
        put_request(&e, &["openssl"]);
        let out = process(&e.ctx).unwrap();
        assert!(!out.result.succeeded && !out.reboot);
        assert!(out.result.note.unwrap().contains("dpkg was interrupted"));
    }

    #[test]
    fn reboot_follows_policy_and_only_when_one_is_needed() {
        // Needed + allowed (ask, inside window).
        let e = env("reboot-yes", sunday_3am, RebootPolicy::Ask, Fake::default());
        fs::write(&e.ctx.reboot_marker, b"").unwrap();
        put_request(&e, &["openssl"]);
        let out = process(&e.ctx).unwrap();
        assert!(out.reboot && out.result.reboot_started);

        // Needed but policy says never.
        let e = env(
            "reboot-never",
            sunday_3am,
            RebootPolicy::Never,
            Fake::default(),
        );
        fs::write(&e.ctx.reboot_marker, b"").unwrap();
        put_request(&e, &["openssl"]);
        let out = process(&e.ctx).unwrap();
        assert!(!out.reboot && out.result.note.unwrap().contains("policy does not allow"));

        // Allowed but not needed.
        let e = env(
            "reboot-unneeded",
            sunday_3am,
            RebootPolicy::Always,
            Fake::default(),
        );
        put_request(&e, &["openssl"]);
        assert!(!process(&e.ctx).unwrap().reboot);
    }
}
