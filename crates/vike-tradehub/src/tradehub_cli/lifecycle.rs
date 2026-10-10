//! The periodic summary thread, the teardown report and the signal-handler outcome log.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::alerts::AlertMount;
use crate::hot_reload;
use vike_core::CoreHandle;
use vike_mount::MakerMountConfig;
use vike_ops::shutdown::ShutdownOutcome;
use vike_ops::stop;

/// **Startup phase — the periodic snapshot SUMMARY thread** (also the alerting engine's one input
/// tick, and where the hot-apply queue is drained). Returns the thread's own stop flag and its
/// handle: [`run`] raises the flag at the top of the teardown and joins the handle as a BOUNDED
/// task — see the comment there for why that join must not be inline.
///
/// `alerts` is `mut` here, not at [`run`]'s binding: this is where the engine MOVES onto the tick
/// thread and is mutated (off-fold) on each tick.
pub(super) fn spawn_summary_thread(
    handle: &CoreHandle,
    cfg: &MakerMountConfig,
    profile: &crate::config::DaemonProfile,
    log_reload: vike_log::LogReloadHandles,
    booted: &vike_boot::Booted,
    hot_ticker: hot_reload::HotApplyTicker,
    mut alerts: Option<AlertMount>,
) -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let summary_stop = Arc::new(AtomicBool::new(false));
    let summary_handle = {
        let cell = handle.snapshot_cell();
        let token = cfg.token_id.clone();
        let interval = profile.summary_interval();
        let stop = Arc::clone(&summary_stop);
        // The tick side of the hot-apply seam (REQ-7 v2): the applier holds the log-filter
        // reload handles plus the SAME boot fact the `SettingsShowSource` holds (the directory), so
        // a hot apply re-resolves exactly the rows a restart would.
        let hot_applier = hot_reload::LogLevelApplier {
            handles: log_reload,
            settings_dir: booted.settings_dir.clone(),
        };
        std::thread::Builder::new()
            .name("vt-tradehub-summary".into())
            .spawn(move || {
                let step = interval.min(Duration::from_millis(100)).max(Duration::from_millis(1));
                let mut waited = Duration::ZERO;
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(step);
                    // Hot applies drain on EVERY wake (≤100 ms), not only on summary prints: the
                    // server thread is holding a `SetSetting` peer against HOT_APPLY_DEADLINE,
                    // and a 5 s summary cadence must not be what times an apply out. Same
                    // thread, same tick loop — still never the fold, never the wire thread.
                    hot_ticker.drain(|key| hot_applier.apply(key));
                    waited += step;
                    if waited < interval {
                        continue;
                    }
                    waited = Duration::ZERO;
                    let snap = cell.load_full();
                    println!("{}", crate::summary::summary_line(&snap, &token));
                    // Best-effort flush of a status line: a closed stdout has no reader to inform.
                    let _ = std::io::stdout().flush();
                    // Alerting rides the SAME tick, strictly AFTER the protocol line so stdout
                    // ordering is untouched. Absent a mount this `if let` is skipped entirely.
                    // The pure alert evaluator owns no clock by design, so the wall clock is read
                    // HERE and handed to it.
                    if let Some(m) = alerts.as_mut() {
                        m.on_snapshot(&snap, vike_model::now_ms());
                    }
                }
            })
            .expect("spawn vt-tradehub-summary thread")
    };
    (summary_stop, summary_handle)
}

