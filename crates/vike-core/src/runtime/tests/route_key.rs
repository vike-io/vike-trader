//! Routing by `ExecutionEngine::route_key` — the gate over the split that made two accounts of one
//! venue expressible in one process.
//!
//! # What was wrong
//!
//! [`CoreThread::engine_idx_for_route_key`] used to compare against `ExecutionEngine::venue`, the
//! same field every per-venue capability table is keyed on (`vike_model::caps_for`,
//! `amend_semantics`, `fee_schedule_for`, …). One field, two incompatible jobs — so a second
//! account of an exchange had no spelling:
//!
//! * label both engines `"binance"` and this function returns the FIRST match forever. The second
//!   engine is unreachable for fills, both accounts fold into ONE book, and reconcile compounds it:
//!   under `hybrid` `PositionDrift` auto-applies, so each pass rewrites the local position onto
//!   whichever account answered last.
//! * label the second `"binance#2"` and routing works while every capability lookup misses — the
//!   trap `crates/vike-exec/tests/engine/route_key.rs` gates from the other side.
//!
//! # …and the two ROUND TRIPS the split left open
//!
//! Splitting the engine's field did not split the strings that reach it. Two chains carried a
//! CANONICAL venue out of the core and handed it back as a ROUTING key, and both are gated below:
//!
//! * **The reconcile round trip.** `ReconcileReports` carried one venue string;
//!   `CoreThread::reconcile_reports` routed on it, then STORED it on `HeldReconAlert` and routed on
//!   it AGAIN in `confirm_recon` when an operator approved the held events. The payload now carries
//!   `route_key` beside `venue`, the held record keeps both, and the sections below drive each leg
//!   with the two facts different.
//! * **The order-payload double load.** `OrderRequest::venue` is what `engine_idx_for_route_key`
//!   routes on AND what `vike_model::preflight_order` selected the capability row by — and a
//!   non-roster string does not fail closed there, it returns `Ok(())`. `CoreThread::caps_venue`
//!   now asks the ROUTED ENGINE for its canonical venue instead of asking the payload.
//!
//! # White-box, and why
//!
//! `engine_idx_for_route_key`, `caps_venue`, `route_event`, `publish_to`, `reconcile_reports`,
//! `confirm_recon`, `apply_intent` and the `recon_alerts` store are all private to this module, and
//! they are precisely what is under test — composing `route_event` with `publish_to` is exactly
//! what the `Ingest::Event` dispatch arm does inline, and `reconcile_reports`/`confirm_recon` are
//! what the `Command::ReconcileReports`/`ConfirmRecon` arms call. The `use super::*`
//! sibling-test-module idiom of `crates/vike-core/src/runtime/tests/safe_state.rs`/
//! `crates/vike-core/src/runtime/tests/multi_mount.rs`.

use super::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate};
use vike_model::RiskLimits;
use vike_model::events::{FillEvent, TradeId};

// The runtime's shared white-box assembly; every core here passes `CoreConfig::default()`.
use crate::runtime::test_support::core_of;

/// The canonical venue BOTH engines below declare. A roster id, so every capability table resolves
/// its real row for either engine.
const CANON: &str = "binance";
/// The primary account's routing key — equal to [`CANON`], i.e. the default `ExecutionEngine::new`
/// produces and the shape every mount in this workspace builds.
const ACCOUNT_A: &str = "binance";
/// The SECOND account's routing key. Not a roster id; that is the point.
const ACCOUNT_B: &str = "binance-sub2";

const SYMBOL: &str = "BTCUSDT";

/// One engine whose canonical venue is `venue` and whose routing key is `route_key`.
///
/// ⚠ The `Account` is seeded with `route_key`, not `venue`, and that is not a shortcut — it is a
/// second seam this split does NOT close, recorded here because the test would otherwise hide it.
/// `Account::apply_fill` opens with a hard `assert_eq!(fill.venue, self.venue)`, so a fill can only
/// fold into an account whose venue string it carries. Routing a fill to the right one of two
/// same-venue engines therefore needs the fill to be DISTINGUISHABLE, and today the only field on
/// the payload that can distinguish it is the one that assert compares. Wiring a real second
/// account has to give `FillEvent` a route key of its own (or route it by coid, the way
/// `coid_venue` already routes order-lifecycle replies, which carry no venue at all); until then
/// this is how the two-account shape is expressible at all, and pinning it here is what makes the
/// gap visible instead of theoretical.
fn engine(venue: &str, route_key: &str, symbol: &str) -> ExecutionEngine<RecordingClient> {
    engine_with_account_venue(venue, route_key, route_key, symbol)
}

