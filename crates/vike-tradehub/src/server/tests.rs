//! The node server's unit tests: one child per suite, each reaching `server` items through `use super::*`.
use super::*;

/// The REQ-2 datahub advertisement in `Welcome.features` — served ONLY when configured, and
/// readable by the client-side parser it is spelled for (the round-trip's server half; the
/// client half is pinned in `vike_tradehub_client::proto`'s own tests).
#[cfg(test)]
mod served_features;

/// [`tearsheet_reply`] — every way the LIVE-JOURNAL report verb can answer, decided without a
/// socket. The shape under test is the HONESTY rule the retired IOU arm followed: each failure
/// names its own cause, and in particular "this node has no journal" never wears the words of
/// "this build cannot serve the verb".
#[cfg(test)]
mod tearsheet_reply;

/// The REACHABILITY half of this module's contract — the three properties that hold up "auth is
/// defense in depth, the tunnel is the barrier" (see the module doc). Each is stated as a test
/// because each is one edit away from silently evaporating.
#[cfg(test)]
mod exposure;

#[cfg(test)]
mod control_limits;

#[cfg(test)]
mod venue_refusal;

#[cfg(test)]
mod account_refusal;

/// The un-latch of a mount's budget at the control door (decision 0239): [`plan_unlatch`](super::unlatch::plan_unlatch)
/// against the published mount rows, and the verb through `accept_command` into a real core.
#[cfg(test)]
mod unlatch_door;

/// `coid_reuse_refusal` — a `Submit` whose client order id an order of this node already carries is
/// refused in words before the Ack, for every status.
#[cfg(test)]
mod coid_reuse_refusal;

/// The closed-market NOTE of the door's answer (decision 0236): an order into a closed market is still sent,
/// and the answer says so in one sentence; the New York daylight-saving boundaries are pinned on both sides.
#[cfg(test)]
mod session_note_door;

/// A post-only limit order whose price would cross the best bid or ask the node's price tap holds is refused by
/// name at the door (decision 0242, item 8), on every venue that has a post-only order; no book held, a plain
/// limit order, a `paper` book and a venue with no post-only order are not this check's.
#[cfg(test)]
mod post_only_crossing;

#[cfg(test)]
mod lower_command;

#[cfg(test)]
mod admits_any_symbol;

/// The two per-symbol money facts an admission carries (the symbol's own leverage, and, for a symbol
/// quoted in another currency than the account's, the rate that converts it), through a real core.
#[cfg(test)]
mod symbol_money_door;

/// IG's twin of the above: a foreign-quoted epic at IG's contract size (10,000 units), through a real core.
#[cfg(test)]
mod ig_money_door;

/// The same for an IBKR STOCK quoted in another currency than the account's (a live forex quote is the rate), and
/// for a forex pair and a dated future, with an option refused by name.
#[cfg(test)]
mod ibkr_money_door;

/// An IBKR stock whose stream is only DELAYED, through a real core: admitted, priced and recorded as delayed
/// (decision 0140), where the tap used to refuse it by name.
#[cfg(test)]
mod ibkr_delayed_price_door;

/// The same for dukascopy's pair quoted in another money than the account's: the fence of decision 0170
/// relaxed only for an account that converts, and only with the currency and the rate the facts state.
#[cfg(test)]
mod dukascopy_money_door;

/// The same planner under random input: the laws (`plan_submit_symbol` is a pure function of the command,
/// the published block and the facts' answers) that `admits_any_symbol`'s rows only sample.
#[cfg(test)]
mod symbol_admission_props;

#[cfg(test)]
mod bybit_door;
mod hyperliquid_door;

/// Binance's lane-suffixed spelling of the classes beyond a plain perpetual: a TradFi perpetual, a dated future
/// (`BTCUSDT_261225.P`), a COIN-M name let through for the facts to refuse, and an options symbol refused by name.
#[cfg(test)]
mod binance_class_door;

/// Polymarket's row: an outcome token id, or a market name the facts source resolves to one, on an engine mounted
/// on no symbol.
#[cfg(test)]
mod outcome_token_door;

// The LINK-LIVENESS suite: a real paper node on a real socket, driven through the crate-private
// `LinkPolicy` seam so the five-minute and fifteen-second properties are proven at ~1000x scale
// rather than in five minutes. A child module rather than an integration test because that seam is
// deliberately not public — see the file's own doc, and `crates/vike-cli/src/cmd/
// mcp_node_drop_tests.rs` for the same trade made for the same reason. The `#[cfg(test)]` on the
// declaration is what the src-walking gates read: they resolve the file the way rustc does,
// `#[path]` included (`vike_model::libm_walk::cfg_test_module_rel_files`), and classify it as TEST
// code, so the module name need not equal the file stem.
#[cfg(test)]
mod server_link_liveness;

// ---------------------------------------------------------------------------------------------
// THE ACCOUNT PLANE'S CEREMONY AND ADVERTISEMENT (`docs/decisions/0065-accounts-are-managed-and-
// the-barrier-is-declared.md`).
//
// ⚠ Every test below hands the source a settings directory that DOES NOT EXIST, and that is the
// assertion rather than a shortcut: the ceremony is decided BEFORE the store is opened —
// `apply_set_setting`'s ordering, so a refused ceremony costs no lock and cannot be distinguished
// from an accepted one by timing what the store did. A refusal that reached the store first would
// fail here by returning the store's message instead of the ceremony's.
// ---------------------------------------------------------------------------------------------
#[cfg(test)]
mod account_plane;

/// [`account_admission`] — the AUTHORIZATION half of `docs/decisions/0065`'s barrier, driven over
/// every scope the wire has.
///
/// ⚠ **This module exists because a mutation proved the decision was unratcheted.** On 2026-09-17
/// the accepting arm was widened to `(Some(src), _)` and the scope refusal deleted — any
/// authenticated peer, Observe included, reaching `SetCredential` — and `cargo nextest run` over
/// `vike-tradehub`, `vike-tradehub-client`, `vike-ops`, `vike-cli`, `vike-gateway-panel` and
/// `vike-secrets` reported **2911 passed**. Nothing looked. The decision could not be driven from a
/// test at all while it lived inside the `Request::Account` arm, because `Request::Account` has one
/// construction site in the tree and it is that arm; every account test called
/// `AccountAdminSource::apply` directly, BELOW the check.
///
/// So the assertions below are deliberately exhaustive over `Scope` rather than a happy-path pair:
/// a fourth variant must redden this file rather than silently inherit whichever branch it falls in.
#[cfg(test)]
mod account_admission;

/// **The handshake's closed-gate check** — [`handshake::scope_admission`], the FIRST of the account
/// boundary's two layers (`docs/decisions/0070`).
///
/// ⚠ These tests did not exist before the function did, and could not: the check lived inside
/// `run_handshake`, which takes a `&mut TcpStream`. Measured on `main` at the time — nothing under
/// `crates/vike-tradehub/tests/` named `run_handshake` or `Scope::Account` at all, so the one check
/// between a peer CLAIMING admin and the account plane was covered by nothing.
#[cfg(test)]
mod scope_admission;
