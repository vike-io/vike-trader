//! `route` — WHERE a reader gets history from, decided once for everybody who asks.
//!
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` gave the hist store ONE reader — the
//! datahub — and everything else asks over the wire.
//!
//! # ⚠ There is ONE answer now, and there used to be two
//!
//! Until 2026-09-25 this module resolved a two-armed question: the datahub over the wire, or the
//! files in place when `--store DIR` was on the line. The owner closed the second arm that day,
//! choosing "impossible" over "visible after the fact". The local arm had not created a second
//! STORE — it opened the same `DataFusionHist` with the same layout — but a second PATH, one that
//! could point at a different DATASET: a stale copy, another box's files. The run record already
//! named the source (`DataFingerprint.store` held a path on a local run and the datahub's address
//! on a routed one), so a divergence was attributable after the fact; the ruling made it
//! impossible instead, at the stated cost that nothing reads history without a datahub running.
//!
//! **That cost is one process and no keys.** MEASURED on the day: `vike-backend datahub` takes its
//! root from `VIKE_DATAHUB_STORE`, and with no node keys configured it authenticates nothing and
//! CONFINES itself to a loopback bind by construction — while [`history_route`] defaults to that
//! same loopback address. So the local run that `--store DIR` used to be is now
//! `VIKE_DATAHUB_STORE=DIR vike-backend datahub`, beside it, with nothing to mint.
//!
//! ⚠ **The ruling was about READERS.** The collectors and the backtest `data` verbs that WRITE
//! still open the store through their own `--store`; closing those would need a wire write-verb
//! per kind, which is a separate decision nobody has made. `backtest data export` is NOT one of
//! them: it READS a series out to a file, and since 2026-09-26 it takes its bars from
//! [`open_routed_history`] like every other reader and only encodes the file itself.
//!
//! # ⚠ Why the decision lives in the CLIENT crate
//!
//! It was `crates/vike-backtest/src/backtest_cli.rs`'s until 2026-09-23, because the compute daemon
//! and the three `cheap_np` bins were the only callers and they are all that crate's. `vike-report`
//! is a fourth reader, and it cannot name `vike-backtest`: both declare layer 30 and
//! `crates/vike-ops/tests/arch/layer_gate.rs` refuses a same-rank edge. So the choice was a second
//! spelling or a shared home BELOW both — and `CLAUDE.md` states the rule outright: *when two sides
//! must not disagree, the cure is a shared crate BELOW both, not a shared crate containing both*.
//! This crate is below every reader already and owns the body outright ([`crate::RemoteHistStore`]
//! and [`vike_node_proto::auth::node_keys_from_vars`]).
//!
//! # ⚠ The feature, and what closing the local arm took out of it
//!
//! The module sits behind `hist-route`, off by default, because resolving node keys names
//! `vike-secrets` and the compiled default address names `vike-config`, and nine crates take this
//! one — including `vike-studio`, whose `studio-standalone` lane (`scripts/ci_feature_suite.sh`)
//! asserts STRUCTURALLY that a default Studio build carries no datafusion crate.
//!
//! It USED to forward `vike-data/hist-datafusion` as well, and that was the local arm's cost: it
//! opened a `DataFusionHist`, so every crate that routed was made to link the Arrow/DataFusion tree
//! whether or not it ever read a local file. With the arm gone the forward is gone, and it is not
//! tidying: MEASURED 2026-09-25, `vike-report`'s tearsheet and `vike-studio-core`'s `study` verb
//! have NO other production use of DataFusion — every other site in either crate is a test — so
//! both reader paths stop linking it. `vike-backtest` keeps it, because its `data` verbs write.
//!
//! # ⚠ This module's tests do not live here, and a test ADDED here would not run
//!
//! The ones that cover the decision live in `crates/vike-backtest/src/backtest_cli.rs`'s
//! `history_route_tests`, where the `backtest-datafusion-store` lane
//! (`scripts/ci_feature_suite.sh`) executes them on every PR. The roster lane builds this crate with
//! DEFAULT features, so it compiles none of this file, and the lanes that do enable `hist-route`
//! reach it as a DEPENDENCY, where cargo compiles no `#[cfg(test)]` module at all. There is no
//! `-p vike-datahub-client --features hist-route` lane today, so a `#[cfg(test)] mod` added below
//! would read green by never running. Add the lane first, or put the test beside the caller that
//! proves the behaviour.

use vike_data::HistStore;

