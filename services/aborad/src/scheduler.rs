//! The update scheduler: a background task that checks for updates every
//! `[updates] check_interval` and keeps `GET /api/v1/updates` informed about what the
//! maintenance policy currently permits.
//!
//! The rules themselves live in `abora_config::schedule` (pure and unit tested). This module
//! only supplies the clock and does the waiting.
//!
//! Checks are read-only and run whatever the time. Installing and rebooting are not
//! implemented yet, so the policy is *reported* (and logged when it changes), never acted on.
//! The configuration is re-read on every tick, so a reload takes effect within seconds.

use std::process::Command;
use std::time::{Duration, Instant};

use abora_api::ScheduleInfo;
use abora_config::schedule::{check_interval, check_is_due, permissions, LocalTime, Permissions};
use abora_log::{info, warn};
use abora_update::Weekday;

use crate::state::SharedState;

/// How often the policy is re-evaluated and a due check is looked for.
const TICK: Duration = Duration::from_secs(30);

/// The server's local time, from `date` (which honours `TZ` and `/etc/localtime`, and the
/// standard library has no way to ask). `None` if it cannot be determined.
pub fn local_time() -> Option<LocalTime> {
    let out = Command::new("date").arg("+%u %H:%M").env("LC_ALL", "C").output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_date_output(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `date +"%u %H:%M"` output such as `7 03:15` (`%u` is 1 = Monday .. 7 = Sunday).
fn parse_date_output(text: &str) -> Option<LocalTime> {
    let (day, clock) = text.trim().split_once(' ')?;
    let (hour, minute) = clock.split_once(':')?;
    let weekday = match day {
        "1" => Weekday::Monday,
        "2" => Weekday::Tuesday,
        "3" => Weekday::Wednesday,
        "4" => Weekday::Thursday,
        "5" => Weekday::Friday,
        "6" => Weekday::Saturday,
        "7" => Weekday::Sunday,
        _ => return None,
    };
    let (hour, minute): (u32, u32) = (hour.parse().ok()?, minute.parse().ok()?);
    (hour < 24 && minute < 60).then_some(LocalTime { weekday, minutes: hour * 60 + minute })
}

/// Runs for the life of the daemon. The first pass checks for updates immediately.
pub async fn run(state: SharedState) {
    let mut last_attempt: Option<Instant> = None;
    let mut last_ok = true;
    let mut previous: Option<Permissions> = None;

    loop {
        // Copy what is needed out of the config so no lock is held across an await.
        let (interval, channel, config_snapshot) = {
            let config = state.config.read().expect("config lock poisoned");
            (check_interval(&config), config.updates.channel.clone(), config.clone())
        };

        let now = tokio::task::spawn_blocking(local_time).await.ok().flatten();
        if now.is_none() {
            warn!(state.logger, "could not read the local time; maintenance windows are treated as closed");
        }
        let perms = permissions(&config_snapshot, now);
        state.updates.set_schedule(ScheduleInfo {
            check_interval_seconds: interval.as_secs(),
            in_maintenance_window: perms.in_maintenance_window,
            installs_permitted: perms.installs_permitted,
            reboot_permitted: perms.reboot_permitted,
        });
        if previous.is_some() && previous != Some(perms) {
            info!(
                state.logger,
                "update policy changed: maintenance window {}, installs {}, reboot {} (installing is not implemented, so nothing is done)",
                match perms.in_maintenance_window { Some(true) => "open", Some(false) => "closed", None => "unknown" },
                if perms.installs_permitted { "permitted" } else { "not permitted" },
                if perms.reboot_permitted { "permitted" } else { "not permitted" },
            );
        }
        previous = Some(perms);

        let due = match last_attempt {
            None => true,
            Some(t) => check_is_due(t.elapsed(), last_ok, interval),
        };
        if due {
            last_attempt = Some(Instant::now());
            let worker = state.clone();
            let result = tokio::task::spawn_blocking(move || worker.updates.refresh(&channel)).await;
            match result {
                Ok(Ok(n)) => {
                    last_ok = true;
                    info!(state.logger, "update check finished: {n} update(s) available");
                }
                Ok(Err(e)) => {
                    last_ok = false;
                    warn!(state.logger, "update check failed: {e}");
                }
                Err(e) => {
                    last_ok = false;
                    warn!(state.logger, "update check task failed: {e}");
                }
            }
        }

        tokio::time::sleep(TICK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_output_is_parsed() {
        assert_eq!(parse_date_output("7 03:15\n"), Some(LocalTime { weekday: Weekday::Sunday, minutes: 3 * 60 + 15 }));
        assert_eq!(parse_date_output("1 00:00"), Some(LocalTime { weekday: Weekday::Monday, minutes: 0 }));
        assert_eq!(parse_date_output("5 23:59"), Some(LocalTime { weekday: Weekday::Friday, minutes: 23 * 60 + 59 }));
    }

    #[test]
    fn garbage_date_output_is_rejected() {
        for bad in ["", "0 01:00", "8 01:00", "3 25:00", "3 12:60", "Tue 10:00", "3", "3 10-30"] {
            assert_eq!(parse_date_output(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_real_date_command_gives_a_time() {
        // `date` exists on every Unix this daemon targets.
        if Command::new("date").arg("--version").output().is_ok() {
            assert!(local_time().is_some());
        }
    }
}
