//! What the update policy allows *right now*: pure functions of the configuration and the
//! local time, so they can be tested without a clock.
//!
//! Rules (see `docs/update.md`):
//!
//! * **Checking** for updates is read-only and is never gated by windows; it just runs every
//!   `[updates] check_interval`.
//! * **Installing** is permitted only when `[updates] automatic` is on, `[maintenance] enabled`
//!   is on, and the local time is inside one of the windows. No window means never.
//! * **Rebooting** follows `[updates] reboot_policy`: `always` may reboot at any time, `never`
//!   never does, and `ask` (the default) only inside a maintenance window.
//! * If the local time is unknown, nothing is permitted. The safe answer is "no".
//!
//! Windows are half-open (`start <= now < end`) and never cross midnight (validation requires
//! `end > start`).

use std::time::Duration;

use abora_update::{MaintenanceWindow, RebootPolicy, Weekday};

use crate::{parse_clock, Config};

/// Local wall-clock time as far as windows care: the day and the minute of the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub weekday: Weekday,
    /// Minutes since local midnight, `0..1440`.
    pub minutes: u32,
}

/// What policy permits at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// `None` when the local time is unknown.
    pub in_maintenance_window: Option<bool>,
    pub installs_permitted: bool,
    pub reboot_permitted: bool,
}

/// Whether `now` falls inside any window. Windows that do not parse are ignored (validation
/// rejects them at load time, so this only matters for a config that skipped validation).
pub fn in_window(windows: &[MaintenanceWindow], now: LocalTime) -> bool {
    windows.iter().any(|w| {
        let (Ok(start), Ok(end)) = (parse_clock(&w.start), parse_clock(&w.end)) else {
            return false;
        };
        w.days.contains(&now.weekday) && start <= now.minutes && now.minutes < end
    })
}

/// Evaluate the policy for `now` (`None` = local time unknown).
pub fn permissions(config: &Config, now: Option<LocalTime>) -> Permissions {
    let in_maintenance_window = now.map(|t| config.maintenance.enabled && in_window(&config.maintenance.windows, t));
    let in_win = in_maintenance_window == Some(true);
    Permissions {
        in_maintenance_window: now.map(|t| in_window(&config.maintenance.windows, t)),
        installs_permitted: config.updates.automatic && in_win,
        reboot_permitted: match config.updates.reboot_policy {
            RebootPolicy::Always => true,
            RebootPolicy::Never => false,
            RebootPolicy::Ask => in_win,
        },
    }
}

/// `[updates] check_interval` as a [`Duration`]. Falls back to the 6 hour default for a value
/// that does not parse (validation rejects those at load time).
pub fn check_interval(config: &Config) -> Duration {
    Duration::from_secs(crate::parse_duration(&config.updates.check_interval).unwrap_or(6 * 60 * 60))
}

/// How soon to try again after a failed check, at most.
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(15 * 60);

