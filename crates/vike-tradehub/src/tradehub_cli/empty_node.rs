//! **A settings database with no ACTIVE daemon-profile row starts an EMPTY node** (the owner, 2026-11-16: an end user
//! never runs a setup step, so a brand-new server has its hubs up and its door open). Such a box used to PARK here
//! (exit 78, decision 0260), so a fresh server had no door for a device to register through.
//!
//! The empty node is a core with no engine (`vike_core::spawn_core_without_accounts`, decision 0283's placeholder),
//! the node server on `config.tradehub_addr` serving every read and the invitation's registration (decision 0288),
//! and the ready line led by [`NO_DAEMON_PROFILE`]. It mounts nothing, runs no account, and neither writes nor
//! restores a journal. What needs a profile or an account says so where it is asked: the identity block is named
//! [`NO_DAEMON_PROFILE`] (the GUI's daemon label, `vike-cli trade health`'s `build` row), the frame runs no account
//! (`nothing running`), the core refuses an order or a mount, and `vike-cli trade health` says why nothing runs.
//!
//! It watches the store: once an ACTIVE daemon-profile row is there it stops gracefully and exits
//! [`vike_utility::cli::PLANNED_RESTART_EXIT`] (75), the planned restart of the unit (`RestartForceExitStatus=`) and of the
//! server install's supervisor, and the next start runs that profile. A box with NO settings database is not this
//! case: it still refuses (78) naming `vike-cli secrets init`, because a node with no store has no access list to
//! admit anyone by.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_boot::shutdown::{ShutdownOutcome, run_with_deadline};
use vike_boot::stop::StopSignal;
use vike_secrets::profile_store::ProfileKind;

use super::args::Args;
use super::banner::{NO_DAEMON_PROFILE, node_identity, ready_banner};
use super::lifecycle::report_teardown;
use super::paths::strategy_state_dir;
use super::settings::live_switch;
use crate::hub_server;
use crate::server;

/// How often the empty node asks the store for an ACTIVE daemon-profile row.
const PROFILE_POLL: Duration = Duration::from_secs(2);

/// The settings database this boot resolved, when one EXISTS: the empty node's precondition.
pub(super) fn store_of(booted: &vike_boot::Booted) -> Option<PathBuf> {
    let db = vike_secrets::db_path_in(booted.settings_dir.as_deref()?);
    // A store the filesystem will not stat is not one to serve empty from: the start parks on its refusal.
    matches!(vike_secrets::database_present(&db), Ok(true)).then_some(db)
}

/// The name of the ACTIVE daemon-profile row in `db`, if one is there. A store that will not read answers `None`
/// (the next poll asks again).
fn active_daemon(db: &Path) -> Option<String> {
    let profiles = vike_secrets::profile_store::read_profiles(db).ok()?;
    profiles.active(ProfileKind::Daemon).map(|p| p.row.name.clone())
}

/// **Run the empty node until a stop or an active daemon profile**, then tear it down. `db` is [`store_of`]'s
/// answer; `stop` is the process's one stop flag, its handlers already installed.
pub(super) fn serve(
    booted: &vike_boot::Booted,
    args: &Args,
    stop: &StopSignal,
    db: PathBuf,
) -> ExitCode {
    let settings = &booted.settings;
    let live = live_switch(settings);
    tracing::warn!(
        "{NO_DAEMON_PROFILE}: the settings database {} holds no ACTIVE daemon profile, so this node starts EMPTY: no \
         strategy and no account run, the node server answers reads and device registration. Store a daemon profile \
         and this node restarts into it by itself (it asks the store every {} s)",
        db.display(),
        PROFILE_POLL.as_secs()
    );
    // The same door, keys, control gate and account barrier a configured start opens, from the same rows.
    let door = hub_server::open_observe_door(
        settings.config.tradehub_addr.as_deref(),
        settings.flags.tradehub_control,
        settings.flags.tradehub_allow_public_bind || args.allow_public_bind,
        settings.config.tradehub_account_admin.as_deref(),
        booted.settings_dir.as_deref(),
        settings.config.datahub_advertise_addr.as_deref(),
        super::observer_stream(settings),
    );
    // No engine: the placeholder primary decision 0283 gives a core whose accounts have not joined. Its client
    // is never sent an order, so the journal-replay no-op serves; no journal, no restore, no strategy factory.
    let placeholder: Box<dyn vike_execution::ExecutionClient + Send> =
        Box::new(vike_core::replay::ExecutionClientJournalReplay);
    let handle =
        vike_core::spawn_core_without_accounts(placeholder, vike_core::CoreConfig::default());
    let (mode, mounts) = ready_banner(live, &std::collections::HashSet::new(), Vec::new());
    let mode = format!("{NO_DAEMON_PROFILE}: nothing is mounted — {mode}");
    let identity = node_identity(settings, &None, "", "", live);
    let observe = door.and_then(|door| door.stop_answering_starting(None)).and_then(|door| {
        hub_server::start_observe_server(
            door,
            &handle,
            identity,
            mounts,
            Vec::new(),
            server::settings::SettingsShowSource {
                settings_dir: booted.settings_dir.clone(),
                hot: None,
            },
            Default::default(),
            strategy_state_dir(),
            Arc::default(),
            crate::config::MountBuilds::default(),
        )
    });
    super::spawn_stdin_control(&handle, String::new(), mode, stop.flag(), observe.clone());
    let found = Arc::new(AtomicBool::new(false));
    let watch = spawn_profile_watch(db, stop.flag(), Arc::clone(&found));

    stop.wait();

    if let Some(p) = &observe {
        p.shutdown();
    }
    let deadline =
        Duration::from_millis(crate::config::ProfileDaemonConfig::default().shutdown_deadline_ms);
    let tasks: Vec<Box<dyn FnOnce() + Send + 'static>> = vec![Box::new(move || {
        // A `join` Err is a panic its hook already reported; the teardown goes on.
        let _ = watch.join();
    })];
    let started = Instant::now();
    let outcome = run_with_deadline(tasks, Box::new(move || handle.shutdown_and_join()), deadline);
    let graceful = matches!(outcome, ShutdownOutcome::Graceful);
    let code = report_teardown(outcome, started.elapsed(), deadline);
    match crate::late_arm::exit_status(found.load(Ordering::SeqCst), graceful) {
        Some(restart) => ExitCode::from(restart),
        None => code,
    }
}

/// Ask the store every [`PROFILE_POLL`] for an ACTIVE daemon-profile row; when one is there, set `found` and raise
/// the stop flag, so the teardown runs and the process exits for its restart. Ends with the stop flag either way.
fn spawn_profile_watch(
    db: PathBuf,
    stop: Arc<AtomicBool>,
    found: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("th-profile-watch".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Some(name) = active_daemon(&db) {
                    tracing::warn!(
                        "the daemon profile `{name}` is ACTIVE now: stopping this empty node so its restart runs it \
                         (exit {})",
                        vike_utility::cli::PLANNED_RESTART_EXIT
                    );
                    found.store(true, Ordering::SeqCst);
                    vike_boot::stop::request_stop(&stop);
                    return;
                }
                let next = Instant::now() + PROFILE_POLL;
                while Instant::now() < next && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(vike_boot::stop::STOP_POLL);
                }
            }
        })
        .expect("spawn th-profile-watch")
}
