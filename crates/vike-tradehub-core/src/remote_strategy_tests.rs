//! The sandboxed mount's two halves together, with no process: the child is [`serve`] on a thread over
//! two pipes, running a strategy written here, so every rule of the module root's failure table but the
//! jail itself is driven through the real proxy, supervisor and protocol code. The jail and the loaded
//! artifact are the real child's (`crates/vike-strategy-sandbox`'s tests).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::LiveBroker;
use vike_hub_protocol::child_pipe;
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, Strategy, StrategyState};

use super::protocol::{
    self, BrokerCall, Event, FromChild, Hook, Open, Opened, SeriesDelta, ToChild, View, ViewBroker,
    ViewRow,
};
use super::proxy::{FoldView, Pending};
use super::*;

/// The build both ends state here (the binary hands in `vike_buildinfo::summary`; a library links none).
const BUILD: &str = "git 0123abcd clean, built 2026-11-19T00:00:00Z, rustc 1.0.0, test";

/// The `LiveBroker` the core builds per hook, empty but for what a test sets.
fn ctx(position: f64, now: i64) -> LiveBroker {
    LiveBroker {
        data_delay: None,
        inputs: None,
        closed_trades: None,
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        leg_values: None,
        position,
        average_price: 101.5,
        price: 100.0,
        equity: 1_000.0,
        bars: Arc::new(Vec::new()),
        index: 0,
        now,
        contract_value: vike_model::ContractValue::default(),
        quote_rate: 1.0,
        lot_size: 0.001,
        tick_size: 0.1,
        min_qty: 0.0,
        min_notional: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        brackets: Vec::new(),
        order_lists: Vec::new(),
        combos: Vec::new(),
        order_emulation_arms: Vec::new(),
        mass_cancel: false,
        forecasts: Vec::new(),
    }
}

fn bar(ts: i64, close: f64) -> Bar {
    Bar::ohlcv(ts, close, close, close, close, 1.0)
}

/// The child's view of `ctx`, as the supervisor would first send it (the whole history).
fn first_view(ctx: &LiveBroker) -> View {
    let v = FoldView::capture(ctx);
    View {
        bars: v.bars.iter().filter_map(|(name, s)| SeriesDelta::between(name, None, s)).collect(),
        rows: v.rows,
        declared: v.declared,
        equity: v.equity,
        index: v.index,
        now: v.now,
        inputs: Vec::new(),
        closed_from: 0,
        closed: Vec::new(),
    }
}

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits()
}

/// Every read the child answers is the one the core's `LiveBroker` answers, for the own symbol, a declared
/// leg and an undeclared symbol, on an undeclared and on a declared mount (the NaN of a declared row
/// included).
#[test]
fn the_child_reads_what_the_core_broker_reads() {
    let mut undeclared = ctx(2.0, 7);
    undeclared.bars = Arc::new(vec![bar(1, 99.0), bar(2, 100.0)]);
    undeclared.index = 1;
    let mut declared = ctx(2.0, 7);
    declared.bar_views = vec![
        ("BTCUSDT".to_string(), Arc::new(vec![bar(1, 99.0)])),
        ("ETHUSDT".to_string(), Arc::new(vec![bar(1, 5.0), bar(2, 6.0)])),
    ];
    declared.positions = vec![("BTCUSDT".to_string(), 3.0), ("ETHUSDT".to_string(), -1.0)];
    declared.prices = vec![("ETHUSDT".to_string(), 6.0)];
    let cv = vike_model::ContractValue::default();
    declared.leg_values = Some(Arc::from(vec![
        ("BTCUSDT".to_string(), cv, None, None),
        ("ETHUSDT".to_string(), cv, None, Some(vike_model::StreamDelay::RealTime)),
    ]));
    for core in [&undeclared, &declared] {
        let mut child = ViewBroker::default();
        child.absorb(first_view(core)).unwrap();
        for name in ["", "BTCUSDT", "ETHUSDT", "XRPUSDT"] {
            assert!(
                same(OrderBroker::position(&child, name), OrderBroker::position(core, name)),
                "{name}"
            );
            assert!(same(child.price(name), core.price(name)), "price {name}");
            assert!(same(child.average_price(name), core.average_price(name)), "avg {name}");
            assert_eq!(child.data_delay(name), core.data_delay(name), "delay {name}");
            assert_eq!(child.instrument_grid(name), core.instrument_grid(name), "grid {name}");
            assert_eq!(child.contract_value(name), core.contract_value(name), "value {name}");
            assert_eq!(child.bars(name), core.bars(name), "bars {name}");
        }
        assert_eq!((child.equity(), child.index(), child.now()), (1_000.0, core.index, 7));
    }
}

/// The frame keeps every bit, a NaN included (JSON would have written `null`).
#[test]
fn a_frame_keeps_a_nan_and_every_kind_of_call() {
    let calls = vec![
        BrokerCall::Place {
            symbol: String::new(),
            tag: "bid".into(),
            side: 1,
            qty: f64::NAN,
            order: OrderPlacement::StopLimit { trigger: 1.0, limit: 2.0 },
            reduce_only: true,
        },
        BrokerCall::Amend {
            tag: "bid".into(),
            new_qty: None,
            new_price: Some(3.0),
            new_trigger: None,
        },
        BrokerCall::Cancel { tag: "ask".into() },
        BrokerCall::PublishForecast {
            symbol: "ETHUSDT".into(),
            forecast: vike_model::Forecast::new(-0.25, 60_000, Some(0.75)).unwrap(),
        },
    ];
    let mut wire = Vec::new();
    child_pipe::write_message(&mut wire, &FromChild::Reply { sequence: 9, calls: calls.clone() })
        .unwrap();
    let back: FromChild = child_pipe::read_message(&mut wire.as_slice()).unwrap();
    let FromChild::Reply { sequence: 9, calls: got } = back else { panic!("not the reply") };
    let BrokerCall::Place { qty, .. } = &got[0] else { panic!("not the place") };
    assert!(qty.is_nan());
    assert_eq!(got[1..], calls[1..]);
    let mut core = ViewBroker::default();
    let row = ViewRow { average_price: f64::NAN, ..ViewRow::of(&ctx(0.0, 0), "") };
    let event = ToChild::Event(Box::new(Event {
        sequence: 1,
        hook: Hook::Bar(bar(1, 1.0)),
        view: Some(View { rows: vec![row], ..first_view(&ctx(0.0, 0)) }),
    }));
    let mut wire = Vec::new();
    child_pipe::write_message(&mut wire, &event).unwrap();
    let ToChild::Event(e) = child_pipe::read_message(&mut wire.as_slice()).unwrap() else {
        panic!("not the event")
    };
    core.absorb(e.view.unwrap()).unwrap();
    assert!(core.average_price("").is_nan());
}

/// A history is sent once: the new bars only, the last bar again when it re-closed, the whole series when
/// it was replaced, and nothing when nothing changed.
#[test]
fn a_history_is_sent_as_its_change() {
    let s = vec![bar(1, 1.0), bar(2, 2.0), bar(3, 3.0)];
    let sent = (2, bar(2, 2.0));
    assert_eq!(SeriesDelta::between("x", Some(&sent), &s).unwrap().from, 2);
    assert!(SeriesDelta::between("x", Some(&(3, bar(3, 3.0))), &s).is_none());
    let reclosed = (3, bar(3, 2.5));
    assert_eq!(SeriesDelta::between("x", Some(&reclosed), &s).unwrap().from, 2);
    let replaced = (2, bar(9, 9.0));
    assert_eq!(SeriesDelta::between("x", Some(&replaced), &s).unwrap().from, 0);
    assert!(SeriesDelta::between("x", None, &[]).is_none());
    let shrunk = SeriesDelta::between("x", Some(&(5, bar(5, 5.0))), &[]).unwrap();
    assert_eq!((shrunk.from, shrunk.bars.len()), (0, 0));
}

/// A broker on a DECLARED mount (legs `A` and `B`), dispatched on `leg`'s series, as the core lends it:
/// the broker's own `bars` is the very `Arc` of that leg's row.
fn leg_context(legs: &[(&str, Arc<Vec<Bar>>)], leg: usize) -> LiveBroker {
    let mut c = ctx(0.0, 0);
    c.bar_views = legs.iter().map(|(n, s)| (n.to_string(), Arc::clone(s))).collect();
    c.bars = Arc::clone(&legs[leg].1);
    c
}

