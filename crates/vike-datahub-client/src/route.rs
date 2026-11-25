//! `route` — WHERE a reader gets history from, decided once for everybody who asks.
//!
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` gave the hist store ONE reader — the
//! datahub — and everything else asks over the wire.
//!
//! # ⚠ There is ONE answer: the datahub
//!
//! The local arm (`--store DIR` read the files in place) closed on 2026-09-25: it opened the same
//! store layout but a second PATH, one that could point at a different DATASET (a stale copy,
//! another box's files), and the owner chose "impossible" over "attributable after the fact". The
//! cost is one process: [`history_route`] defaults to the datahub's loopback address, so the old
//! local run is `vike-backend datahub --store DIR` beside it, dialled with this box's own device key
//! (decision 0288 item 8: every hub authenticates).
//!
//! ⚠ **The ruling is about READERS.** The collectors and the `vike-datahub store` verbs that WRITE still
//! open the store through their own `--store` (closing those needs a wire write-verb per kind).
//! `vike-datahub store export` READS, so it takes its bars from [`open_routed_history`] like every other
//! reader and only encodes the file itself.
//!
//! # Why the decision lives in the CLIENT crate
//!
//! `vike-report` read history and could not name `vike-backtest` (both declare layer 30, and
//! `crates/vike-ops/tests/architecture/layer_gate.rs` refuses a same-rank edge; the `tearsheet` CLI
//! that called this has since moved up to `crates/vike-backend`, which can name both), so the choice was a
//! second spelling or a shared home BELOW both — and *when two sides must not disagree, the cure is
//! a shared crate BELOW both*. This crate is below every reader and owns the body outright
//! ([`crate::HistoryStoreDatahub`] and [`routed_hub_keys`]).
//!
//! # The feature
//!
//! The module sits behind `hist-route`, off by default, because the address row and the hub-key
//! read's result name `vike-secrets` and the compiled default address names `vike-config`, and
//! nine crates take this one. It forwards no DataFusion feature, so enabling it links no Arrow/DataFusion tree.
//!
//! # ⚠ This module's tests do not live here, and a test ADDED here would not run
//!
//! They live in `crates/vike-backtesthub/src/backtest_cli/tests.rs`'s `history_route`, run by the
//! roster `test` job's run of vike-backtesthub.
//! The roster lane builds this crate with DEFAULT features, so it compiles none of this file, and
//! the lanes that enable `hist-route` reach it as a DEPENDENCY, where cargo compiles no
//! `#[cfg(test)]` module. There is no `-p vike-datahub-client --features hist-route` lane, so a
//! `#[cfg(test)] mod` added below would read green by never running. Add the lane first, or put
//! the test beside the caller that proves the behaviour.

use vike_data::HistoryStore;

/// WHERE a reader gets history from: the datahub at one address, plus the one rendering of it every
/// caller shares. (A struct: with the local arm closed, an enum would describe a choice that no
/// longer exists.)
///
/// ⚠ **The environment does not choose between datahubs.** [`history_route`] takes the configured
/// address and nothing else; the store root is the DATAHUB's own setting, which no reader consults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRoute {
    hub: String,
}

impl HistoryRoute {
    /// The address a reader dials.
    pub fn hub(&self) -> &str {
        &self.hub
    }

    /// What to PRINT, and what a run RECORDS as its data's provenance: the datahub's ADDRESS.
    ///
    /// ⚠ It never names a directory: the resolved root is the SERVER's, so a path printed
    /// client-side would be a guess about another box's filesystem.
    pub fn label(&self) -> String {
        format!("the datahub at {}", self.hub)
    }
}

/// The address a BARE BIN dials: the `config.datahub_addr` row of the settings database `vars`
/// resolve (`VIKE_SETTINGS_DIR`, else the project walk) — the database these bins already read
/// for this box's device key (handed to [`routed_hub_keys`]), and the row the backtesthub
/// reads through its settings load, so a box has ONE answer to "where is my datahub". No environment variable is
/// consulted (decision 0111 retired `VIKE_DATAHUB_ADDR`).
///
/// ⚠ A ROW READ, not a settings load: a bin boots nothing, and this one key is all it needs. No
/// database or no row answers `None`, which [`history_route`] turns into
/// [`vike_config::DEFAULT_DATAHUB_ADDRESS`]; a row that is not a JSON string is named on stderr and
/// answers `None` too (the daemon's loader refuses such a row at its boot; a bin reports and
/// carries on, as it does for an unreadable credential store). The BLANK case reaches
/// [`history_route`], which owns that filter for every caller.
pub fn datahub_address_for_bin(vars: &std::collections::HashMap<String, String>) -> Option<String> {
    let dir = vike_secrets::workspace_settings_dir_from(
        vars.get("VIKE_SETTINGS_DIR").map(String::as_str),
    );
    let source = vike_secrets::read_settings_in(&dir).ok()?;
    let row = source
        .rows()?
        .settings
        .iter()
        .find(|r| r.section == "config" && r.key == "datahub_addr")?;
    match serde_json::from_str::<String>(&row.value) {
        Ok(addr) => Some(addr),
        Err(e) => {
            eprintln!(
                "the config.datahub_addr row in {} is not a string ({e}) — dialling the default \
                 datahub instead. Rewrite it with `vike-cli config set config.datahub_addr \
                 HOST:PORT`",
                dir.display()
            );
            None
        }
    }
}