/// Whether the next check is due, `since_last_attempt` after the previous one.
/// After a failure the wait is shortened to [`RETRY_AFTER_FAILURE`] (if that is shorter than
/// the interval), so one bad moment does not silence checks for hours.
pub fn check_is_due(since_last_attempt: Duration, last_succeeded: bool, interval: Duration) -> bool {
    let wait = if last_succeeded { interval } else { interval.min(RETRY_AFTER_FAILURE) };
    since_last_attempt >= wait
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(days: &[Weekday], start: &str, end: &str) -> MaintenanceWindow {
        MaintenanceWindow { days: days.to_vec(), start: start.into(), end: end.into(), max_duration_minutes: None }
    }

    fn at(weekday: Weekday, h: u32, m: u32) -> LocalTime {
        LocalTime { weekday, minutes: h * 60 + m }
    }

    fn config(automatic: bool, enabled: bool, policy: RebootPolicy, windows: Vec<MaintenanceWindow>) -> Config {
        let mut c = Config::default();
        c.updates.automatic = automatic;
        c.updates.reboot_policy = policy;
        c.maintenance.enabled = enabled;
        c.maintenance.windows = windows;
        c
    }

    #[test]
    fn windows_are_half_open_and_day_specific() {
        let w = [window(&[Weekday::Sunday], "02:00", "04:00")];
        assert!(!in_window(&w, at(Weekday::Sunday, 1, 59)));
        assert!(in_window(&w, at(Weekday::Sunday, 2, 0)), "start is inclusive");
        assert!(in_window(&w, at(Weekday::Sunday, 3, 59)));
        assert!(!in_window(&w, at(Weekday::Sunday, 4, 0)), "end is exclusive");
        assert!(!in_window(&w, at(Weekday::Monday, 3, 0)), "wrong day");
    }

    #[test]
    fn any_of_several_windows_counts() {
        let w = [window(&[Weekday::Saturday], "01:00", "02:00"), window(&[Weekday::Wednesday, Weekday::Thursday], "23:00", "23:59")];
        assert!(in_window(&w, at(Weekday::Saturday, 1, 30)));
        assert!(in_window(&w, at(Weekday::Thursday, 23, 30)));
        assert!(!in_window(&w, at(Weekday::Friday, 23, 30)));
        assert!(!in_window(&[], at(Weekday::Sunday, 3, 0)), "no windows means never");
    }

    #[test]
    fn installs_need_automatic_enabled_and_a_window() {
        let w = vec![window(&[Weekday::Sunday], "02:00", "04:00")];
        let inside = Some(at(Weekday::Sunday, 3, 0));
        let outside = Some(at(Weekday::Sunday, 12, 0));

        assert!(permissions(&config(true, true, RebootPolicy::Ask, w.clone()), inside).installs_permitted);
        assert!(!permissions(&config(true, true, RebootPolicy::Ask, w.clone()), outside).installs_permitted);
        assert!(!permissions(&config(false, true, RebootPolicy::Ask, w.clone()), inside).installs_permitted, "automatic off");
        assert!(!permissions(&config(true, false, RebootPolicy::Ask, w), inside).installs_permitted, "maintenance off");
        assert!(!permissions(&config(true, true, RebootPolicy::Ask, vec![]), inside).installs_permitted, "no windows");
    }

    #[test]
    fn reboot_follows_the_policy() {
        let w = vec![window(&[Weekday::Sunday], "02:00", "04:00")];
        let inside = Some(at(Weekday::Sunday, 3, 0));
        let outside = Some(at(Weekday::Tuesday, 15, 0));

        let ask = config(true, true, RebootPolicy::Ask, w.clone());
        assert!(permissions(&ask, inside).reboot_permitted);
        assert!(!permissions(&ask, outside).reboot_permitted, "ask never reboots outside a window");

        let always = config(true, true, RebootPolicy::Always, w.clone());
        assert!(permissions(&always, outside).reboot_permitted);

        let never = config(true, true, RebootPolicy::Never, w);
        assert!(!permissions(&never, inside).reboot_permitted);
    }

    #[test]
    fn unknown_time_permits_nothing_except_an_always_reboot_policy() {
        let w = vec![window(&[Weekday::Sunday], "00:00", "23:59")];
        let p = permissions(&config(true, true, RebootPolicy::Ask, w.clone()), None);
        assert_eq!(p, Permissions { in_maintenance_window: None, installs_permitted: false, reboot_permitted: false });
        // `always` does not depend on the clock at all.
        assert!(permissions(&config(true, true, RebootPolicy::Always, w), None).reboot_permitted);
    }

    #[test]
    fn disabled_maintenance_never_counts_as_inside_for_policy() {
        let w = vec![window(&[Weekday::Sunday], "02:00", "04:00")];
        let p = permissions(&config(true, false, RebootPolicy::Ask, w), Some(at(Weekday::Sunday, 3, 0)));
        assert!(!p.reboot_permitted && !p.installs_permitted);
    }

    #[test]
    fn checks_are_due_after_the_interval_and_retried_sooner_after_a_failure() {
        let six_hours = Duration::from_secs(6 * 3600);
        assert!(!check_is_due(Duration::from_secs(3600), true, six_hours));
        assert!(check_is_due(six_hours, true, six_hours));
        assert!(!check_is_due(Duration::from_secs(600), false, six_hours));
        assert!(check_is_due(RETRY_AFTER_FAILURE, false, six_hours), "retry after 15 minutes, not 6 hours");
        // A short interval is never stretched by the retry rule.
        assert!(check_is_due(Duration::from_secs(60), false, Duration::from_secs(60)));
    }

    #[test]
    fn interval_comes_from_the_config() {
        let mut c = Config::default();
        assert_eq!(check_interval(&c), Duration::from_secs(6 * 3600));
        c.updates.check_interval = "30m".into();
        assert_eq!(check_interval(&c), Duration::from_secs(1800));
    }
}
