//! Shared Binance-wire-grammar exec-dispatch helpers reused by vike-aster (rung 4c).
//!
//! The two venues' `execution/mod.rs` drove the command→REST→ingest loop with byte-identical code: the
//! `.P`-suffix symbol split and the `run_loop` command pump were verbatim copies (the only textual
//! diffs were doc comments). The pump turned out not to be family-specific at all — deribit/bybit/
//! okx carried the SAME verbatim copy — so `run_loop` was hoisted one layer further down, into
//! [`vike_bridge_core::exec_actor::run_loop`] (it names only shared-crate types: `ExecutionClientActorCommand`/
//! `cancel_event`/`ExecutionClientActorCancelOutcome` from `vike_bridge_core::exec_actor`, `VenueRest` from
//! `vike_bridge_core::rest`, `EventSender` from vike-execution); all four venues now call that ONE copy.
//! What stays here is the family-local slice: `split_symbol` (the `.P`-suffix split — binance/aster
//! wire grammar, not venue-neutral) and `lane_symbol` (the one-lane refusal both venues' per-symbol
//! factories ask, in each venue's own words). The `run_spot`/`run_perp` drivers, the confirm, the
//! foreign-symbol factories, the grid fetch and the history replay are `crate::family::execution`'s
//! since the owner's ruling of 2026-11-13, over each venue's `FamilyExecutionSpec`; each venue's
//! `execution/mod.rs` keeps its `run`, its actor, its client builds (`mk_spot_rest`/`mk_perp_rest`, which name
//! its REST types, hosts and signers) and its facts for the plane.

/// Split a core symbol into its EXCHANGE symbol + perp flag: `BTCUSDT.P` → (`BTCUSDT`, true),
/// `BTCUSDT` → (`BTCUSDT`, false).
///
/// An owning wrapper over [`vike_venues::split_perp`] — the ONE definition of that split — kept
/// because this crate's exec callers want `String`s they can move into per-order state.
pub fn split_symbol(symbol: &str) -> (String, bool) {
    let (api, perp) = vike_venues::split_perp(symbol);
    (api.to_string(), perp)
}

/// The exchange symbol of an order on `label`, or why this account's lane cannot take it, in words
/// that open with `venue` (`"binance"`, `"aster"`). An engine runs ONE lane (spot or USDⓈ-M perp, by
/// the mounted symbol's `.P`), with ONE user-data stream: an order on the OTHER lane would be signed
/// but its fills would never arrive, so it is refused here rather than traded blind. (Two lanes in one
/// engine is phase 1c of the orders-follow-the-symbol spec.)
pub fn lane_symbol(venue: &str, label: &str, want_perp: bool) -> Result<String, String> {
    let (api_symbol, is_perp) = split_symbol(label);
    if is_perp == want_perp {
        return Ok(api_symbol.to_uppercase());
    }
    let (this, other) = if want_perp { ("perp", "spot") } else { ("spot", "perp") };
    Err(format!(
        "{venue}: cannot sign {label:?}: this account's engine runs the {this} lane and {label:?} \
         is a {other} symbol; the {other} lane has no user-data stream here, so its fills would \
         never arrive. The order was not sent"
    ))
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod dispatch_tests;
