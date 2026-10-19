//! **The lane twin's OWN symbol is priced in the twin ENGINE** (v0.1.80 independent review b, finding 1).
//!
//! The core hands a venue's quote, trade and book to the venue's DEFAULT engine and to the labelled engines
//! that hold a symbol admitted on demand (`vike-core`'s `CoreThread::admitted_fanout`). The twin's own symbol
//! is its MOUNTED one, so a pump tick for `BTCUSDT.P` reaches no price board of the twin, and the door's
//! `AdmitSymbol` for it is a no-op (`ExecutionEngine::admit_symbol` answers `Mounted` before it applies
//! `initial_mark`). An order there was judged with NO mark: a market order at notional zero with every cap
//! passing, a limit order against its own price.
//!
//! Every test sees the fact through an exposure ceiling both engines carry: an order the ceiling refuses AT A
//! MARK passes where the engine holds none, and a later order the ceiling always allows (the sentinel) is
//! the clock, because the core folds the lane in order.
//!
//! The first test pins the core's routing (the measured-crate fact the rest works around); the others drive
//! the production chain, `TapSink` over `CoreLaneSink`, into a REAL two-engine core.

use super::*;
use crate::feeds::price_tap::{PriceTap, TapSink};
use vike_data::LiveDataSink;
use vike_tradehub_core::CoreLaneSink;

/// The per-symbol exposure ceiling both engines carry: 0.1 BTC at 74,300 is 7,430, far over it.
const CAP: f64 = 1_000.0;

fn twin_order(coid: &str, order_type: &str, price: Option<f64>, reduce_only: bool) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: if reduce_only { 0.001 } else { 0.1 },
        order_type: order_type.into(),
        price,
        trigger_price: None,
        reduce_only,
        // The Trade window, the CLI and Telegram leave the default account unnamed: the door addresses it.
        account: None,
    })
}

/// The facts source the production node has for the twin: its price is the REAL tap's `door_price`.
struct TapFacts {
    tap: Arc<PriceTap>,
    admissions: FirstTick,
    waits: bool,
}

impl TapFacts {
    fn new(tap: &Arc<PriceTap>, waits: bool) -> Self {
        TapFacts { tap: Arc::clone(tap), admissions: FirstTick::new(false), waits }
    }
}

impl SymbolFacts for TapFacts {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
        self.admissions.admission(route_key, venue, symbol)
    }
    fn price(&self, _: &str, venue: &str, symbol: &str, _: bool) -> Result<Option<f64>, String> {
        self.tap.door_price(venue, symbol)
    }
    fn waits(&self) -> bool {
        self.waits
    }
}

