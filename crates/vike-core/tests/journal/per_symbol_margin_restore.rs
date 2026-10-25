//! A BOOT RESTORE keeps an admitted symbol's own margin rate (`SymbolAdmission::im_requirement`).
//!
//! Run 1 admits a 1x spot symbol on an engine whose engine-wide initial margin is 0.25 (4x), holds a
//! position on it and stops. Run 2 is the production restart (`restore_for_boot` +
//! `BootRestore::apply_to`, then a new core built from the SAME settings: engine-wide 0.25, no
//! per-symbol rows, because a restore never writes the limits). Until the node's re-admission lands,
//! the restored symbol must still be margined at ITS rate, not the engine-wide one: that is the
//! restore carrying `im_by_symbol[symbol]` back with the admitted symbol (`restore_admitted_symbols`).
//! Pinned through what an operator reads and what the gate does:
//!
//! - the published block: `margin_used` and the position's leverage are the symbol's own (1x);
//! - the gate: a 400-notional buy that passes at 0.25 is REFUSED (`insufficient-margin`) at the
//!   restored 1.0;
//! - the node's re-admission with NO rate removes the row (the admission is the complete statement of
//!   it), one with `Some(0.5)` puts one back.
//!
//! Dyadic figures throughout, compared by `to_bits`.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::restore_for_boot;
use vike_core::{CoreConfig, CoreHandle, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, CoreSnapshot, ExecutionEngine, OrderIntent, QuoteUpdate,
    RiskGate, SymbolAdmission, VenueBlock,
};
use vike_journal::JournalFileConfig;
use vike_marketdata::QuoteTick;
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{OrderRequest, OrderType, RiskLimits, SymbolGrid, TradingState};

use crate::kit::handle::wait_for_snapshot;

const SPOT: &str = "SPOT_A";

/// The settings of BOTH runs: engine-wide 0.25, tick and lot 0.5. No per-symbol row: those come only
/// from an admission (and, after a restart, from the restore).
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

fn journal_at(dir: &Path) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1_000,
    }
}

fn config(journal: Option<JournalConfig>) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash: 400.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        journal,
        ..CoreConfig::default()
    }
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
    }))
}

/// Long 2 @ 50 on the spot symbol: a 100 notional, 100 of margin at 1x and 25 at 0.25.
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

/// The market at 50 (what values a restored position: a restore carries no marks).
fn spot_quote() -> QuoteUpdate {
    QuoteUpdate {
        venue: "sim".into(),
        symbol: SPOT.into(),
        quote: QuoteTick {
            ts: 1,
            local_ts: 1,
            bid: 50.0,
            ask: 50.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: SPOT.into(),
        },
    }
}

/// Buy 4 @ 100 on the spot symbol: 400 notional, so 400 of margin at 1x, 200 at 0.5 and 100 at 0.25.
fn buy_400(coid: &str) -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: SPOT.into(),
        side: 1,
        qty: 4.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
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

/// The spot position's published leverage (`None` on the placeholder, or with no such position).
fn spot_leverage(snap: &CoreSnapshot) -> Option<f64> {
    try_block(snap)?.positions.iter().find(|p| p.symbol == SPOT).map(|p| p.leverage)
}

/// Run 1: the admission stating `rate`, the position, a clean stop; waits until the block shows
/// `margin_used` of margin at `leverage`.
fn first_run(dir: &Path, rate: Option<f64>, margin_used: f64, leverage: f64) {
    let handle = spawn_core(engine(), config(Some(journal_at(dir))));
    let cell = handle.snapshot_cell();
    handle.send_command(admission(rate));
    handle.event_sender().blocking_send(spot_fill()).unwrap();
    wait_for_snapshot(&cell, 5, "the spot position at its rate", |s| {
        try_block(s).is_some_and(|b| b.margin_used == margin_used)
            && spot_leverage(s) == Some(leverage)
    });
    handle.shutdown_and_join();
}

