//! Decision 0169, slice S2: an account STOPPED in a RUNNING core (`AccountChange::StopEngine`).
//!
//! What a stop must keep true, each one test: phase one cancels the stopped account's own resting orders
//! and leaves its client attached, phase two begins the detach of ITS client, and of no other account's;
//! the engine stays in the published set
//! (an index is an attribution key); an order that still reaches the stopped account is answered by
//! its dead client with a terminal reject and never reaches another book; the venue stops being
//! ambiguous; a stop of an account that does not run is a note; the account can start again as a NEW
//! engine that every lookup prefers; and the link dead-man's window for a link closed on purpose is
//! forgotten (the unit test beside `link_deadman.rs` holds that half).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use vike_core::spawn_core_multi;
use vike_exec::{
    Command, CoreSnapshot, ExecutionClient, ExecutionEngine, NotRunningAccount, OrderIntent,
};
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{AccountState, Event, OrderAccepted, OrderRejected, OrderSubmitted};

use crate::kit::engines::{dyn_engine, test_config};
use crate::kit::handle::{CoreJoinGuard, wait_for_snapshot};

type Engine = ExecutionEngine<Box<dyn ExecutionClient + Send>>;

/// What a probe client saw, shared with the test.
#[derive(Default)]
struct Seen {
    submitted: Vec<String>,
    cancelled: Vec<String>,
    begun_detach: bool,
}

/// A client that accepts what it is sent and, once its detach has begun, answers a submit the way the
/// gatewayhub client does: with a terminal `OrderRejected`.
struct Probe {
    seen: Arc<Mutex<Seen>>,
    events: VecDeque<Event>,
}

