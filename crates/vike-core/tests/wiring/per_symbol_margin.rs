//! A PER-SYMBOL MARGIN RATE (`SymbolAdmission::im_requirement`) through the real runtime and the
//! published block: what the operator reads of the margin once symbols of different leverage share
//! one account.
//!
//! The engine-wide initial margin is 0.25 (4x), the mounted `BTCUSDT` is the 4x perp and `SPOT_A` is
//! admitted at 1.0 (no leverage). `VenueBlock::margin_used`, `free_bp` and each position's
//! `leverage` come out of `RiskLimits::im_for`, which reads the symbol's own `im_by_symbol` row
//! ahead of the engine-wide rate. An engine none of whose admissions state a rate publishes what it
//! always did. Every figure is a whole or dyadic number, compared by `to_bits`.

use vike_core::spawn_core;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, CoreSnapshot, ExecutionEngine, RiskGate, SymbolAdmission,
    VenueBlock,
};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{RiskLimits, SymbolGrid};

use crate::kit::engines::test_config;
use crate::kit::events::sim_bare_fill;
use crate::kit::handle::wait_for_snapshot;

const SPOT: &str = "SPOT_A";

/// Engine-wide initial margin 0.25 (4x) on the mounted `BTCUSDT`, tick and lot 0.5.
fn engine() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.5),
        lot_size: Some(0.5),
        im_requirement: Some(0.25),
        ..RiskLimits::new()
    };
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn admission(im_requirement: Option<f64>) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: SPOT.into(),
        grid: SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(50.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement,
        release: false,
        margin_mode: None,
    }))
}

/// Long 2 @ 50 on the spot symbol, marked at 50: a 100 notional.
fn spot_fill() -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new("spot1").expect("non-empty"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: SPOT.into(),
        side: 1,
        last_qty: 2.0,
        last_px: 50.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(50.0),
        position_side: PositionSide::Both,
    })
}

fn block(snap: &CoreSnapshot) -> &VenueBlock {
    &snap.portfolio.venues[0]
}

/// [`block`] for a wait predicate: the placeholder snapshot the cell holds before the first real
/// build has no venue block at all.
fn try_block(snap: &CoreSnapshot) -> Option<&VenueBlock> {
    snap.portfolio.venues.first()
}

fn leverage_of(snap: &CoreSnapshot, symbol: &str) -> Option<f64> {
    try_block(snap)?.positions.iter().find(|p| p.symbol == symbol).map(|p| p.leverage)
}

/// Admit the spot symbol stating `rate`, hold 1 BTC @ 100 (the 4x perp) and 2 SPOT @ 50, and wait
/// for both positions to be priced.
fn publish_two_positions(rate: Option<f64>) -> std::sync::Arc<CoreSnapshot> {
    let handle = spawn_core(engine(), test_config(10_000.0));
    let cell = handle.snapshot_cell();
    handle.send_command(admission(rate));
    let events = handle.event_sender();
    events.blocking_send(sim_bare_fill("btc1", 1.0, 100.0)).unwrap();
    events.blocking_send(spot_fill()).unwrap();
    wait_for_snapshot(&cell, 5, "both positions priced", |s| {
        try_block(s).is_some_and(|b| b.positions.len() == 2 && b.missing_prices == 0)
    });
    handle.shutdown_and_join();
    cell.load_full()
}

/// **The headline.** A 1x symbol next to the 4x perp: the block's margin is the SUM of each symbol's
/// own requirement (25 for the perp, 100 for the spot), the free buying power is the equity less
/// that, and each position's leverage is its own `1 / im`.
#[test]
fn the_block_margins_each_symbol_at_its_own_rate() {
    let snap = publish_two_positions(Some(1.0));
    let b = block(&snap);
    assert_eq!(
        b.equity.to_bits(),
        10_000.0_f64.to_bits(),
        "marked at the cost: nothing unrealized"
    );
    assert_eq!(
        b.margin_used.to_bits(),
        (1.0 * 100.0 * 0.25 + 2.0 * 50.0 * 1.0_f64).to_bits(),
        "25 for the perp at 4x plus 100 for the spot at 1x"
    );
    assert_eq!(b.margin_used.to_bits(), 125.0_f64.to_bits());
    assert_eq!(b.free_bp.to_bits(), (10_000.0 - 125.0_f64).to_bits());
    assert_eq!(b.margin_ratio.to_bits(), (125.0 / 10_000.0_f64).to_bits());
    assert_eq!(leverage_of(&snap, "BTCUSDT").map(f64::to_bits), Some(4.0_f64.to_bits()));
    assert_eq!(leverage_of(&snap, SPOT).map(f64::to_bits), Some(1.0_f64.to_bits()));
}

