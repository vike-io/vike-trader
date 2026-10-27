//! Dials and homes built from values only this root holds: the boot walk and the process env.

use super::*;

/// The datahub STORE handle every hist-plane read dials through — **authenticated** when this
/// desktop holds a datahub observe key.
///
/// ⚠ **This exists because the store plane was the half that never got wired.**
/// `crate::backend_registry`'s `datahub_observe_keys` doc records that before it landed, nothing in
/// `vike-app-core` or `vike-desktop` called `hub_keys_from_vars`, `RemoteHistStore::with_keys` or
/// `DatahubClient::connect_authed`, and "every dial from the desktop was UNAUTHENTICATED". The
/// MARKET-DATA half was then fixed — `md_session`'s dials go through `connect_authed` — and the
/// STORE half was not: both `RemoteHistStore::new` call sites stayed on the key-less constructor,
/// so `with_keys` had no production caller at all.
///
/// What that cost, measured against a real deployed server: the Data Manager's Stored tab read
/// `No stored data · 0 rows · 0 B` from a datahub that answered the very same address with five
/// series and 7.49 billion rows seconds earlier on the CLI, and the log said why — *"this datahub
/// REQUIRES authentication (it advertises the `auth` feature) but no hub keys were supplied"*.
/// Studio failed the same way through its own `Err`.
///
/// ⚠ **The severity is structural, not situational.** `vike_datahub_client::bind::bind_decision`
/// REFUSES to start a datahub bound to a non-loopback address without keys, so every datahub
/// reachable off-box is necessarily authenticated — which made the GUI's whole store plane
/// unusable against every remote deployment, by construction, while the local dev case kept working
/// and hid it.
///
/// ⚠ **There is no configuration in which the key-less constructor is the better choice**, which is
/// why this helper has no "plain" arm to pick: `RemoteHistStore::with_keys`' own contract is that
/// against a KEY-LESS server it degrades to an ordinary unauthenticated connect, "so one configured
/// GUI works against both a keyed production datahub and a local dev one". The `None` arm below is
/// reached only when this desktop holds NO key to offer, never as a preference.
/// ⚠ **The BODY moved to `vike_app_core::backend::backend_registry::remote_hist_store`** — beside
/// `datahub_observe_keys`, whose resolution is the hard half — so the merge gate compiles and
/// tests it and so the chart's own store read
/// (`vike_app_core::data::store_bars`) dials identically without importing this binary. What is left
/// here is the two composition-root facts a library may not read for itself.
pub fn remote_hist_store(addr: String, key_name: &str) -> vike_datahub_client::RemoteHistStore {
    vike_app_core::backend::backend_registry::remote_hist_store(
        addr,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        // ⚠ `PROCESS_ENV`, NOT `workspace_credentials()` — the same distinction the market-data
        // mount states at its own call site: the VENUE store would miss a key set only in the
        // environment and would find a legacy one SILENTLY, skipping the decision-0051 notice.
        process_env(),
        key_name,
    )
}

/// The OBSERVE hub key the Studio's NAMED-RUN backend signs its dial with
/// (`docs/decisions/0064-a-named-run-carries-no-source.md`) — `remote_hist_store`'s twin, built from
/// this binary's two `OnceLock`s for the identical reason: only a BINARY reads the process
/// environment (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`), and the resolution behind it opens
/// the settings database's `hub_key` table.
///
/// ⚠ **The notices are dropped here and that is a deliberate difference from `remote_hist_store`'s
/// caller.** `datahub_observe_keys` returns a `Vec<String>` of node-store notices, and
/// the store plane surfaces them because the store is where a missing key shows up as an empty
/// catalog somebody then debugs. A named run cannot fail silently the same way: an unauthenticated
/// dial against a keyed backtesthub fails AT CONNECT with a message naming the keys, which
/// `crates/vike-studio/src/backend/remote.rs`'s `connect_observe` puts straight into the Studio's
/// error banner. Re-surfacing the same notice twice per frame would be noise, and this is called
/// once per Studio construction.
pub(crate) fn named_run_observe_keys(key_name: &str) -> Option<vike_hub_proto::auth::HubKeys> {
    vike_app_core::backend::backend_registry::datahub_observe_keys(
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        // ⚠ `PROCESS_ENV`, NOT `workspace_credentials()` — the same distinction every other dial in
        // this file states: the VENUE store would miss a key set only in the environment and would
        // find one filed in the wrong store SILENTLY (decision 0051: hub keys live in `hub_key`).
        process_env(),
        key_name,
    )
    .0
}

/// The CHART-SEED dial, built from this binary's two `OnceLock`s — `remote_hist_store`'s twin, and
/// here for the identical reason: only a BINARY reads the process environment
/// (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`), so the SWEEP belongs in the composition root even
/// though everything it feeds lives one crate down in `vike_app_core::data::chart_seed`.
pub(crate) fn chart_seed_dial(
    addr: String,
    key_name: &str,
    note: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> vike_app_core::data::chart_seed::SeedDial {
    vike_app_core::data::chart_seed::SeedDial::resolve(
        addr,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
        process_env(),
        key_name,
        note,
    )
}

/// **WHERE the two GUI surfaces that WRITE credentials put them, and where the write is RECORDED** —
/// the Connections editor's Save arm and the Data Manager's Polymarket proxy box.
///
/// Both homes are DERIVED from this binary's ONE boot walk ([`SETTINGS_DIR`] / [`STATE_DIR`]) rather
/// than found again, which is a FIX and not a tidy-up: both sites used to call a `_from`-less,
/// `$VIKE_SETTINGS_DIR`-BLIND resolver while [`workspace_credentials`] — the READ their grids render
/// from — honours the override, so a deployment naming its settings directory showed one project's
/// keys and saved into another's. `vike_gateway_panel::CredentialHome` is where that argument lives in
/// full, and it lives THERE rather than here because this crate is outside the CI roster: the
/// resolution is covered by that crate's own suite, and the shell keeps only the wiring.
///
/// `Proc::current` reads `current_exe`, so the whole thing is resolved once per process rather than
/// per write; the INSTANT is not cached, because the journal reads no clock and every record stamps
/// its own.
pub(crate) fn credential_home() -> &'static vike_gateway_panel::CredentialHome {
    static HOME: std::sync::OnceLock<vike_gateway_panel::CredentialHome> =
        std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        vike_gateway_panel::CredentialHome::resolve(
            SETTINGS_DIR.get().and_then(Option::as_ref).map(std::path::PathBuf::as_path),
            STATE_DIR.get().and_then(Option::as_ref).map(std::path::PathBuf::as_path),
            vike_change_journal::Proc::current(env!("CARGO_PKG_VERSION")),
        )
    })
}
