//! The events a strategy HEARS beside its market, as live hears them (`TickEvent::Reference`, `Mark`,
//! `Flow`, `FeedStatus`, and a traded leg on another venue, `StrategyEngineConfig::reference_legs`):
//!
//! 1. each heard event reaches its hook with its payload, in time order, not warmup-gated, and fills
//!    nothing, samples no equity and moves no warmup index;
//! 2. a strategy that ignores the hooks runs byte-identically with heard events on its tape and without;
//! 3. a traded leg fills on its own events and reaches no hook: its touch is heard on
//!    `on_reference_quote` as the `Reference` events beside it, and an order naming it fills on its own
//!    tape.

use std::cell::RefCell;
use std::rc::Rc;

use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};
use vike_model::{FeedStatus, FlowToxicity, OrderBroker, OrderPlacement, Strategy, TickMark};
use vike_simulator::test_support::{quote, trade};
use vike_simulator::{
    FillModelKind, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, TickEvent,
};

/// Every hook call, spelled with its payload.
#[derive(Clone, Default)]
struct Ear {
    heard: Rc<RefCell<Vec<String>>>,
    warmup: usize,
    /// Buys this symbol once, on the first reference quote.
    hedge: Option<&'static str>,
}

impl Ear {
    fn note(&self, line: String) {
        self.heard.borrow_mut().push(line);
    }
}

impl Strategy<OrderBrokerSimulator> for Ear {
    fn warmup(&self) -> usize {
        self.warmup
    }
    fn on_quote_tick(&mut self, _b: &mut OrderBrokerSimulator, q: &TickQuote) {
        self.note(format!("quote {} {}", q.symbol, q.timestamp));
    }
    fn on_trade_tick(&mut self, _b: &mut OrderBrokerSimulator, t: &TickTrade) {
        self.note(format!("trade {} {}", t.symbol, t.timestamp));
    }
    fn on_reference_quote(&mut self, b: &mut OrderBrokerSimulator, venue: &str, q: &TickQuote) {
        let (bid, ask, symbol, ts) = (q.bid, q.ask, &q.symbol, q.timestamp);
        self.note(format!("reference {venue} {symbol} {bid}/{ask} {ts}"));
        if let Some(leg) = self.hedge.take() {
            b.place(leg, "", 1, 1.0, OrderPlacement::Market, false);
        }
    }
    fn on_mark(&mut self, _b: &mut OrderBrokerSimulator, mark: &TickMark) {
        self.note(format!("mark {} {} {}", mark.symbol, mark.price, mark.timestamp));
    }
    fn on_flow(&mut self, _b: &mut OrderBrokerSimulator, flow: FlowToxicity) {
        self.note(format!("flow {}/{} {}", flow.bid, flow.ask, flow.timestamp));
    }
    fn on_feed_status(&mut self, b: &mut OrderBrokerSimulator, status: FeedStatus) {
        self.note(format!("status {status:?} {}", b.now));
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &vike_model::Fill) {
        self.note(format!("fill {} {} @{}", fill.symbol, fill.size, fill.price));
    }
}

fn reference(venue: &str, symbol: &str, ts: i64, bid: f64, ask: f64) -> TickEvent {
    let TickEvent::Quote(q) = quote(symbol, ts, bid, ask, 1.0, 1.0) else { unreachable!() };
    TickEvent::Reference { venue: venue.to_string(), quote: Box::new(q), heard_at: ts }
}

fn heard_tape() -> Vec<TickEvent> {
    vec![
        reference("binance", "BTC", 1_500, 99.0, 100.0),
        TickEvent::Mark {
            mark: TickMark { symbol: "SPOT".into(), price: 7.5, timestamp: 2_500 },
            heard_at: 2_500,
        },
        TickEvent::Flow(FlowToxicity { bid: 0.25, ask: 0.75, timestamp: 3_500 }),
        TickEvent::FeedStatus { status: FeedStatus::Disconnected, timestamp: 4_500 },
    ]
}

fn own_tape() -> Vec<TickEvent> {
    (1..=5).map(|k| quote("BTC", k * 1_000, 100.0 + k as f64, 101.0 + k as f64, 1.0, 1.0)).collect()
}

fn tick_params() -> StrategyEngineConfig {
    StrategyEngineConfig { fill_model: FillModelKind::Tick, ..Default::default() }
}

