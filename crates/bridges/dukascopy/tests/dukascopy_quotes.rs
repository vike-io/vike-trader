//! The PRICE LANE end to end against the scripted fake bridge (`src/bin/fake_jforex_bridge.rs`) - no Java, no
//! network. The exec client's reader attaches the sidecar to a `QuoteBus` on `ready`; a consumer's `want` writes a
//! `subscribe` line the fake answers with a `tick`; the reader hands the tick to the bus, which judges it and gives
//! it to the listener. What this proves is the Rust half over the real stdio protocol; the Java half is
//! `jforex-bridge/src/test/java/vike/jforex/TickLaneTest.java`, and a real tick cadence needs a live login.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_dukascopy::quotes::{Listening, PriceTick, QuoteBus};
use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient};
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
        ledger_mount: None,
    }
}

fn spawn(mode: &str, bus: &Arc<QuoteBus>) -> DukascopyExecutionClient {
    // Nothing here reads the ingest lane: a tick is not an `Event`, and the fake sends no other.
    let (events, _rx) = event_channel(64);
    let args: Vec<String> = if mode.is_empty() { vec![] } else { vec![mode.to_string()] };
    DukascopyExecutionClient::spawn_with_program_quoting(
        BRIDGE,
        &args,
        &config(),
        events,
        Arc::clone(bus),
    )
    .expect("the fake sidecar says ready")
}

/// A consumer collecting what the bus hands it.
fn consumer(bus: &Arc<QuoteBus>) -> (Listening, Arc<Mutex<Vec<PriceTick>>>) {
    let got: Arc<Mutex<Vec<PriceTick>>> = Arc::default();
    let sink = Arc::clone(&got);
    let listening = bus.listen(Arc::new(move |t: &PriceTick| sink.lock().unwrap().push(t.clone())));
    (listening, got)
}

/// Poll `ready` for up to ten seconds; the failure names what never happened.
fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn count(got: &Arc<Mutex<Vec<PriceTick>>>) -> usize {
    got.lock().unwrap().len()
}

#[test]
fn a_wanted_symbol_is_quoted_by_the_sidecar_and_handed_to_the_listener() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);
    let mut client = spawn("", &bus);

    listening.want("EURUSD").expect("a sidecar that announced ticks takes the subscription");
    wait_for("the first tick", || count(&got) == 1);
    let tick = got.lock().unwrap()[0].clone();
    assert_eq!(tick.symbol, "EURUSD");
    assert!((tick.bid - 1.1).abs() < 1e-9 && tick.ask > tick.bid, "{tick:?}");
    let age = vike_model::now_ms() - tick.ts;
    assert!((0..10_000).contains(&age), "the tick carries its own, current time: {age} ms old");

    client.detach();
}

#[test]
fn a_bad_tick_never_reaches_the_listener() {
    // The fake spoils EURUSD only; XAUUSD is asked second and answered second, so when its tick has arrived the
    // spoiled one has already been judged.
    for mode in ["ticks-stale", "ticks-one-sided", "ticks-ghost"] {
        let bus = QuoteBus::new();
        let (listening, got) = consumer(&bus);
        let mut client = spawn(mode, &bus);
        listening.want("EURUSD").expect("accepted");
        listening.want("XAUUSD").expect("accepted");
        wait_for("the good tick", || count(&got) >= 1);
        // a short settle: a tick wrongly let through would be right behind
        std::thread::sleep(Duration::from_millis(100));
        let symbols: Vec<String> = got.lock().unwrap().iter().map(|t| t.symbol.clone()).collect();
        assert_eq!(
            symbols,
            ["XAUUSD"],
            "{mode}: only the good tick of the asked symbol is a quote"
        );
        client.detach();
    }
}

#[test]
fn a_jar_that_announced_no_ticks_is_refused_in_words() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);
    let mut client = spawn("old-jar", &bus);
    let why = listening.want("EURUSD").expect_err("an old jar");
    assert!(why.contains("predates the price lane"), "{why}");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(count(&got), 0);
    client.detach();
}

