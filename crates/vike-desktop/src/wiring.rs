//! Dials and homes built from values only this root holds: the boot walk.

use super::*;

/// The datahub STORE handle every hist-plane read dials through — **authenticated** when this
/// desktop holds its device key.
///
/// ⚠ **This exists because the store plane was the half that never got wired.**
/// `crate::registry`'s `datahub_dial_keys` doc records that before it landed, nothing in
/// `vike-app-core` or `vike-desktop` called a hub-key constructor, `HistoryStoreDatahub::with_keys` or
/// `DatahubClient::connect_authed`, and "every dial from the desktop was UNAUTHENTICATED". The
/// MARKET-DATA half was then fixed — `market_data_session`'s dials go through `connect_authed` — and the
/// STORE half was not: both `HistoryStoreDatahub::new` call sites stayed on the key-less constructor,
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
/// why this helper has no "plain" arm to pick: `HistoryStoreDatahub::with_keys`' own contract is that
/// against a KEY-LESS server it degrades to an ordinary unauthenticated connect, "so one configured
/// GUI works against both a keyed production datahub and a local dev one". The `None` arm below is
/// reached only when this desktop holds NO key to offer, never as a preference.
/// ⚠ **The BODY moved to `vike_app_core::backend::registry::open_history_store_datahub`** — beside
/// `datahub_dial_keys`, whose resolution is the hard half — so the merge gate compiles and
/// tests it and so the chart's own store read
/// (`vike_app_core::data::stored`) dials identically without importing this binary. What is left
/// here is the two composition-root facts a library may not read for itself.
pub(crate) fn open_history_store_datahub(
    address: String,
) -> vike_datahub_client::HistoryStoreDatahub {
    vike_app_core::backend::registry::open_history_store_datahub(
        address,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
    )
}

/// **This PC's device key** (decision 0288), as the `HubKeys` every hub dial of Studio signs with — the
/// backtesthub's Read dials (the Named run, the strategy-file list) and its Write dials (Run, Sweep,
/// Walk-Forward, Save, Build) alike, the server's access list deciding which the device's role admits.
/// `open_history_store_datahub`'s twin, built from this binary's boot-walk `OnceLock` for the identical reason:
/// the resolution behind it opens the settings database's `device_key` table, whose directory only the
/// binary knows.
///
/// ⚠ **The notices are dropped here and that is a deliberate difference from `open_history_store_datahub`'s
/// caller.** `datahub_dial_keys` returns a `Vec<String>` of node-store notices, and
/// the store plane surfaces them because the store is where a missing key shows up as an empty
/// catalog somebody then debugs. A Studio dial cannot fail silently the same way: a dial against a keyed
/// backtesthub that refuses this device fails AT CONNECT with a message saying how to register it
/// (`crates/vike-studio/src/backend/backtesthub/mod.rs`'s `device_refusal_message`), straight into the Studio's
/// error banner. Re-surfacing the same notice twice per frame would be noise, and this is called
/// once per Studio construction.
pub(crate) fn studio_device_keys() -> Option<vike_hub_protocol::auth::HubKeys> {
    vike_app_core::backend::registry::datahub_dial_keys(
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
    )
    .0
}

/// The CHART-SEED dial, built from this binary's boot-walk `OnceLock` — `open_history_store_datahub`'s twin,
/// and here for the identical reason: the settings directory is a binary's fact, even though
/// everything it feeds lives one crate down in `vike_app_core::data::stored::chart_seed`.
pub(crate) fn chart_seed_dial(
    address: String,
    note: std::sync::Arc<std::sync::Mutex<Option<String>>>,
) -> vike_app_core::data::stored::chart_seed::ChartSeedDial {
    vike_app_core::data::stored::chart_seed::ChartSeedDial::resolve(
        address,
        SETTINGS_DIR.get().and_then(Option::as_deref).and_then(std::path::Path::to_str),
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
/// keys and saved into another's. `vike_account_manager::AccountManagerCredentialHome` is where that argument lives in
/// full, and it lives THERE rather than here because this crate is outside the CI roster: the
/// resolution is covered by that crate's own suite, and the shell keeps only the wiring.
///
/// `Proc::current` reads `current_exe`, so the whole thing is resolved once per process rather than
/// per write; the INSTANT is not cached, because the journal reads no clock and every record stamps
/// its own.
pub(crate) fn credential_home() -> &'static vike_account_manager::AccountManagerCredentialHome {
    static HOME: std::sync::OnceLock<vike_account_manager::AccountManagerCredentialHome> =
        std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        vike_account_manager::AccountManagerCredentialHome::resolve(
            SETTINGS_DIR.get().and_then(Option::as_ref).map(std::path::PathBuf::as_path),
            STATE_DIR.get().and_then(Option::as_ref).map(std::path::PathBuf::as_path),
            vike_change_journal::ChangeProcess::current(env!("CARGO_PKG_VERSION")),
        )
    })
}
