//! The sandboxed mount's two halves together, with no process: the child is [`serve`] on a thread over
//! two pipes, running a strategy written here, so every rule of the module root's failure table but the
//! jail itself is driven through the real proxy, supervisor and protocol code. The jail and the loaded
//! artifact are the real child's (`crates/vike-strategy-sandbox`'s tests).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_core::LiveBroker;
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, Strategy};

use super::protocol::{
    self, BrokerCall, Event, FromChild, Hook, SeriesDelta, ToChild, View, ViewBroker, ViewRow,
};
use super::proxy::{FoldView, Pending};
use super::*;

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
    ];
    let mut wire = Vec::new();
    protocol::write_message(&mut wire, &FromChild::Reply { sequence: 9, calls: calls.clone() })
        .unwrap();
    let back: FromChild = protocol::read_message(&mut wire.as_slice()).unwrap();
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
    protocol::write_message(&mut wire, &event).unwrap();
    let ToChild::Event(e) = protocol::read_message(&mut wire.as_slice()).unwrap() else {
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
            symbol: symbol.to_string(),
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
            let _ = serve(child_in, child_out, |_| Ok((Slow(log), None)));
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
    fn save_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "fills": self.fills }))
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        self.fills = state["fills"].as_u64().unwrap_or(0);
    }
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
            let _ = serve(child_in, child_out, |_| Ok((Probe { mode, fills: 0 }, None)));
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
    proxy.load_state(&serde_json::json!({ "fills": 4 }));
    proxy.on_bar(&mut ctx(0.0, 1), &bar(1, 1.0)); // the first child panics here
    proxy.on_fill(&mut ctx(0.0, 2), &fill);
    until("the restart", || starts.load(Ordering::SeqCst) == 2);
    let applied = next_reply(&mut rx, Duration::from_secs(5)).expect("the fill's cancel");
    assert_eq!(applied.cancels, vec!["bid".to_string()]);
    until("the state", || proxy.save_state() == Some(serde_json::json!({ "fills": 5 })));
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

/// A child that refuses the open (its file did not load) leaves the mount unopened, in the child's words.
#[test]
fn a_refused_open_is_the_mounts_reason() {
    let launcher: Launcher = Box::new(|| {
        let (child_in, to_child) = std::io::pipe()?;
        let (from_child, child_out) = std::io::pipe()?;
        std::thread::spawn(move || {
            let _ =
                serve::<Probe, _, _>(child_in, child_out, |_| Err("built for version 7".into()));
        });
        Ok(Launched {
            child: Box::new(ThreadChild),
            to_child: Box::new(to_child),
            from_child: Box::new(from_child),
            reasons: None,
        })
    });
    let Err(reason) = RemoteStrategy::open(spec(), launcher) else { panic!("it opened") };
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
                    let _ = serve(child_in, child_out, |_| Ok((Ledger(log), None)));
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
            symbol: String::new(),
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
            let _ = serve(child_in, child_out, |open: &protocol::Open| {
                log.lock().unwrap().push(open.facts.clone());
                Ok((Probe { mode: Mode::Quote, fills: 0 }, None))
            });
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