/// Quotes, marks and flow conflate per stream; books per stream too, on a declared mount per LEG (the
/// series the book dispatched on), and not when that leg is not known; a bar never does.
#[test]
fn only_the_market_streams_conflate() {
    let quote = |symbol: &str| {
        Hook::TickQuote(vike_marketdata::TickQuote {
            timestamp: 0,
            local_timestamp: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: symbol.into(),
        })
    };
    let plain = FoldView::capture(&ctx(0.0, 0));
    let pending = |hook: Hook, view: &FoldView| Pending { hook, view: Some(view.clone()) };
    assert!(pending(quote("A"), &plain).conflates_with(&quote("A"), Some(&plain)));
    assert!(!pending(quote("A"), &plain).conflates_with(&quote("B"), Some(&plain)));
    let book = || Hook::OrderBook(vike_marketdata::L2Book::new(0.1));
    assert!(pending(book(), &plain).conflates_with(&book(), Some(&plain)));
    let legs = [("A", Arc::new(vec![bar(1, 1.0)])), ("B", Arc::new(vec![bar(1, 2.0)]))];
    let (a, b) =
        (FoldView::capture(&leg_context(&legs, 0)), FoldView::capture(&leg_context(&legs, 1)));
    assert_eq!((a.dispatched, b.dispatched), (Some(0), Some(1)));
    assert!(pending(book(), &a).conflates_with(&book(), Some(&a)), "leg A's newer book wins");
    assert!(
        !pending(book(), &a).conflates_with(&book(), Some(&b)),
        "leg B's book is another stream"
    );
    let mut unknown = leg_context(&legs, 0);
    unknown.bars = Arc::new(Vec::new());
    let unknown = FoldView::capture(&unknown);
    assert!(!pending(book(), &unknown).conflates_with(&book(), Some(&unknown)), "no leg: queued");
    let bar_hook = |ts| Hook::Bar(bar(ts, 1.0));
    assert!(!pending(bar_hook(1), &plain).conflates_with(&bar_hook(2), Some(&plain)));
}

/// While the child is busy, a declared mount's books conflate per leg, the latest of each leg winning: the
/// child hears the book in flight, then the newest of each leg in the order their newest arrived.
#[test]
fn while_the_child_is_busy_the_latest_book_of_each_leg_wins() {
    struct Slow(Arc<std::sync::Mutex<Vec<i64>>>);
    impl Strategy<ViewBroker> for Slow {
        fn on_order_book(&mut self, _b: &mut ViewBroker, book: &vike_marketdata::L2Book) {
            let heard = {
                let mut h = self.0.lock().unwrap();
                h.push(book.best_bid().map_or(0, |l| l.price as i64));
                h.len()
            };
            if heard == 1 {
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&heard);
    let launcher: Launcher = Box::new(move || {
        let (child_in, to_child) = std::io::pipe()?;
        let (from_child, child_out) = std::io::pipe()?;
        let log = Arc::clone(&log);
        std::thread::spawn(move || {
            let _ = serve(child_in, child_out, BUILD, |_| Ok((Slow(log), None)));
        });
        Ok(Launched {
            child: Box::new(ThreadChild),
            to_child: Box::new(to_child),
            from_child: Box::new(from_child),
            reasons: None,
        })
    });
    let (mut proxy, _remote) = RemoteStrategy::open(spec(), launcher).expect("it opens");
    let legs = [("A", Arc::new(vec![bar(1, 1.0)])), ("B", Arc::new(vec![bar(1, 2.0)]))];
    let book_at = |bid: f64| {
        let mut b = vike_marketdata::L2Book::new(1.0);
        b.apply_snapshot(
            1,
            &[vike_marketdata::L2BookLevel::new(bid, 1.0)],
            &[vike_marketdata::L2BookLevel::new(bid + 1.0, 1.0)],
        );
        b
    };
    proxy.on_order_book(&mut leg_context(&legs, 0), &book_at(100.0));
    until("the first book in flight", || heard.lock().unwrap().len() == 1);
    for (leg, bid) in [(0, 101.0), (0, 102.0), (1, 201.0), (0, 103.0)] {
        proxy.on_order_book(&mut leg_context(&legs, leg), &book_at(bid));
    }
    until("the latest of each leg", || heard.lock().unwrap().len() == 3);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(*heard.lock().unwrap(), vec![100, 201, 103]);
}

/// What the test child does.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// On a bar, rest a bid sized by the history it holds; on a fill, pull it.
    Quote,
    /// On a bar, take longer than the hook deadline.
    Hang,
    /// On a bar, order a symbol the mount does not trade.
    Stray,
    /// On a bar, panic.
    Panic,
}

struct Probe {
    mode: Mode,
    fills: u64,
}

impl Strategy<ViewBroker> for Probe {
    fn warmup(&self) -> usize {
        2
    }
    fn on_bar(&mut self, b: &mut ViewBroker, bar: &Bar) {
        match self.mode {
            Mode::Quote => {
                let size = b.bars("").len() as f64;
                let order = OrderPlacement::Limit { price: bar.close - 1.0 };
                b.place("", "bid", 1, size, order, false);
            }
            Mode::Hang => std::thread::sleep(HOOK_DEADLINE + Duration::from_millis(500)),
            Mode::Stray => b.place("ETHUSDT", "", 1, 1.0, OrderPlacement::Market, false),
            Mode::Panic => panic!("the test strategy panics on a bar"),
        }
    }
    fn on_fill(&mut self, b: &mut ViewBroker, _fill: &vike_model::Fill) {
        self.fills += 1;
        b.cancel("bid");
    }
    fn save_state(&self) -> Option<StrategyState> {
        Some(fills_state(self.fills))
    }
    fn load_state(&mut self, state: &StrategyState) {
        self.fills = state.to_json_value().ok().and_then(|s| s["fills"].as_u64()).unwrap_or(0);
    }
}

/// The state the probe saves after `fills` fills.
fn fills_state(fills: u64) -> StrategyState {
    StrategyState::from_json_value(&serde_json::json!({ "fills": fills }))
}

struct ThreadChild;

impl SandboxChild for ThreadChild {
    fn kill(&mut self) {}
    fn wait(&mut self) -> String {
        "a test thread".to_string()
    }
}

/// A launcher whose child is [`serve`] on a thread; the `n`th start (0-based) runs `mode(n)`.
fn thread_launcher(
    mode: impl Fn(usize) -> Mode + Send + Sync + 'static,
) -> (Launcher, Arc<AtomicUsize>) {
    let starts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&starts);
    let launcher: Launcher = Box::new(move || {
        let mode = mode(counter.fetch_add(1, Ordering::SeqCst));
        let (child_in, to_child) = std::io::pipe()?;
        let (from_child, child_out) = std::io::pipe()?;
        std::thread::spawn(move || {
            let _ = serve(child_in, child_out, BUILD, |_| Ok((Probe { mode, fills: 0 }, None)));
        });
        Ok(Launched {
            child: Box::new(ThreadChild),
            to_child: Box::new(to_child),
            from_child: Box::new(from_child),
            reasons: None,
        })
    });
    (launcher, starts)
}

fn spec() -> RemoteSpec {
    RemoteSpec {
        file: "trend/probe.rs".into(),
        sha256: "00".into(),
        params: String::new(),
        facts: vike_model::params::MountFacts {
            symbol: "BTCUSDT".into(),
            venue: "sim".into(),
            interval: "1m".into(),
            ..Default::default()
        },
        controller_id: None,
        build: BUILD.into(),
    }
}

type Lane = tokio::sync::mpsc::Receiver<vike_execution::Ingest>;

fn open(
    mode: impl Fn(usize) -> Mode + Send + Sync + 'static,
) -> (RemoteStrategy, RemoteHandle, Lane, Arc<AtomicUsize>) {
    let (launcher, starts) = thread_launcher(mode);
    let (proxy, handle) = RemoteStrategy::open(spec(), launcher).expect("the strategy opens");
    let (lane, rx) = vike_execution::event_channel(64);
    handle.attach(lane);
    (proxy, handle, rx, starts)
}

/// The next reply the core is handed, applied to a fresh broker of the mount, within `wait`.
fn next_reply(rx: &mut Lane, wait: Duration) -> Option<LiveBroker> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(vike_execution::Ingest::MountReply(reply)) => {
                assert_eq!(reply.mount_id, "sim__BTCUSDT__1m");
                let intents = reply.apply.downcast::<vike_core::MountReplyIntents>().unwrap();
                let mut b = ctx(0.0, 0);
                (intents.write)(&mut b);
                (intents.applied)();
                return Some(b);
            }
            Ok(other) => panic!("not a reply: {other:?}"),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    None
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A bar reaches the child with its history, and the order it rests comes back to the core as the mount's
/// own buffered intent; the child's declarations answer the core's synchronous reads.
#[test]
fn a_hook_round_trips_as_the_mounts_own_intent() {
    let (mut proxy, handle, mut rx, _) = open(|_| Mode::Quote);
    assert_eq!(proxy.warmup(), 2);
    assert_eq!(handle.status(), RemoteStatus::Running);
    let mut core = ctx(0.0, 1_000);
    core.bars = Arc::new(vec![bar(1, 99.0), bar(2, 100.0), bar(3, 101.0)]);
    core.index = 2;
    proxy.on_bar(&mut core, &bar(3, 101.0));
    assert!(core.submissions.is_empty(), "the proxy buffers nothing on the fold");
    let applied = next_reply(&mut rx, Duration::from_secs(5)).expect("the reply");
    let s = &applied.submissions[0];
    assert_eq!((s.tag.as_deref(), s.qty, s.price), (Some("bid"), 3.0, Some(100.0)));
    // the next bar sends only the new bar: the child sizes off the four it now holds
    core.bars = Arc::new(vec![bar(1, 99.0), bar(2, 100.0), bar(3, 101.0), bar(4, 102.0)]);
    proxy.on_bar(&mut core, &bar(4, 102.0));
    assert_eq!(next_reply(&mut rx, Duration::from_secs(5)).unwrap().submissions[0].qty, 4.0);
}

