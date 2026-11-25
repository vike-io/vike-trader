//! The owner's four timing examples of 2026-11-16 END TO END (decision 0285's amendment): a paper mount
//! driven the way the tradehub drives one, its candles and two-sided quotes through the node's
//! `CoreLaneSink`, a manual order as `Command::Order`, a strategy's orders from `on_bar`. On 1m and on 1h:
//! the 12:00 bar opens 100.00 and closes 101.00, the next bar's first price is 101.20, 300 ms into it.
//! Each price is the ASK (a buy pays it), the bid 0.10 under it.

use crate::common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_core::LiveBroker;
use vike_data::DataClientSink;
use vike_execution::{Command, MountSpec, OrderIntent};
use vike_log::poll::wait_until;
use vike_marketdata::{Bar, TickQuote};
use vike_model::{Fill, OrderBroker, OrderPlacement, OrderRequest, OrderType, Strategy};
use vike_tradehub_core::remote_strategy::protocol::ViewBroker;
use vike_tradehub_core::remote_strategy::{
    Launched, Launcher, RemoteHandle, RemoteSpec, RemoteStrategy, SandboxChild, serve,
};
use vike_tradehub_core::{
    CoreLaneSink, MountPlacement, StrategyCorePaper, StrategyCorePaperConfig,
    build_paper_strategy_core_with,
};

const HALT_ROOT_PREFIX: &str = "vike-tradehub-core-paper-timing-owns-this-halt-";
const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
/// The build both ends of the sandboxed mount's pipe state (the binary hands in its own).
const BUILD: &str = "paper_timing";
/// 2026-11-15 12:00:00 UTC: the bar "12:00".
const T0: i64 = 1_794_744_000_000;
const SPREAD: f64 = 0.10;
const WIDTHS: [(&str, i64); 2] = [("1m", 60_000), ("1h", 3_600_000)];

/// What the strategy does on the 12:00 close.
#[derive(Clone, Copy, PartialEq)]
enum Plan {
    Nothing,
    BuyMarket,
    /// a market buy and a stop-loss at 99.50 (`LiveBroker::submit_stop`, the core's emulated stop)
    BuyWithStopLoss,
}

/// The strategy: its plan on the 12:00 close, and every bar and quote it heard (the test's clock).
struct Scripted {
    plan: Plan,
    heard: Arc<Mutex<Vec<i64>>>,
}

impl Strategy<LiveBroker> for Scripted {
    fn on_bar(&mut self, b: &mut LiveBroker, bar: &Bar) {
        if bar.timestamp == T0 {
            if self.plan != Plan::Nothing {
                b.place("", "", 1, 1.0, OrderPlacement::Market, false);
            }
            if self.plan == Plan::BuyWithStopLoss {
                b.submit_stop(-1, 1.0, 99.50);
            }
        }
        self.heard.lock().unwrap().push(bar.timestamp);
    }

    fn on_quote_tick(&mut self, _b: &mut LiveBroker, q: &TickQuote) {
        self.heard.lock().unwrap().push(q.timestamp);
    }
}

struct Node {
    mount: StrategyCorePaper,
    sink: CoreLaneSink,
    /// What the boot strategy heard, `None` when it is not a [`Scripted`] (the core's mark is the clock then).
    heard: Option<Arc<Mutex<Vec<i64>>>>,
    interval: &'static str,
    width: i64,
    _halt_root: tempfile::TempDir,
}

impl Node {
    /// A paper mount of `plan` on `interval`; `fill_at_close` is its file's `fill = "close"`.
    fn new(plan: Plan, interval: &'static str, width: i64, fill_at_close: bool) -> Node {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let strategy = Box::new(Scripted { plan, heard: Arc::clone(&heard) });
        Node::with(strategy, Some(heard), (interval, width), fill_at_close, None)
    }

