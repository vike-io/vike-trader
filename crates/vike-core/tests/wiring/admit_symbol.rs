//! `Command::AdmitSymbol` through the REAL runtime (decision 0173): spawn_core + the command lane +
//! the event lane + the published snapshot, the surface the node and the GUI actually use.
//!
//! The owner's rule: an ACCOUNT trades any symbol of its exchange at any time. The engine half is a
//! journaled command carrying the new symbol's own facts; this suite pins what the operator gets
//! from it end to end (admitted at first use, snapped on that symbol's grid, filled into that
//! symbol's position, an unknown symbol still refused, published as tradable), and that a refused
//! admission leaves the engine as it was.

use vike_core::spawn_core;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate};
use vike_exec::{SymbolAdmission, VenueBlock};
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{OrderRequest, RiskLimits, SymbolGrid};

use crate::kit::engines::test_config;
use crate::kit::handle::wait_for_snapshot;

const VENUE: &str = "binance";
const MOUNTED: &str = "BTCUSDT";
const SECOND: &str = "ETHUSDT";

/// A binance/BTCUSDT engine on BTC's coarse grid (tick 0.1, lot 0.001).
fn engine() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits { tick_size: Some(0.1), lot_size: Some(0.001), ..RiskLimits::new() };
    ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    )
}

fn eth_admission(route_key: &str) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: route_key.into(),
        symbol: SECOND.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.0001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(3000.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
        release: false,
        margin_mode: None,
    }))
}

fn eth_limit() -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        venue: VENUE.into(),
        symbol: SECOND.into(),
        side: 1,
        qty: 0.12346,
        order_type: vike_model::OrderType::Limit,
        price: Some(3000.456),
        ..Default::default()
    })))
}

fn fill(coid: &str, tid: &'static str, symbol: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: tid.into(),
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

fn block(snap: &vike_exec::CoreSnapshot) -> &VenueBlock {
    &snap.portfolio.venues[0]
}

#[test]
fn an_order_on_a_second_symbol_is_admitted_snapped_and_folded_on_that_symbol() {
    let handle = spawn_core(engine(), test_config(10_000.0));
    let cell = handle.snapshot_cell();
    handle.send_command(eth_admission(VENUE));
    handle.send_command(eth_limit());
    wait_for_snapshot(&cell, 5, "the ETH order to rest", |s| !s.orders.is_empty());

    let snap = cell.load_full();
    assert!(block(&snap).trades(SECOND), "the published block says the account trades ETH now");
    assert!(!block(&snap).trades("SOLUSDT"), "…and nothing else it was not told");
    let order = &snap.orders[0];
    assert_eq!(order.symbol, SECOND);
    assert!(
        (order.price.unwrap() - 3000.46).abs() < 1e-9,
        "ETH's 0.01 tick, got {:?}",
        order.price
    );
    assert!((order.qty - 0.1235).abs() < 1e-9, "ETH's 0.0001 lot, got {}", order.qty);
    let coid = order.client_order_id.clone();

    // A symbol nobody admitted is still refused, even beside an admitted one. Sent FIRST: the event
    // lane is FIFO, so once the ETH position below is published the SOL fill has been judged.
    let events = handle.event_sender();
    events.blocking_send(fill("", "t2", "SOLUSDT", 1.0, 150.0)).unwrap();
    // The venue fills the order: the position lands on ETH, never on the mounted BTC.
    events.blocking_send(fill(&coid, "t1", SECOND, 0.1235, 3000.46)).unwrap();
    wait_for_snapshot(&cell, 5, "the ETH position", |s| {
        s.positions.iter().any(|p| p.symbol == SECOND)
    });
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let sizes: Vec<(&str, f64)> =
        snap.positions.iter().map(|p| (p.symbol.as_str(), p.size)).collect();
    assert_eq!(sizes, vec![(SECOND, 0.1235)], "ETH only: no BTC, and no SOL");
}

/// A route key no engine carries is refused by name and changes nothing: no symbol joins the set,
/// and the order that follows is judged as an unadmitted one (the mounted grid), not as ETH.
#[test]
fn an_admission_for_an_unknown_account_is_refused_and_changes_nothing() {
    let handle = spawn_core(engine(), test_config(10_000.0));
    let cell = handle.snapshot_cell();
    handle.send_command(eth_admission("binance#nobody"));
    handle.send_command(eth_limit());
    wait_for_snapshot(&cell, 5, "the order to rest", |s| !s.orders.is_empty());
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(!block(&snap).trades(SECOND), "nothing was admitted");
    assert!(
        snap.recent_events
            .iter()
            .any(|e| e.contains("AdmitSymbol") && e.contains("binance#nobody")),
        "the refusal is in recent events: {:?}",
        snap.recent_events
    );
    let price = snap.orders[0].price.unwrap();
    assert!((price - 3000.5).abs() < 1e-9, "still the mounted 0.1 tick: {price}");
}

/// **A position held in an admitted symbol is valued at the venue's live quotes, not at the price the admission
/// seeded** (the CI box, fxcm, 2026-10-28: the published unrealized PnL of a held admitted pair stayed at the spread of
/// the entry for as long as it was watched while the quotes reached the engine). Through the real runtime: the
/// admission seeds 3000, the position opens at 3001, and the published unrealized PnL is the seed's (-1) until the
/// venue's first quote, then follows it (the long is valued at the bid).
#[test]
fn a_held_admitted_symbol_is_published_at_the_live_quote_after_the_seed() {
    use vike_exec::QuoteUpdate;
    use vike_marketdata::QuoteTick;
    let handle = spawn_core(engine(), test_config(10_000.0));
    let cell = handle.snapshot_cell();
    handle.send_command(eth_admission(VENUE));
    // The command and the event lanes are two queues: the fill is sent once the admission is published.
    wait_for_snapshot(&cell, 5, "ETH to be admitted", |s| {
        s.portfolio.venues.first().is_some_and(|b| b.trades(SECOND))
    });
    handle.event_sender().blocking_send(fill("", "t1", SECOND, 1.0, 3001.0)).unwrap();
    let unrealized = |s: &vike_exec::CoreSnapshot| {
        s.positions.iter().find(|p| p.symbol == SECOND).map(|p| p.unrealized)
    };
    wait_for_snapshot(&cell, 5, "the position valued at the seed", |s| {
        unrealized(s).is_some_and(|u| (u - (3000.0 - 3001.0)).abs() < 1e-9)
    });

    handle
        .tick_sender()
        .quote(QuoteUpdate {
            venue: VENUE.into(),
            symbol: SECOND.into(),
            quote: QuoteTick {
                ts: 5,
                local_ts: 5,
                bid: 3050.0,
                ask: 3051.0,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: SECOND.into(),
            },
        })
        .unwrap();
    wait_for_snapshot(&cell, 5, "the position valued at the bid", |s| {
        unrealized(s).is_some_and(|u| (u - (3050.0 - 3001.0)).abs() < 1e-9)
    });
    handle.shutdown_and_join();
}