/// A hook that runs past the deadline is killed and the child restarted; the reply it never sent applies
/// nothing.
#[test]
fn a_hung_hook_is_killed_at_the_deadline() {
    let (mut proxy, handle, mut rx, starts) =
        open(|n| if n == 0 { Mode::Hang } else { Mode::Quote });
    proxy.on_bar(&mut ctx(0.0, 1), &bar(1, 1.0));
    until("the restart", || starts.load(Ordering::SeqCst) == 2);
    assert!(next_reply(&mut rx, Duration::from_millis(200)).is_none(), "nothing applied");
    until("the restart to be said", || handle.status() == RemoteStatus::Running);
}

/// A reply with an intent for a symbol the mount does not trade applies NONE of its intents, and the child
/// is restarted saying why.
#[test]
fn garbage_applies_nothing() {
    let (mut proxy, handle, mut rx, starts) = open(|_| Mode::Stray);
    proxy.on_bar(&mut ctx(0.0, 1), &bar(1, 1.0));
    until("the restart", || starts.load(Ordering::SeqCst) == 2);
    assert!(next_reply(&mut rx, Duration::from_millis(200)).is_none(), "nothing applied");
    drop(handle);
}

/// A child that dies inside a fill is restarted with the state it last sent, and the next child hears that
/// fill: an order's end is never lost.
#[test]
fn a_crash_restarts_with_the_state_and_hears_the_fill_again() {
    let (mut proxy, handle, mut rx, starts) =
        open(|n| if n == 0 { Mode::Panic } else { Mode::Quote });
    let fill = vike_model::Fill {
        side: 1,
        size: 1.0,
        price: 1.0,
        fee: 0.0,
        timestamp: 1,
        is_maker: true,
        symbol: "BTCUSDT".into(),
        tag: Some("bid".into()),
    };
    proxy.load_state(&fills_state(4));
    proxy.on_bar(&mut ctx(0.0, 1), &bar(1, 1.0)); // the first child panics here
    proxy.on_fill(&mut ctx(0.0, 2), &fill);
    until("the restart", || starts.load(Ordering::SeqCst) == 2);
    let applied = next_reply(&mut rx, Duration::from_secs(5)).expect("the fill's cancel");
    assert_eq!(applied.cancels, vec!["bid".to_string()]);
    until("the state", || proxy.save_state() == Some(fills_state(5)));
    assert_eq!(handle.status(), RemoteStatus::Running);
}

/// Five crashes in the window stop the mount: no more restarts, events dropped, nothing cancelled.
#[test]
fn a_crash_loop_stops_the_mount() {
    let (mut proxy, handle, mut rx, starts) = open(|_| Mode::Panic);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ts = 0;
    while !matches!(handle.status(), RemoteStatus::Stopped { .. }) {
        assert!(Instant::now() < deadline, "no crash loop: {}", handle.status());
        ts += 1;
        proxy.on_bar(&mut ctx(0.0, ts), &bar(ts, 1.0));
        std::thread::sleep(Duration::from_millis(50));
    }
    let RemoteStatus::Stopped { reason } = handle.status() else { unreachable!() };
    assert!(reason.starts_with("crash loop: "), "{reason}");
    assert_eq!(starts.load(Ordering::SeqCst), CRASH_LIMIT);
    proxy.on_bar(&mut ctx(0.0, ts + 1), &bar(ts + 1, 1.0));
    assert!(next_reply(&mut rx, Duration::from_millis(200)).is_none());
}

/// A child that refuses the open (its file did not load) leaves the mount unopened, in the child's words, with the
/// id of a declared refusal kept across the pipe (`FromChild::Refused::id`) and none invented for free text.
#[test]
fn a_refused_open_is_the_mounts_reason() {
    use vike_utility::errors::{CarriedRefusal, Declared};
    let refused_with = |refusal: CarriedRefusal| -> Launcher {
        Box::new(move || {
            let (child_in, to_child) = std::io::pipe()?;
            let (from_child, child_out) = std::io::pipe()?;
            let refusal = refusal.clone();
            std::thread::spawn(move || {
                let _ = serve::<Probe, _, _>(child_in, child_out, BUILD, |_| Err(refusal));
            });
            Ok(Launched {
                child: Box::new(ThreadChild),
                to_child: Box::new(to_child),
                from_child: Box::new(from_child),
                reasons: None,
            })
        })
    };
    let declared = CarriedRefusal::new(
        Some("strategy.sandbox.not_created".into()),
        "`x.rs` did not build from its params and mount facts".into(),
    );
    let Err(reason) = RemoteStrategy::open(spec(), refused_with(declared)) else {
        panic!("it opened")
    };
    assert_eq!(reason.declared_id(), Some("strategy.sandbox.not_created"), "{reason}");
    assert!(
        reason.to_string().starts_with("refused: `x.rs` did not build from its params"),
        "the bare words: {reason}"
    );
    let Err(reason) = RemoteStrategy::open(spec(), refused_with("built for version 7".into()))
    else {
        panic!("it opened")
    };
    assert_eq!(reason.declared_id(), None, "free text has no id: {reason}");
    let reason = reason.to_string();
    assert!(reason.contains("built for version 7"), "{reason}");
}

/// Hears trades and order ends, in order, and has every trade's order refused by the core's drain (an
/// order end the core delivers as a CONSEQUENCE of applying the hook's orders).
struct Ledger(Arc<std::sync::Mutex<Vec<String>>>);

impl<B: OrderBroker> Strategy<B> for Ledger {
    fn on_trade_tick(&mut self, b: &mut B, t: &vike_marketdata::TickTrade) {
        self.0.lock().unwrap().push(format!("trade {}", t.timestamp));
        b.deny("", &format!("t{}", t.timestamp), 1.0, &format!("refused {}", t.timestamp));
    }
    fn on_order_event(&mut self, _b: &mut B, e: &vike_model::OrderEvent) {
        if let vike_model::OrderEventKind::Denied { reason } = &e.kind {
            self.0.lock().unwrap().push(reason.clone());
        }
    }
}

/// What a mount on a REAL core hears when three trades arrive back to back: in-process, or sandboxed.
fn heard_on_a_core(sandboxed: bool) -> Vec<String> {
    let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (strategy, remote): (Box<dyn Strategy<LiveBroker> + Send>, Option<RemoteHandle>) =
        if sandboxed {
            let log = Arc::clone(&heard);
            let launcher: Launcher = Box::new(move || {
                let (child_in, to_child) = std::io::pipe()?;
                let (from_child, child_out) = std::io::pipe()?;
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    let _ = serve(child_in, child_out, BUILD, |_| Ok((Ledger(log), None)));
                });
                Ok(Launched {
                    child: Box::new(ThreadChild),
                    to_child: Box::new(to_child),
                    from_child: Box::new(from_child),
                    reasons: None,
                })
            });
            let (proxy, remote) = RemoteStrategy::open(spec(), launcher).expect("it opens");
            (Box::new(proxy), Some(remote))
        } else {
            (Box::new(Ledger(Arc::clone(&heard))), None)
        };
    let engine = vike_execution::Engine::new(
        vike_execution::Account::new(1.0, "sim", None, vike_execution::BalanceMode::Delta),
        vike_execution::RiskGate::new(vike_model::RiskLimits::new()),
        crate::test_support::doubles::NoopClient,
        "sim",
        "BTCUSDT",
    );
    let mount = vike_core::Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy,
    };
    let core = vike_core::spawn_core(
        engine,
        vike_core::CoreConfig { strategy: Some(mount), ..vike_core::CoreConfig::default() },
    );
    if let Some(remote) = &remote {
        remote.attach(core.event_sender());
    }
    for ts in 1..=3 {
        let trade = vike_marketdata::TickTrade {
            timestamp: ts,
            local_timestamp: 0,
            price: 100.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: Default::default(),
        };
        let update =
            vike_execution::TradeUpdate { venue: "sim".into(), symbol: "BTCUSDT".into(), trade };
        core.tick_sender().trade(update).unwrap();
    }
    until("six things heard", || heard.lock().unwrap().len() >= 6);
    core.shutdown_and_join();
    heard.lock().unwrap().clone()
}