/// A sidecar that goes away takes the lane with it (a `want` is refused in words), and the wants outlive it: the
/// next sidecar is told to quote them without anybody asking again.
#[test]
fn a_dead_sidecar_is_detached_and_its_wants_are_replayed_to_the_next_one() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);

    let mut first = spawn("", &bus);
    listening.want("EURUSD").expect("accepted");
    wait_for("the first sidecar's tick", || count(&got) == 1);
    // shutdown + reap: the reader hits EOF, detaches the lane, and is joined before this returns
    first.detach();
    let why = listening.want("XAUUSD").expect_err("the sidecar is gone");
    assert!(why.contains("no JForex sidecar is running"), "{why}");

    let mut second = spawn("", &bus);
    wait_for("the replayed subscription's tick", || count(&got) == 2);
    assert!(got.lock().unwrap().iter().all(|t| t.symbol == "EURUSD"));
    second.detach();
}

#[test]
fn closing_the_consumer_stops_the_listener() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);
    let mut client = spawn("", &bus);
    listening.want("EURUSD").expect("accepted");
    wait_for("the tick", || count(&got) == 1);
    drop(listening);
    // a new consumer starts with no wants: the previous one's EURUSD is not quoted to it
    let (next, next_got) = consumer(&bus);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(count(&next_got), 0);
    next.want("XAUUSD").expect("a fresh consumer asks for itself");
    wait_for("its own tick", || count(&next_got) == 1);
    assert_eq!(next_got.lock().unwrap()[0].symbol, "XAUUSD");
    client.detach();
}

/// A jar that answers a `subscribe` and DECLINES an instrument: the want is refused with the jar's reason and the place
/// it took is free; the other symbol of the same sidecar is priced as usual.
#[test]
fn a_declined_instrument_is_refused_with_the_jars_reason_and_frees_its_place() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);
    let mut client = spawn("ticks-nack", &bus);

    let why = listening.want("GBPUSD").expect_err("the sidecar declines GBPUSD");
    assert!(why.contains("declined") && why.contains("scripted decline"), "{why}");
    // the place is free: a declined symbol can be asked again (and is declined again), and the cap is not used up
    listening.want("GBPUSD").expect_err("declined again, not remembered as wanted");
    listening.want("EURUSD").expect("the other symbol is priced");
    wait_for("the tick of the accepted symbol", || count(&got) == 1);
    assert_eq!(got.lock().unwrap()[0].symbol, "EURUSD");
    client.detach();
}

/// The jar of the release before the answers announces `ticks` and no `tick-acks`: its `subscribe` is not waited for
/// (the ack wait is five seconds) and prices as it always did.
#[test]
fn a_jar_that_announces_ticks_but_no_answers_is_priced_without_a_wait() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);
    let mut client = spawn("ticks-no-acks", &bus);
    let started = Instant::now();
    listening.want("EURUSD").expect("accepted");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "waited {:?} for an answer",
        started.elapsed()
    );
    wait_for("the tick", || count(&got) == 1);
    client.detach();
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

/// Receive the next Event from the ingest lane, failing loudly after 10s.
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

/// **The guarantee: a fault in the price consumer never stops the fills.** The consumer panics on its first tick (on the
/// bus's own thread, caught and detached); the exec client's reader, which pumps fills, keeps pumping: an order placed
/// afterwards runs its whole lifecycle through the ingest lane.
#[test]
fn a_panicking_consumer_never_stops_the_fills() {
    let bus = QuoteBus::new();
    let listening = bus.listen(Arc::new(|_: &PriceTick| {
        panic!("a consumer bug");
    }));
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program_quoting(
        BRIDGE,
        &[],
        &config(),
        events,
        Arc::clone(&bus),
    )
    .expect("the fake sidecar says ready");

    // The fake answers a want with its tick at once and the consumer panics on that tick, on the bus's own thread:
    // on a loaded box the panic can land before `want` has returned, and `want` then says the stream ended after a
    // fault in its consumer (the v0.1.81 tag run's first attempt). That IS the fault under test; any other refusal
    // is not.
    if let Err(why) = listening.want("EURUSD") {
        assert!(why.contains("after a fault in its consumer"), "an unexpected refusal: {why}");
    }
    wait_for("the faulty consumer to be detached", || listening.ensure_current().is_err());

    client.submit(&order("c1"));
    assert!(matches!(recv_event(&mut rx), Event::OrderSubmitted(e) if e.client_order_id == "c1"));
    assert!(matches!(recv_event(&mut rx), Event::OrderAccepted(e) if e.client_order_id == "c1"));
    assert!(matches!(recv_event(&mut rx), Event::Fill(f) if f.client_order_id == "c1"));
    assert!(matches!(recv_event(&mut rx), Event::OrderFilled(e) if e.client_order_id == "c1"));
    client.detach();
}