/// Resolve [`HistoryRoute`] from the configured address.
///
/// A BLANK configured address is treated as absent rather than dialled — the same rule
/// `vike_config` applies everywhere a string names a peer, so an empty settings line cannot produce
/// a connect to `""`. Absent falls back to [`vike_config::DEFAULT_DATAHUB_ADDRESS`], a LOOPBACK
/// address, which is exactly where a key-less local datahub binds.
///
/// ⚠ There is no `local_store` parameter, deliberately: an ignored one would let a reader that
/// still held a `--store` value compile and quietly read over the wire, and the operator would
/// believe their local directory had been read.
pub fn history_route(datahub_addr: Option<&str>) -> HistoryRoute {
    HistoryRoute {
        hub: datahub_addr
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(vike_config::DEFAULT_DATAHUB_ADDRESS)
            .to_string(),
    }
}

/// **What a BIN dials the datahub with, out of the device-key read the bin made** — the
/// warn-and-continue disposition every history-route reader takes, spelled once. It reads nothing:
/// the caller made the read (`vike_secrets::access::device_key_load` or `device_key_named` over its own
/// settings directory) and hands the RESULT in, and `hub` is only what the warning names. A bin signs with its
/// DEVICE key (decision 0288 item 8): no shared datahub pair is read.
///
/// ⚠ **An UNREADABLE store WARNS and continues rather than refusing** — the opposite of
/// `crates/vike-backtesthub/src/backtest_cli/serve.rs`'s `run_serve`, and deliberately: a server
/// decides from that read whether it may bind at all, while a bin has no listener, and against a
/// KEY-LESS local datahub (the ONLY way to read local files) an unreadable store is harmless, so
/// refusing would block the one working configuration. The warning names the cause. A caller with
/// a stricter disposition does not call this.
pub fn routed_hub_keys(
    read: Result<Option<vike_secrets::access::DeviceKeyRow>, vike_secrets::StoreError>,
    hub: &str,
) -> Option<vike_hub_protocol::auth::HubKeys> {
    let unauthenticated = |why: &dyn std::fmt::Display| {
        eprintln!(
            "{why} — this run will connect to {hub} UNAUTHENTICATED. Against a keyed datahub that \
             connect is refused, and the cause is this box's device key rather than the datahub: \
             fix it and re-run"
        );
        None
    };
    match read {
        Ok(None) => None,
        Ok(Some(row)) => match vike_hub_protocol::access::DeviceKey::from_pkcs8(&row.private_key) {
            Ok(device) => Some(vike_hub_protocol::auth::HubKeys::of_device(device)),
            Err(e) => {
                unauthenticated(&format!("the device key {} is not a usable key ({e})", row.key_id))
            }
        },
        Err(e) => unauthenticated(&format!("settings database PRESENT but UNREADABLE ({e})")),
    }
}

/// Open the history the route names — the BINS' half of `0084`, and the one call that turns a
/// [`HistoryRoute`] into something with `scan_*` on it.
///
/// A LIBRARY call: it reads no store and no environment. The ADDRESS is the route's and the node
/// `keys` are the caller's, resolved by the caller under its own unreadable-store disposition
/// ([`routed_hub_keys`] is the warn-and-continue one every bin takes).
///
/// ⚠ **Infallible**: nothing connects until the first read, so a connect failure surfaces at that
/// read, naming the address, which is where it can be acted on.
///
/// ⚠ **Every `scan_*` is a network read**, and these callers scan a whole series per entry with
/// `TimestampRange::all()`. That is the cost `0084` buys the single-reader property with.
pub fn open_routed_history(
    route: &HistoryRoute,
    keys: Option<vike_hub_protocol::auth::HubKeys>,
) -> Box<dyn HistoryStore + Send + Sync> {
    let hub = route.hub();
    // `with_keys` whenever a pair resolved (it degrades to a plain connect against a KEY-LESS
    // server), `new` only when none did — the choice `run_serve` makes.
    match keys {
        Some(k) => Box::new(crate::HistoryStoreDatahub::with_keys(hub, k)),
        None => Box::new(crate::HistoryStoreDatahub::new(hub)),
    }
}
