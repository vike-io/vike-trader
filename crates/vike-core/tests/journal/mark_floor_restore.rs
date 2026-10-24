//! The MARK FLOOR across a BOOT RESTORE (the cold-boot window): run 1 journals a long position and
//! stops; run 2 is the production restart (`restore_for_boot` + `BootRestore::apply_to`, then a new
//! core) over an engine built with the floor on (`RiskGate::require_mark`).
//!
//! A restore carries no market data, so the restarted engine holds a position and NO mark for it
//! until the first tick. What must hold, against a real spawned core:
//!
//! - an OPENING order is denied `no-mark-for-mounted-symbol` (a market order would otherwise be
//!   judged at notional zero, every cap passing);
//! - a COVERED reduce of the restored position rests: a restart must never trap a position;
//! - after one quote the same engine judges the opening order at the mark (`over-max-notional` for the
//!   big one, a small one rests).

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::restore_for_boot;
use vike_core::{CoreConfig, CoreHandle, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, QuoteUpdate, RiskGate,
};
use vike_journal::JournalFileConfig;
use vike_marketdata::QuoteTick;
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{AccountTier, Clock, OrderRequest, OrderType, RiskLimits, TradingState};

use crate::kit::handle::wait_for_snapshot;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const CAP: f64 = 5_000.0;
const MARK: f64 = 74_300.0;

/// Switch the floor on: what the live assembly does for the derived lane twin engine (any engine with the
/// flag set restores the same way, which is all this file needs).
fn arm(e: &mut ExecutionEngine<RecordingClient>) {
    e.gate.require_mark = true;
}

/// A `live`-tier engine with both ceilings armed; `floor` says whether the mark floor is on.
fn engine(floor: bool) -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.1),
        lot_size: Some(0.001),
        max_notional_per_order: Some(CAP),
        max_total_exposure: Some(CAP),
        ..RiskLimits::new()
    };
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    e.tier = AccountTier::Live;
    if floor {
        arm(&mut e);
    }
    e
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

fn open_fill() -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new("open").expect("non-empty"),
        client_order_id: String::new(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: 0.05,
        last_px: 74_000.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

fn quote() -> QuoteUpdate {
    QuoteUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        quote: QuoteTick {
            ts: 2,
            local_ts: 2,
            bid: MARK,
            ask: MARK,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: SYMBOL.into(),
        },
    }
}

fn market(coid: &str, side: i32, qty: f64, reduce_only: bool) -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type: OrderType::Market,
        reduce_only,
        ..Default::default()
    })))
}

fn denied_with(s: &vike_exec::CoreSnapshot, coid: &str, word: &str) -> bool {
    let head = format!("OrderDenied {coid}");
    s.recent_events.iter().any(|l| l.contains(&head) && l.contains(word))
}

fn has_position(s: &vike_exec::CoreSnapshot) -> bool {
    s.portfolio
        .venues
        .first()
        .is_some_and(|b| b.positions.iter().any(|p| p.symbol == SYMBOL && p.size == 0.05))
}

/// Run 1: open the position (no mark ever, no floor needed: a fill is not gated), stop. The journal is
/// left in `dir`.
fn first_run(dir: &Path) {
    let now = Arc::new(AtomicI64::new(1_000_000));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: clock(&now),
        journal: Some(journal_at(dir)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine(false), config);
    let cell = handle.snapshot_cell();
    handle.event_sender().blocking_send(open_fill()).unwrap();
    handle.send_command(Command::SetTradingState(TradingState::Active));
    wait_for_snapshot(&cell, 5, "the position to be published", has_position);
    handle.shutdown_and_join();
}

/// Run 2: the production restart over `dir`, floor on.
fn restart(dir: &Path) -> CoreHandle {
    let tmp = Scratch::created("mark-floor-restore-tmp");
    let restore = restore_for_boot(Some(&journal_at(dir)), &tmp, 42).expect("the journal reads");
    assert_eq!(restore.restored_engines().len(), 1, "the one engine's snapshot comes back");
    let now = Arc::new(AtomicI64::new(2_000_000));
    let mut config =
        CoreConfig { seed_cash: 10_000.0, clock: clock(&now), ..CoreConfig::default() };
    restore.apply_to(&mut config);
    spawn_core(engine(true), config)
}

/// **The cold-boot window.** The restored engine holds the position and no mark: an opening order is
/// denied by name, the covered reduce rests, and one quote later the ceilings judge at the mark.
#[test]
fn a_restored_engine_with_no_tick_denies_openings_and_still_closes_the_position() {
    let dir = Scratch::reserved("mark-floor-restore");
    first_run(&dir);

    let handle = restart(&dir);
    let cell = handle.snapshot_cell();
    // A restart publishes nothing until something dirties the core.
    handle.send_command(Command::SetTradingState(TradingState::Active));
    wait_for_snapshot(&cell, 5, "the restored position", has_position);

    handle.send_command(market("big", 1, 0.1, false));
    handle.send_command(market("close", -1, 0.05, true));
    wait_for_snapshot(&cell, 5, "the denial by name and the close resting", |s| {
        denied_with(s, "big", "no-mark-for-mounted-symbol")
            && s.orders.iter().any(|o| o.client_order_id == "close" && o.status.is_live())
    });

    handle.tick_sender().quote(quote()).unwrap();
    handle.send_command(market("big2", 1, 0.1, false));
    handle.send_command(market("small", 1, 0.01, false));
    wait_for_snapshot(&cell, 5, "the ceilings judging at the mark", |s| {
        denied_with(s, "big2", "over-max-notional")
            && s.orders.iter().any(|o| o.client_order_id == "small" && o.status.is_live())
    });
    handle.shutdown_and_join();
}
