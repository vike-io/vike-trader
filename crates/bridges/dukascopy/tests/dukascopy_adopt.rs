//! A NEW sidecar takes up what the old one left (A34): the Rust half, over real child processes of the scripted fake
//! (`src/bin/fake_jforex_bridge.rs`, mode `adopt-die-once`: a sidecar whose `ready` announces `adopt`, whose first launch
//! dies on a `cancel` of `DIE`, and whose later launches send the `position` lines and the `adopted` census the test
//! wrote into `<log>.positions` / `<log>.adopted`).
//!
//! What these prove: a native protection (stop-loss, take-profit) that rested when an ADOPTING sidecar died is not ended
//! on the node, it is sent again to the next sidecar, which takes the price over (the answer is not a second
//! acceptance), and a protection the venue no longer holds ends `OrderCanceled` in the venue's words; the census
//! lifts the `stranded` refusal for an order it adopted and names a cancel of one it does not know; and a symbol the
//! node holds a position in that the census lists no order on is flat at the venue. They do not prove the JForex half
//! (what `engine.getOrders()` lists at a real `onStart`): that is `StrategyBridge.adoptOpenOrders` (`StopLossTest.java`
//! over SDK stand-ins) and the live proof in `crates/bridges/dukascopy/CLAUDE.md`. A jar that does NOT adopt keeps
//! the old ending at a death (`dukascopy_restart.rs`'s `a_resting_stop_loss_at_a_sidecars_death_ends_canceled_in_words...`).

use std::time::{Duration, Instant};

use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient, SidecarState};
use vike_exec::recon::ReconClient;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Fold, Ingest, Outbox,
    RiskGate, event_channel,
};
use vike_model::events::Event;
use vike_model::{ManagedOrder, OrderRequest, OrderStatus, RiskLimits};
use vike_venues::venue_timings::Reconnect;

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

/// A restart policy in milliseconds: 40 ms, doubling to 160 ms.
const FAST: Reconnect =
    Reconnect::new(Duration::from_millis(40), Duration::from_millis(160), "a test");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
        ledger_mount: None,
    }
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Market,
        price: None,
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: 1,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

fn resting_limit(coid: &str) -> OrderRequest {
    OrderRequest { order_type: vike_model::OrderType::Limit, price: Some(1.1), ..order(coid) }
}

/// A native stop-loss on a long EURUSD position: a reduce-only SELL stop.
fn resting_stop(coid: &str) -> OrderRequest {
    OrderRequest {
        order_type: vike_model::OrderType::Stop,
        side: -1,
        trigger_price: Some(1.09),
        reduce_only: true,
        ..order(coid)
    }
}

/// A native take-profit on a long EURUSD position: a reduce-only SELL limit.
fn resting_take_profit(coid: &str) -> OrderRequest {
    OrderRequest {
        order_type: vike_model::OrderType::Limit,
        side: -1,
        price: Some(1.2),
        reduce_only: true,
        ..order(coid)
    }
}

fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(Ingest::Event(ev)) = rx.try_recv() {
        out.push(ev);
    }
    out
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn launches(log: &std::path::Path) -> Vec<u32> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.parse().expect("pid line"))
        .collect()
}

fn scratch(tag: &str) -> vike_paths::scratch::ScratchDir {
    vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), tag).expect("scratch dir")
}

fn adopting_client(
    log: &std::path::Path,
) -> (DukascopyExecutionClient, tokio::sync::mpsc::Receiver<Ingest>) {
    let (events, rx) = event_channel(64);
    let client = DukascopyExecutionClient::spawn_with_restart(
        BRIDGE,
        &["adopt-die-once".into(), log.display().to_string()],
        &config(),
        events,
        FAST,
    )
    .expect("ready");
    (client, rx)
}

/// The node's real order FSM holding `req` as a registered order, as the engine of a dukascopy account would.
fn engine_holding(req: &OrderRequest) -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "dukascopy", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "dukascopy",
        "EURUSD",
    );
    engine.registry.insert(req.client_order_id.clone(), ManagedOrder::new(req.clone()));
    engine
}

