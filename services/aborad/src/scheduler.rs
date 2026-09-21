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

use std::time::{Duration, Instant};

use abora_api::ScheduleInfo;
use abora_config::schedule::{
    check_interval, check_is_due, local_time, permissions, should_auto_apply, Permissions,
};
use abora_log::{info, warn};

use crate::state::SharedState;

/// How often the policy is re-evaluated and a due check is looked for.
const TICK: Duration = Duration::from_secs(30);

/// Runs for the life of the daemon. The first pass checks for updates immediately.
pub async fn run(state: SharedState) {
    let mut last_attempt: Option<Instant> = None;
    let mut last_ok = true;
    let mut previous: Option<Permissions> = None;
    let mut last_auto_attempt: Option<Instant> = None;

    loop {
        // Copy what is needed out of the config so no lock is held across an await.
        let (interval, channel, config_snapshot) = {
            let config = state.config.read().expect("config lock poisoned");
            (
                check_interval(&config),
                config.updates.channel.clone(),
                config.clone(),
            )
        };

        let now = tokio::task::spawn_blocking(local_time).await.ok().flatten();
        if now.is_none() {
            warn!(
                state.logger,
                "could not read the local time; maintenance windows are treated as closed"
            );
        }
        let perms = permissions(&config_snapshot, now);
        state.updates.set_schedule(ScheduleInfo {
            check_interval_seconds: interval.as_secs(),
            in_maintenance_window: perms.in_maintenance_window,
            apply_permitted: perms.apply_permitted,
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

        // If the helper has finished an apply, record it and look at what is left.
        let ingest_channel = channel.clone();
        let ingest_state = state.clone();
        let ingested = tokio::task::spawn_blocking(move || {
            ingest_state.updates.ingest_apply_result(&ingest_channel)
        })
        .await
        .unwrap_or(false);
        if ingested {
            info!(
                state.logger,
                "an update apply finished; recorded in the history"
            );
            last_attempt = None;
        }

        let due = match last_attempt {
            None => true,
            Some(t) => check_is_due(t.elapsed(), last_ok, interval),
        };
        if due {
            last_attempt = Some(Instant::now());
            let worker = state.clone();
            let result =
                tokio::task::spawn_blocking(move || worker.updates.refresh(&channel)).await;
            match result {
                Ok(Ok(n)) => {
                    last_ok = true;
                    info!(
                        state.logger,
                        "update check finished: {n} update(s) available"
                    );
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

        // Opt-in automatic apply: `[updates] automatic = true`, inside a maintenance window. The request
        // goes through exactly the same checks and the same root helper as a manual one.
        let (has_available, pending) = state.updates.apply_inputs();
        if should_auto_apply(
            &perms,
            has_available,
            pending,
            last_auto_attempt.map(|t| t.elapsed()),
            interval,
        ) {
            last_auto_attempt = Some(Instant::now());
            let worker = state.clone();
            let cfg = config_snapshot.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                worker
                    .updates
                    .request_apply(&cfg, "automatic (scheduler)", false)
            })
            .await;
            match outcome {
                Ok(Ok(r)) => info!(
                    state.logger,
                    "audit: automatic apply requested for {} package(s) (request {})",
                    r.packages.len(),
                    r.id.as_deref().unwrap_or("-")
                ),
                Ok(Err(e)) => warn!(state.logger, "audit: automatic apply not requested: {e:?}"),
                Err(e) => warn!(state.logger, "automatic apply task failed: {e}"),
            }
        }

        tokio::time::sleep(TICK).await;
    }
}
