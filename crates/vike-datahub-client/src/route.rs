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
//! cost is one process and no keys: with no node keys `vike-backend datahub` authenticates nothing
//! and binds loopback only, and [`history_route`] defaults to that loopback address, so the old
//! local run is `VIKE_DATAHUB_STORE=DIR vike-backend datahub` beside it.
//!
//! ⚠ **The ruling is about READERS.** The collectors and the backtest `data` verbs that WRITE still
//! open the store through their own `--store` (closing those needs a wire write-verb per kind).
//! `backtest data export` READS, so it takes its bars from [`open_routed_history`] like every other
//! reader and only encodes the file itself.
//!
//! # Why the decision lives in the CLIENT crate
//!
//! `vike-report` reads history and cannot name `vike-backtest` (both declare layer 30, and
//! `crates/vike-ops/tests/architecture/layer_gate.rs` refuses a same-rank edge), so the choice was a
//! second spelling or a shared home BELOW both — and *when two sides must not disagree, the cure is
//! a shared crate BELOW both*. This crate is below every reader and owns the body outright
//! ([`crate::RemoteHistStore`] and [`vike_node_proto::auth::node_keys_from_vars`]).
//!
//! # The feature
//!
//! The module sits behind `hist-route`, off by default, because resolving node keys names
//! `vike-secrets` and the compiled default address names `vike-config`, and nine crates take this
//! one. It forwards no DataFusion feature, so enabling it links no Arrow/DataFusion tree.
//!
//! # ⚠ This module's tests do not live here, and a test ADDED here would not run
//!
//! They live in `crates/vike-backtest/src/backtest_cli/tests.rs`'s `history_route`, run by the
//! `backtest-hist-replay` lane's `--features datafusion-store` half (`scripts/ci_feature_suite.sh`).
//! The roster lane builds this crate with DEFAULT features, so it compiles none of this file, and
//! the lanes that enable `hist-route` reach it as a DEPENDENCY, where cargo compiles no
//! `#[cfg(test)]` module. There is no `-p vike-datahub-client --features hist-route` lane, so a
//! `#[cfg(test)] mod` added below would read green by never running. Add the lane first, or put
//! the test beside the caller that proves the behaviour.

use vike_data::HistStore;

/// WHERE a reader gets history from: the datahub at one address, plus the one rendering of it every
/// caller shares. (A struct: with the local arm closed, an enum would describe a choice that no
/// longer exists.)
///
/// ⚠ **The environment does not choose between datahubs.** [`history_route`] takes the configured
/// address and nothing else; `VIKE_HIST_STORE` is the DATAHUB's fallback store variable, which no
/// reader consults.
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

/// `VIKE_DATAHUB_ADDR` — where the bins dial.
///
/// ⚠ **Spelled here instead of imported from `vike_config`, and that is not duplication.**
/// `vike_model::scan` resolves constants CRATE-WIDE, so a crate that imports the name goes
/// INVISIBLE to the settings-registry sweep and passes `every_read_variable_is_declared` by
/// BLINDNESS. An equality assertion pays for the second spelling:
/// `crates/vike-backtest/src/backtest_cli/tests/history_route.rs`'s
/// `datahub_addr_env_matches_the_config_crate`.
pub const DATAHUB_ADDR_ENV: &str = "VIKE_DATAHUB_ADDR";

/// The address rung available to a BARE BIN — the environment, and nothing above it.
///
/// ⚠ Deliberately SHORTER than the compute daemon's ladder, which starts at
/// `settings.config.datahub_addr`: a `cheap_np` bin loads no settings file, and inventing one would
/// give two answers to "where is my datahub" on one box. Both feed the SAME [`history_route`],
/// which owns the blank-value filter and [`vike_config::DEFAULT_DATAHUB_ADDR`].
pub fn datahub_addr_for_bin(vars: &std::collections::HashMap<String, String>) -> Option<&str> {
    vars.get(DATAHUB_ADDR_ENV).map(String::as_str)
}

/// Resolve [`HistoryRoute`] from the configured address.
///
/// A BLANK configured address is treated as absent rather than dialled — the same rule
/// `vike_config` applies everywhere a string names a peer, so an empty settings line cannot produce
/// a connect to `""`. Absent falls back to [`vike_config::DEFAULT_DATAHUB_ADDR`], a LOOPBACK
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
            .unwrap_or(vike_config::DEFAULT_DATAHUB_ADDR)
            .to_string(),
    }
}

/// Open the history the route names — the BINS' half of `0084`, and the one call that turns a
/// [`HistoryRoute`] into something with `scan_*` on it.
///
/// ⚠ **Infallible**: nothing connects until the first read, so a connect failure surfaces at that
/// read, naming the address, which is where it can be acted on.
///
/// ⚠ **Node keys come from the credential STORE, not the environment**, through the same
/// `resolve_node_keys` + `is_datahub_node_key` pair that
/// `crates/vike-backtest/src/backtest_cli/serve.rs`'s `run_serve` uses: the datahub pair is a
/// settings-database row, so an env read would resolve `None`, connect unauthenticated, and be
/// refused with a message about auth rather than about where the keys were looked for.
///
/// ⚠ **An UNREADABLE store WARNS and continues rather than refusing** — the opposite of `run_serve`,
/// and deliberately: a server decides from that read whether it may bind at all, while a bin has
/// no listener, and against a KEY-LESS local datahub (the ONLY way to read local files) an
/// unreadable store is harmless, so refusing would block the one working configuration. The
/// warning names the cause.
///
/// ⚠ **Every `scan_*` is a network read**, and these callers scan a whole series per entry with
/// `TsRange::all()`. That is the cost `0084` buys the single-reader property with.
pub fn open_routed_history(
    route: &HistoryRoute,
    vars: &std::collections::HashMap<String, String>,
) -> Box<dyn HistStore + Send + Sync> {
    let hub = route.hub();
    let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
    let keys = match vike_secrets::resolve_node_keys(
        settings_override,
        vike_model::credential_keys::is_datahub_node_key,
    ) {
        Ok(resolved) => vike_node_proto::auth::node_keys_from_vars(&resolved.secrets.into_map()),
        Err(e) => {
            eprintln!(
                "credential store PRESENT but UNREADABLE ({e}) — any configured datahub node keys \
                 were NOT loaded, so this run will connect to {hub} UNAUTHENTICATED. Against a keyed \
                 datahub that connect is refused, and the cause is that store rather than the keys: \
                 fix its permissions and re-run"
            );
            None
        }
    };
    // `with_keys` whenever a pair resolved (it degrades to a plain connect against a KEY-LESS
    // server), `new` only when none did — the choice `run_serve` makes.
    match keys {
        Some(k) => Box::new(crate::RemoteHistStore::with_keys(hub, k)),
        None => Box::new(crate::RemoteHistStore::new(hub)),
    }
}