fn folds(engine: &mut ExecutionEngine<RecordingClient>, event: &Event) -> bool {
    matches!(engine.on_event(event, &mut Outbox::default()), Fold::Applied)
}

fn status(engine: &ExecutionEngine<RecordingClient>, coid: &str) -> OrderStatus {
    engine.registry[coid].status
}

/// The ladder a healthy sidecar gives a market order: Submitted, Accepted, the bare Fill, the OrderFilled wrap.
fn expect_filled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str) {
    assert!(matches!(recv_event(rx), Event::OrderSubmitted(e) if e.client_order_id == coid));
    assert!(matches!(recv_event(rx), Event::OrderAccepted(e) if e.client_order_id == coid));
    assert!(matches!(recv_event(rx), Event::Fill(f) if f.client_order_id == coid));
    assert!(matches!(recv_event(rx), Event::OrderFilled(e) if e.client_order_id == coid));
}

fn sidecar_up(client: &DukascopyExecutionClient) -> bool {
    client.sidecar_state() == SidecarState::Up
}

/// **A protection that rested when an ADOPTING sidecar died stays accepted on the node, is sent again to the next
/// sidecar, and is taken over.** The wave-1 ending (`OrderCanceled "bridge died ... the venue may STILL hold the
/// stop-loss"`) is for a jar that cannot take it over; this jar can, so the node keeps believing in the protection the
/// venue provably holds. Driven through the real order FSM: the assertion is what the node ends up BELIEVING, and that the
/// answer to the stop sent again was not a second acceptance (the FSM would have logged an invalid transition) and not
/// an ending (a dropped terminal). A cancel of it works afterwards, as it could not on a sidecar that knows nothing.
///
/// ⚠ Mutation proof: make the EOF drain end an acknowledged protection regardless of `adopts`
/// (`keeps_protections`) and the first assertion after the death goes red on the `OrderCanceled` it reads.
#[test]
fn a_protection_resting_at_an_adopting_sidecars_death_is_sent_again_and_taken_over() {
    for (what, protection) in
        [("stop-loss", resting_stop("p1")), ("take-profit", resting_take_profit("p1"))]
    {
        let dir = scratch("vike-duka-adopt-protection");
        let log = dir.path().join("launches.txt");
        let (mut client, mut rx) = adopting_client(&log);
        let mut engine = engine_holding(&protection);

        client.submit(&protection);
        for _ in 0..2 {
            let event = recv_event(&mut rx);
            assert!(folds(&mut engine, &event), "{what}: {event:?}");
        }
        assert_eq!(status(&engine, "p1"), OrderStatus::Accepted, "{what}");

        // The sidecar dies with the protection resting; the venue keeps it (the fake's book file).
        client.cancel("DIE");
        wait_until("the second sidecar to be up", || {
            launches(&log).len() == 2 && sidecar_up(&client)
        });

        // Its cancel is the first thing the new sidecar answers after the protection sent again: if the take-over's
        // acceptance had been passed on, or the death had ended the protection, the first event would be one of those.
        client.cancel("p1");
        let ended = recv_event(&mut rx);
        match &ended {
            Event::OrderCanceled(c) => {
                assert_eq!(c.client_order_id, "p1", "{what}");
                assert!(
                    !c.reason.contains("bridge died"),
                    "{what}: this is the operator's cancel, not the death's: {}",
                    c.reason
                );
            }
            other => {
                panic!("{what}: the first event after the restart is the cancel's, got {other:?}")
            }
        }
        assert!(
            folds(&mut engine, &ended),
            "{what}: the FSM takes the cancel of an Accepted order"
        );
        assert_eq!(status(&engine, "p1"), OrderStatus::Canceled, "{what}");
        assert_eq!(
            engine.dropped_terminal_on_live, 0,
            "{what}: nothing was dropped on a live order"
        );

        // The venue's book: taken over (not added twice) and then taken off by the cancel; the cancels the launches saw.
        client.detach();
        let cancels =
            std::fs::read_to_string(format!("{}.cancels", log.display())).expect("cancel log");
        assert_eq!(cancels.lines().collect::<Vec<_>>(), ["DIE", "p1"], "{what}");
        assert!(drain(&mut rx).is_empty(), "{what}: nothing else was said about p1");
    }
}

