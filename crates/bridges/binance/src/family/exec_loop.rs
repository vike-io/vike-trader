//! Shared Binance-wire-grammar exec-dispatch helpers reused by vike-aster (rung 4c).
//!
//! The two venues' `exec.rs` drove the command→REST→ingest loop with byte-identical code: the
//! `.P`-suffix symbol split and the `run_loop` command pump were verbatim copies (the only textual
//! diffs were doc comments). The pump turned out not to be family-specific at all — deribit/bybit/
//! okx carried the SAME verbatim copy — so `run_loop` was hoisted one layer further down, into
//! [`vike_bridge_core::exec_actor::run_loop`] (it names only shared-crate types: `ExecCommand`/
//! `cancel_event`/`CancelOutcome` from `vike_bridge_core::exec_actor`, `VenueRest` from
//! `vike_bridge_core::rest`, `EventSender` from vike-exec); all four venues now call that ONE copy.
//! What stays here is the family-local slice: `split_symbol` (the `.P`-suffix split — binance/aster
//! wire grammar, not venue-neutral). Each venue's `exec.rs` keeps its own `run`/`run_spot`/
//! `run_perp` drivers (which name venue REST types, hosts, signers, and `tracing` targets).
//!
//! Deliberately NOT hoisted (they name per-venue REST types / hosts / signers / log targets):
//! `mk_spot_rest`/`mk_perp_rest`, `fetch_*_properties`, `spot_history_events`/`perp_history_events`,
//! and the `run`/`run_spot`/`run_perp` drivers themselves.

/// Split a core symbol into its EXCHANGE symbol + perp flag: `BTCUSDT.P` → (`BTCUSDT`, true),
/// `BTCUSDT` → (`BTCUSDT`, false).
///
/// An owning wrapper over [`vike_catalog::split_perp`] — the ONE definition of that split — kept
/// because this crate's exec callers want `String`s they can move into per-order state.
pub fn split_symbol(symbol: &str) -> (String, bool) {
    let (api, perp) = vike_catalog::split_perp(symbol);
    (api.to_string(), perp)
}