/// THE ORDER (ruling 4 of the F5 review): a sandboxed mount hears every consequence of its orders (here
/// the core's refusal of each) BEFORE the next hook, exactly as in-process, although the next trades
/// reached the core while its reply was still travelling.
#[test]
fn a_sandboxed_mount_hears_its_orders_consequences_in_the_in_process_order() {
    let in_process = heard_on_a_core(false);
    assert_eq!(
        in_process,
        ["trade 1", "refused 1", "trade 2", "refused 2", "trade 3", "refused 3"],
        "the in-process order this pins"
    );
    assert_eq!(heard_on_a_core(true), in_process);
}

/// A sandboxed mount whose child records the facts of every open, with its static fee floor at `static_rate`.
fn opens_of(static_rate: Option<f64>) -> (RemoteStrategy, RemoteHandle, Opens) {
    let opens: Opens = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&opens);
    let launcher: Launcher = Box::new(move || {
        let (child_in, to_child) = std::io::pipe()?;
        let (from_child, child_out) = std::io::pipe()?;
        let log = Arc::clone(&log);
        std::thread::spawn(move || {
            let build = |open: &protocol::Open| {
                log.lock().unwrap().push(open.facts.clone());
                Ok((Probe { mode: Mode::Quote, fills: 0 }, None))
            };
            let _ = serve(child_in, child_out, BUILD, build);
        });
        Ok(Launched {
            child: Box::new(ThreadChild),
            to_child: Box::new(to_child),
            from_child: Box::new(from_child),
            reasons: None,
        })
    });
    let mut spec = spec();
    spec.facts.static_round_trip_fee_rate = static_rate;
    let (proxy, remote) = RemoteStrategy::open(spec, launcher).expect("it opens");
    (proxy, remote, opens)
}

type Opens = Arc<std::sync::Mutex<Vec<vike_model::params::MountFacts>>>;

/// The floor the IN-PROCESS maker was re-armed to for the same account (`vike_market_making::rearm_fee_floor` over its
/// params bag at the static lane's floor), or `None` when it kept its maker.
fn in_process_floor(static_rate: f64, schedule: Option<vike_model::FeeSchedule>) -> Option<f64> {
    let mut params = vike_model::params::Params::default();
    params.insert("round_trip_fee_rate", static_rate);
    let armed = vike_market_making::rearm_fee_floor(&params, schedule, "sim", "BTCUSDT")?;
    armed.get("round_trip_fee_rate").and_then(vike_model::params::ParamValue::as_f64)
}

/// F6b's finding 2: a sandboxed maker gets the SAME fee floor as the in-process one for the same account, at
/// the same moment (the assembly's re-arm, before the core runs): the account's own rate rebuilds the file
/// with it in its facts when the in-process law moves the floor, and leaves the file as it opened when it
/// does not (a rate equal to the static lane's, a shape with no flat rate, no engine answering).
#[test]
fn a_sandboxed_maker_is_re_armed_to_the_in_process_floor() {
    let static_rate = 0.0004; // 2 bps a side, the static lane
    let account = vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 5.5 };
    let (_proxy, remote, opens) = opens_of(Some(static_rate));
    assert!(remote.fee_rearm()(Some(account)).is_none(), "the mount keeps its proxy");
    let opened = opens.lock().unwrap().clone();
    assert_eq!(opened.len(), 2, "the file was rebuilt once: {opened:?}");
    assert_eq!(opened[0].account_round_trip_fee_rate, None, "it opened at the static lane");
    let expected = in_process_floor(static_rate, Some(account));
    assert_eq!(expected, Some(0.0002), "the in-process maker's re-armed floor");
    assert_eq!(opened[1].account_round_trip_fee_rate, expected, "the same floor, sandboxed");
    assert_eq!(
        opened[1].static_round_trip_fee_rate,
        Some(static_rate),
        "the rest of the facts stay"
    );

    for schedule in [
        Some(vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 }),
        Some(vike_model::FeeSchedule::Free),
        None,
    ] {
        assert_eq!(in_process_floor(static_rate, schedule), None, "in-process keeps its maker");
        let (_proxy, remote, opens) = opens_of(Some(static_rate));
        assert!(remote.fee_rearm()(schedule).is_none());
        assert_eq!(opens.lock().unwrap().len(), 1, "nothing rebuilt at {schedule:?}");
    }
}

/// Book W35: a forecast the sandboxed file publishes is recorded as its own call (not an order, so the mount's
/// symbol check does not hold it) and replayed onto the core's `LiveBroker`, which buffers it for the drain to
/// journal, the symbol as named (`""` as `None`, the mount's own).
#[test]
fn a_forecast_crosses_from_the_child_to_the_cores_broker() {
    let forecast = vike_model::Forecast::new(0.5, 60_000, None).unwrap();
    let mut child = ViewBroker::default();
    child.publish_forecast("", forecast);
    child.publish_forecast("XRPUSDT", forecast);
    assert!(child.calls.iter().all(|c| c.symbol().is_none()), "no symbol check holds a forecast");
    let mut core = ctx(0.0, 7);
    for call in std::mem::take(&mut child.calls) {
        call.apply(&mut core);
    }
    assert_eq!(core.forecasts, [(None, forecast), (Some("XRPUSDT".to_string()), forecast)]);
    assert!(core.submissions.is_empty(), "a forecast is no order");
}

/// The child's stderr is its log's only way out of the jail: each line reaches the daemon's log under
/// the mount's id (the spread maker's break-even refusal was said there and nowhere else), the crash
/// tail still keeps the end, and a child printing per tick is held to the budget, the rest counted.
#[test]
fn the_childs_stderr_lines_reach_the_daemons_log_within_a_budget() {
    let refusal = "2026-11-19T07:12:25Z  WARN strategy_file: maker HOLDING: no half-spread is both \
                   under max_half_spread_ticks and above the round-trip fee \
                   break_even_half_spread=12.73 max_half_spread=6.0";
    let mut stderr = format!("{refusal}\n");
    for i in 0..100 {
        stderr.push_str(&format!("tick {i}\n"));
    }
    stderr.push_str("a last line with no newline");
    let tail = std::sync::Mutex::new(std::collections::VecDeque::new());
    let ((), events) = vike_log::capture::captured(|| {
        supervisor::read_reasons(stderr.as_bytes(), &tail, "bybit__BTCUSDT_P__1m");
    });
    let logged: Vec<&str> = events
        .iter()
        .filter(|e| e.level == tracing::Level::INFO)
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(logged.len(), 60, "the budget holds a per-tick printer: {logged:?}");
    assert_eq!(logged[0], format!("sandboxed strategy: {refusal}"), "the line, whole");
    assert!(
        events
            .iter()
            .all(|e| e.fields.get("mount_id").map(String::as_str) == Some("bybit__BTCUSDT_P__1m")),
        "every line names its mount: {events:?}"
    );
    let dropped: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::WARN).collect();
    assert_eq!(dropped.len(), 1, "the rest are counted once: {events:?}");
    assert_eq!(dropped[0].fields.get("dropped").map(String::as_str), Some("42"));
    let tail: Vec<u8> = tail.into_inner().expect("not poisoned").into_iter().collect();
    assert!(tail.ends_with(b"a last line with no newline"), "the crash tail keeps the end");
}

// The lean pipe (2026-11-19, decision 0281's amendment of that day): positional messages behind a build
// check, and the state as the text the strategy wrote.

/// `msg` framed as the pipe framed it before 2026-11-19: every struct a map of its field names.
fn named_frame<T: serde::Serialize>(msg: &T) -> Vec<u8> {
    let body = rmp_serde::to_vec_named(msg).expect("msgpack");
    let mut bytes = u32::try_from(body.len()).expect("a frame").to_be_bytes().to_vec();
    bytes.extend(body);
    bytes
}

/// `msg` framed as the pipe frames it.
fn frame<T: serde::Serialize>(msg: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    child_pipe::write_message(&mut bytes, msg).expect("a frame");
    bytes
}

/// A closed row; with an empty `tag` its body leaves `TradeClosedRow::tag` out (second of thirteen).
fn closed_row(tag: &str, net: f64) -> vike_model::TradeClosedRow {
    vike_model::TradeClosedRow {
        symbol: "BTCUSDT".into(),
        tag: tag.into(),
        side: -1,
        entry_timestamp: 1_000,
        entry_bar: 2,
        entry_price: 100.0,
        exit_timestamp: 2_000,
        exit_bar: 3,
        exit_price: 99.5,
        qty: 0.25,
        pnl: 0.125,
        commission: 0.01,
        net,
    }
}