impl ExecutionClient for Probe {
    fn submit(&mut self, request: &OrderRequest) {
        let mut seen = self.seen.lock().unwrap();
        let coid = request.client_order_id.clone();
        if seen.begun_detach {
            self.events.push_back(Event::OrderRejected(OrderRejected {
                cause: None,
                client_order_id: coid,
                reason: "the lease is closed".to_string().into(),
                ts: request.ts,
            }));
            return;
        }
        seen.submitted.push(coid.clone());
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: coid.clone(),
            ts: request.ts,
        }));
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: None,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.seen.lock().unwrap().cancelled.push(client_order_id.to_string());
    }
    fn begin_detach(&mut self) {
        self.seen.lock().unwrap().begun_detach = true;
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

fn probe_engine(route_key: &str) -> (Engine, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let client: Box<dyn ExecutionClient + Send> =
        Box::new(Probe { seen: Arc::clone(&seen), events: VecDeque::new() });
    let mut engine = dyn_engine("binance", "BTCUSDT", client);
    engine.route_key = route_key.to_string();
    (engine, seen)
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").unwrap()
}

fn stop_of_alt() -> NotRunningAccount {
    NotRunningAccount {
        venue: "binance".to_string(),
        account: Some(alt()),
        route_key: "binance#ALT".to_string(),
        tier: None,
        reason: "account inactive".to_string(),
    }
}

fn limit(coid: &str, account: Option<AccountLabel>) -> OrderRequest {
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

fn submit(handle: &vike_core::CoreHandle, coid: &str, account: Option<AccountLabel>) {
    handle
        .try_command(Command::Order(OrderIntent::Submit(Box::new(limit(coid, account)))))
        .unwrap();
}

fn balance(usdt: f64) -> Event {
    Event::AccountState(AccountState {
        venue: "binance".into(),
        balances: vec![("USDT".into(), usdt)],
        ts: 2,
        route_key: None,
    })
}

fn said(snap: &CoreSnapshot, needle: &str) -> bool {
    snap.recent_events.iter().any(|l| l.contains(needle))
}

/// The running core, what each client saw, and the snapshot cell.
type Rig =
    (CoreJoinGuard, Arc<Mutex<Seen>>, Arc<Mutex<Seen>>, Arc<arc_swap::ArcSwap<CoreSnapshot>>);

/// Two accounts of binance, each holding one resting order; returns the running core.
fn two_accounts_with_one_order_each() -> Rig {
    let (primary, default_seen) = probe_engine("binance");
    let (second, alt_seen) = probe_engine("binance#ALT");
    let handle = CoreJoinGuard::new(spawn_core_multi(primary, Vec::new(), test_config(10_000.0)));
    let cell = handle.snapshot_cell();
    handle.event_sender().add_account_engine(500.0, second).unwrap();
    wait_for_snapshot(&cell, 5, "the second engine is published", |s| {
        s.portfolio.venues.len() == 2
    });
    submit(&handle, "d1", Some(AccountLabel::Default));
    submit(&handle, "a1", Some(alt()));
    wait_for_snapshot(&cell, 5, "both orders rest", |s| s.orders.len() == 2);
    (handle, default_seen, alt_seen, cell)
}

#[test]
fn a_stop_cancels_only_its_own_orders_and_begins_only_its_own_detach() {
    let (handle, default_seen, alt_seen, cell) = two_accounts_with_one_order_each();
    handle.event_sender().stop_account_engine(stop_of_alt()).unwrap();
    wait_for_snapshot(&cell, 5, "the stop is said", |s| said(s, "ACCOUNT STOPPED"));
    // Read BEFORE the shutdown: the core's own teardown begins every engine's detach.
    {
        let alt_seen = alt_seen.lock().unwrap();
        assert_eq!(
            alt_seen.cancelled,
            vec!["a1".to_string()],
            "the stopped account's order is pulled"
        );
        assert!(
            !alt_seen.begun_detach,
            "phase one keeps the client attached: the venue's answers to the cancel still arrive"
        );
        let default_seen = default_seen.lock().unwrap();
        assert!(default_seen.cancelled.is_empty(), "the other account's order is not touched");
        assert!(!default_seen.begun_detach);
    }
    handle.event_sender().detach_account_engine("binance#ALT").unwrap();
    wait_for_snapshot(&cell, 5, "the detach is said", |s| said(s, "ACCOUNT DETACHED"));
    {
        assert!(
            alt_seen.lock().unwrap().begun_detach,
            "phase two: its client's detach began (the lease closes on its own thread)"
        );
        assert!(!default_seen.lock().unwrap().begun_detach, "and the other client is not detached");
    }
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.portfolio.venues.len(), 2, "the engine stays: its index is an attribution key");
    assert!(
        said(&snap, "account inactive"),
        "the reason the account is listed with is said in the ring: {:?}",
        snap.recent_events
    );
}

#[test]
fn an_order_that_still_reaches_a_stopped_account_dies_there_and_reaches_no_other_book() {
    let (handle, default_seen, alt_seen, cell) = two_accounts_with_one_order_each();
    handle.event_sender().stop_account_engine(stop_of_alt()).unwrap();
    wait_for_snapshot(&cell, 5, "the stop is said", |s| said(s, "ACCOUNT STOPPED"));
    handle.event_sender().detach_account_engine("binance#ALT").unwrap();
    wait_for_snapshot(&cell, 5, "the detach is said", |s| said(s, "ACCOUNT DETACHED"));
    submit(&handle, "late", Some(alt()));
    wait_for_snapshot(&cell, 5, "the late order is terminal", |s| {
        s.orders.iter().any(|o| o.client_order_id == "late" && o.status.is_terminal())
    });
    handle.shutdown_and_join();

    assert!(
        !alt_seen.lock().unwrap().submitted.contains(&"late".to_string()),
        "the dead client sent nothing"
    );
    assert!(
        !default_seen.lock().unwrap().submitted.contains(&"late".to_string()),
        "and the order did not fall through to the other account's book"
    );
}

/// Between phase one and the detach the client is still ATTACHED (it waits for the cancels' answers, up to
/// seconds), so an order that reached it then would rest at the venue after the mass cancel: nothing would
/// ever cancel it. The core refuses it itself, with a terminal reject, whoever sent it (a strategy included).
#[test]
fn an_order_between_phase_one_and_the_detach_is_refused_and_never_reaches_the_attached_client() {
    let (handle, default_seen, alt_seen, cell) = two_accounts_with_one_order_each();
    handle.event_sender().stop_account_engine(stop_of_alt()).unwrap();
    wait_for_snapshot(&cell, 5, "the stop is said", |s| said(s, "ACCOUNT STOPPED"));
    // NO detach yet: the client is attached.
    submit(&handle, "mid", Some(alt()));
    wait_for_snapshot(&cell, 5, "the mid-stop order is terminal", |s| {
        s.orders.iter().any(|o| o.client_order_id == "mid" && o.status.is_terminal())
    });
    assert!(
        !alt_seen.lock().unwrap().submitted.contains(&"mid".to_string()),
        "the attached client of a stopped account was sent nothing"
    );
    assert!(!default_seen.lock().unwrap().submitted.contains(&"mid".to_string()));
    handle.shutdown_and_join();
}

#[test]
fn after_a_stop_the_venue_has_one_account_and_an_untargeted_order_is_not_ambiguous() {
    let (handle, default_seen, _alt_seen, cell) = two_accounts_with_one_order_each();
    handle.event_sender().stop_account_engine(stop_of_alt()).unwrap();
    wait_for_snapshot(&cell, 5, "the stop is said", |s| said(s, "ACCOUNT STOPPED"));
    submit(&handle, "u1", None);
    wait_for_snapshot(&cell, 5, "the untargeted order rests", |s| {
        s.orders.iter().any(|o| o.client_order_id == "u1")
    });
    handle.shutdown_and_join();

    assert!(
        default_seen.lock().unwrap().submitted.contains(&"u1".to_string()),
        "with one running account left, an order that names only the exchange is that account's"
    );
}

#[test]
fn a_stop_of_an_account_that_does_not_run_is_a_note_and_nothing_else() {
    let (primary, default_seen) = probe_engine("binance");
    let handle = CoreJoinGuard::new(spawn_core_multi(primary, Vec::new(), test_config(10_000.0)));
    let cell = handle.snapshot_cell();
    handle.event_sender().stop_account_engine(stop_of_alt()).unwrap();
    wait_for_snapshot(&cell, 5, "the refusal is said", |s| said(s, "ACCOUNT STOP ignored"));
    // A detach of an account that was never stopped (a running one) is a note too.
    handle.event_sender().detach_account_engine("binance").unwrap();
    wait_for_snapshot(&cell, 5, "the second refusal is said", |s| {
        said(s, "ACCOUNT DETACH ignored")
    });
    // Read BEFORE the shutdown: the core's own teardown begins every engine's detach.
    assert!(!default_seen.lock().unwrap().begun_detach, "no other account is stopped by a typo");
    assert_eq!(cell.load_full().portfolio.venues.len(), 1);
    handle.shutdown_and_join();
}

#[test]
fn a_stopped_account_starts_again_as_a_new_engine_that_every_lookup_prefers() {
    let (handle, _default_seen, alt_seen, cell) = two_accounts_with_one_order_each();
    handle.event_sender().stop_account_engine(stop_of_alt()).unwrap();
    wait_for_snapshot(&cell, 5, "the stop is said", |s| said(s, "ACCOUNT STOPPED"));

    let (again, again_seen) = probe_engine("binance#ALT");
    handle.event_sender().add_account_engine(500.0, again).unwrap();
    wait_for_snapshot(&cell, 5, "the restart is published", |s| s.portfolio.venues.len() == 3);
    // The account's own lane stamps its key: the NEWEST engine of the key takes it.
    handle.event_sender().routed("binance#ALT").blocking_send(balance(700.0)).unwrap();
    submit(&handle, "a2", Some(alt()));
    wait_for_snapshot(&cell, 5, "the new order rests", |s| {
        s.orders.iter().any(|o| o.client_order_id == "a2" && !o.status.is_terminal())
    });
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.portfolio.venues[1].balance, 0.0, "the stopped engine folded none of it");
    assert_eq!(snap.portfolio.venues[2].balance, 700.0, "the new engine did");
    assert!(again_seen.lock().unwrap().submitted.contains(&"a2".to_string()));
    assert!(
        !alt_seen.lock().unwrap().submitted.contains(&"a2".to_string()),
        "the order did not go to the stopped engine"
    );
    assert!(!said(&snap, "ACCOUNT ADD refused"), "{:?}", snap.recent_events);
}

