//! Combo gate 4 — bare-`Fill` coid-ownership routing (`ExecutionEngine::owns_fill_symbol`).
//!
//! A live deribit combo fills under symbols the engine never mounted: the venue reports one combo
//! execution as N LEG trade rows (real per-leg position deltas, leg symbols) PLUS one aggregate
//! net-price row under the venue-minted COMBO instrument id — all labeled with OUR coid (captured
//! live: `vike-deribit`'s `deribit_combo_fill_probe`, 2026-07-19 testnet). Before the fix,
//! `on_event`'s symbol filter dropped every one of those bare fills — order Filled via the
//! coid-routed wrap, `Account` flat, position/PnL wrong. The fix folds a bare fill when its coid
//! names an order THIS engine manages AND the fill's symbol carries position truth for it (the
//! order's own `request.symbol`, or one of its `combo_legs`), while the combo net print (matching
//! neither — a combo request's `symbol` is empty by `build_combo`'s contract) stays deliberately
//! out of the fold: the venue books NO position under the combo id, so folding it would mint a
//! phantom position at the net price and double-count PnL.
//!
//! The account-wide-stream contract is pinned unchanged: a foreign-symbol fill with a foreign or
//! EMPTY coid (venues stream label-less external-order fills on shared accounts) is still
//! filtered, and the mounted-symbol paths are byte-identical.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, ManagedOrder, Outbox, RiskGate, RiskLimits,
};
use vike_model::events::{Event, FillEvent};
use vike_model::{build_combo, ComboLeg, ComboSpec, OrderRequest, TimeInForce};

const VENUE: &str = "deribit";
const MOUNTED: &str = "BTC-PERPETUAL";
const COMBO_ID: &str = "BTC-FS-25DEC26_20JUL26"; // the venue-minted combo instrument (probe's)
const LEG_NEAR: &str = "BTC-20JUL26";
const LEG_FAR: &str = "BTC-25DEC26";

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    )
}

/// Register the probe's future-spread combo under `coid` (buy 1×FAR / sell 1×NEAR): the request
/// `build_combo` mints keeps `symbol` EMPTY (the adapter resolves the combo instrument at
/// submit), and the legs live in `combo_legs` — exactly what the routing keys on.
fn register_combo(e: &mut ExecutionEngine<RecordingClient>, coid: &str) {
    let spec = ComboSpec {
        venue: VENUE.into(),
        side: 1,
        qty: 10.0,
        legs: vec![
            ComboLeg { symbol: LEG_FAR.into(), ratio: 1 },
            ComboLeg { symbol: LEG_NEAR.into(), ratio: -1 },
        ],
        net_limit: Some(1199.0),
        time_in_force: TimeInForce::Gtc,
    };
    let req = build_combo(&spec, coid).expect("valid combo spec");
    assert!(req.symbol.is_empty(), "build_combo's contract: the adapter names the instrument");
    e.registry.insert(coid.to_string(), ManagedOrder::new(req));
}

/// Register a plain single-leg order on a symbol the engine never mounted (the deribit
/// options-ticket class: the engine mounts BTC-PERPETUAL, the ticket submits an option).
fn register_single(e: &mut ExecutionEngine<RecordingClient>, coid: &str, symbol: &str) {
    let req = OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 0.1,
        order_type: "limit".into(),
        price: Some(0.05),
        ..Default::default()
    };
    e.registry.insert(coid.to_string(), ManagedOrder::new(req));
}

fn fill(
    coid: &str,
    trade_id: &'static str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn pos(e: &ExecutionEngine<RecordingClient>, symbol: &str) -> Option<(f64, f64)> {
    e.account
        .positions
        .get(&(VENUE.into(), symbol.into(), "BOTH".into()))
        .map(|p| (p.size, p.avg_px))
}

/// THE gate-4 fix: the probe's two leg prints — foreign symbols, owned coid — now fold into the
/// Account at per-leg prices (they were silently dropped before), and the strategy seam sees them.
#[test]
fn owned_coid_leg_fills_fold_under_foreign_symbols() {
    let mut e = engine();
    register_combo(&mut e, "cb1");
    e.collect_applied_fills = true; // what the runtime sets iff a strategy is mounted
    let mut ob = Outbox::default();
    // the probe's captured leg rows: sell NEAR at 64451.5, buy FAR at 65650.5, 10 each
    e.on_event(&Event::Fill(fill("cb1", "t-near", LEG_NEAR, -1, 10.0, 64451.5)), &mut ob);
    e.on_event(&Event::Fill(fill("cb1", "t-far", LEG_FAR, 1, 10.0, 65650.5)), &mut ob);
    assert_eq!(pos(&e, LEG_NEAR), Some((-10.0, 64451.5)), "short leg folded at its own price");
    assert_eq!(pos(&e, LEG_FAR), Some((10.0, 65650.5)), "long leg folded at its own price");
    let seen: Vec<(&str, f64)> =
        e.applied_fills.iter().map(|a| (a.fill.symbol.as_str(), a.fill.last_qty)).collect();
    assert_eq!(seen, vec![(LEG_NEAR, 10.0), (LEG_FAR, 10.0)], "Strategy::on_fill sees both legs");
}