/// A group leg; with no `trail` its body leaves `OrderListLeg::trail` out (sixth of twelve).
fn leg(trail: Option<f64>) -> vike_model::OrderListLeg {
    vike_model::OrderListLeg {
        side: -1,
        qty: 2.0,
        order_type: vike_model::OrderType::default(),
        price: Some(101.0),
        trigger_price: None,
        trail,
        reduce_only: true,
        time_in_force: vike_model::TimeInForce::default(),
        parent: Some(0),
        contingency: None,
        linked: vec![0],
        order_client_id: None,
    }
}

/// The maker's params with a ladder and no refresh tolerance, both left out of a body when `None`.
fn maker_params() -> vike_model::StrategyParams {
    vike_model::StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
        qty: 0.001,
        half_spread: 1.5,
        target_inventory: 0.0,
        max_inventory: 0.01,
        skew: 0.5,
        fill_window_ms: 60_000,
        net_fill_threshold: 0.002,
        suppress_cooldown_ms: 30_000,
        style: vike_model::QuoteStyle::default(),
        depth_levels: 3,
        tick_size: 0.1,
        filter_own: true,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: Some(vike_model::LadderParams {
            levels: 3,
            offset_step: 2.0,
            offset_unit: vike_model::LadderOffsetUnit::default(),
            size_profile: vike_model::LadderSizeProfile::default(),
            size_ratio: 1.5,
        }),
        reward: None,
        toxicity: None,
    })
}

fn quote(bid: f64) -> vike_marketdata::TickQuote {
    vike_marketdata::TickQuote {
        timestamp: 5,
        local_timestamp: 6,
        bid,
        ask: bid + 1.0,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: "BTCUSDT".into(),
    }
}

/// A book of `levels` a side around `mid`.
fn book(mid: f64, levels: usize) -> vike_marketdata::L2Book {
    let side = |sign: f64| -> Vec<vike_marketdata::L2BookLevel> {
        (1..=levels)
            .map(|k| vike_marketdata::L2BookLevel::new(mid + sign * 0.1 * k as f64, k as f64 * 0.5))
            .collect()
    };
    let mut book = vike_marketdata::L2Book::new(0.1);
    book.apply_snapshot(7, &side(-1.0), &side(1.0));
    book
}

/// One of every hook, at `i`, the optional fields both ways.
fn every_hook(i: i64) -> Vec<Hook> {
    let price = 100.0 + i as f64 * 0.5;
    let fill = |tag: Option<&str>| vike_model::Fill {
        side: -1,
        size: 0.5,
        price,
        fee: 0.01,
        timestamp: i,
        is_maker: true,
        symbol: "BTCUSDT".into(),
        tag: tag.map(Into::into),
    };
    let order_event = |id: String, tag: Option<&str>, kind| {
        Hook::OrderEvent(vike_model::OrderEvent {
            order_client_id: id,
            tag: tag.map(Into::into),
            kind,
        })
    };
    vec![
        Hook::Start,
        Hook::Bar(Bar {
            funding: Some(0.0001),
            bid: Some(price - 0.5),
            symbol: Some("BTCUSDT.BYBIT".into()),
            ..bar(i, price)
        }),
        Hook::TickQuote(quote(price)),
        Hook::TickTrade(vike_marketdata::TickTrade {
            timestamp: i,
            local_timestamp: i + 1,
            price,
            size: 0.25,
            is_buyer_maker: i % 2 == 0,
            symbol: Default::default(),
        }),
        Hook::OrderBook(book(price, 3)),
        Hook::Schedule("rebalance".into()),
        Hook::Fill(fill(None)),
        Hook::Fill(fill(Some("bid"))),
        Hook::FeedStatus(vike_model::FeedStatus::Stale),
        Hook::Mark(vike_model::TickMark { symbol: "btcusdt".into(), price, timestamp: i }),
        Hook::ReferenceQuote { venue: "binance".into(), quote: quote(price - 1.0) },
        Hook::Flow(vike_model::FlowToxicity { bid: 0.25, ask: 0.75, timestamp: i }),
        order_event(
            format!("c{i}"),
            None,
            vike_model::OrderEventKind::Canceled { reason: "venue".into() },
        ),
        order_event(format!("d{i}"), Some("ask"), vike_model::OrderEventKind::Filled),
        Hook::ParamsUpdated(Box::new(maker_params())),
        Hook::LoadState(StrategyState::from_json_value(&serde_json::json!({ "fills": i }))),
        Hook::Stop,
    ]
}

/// The view of event `i` for a child that holds `held` bars and `closed` closed rows: a bar appends
/// one, every third event appends a closed row (untagged and tagged in turn), and a row's NaN crosses.
fn view_at(i: i64, hook: &Hook, held: &mut usize, closed: &mut usize) -> View {
    let row = ViewRow {
        name: String::new(),
        position: (i % 5) as f64,
        price: 100.0 + i as f64,
        average_price: if i % 4 == 0 { f64::NAN } else { 100.0 },
        data_delay: (i % 2 == 0).then_some(vike_model::StreamDelay::Delayed { lag_ms: None }),
        grid: Some(vike_model::Grid {
            tick_size: Some(0.1),
            lot_size: None,
            min_notional: Some(5.0),
            min_qty: None,
        }),
        contract_value: Some(vike_model::ContractValue::default()),
    };
    let bars = match hook {
        Hook::Bar(b) => {
            *held += 1;
            vec![SeriesDelta { name: String::new(), from: *held - 1, bars: vec![b.clone()] }]
        }
        _ => Vec::new(),
    };
    let rows = if i % 3 == 0 {
        vec![closed_row(if i % 2 == 0 { "" } else { "bid" }, i as f64 * 0.01)]
    } else {
        Vec::new()
    };
    let closed_from = *closed;
    *closed += rows.len();
    View {
        rows: vec![row],
        declared: false,
        equity: 1_000.0 + i as f64,
        index: *held,
        now: i,
        bars,
        inputs: Vec::new(),
        closed_from,
        closed: rows,
    }
}

fn open_message(state: Option<&str>) -> ToChild {
    ToChild::Open(Box::new(Open {
        file: "market_making/maker.rs".into(),
        sha256: "ab12".into(),
        params: "qty = 0.001\n".into(),
        facts: vike_model::params::MountFacts {
            tick_size: Some(0.1),
            static_round_trip_fee_rate: Some(0.0004),
            ..spec().facts
        },
        state: state.map(json_state),
    }))
}

/// The state whose document is the JSON `text` (msgpack with field names).
fn json_state(text: &str) -> StrategyState {
    StrategyState::from_json_value(&serde_json::from_str(text).expect("the test's JSON"))
}