// ---- the venue's terms of a metal or a CFD, over the real stdio protocol --------------------------------------

/// **The terms line travels the stdio protocol**: a `venue_terms` ask subscribes the instrument by its WIRE spelling (a CFD
/// by the SDK's slashed name), the fake writes the `terms` line before the instrument's first tick, the reader hands it to
/// the bus, and the figures come back whole. A second ask is answered from what the bus kept and writes nothing.
#[test]
fn a_metal_and_a_cfd_state_their_terms_over_the_protocol() {
    let bus = QuoteBus::new();
    let mut client = spawn("", &bus);
    let gold = bus.venue_terms("XAUUSD", Duration::from_secs(10)).expect("gold's terms");
    assert_eq!((gold.class.as_str(), gold.min, gold.step), ("METAL", Some(1.0), Some(1.0)));
    let copper =
        bus.venue_terms("copper.cmd/usd", Duration::from_secs(10)).expect("copper's terms");
    assert_eq!(copper.symbol, "COPPER.CMD/USD");
    assert_eq!(
        (copper.class.as_str(), copper.min, copper.quote.as_deref()),
        ("CFD", Some(20.0), Some("USD"))
    );
    let silver = bus.venue_terms("XAG/USD", Duration::from_secs(10)).expect("silver's terms");
    assert_eq!((silver.min, silver.step), (Some(50.0), Some(10.0)));
    // kept: asking again is instant and needs no sidecar round trip
    let again = bus.venue_terms("XAUUSD", Duration::from_millis(1)).expect("kept");
    assert_eq!(again, gold);
    client.detach();
}

/// The facts a gatewayhub answers from those terms, over the same fake: the metal's and the CFDs' grid are the venue's,
/// and GAS.CMD/USD (100 units per contract) is refused as a verdict. Pins the chain `venue_terms` -> `facts_from_terms`.
#[test]
fn the_terms_the_fake_states_admit_a_metal_and_a_cfd_and_refuse_a_contract_of_a_hundred() {
    use vike_dukascopy::any_symbol::{FactsBasis, facts_basis, facts_from_terms};
    let bus = QuoteBus::new();
    let mut client = spawn("", &bus);
    for (symbol, min, step, tick) in [
        ("XAUUSD", 1.0, 1.0, 0.001),
        ("XAGUSD", 50.0, 10.0, 0.001),
        ("COPPER.CMD/USD", 20.0, 1.0, 0.0001),
        ("AAPL.US/USD", 1.0, 1.0, 0.001),
    ] {
        let Ok(FactsBasis::VenueTerms(row)) = facts_basis(symbol) else { panic!("{symbol}") };
        let terms = bus.venue_terms(symbol, Duration::from_secs(10)).expect(symbol);
        let (props, quote) =
            facts_from_terms(row, &terms).unwrap_or_else(|e| panic!("{symbol}: {e}"));
        assert_eq!(
            (props.min_qty, props.step_size, props.tick_size),
            (min, step, tick),
            "{symbol}"
        );
        assert_eq!(quote, "USD", "{symbol}");
    }
    let Ok(FactsBasis::VenueTerms(gas)) = facts_basis("GAS.CMD/USD") else { panic!("gas") };
    let terms = bus.venue_terms("GAS.CMD/USD", Duration::from_secs(10)).expect("gas terms");
    let why = facts_from_terms(gas, &terms).expect_err("100 per contract");
    assert!(why.contains("100 units of the underlying per contract"), "{why}");
    client.detach();
}

/// A jar that announces no `terms`, and a sidecar that announces it and never states a line (a closed market), are each
/// refused in words: the first before anything is written to the sidecar, the second after the wait.
#[test]
fn a_jar_without_terms_and_a_silent_market_are_refused_in_words() {
    let bus = QuoteBus::new();
    let mut old = spawn("ticks-no-terms", &bus);
    let why =
        bus.venue_terms("XAUUSD", Duration::from_millis(200)).expect_err("a jar without terms");
    assert!(why.contains("predates the venue's terms") && why.contains("`terms`"), "{why}");
    old.detach();

    let quiet_bus = QuoteBus::new();
    let mut quiet = spawn("terms-silent", &quiet_bus);
    let why = quiet_bus
        .venue_terms("XAUUSD", Duration::from_millis(300))
        .expect_err("a closed market states nothing");
    assert!(why.contains("stated no terms of `XAUUSD`") && why.contains("closed market"), "{why}");
    quiet.detach();
}