/// The node's production sink chain for the venue's price feed, over `node`'s core.
fn production_sink(node: &Node, tap: &Arc<PriceTap>) -> TapSink {
    let core = Arc::new(CoreLaneSink::new(
        node.core.bar_sender(),
        node.core.market_sender(),
        node.core.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    TapSink::new(core, Arc::clone(tap))
}

fn frame(bid: f64, ask: f64) -> vike_marketdata::QuoteTick {
    vike_marketdata::QuoteTick {
        ts: 1,
        local_ts: 0,
        bid,
        ask,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}

/// Whether the venue client was handed `coid`: asked only after a later order that the ceiling always allows
/// (the sentinel) reached the venue, so the verdict on `coid` is final. The sentinel is a small reduce-only SELL:
/// flat it is 74 of notional, far under the ceiling, and against a position an unpriced order opened it is a
/// covered reduce, which no ceiling refuses (a BUY would be refused at the mark the opened position leaves).
fn reached_the_venue(node: &mut Node, facts: &dyn SymbolFacts, coid: &str) -> bool {
    let WireCommand::Submit(mut sentinel) =
        limit("sentinel", "binance", "BTCUSDT.P", 0.001, 74_000.0)
    else {
        unreachable!()
    };
    (sentinel.side, sentinel.reduce_only) = (-1, true);
    node.send(WireCommand::Submit(sentinel), Some(facts)).expect("the sentinel is accepted");
    node.venue_saw("sentinel");
    node.submitted.lock().unwrap().iter().any(|o| o.client_order_id == coid)
}

/// **The measured-crate fact.** A quote handed straight to the core's tick lane (what the pump's tick was) does
/// not price the twin engine: the ceiling that refuses a 7,430 market order at a mark lets it through. If this
/// FAILS the core now routes a labelled engine's own symbol, and `TapSink::mark_lane_twin` is not needed any
/// more (the proper fix is in `crates/vike-core/src/runtime/dispatch.rs`).
#[test]
fn a_quote_alone_prices_no_board_of_the_twin_engine() {
    let mut node = Node::start_lane_twin_capped(Some(CAP));
    let tick = FirstTick::new(false);
    node.tick("BTCUSDT.P", 74_299.9, 74_300.1);
    tick.tick(74_300.0);
    node.send(twin_order("quote-only", "market", None, false), Some(&tick)).expect("accepted");
    assert!(
        reached_the_venue(&mut node, &tick, "quote-only"),
        "the engine had no mark: the ceiling saw notional zero"
    );
    node.core.shutdown_and_join();
}

/// **Through the production sink the twin engine IS priced**: the same market order is refused by the engine's
/// own ceiling at the mark, and so is a LIMIT order (whose exposure the ceiling reads at the MARK, not at the
/// order's own price). The venue never sees either.
#[test]
fn a_tick_through_the_production_sink_prices_the_twin_engine_on_its_own_symbol() {
    let mut node = Node::start_lane_twin_capped(Some(CAP));
    let tap = Arc::new(PriceTap::default());
    let sink = production_sink(&node, &tap);
    let facts = TapFacts::new(&tap, false);

    sink.quote("binance", "BTCUSDT.P", frame(74_299.9, 74_300.1));
    node.send(twin_order("mkt", "market", None, false), Some(&facts)).expect("acked");
    node.send(twin_order("lim", "limit", Some(74_000.0), false), Some(&facts)).expect("acked");
    assert!(!reached_the_venue(&mut node, &facts, "mkt"), "the ceiling refused the market order");
    assert!(
        !node.submitted.lock().unwrap().iter().any(|o| o.client_order_id == "lim"),
        "and the limit order"
    );
    node.core.shutdown_and_join();
}

/// **A price that cannot be asked for is a REFUSAL on the twin's own symbol** (it passed before: the engine's
/// `no-reference-price` is armed for an admitted symbol only, and the twin's own is its mounted one). Judged on
/// the plan: no price source at all, and a source that says no feed can be had. A close is never refused for
/// it, and a Preview asks nothing, so it refuses only for want of a source.
#[test]
fn an_opening_order_on_the_twin_is_refused_when_no_price_can_be_asked_for() {
    let roster = two_lanes();
    let opening = submit("binance", "BTCUSDT.P", Some("PERP"));
    let WireCommand::Submit(mut close) = opening.clone() else { unreachable!() };
    close.reduce_only = true;
    let close = WireCommand::Submit(close);

    let why = refused(plan_submit_symbol(&opening, &roster, None, true));
    for want in ["perp lane", "`binance#PERP`", "`BTCUSDT.P`", "no price", "REFUSED"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(
        refused(plan_submit_symbol(&opening, &roster, None, false)).contains("REFUSED"),
        "a Preview with no source is refused too"
    );
    assert!(matches!(
        plan_submit_symbol(&close, &roster, None, true),
        SymbolPlan::Place { wait: None, admit: None, .. }
    ));

    let failing = Table::eth().with_price(Price::Failing);
    let why = refused(plan_submit_symbol(&opening, &roster, Some(&failing), true));
    for want in ["perp lane", "no price for `BTCUSDT.P` in its engine", "no price feed", "REFUSED"]
    {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(
        matches!(
            plan_submit_symbol(&close, &roster, Some(&failing), true),
            SymbolPlan::Place { wait: None, admit: None, .. }
        ),
        "a close is never held up or refused for want of a price"
    );
    let peek = Table::eth().with_price(Price::Failing);
    assert!(matches!(
        plan_submit_symbol(&opening, &roster, Some(&peek), false),
        SymbolPlan::Place { wait: None, .. }
    ));
    assert!(peek.events().is_empty(), "a Preview asks the price lane nothing");
}

/// **Before the first tick the door answers for the engine**: the twin's own symbol has no price for it, so a
/// surface that must not wait is refused at once in the door's words, and nothing reaches the venue. The first
/// frame through the sink releases the next order.
#[test]
fn the_door_refuses_an_opening_order_on_the_twin_until_its_engine_has_a_mark() {
    let mut node = Node::start_lane_twin_capped(Some(CAP));
    let tap = Arc::new(PriceTap::default());
    let sink = production_sink(&node, &tap);
    let facts = TapFacts::new(&tap, false);

    let why = node
        .send(twin_order("early", "market", None, false), Some(&facts))
        .expect_err("no mark in the engine yet");
    for want in ["`BTCUSDT.P`", "no price", "REFUSED"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(node.submitted.lock().unwrap().is_empty());

    sink.quote("binance", "BTCUSDT.P", frame(74_299.9, 74_300.1));
    node.send(twin_order("late", "limit", Some(74_300.0), true), Some(&facts))
        .expect("a reduce-only order is never held up");
    node.venue_saw("late");
    node.core.shutdown_and_join();
}

// ── the same at LIVE (review r2, finding 1): the one engine that reaches MAINNET through this path ──────────

/// The ceilings a MAINNET twin runs under (`assemble_engine` refuses a live engine without the first two and
/// `policy.max_account_exposure` is the third): 500 per order, 1,000 per symbol, 2,000 for the account. 0.1 BTC
/// at 74,300 is 7,430, far over all three; 0.005 is 371.5, under all three.
const LIVE_CAPS: TwinCaps = TwinCaps {
    max_notional_per_order: Some(500.0),
    max_total_exposure: Some(1_000.0),
    max_account_exposure: Some(2_000.0),
};

/// A MARKET BUY on the twin's own symbol, addressed to the twin as the Trade window does (`PERP` by name).
fn perp_market(coid: &str, qty: f64) -> WireCommand {
    let WireCommand::Submit(mut req) = twin_order(coid, "market", None, false) else {
        unreachable!()
    };
    (req.qty, req.account) = (qty, Some("PERP".into()));
    WireCommand::Submit(req)
}

/// The core's `OrderDenied` line for `coid`, once it has folded (`{coid} ({reason})`).
fn denial_of(node: &Node, coid: &str) -> Option<String> {
    node.core
        .snapshot_cell()
        .load()
        .recent_events
        .iter()
        .find(|line| line.contains("OrderDenied") && line.contains(coid))
        .map(|line| line.to_string())
}

/// **At LIVE, no opening order reaches the venue before the twin's engine was handed a mark, and after one
/// every ceiling binds.** Both engines are `Live` with the three ceilings armed, driven through the production
/// chain (`TapSink` over `CoreLaneSink`) into a real two-engine core, and the venue client records what it was
/// handed. With no tick a MARKET BUY above the cap AND one under it are refused at the door in words and the
/// venue sees NOTHING; after one production-sink tick the same big order is accepted by the door (it sizes no
/// priceless order here) and DENIED by the engine's own `over-max-notional` — which it can only do at a
/// nonzero mark — and the small one is submitted.
#[test]
fn a_live_twin_takes_no_market_order_before_its_engine_has_a_mark_and_every_ceiling_binds_after() {
    let mut node = Node::start_lane_twin_tiered(AccountTier::Live, LIVE_CAPS);
    let tap = Arc::new(PriceTap::default());
    let sink = production_sink(&node, &tap);
    let facts = TapFacts::new(&tap, false);

    for (coid, qty) in [("big-0", 0.1), ("small-0", 0.005)] {
        let why = node
            .send(perp_market(coid, qty), Some(&facts))
            .expect_err("the engine has no mark for its own symbol yet");
        for want in ["`BTCUSDT.P`", "no price", "REFUSED"] {
            assert!(why.contains(want), "{coid}: {want:?} missing: {why}");
        }
    }
    assert!(node.submitted.lock().unwrap().is_empty(), "ZERO submits before the first tick");

    sink.quote("binance", "BTCUSDT.P", frame(74_299.9, 74_300.1));
    node.send(perp_market("big-1", 0.1), Some(&facts)).expect("the door takes it");
    wait_until("the engine to deny the big order", || denial_of(&node, "big-1").is_some());
    let denial = denial_of(&node, "big-1").expect("denied");
    assert!(denial.contains("over-max-notional"), "{denial}");
    node.send(perp_market("small-1", 0.005), Some(&facts)).expect("the door takes it");
    let sent = node.venue_saw("small-1");
    assert_eq!((sent.symbol.as_str(), sent.qty), ("BTCUSDT.P", 0.005));
    let seen: Vec<String> =
        node.submitted.lock().unwrap().iter().map(|o| o.client_order_id.clone()).collect();
    assert_eq!(seen, ["small-1"], "only the order under every ceiling reached the venue");
    node.core.shutdown_and_join();
}

/// **The door sizes a priceless opening order on a LIVE twin's own symbol itself**, whatever mark the engine has
/// (review r2, finding 1): the door's notional ceiling reads `(None, Linear) => None` for a market order, and the
/// core judges it at the engine's mark, which a cold or restored engine does not have. Here the door has a price
/// (a source that holds one) and the engine has NONE (nothing ticked it, restored or fresh); the ceiling still
/// refuses the big order before anything is sent, the small one goes, a stop-market is sized at its trigger, a
/// close is never sized, and the DEMO twin keeps what it had (the same order passes the door unsized).
#[test]
fn the_door_sizes_a_priceless_opening_order_on_a_live_twin_whatever_the_engine_knows() {
    let ceiling =
        || ControlLimits::new(ControlLimitsConfig { max_notional: Some(500.0), rate_per_sec: 1e9 });
    for (what, mut node) in [
        ("fresh", Node::start_lane_twin_tiered(AccountTier::Live, TwinCaps::default())),
        ("restored", Node::start_lane_twin_restored(AccountTier::Live, TwinCaps::default())),
    ] {
        node.limits = ceiling();
        let facts = FirstTick::new(false);
        facts.tick(74_300.0);

        let why = node
            .send(perp_market("big", 0.1), Some(&facts))
            .expect_err("7,430 over a 500 ceiling, sized at the door's own price");
        assert!(
            why.contains("max_notional_per_order") && why.contains("7430"),
            "{what}: the ceiling's words: {why}"
        );
        assert!(node.submitted.lock().unwrap().is_empty(), "{what}: nothing was sent");

        let WireCommand::Submit(mut stop) = perp_market("stop", 0.1) else { unreachable!() };
        (stop.order_type, stop.trigger_price) = ("stop".into(), Some(74_000.0));
        node.send(WireCommand::Submit(stop), Some(&facts)).expect_err(
            "a priceless stop is sized at its trigger (or the door's price, the higher)",
        );

        node.send(perp_market("small", 0.005), Some(&facts)).expect("371.5 is under the ceiling");
        node.venue_saw("small");

        // A reducing order is never sized, here or anywhere.
        let WireCommand::Submit(mut close) = perp_market("close", 0.001) else { unreachable!() };
        (close.side, close.reduce_only) = (-1, true);
        node.send(WireCommand::Submit(close), Some(&facts)).expect("a close is never size-capped");
        node.venue_saw("close");
        node.core.shutdown_and_join();
    }

    // The DEMO twin: the same order passes the door as it always did (the engine is the floor).
    let mut demo = Node::start_lane_twin_tiered(AccountTier::Demo, TwinCaps::default());
    demo.limits = ceiling();
    let facts = FirstTick::new(false);
    facts.tick(74_300.0);
    demo.send(perp_market("big", 0.1), Some(&facts)).expect("the demo twin is unchanged");
    demo.venue_saw("big");
    demo.core.shutdown_and_join();

    // No price at all is a refusal at live, in words (the door's hold for the first price, then the sizing).
    let mut live = Node::start_lane_twin_tiered(AccountTier::Live, TwinCaps::default());
    live.limits = ceiling();
    let no_price = FirstTick::new(false);
    let why = live.send(perp_market("early", 0.001), Some(&no_price)).expect_err("no price");
    assert!(why.contains("no price") && why.contains("REFUSED"), "{why}");
    live.core.shutdown_and_join();
}

/// **The twin's price rule is the same at LIVE** (the plan, no core): an opening order on a live twin's own symbol
/// is refused in the lane's words when the surface has no price source or the source says no feed can be had,
/// is held for the first price when none has come, and a close is never refused for want of one.
#[test]
fn an_opening_order_on_a_live_twin_is_refused_when_no_price_can_be_asked_for() {
    let roster = [live(engine("binance", "BTCUSDT")), live(engine("binance#PERP", "BTCUSDT.P"))];
    let opening = submit("binance", "BTCUSDT.P", Some("PERP"));
    let WireCommand::Submit(mut close) = opening.clone() else { unreachable!() };
    close.reduce_only = true;
    let close = WireCommand::Submit(close);

    let why = refused(plan_submit_symbol(&opening, &roster, None, true));
    for want in ["perp lane", "`binance#PERP`", "`BTCUSDT.P`", "no price", "REFUSED"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    let failing = Table::eth().with_price(Price::Failing);
    let why = refused(plan_submit_symbol(&opening, &roster, Some(&failing), true));
    assert!(
        why.contains("no price for `BTCUSDT.P` in its engine") && why.contains("REFUSED"),
        "{why}"
    );
    let pending = Table::eth().with_price(Price::Pending);
    assert!(matches!(
        plan_submit_symbol(&opening, &roster, Some(&pending), true),
        SymbolPlan::Place { wait: Some(_), admit: None, .. }
    ));
    for facts in [None, Some(&failing as &dyn SymbolFacts)] {
        assert!(matches!(
            plan_submit_symbol(&close, &roster, facts, true),
            SymbolPlan::Place { wait: None, admit: None, .. }
        ));
    }
}

// ── a symbol admitted into a LIVE twin is traded at the leverage the exchange was SEEN holding ──────────────

/// An opening limit order on `ETHUSDT.P` (a symbol the twin was not mounted on) at the twin's own address.
fn eth_perp(coid: &str) -> WireCommand {
    let WireCommand::Submit(mut req) = limit(coid, "binance", "ETHUSDT.P", 0.01, 3_000.0) else {
        unreachable!()
    };
    req.account = Some("PERP".into());
    WireCommand::Submit(req)
}

/// **A LIVE twin refuses an opening order on a symbol the exchange did not confirm the account's leverage for,
/// and a DEMO twin admits it as before** (review r2, finding 6). The commit that admits the symbol makes the
/// exchange hold the account's figure for it and reads it back; unconfirmed, the symbol would trade real money
/// at whatever leverage the exchange already holds for it. The first order admits the symbol into the engine
/// and is refused in words (nothing reaches the venue); the next order asks the exchange again (the admission
/// is restated, so the commit runs) and is refused again; once the exchange confirms, the order goes. A close
/// is never refused. Demo: unconfirmed, the order goes (the gatewayhub's log says why, the engine keeps its own
/// rate).
#[test]
fn a_live_twin_refuses_an_unconfirmed_symbol_leverage_and_a_demo_twin_admits_it() {
    let mut node = Node::start_lane_twin_tiered(AccountTier::Live, TwinCaps::default());
    let unconfirmed = Table::eth();
    let why = node.send(eth_perp("first"), Some(&unconfirmed)).expect_err("nothing confirmed");
    for want in ["`binance#PERP`", "LIVE account", "has not confirmed", "`ETHUSDT.P`", "REFUSED"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(
        unconfirmed.events().iter().any(|e| e == "admitted:ETHUSDT.P"),
        "{:?}",
        unconfirmed.events()
    );
    assert!(node.submitted.lock().unwrap().is_empty(), "nothing reached the venue");

    // The symbol is admitted now; the next order is refused the same way, and it asked again.
    node.blocks_once_trading("binance#PERP", "ETHUSDT.P");
    let asked_before = unconfirmed.events().iter().filter(|e| *e == "admitted:ETHUSDT.P").count();
    node.send(eth_perp("second"), Some(&unconfirmed)).expect_err("still nothing confirmed");
    let asked_after = unconfirmed.events().iter().filter(|e| *e == "admitted:ETHUSDT.P").count();
    assert_eq!(asked_after, asked_before + 1, "the order asked the exchange again");
    assert!(node.submitted.lock().unwrap().is_empty());

    // A close is never refused for it.
    let WireCommand::Submit(mut close) = eth_perp("close") else { unreachable!() };
    (close.side, close.reduce_only) = (-1, true);
    node.send(WireCommand::Submit(close), Some(&unconfirmed)).expect("a close is never refused");
    node.venue_saw("close");

    // The exchange confirms 5x: the order goes.
    let confirmed = Table::eth().confirming(5.0);
    node.send(eth_perp("third"), Some(&confirmed)).expect("confirmed");
    node.venue_saw("third");
    node.core.shutdown_and_join();

    // DEMO: the same unconfirmed commit changes nothing, as before.
    let mut demo = Node::start_lane_twin_tiered(AccountTier::Demo, TwinCaps::default());
    demo.send(eth_perp("demo"), Some(&Table::eth())).expect("a demo twin admits it as before");
    demo.venue_saw("demo");
    demo.core.shutdown_and_join();
}