/// One of every message each way, the optional fields both ways.
fn every_message() -> (Vec<ToChild>, Vec<FromChild>) {
    let mut to_child = vec![open_message(Some(r#"{"fills": [1, 2.5]}"#)), open_message(None)];
    let (mut held, mut closed) = (0, 0);
    for (n, hook) in (0..).zip(every_hook(3)) {
        let view = (!matches!(hook, Hook::LoadState(_)))
            .then(|| view_at(n, &hook, &mut held, &mut closed));
        to_child.push(ToChild::Event(Box::new(Event { sequence: n.unsigned_abs(), hook, view })));
    }
    let forecast = |value, confidence| {
        vike_model::Forecast::new(value, 60_000, confidence).expect("a forecast")
    };
    let calls = vec![
        BrokerCall::Place {
            symbol: String::new(),
            tag: "bid".into(),
            side: 1,
            qty: 0.5,
            order: OrderPlacement::Limit { price: 99.5 },
            reduce_only: false,
        },
        BrokerCall::Place {
            symbol: "BTCUSDT".into(),
            tag: "peg".into(),
            side: -1,
            qty: 0.5,
            order: OrderPlacement::Pegged {
                peg: vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Mid, offset: -0.1 },
            },
            reduce_only: true,
        },
        BrokerCall::Place {
            symbol: String::new(),
            tag: "tp".into(),
            side: 1,
            qty: 0.5,
            order: OrderPlacement::TakeProfit { trigger: 105.0, limit: None },
            reduce_only: false,
        },
        BrokerCall::PlaceGroup {
            symbol: "BTCUSDT".into(),
            legs: vec![("entry".into(), leg(None)), ("trail".into(), leg(Some(1.5)))],
        },
        BrokerCall::Amend {
            tag: "bid".into(),
            new_qty: None,
            new_price: Some(99.6),
            new_trigger: None,
        },
        BrokerCall::Cancel { tag: "ask".into() },
        BrokerCall::Deny { symbol: String::new(), tag: "x".into(), qty: 1.0, reason: "no".into() },
        BrokerCall::PublishForecast { symbol: String::new(), forecast: forecast(0.5, None) },
        BrokerCall::PublishForecast {
            symbol: "ETHUSDT".into(),
            forecast: forecast(-0.5, Some(0.9)),
        },
    ];
    let from_child = vec![
        FromChild::Opened(Opened {
            symbols: vec!["ETHUSDT".into()],
            inputs: vec!["funding".into()],
            warmup: 3,
            state: Some(json_state("{}")),
            params: Some(Box::new(maker_params())),
            echo: Some(vec![("qty".into(), "0.001".into())]),
        }),
        FromChild::Opened(Opened {
            symbols: Vec::new(),
            inputs: Vec::new(),
            warmup: 0,
            state: None,
            params: None,
            echo: None,
        }),
        FromChild::Refused { reason: "built for version 7".into(), id: None },
        FromChild::Refused {
            reason: "`x.rs` did not build from its params and mount facts".into(),
            id: Some("strategy.sandbox.not_created".into()),
        },
        FromChild::Reply { sequence: 7, calls },
        FromChild::Reply { sequence: 8, calls: Vec::new() },
        FromChild::State {
            state: Some(json_state(r#"{"fills": 5, "note": "café"}"#)),
            params: Some(Box::new(maker_params())),
        },
        FromChild::State { state: None, params: None },
    ];
    (to_child, from_child)
}

/// Every message, each way, reads back from the positional pipe as it reads back named (the pipe before
/// 2026-11-19), field for field, and in fewer bytes. The payloads that leave a field out in the MIDDLE of
/// a struct cross named inside (`vike_hub_protocol::child_pipe::named`), and the end shows why: positional alone, a closed
/// row with no tag reads its later fields into the wrong slots.
#[test]
fn every_message_crosses_positional_as_it_crosses_named() {
    fn both<T>(msg: &T) -> (usize, usize)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let (positional, named) = (frame(msg), named_frame(msg));
        let a: T = child_pipe::read_message(&mut positional.as_slice()).expect("positional reads");
        let b: T = child_pipe::read_message(&mut named.as_slice()).expect("named reads");
        let want = format!("{msg:?}");
        assert_eq!(format!("{a:?}"), want, "positional");
        assert_eq!(format!("{b:?}"), want, "named");
        (positional.len(), named.len())
    }
    let (to_child, from_child) = every_message();
    let (mut positional, mut named) = (0, 0);
    for (p, n) in to_child.iter().map(both).chain(from_child.iter().map(both)) {
        assert!(p <= n, "positional is never the larger: {p} > {n}");
        (positional, named) = (positional + p, named + n);
    }
    assert!(positional < named, "{positional} B positional, {named} B named");
    let row = closed_row("", 1.0);
    let bare = rmp_serde::to_vec(&row).expect("msgpack");
    let back = rmp_serde::from_slice::<vike_model::TradeClosedRow>(&bare);
    assert!(!back.is_ok_and(|back| back == row), "without the island this row does not cross");
}

/// The messages are positional, so another build is refused before one is read, in words naming both, at
/// both ends: the tradehub's open fails with them, and the child answers nothing but its own build. A
/// first frame that is no build at all (the pipe before the check) is refused the same way.
#[test]
fn another_build_is_refused_in_words_at_both_ends() {
    let other = "git 0123abcd clean, built 2026-01-01T00:00:00Z, rustc 1.0.0, nowhere".to_string();
    let theirs = other.clone();
    let launcher: Launcher = Box::new(move || {
        let (mut child_in, to_child) = std::io::pipe()?;
        let (from_child, mut child_out) = std::io::pipe()?;
        let theirs = theirs.clone();
        std::thread::spawn(move || {
            let _ = child_pipe::write_message(&mut child_out, &theirs);
            let _ = std::io::copy(&mut child_in, &mut std::io::sink());
        });
        Ok(Launched {
            child: Box::new(ThreadChild),
            to_child: Box::new(to_child),
            from_child: Box::new(from_child),
            reasons: None,
        })
    });
    let Err(reason) = RemoteStrategy::open(spec(), launcher) else {
        panic!("another build opened")
    };
    let reason = reason.to_string();
    assert!(reason.contains(&other), "{reason}");
    assert!(reason.contains(BUILD), "{reason}");

    for (first, words) in
        [(frame(&other), other.as_str()), (named_frame(&open_message(None)), "no build")]
    {
        let mut input = first;
        input.extend(frame(&open_message(None)));
        let mut output = Vec::new();
        let refused = serve(input.as_slice(), &mut output, BUILD, |_| {
            Ok((Probe { mode: Mode::Quote, fills: 0 }, None))
        })
        .expect_err("another build is refused");
        assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported, "{refused}");
        assert!(refused.to_string().contains(words), "{refused}");
        assert_eq!(output, frame(&BUILD), "nothing but its own build");
    }
}

/// The state crosses as the bytes the strategy encoded (msgpack with field names, decision 0296's ABI 15):
/// the tradehub keeps them as they are, hands them back byte for byte to a restarted child, and the core
/// saves exactly them, nothing decoded or re-encoded on the way, a NaN and a negative zero included.
#[test]
fn the_saved_state_crosses_as_its_bytes() {
    #[derive(serde::Serialize)]
    struct Saved {
        trades: Vec<(f64, f64)>,
        note: &'static str,
        n: u64,
        neg: f64,
    }
    let state = StrategyState::of(&Saved {
        trades: vec![(0.1 + 0.2, 1e-7), (65_000.5, f64::NAN)],
        note: "café \"q\"",
        n: u64::MAX,
        neg: -0.0,
    })
    .expect("encodes");
    struct Fixed(Mode, StrategyState);
    impl Strategy<ViewBroker> for Fixed {
        fn on_bar(&mut self, _b: &mut ViewBroker, _bar: &Bar) {
            assert!(self.0 != Mode::Panic, "the first child dies on a bar");
        }
        fn save_state(&self) -> Option<StrategyState> {
            Some(self.1.clone())
        }
    }
    let opens = Arc::new(Mutex::new(Vec::new()));
    let starts = Arc::new(AtomicUsize::new(0));
    let (log, counter, theirs) = (Arc::clone(&opens), Arc::clone(&starts), state.clone());
    let launcher: Launcher = Box::new(move || {
        let mode =
            if counter.fetch_add(1, Ordering::SeqCst) == 0 { Mode::Panic } else { Mode::Quote };
        let (child_in, to_child) = std::io::pipe()?;
        let (from_child, child_out) = std::io::pipe()?;
        let (log, theirs) = (Arc::clone(&log), theirs.clone());
        std::thread::spawn(move || {
            let build = |open: &Open| {
                log.lock().unwrap().push(open.state.clone());
                Ok((Fixed(mode, theirs), None))
            };
            let _ = serve(child_in, child_out, BUILD, build);
        });
        Ok(Launched {
            child: Box::new(ThreadChild),
            to_child: Box::new(to_child),
            from_child: Box::new(from_child),
            reasons: None,
        })
    });
    let (mut proxy, handle) = RemoteStrategy::open(spec(), launcher).expect("it opens");
    proxy.on_bar(&mut ctx(0.0, 1), &bar(1, 1.0)); // the first child dies here
    until("the second open", || opens.lock().unwrap().len() == 2);
    until("the restart to be said", || handle.status() == RemoteStatus::Running);
    assert_eq!(opens.lock().unwrap()[1].as_ref(), Some(&state), "handed back byte for byte");
    assert_eq!(proxy.save_state(), Some(state), "the core saves the very bytes");
}

/// A strategy file whose `save_state` bytes stop being msgpack is stopped ONCE, in those words, when the
/// changed bytes arrive: no crash, no restart (one would be handed the same bytes), nothing more reaches
/// it, and the last good state is kept. One whose state is not msgpack at its open does not mount, same
/// words. Each kind `vike_model::StrategyState::check` refuses (the check the sidecar's read and the child's
/// load door make too): a reserved marker, bytes after the value, a value cut short.
#[test]
fn a_state_that_is_not_msgpack_stops_the_mount_once() {
    /// A good state (an empty map) until the first fill, then `broken`; or `broken` from the open.
    struct Broken {
        fills: u64,
        never: bool,
        broken: Vec<u8>,
    }
    impl Strategy<ViewBroker> for Broken {
        fn on_fill(&mut self, _b: &mut ViewBroker, _fill: &vike_model::Fill) {
            self.fills += 1;
        }
        fn save_state(&self) -> Option<StrategyState> {
            let good = self.fills == 0 && !self.never;
            Some(StrategyState::from_bytes(if good { vec![0x80] } else { self.broken.clone() }))
        }
    }
    fn launcher(broken: &[u8], never: bool, starts: Arc<AtomicUsize>) -> Launcher {
        let broken = broken.to_vec();
        Box::new(move || {
            starts.fetch_add(1, Ordering::SeqCst);
            let (child_in, to_child) = std::io::pipe()?;
            let (from_child, child_out) = std::io::pipe()?;
            let broken = broken.clone();
            std::thread::spawn(move || {
                let child = Broken { fills: 0, never, broken };
                let _ = serve(child_in, child_out, BUILD, |_| Ok((child, None)));
            });
            Ok(Launched {
                child: Box::new(ThreadChild),
                to_child: Box::new(to_child),
                from_child: Box::new(from_child),
                reasons: None,
            })
        })
    }
    const WORDS: &str = "the strategy's save_state is not valid msgpack: ";
    let fill = vike_model::Fill {
        side: 1,
        size: 1.0,
        price: 1.0,
        fee: 0.0,
        timestamp: 1,
        is_maker: true,
        symbol: "BTCUSDT".into(),
        tag: None,
    };
    let refused: [&[u8]; 3] = [&[0x81, 0xc1], &[0x80, 0x80], &[0x92, 0x01]];
    for broken in refused {
        assert!(StrategyState::check(broken).is_err(), "{broken:02x?} is refused by the check");
        let starts = Arc::new(AtomicUsize::new(0));
        let (mut proxy, handle) =
            RemoteStrategy::open(spec(), launcher(broken, false, Arc::clone(&starts)))
                .expect("a msgpack state opens");
        proxy.on_fill(&mut ctx(0.0, 1), &fill);
        until("the stop", || matches!(handle.status(), RemoteStatus::Stopped { .. }));
        let RemoteStatus::Stopped { reason } = handle.status() else { unreachable!() };
        assert!(reason.starts_with(WORDS), "{broken:02x?}: {reason}");
        proxy.on_bar(&mut ctx(0.0, 2), &bar(2, 1.0));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(starts.load(Ordering::SeqCst), 1, "{broken:02x?}: never restarted");
        assert_eq!(handle.status(), RemoteStatus::Stopped { reason }, "stopped once, as it was");
        let kept = Some(StrategyState::from_bytes(vec![0x80]));
        assert_eq!(proxy.save_state(), kept, "the last good state is kept");

        let starts = Arc::new(AtomicUsize::new(0));
        let Err(reason) = RemoteStrategy::open(spec(), launcher(broken, true, Arc::clone(&starts)))
        else {
            panic!("a state that is not msgpack mounted: {broken:02x?}")
        };
        assert!(reason.to_string().starts_with(WORDS), "{broken:02x?}: {reason}");
        assert_eq!(starts.load(Ordering::SeqCst), 1, "{broken:02x?}: one open, no restart");
    }
}

/// The replay's child: an order verb on most hooks, made from the fields it was handed, and a state that
/// remembers what it heard, so a field read into the wrong place changes an order or the state.
#[derive(Default)]
struct Replay {
    heard: u64,
    sum: f64,
    notes: Vec<String>,
    /// The document behind the last state the serve loop read: what the pipe before 2026-11-19 carried.
    said: Arc<Mutex<Option<serde_json::Value>>>,
}

impl Replay {
    fn note(&mut self, what: String) {
        self.heard += 1;
        match self.notes.len() {
            0..8 => self.notes.push(what),
            n => self.notes[usize::try_from(self.heard).expect("small") % n] = what,
        }
    }
}

impl Strategy<ViewBroker> for Replay {
    fn on_start(&mut self, _b: &mut ViewBroker) {
        self.note("start".into());
    }
    fn on_bar(&mut self, b: &mut ViewBroker, bar: &Bar) {
        let held = b.bars("").len() as f64;
        let closed = b
            .closed_trades()
            .map_or(0.0, |rows| rows.iter().map(|r| r.net + r.tag.len() as f64).sum::<f64>());
        self.sum += closed;
        self.note(format!("bar {} {:?} {:?} held {held}", bar.close, bar.funding, bar.symbol));
        b.place("", "bid", 1, held, OrderPlacement::Limit { price: bar.close - 1.0 }, false);
    }
    fn on_quote_tick(&mut self, b: &mut ViewBroker, q: &vike_marketdata::TickQuote) {
        b.amend("bid", None, Some(q.bid), None);
        b.amend("ask", Some(q.ask_size), Some(q.ask), None);
    }
    fn on_trade_tick(&mut self, b: &mut ViewBroker, t: &vike_marketdata::TickTrade) {
        b.deny("", "taker", t.size, &format!("trade {} {}", t.price, t.is_buyer_maker));
    }
    fn on_order_book(&mut self, b: &mut ViewBroker, book: &vike_marketdata::L2Book) {
        if let Some(level) = book.best_ask() {
            b.amend("ask", Some(level.qty), Some(level.price), None);
        }
    }
    fn on_schedule(&mut self, _b: &mut ViewBroker, tag: &str) {
        self.note(format!("schedule {tag}"));
    }
    fn on_fill(&mut self, b: &mut ViewBroker, fill: &vike_model::Fill) {
        self.sum += fill.size * fill.price;
        self.note(format!("fill {:?}", fill.tag));
        b.cancel("bid");
    }
    fn on_feed_status(&mut self, _b: &mut ViewBroker, status: vike_model::FeedStatus) {
        self.note(format!("{status:?}"));
    }
    fn on_mark(&mut self, b: &mut ViewBroker, mark: &vike_model::TickMark) {
        let forecast =
            vike_model::Forecast::new(mark.price / 1e3, 60_000, None).expect("a forecast");
        b.publish_forecast("", forecast);
    }
    fn on_reference_quote(
        &mut self,
        b: &mut ViewBroker,
        venue: &str,
        q: &vike_marketdata::TickQuote,
    ) {
        b.amend("bid", None, Some(q.bid.min(b.price(""))), None);
        self.note(format!("{venue} {}", b.average_price("")));
    }
    fn on_flow(&mut self, _b: &mut ViewBroker, flow: vike_model::FlowToxicity) {
        self.sum += flow.bid - flow.ask;
    }
    fn on_order_event(&mut self, b: &mut ViewBroker, event: &vike_model::OrderEvent) {
        self.note(format!("{event:?}"));
        b.place_group(
            "",
            vec![("entry".into(), leg(None)), ("stop".into(), leg(Some(b.position(""))))],
        );
    }
    fn on_params_updated(&mut self, _b: &mut ViewBroker, params: &vike_model::StrategyParams) {
        self.note(format!("{params:?}"));
    }
    fn on_stop(&mut self, _b: &mut ViewBroker) {
        self.note("stop".into());
    }
    fn save_state(&self) -> Option<StrategyState> {
        let document =
            serde_json::json!({ "heard": self.heard, "sum": self.sum, "notes": self.notes });
        *self.said.lock().unwrap() = Some(document.clone());
        Some(StrategyState::from_json_value(&document))
    }
    fn load_state(&mut self, state: &StrategyState) {
        let document = state.to_json_value().expect("the replay's states are msgpack");
        self.note(format!("loaded {document}"));
    }
}

/// The stream the replay sends: the open, then forty rounds of every hook with its view.
fn recorded_stream() -> Vec<ToChild> {
    let mut stream = vec![open_message(None)];
    let (mut held, mut closed, mut sequence) = (0, 0, 0u64);
    for round in 0..40 {
        for hook in every_hook(round) {
            sequence += 1;
            let i = i64::try_from(sequence).expect("small");
            let view = (!matches!(hook, Hook::LoadState(_)))
                .then(|| view_at(i, &hook, &mut held, &mut closed));
            stream.push(ToChild::Event(Box::new(Event { sequence, hook, view })));
        }
    }
    stream
}

/// The child's answers to `stream` framed by `encode`: its replies, the bytes of its last state, and the
/// document behind that state.
fn replay(
    stream: &[ToChild],
    encode: fn(&ToChild) -> Vec<u8>,
) -> (Vec<String>, Option<StrategyState>, Option<serde_json::Value>) {
    let mut input = frame(&BUILD);
    for message in stream {
        input.extend(encode(message));
    }
    let said = Arc::new(Mutex::new(None));
    let child = Replay { said: Arc::clone(&said), ..Replay::default() };
    let mut output = Vec::new();
    serve(input.as_slice(), &mut output, BUILD, |_| Ok((child, None)))
        .expect("the child serves the whole stream");
    let mut answers = output.as_slice();
    let build: String = child_pipe::read_message(&mut answers).expect("its build first");
    assert_eq!(build, BUILD);
    let (mut replies, mut state) = (Vec::new(), None);
    while !answers.is_empty() {
        match child_pipe::read_message(&mut answers).expect("an answer") {
            reply @ FromChild::Reply { .. } => replies.push(format!("{reply:?}")),
            FromChild::State { state: text, .. } => state = text,
            other => assert!(matches!(other, FromChild::Opened(_)), "{other:?}"),
        }
    }
    let document = said.lock().unwrap().clone();
    (replies, state, document)
}

/// A recorded stream (every hook, forty rounds, each with its view) replayed through the child, framed as
/// the pipe framed it before 2026-11-19 and as it frames it now: the same orders in the same order and the
/// same last state, whose one parse is the document the old pipe carried. (A state the refresh clock adds
/// lands between replies and is not compared; the stream ends on `on_stop`, which states the last.)
#[test]
fn a_recorded_stream_replays_to_the_same_orders_and_state_in_either_encoding() {
    let stream = recorded_stream();
    let (old_replies, old_state, old_document) = replay(&stream, named_frame::<ToChild>);
    let (new_replies, new_state, new_document) = replay(&stream, frame::<ToChild>);
    assert_eq!(old_replies.len(), stream.len() - 1, "one reply an event");
    assert!(old_replies.iter().filter(|r| r.contains("PlaceGroup")).count() >= 80, "orders made");
    assert_eq!(new_replies, old_replies);
    assert_eq!(new_state, old_state);
    assert_eq!(new_document, old_document);
    let state = new_state.expect("a state was sent");
    assert_eq!(state.to_json_value().ok(), new_document, "the bytes are the document's msgpack");
}

/// The maker's state the shape the CI box's bybit maker writes (2026-11-19: 39.5 KB, almost all its
/// `as_state/trades`), `net_fill` (near the end of the text) the field that moves.
fn maker_state_text(trades: usize, net_fill: f64) -> String {
    let rows: Vec<serde_json::Value> = (0..trades)
        .map(|k| {
            serde_json::json!({
                "timestamp": 1_763_510_400_000_u64 + k as u64 * 731,
                "price": 91_234.5 + (k % 97) as f64 * 0.1,
                "size": 0.001 * (1 + k % 7) as f64,
                "buy": k % 3 == 0,
            })
        })
        .collect();
    serde_json::json!({
        "as_state": { "trades": rows, "sigma": 1.234e-4, "kappa": 1.75 },
        "breaker": { "bid_until": 0, "ask_until": 1_763_510_400_000_u64, "net_fill": net_fill },
    })
    .to_string()
}

/// PERF MEASUREMENT (not a gate): the pipe's bytes and time per event, the encoding before 2026-11-19
/// (named; the state a document the child parsed, compared and sent) against the one now (positional; the
/// state its msgpack bytes, compared as bytes and checked once). Each time is one side's encode plus the other's decode, the best of
/// five runs. Release only: `cargo test -p vike-tradehub-core --release --lib -- --ignored --nocapture
/// --exact remote_strategy::remote_strategy_tests::measure_the_pipe_per_event`.
#[test]
#[ignore = "release-only measurement"]
fn measure_the_pipe_per_event() {
    use std::hint::black_box;
    fn ns_per(n: u32, mut f: impl FnMut()) -> f64 {
        (0..5)
            .map(|_| {
                let t = Instant::now();
                for _ in 0..n {
                    f();
                }
                t.elapsed().as_nanos() as f64 / f64::from(n)
            })
            .fold(f64::INFINITY, f64::min)
    }
    fn trip<T>(label: &str, msg: &T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let (old, new) = (named_frame(msg).len(), frame(msg).len());
        let old_ns = ns_per(20_000, || {
            let bytes = named_frame(black_box(msg));
            black_box(child_pipe::read_message::<_, T>(&mut bytes.as_slice()).expect("named"));
        });
        let new_ns = ns_per(20_000, || {
            let bytes = frame(black_box(msg));
            black_box(child_pipe::read_message::<_, T>(&mut bytes.as_slice()).expect("positional"));
        });
        let share = 100.0 * new as f64 / old as f64;
        println!(
            "PIPE-BENCH {label}: bytes old={old} new={new} ({share:.0}%), encode+decode ns \
             old={old_ns:.0} new={new_ns:.0}"
        );
    }
    let maker_row = ViewRow {
        name: String::new(),
        position: 0.003,
        price: 91_234.5,
        average_price: 91_200.1,
        data_delay: Some(vike_model::StreamDelay::RealTime),
        grid: Some(vike_model::Grid {
            tick_size: Some(0.1),
            lot_size: Some(0.001),
            min_notional: Some(5.0),
            min_qty: Some(0.001),
        }),
        contract_value: Some(vike_model::ContractValue::default()),
    };
    let view = View {
        rows: vec![maker_row],
        declared: false,
        equity: 10_000.0,
        index: 1_440,
        now: 1_763_510_400_000,
        bars: Vec::new(),
        inputs: Vec::new(),
        closed_from: 12,
        closed: Vec::new(),
    };
    let event = |hook: Hook, view: &View| {
        ToChild::Event(Box::new(Event { sequence: 1, hook, view: Some(view.clone()) }))
    };
    let trade = vike_marketdata::TickTrade {
        timestamp: 1_763_510_400_000,
        local_timestamp: 1_763_510_400_003,
        price: 91_234.5,
        size: 0.004,
        is_buyer_maker: true,
        symbol: "BTCUSDT".into(),
    };
    let order_event = vike_model::OrderEvent {
        order_client_id: "V2abcdefgh0123456789".into(),
        tag: Some("bid".into()),
        kind: vike_model::OrderEventKind::Accepted,
    };
    let mut with_bar = view.clone();
    with_bar.bars =
        vec![SeriesDelta { name: String::new(), from: 1_440, bars: vec![bar(1, 91_234.5)] }];
    trip("quote", &event(Hook::TickQuote(quote(91_234.5)), &view));
    trip("trade", &event(Hook::TickTrade(trade), &view));
    trip("book-10", &event(Hook::OrderBook(book(91_234.5, 10)), &view));
    trip("book-25", &event(Hook::OrderBook(book(91_234.5, 25)), &view));
    trip("order-event", &event(Hook::OrderEvent(order_event), &view));
    trip("bar", &event(Hook::Bar(bar(1, 91_234.5)), &with_bar));
    let amend = |tag: &str, price| BrokerCall::Amend {
        tag: tag.into(),
        new_qty: Some(0.001),
        new_price: Some(price),
        new_trigger: None,
    };
    let two_amends = vec![amend("bid", 91_234.4), amend("ask", 91_234.6)];
    trip("reply-2-amends", &FromChild::Reply { sequence: 1, calls: two_amends });
    trip("reply-empty", &FromChild::Reply { sequence: 1, calls: Vec::new() });

    // The state refresh (at most once a second): what the child does from the text the file lent it (copied
    // in both encodings) to the tradehub holding the state, for a state that did not move and one that did.
    let (said, moved) = (maker_state_text(565, -0.002), maker_state_text(565, -0.003));
    let said_document: serde_json::Value = serde_json::from_str(&said).expect("JSON");
    let old_still = ns_per(200, || {
        let document: serde_json::Value =
            serde_json::from_str(&black_box(&said).clone()).expect("JSON");
        assert!(document == said_document);
    });
    let old_moved = ns_per(200, || {
        let document: serde_json::Value =
            serde_json::from_str(&black_box(&moved).clone()).expect("JSON");
        assert!(document != said_document);
        let bytes = named_frame(&document.clone());
        black_box(
            child_pipe::read_message::<_, serde_json::Value>(&mut bytes.as_slice()).expect("named"),
        );
    });
    // New (`ABI_VERSION` 15): the bytes the file lent are msgpack; a moved state is framed, read and
    // checked once (`supervisor.rs`'s `unusable_state`), never decoded.
    let msgpack = |text: &str| {
        StrategyState::from_json_value(&serde_json::from_str(text).expect("the maker's JSON"))
    };
    let (said_state, moved_state) = (msgpack(&said), msgpack(&moved));
    let new_still = ns_per(200, || {
        let state = black_box(&said_state).clone();
        assert!(state == said_state);
    });
    let new_moved = ns_per(200, || {
        let state = black_box(&moved_state).clone();
        assert!(state != said_state);
        let bytes = frame(&FromChild::State { state: Some(state), params: None });
        let read =
            child_pipe::read_message::<_, FromChild>(&mut bytes.as_slice()).expect("positional");
        let FromChild::State { state: Some(state), .. } = read else { panic!("a state: {read:?}") };
        StrategyState::check(state.bytes()).expect("msgpack");
        black_box(state);
    });
    println!(
        "PIPE-BENCH state-refresh {} B as JSON, {} B as msgpack: still old={:.1}us new={:.1}us, \
         moved old={:.1}us new={:.1}us",
        said.len(),
        said_state.bytes().len(),
        old_still / 1e3,
        new_still / 1e3,
        old_moved / 1e3,
        new_moved / 1e3
    );
}
