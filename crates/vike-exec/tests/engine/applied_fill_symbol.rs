//! `AppliedFill.position_after` must report the position of the FILL's symbol.
//!
//! `collect_applied_fills` is set by the runtime iff a strategy is mounted; each entry is
//! handed to that strategy's `on_fill` as the position after THAT fill
//! (`crates/vike-core/src/runtime/strategy_drive.rs`'s `dispatch_applied_fills` reads
//! `af.position_after`).
//!
//! `ExecutionEngine::position_size` delegates to the engine's PRIMARY symbol, so on any
//! engine carrying `extra_symbols` a fill in the secondary symbol reported the PRIMARY
//! symbol's position — a wrong number, silently, about the very symbol the handler is
//! being told about. A strategy sizing off `on_fill`'s position would act on another
//! instrument's inventory.
//!
//! Single-symbol engines are unaffected: `fill.symbol == self.symbol` for every fill they
//! fold, so the same `f64` is read either way.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent};

const VENUE: &str = "binance";
const PRIMARY: &str = "BTCUSDT";
const SECONDARY: &str = "ETHUSDT";

fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1_000_000.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        PRIMARY,
    );
    e.extra_symbols = vec![SECONDARY.to_string()];
    // what the runtime sets iff a strategy is mounted
    e.collect_applied_fills = true;
    e
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

fn order(coid: &str, symbol: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: "market".into(),
        ..Default::default()
    }
}

/// THE FIX. Two symbols on one engine: each fill must report ITS OWN symbol's position.
///
/// FAILS ON THE PRE-FIX CODE: the ETHUSDT fill reported `+10.0` — the BTCUSDT position —
/// because `position_size("BOTH")` delegates to the engine's primary symbol.
#[test]
fn each_fill_reports_its_own_symbols_position() {
    let mut e = engine();
    let mut ob = Outbox::default();

    e.submit_order(&order("b1", PRIMARY, 1, 10.0), 1, &mut ob);
    e.on_event(&Event::Fill(fill("b1", "t-b1", PRIMARY, 1, 10.0, 60_000.0)), &mut ob);

    e.submit_order(&order("e1", SECONDARY, 1, 3.0), 2, &mut ob);
    e.on_event(&Event::Fill(fill("e1", "t-e1", SECONDARY, 1, 3.0, 3_000.0)), &mut ob);

    assert_eq!(e.applied_fills.len(), 2, "both fills must be collected");
    assert_eq!(e.applied_fills[0].fill.symbol.as_str(), PRIMARY);
    assert!(
        (e.applied_fills[0].position_after - 10.0).abs() < 1e-12,
        "{PRIMARY} fill must report the {PRIMARY} position, got {}",
        e.applied_fills[0].position_after
    );
    assert_eq!(e.applied_fills[1].fill.symbol.as_str(), SECONDARY);
    assert!(
        (e.applied_fills[1].position_after - 3.0).abs() < 1e-12,
        "{SECONDARY} fill must report the {SECONDARY} position (3.0), not the {PRIMARY} \
         position (10.0); got {}",
        e.applied_fills[1].position_after
    );
}

/// The per-fill snapshot law must survive the change: within a multi-fill batch on ONE
/// symbol, each entry still carries the position after ITS OWN fill, not the batch total.
#[test]
fn a_multi_fill_batch_still_snapshots_each_fill_separately() {
    let mut e = engine();
    let mut ob = Outbox::default();

    e.submit_order(&order("b1", PRIMARY, 1, 30.0), 1, &mut ob);
    e.on_event(&Event::Fill(fill("b1", "t1", PRIMARY, 1, 10.0, 60_000.0)), &mut ob);
    e.on_event(&Event::Fill(fill("b1", "t2", PRIMARY, 1, 10.0, 60_100.0)), &mut ob);
    e.on_event(&Event::Fill(fill("b1", "t3", PRIMARY, 1, 10.0, 60_200.0)), &mut ob);

    let got: Vec<f64> = e.applied_fills.iter().map(|a| a.position_after).collect();
    assert_eq!(got.len(), 3);
    for (i, want) in [10.0f64, 20.0, 30.0].iter().enumerate() {
        assert!(
            (got[i] - want).abs() < 1e-12,
            "partial {i} must see the position after ITS fill ({want}), got {}",
            got[i]
        );
    }
}

/// A single-symbol engine is byte-identical — `fill.symbol == self.symbol` for every fill
/// it folds, so the changed read returns the same value.
#[test]
fn single_symbol_engine_is_unchanged() {
    let mut e = ExecutionEngine::new(
        Account::new(1_000_000.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        PRIMARY,
    );
    e.collect_applied_fills = true;
    let mut ob = Outbox::default();

    e.submit_order(&order("b1", PRIMARY, 1, 7.0), 1, &mut ob);
    e.on_event(&Event::Fill(fill("b1", "t1", PRIMARY, 1, 7.0, 60_000.0)), &mut ob);

    assert_eq!(e.applied_fills.len(), 1);
    assert!((e.applied_fills[0].position_after - 7.0).abs() < 1e-12);
}