    /// A paper mount of `strategy` on `(interval, width)` whose file says `fill_at_close`, the core resolving
    /// a run-time mount through `factory`.
    fn with(
        strategy: Box<dyn Strategy<LiveBroker> + Send>,
        heard: Option<Arc<Mutex<Vec<i64>>>>,
        (interval, width): (&'static str, i64),
        fill_at_close: bool,
        strategy_factory: Option<vike_core::StrategyFactory>,
    ) -> Node {
        let (halt_root, halt) = common::no_halt(HALT_ROOT_PREFIX);
        let mut spec = MountPlacement::new(VENUE, SYMBOL, 1_000_000.0, 0.01);
        (spec.interval, spec.interval_ms, spec.fill_at_close) =
            (interval.to_string(), width, fill_at_close);
        let mount = build_paper_strategy_core_with(
            strategy,
            &spec,
            StrategyCorePaperConfig { halt, strategy_factory, ..Default::default() },
        );
        let h = &mount.handle;
        let sink = CoreLaneSink::new(h.bar_sender(), h.market_sender(), h.tick_sender());
        let seed = Bar::ohlcv(T0 - width, 99.0, 99.0, 99.0, 99.0, 1.0);
        sink.seed_bars(VENUE, SYMBOL, interval, vec![seed]);
        Node { mount, sink, heard, interval, width, _halt_root: halt_root }
    }

    /// One quote whose ask is `ask` (the bid `SPREAD` under it), once the strategy heard it.
    fn quote(&self, at: i64, ask: f64) {
        self.quote_sides(at, ask - SPREAD, ask);
    }

    fn quote_sides(&self, at: i64, bid: f64, ask: f64) {
        let q = TickQuote {
            timestamp: at,
            local_timestamp: 0,
            bid,
            ask,
            bid_size: 10.0,
            ask_size: 10.0,
            symbol: SYMBOL.into(),
        };
        self.sink.quote(VENUE, SYMBOL, q);
        match &self.heard {
            Some(_) => self.wait_heard(at),
            None => self.wait_mark((bid + ask) / 2.0),
        }
    }

    /// The 12:00 bar: its quotes as they came (100.00 at the open, 100.60 halfway, 101.00 last), then
    /// its close, once the strategy stepped it.
    fn noon_bar(&self) {
        self.quote(T0 + 100, 100.00);
        self.quote(T0 + self.width / 2 - 100, 100.60);
        self.quote(T0 + self.width - 100, 101.00);
        let bar = Bar::ohlcv(T0, 99.95, 100.95, 99.95, 100.95, 1.0);
        self.sink.close_bar(VENUE, SYMBOL, self.interval, bar);
        if self.heard.is_some() {
            self.wait_heard(T0);
        }
    }

    fn wait_heard(&self, at: i64) {
        let heard = self.heard.as_ref().expect("a scripted boot strategy");
        let stepped = || heard.lock().unwrap().contains(&at);
        assert!(wait_until(Duration::from_secs(10), stepped), "the core never stepped {at}");
    }

    /// Wait until the core marked the symbol at `mid` (a quote folded; the tick lane is in order).
    fn wait_mark(&self, mid: f64) {
        let marked = || {
            let s = self.mount.handle.snapshot();
            s.marks.iter().any(|(v, sym, px)| &**v == VENUE && &**sym == SYMBOL && *px == mid)
        };
        assert!(wait_until(Duration::from_secs(10), marked), "the core never marked {mid}");
    }

    fn manual(&self, kind: OrderType, price: Option<f64>, at: i64) {
        self.mount.handle.send_command(Command::Order(OrderIntent::Submit(Box::new(
            OrderRequest {
                order_client_id: "manual-1".into(),
                venue: VENUE.into(),
                symbol: SYMBOL.into(),
                side: 1,
                qty: 1.0,
                order_type: kind,
                price,
                timestamp: at,
                ..Default::default()
            },
        ))));
    }

    /// The fills once there are `n` (a bounded wait: a fill folds on the core's thread).
    fn fills(&self, n: usize) -> Vec<Fill> {
        let fills = || self.mount.fills.lock().unwrap().clone();
        assert!(wait_until(Duration::from_secs(10), || fills().len() >= n), "{:?}", fills());
        fills()
    }

    /// The fills now (every caller waited for what it sent to fold: `wait_heard`, `wait_resting`).
    fn fills_now(&self) -> Vec<Fill> {
        self.mount.fills.lock().unwrap().clone()
    }

    /// Wait until the manual order rests in the published book, so a quote sent next meets it.
    fn wait_resting(&self) {
        let rests = || !self.mount.handle.snapshot().orders.is_empty();
        assert!(wait_until(Duration::from_secs(10), rests), "the manual order never rested");
    }

    fn end(self) {
        let cell = self.mount.handle.snapshot_cell();
        self.mount.handle.shutdown_and_join();
        assert!(cell.load_full().fault.is_none(), "the core faulted");
    }
}

fn price_at(f: &Fill) -> (f64, i64) {
    (f.price, f.timestamp)
}

/// Example 1, `next_open`: the strategy's market buy on the 12:00 close fills at 101.20, REPORTED as
/// the next bar's first quote comes (12:01:00.3, on 1h 13:00:00.3), with no later bar sent at all.
#[test]
fn a_strategy_order_fills_at_the_next_bars_first_price_the_moment_it_comes() {
    vike_log::test_init();
    for (interval, width) in WIDTHS {
        let node = Node::new(Plan::BuyMarket, interval, width, false);
        node.noon_bar();
        assert!(node.fills_now().is_empty(), "{interval}: nothing at the 12:00 close");
        node.quote(T0 + width + 300, 101.20);
        let fills = node.fills(1);
        assert_eq!(fills.iter().map(price_at).collect::<Vec<_>>(), [(101.20, T0 + width + 300)]);
        node.end();
    }
}

/// Example 1, `close`: the same order of a file declaring `fill = "close"` fills at the 12:00 close,
/// 101.00, at 12:01:00 (on 1h 13:00:00), the moment the bar closes, against `next_open`'s 101.20.
#[test]
fn a_close_files_strategy_order_fills_at_that_bars_close_against_next_open() {
    vike_log::test_init();
    for (interval, width) in WIDTHS {
        let close = Node::new(Plan::BuyMarket, interval, width, true);
        close.noon_bar();
        assert_eq!(
            close.fills(1).iter().map(price_at).collect::<Vec<_>>(),
            [(101.00, T0 + width)],
            "{interval}: the close"
        );
        close.quote(T0 + width + 300, 101.20);
        assert_eq!(close.fills_now().len(), 1, "{interval}: filled once");
        close.end();
    }
}

/// Example 2: a strategy's stop-loss at 99.50 under its long (the core's emulated stop) SELLS at 99.50
/// the moment the bid reaches it, 20 s into the next bar, not two bars later at an open.
#[test]
fn a_strategy_stop_loss_sells_the_moment_the_bid_reaches_it() {
    vike_log::test_init();
    for (interval, width) in WIDTHS {
        let node = Node::new(Plan::BuyWithStopLoss, interval, width, false);
        node.noon_bar();
        let next = T0 + width;
        node.quote(next + 300, 101.20);
        assert_eq!(node.fills(1)[0].price, 101.20, "{interval}: the long");
        node.quote_sides(next + 10_000, 99.80, 99.90);
        assert_eq!(node.fills_now().len(), 1, "{interval}: the bid above the stop");
        node.quote_sides(next + 20_000, 99.50, 99.60);
        let fills = node.fills(2);
        assert_eq!(
            (fills[1].side, price_at(&fills[1])),
            (-1, (99.50, next + 20_000)),
            "{interval}"
        );
        node.end();
    }
}

/// Example 3: a manual market buy at 12:00:30, the ask 100.60 then, fills at 100.60 at 12:00:30 (on 1h,
/// halfway through the bar), never at the 12:00 open, a price from before the order.
#[test]
fn a_manual_market_buy_fills_at_the_standing_ask_at_once() {
    vike_log::test_init();
    for (interval, width) in WIDTHS {
        let node = Node::new(Plan::Nothing, interval, width, false);
        let at = T0 + width / 2;
        node.quote(T0 + 100, 100.00);
        node.quote(at - 100, 100.60);
        node.manual(OrderType::Market, None, at);
        assert_eq!(node.fills(1).iter().map(price_at).collect::<Vec<_>>(), [(100.60, at)]);
        node.end();
    }
}

/// Example 4: a manual buy limit at 100.20, placed with the ask at 100.60, fills at 100.20 when the ask
/// touches it (12:00:45; on 1h three quarters into the bar). The ask under it before the order is no
/// fill, and neither is the 12:00 bar whose path holds it.
#[test]
fn a_manual_limit_fills_when_the_ask_touches_it() {
    vike_log::test_init();
    for (interval, width) in WIDTHS {
        let node = Node::new(Plan::Nothing, interval, width, false);
        let at = T0 + width / 2;
        node.quote(T0 + 100, 100.00);
        node.quote(T0 + width / 6, 100.10);
        node.quote(at - 100, 100.60);
        node.manual(OrderType::Limit, Some(100.20), at);
        node.wait_resting();
        assert!(node.fills_now().is_empty(), "{interval}: the ask is above it");
        let touch = T0 + width * 3 / 4;
        node.quote(touch, 100.20);
        assert_eq!(node.fills(1).iter().map(price_at).collect::<Vec<_>>(), [(100.20, touch)]);
        node.end();
    }
}

/// A strategy mounted while the node RUNS (`Command::MountStrategy`, its file resolved by the strategy
/// factory) whose file declares `fill = "close"` fills its bar order at the 12:00 close, at the bar's end,
/// exactly as one present at boot (`vike_core::MountStrategyFile::fill_at_close`).
#[test]
fn a_run_time_mount_of_a_close_file_fills_at_that_bars_close() {
    vike_log::test_init();
    for (interval, width) in WIDTHS {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let its = Arc::clone(&heard);
        let factory: vike_core::StrategyFactory = Box::new(move |_: &MountSpec| {
            let strategy = Scripted { plan: Plan::BuyMarket, heard: Arc::clone(&its) };
            let file = vike_journal::JournalStrategyFile {
                path: "trend/closes.rhai".into(),
                sha256: "00".into(),
                sdk: None,
            };
            let file = vike_core::MountStrategyFile { file, fill_at_close: true };
            Ok((Box::new(strategy) as Box<dyn Strategy<LiveBroker> + Send>, Some(file)))
        });
        let boot = Box::new(Scripted { plan: Plan::Nothing, heard: Arc::clone(&heard) });
        let node = Node::with(boot, Some(heard), (interval, width), false, Some(factory));
        node.mount.handle.send_command(Command::MountStrategy(Box::new(MountSpec {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: interval.into(),
            account: None,
            controller_id: Some("runtime_close".into()),
            file: Some("trend/closes.rhai".into()),
            name: None,
            rhai: None,
            params: vike_model::params::Params::new(),
            order_client_id_code: None,
        })));
        let mounted =
            || node.mount.handle.snapshot().mounts.iter().any(|m| m.mount_id == "runtime_close");
        assert!(
            wait_until(Duration::from_secs(10), mounted),
            "{interval}: never mounted: {:?}",
            node.mount.handle.snapshot().recent_events
        );
        node.noon_bar();
        assert_eq!(
            node.fills(1).iter().map(price_at).collect::<Vec<_>>(),
            [(101.00, T0 + width)],
            "{interval}: the close, as a boot mount's"
        );
        node.end();
    }
}

/// The sandboxed `.rs` child: a market buy on the 12:00 close.
/// It answers that bar only once the test opens `gate` (or a while passes), so the test can fold a quote of
/// the next bar into the core first: the gap a sandboxed reply always has.
struct BuysAtNoon {
    gate: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl Strategy<ViewBroker> for BuysAtNoon {
    fn on_bar(&mut self, b: &mut ViewBroker, bar: &Bar) {
        if bar.timestamp == T0 {
            // within the supervisor's hook deadline, or the child is killed as hung
            let _ = self.gate.lock().unwrap().recv_timeout(Duration::from_millis(800));
            b.place("", "", 1, 1.0, OrderPlacement::Market, false);
        }
    }
}

/// The child half of a sandboxed mount on a thread: the real proxy, supervisor and protocol, no process.
struct ThreadChild;

impl SandboxChild for ThreadChild {
    fn kill(&mut self) {}
    fn wait(&mut self) -> String {
        "a test thread".to_string()
    }
}

/// A `.rs` strategy file's mount as the tradehub opens one, its child [`serve`] on a thread over two pipes.
fn sandboxed(interval: &str) -> (RemoteStrategy, RemoteHandle, std::sync::mpsc::Sender<()>) {
    let (open, gate) = std::sync::mpsc::channel();
    let gate = Arc::new(std::sync::Mutex::new(gate));
    let launcher: Launcher = Box::new(move || {
        let (child_in, to_child) = std::io::pipe()?;
        let (from_child, child_out) = std::io::pipe()?;
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || {
            let _ = serve(child_in, child_out, BUILD, |_| Ok((BuysAtNoon { gate }, None)));
        });
        Ok(Launched {
            child: Box::new(ThreadChild),
            to_child: Box::new(to_child),
            from_child: Box::new(from_child),
            reasons: None,
        })
    });
    let spec = RemoteSpec {
        file: "trend/buys_at_noon.rs".into(),
        sha256: "00".into(),
        params: String::new(),
        facts: vike_model::params::MountFacts {
            symbol: SYMBOL.into(),
            venue: VENUE.into(),
            interval: interval.into(),
            ..Default::default()
        },
        controller_id: None,
        build: BUILD.into(),
    };
    let (proxy, remote) =
        RemoteStrategy::open(spec, launcher).expect("the sandboxed strategy opens");
    (proxy, remote, open)
}

/// A strategy that buys on the first bar it is stepped on.
struct BuysOnItsFirstBar(bool);

impl Strategy<LiveBroker> for BuysOnItsFirstBar {
    fn on_bar(&mut self, b: &mut LiveBroker, _bar: &Bar) {
        if !std::mem::replace(&mut self.0, true) {
            b.place("", "", 1, 1.0, OrderPlacement::Market, false);
        }
    }
}

/// The two bar stamps a close fill meets end to end (review FIX, finding 3): a venue candle through the
/// node's `CoreLaneSink` is stamped at its window's START (the examples above), and a bar the tick
/// synthesizer folds from quotes (`TickSink`: the cTrader and Polymarket mounts) at its END. The close
/// fill lands at the bar's end on both: here the [12:00, 12:01) window, stamped 12:01, fills at 12:01
/// (13:00 on 1h), not a bar later.
#[test]
fn a_close_fill_on_a_synthesized_bar_is_stamped_at_the_bars_end() {
    vike_log::test_init();
    for (interval, width) in WIDTHS {
        let (_halt_root, halt) = common::no_halt(HALT_ROOT_PREFIX);
        let mut spec = MountPlacement::new(VENUE, SYMBOL, 1_000_000.0, 0.01);
        (spec.interval, spec.interval_ms, spec.fill_at_close) = (interval.to_string(), width, true);
        let mount = build_paper_strategy_core_with(
            Box::new(BuysOnItsFirstBar(false)),
            &spec,
            StrategyCorePaperConfig { halt, ..Default::default() },
        );
        let sink = vike_tradehub_core::TickSink::new(&mount.handle, VENUE, SYMBOL, interval, width);
        let quote = |at: i64, ask: f64| {
            let q = TickQuote {
                timestamp: at,
                local_timestamp: 0,
                bid: ask - SPREAD,
                ask,
                bid_size: 10.0,
                ask_size: 10.0,
                symbol: SYMBOL.into(),
            };
            sink.quote(VENUE, SYMBOL, q);
        };
        quote(T0 + 100, 100.00);
        quote(T0 + width / 2, 100.60);
        quote(T0 + width - 100, 101.00);
        // the next window's first quote closes [12:00, 12:01), stamped 12:01, and the strategy buys on it
        quote(T0 + width + 300, 101.20);
        let fills = || mount.fills.lock().unwrap().clone();
        assert!(wait_until(Duration::from_secs(10), || !fills().is_empty()), "{interval}: no fill");
        assert_eq!(
            fills().iter().map(price_at).collect::<Vec<_>>(),
            [(101.00, T0 + width)],
            "{interval}: the window's close, at its end"
        );
        let cell = mount.handle.snapshot_cell();
        mount.handle.shutdown_and_join();
        assert!(cell.load_full().fault.is_none(), "{interval}: the core faulted");
    }
}

/// A `.rs` file runs in a sandboxed process, so its bar order reaches the core LATER, in the reply to that
/// bar (`vike_execution::MountReply::on_bar`), and a quote of the next bar can come first: here the ask
/// 101.20, 920 ms into it. Its file declaring `fill = "close"`, the order still fills at the 12:00 close,
/// 101.00, at the bar's end, never at that quote (review FIX, finding 2).
#[test]
fn a_sandboxed_close_files_bar_order_fills_at_that_bars_close_past_a_later_quote() {
    vike_log::test_init();
    for (interval, width) in WIDTHS {
        let (proxy, remote, open) = sandboxed(interval);
        let node = Node::with(Box::new(proxy), None, (interval, width), true, None);
        remote.attach(node.mount.handle.event_sender());
        node.noon_bar();
        // the core folded the 12:00 bar (the child now waits at the gate), then a quote of the next bar
        let key = (VENUE.to_string(), SYMBOL.to_string(), interval.to_string());
        let closed = || {
            let s = node.mount.handle.snapshot();
            s.bars.get(&key).is_some_and(|b| b.closed.iter().any(|bar| bar.timestamp == T0))
        };
        assert!(wait_until(Duration::from_secs(10), closed), "{interval}: the bar never folded");
        node.quote(T0 + width + 920, 101.20);
        open.send(()).expect("the child waits at its gate");
        assert_eq!(
            node.fills(1).iter().map(price_at).collect::<Vec<_>>(),
            [(101.00, T0 + width)],
            "{interval}: the close, not the later quote"
        );
        node.end();
        drop(remote);
    }
}
