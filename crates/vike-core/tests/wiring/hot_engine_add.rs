//! Decision 0169, slice S1: an engine APPENDED to a RUNNING core (`Ingest::Accounts`).
//!
//! What an append must keep true, each one test: the new account joins the published set and folds
//! its own events; the existing account is untouched; the venue turns ambiguous (an untargeted order
//! is refused by name, never routed to either book); an engine added under a closed risk state
//! starts closed; a duplicate route key and a parcel of the wrong type are refused and the core
//! keeps running.

use vike_core::spawn_core_multi;
use vike_exec::testing::RecordingClient;
use vike_exec::{AccountChange, Command, ExecutionClient, ExecutionEngine, Ingest, OrderIntent};
use vike_model::OrderRequest;
use vike_model::TradingState;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{AccountState, Event};

use crate::kit::engines::{dyn_engine, test_config};
use crate::kit::handle::CoreJoinGuard;

type Engine = ExecutionEngine<Box<dyn ExecutionClient + Send>>;

fn binance() -> Engine {
    dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default()))
}

/// A second binance account: same canonical venue, its own route key.
fn binance_alt() -> Engine {
    let mut e = binance();
    e.route_key = "binance#ALT".to_string();
    e
}

fn balance(venue: &str, usdt: f64) -> Event {
    Event::AccountState(AccountState {
        venue: venue.into(),
        balances: vec![("USDT".into(), usdt)],
        ts: 2,
        route_key: None,
    })
}

fn limit_order(coid: &str, account: Option<AccountLabel>) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ts: 1,
        account,
        ..Default::default()
    }
}

#[test]
fn an_added_engine_joins_the_published_set_and_folds_only_its_own_events() {
    let handle = CoreJoinGuard::new(spawn_core_multi(binance(), Vec::new(), test_config(10_000.0)));
    let cell = handle.snapshot_cell();
    handle.event_sender().add_account_engine(500.0, binance_alt()).unwrap();
    // The NEW account's lane stamps its route key; the default account's balance is not it.
    handle.event_sender().routed("binance#ALT").blocking_send(balance("binance", 700.0)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.portfolio.venues.len(), 2, "the appended engine is a published block");
    assert_eq!(snap.portfolio.venues[0].balance, 0.0, "the account that was running is untouched");
    assert_eq!(snap.portfolio.venues[1].balance, 700.0, "the new account folded its own event");
    assert_eq!(snap.portfolio.venues[1].venue, "binance");
    assert!(
        snap.recent_events
            .iter()
            .any(|l| l.contains("ACCOUNT STARTED") && l.contains("binance#ALT")),
        "the start is said in the recent-events ring: {:?}",
        snap.recent_events
    );
}

#[test]
fn after_the_add_an_untargeted_order_to_the_venue_is_refused_and_a_targeted_one_is_not() {
    let handle = CoreJoinGuard::new(spawn_core_multi(binance(), Vec::new(), test_config(10_000.0)));
    let cell = handle.snapshot_cell();
    handle.event_sender().add_account_engine(500.0, binance_alt()).unwrap();
    // The venue now has two accounts: an order that names only the EXCHANGE reaches neither book ...
    handle
        .try_command(Command::Order(OrderIntent::Submit(Box::new(limit_order("u1", None)))))
        .unwrap();
    // ... while one that names the new account reaches its engine.
    let alt = AccountLabel::parse("ALT").unwrap();
    handle
        .try_command(Command::Order(OrderIntent::Submit(Box::new(limit_order("t1", Some(alt))))))
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(
        snap.recent_events.iter().any(|l| l.contains("REFUSED") && l.contains("binance#ALT")),
        "the untargeted order is refused by name, naming both candidates: {:?}",
        snap.recent_events
    );
    assert!(
        snap.orders.iter().all(|o| o.client_order_id != "u1"),
        "the refused order is in no book"
    );
    assert!(
        snap.orders.iter().any(|o| o.client_order_id == "t1"),
        "the order that named the new account was placed"
    );
}

#[test]
fn an_engine_added_under_a_halt_starts_halted() {
    let handle = CoreJoinGuard::new(spawn_core_multi(binance(), Vec::new(), test_config(10_000.0)));
    let cell = handle.snapshot_cell();
    handle.send_command(Command::SetTradingState(TradingState::Halted));
    handle.event_sender().add_account_engine(500.0, binance_alt()).unwrap();
    let alt = AccountLabel::parse("ALT").unwrap();
    handle
        .try_command(Command::Order(OrderIntent::Submit(Box::new(limit_order("h1", Some(alt))))))
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let placed = snap.orders.iter().find(|o| o.client_order_id == "h1");
    assert!(
        placed.is_none_or(|o| o.status.is_terminal()),
        "a hot start never reopens risk the core has closed: {placed:?}"
    );
}

#[test]
fn a_duplicate_route_key_is_refused_and_the_core_keeps_running() {
    let handle = CoreJoinGuard::new(spawn_core_multi(binance(), Vec::new(), test_config(10_000.0)));
    let cell = handle.snapshot_cell();
    // "binance" is the primary's own route key.
    handle.event_sender().add_account_engine(500.0, binance()).unwrap();
    handle.event_sender().blocking_send(balance("binance", 900.0)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.portfolio.venues.len(), 1, "no second engine for one account");
    assert_eq!(snap.portfolio.venues[0].balance, 900.0, "and the core still folds");
    assert!(snap.recent_events.iter().any(|l| l.contains("ACCOUNT ADD refused")));
}

#[test]
fn a_parcel_of_the_wrong_type_is_refused_and_the_core_keeps_running() {
    let handle = CoreJoinGuard::new(spawn_core_multi(binance(), Vec::new(), test_config(10_000.0)));
    let cell = handle.snapshot_cell();
    let wrong = AccountChange::AddEngine { seed: 1.0, engine: Box::new(7_u8) };
    handle.event_sender().ingest.blocking_send(Ingest::Accounts(Box::new(wrong))).unwrap();
    handle.event_sender().blocking_send(balance("binance", 900.0)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.portfolio.venues.len(), 1);
    assert_eq!(snap.portfolio.venues[0].balance, 900.0);
    assert!(snap.recent_events.iter().any(|l| l.contains("ACCOUNT ADD refused")));
}