/// **A protection the venue no longer holds ends `OrderCanceled` in the venue's words at the restart.** The stop-loss
/// fired (or was taken off in the platform) while no sidecar listened: the new sidecar refuses the stop sent again with
/// `no open long order to protect`, which the node reads as the end of an order it had accepted (a cancel the core takes;
/// a rejection it would DROP), and says what happened.
#[test]
fn a_protection_the_venue_lost_while_no_sidecar_listened_ends_canceled_in_the_venues_words() {
    let dir = scratch("vike-duka-adopt-lost");
    let log = dir.path().join("launches.txt");
    let (mut client, mut rx) = adopting_client(&log);
    let stop = resting_stop("p1");
    let mut engine = engine_holding(&stop);

    client.submit(&stop);
    for _ in 0..2 {
        let event = recv_event(&mut rx);
        assert!(folds(&mut engine, &event), "{event:?}");
    }
    assert_eq!(status(&engine, "p1"), OrderStatus::Accepted);

    // The venue's stop-loss fires while the sidecar is gone: its book no longer lists the stop.
    std::fs::write(format!("{}.book", log.display()), "").expect("book file");
    client.cancel("DIE");

    let ended = recv_event(&mut rx);
    match &ended {
        Event::OrderCanceled(c) => {
            assert_eq!(c.client_order_id, "p1");
            assert!(c.reason.contains("no open long order to protect"), "{}", c.reason);
            assert!(
                c.reason.contains("the venue ended this order after accepting it"),
                "{}",
                c.reason
            );
        }
        other => {
            panic!("a protection the venue lost ends with a cancel the core takes, got {other:?}")
        }
    }
    assert!(folds(&mut engine, &ended), "the order FSM must take the end of an Accepted stop");
    assert_eq!(status(&engine, "p1"), OrderStatus::Canceled);
    assert_eq!(engine.dropped_terminal_on_live, 0);
    client.detach();
}

/// **The census lifts the `stranded` refusal for what the new sidecar adopted, and a cancel of an id it does not know is
/// refused by name.** A resting limit in flight at the death is rejected `bridge died` (dropped by the core on an
/// accepted order) and remembered as stranded, so a cancel of it was refused by name; the adopting sidecar lists it
/// (`<log>.adopted`), and the cancel then reaches it and is served. An id the census does not list (and nobody
/// stranded) reaches the sidecar and gets the sidecar's own named refusal instead of silence.
#[test]
fn the_census_lifts_the_stranded_refusal_for_an_adopted_order_and_a_stranger_is_refused_by_name() {
    let dir = scratch("vike-duka-adopt-census");
    let log = dir.path().join("launches.txt");
    std::fs::write(format!("{}.adopted", log.display()), "c1 EURUSD 1 OPENED 0 0 0\n")
        .expect("adopted file");
    let (mut client, mut rx) = adopting_client(&log);

    client.submit(&resting_limit("c1"));
    assert!(matches!(recv_event(&mut rx), Event::OrderSubmitted(e) if e.client_order_id == "c1"));
    assert!(matches!(recv_event(&mut rx), Event::OrderAccepted(e) if e.client_order_id == "c1"));
    client.cancel("DIE");
    assert!(
        matches!(recv_event(&mut rx), Event::OrderRejected(e) if e.client_order_id == "c1" && e.reason.starts_with("bridge died")),
        "an order that is not a protection keeps the rejection at a death"
    );
    wait_until("the second sidecar to be up", || launches(&log).len() == 2 && sidecar_up(&client));

    // The census arrives right after `ready`: until the reader has handled it the cancel is still refused as stranded.
    let mut served = false;
    for _ in 0..2000 {
        client.cancel("c1");
        match recv_event(&mut rx) {
            Event::OrderCancelRejected(r) => {
                assert!(r.reason.contains("restarted after this order was placed"), "{}", r.reason);
                std::thread::sleep(Duration::from_millis(5));
            }
            Event::OrderCanceled(c) => {
                assert_eq!(c.client_order_id, "c1");
                served = true;
                break;
            }
            other => panic!("expected the refusal or the cancel, got {other:?}"),
        }
    }
    assert!(served, "the census never lifted the stranded refusal of the adopted order");

    client.cancel("ghost");
    match recv_event(&mut rx) {
        Event::OrderCancelRejected(r) => {
            assert_eq!(r.client_order_id, "ghost");
            assert!(r.reason.contains("knows no order"), "{}", r.reason);
        }
        other => panic!("a cancel of an id nobody holds is refused by name, got {other:?}"),
    }
    client.detach();
}

