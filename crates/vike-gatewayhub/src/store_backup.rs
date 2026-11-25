//! **The settings database's integrity check at this daemon's start, and its nightly copy** (`vike_secrets`'
//! `check_store_in` and `back_up_store_in`, whose module doc is the contract; the design is
//! `docs/decisions/0054-settings-move-into-one-database.md`'s addendum of 2026-11-23, the owner's "agree" to ruling Q5).
//!
//! This daemon runs them because it is the one long-running process of every server install (a container or the host,
//! decision 0260) that already holds every venue key: the datahub and the tradehub read the store under `NoVenueKeys`,
//! and a copy taken there would pass every key through a process that holds none. It already keeps a once-a-day task
//! (`crate::url_probe`), and a no-new-process ruling leaves no other home.
//!
//! * **At the start, the check**: `PRAGMA quick_check`. A damaged store is an ERROR line naming the newest copy to
//!   restore from. Nothing is refused: a store that reads is still what every reader answers from, and the restore is
//!   the operator's.
//! * **Every [`LOOK_EVERY`], the copy**: one per UTC day, made by the first look of a day that has none (soon after
//!   midnight UTC, or at a start on a day with none), so a restart makes no second one. A damaged store is not copied.
//!
//! Both run on a thread of their own, so neither costs the start a moment; the check's own time is in its line. A copy
//! that fails is a WARN line, and the next look tries again.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vike_model::MS_PER_HOUR_U64;
use vike_secrets::{StoreBackup, StoreDamage, StoreIntegrity};

/// How often the thread looks for the day's copy.
pub(crate) const LOOK_EVERY: Duration = Duration::from_millis(MS_PER_HOUR_U64);

/// Start the thread over `settings_dir`, for the process's life. A thread that cannot be spawned is logged.
pub(crate) fn start(settings_dir: PathBuf) {
    let spawned =
        std::thread::Builder::new().name("settings-backup".to_string()).spawn(move || {
            check(&settings_dir);
            loop {
                back_up(&settings_dir);
                std::thread::sleep(LOOK_EVERY);
            }
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "vike-gatewayhub: the settings database's check and nightly copy did not start");
    }
}

/// The start's check, said once.
fn check(settings_dir: &Path) {
    let started = Instant::now();
    match vike_secrets::check_store_in(settings_dir) {
        Ok(StoreIntegrity::Sound) => tracing::info!(
            took_ms = started.elapsed().as_millis() as u64,
            "vike-gatewayhub: the settings database passed its integrity check"
        ),
        Ok(StoreIntegrity::Damaged(damage)) => say_damaged(&damage),
        Err(e) => {
            tracing::warn!(error = %e, "vike-gatewayhub: the settings database could not be checked")
        }
    }
}

/// One look for the day's copy.
fn back_up(settings_dir: &Path) {
    match vike_secrets::back_up_store_in(settings_dir, vike_model::now_ms()) {
        Ok(StoreBackup::AlreadyMade(_)) => {}
        Ok(StoreBackup::Made { copy, removed }) => tracing::info!(
            removed = removed.len(),
            "vike-gatewayhub: the settings database's nightly copy is {}",
            copy.display()
        ),
        Ok(StoreBackup::Damaged(damage)) => say_damaged(&damage),
        Err(e) => tracing::warn!(
            error = %e,
            "vike-gatewayhub: the settings database's nightly copy could not be made; the next look tries again"
        ),
    }
}

/// The ERROR a damaged store is, naming the copy to restore from.
fn say_damaged(damage: &StoreDamage) {
    let first = damage.findings.first().map_or("", String::as_str);
    match &damage.restore_from {
        Some(copy) => tracing::error!(
            findings = damage.findings.len(),
            "vike-gatewayhub: the settings database is DAMAGED ({first}); no copy is made of it. Stop the server and \
             restore it from {}, the newest nightly copy",
            copy.display()
        ),
        None => tracing::error!(
            findings = damage.findings.len(),
            "vike-gatewayhub: the settings database is DAMAGED ({first}); no copy is made of it, and no nightly copy \
             was found to restore it from"
        ),
    }
}