/// The combo-INSTRUMENT net print (owned coid, but a symbol that is neither the request's own —
/// empty for a combo — nor any leg) must NOT fold: the venue books no position under the combo
/// id, so folding the net row would mint a phantom position and double-count PnL.
#[test]
fn owned_coid_combo_net_print_never_folds() {
    let mut e = engine();
    register_combo(&mut e, "cb1");
    let mut ob = Outbox::default();
    // the probe's captured combo row: BUY 10 @ net 1199.0 under the combo instrument itself
    e.on_event(&Event::Fill(fill("cb1", "t-combo", COMBO_ID, 1, 10.0, 1199.0)), &mut ob);
    assert_eq!(pos(&e, COMBO_ID), None, "no phantom position under the combo instrument");
    assert!(e.account.positions.is_empty(), "the net print folds NOTHING");
}

/// The full probe capture in venue order (leg, leg, net print): Account ends with exactly the two
/// per-leg positions — the venue's own `get_positions` answer after the live fill.
#[test]
fn probe_row_sequence_folds_to_per_leg_positions_only() {
    let mut e = engine();
    register_combo(&mut e, "cb1");
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(fill("cb1", "258544119", LEG_NEAR, -1, 10.0, 64451.5)), &mut ob);
    e.on_event(&Event::Fill(fill("cb1", "258544120", LEG_FAR, 1, 10.0, 65650.5)), &mut ob);
    e.on_event(&Event::Fill(fill("cb1", "258544118", COMBO_ID, 1, 10.0, 1199.0)), &mut ob);
    assert_eq!(e.account.positions.len(), 2, "per-leg positions only: {:?}", e.account.positions);
    assert_eq!(pos(&e, LEG_NEAR), Some((-10.0, 64451.5)));
    assert_eq!(pos(&e, LEG_FAR), Some((10.0, 65650.5)));
}

/// The options-ticket class: a plain order submitted on a never-mounted symbol — its fill now
/// folds via coid ownership (request.symbol match), no `extra_symbols` wiring required.
#[test]
fn owned_coid_single_leg_fill_folds_on_unmounted_symbol() {
    let mut e = engine();
    register_single(&mut e, "opt1", "BTC-25SEP26-65000-C");
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(fill("opt1", "t1", "BTC-25SEP26-65000-C", 1, 0.1, 0.05)), &mut ob);
    assert_eq!(pos(&e, "BTC-25SEP26-65000-C"), Some((0.1, 0.05)), "owned option fill folded");
}

/// The account-wide-stream contract stands: a foreign-symbol fill whose coid this engine does NOT
/// manage is still filtered — a shared exchange account streams other sessions' fills.
#[test]
fn foreign_symbol_foreign_coid_still_ignored() {
    let mut e = engine();
    register_combo(&mut e, "cb1");
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(fill("someone-else", "t9", LEG_NEAR, 1, 5.0, 64000.0)), &mut ob);
    assert!(e.account.positions.is_empty(), "foreign-coid fill dropped");
}

/// Label-less external fills (the common shared-account shape) never probe the registry and stay
/// filtered, even while owned orders coexist in it.
#[test]
fn foreign_symbol_empty_coid_still_ignored() {
    let mut e = engine();
    register_combo(&mut e, "cb1");
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(fill("", "t9", LEG_NEAR, 1, 5.0, 64000.0)), &mut ob);
    assert!(e.account.positions.is_empty(), "label-less foreign fill dropped");
}

/// Regression pin: the single-symbol paths are byte-identical — a mounted-symbol fill folds with
/// ANY coid (owned, foreign, or empty), exactly as before the routing change.
#[test]
fn mounted_symbol_fills_fold_regardless_of_coid() {
    let mut e = engine();
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(fill("", "t1", MOUNTED, 1, 10.0, 64000.0)), &mut ob);
    e.on_event(&Event::Fill(fill("unknown-coid", "t2", MOUNTED, 1, 10.0, 64100.0)), &mut ob);
    assert_eq!(pos(&e, MOUNTED), Some((20.0, 64050.0)), "mounted-symbol folds unchanged");
}

/// Regression pin: `extra_symbols` admission (the Phase D seam the combo lowering wires for leg
/// funding/liquidation) still folds label-less fills on admitted symbols, byte-identical.
#[test]
fn extra_symbols_admission_unchanged() {
    let mut e = engine();
    e.extra_symbols = vec![LEG_NEAR.to_string()];
    let mut ob = Outbox::default();
    e.on_event(&Event::Fill(fill("", "t1", LEG_NEAR, 1, 10.0, 64000.0)), &mut ob);
    assert_eq!(pos(&e, LEG_NEAR), Some((10.0, 64000.0)), "extra_symbols path unchanged");
}

/// The reconnect-replay dedup guards the coid-routed lane too: the same venue trade_id replayed
/// through the ownership route folds exactly once.
#[test]
fn coid_routed_fills_share_the_reconnect_dedup() {
    let mut e = engine();
    register_combo(&mut e, "cb1");
    let mut ob = Outbox::default();
    let f = fill("cb1", "t-near", LEG_NEAR, -1, 10.0, 64451.5);
    e.on_event(&Event::Fill(f.clone()), &mut ob);
    e.on_event(&Event::Fill(f), &mut ob); // WS reconnect replay
    assert_eq!(pos(&e, LEG_NEAR), Some((-10.0, 64451.5)), "replay folded exactly once");
}