/// WHERE a reader gets history from: the datahub at one address.
///
/// ⚠ **A struct, not the enum it was.** It was `enum HistoryRoute { Local, Wire(String) }` until
/// 2026-09-25; with the local arm closed an enum would have ONE variant, and a one-variant enum
/// describes a choice that no longer exists. What survives is the address, plus the one rendering of
/// it every caller shares.
///
/// ⚠ **The environment does not choose between datahubs either.** [`history_route`] takes the
/// configured address and nothing else. `VIKE_HIST_STORE` is the DATAHUB's fallback store variable
/// and no reader consults it to decide where history comes from — which is why the deployed compute
/// unit, which set it until 2026-09-26, no longer does.
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
    /// ⚠ It never names a directory. The resolved root is the SERVER's, so a path printed
    /// client-side is a guess about another box's filesystem. Until 2026-09-25 this took a
    /// `local_root` so the local arm could print a path; there is no local arm, so there is no path
    /// a reader can honestly name, and the parameter went with it.
    pub fn label(&self) -> String {
        format!("the datahub at {}", self.hub)
    }
}

/// `VIKE_DATAHUB_ADDR` — where the bins dial.
///
/// ⚠ **This crate spells its own constant instead of importing `vike_config`'s, and that is not
/// duplication.** `vike_ops::scan` resolves constants CRATE-WIDE, so a crate that imports the name
/// from another crate goes INVISIBLE to the settings-registry sweep and its read then passes
/// `every_read_variable_is_declared` by BLINDNESS rather than by declaration. The price of the
/// second spelling is the equality assertion that pays for it —
/// `datahub_addr_env_matches_the_config_crate` below, the shape
/// `crates/vike-app-core/src/backend/backend_registry_tests.rs`'s
/// `observe_and_control_key_names_match_the_client` set.
pub const DATAHUB_ADDR_ENV: &str = "VIKE_DATAHUB_ADDR";

/// The address rung available to a BARE BIN — the environment, and nothing above it.
///
/// ⚠ It is deliberately SHORTER than the compute daemon's ladder, which starts at
/// `settings.config.datahub_addr`. A `cheap_np` bin loads no settings file at all, so that rung is
/// not one it can reach, and inventing a settings load for it would give two answers to "where is
/// my datahub" on one box. Both feed the SAME [`history_route`], which owns the blank-value filter
/// and [`vike_config::DEFAULT_DATAHUB_ADDR`]: one decision, two sources.
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
/// ⚠ This took `local_store: Option<&Path>` first until 2026-09-25, and ANY `Some` short-circuited
/// to the local arm. The parameter is gone rather than ignored: a reader that still had a
/// `--store` value in hand and passed it here would otherwise compile and quietly read over the
/// wire, and the operator would believe their local directory had been read.
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
/// ⚠ **Infallible, and that is the local arm's absence showing.** It returned
/// `Result<_, String>` until 2026-09-25 because opening a local directory could fail; building a
/// remote client cannot, since nothing connects until the first read. A connect failure therefore
/// surfaces at that read, naming the address, which is where it can be acted on.
///
/// ⚠ **Node keys come from the credential STORE, not the environment**, through the same
/// `resolve_node_keys` + `is_datahub_node_key` pair that
/// `crates/vike-backtest/src/backtest_cli.rs`'s `run_serve` uses. An env-only read was the
/// tempting shape for a bare research bin and it is the wrong one HERE: both boxes migrated to the
/// settings database on 2026-09-14, so the datahub pair is a row rather than a shell export, and
/// an env read would resolve `None`, connect unauthenticated, and be refused by the keyed server
/// with a message about auth rather than about where the keys were looked for.
///
/// ⚠ **An UNREADABLE store WARNS and continues rather than refusing.** That is the opposite of
/// the disposition of `crates/vike-backtest/src/backtest_cli.rs`'s `run_serve`, and the difference
/// is real: a server decides from that read whether it may bind at all, while a bin has no
/// listener and no such decision — and against a KEY-LESS local datahub, which is now the ONLY way
/// to read local files, an unreadable store is harmless, so refusing would block the one working
/// configuration. The warning names the cause, which is the half that would otherwise be lost.
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
        Ok((resolved, _source)) => {
            vike_node_proto::auth::node_keys_from_vars(&resolved.secrets.into_map())
        }
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
    // `with_keys` whenever a pair resolved, `new` only when none did — `with_keys` degrades to a
    // plain unauthenticated connect against a KEY-LESS server, so one spelling works against the
    // keyed production datahub and a bare local one. `run_serve` chooses the same way.
    match keys {
        Some(k) => Box::new(crate::RemoteHistStore::with_keys(hub, k)),
        None => Box::new(crate::RemoteHistStore::new(hub)),
    }
}
