//! **A paper book's fills are booked after a restart** (the CI box, ibkr paper, 2026-11-17). A restart puts the
//! engine's fill-dedup ledgers back from the journal (`Engine::restore_runtime_state`, the boot restore) behind a
//! NEW paper book, which counts its fills from one again. Named a bare `paper-<n>`, the new book's first fills
//! carried the ids the ledgers already held, so the account and the order FSM refused them as re-deliveries,
//! silently (a restored id carries no print): the market order filled at once at the quote in the book while its
//! row stayed `Accepted` with nothing booked, and a cancel then found nothing to cancel. A fill names its order
//! now (`paper-<order client id>-<n>`).

use vike_execution::testing::pump;
use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, ExecutionClient, RiskGate};
use vike_model::events::PositionSide;
use vike_model::{OrderRequest, OrderStatus, OrderType, RiskLimits};
use vike_paper::ExecutionClientPaper;

const VENUE: &str = "ibkr";
const SYMBOL: &str = "AAPL";
const BID: f64 = 353.60;
const ASK: f64 = 353.80;

/// The operator's ibkr row of `AAPL` (decision 0290: ibkr lists nothing, so its kept list is the operator's rows),
/// held as this process's ibkr kept list: the paper book reads the stock's dollar quote and unit off it.
fn hold_aapl_row() {
    use vike_model::{Contract, ContractEquity, Instrument};
    let equity = ContractEquity { exchange: None, currency: Some("USD".into()) };
    let row = Instrument::new(VENUE, "AAPL.SMART.USD", SYMBOL, "USD", Contract::Equity(equity));
    vike_catalog::hold_kept_list(VENUE, vec![row]);
}

fn engine() -> Engine<ExecutionClientPaper> {
    hold_aapl_row();
    Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientPaper::new(VENUE, SYMBOL, 0.0, 0.0, 0.0),
        VENUE,
        SYMBOL,
    )
}

fn market(order_client_id: &str, side: i32, at: i64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty: 1.0,
        order_type: OrderType::Market,
        timestamp: at,
        ..Default::default()
    }
}

fn position(engine: &Engine<ExecutionClientPaper>) -> f64 {
    let key = (VENUE.into(), SYMBOL.into(), PositionSide::Both);
    engine.account.positions.get(&key).map_or(0.0, |p| p.size)
}

/// The operator's check: one share bought and sold at the standing quote, each filled at once and booked.
fn buy_then_sell(engine: &mut Engine<ExecutionClientPaper>, buy: &str, sell: &str) {
    let mut outbox = EventBusOutbox::default();
    engine.client.on_quote(SYMBOL, BID, ASK, 1_000);
    engine.submit_order(&market(buy, 1, 2_000), 2_000, &mut outbox);
    pump(engine, &mut outbox);
    assert_eq!(position(engine), 1.0, "{buy}: the buy is booked");
    engine.submit_order(&market(sell, -1, 3_000), 3_000, &mut outbox);
    pump(engine, &mut outbox);
    for id in [buy, sell] {
        let row = &engine.registry[id];
        assert_eq!((row.status, row.filled_qty), (OrderStatus::Filled, 1.0), "{id}");
    }
    assert_eq!(position(engine), 0.0, "flat again");
    assert_eq!(engine.account.duplicate_fills_refused, 0, "no fill was refused as already folded");
}

#[test]
fn a_paper_book_after_a_restart_books_its_fills_on_the_restored_engine() {
    let mut before = engine();
    buy_then_sell(&mut before, "C-buy-1", "C-sell-1");
    let journaled = before.journal_snapshot();
    assert_eq!(journaled.seen_trade_ids.len(), 2, "the ledger the journal carries");

    // the restart: the engine's state comes back, the book is new
    let mut after = engine();
    after.restore_runtime_state(&journaled);
    let realized = after.account.realized_pnl;
    buy_then_sell(&mut after, "C-buy-2", "C-sell-2");
    let spread = after.account.realized_pnl - realized;
    assert!((spread - (BID - ASK)).abs() < 1e-9, "the round trip books its spread: {spread}");
    let ids: Vec<&str> = after.seen_trade_ids().collect();
    assert_eq!(ids.len(), 4, "no fill id repeats across the restart: {ids:?}");
}