// ---- a strategy MOUNTED on the account -------------------------------------------------------------

/// Submits, once armed, one tagged limit and one bracket on its next quote tick.
struct Acts {
    armed: Arc<std::sync::atomic::AtomicBool>,
}

impl vike_model::Strategy<vike_core::LiveBroker> for Acts {
    fn on_quote_tick(
        &mut self,
        broker: &mut vike_core::LiveBroker,
        _q: &vike_marketdata::QuoteTick,
    ) {
        if self.armed.swap(false, std::sync::atomic::Ordering::Relaxed) {
            broker.submit_limit_tagged("q", 1, 1.0, 100.0);
            broker.submit_bracket(1, 1.0, Some(100.0), 95.0, 110.0);
        }
    }
}

/// Two accounts of binance; a strategy mounted on the ALT one.
fn mounted_rig() -> (Rig, Arc<std::sync::atomic::AtomicBool>) {
    let (primary, default_seen) = probe_engine("binance");
    let (second, alt_seen) = probe_engine("binance#ALT");
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let config = vike_core::CoreConfig {
        strategy: Some(vike_core::StrategyMount {
            account: Some(alt()),
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(Acts { armed: Arc::clone(&armed) }),
        }),
        ..test_config(10_000.0)
    };
    let handle = CoreJoinGuard::new(spawn_core_multi(primary, vec![(500.0, second)], config));
    let cell = handle.snapshot_cell();
    ((handle, default_seen, alt_seen, cell), armed)
}

