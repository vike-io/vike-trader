//! The state-root paths the daemon writes under, and the alerting mount that reads one of them.

use std::path::{Path, PathBuf};

use crate::alerts::{self, AlertMount};

use super::process_env;
use super::settings::{SETTINGS_STATE_DIR, workspace_credentials};

/// The alerting rule-file override, read HERE rather than in the library so the env read stays in
/// the binary — the settings-registry rule. Since settings STEP 2 this is the workspace's ONLY
/// `$VIKE_ALERTS` read (`vike_alerting::persist` no longer reads it at all). Unset ⇒
/// `<project>/settings/state/alerts.json` (see [`alerts_path`]). Absent file ⇒ no rules ⇒ no
/// engine (see [`maybe_mount_alerts`]).
const ALERTS_ENV: &str = "VIKE_ALERTS";

/// The STATE-ROOT override — the one root for every file the program writes and no human edits.
/// Unset, it is `<project>/settings/state` (`vike_model::paths::state_path`). Read here in the BINARY,
/// which is the correct shape; the resolver itself is pure.
///
/// ⚠ `_ROOT`, not `_DIR`: `VIKE_STATE_DIR` was `vike-app`'s strategy-state SIDECAR directory and
/// meant something else entirely — and it is a REMOVED variable now, refused at startup — see
/// `vike_model::paths::state_path`'s module doc.
const STATE_ROOT_ENV: &str = "VIKE_STATE_ROOT";

/// The alerting rule file: `$VIKE_ALERTS` (see [`ALERTS_ENV`]) if set and non-blank, else
/// `<state_dir>/alerts.json`.
///
/// The ENV half is resolved HERE, in the binary — settings STEP 2 deleted the twin read that used
/// to sit in `persist::path()` inside a LIBRARY (exactly the shape the settings registry exists to
/// push out of libraries), so this is now the workspace's only `$VIKE_ALERTS` read and the daemon
/// hands the result to `persist::load_path`. The library still states the BASENAME
/// (`vike_alerting::persist::ALERTS_FILE`), so the two sides cannot disagree about it.
///
/// `None` when there is no override AND no state directory resolves — there is no file to read
/// then, and [`maybe_mount_alerts`] says so rather than reading somewhere else.
fn alerts_path() -> Option<PathBuf> {
    alerts_path_in(process_env().get(ALERTS_ENV).map(String::as_str), state_dir().as_deref())
}

/// [`alerts_path`]'s pure core, so the precedence is testable without touching the process
/// environment: the override when set and non-blank, else `<state_dir>/alerts.json`.
///
/// A blank override falls THROUGH rather than resolving to `""` — an empty systemd
/// `Environment=VIKE_ALERTS=` line would otherwise point the loader at the working directory, the
/// same class of bug `vike_model::paths::state_path`'s own blank-value guard exists for.
pub fn alerts_path_in(override_path: Option<&str>, state_dir: Option<&Path>) -> Option<PathBuf> {
    match override_path.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => Some(state_dir?.join(vike_alerting::persist::ALERTS_FILE)),
    }
}

/// The project's STATE directory — `<project>/settings/state` — or `None` when no project sits
/// above the working directory.
///
/// The env-reading half of `vike_model::paths::state_path` (which is pure), living in the BINARY exactly
/// as the settings-registry rule wants. [`STATE_ROOT_ENV`] names the directory outright and wins;
/// there is no third location.
///
/// ⚠ **The rung below the override is [`SETTINGS_STATE_DIR`] — the BOOT's walk — and it used to be
/// a walk of its own.** `project_state_dir(&cwd)` is `$VIKE_SETTINGS_DIR`-BLIND, so this daemon's
/// log home, `alerts.json` and telegram ledger hung off a project the settings and credentials had
/// not necessarily come from. `deploy/vike-tradehub.service` sets the override AND
/// `WorkingDirectory=` to the same tree, so the two agreed on the CI box by coincidence of the working
/// directory rather than because the override was honoured — which is precisely the dependency the
/// unit's own comment says the variable removes.
pub(super) fn state_dir() -> Option<PathBuf> {
    if let Some(explicit) =
        process_env().get(STATE_ROOT_ENV).cloned().filter(|s| !s.trim().is_empty())
    {
        return Some(PathBuf::from(explicit));
    }
    SETTINGS_STATE_DIR.get().cloned().flatten()
}

/// The daemon's STRATEGY-STATE directory — `<state root>/strategy-state`, handed to
/// `vike_core::CoreConfig::state_dir` by BOTH mount arms (split-plane B5, residual closed). Two
/// families of files live under it, both written by the core's own arms and read back at the
/// next boot: the per-mount `<mount_id>.json` durable-state sidecars
/// (`vike_core::strategy_state`) and the runtime-mount TOPOLOGY sidecar
/// (`vike_core::mount_topology` — what [`resurrect_runtime_mounts`] replays;
/// `crate::mount_factory`'s `resurrect_runtime_mounts` documents the ordering contract
/// both arms obey).
///
/// The SUBDIRECTORY is the spelling vike-app's project rung used (`state_dir_path` in that binary
/// joined `strategy-state` under ITS state root, until it went with the desktop's local core) so
/// the two binaries shelved strategy state the same way; hanging it off [`state_dir`] means
/// `$VIKE_STATE_ROOT` relocates it together with every other file this daemon writes (alerts, the
/// telegram ledger, logs). `None` — no project, no override — keeps `CoreConfig::state_dir` at
/// `None`: no sidecar is ever written or read, byte-identical to this daemon before B5's residual
/// closed. Deliberately NOT `config.state_dir`:
/// that key was the DESKTOP's sidecar knob, and this daemon's state tree is uniformly
/// `$VIKE_STATE_ROOT`-rooted. ⚠ That key no longer exists at all — the unread-settings sweep
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
/// (still the operator's override) and below anything a config file names, and ABOVE vike-log's
/// `<exe_dir>/logs` last resort. `None` — no state root, no project — keeps that last resort, which
/// is what a binary run from a directory with no project above it gets.
///
/// The env read stays here in the binary (via [`state_dir`]); `vike_model::paths::state_path` is pure and
/// vike-log depends on no crate at all, so neither can resolve this itself.
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
/// `crates/vike-ops/tests/settings/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down. Moving the
/// read here is also a small correctness gain: this daemon resolves the credential store the same
/// way every other consumer does, so alert webhook targets are found where the venue credentials
/// already live.
///
/// **The OFF path is LOGGED, and that is deliberate.** Every other consequence of a mis-resolved
/// state directory announces itself — panes reset, a layout list comes back empty — but alerting's
/// steady state IS silence, so "no alerts arrived" is indistinguishable from "the rules file was
/// never found". One line naming the path the daemon actually consulted closes that, for every
/// cause at once: a mistyped `$VIKE_ALERTS`, a unit started from the wrong `WorkingDirectory`, a
/// file that parsed but holds no ENABLED rule. `info!` rather than `warn!` because alerting is
/// off-by-default and unconfigured is the normal state for most nodes — a warn on every daemon
/// that never wanted alerts is a warn nobody reads.
pub(super) fn maybe_mount_alerts() -> Option<AlertMount> {
    let Some(path) = alerts_path() else {
        tracing::info!(
            "alerting: no rules file — ${ALERTS_ENV} is unset and no project settings directory \
             resolves above the working directory, so no alerts can fire"
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