/// Run 2: the production restart over `dir`, with the market handed in (a restore carries none).
/// Returns once the restored position is PRICED, so a margin figure read next is the real one, and
/// the row the journal's snapshot held for the spot symbol.
fn restart(dir: &Path) -> (CoreHandle, Arc<arc_swap::ArcSwap<CoreSnapshot>>, Option<f64>) {
    let tmp = Scratch::created("margin-rate-restore-tmp");
    let restore = restore_for_boot(Some(&journal_at(dir)), &tmp, 42).expect("the journal reads");
    let journaled_row = restore.restored_engines()[0].limits.im_by_symbol.get(SPOT).copied();
    let mut config = config(None);
    restore.apply_to(&mut config);
    let handle = spawn_core(engine(), config);
    let cell = handle.snapshot_cell();
    handle.tick_sender().quote(spot_quote()).unwrap();
    handle.send_command(Command::SetTradingState(TradingState::Active));
    wait_for_snapshot(&cell, 5, "the restored position priced", |s| {
        try_block(s).is_some_and(|b| b.missing_prices == 0) && spot_leverage(s).is_some()
    });
    (handle, cell, journaled_row)
}

/// **The headline.** After the restart, before any re-admission, the symbol is margined at its own
/// 1x: the block says so (margin and leverage) and the gate refuses an order that only the
/// engine-wide 0.25 would let through.
#[test]
fn a_restart_keeps_the_symbols_own_margin_rate_until_the_node_readmits_it() {
    let dir = Scratch::reserved("margin-rate-restore");
    first_run(&dir, Some(1.0), 100.0, 1.0);
    let (handle, cell, journaled_row) = restart(&dir);
    assert_eq!(
        journaled_row.map(f64::to_bits),
        Some(1.0_f64.to_bits()),
        "the journal's snapshot holds the row"
    );

    let snap = cell.load_full();
    assert_eq!(
        block(&snap).margin_used.to_bits(),
        100.0_f64.to_bits(),
        "2 * 50 at 1x, not at 0.25"
    );
    assert_eq!(spot_leverage(&snap).map(f64::to_bits), Some(1.0_f64.to_bits()));

    // 400 notional: 400 of margin at the restored 1x against 300 free (400 - 100) → refused. At
    // the engine-wide 0.25 it would be 100 → passed.
    handle.send_command(buy_400("o1"));
    wait_for_snapshot(&cell, 5, "the refusal", |s| {
        s.recent_events
            .iter()
            .any(|l| l.contains("OrderDenied o1") && l.contains("insufficient-margin"))
    });
    assert!(cell.load_full().orders.iter().all(|o| o.client_order_id != "o1"));

    // The node's re-admission states no rate (an exchange that no longer reports a leverage for the
    // symbol): the admission is the whole statement of the row, so it is REMOVED and the engine-wide
    // 0.25 margins the position again (2 x 50 x 0.25).
    handle.send_command(admission(None));
    wait_for_snapshot(&cell, 5, "the removed row", |s| spot_leverage(s) == Some(4.0));
    assert_eq!(block(&cell.load_full()).margin_used.to_bits(), 25.0_f64.to_bits());

    // …and one that states 0.5 puts a row back: 2x, margin 50, and the order the restored 1x refused
    // now passes (200 of margin against 350 free).
    handle.send_command(admission(Some(0.5)));
    wait_for_snapshot(&cell, 5, "the replaced row", |s| spot_leverage(s) == Some(2.0));
    assert_eq!(block(&cell.load_full()).margin_used.to_bits(), 50.0_f64.to_bits());
    handle.send_command(buy_400("o3"));
    wait_for_snapshot(&cell, 5, "the order to rest at 2x", |s| {
        s.orders.iter().any(|o| o.client_order_id == "o3" && o.status.is_live())
    });
    handle.shutdown_and_join();
}

/// The control: a restore of a journal whose admission stated NO rate leaves no row, and the symbol
/// is margined at the engine-wide 0.25 after the restart exactly as before it.
#[test]
fn a_symbol_admitted_without_a_rate_stays_at_the_engine_wide_rate_across_a_restart() {
    let dir = Scratch::reserved("margin-rate-restore-none");
    first_run(&dir, None, 25.0, 4.0);
    let (handle, cell, journaled_row) = restart(&dir);
    assert_eq!(journaled_row, None, "no rate stated, no row written");
    let snap = cell.load_full();
    assert_eq!(block(&snap).margin_used.to_bits(), 25.0_f64.to_bits());
    assert_eq!(spot_leverage(&snap).map(f64::to_bits), Some(4.0_f64.to_bits()));
    handle.send_command(buy_400("o1"));
    wait_for_snapshot(&cell, 5, "the order to rest at 4x", |s| {
        s.orders.iter().any(|o| o.client_order_id == "o1" && o.status.is_live())
    });
    handle.shutdown_and_join();
}
