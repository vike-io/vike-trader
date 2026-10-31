//! A BOOT RESTORE across a stale quote-currency rate (multi-currency ledger, phase 1).
//!
//! Run 1 journals a USD account that admits a JPY-quoted symbol at a rate and opens a position on
//! it, then stops cleanly. Run 2 is the production restart (`restore_for_boot` +
//! `BootRestore::apply_to`, then a new core) with the core clock past the rate's time-to-live
//! (`DEFAULT_QUOTE_RATE_TTL_MS`). What must hold, and is pinned here against a real spawned core:
//!
//! - the restored position is valued with the RESTORED rate, so the equity the restart publishes is
//!   the equity the first run published (bit for bit);
//! - the published rate row says the rate is stale, because it is as old as the snapshot;
//! - an OPENING order on the symbol is denied with `quote-rate-stale` (fail closed, never par);
//! - a REDUCING order is not: a stale rate must never trap a position;
//! - a fresh re-admission (a new rate with a new time) lifts the denial and re-prices the position.
//!
//! The control run restarts INSIDE the window and shows the denial is the rate's age and nothing
//! else. Every figure is a dyadic rational, compared by `to_bits`.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::restore_for_boot;
use vike_core::{CoreConfig, CoreHandle, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Command, CoreSnapshot, DEFAULT_QUOTE_RATE_TTL_MS, ExecutionEngine, OrderIntent, QuoteUpdate,
    SymbolAdmission, VenueBlock,
};
use vike_journal::JournalFileConfig;
use vike_marketdata::QuoteTick;
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{Clock, ContractValue, OrderRequest, OrderType, SymbolGrid, TradingState};

use crate::kit::engines::engine_on;
use crate::kit::handle::wait_for_snapshot;

const JPY_PAIR: &str = "USD_JPY";
/// 128 yen to the dollar.
const R1: f64 = 1.0 / 128.0;
/// The renewed rate: 65/8192, 1.6% away from `R1`.
const R2: f64 = 65.0 / 8192.0;
/// The clock of run 1, and the instant its rate was observed.
const T0: i64 = 1_000_000;

fn usd_engine() -> ExecutionEngine<RecordingClient> {
    let mut engine = engine_on("sim", "BTCUSDT", RecordingClient::default());
    engine.quote_asset = "USD".into();
    engine
}

fn clock(now: &Arc<AtomicI64>) -> Box<dyn Clock + Send> {
    let now = Arc::clone(now);
    Box::new(move || now.load(Ordering::Relaxed))
}

fn journal_at(dir: &Path) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1_000,
    }
}

fn admission(rate: f64, as_of_ms: i64) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: JPY_PAIR.into(),
        grid: SymbolGrid { tick_size: Some(0.25), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(150.0),
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        im_requirement: None,
        release: false,
        margin_mode: None,
    }))
}

/// Long 1000 @ 150 with the venue's mark at 151, so the position is worth `1000 * 1 * R1` USD.
fn open_fill() -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new("open").expect("non-empty"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: JPY_PAIR.into(),
        side: 1,
        last_qty: 1000.0,
        last_px: 150.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(151.0),
        position_side: PositionSide::Both,
    })
}

/// The market at 151 on the JPY pair: what values the position once the venue's mark is gone (a
/// restore carries no market data).
fn jpy_quote() -> QuoteUpdate {
    QuoteUpdate {
        venue: "sim".into(),
        symbol: JPY_PAIR.into(),
        quote: QuoteTick {
            ts: 1,
            local_ts: 1,
            bid: 151.0,
            ask: 151.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: JPY_PAIR.into(),
        },
    }
}

fn order(coid: &str, side: i32, qty: f64, px: f64, reduce_only: bool) -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: JPY_PAIR.into(),
        side,
        qty,
        order_type: OrderType::Limit,
        price: Some(px),
        reduce_only,
        ..Default::default()
    })))
}

fn block(snap: &CoreSnapshot) -> &VenueBlock {
    &snap.portfolio.venues[0]
}