/// **Shutdown phase — say how the bounded teardown ended**, and return the exit code: 0 on every
/// outcome (an operator stop is not a crash, so a systemd `Restart=on-failure` unit must not read
/// it as one), with the wording keyed to which outcome it was.
pub(super) fn report_teardown(
    outcome: ShutdownOutcome,
    teardown_took: Duration,
    deadline: Duration,
) -> ExitCode {
    match outcome {
        ShutdownOutcome::Graceful => {
            tracing::info!(
                elapsed_ms = teardown_took.as_millis() as u64,
                "vike-tradehub shut down gracefully"
            );
            ExitCode::SUCCESS
        }
        // The orchestration thread died without signalling — a panic in the teardown itself. It
        // returns at once rather than at the deadline, so it must NOT be reported as a hard cap:
        // that wording named a budget nothing had spent. Still exit 0 (an operator stop is not a
        // crash), but say plainly that the teardown did not complete.
        ShutdownOutcome::Aborted { stage } => {
            tracing::error!(
                elapsed_ms = teardown_took.as_millis() as u64,
                stage = ?stage,
                "vike-tradehub teardown ABORTED after {teardown_took:?} — the shutdown \
                 orchestration panicked while {}; flushes past that point did NOT run",
                stage.describe()
            );
            ExitCode::SUCCESS
        }
        ShutdownOutcome::HardCapped { still_running, stage } => {
            // A deliberate stop that outran its deadline — surface it, but still exit 0 so a systemd
            // `Restart=on-failure` unit does not treat an operator stop as a crash.
            //
            // ⚠ The message names the STAGE and the ELAPSED time, not just a count and the budget.
            // It used to read "hard-capped at the 10s deadline; 0 task(s) still in flight", which is
            // self-contradictory: `still_running` counts only the PARALLEL tasks, so it is 0 for the
            // whole of the tail. A live stop printed exactly that and sent an operator hunting a
            // straggler that did not exist, while the real holder was the tail's core join
            // (the alpaca+ctrader live rehearsal (PR #1407), Finding C). Printing `{deadline:?}`
            // was the other half: it echoed the CONFIGURED budget and so could not even show that
            // the time had been spent.
            tracing::warn!(
                elapsed_ms = teardown_took.as_millis() as u64,
                deadline_ms = deadline.as_millis() as u64,
                still_running,
                stage = ?stage,
                "vike-tradehub shutdown hard-capped after {teardown_took:?} (deadline {deadline:?}): {} \
                 — {still_running} parallel task(s) still in flight",
                stage.describe()
            );
            ExitCode::SUCCESS
        }
    }
}

/// Say what [`vike_ops::stop::install_handlers`] did, through the subscriber this binary owns.
///
/// It is a separate step from the install because the install happens FIRST — at the very top of
/// [`main`], before the settings load that decides where logs go — and a library that wrote to a
/// caller's stderr on its own initiative could not be used by a binary whose stdout is a protocol.
/// So the outcome travels as DATA and is reported here, exactly like `settings_warning_lines`.
pub(super) fn log_handler_outcome(outcome: &stop::HandlerOutcome) {
    match outcome {
        // ⚠ The line names what THIS platform actually installed. `deploy/vike-tradehub.service`
        // tells an operator to grep the log for the unix wording as proof the build is new enough,
        // so the two must not be one string that is half true on each platform — and B10 gave
        // Windows a real arm while leaving this message claiming a signal that does not exist there.
        #[cfg(not(windows))]
        stop::HandlerOutcome::Installed => tracing::info!(
            "SIGTERM/SIGINT will stop this daemon gracefully — the teardown runs, so \
             `cancel_orders_on_shutdown` is honoured on a `systemctl stop` as well as on an \
             interactive one"
        ),
        // Windows: console control events, which reach only a daemon that HAS a console. A detached
        // background run is stopped through the stop file armed in `main` instead — this line says
        // which of the two you have, because the answer decides how an operator stops the box.
        #[cfg(windows)]
        stop::HandlerOutcome::Installed => tracing::info!(
            "Ctrl-C / Ctrl-Break / console close will stop this daemon gracefully — the teardown \
             runs, so `cancel_orders_on_shutdown` is honoured. A DETACHED run has no console to \
             deliver those: it stops through the stop file (docs/ops/tradehub-windows.md)"
        ),
        // Neither POSIX signals nor a Windows console. The truth about the platform, not a
        // degradation — said out loud so nobody believes a service-manager stop is graceful.
        stop::HandlerOutcome::Unsupported => tracing::info!(
            "no signal or console stop on this platform — stop with the stdio `shutdown` word"
        ),
        // The one case an operator MUST see: the daemon trades, but a stop is back to abandoning
        // the resting book at the venue and nothing else would say so.
        stop::HandlerOutcome::Failed(e) => tracing::error!(
            error = %e,
            "could NOT install the signal handler — a SIGTERM will run NO teardown, so resting \
             orders stay live at the venue; close the book yourself before stopping \
             (docs/ops/kill-switches.md section C)"
        ),
    }
}