/// **A symbol the node holds a position in and the venue lists no order on is FLAT at the venue.** The reconcile client
/// (`recon_client`) PULLS what the venue lists (the fake's census files stand for `engine.getOrders()`), and states a flat
/// row for a symbol it heard of from a fill, so the flat symbol reads 0 where the push-line snapshot this replaced read
/// nothing (and, for a close the sidecar never heard, the node's own last belief); a symbol the venue lists keeps the
/// venue's figure, and a line for a symbol no fill of this client explains is adopted as a baseline and BOOKS NOTHING
/// (no synthesized fill reaches the node).
#[test]
fn a_symbol_the_node_holds_and_the_venue_lists_no_order_on_is_flat_at_the_venue() {
    fn venue_size(client: &DukascopyExecutionClient, symbol: &str) -> Option<f64> {
        client
            .recon_client()
            .fetch_position_status_reports()
            .expect("the venue's answer")
            .into_iter()
            .find(|p| p.symbol == symbol)
            .map(|p| p.qty)
    }

    // (a) the venue lists nothing: EURUSD, long 1000 on the node, is flat at the venue.
    let dir = scratch("vike-duka-adopt-flat");
    let log = dir.path().join("launches.txt");
    let (mut client, mut rx) = adopting_client(&log);
    client.submit(&order("m1")); // a market buy: the fake fills it and the shadow folds the bare fill
    expect_filled(&mut rx, "m1");
    // The venue (the fake lists no position) is flat on a symbol the node holds: the pull says so at once, with no
    // restart and no push line to wait for.
    assert_eq!(venue_size(&client, "EURUSD"), Some(0.0), "the venue lists no order on it");
    client.cancel("DIE");
    wait_until("the second sidecar to be up", || launches(&log).len() == 2 && sidecar_up(&client));
    wait_until("the census to name EURUSD flat", || venue_size(&client, "EURUSD") == Some(0.0));
    client.detach();

    // (b) the venue lists a position on EURUSD (a line before the census) and one on USDJPY no fill explains.
    let dir = scratch("vike-duka-adopt-listed");
    let log = dir.path().join("launches.txt");
    std::fs::write(
        format!("{}.positions", log.display()),
        "EURUSD 1000.0 1.1\nUSDJPY 2000.0 150.0\n",
    )
    .expect("positions file");
    let (mut client, mut rx) = adopting_client(&log);
    client.submit(&order("m1"));
    expect_filled(&mut rx, "m1");
    client.cancel("DIE");
    wait_until("the second sidecar to be up", || launches(&log).len() == 2 && sidecar_up(&client));
    wait_until("the lines to arrive", || {
        venue_size(&client, "EURUSD") == Some(1000.0)
            && venue_size(&client, "USDJPY") == Some(2000.0)
    });
    // Everything the sidecar said before this answer was handled before it (one ordered stream): the census too.
    client.submit(&order("m2"));
    expect_filled(&mut rx, "m2");
    assert_eq!(venue_size(&client, "EURUSD"), Some(1000.0), "a symbol with a line is not flat");
    assert_eq!(venue_size(&client, "USDJPY"), Some(2000.0));
    let extra = drain(&mut rx);
    assert!(
        extra.iter().all(|e| !matches!(e, Event::Fill(_))),
        "the baseline of a position no fill explains books nothing: {extra:?}"
    );
    client.detach();
}