/// [`block`] for a wait predicate: the placeholder snapshot the cell holds before the first real
/// build has no venue block at all.
fn try_block(snap: &CoreSnapshot) -> Option<&VenueBlock> {
    snap.portfolio.venues.first()
}

/// The JPY position's row in the published block (`None` on the placeholder too).
fn jpy_position(snap: &CoreSnapshot) -> Option<&vike_exec::PositionView> {
    try_block(snap)?.positions.iter().find(|p| p.symbol == JPY_PAIR)
}

/// Run 1: admit at `R1` (observed at `T0`), open, mark it at 151, stop. Returns the equity it
/// published at that point and leaves the journal in `dir`.
fn first_run(dir: &Path) -> f64 {
    let now = Arc::new(AtomicI64::new(T0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: clock(&now),
        journal: Some(journal_at(dir)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(usd_engine(), config);
    let cell = handle.snapshot_cell();
    handle.send_command(admission(R1, T0));
    handle.event_sender().blocking_send(open_fill()).unwrap();
    handle.tick_sender().quote(jpy_quote()).unwrap();
    handle.send_command(Command::SetTradingState(TradingState::Active));
    wait_for_snapshot(&cell, 5, "the position valued at the market", |s| {
        jpy_position(s).is_some_and(|p| p.size == 1000.0 && p.unrealized == 7.8125)
    });
    let equity = block(&cell.load_full()).equity;
    assert_eq!(equity.to_bits(), 10_007.812_5_f64.to_bits(), "seed + 1000 yen of gain at 1/128");
    handle.shutdown_and_join();
    equity
}

/// Run 2: the production restart over `dir`, on the core clock `now` the test steps.
fn restart(dir: &Path, now: &Arc<AtomicI64>) -> CoreHandle {
    let tmp = Scratch::created("quote-rate-restore-tmp");
    let restore = restore_for_boot(Some(&journal_at(dir)), &tmp, 42).expect("the journal reads");
    assert_eq!(restore.restored_engines().len(), 1, "the one engine's snapshot comes back");
    assert!(restore.restored_engines()[0].account.fx.is_some(), "…and it carries the fx table");
    let mut config = CoreConfig { seed_cash: 10_000.0, clock: clock(now), ..CoreConfig::default() };
    restore.apply_to(&mut config);
    spawn_core(usd_engine(), config)
}

/// Value the restored position at the market (the restore carries no marks) and wait for it.
fn restored_and_marked(handle: &CoreHandle) -> Arc<CoreSnapshot> {
    let cell = handle.snapshot_cell();
    handle.tick_sender().quote(jpy_quote()).unwrap();
    handle.send_command(Command::SetTradingState(TradingState::Active));
    wait_for_snapshot(&cell, 5, "the restored position valued at the market", |s| {
        jpy_position(s).is_some_and(|p| p.size == 1000.0 && p.unrealized != 0.0)
    });
    cell.load_full()
}

/// **The headline.** A restart past the rate's window: valuation uses the restored rate (equity
/// unchanged), the table says stale, an opening order is denied, a reducing one is not, and a fresh
/// re-admission lifts it.
#[test]
fn a_restart_past_the_window_values_at_the_restored_rate_and_refuses_only_openings() {
    let dir = Scratch::reserved("quote-rate-restore-stale");
    let equity_before = first_run(&dir);

    let late = T0 + DEFAULT_QUOTE_RATE_TTL_MS + 5_000;
    let now = Arc::new(AtomicI64::new(late));
    let handle = restart(&dir, &now);
    let cell = handle.snapshot_cell();
    let snap = restored_and_marked(&handle);

    // The restored rate, valued: the equity of run 1, bit for bit.
    let b = block(&snap);
    assert_eq!(b.equity.to_bits(), equity_before.to_bits(), "a restart never moves the equity");
    let p = jpy_position(&snap).expect("the position came back");
    assert_eq!(p.unrealized.to_bits(), 7.8125_f64.to_bits(), "valued at the restored 1/128");
    assert_eq!(b.contract_value_of(JPY_PAIR), ContractValue::linear(R1));
    // …and it says the rate is old.
    assert_eq!(b.quote_rates.len(), 1);
    assert_eq!(b.quote_rates[0].ccy, "JPY");
    assert_eq!(b.quote_rates[0].rate.to_bits(), R1.to_bits());
    assert_eq!(b.quote_rates[0].as_of_ms, T0);
    assert!(b.quote_rates[0].stale, "the restored rate is as old as the snapshot");
    assert!(b.quote_rates_stale());
    assert_eq!(b.quote_rates_in, "USD");

    // An OPENING order is denied for the rate, by name, and never reaches the book.
    handle.send_command(order("o-open", 1, 100.0, 150.0, false));
    wait_for_snapshot(&cell, 5, "the opening order's denial", |s| {
        s.recent_events
            .iter()
            .any(|l| l.contains("OrderDenied o-open") && l.contains("quote-rate-stale"))
    });
    assert!(
        cell.load_full().orders.iter().all(|o| o.client_order_id != "o-open"),
        "a denied order never enters the registry"
    );

    // A REDUCING order passes: it rests.
    handle.send_command(order("o-reduce", -1, 400.0, 151.0, true));
    wait_for_snapshot(&cell, 5, "the reducing order to rest", |s| {
        s.orders.iter().any(|o| o.client_order_id == "o-reduce" && o.status.is_live())
    });

    // The node's fresh re-admission (a new rate, a new time) lifts the denial and re-prices.
    let renewed = late + 1_000;
    now.store(renewed, Ordering::Relaxed);
    handle.send_command(admission(R2, renewed));
    wait_for_snapshot(&cell, 5, "the renewed rate", |s| {
        try_block(s).is_some_and(|b| {
            b.quote_rates.len() == 1 && b.quote_rates[0].rate == R2 && !b.quote_rates_stale()
        })
    });
    let snap = cell.load_full();
    assert_eq!(block(&snap).quote_rates[0].as_of_ms, renewed);
    assert_eq!(block(&snap).contract_value_of(JPY_PAIR), ContractValue::linear(R2));
    assert_eq!(
        jpy_position(&snap).expect("still held").unrealized.to_bits(),
        (1000.0 * 1.0 * R2).to_bits(),
        "the position is worth what the renewed rate says"
    );
    handle.send_command(order("o-open2", 1, 100.0, 150.0, false));
    wait_for_snapshot(&cell, 5, "the opening order to rest", |s| {
        s.orders.iter().any(|o| o.client_order_id == "o-open2" && o.status.is_live())
    });
    handle.shutdown_and_join();
}

/// **The control.** The same restart one second after the rate was observed: the opening order
/// rests and nothing is denied, so the denial above is the rate's age and nothing else.
#[test]
fn a_restart_inside_the_window_opens_normally() {
    let dir = Scratch::reserved("quote-rate-restore-fresh");
    let equity_before = first_run(&dir);

    let now = Arc::new(AtomicI64::new(T0 + 1_000));
    let handle = restart(&dir, &now);
    let cell = handle.snapshot_cell();
    let snap = restored_and_marked(&handle);
    let b = block(&snap);
    assert_eq!(b.equity.to_bits(), equity_before.to_bits());
    assert!(!b.quote_rates[0].stale, "one second old: inside the window");
    assert!(!b.quote_rates_stale());

    handle.send_command(order("o-open", 1, 100.0, 150.0, false));
    wait_for_snapshot(&cell, 5, "the opening order to rest", |s| {
        s.orders.iter().any(|o| o.client_order_id == "o-open" && o.status.is_live())
    });
    assert!(
        !cell.load_full().recent_events.iter().any(|l| l.contains("OrderDenied")),
        "nothing was denied: {:?}",
        cell.load_full().recent_events
    );
    handle.shutdown_and_join();
}
