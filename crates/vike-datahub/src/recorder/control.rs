//! The recorder's operator edges: webhook targets, the per-tick report, stdin control words.

use std::io::{BufRead, IsTerminal};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_ops::stop;
use vike_recorder::runtime::FeedTick;

/// The webhook targets a silent-series alert may be delivered to, from the credential store.
///
/// Read HERE, in the binary, which is where the settings-registry rule puts an I/O-performing
/// configuration read — `vike_alerting::webhook_configs_from_env` is PURE over a caller-supplied
/// map and the alerting library never opens a store itself (its one self-sweeping wrapper was
/// deleted for exactly that reason).
///
/// The PROCESS environment is layered OVER the store, and that order is the point on this daemon:
/// the recorder runs under systemd, where `Environment=` / `EnvironmentFile=` is the channel an
/// operator has, while the project store is the channel a workstation has. Absent both ⇒ an empty
/// list ⇒ log-only delivery, the absent-credentials-is-the-gate idiom.
///
/// A store that exists and cannot be read is reported and treated as empty rather than failing the
/// mount: a missing pager must never stop a recorder from recording.
///
/// `settings_dir` is `$VIKE_SETTINGS_DIR` as `main` resolved it. ⚠ It used to be a hard-coded
/// `None` — the walk with no override — so the unit's `Environment=VIKE_SETTINGS_DIR=` line named a
/// directory this daemon never consulted for its own credentials.
pub(super) fn webhook_targets(
    settings_dir: Option<&str>,
    env: &std::collections::HashMap<String, String>,
) -> Vec<vike_alerting::WebhookConfig> {
    // ⚠ The SCOPE is `vike_alerting`'s own constant, never a list restated here — owner ruling
    // 2026-09-16, a process materialises only what it asked for. This daemon trades nothing and
    // signs nothing, and it used to resolve the WHOLE venue credential store (67 rows on the live
    // box) to reach three alerting names. A scope restated locally could drift from the reader that
    // consumes it, and the symptom of that drift would be alerting silently delivering nowhere —
    // `vike_alerting::WEBHOOK_KEYS`'s own doc and its
    // `the_declared_webhook_keys_are_the_ones_the_reader_uses` hold the two together.
    let scope = vike_secrets::KeyScope::of(vike_alerting::WEBHOOK_KEYS);
    let mut vars = match vike_secrets::resolve_project_scoped(settings_dir, &scope) {
        Ok(scoped) => scoped.into_map(),
        Err(e) => {
            tracing::warn!(error = %e, "recorder: credential store unreadable — alerting will \
                 deliver to the log only");
            Default::default()
        }
    };
    vars.extend(env.clone());
    vike_alerting::webhook_configs_from_env(&vars)
}

/// The per-tick OBSERVATION column. Takes a slice rather than owning the ticks: the same
/// `Vec<FeedTick>` then feeds `alerts::resolve_tick` and, under `--once`,
/// `runtime::dry_run_failures`. It used to CONSUME them, which is why the tick's content could
/// never reach the exit status.
pub(super) fn report(ticks: &[FeedTick]) {
    for t in ticks {
        match t {
            FeedTick::Reconciled { venue, symbols, report, .. } => {
                if !report.is_quiet() {
                    tracing::info!(
                        %venue,
                        symbols,
                        started = report.started.len(),
                        stopped = report.stopped.len(),
                        failed = report.failed.len(),
                        "recorder: subscriptions changed"
                    );
                }
                for (sym, stream, err) in &report.failed {
                    tracing::warn!(%venue, %sym, stream = stream.as_str(), error = %err,
                        "recorder: subscribe failed — retrying next tick");
                }
                for stream in &report.learned_unsupported {
                    tracing::info!(%venue, stream = stream.as_str(),
                        "recorder: venue serves no such stream — not recorded");
                }
            }
            FeedTick::ResolveFailed { venue, error, .. } => {
                // Subscriptions were left untouched; this is a retry, not a gap. ⚠ UNCHANGED, and
                // deliberately: for a venue that HAS resolved before, that sentence is exactly
                // right. The venue that has NEVER resolved is escalated separately, by
                // `alerts::resolve_tick`, to `error!` plus a real alert.
                tracing::warn!(%venue, %error,
                    "recorder: could not resolve the desired set — subscriptions unchanged");
            }
        }
    }
}

/// Reads control words off stdin. EOF sets the stop flag ONLY on a TTY — see the module doc.
pub(super) fn spawn_stdin_control(stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let is_tty = std::io::stdin().is_terminal();
        control_loop(std::io::stdin().lock(), is_tty, &stop);
    });
}

/// The control channel's whole decision, as a function of the LINES and one boolean — so the
/// non-tty-EOF rule can be tested instead of trusted.
///
/// That rule is the one a reader gets wrong: **EOF is a stop only on a TTY.** Under systemd stdin is
/// `/dev/null` and reads EOF at once, so treating EOF as a stop would exit the daemon at startup,
/// every start, on every box. It is not a hypothetical either — it is why the stdio channel could
/// never be the systemd stop path, and therefore why `vike_ops::stop` exists.
///
/// Split out of [`spawn_stdin_control`] purely for testability: that function is three lines of
/// stdin wiring, and this is the part with a rule in it.
pub(super) fn control_loop(reader: impl BufRead, is_tty: bool, stop: &AtomicBool) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        match line.trim() {
            "quit" | "shutdown" | "stop" | "exit" => {
                stop::request_stop(stop);
                return;
            }
            "" => {}
            other => {
                eprintln!("vike-recorder: unknown control word `{other}` (quit|shutdown|stop)")
            }
        }
    }
    if is_tty {
        stop::request_stop(stop);
    } else {
        tracing::info!(
            "stdin closed (non-tty) — the recorder keeps recording headless; stop via SIGTERM (unix) / Ctrl-C (windows)"
        );
    }
}