/// [`engine`] with the `Account`'s venue label chosen SEPARATELY from the routing key.
///
/// The fill-routing tests need it to be the route key (see [`engine`]'s note). The RECONCILE tests
/// need the opposite: a reconcile pass's synthesized fills carry the venue off the venue's own
/// FILL REPORT, which an adapter mints canonically, so they only fold into an account labelled
/// with the canonical venue. Both engines can carry that label at once precisely because reconcile
/// does NOT route by the fill — `reconcile_reports` resolves the engine ONCE from the pass's route
/// key and then publishes straight to it, which is the whole hop under test.
fn engine_with_account_venue(
    venue: &str,
    route_key: &str,
    account_venue: &str,
    symbol: &str,
) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, account_venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        symbol,
    );
    e.route_key = route_key.to_string();
    e
}

/// The two-account core: one canonical venue, two routing keys.
fn two_accounts_of_one_venue() -> CoreThread<RecordingClient> {
    core_of(
        engine(CANON, ACCOUNT_A, SYMBOL),
        vec![(0.0, engine(CANON, ACCOUNT_B, SYMBOL))],
        CoreConfig::default(),
    )
}

fn fill(route_key: &str, trade_id: &'static str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        client_order_id: String::new(),
        venue: route_key.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: "BOTH".into(),
    })
}

/// Signed `BOTH` size held by the engine at routing index `idx`, under `key`.
fn held(core: &CoreThread<RecordingClient>, idx: usize, key: &str) -> f64 {
    core.eng(idx)
        .account
        .positions
        .get(&(
            ustr::Ustr::from(key),
            ustr::Ustr::from(SYMBOL),
            vike_model::events::PositionSide::Both,
        ))
        .map(|p| p.size)
        .unwrap_or(0.0)
}

/// The second account's own symbol — distinct from [`SYMBOL`], so the SYMBOL lane below has an
/// unambiguous answer to give. Two accounts sharing one symbol is now a legal mount; it simply
/// routes by coid instead, which the fill-lane test below covers.
const SYMBOL_B: &str = "ETHUSDT";

/// A fill as a VENUE actually emits one: tagged with the canonical venue, and with the symbol that
/// says which book it belongs to.
fn wire_fill(symbol: &str, trade_id: &'static str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        client_order_id: String::new(),
        venue: CANON.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: "BOTH".into(),
    })
}

/// Signed `BOTH` size held by the engine at `idx`, for an arbitrary (venue-key, symbol) pair.
fn held_at(core: &CoreThread<RecordingClient>, idx: usize, key: &str, symbol: &str) -> f64 {
    core.eng(idx)
        .account
        .positions
        .get(&(
            ustr::Ustr::from(key),
            ustr::Ustr::from(symbol),
            vike_model::events::PositionSide::Both,
        ))
        .map(|p| p.size)
        .unwrap_or(0.0)
}

/// Two accounts of one exchange, each on its OWN symbol — the configuration in which the SYMBOL
/// lane has an unambiguous answer. (`vike_mount::make_engine_accounts` will mount two accounts on
/// ONE symbol too, now that the collision rule is gone; that configuration routes by coid, and
/// `a_shared_symbol_routes_by_coid_not_by_first_match` below is its gate.)
fn two_accounts_on_two_symbols() -> CoreThread<RecordingClient> {
    core_of(
        // ⚠ Both `Account`s are seeded with the CANONICAL venue, which is what
        // `vike_mount::make_engine_for_account` does: it passes `venue` to `Account::new` and
        // decorates only `route_key`. `Account::apply_fill` asserts the fill's venue equals its
        // own, so seeding a route key here would make the harness reject the very wire payload it
        // exists to route. The two books stay distinct because their SYMBOLS differ.
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL_B))],
        CoreConfig::default(),
    )
}

#[path = "route_key/gate.rs"]
#[cfg(test)]
mod gate;
#[path = "route_key/payload_capability.rs"]
#[cfg(test)]
mod payload_capability;
#[path = "route_key/symbol_less_payloads.rs"]
#[cfg(test)]
mod symbol_less_payloads;