#[test]
fn each_heard_event_reaches_its_hook_unwarmed_and_fills_nothing() {
    // A warm-up longer than the tape: no market hook may fire, and every heard one must.
    let ear = Ear { warmup: 100, ..Ear::default() };
    let heard = Rc::clone(&ear.heard);
    let mut engine = StrategyEngine::new(vec![("BTC".into(), Vec::new())], ear, tick_params());
    let result = engine.run_ticks(&[("BTC".into(), own_tape()), ("heard".into(), heard_tape())]);
    assert_eq!(
        *heard.borrow(),
        vec![
            "reference binance BTC 99/100 1500",
            "mark SPOT 7.5 2500",
            "flow 0.25/0.75 3500",
            "status Disconnected 4500",
        ],
        "each heard event, on its own hook, in time order, through a warm-up no market hook passes"
    );
    assert_eq!(
        result.equity_curve.len(),
        5,
        "one equity sample per own quote, none per heard event"
    );
    assert_eq!(result.equity_timestamp, vec![1_000, 2_000, 3_000, 4_000, 5_000]);
    assert_eq!(result.final_equity, 10_000.0, "a heard event fills nothing");
}

/// Buys on the first own quote, sells on the third: a strategy deaf to every heard hook.
#[derive(Default)]
struct RoundTrip {
    quotes: usize,
}

impl Strategy<OrderBrokerSimulator> for RoundTrip {
    fn on_quote_tick(&mut self, b: &mut OrderBrokerSimulator, q: &TickQuote) {
        self.quotes += 1;
        match self.quotes {
            1 => b.place(&q.symbol, "", 1, 2.0, OrderPlacement::Market, false),
            3 => b.place(&q.symbol, "", -1, 2.0, OrderPlacement::Market, false),
            _ => {}
        }
    }
}

#[test]
fn a_tape_with_heard_events_runs_the_market_byte_identically() {
    let run = |streams: &[(String, Vec<TickEvent>)]| {
        let params = tick_params();
        StrategyEngine::new(vec![("BTC".into(), Vec::new())], RoundTrip::default(), params)
            .run_ticks(streams)
    };
    let plain = run(&[("BTC".into(), own_tape())]);
    let heard = run(&[("BTC".into(), own_tape()), ("heard".into(), heard_tape())]);
    assert_eq!(plain.n_trades, 1, "the fixture must trade, or the comparison proves nothing");
    assert_eq!(heard.trades, plain.trades);
    let bits = |curve: &[f64]| curve.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&heard.equity_curve), bits(&plain.equity_curve));
    assert_eq!(heard.equity_timestamp, plain.equity_timestamp);
    assert_eq!(heard.final_equity.to_bits(), plain.final_equity.to_bits());
}

fn leg_book(ts: i64, seq: u64) -> TickEvent {
    TickEvent::Book(Box::new(L2BookUpdate {
        timestamp: ts,
        local_timestamp: 0,
        sequence: seq,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: 0.5,
        bids: vec![L2BookLevel::new(98.0, 3.0), L2BookLevel::new(97.5, 9.0)],
        asks: vec![L2BookLevel::new(99.5, 4.0)],
        symbol: "BTCX".into(),
    }))
}

#[test]
fn a_traded_leg_is_heard_on_its_reference_events_and_fills_on_its_own_tape() {
    let ear = Ear { hedge: Some("BTCX"), ..Ear::default() };
    let heard = Rc::clone(&ear.heard);
    let params = StrategyEngineConfig {
        reference_legs: vec![("BTCX".into(), "binance".into())],
        ..tick_params()
    };
    let slots = vec![("BTC".into(), Vec::new()), ("BTCX".into(), Vec::new())];
    let mut engine = StrategyEngine::new(slots, ear, params);
    let leg = vec![
        quote("BTCX", 1_500, 97.0, 98.0, 1.0, 1.0),
        trade("BTCX", 1_600, 98.0, 1.0),
        leg_book(1_700, 1),
        quote("BTCX", 2_500, 99.0, 100.0, 1.0, 1.0),
    ];
    // Its touch as the caller hears it: each quote, and the book's top.
    let touch = vec![
        reference("binance", "BTCX", 1_500, 97.0, 98.0),
        reference("binance", "BTCX", 1_700, 98.0, 99.5),
        reference("binance", "BTCX", 2_500, 99.0, 100.0),
    ];
    engine.run_ticks(&[
        ("BTC".into(), vec![quote("BTC", 1_000, 100.0, 101.0, 1.0, 1.0)]),
        ("BTCX".into(), leg),
        ("heard".into(), touch),
    ]);
    assert_eq!(
        *heard.borrow(),
        vec![
            "quote BTC 1000",
            "reference binance BTCX 97/98 1500",
            // the leg's print fills the hedge sent on its quote, and reaches no hook
            "fill BTCX 1 @98",
            "reference binance BTCX 98/99.5 1700",
            "reference binance BTCX 99/100 2500",
        ],
        "the leg is heard on the reference hook only, its book as its top, and its hedge fills there"
    );
    assert_eq!(engine.core.position_of("BTCX").size, 1.0);
    assert_eq!(engine.core.position_of("BTC").size, 0.0);
}