fn tick(handle: &vike_core::CoreHandle, ts: i64) {
    handle.tick_sender().quote(crate::kit::events::binance_quote(ts)).unwrap();
}

/// A mounted strategy reaches its account through `EngineRoute::Mount`, past the edge's refusal. Between
/// phase one and the detach its limit AND its bracket are refused by the core; nothing is sent to the
/// attached client.
#[test]
fn a_mounted_strategys_bracket_between_phase_one_and_the_detach_is_refused() {
    let ((handle, _default_seen, alt_seen, cell), armed) = mounted_rig();
    handle.event_sender().stop_account_engine(stop_of_alt()).unwrap();
    wait_for_snapshot(&cell, 5, "the stop is said", |s| said(s, "ACCOUNT STOPPED"));
    armed.store(true, std::sync::atomic::Ordering::Relaxed);
    tick(&handle, 1);
    wait_for_snapshot(&cell, 5, "the limit and the bracket legs are all refused", |s| {
        s.orders.len() >= 4 && s.orders.iter().all(|o| o.status.is_terminal())
    });
    assert!(
        alt_seen.lock().unwrap().submitted.is_empty(),
        "the attached client of a stopped account was sent {:?}",
        alt_seen.lock().unwrap().submitted
    );
    handle.shutdown_and_join();
}

/// After stop, detach and a start of the same account, the mounted strategy trades on the NEW engine.
#[test]
fn a_mounted_strategy_trades_again_after_its_account_stops_and_starts() {
    let ((handle, _default_seen, _old_alt_seen, cell), armed) = mounted_rig();
    handle.event_sender().stop_account_engine(stop_of_alt()).unwrap();
    wait_for_snapshot(&cell, 5, "the stop is said", |s| said(s, "ACCOUNT STOPPED"));
    handle.event_sender().detach_account_engine("binance#ALT").unwrap();
    let (again, again_seen) = probe_engine("binance#ALT");
    handle.event_sender().add_account_engine(500.0, again).unwrap();
    wait_for_snapshot(&cell, 5, "the restart is published", |s| s.portfolio.venues.len() == 3);
    armed.store(true, std::sync::atomic::Ordering::Relaxed);
    tick(&handle, 2);
    wait_for_snapshot(&cell, 5, "the mounted strategy's order rests on the new engine", |s| {
        s.orders.iter().any(|o| !o.status.is_terminal())
    });
    assert!(
        !again_seen.lock().unwrap().submitted.is_empty(),
        "the new engine's client was sent the mounted strategy's orders"
    );
    handle.shutdown_and_join();
}