/// **The engine that never states a rate publishes what it always did**: both positions at the
/// engine-wide 0.25, leverage 4 for each, and no per-symbol row anywhere.
#[test]
fn an_admission_stating_no_rate_publishes_the_engine_wide_margin() {
    let snap = publish_two_positions(None);
    let b = block(&snap);
    assert_eq!(
        b.margin_used.to_bits(),
        (1.0 * 100.0 * 0.25 + 2.0 * 50.0 * 0.25_f64).to_bits(),
        "25 + 25: both at the engine-wide 0.25"
    );
    assert_eq!(b.margin_used.to_bits(), 50.0_f64.to_bits());
    assert_eq!(b.free_bp.to_bits(), (10_000.0 - 50.0_f64).to_bits());
    assert_eq!(leverage_of(&snap, "BTCUSDT").map(f64::to_bits), Some(4.0_f64.to_bits()));
    assert_eq!(leverage_of(&snap, SPOT).map(f64::to_bits), Some(4.0_f64.to_bits()));
}

/// A repeat admission REPLACES the row; one that states none REMOVES it (the symbol is margined at the
/// engine-wide 4x again); a rate outside `(0, 1]` (or not a number) is IGNORED the same way and refuses
/// nothing (decision 0183 item 11), so a bad figure cannot hold back the rest of the admission.
#[test]
fn a_repeat_admission_replaces_the_row_and_none_or_a_bad_rate_removes_it() {
    let handle = spawn_core(engine(), test_config(10_000.0));
    let cell = handle.snapshot_cell();
    handle.send_command(admission(Some(1.0)));
    handle.event_sender().blocking_send(spot_fill()).unwrap();
    wait_for_snapshot(&cell, 5, "the 1x position", |s| leverage_of(s, SPOT) == Some(1.0));

    // 0.5 replaces 1.0.
    handle.send_command(admission(Some(0.5)));
    wait_for_snapshot(&cell, 5, "the replaced row", |s| leverage_of(s, SPOT) == Some(2.0));
    assert_eq!(block(&cell.load_full()).margin_used.to_bits(), 50.0_f64.to_bits());

    // No rate stated: the row is gone, the engine-wide 0.25 margins the position (2 x 50 x 0.25).
    handle.send_command(admission(None));
    wait_for_snapshot(&cell, 5, "the removed row", |s| leverage_of(s, SPOT) == Some(4.0));
    assert_eq!(block(&cell.load_full()).margin_used.to_bits(), 25.0_f64.to_bits());

    // Out of range or not a number: ignored, which is stating none. Each round puts a row back first,
    // so the removal is what the bad figure did.
    for bad in [0.0, -0.25, 1.5, f64::NAN, f64::INFINITY] {
        handle.send_command(admission(Some(0.5)));
        wait_for_snapshot(&cell, 5, "the row back", |s| leverage_of(s, SPOT) == Some(2.0));
        handle.send_command(admission(Some(bad)));
        wait_for_snapshot(&cell, 5, "the bad figure ignored", |s| {
            leverage_of(s, SPOT) == Some(4.0)
        });
    }
    let snap = cell.load_full();
    assert_eq!(block(&snap).margin_used.to_bits(), 25.0_f64.to_bits());
    assert!(
        snap.recent_events.iter().all(|l| !l.contains("REFUSED")),
        "a bad margin figure refuses nothing: {:?}",
        snap.recent_events
    );
    handle.shutdown_and_join();
}
