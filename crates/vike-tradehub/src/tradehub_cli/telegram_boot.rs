//! Starting the Telegram control channel (`telegram` feature): ledger paths and the gated spawn.

use std::path::PathBuf;

use vike_core::CoreHandle;

use super::paths::state_dir;
use super::settings::{resolve_control_limits, workspace_credentials};

/// A resolved boolean flag, spelled as the exact `"1"` string the pure gate parses.
///
/// The gates this feeds (`crate::telegram::control_gates_open`) keep their own
/// exact-`"1"` grammar and their own tests; only the RESOLUTION moved into `vike_config`, which
/// already applied `env > file > default` and already rejects a truthy typo by name. `false` is
/// passed as `None` — absent — which that grammar has always meant "closed", so the OFF path is
/// byte-identical to an unset variable.
#[cfg(feature = "telegram")]
fn as_gate(on: bool) -> Option<&'static str> {
    on.then_some("1")
}

/// The at-most-once Telegram `update_id` ledger's basename inside the project state directory.
///
/// **`.ledger`, not the `.log` it used to be.** The file is not a log and never was — it holds one
/// integer per line and its only reader takes the max. The old suffix invited exactly the reading
/// that produced the defect this move fixes ("it's a log, best-effort is fine"), and
/// `<settings>/state/` is a SHARED directory, so a name that says what the file IS earns its keep —
/// the same reasoning `crates/bridges/ctrader/src/bin/ctrader_authorize.rs`'s `TOKEN_FILE` records
/// for `ctrader_token.json`.
#[cfg(feature = "telegram")]
const TELEGRAM_LEDGER_FILE: &str = "telegram_updates.ledger";

/// The PRE-MOVE location's basename, read once at open so an existing install's mark migrates
/// instead of replaying: `<exe_dir>/telegram_updates.log`.
#[cfg(feature = "telegram")]
const LEGACY_TELEGRAM_LEDGER_FILE: &str = "telegram_updates.log";

/// Where the at-most-once ledger lives: `<state_dir>/telegram_updates.ledger`, plus the legacy
/// `<exe_dir>` copy to migrate from. `None` when no state directory resolves — see
/// `crate::telegram::maybe_spawn`, which refuses to arm the channel rather than inventing a
/// second location.
///
/// ⚠ **It used to be `<exe_dir>/telegram_updates.log`, and that was a production defect.**
/// `deploy/vike-tradehub.service` runs `ProtectSystem=strict` with `ExecStart=<project>/bin/…`, so
/// the executable's directory is READ-ONLY — every append failed, and the writes were best-effort,
/// so it failed SILENTLY. What degraded was the at-most-once record of a remote order-origination
/// path. The cure is not to make the exe directory writable (a daemon that can rewrite its own
/// binary is a worse problem); it is to put program-written state where program-written state
/// goes — `crates/vike-model/src/paths/state_path.rs`'s `project_state_dir`, the one root
/// `<state_dir>/alerts.json` already resolves to, and the one the unit grants `ReadWritePaths=` for.
///
/// Deliberately NOT its own env knob: `$VIKE_STATE_ROOT` and `$VIKE_SETTINGS_DIR` already relocate
/// it, and the settings registry is better off without a third row that only names a path.
#[cfg(feature = "telegram")]
fn telegram_ledger_paths() -> Option<crate::telegram::LedgerPaths> {
    Some(crate::telegram::LedgerPaths {
        path: state_dir()?.join(TELEGRAM_LEDGER_FILE),
        legacy: legacy_telegram_ledger_path(),
    })
}

/// The pre-move ledger path, `<exe_dir>/telegram_updates.log` — READ at open to carry an existing
/// install's mark forward, never written. `None` when the executable's directory is unknowable,
/// which simply means there is nothing to migrate.
#[cfg(feature = "telegram")]
fn legacy_telegram_ledger_path() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|d| d.join(LEGACY_TELEGRAM_LEDGER_FILE))
}

/// Mount the TELEGRAM control channel, or return `None` having constructed NOTHING.
///
/// FOUR simultaneous gates (see [`crate::telegram`]'s module doc): the `flags.tradehub_control` and
/// `flags.telegram_control` FLAGS (each still overridden by `VIKE_TRADEHUB_CONTROL` /
/// `VIKE_TELEGRAM_CONTROL`), plus a `VIKE_TELEGRAM_BOT_TOKEN` and a non-empty
/// `VIKE_TELEGRAM_ALLOWED_CHAT_IDS` (both from the credential store). The two flags arrive as
/// PARAMETERS — resolved once by [`resolve_settings`], not re-read here — and are still evaluated by
/// the pure `telegram::control_gates_open`, which keeps its own exact-`"1"` grammar; a resolved
/// `false` is passed as `None` (absent), which that grammar already means "closed". The credential
/// map, [`workspace_credentials`], is handed over as a FUNCTION (the
/// `maybe_mount_alerts` idiom) — so on the OFF path no credential store is opened at all, the bot
/// token never enters this process, no `ureq` agent is built, and no thread is spawned. That makes
/// the default daemon byte-identical to the pre-Telegram one.
///
/// The channel gets its OWN `ControlLimits` bucket over the SAME [`resolve_control_limits`] config
/// the TCP server uses, and a `CommandSink` clone — the same handle `start_observe_server` threads
/// into `serve`. Nothing here touches the core fold: commands go through the non-blocking ingest
/// lane and reads come off the lossy arc-swap snapshot cell. (The sink/cell clones are made before
/// the gate is consulted purely because the deps builder closure captures them; on the closed path
/// the closure is dropped un-called, and an in-process channel handle nobody can send through
/// grants nothing — the gate is on whether a REMOTE surface exists at all.)
///
/// A FIFTH, compile-time gate sits above all four: the crate's off-by-default `telegram` feature.
/// Without it this function, its ledger-path helpers, its env-name constant and the whole
/// `crate::telegram` module are absent from the binary — the four runtime gates protect a
/// path that exists, the feature makes the path not exist.
///
/// And one PRECONDITION sits below all five, checked inside `maybe_spawn` after the gates: the
/// at-most-once ledger ([`telegram_ledger_paths`]) must be readable and appendable. It is not a
/// gate in the same sense — an operator does not choose it — but it fails the same way, loudly and
/// closed: no channel, one `error!` naming the cause, and a daemon that keeps trading headless.
#[cfg(feature = "telegram")]
pub(super) fn maybe_start_telegram(
    handle: &CoreHandle,
    tradehub_control: bool,
    telegram_control: bool,
) -> Option<vike_bridge_core::poller::StopHandle> {
    let limits = resolve_control_limits();
    let sink = handle.command_sink();
    let cell = handle.snapshot_cell();
    crate::telegram::maybe_spawn(
        as_gate(tradehub_control),
        as_gate(telegram_control),
        workspace_credentials,
        telegram_ledger_paths(),
        move |cfg| {
            Box::new(crate::telegram::ProdTelegramDeps::new(cfg, limits, sink, cell))
                as Box<dyn crate::telegram::TelegramDeps + Send>
        },
    )
}
