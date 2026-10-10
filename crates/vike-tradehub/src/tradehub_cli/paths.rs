//! The state-root paths the daemon writes under, and the alerting mount that reads one of them.

use std::path::PathBuf;

use crate::alerts::{self, AlertMount};

use super::settings::{SETTINGS_STATE_DIR, workspace_credentials};

/// The alerting rule file: `<state_dir>/alerts.json` — `vike_alerting::persist::ALERTS_FILE` in the
/// state directory this daemon booted with. The library states the BASENAME, so the two sides
/// cannot disagree about it.
///
/// ⚠ There is no override: the `VIKE_ALERTS` variable that named another file is refused at startup
/// (decision 0111, `vike_config::REMOVED_ENV`). The path is derived, not a setting —
/// `VIKE_SETTINGS_DIR` moves it together with the settings.
///
/// `None` when no state directory resolves — there is no file to read then, and
/// [`maybe_mount_alerts`] says so rather than reading somewhere else.
fn alerts_path() -> Option<PathBuf> {
    Some(state_dir()?.join(vike_alerting::persist::ALERTS_FILE))
}

/// The project's STATE directory — `<project>/settings/state` — or `None` when no project resolves.
///
/// It is [`SETTINGS_STATE_DIR`] — the BOOT's one walk, which honours `VIKE_SETTINGS_DIR` — and
/// nothing else. ⚠ The `VIKE_STATE_ROOT` override that used to rank above it is refused at startup
/// (decision 0111): the state tree is always the settings directory's `state/`, the tree the shipped
/// units' `ReadWritePaths=` grant, so this daemon's log home, `alerts.json`, telegram ledger and
/// strategy sidecars can never hang off a different project from the settings and credentials.
pub(super) fn state_dir() -> Option<PathBuf> {
    SETTINGS_STATE_DIR.get().cloned().flatten()
}

/// The daemon's STRATEGY-STATE directory — `<state root>/strategy-state`, handed to
/// `vike_core::CoreConfig::state_dir` by BOTH mount arms (split-plane B5, residual closed). Three
/// families of files live under it, all written by the core and read back at the next boot: the
/// per-mount `<mount_id>.json` durable-state sidecars (`vike_core::strategy_state`), the
/// runtime-mount TOPOLOGY sidecar (`vike_core::mount_topology` — what [`resurrect_runtime_mounts`]
/// replays; `crate::mount_factory`'s `resurrect_runtime_mounts` documents the ordering contract
/// both arms obey), and the ORDER-OWNERSHIP file (`vike_core::order_owners`, decision 0113), which
/// both arms hand the core as `CoreConfig::order_owners` over this same directory.
///
/// The SUBDIRECTORY is the spelling vike-app's project rung used (`state_dir_path` in that binary
/// joined `strategy-state` under ITS state root, until it went with the desktop's local core) so
/// the two binaries shelved strategy state the same way; it hangs off [`state_dir`] beside every
/// other file this daemon writes (alerts, the telegram ledger, logs). `None` — no project — keeps `CoreConfig::state_dir` at
/// `None`: no sidecar is ever written or read, byte-identical to this daemon before B5's residual
/// closed. Deliberately NOT `config.state_dir`:
/// that key was the DESKTOP's sidecar knob, and this daemon's state tree is uniformly
/// [`state_dir`]-rooted. ⚠ That key no longer exists at all — the unread-settings sweep
/// deleted it once the desktop cut took its one reader, and `vike_config::REMOVED_ENV` refuses
/// `VIKE_STATE_DIR` at startup — so the distinction this paragraph draws is now permanent rather
/// than a choice this daemon re-makes.
pub(super) fn strategy_state_dir() -> Option<PathBuf> {
    Some(state_dir()?.join("strategy-state"))
}

/// The DEFAULT log directory — `<state root>/logs`, under [`state_dir`] so the daemon's rolling
/// trace file sits with every other file the program writes and no human edits.
///
/// Handed to `vike_log::init` as `LogConfig::project_dir`, which is the layer BELOW `$VIKE_LOG_DIR`
/// (the bootstrap override the logger reads itself) and below the `config.log_dir` row, and ABOVE vike-log's
/// `<exe_dir>/logs` last resort. `None` — no state root, no project — keeps that last resort, which
/// is what a binary run from a directory with no project above it gets.
///
/// `vike_model::paths::state_path` is pure and vike-log depends on no crate at all, so neither can
/// resolve this itself.
pub(super) fn log_dir() -> Option<PathBuf> {
    Some(state_dir()?.join(vike_model::paths::state_path::LOGS_SUBDIR))
}

/// Build the headless alerting mount, or `None` (the DEFAULT-OFF path).
///
/// The rules FILE is the gate — no separate env flag, the same absent-config-is-the-gate idiom the
/// venues use for credentials. `None` (no file, an unparseable file, zero rules, or every rule
/// disabled) means nothing is constructed at all: no `AlertEngine`, no `LogSink`, and no
/// `WebhookSink`. Delivery targets come from the engine's own pure
/// `webhook_configs_from_env` over THIS binary's credential map ([`workspace_credentials`]) —
/// never a second config path invented here, and never a store read performed inside the alerting
/// library. It is still passed as a THUNK, not a value, so on the OFF path the credential store is
/// not even opened and no Telegram token is ever loaded.
///
/// The thunk used to be `vike_alerting::webhook_configs_from_workspace_env`, a library
/// function that opened the workspace `.env` itself — the class
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down. Moving the
/// read here is also a small correctness gain: this daemon resolves the credential store the same
/// way every other consumer does, so alert webhook targets are found where the venue credentials
/// already live.
///
/// **The OFF path is LOGGED, and that is deliberate.** Every other consequence of a mis-resolved
/// state directory announces itself — panes reset, a layout list comes back empty — but alerting's
/// steady state IS silence, so "no alerts arrived" is indistinguishable from "the rules file was
/// never found". One line naming the path the daemon actually consulted closes that, for every
/// cause at once: a settings directory that is not the one the operator meant, a
/// file that parsed but holds no ENABLED rule. `info!` rather than `warn!` because alerting is
/// off-by-default and unconfigured is the normal state for most nodes — a warn on every daemon
/// that never wanted alerts is a warn nobody reads.
pub(super) fn maybe_mount_alerts() -> Option<AlertMount> {
    let Some(path) = alerts_path() else {
        tracing::info!(
            "alerting: no rules file — no project settings directory resolves (name it with \
             VIKE_SETTINGS_DIR), so no alerts can fire"
        );
        return None;
    };
    let Some(mount) = alerts::maybe_mount(alerts::load_rules(&path), || {
        vike_alerting::webhook_configs_from_env(&workspace_credentials())
    }) else {
        tracing::info!(
            ?path,
            "alerting: no enabled rules at this path, so no alerts can fire (a missing, \
             unparseable, empty or all-disabled rules file all land here)"
        );
        return None;
    };
    tracing::warn!(
        ?path,
        rules = mount.rule_count(),
        enabled = mount.enabled_count(),
        "alerting engine mounted (off-fold, driven by the snapshot summary tick): Price / Drawdown \
         / ReconAlert rules can fire here; Fill / OrderRejected / Indicator / Feed / \
         FillRateBreaker / PolymarketResolution / SeriesStale rules load but have no source in the \
         daemon yet"
    );
    Some(mount)
}
