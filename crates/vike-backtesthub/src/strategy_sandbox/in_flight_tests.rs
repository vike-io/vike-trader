//! **Several history reads in flight** between a sandboxed child and its hub (decision 0281's amendment of
//! 2026-11-19): a whole run through a child's `PipeHistStore` and this side's relay answers what the run in this
//! process answers, its refusal included when two reads fail in either order; a slow read does not hold back the
//! reads asked after it; and reads in flight over full pipes, both ways, do not deadlock.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use vike_backtesthub_client::proto::{Request, Response};
use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, PlannedRead, TimestampRange};
use vike_hub_protocol::frame::read_frame_raw;
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, Strategy};
use vike_simulator::OrderBrokerSimulator;

use super::pipe_store::PipeHistStore;
use super::relay;
use vike_hub_protocol::child_pipe::{check_build, read_message, send_build, write_message};

use super::wire::{
    ChildFrame, ChildRequest, ReadAnswerFrame, StoreRead, is_columns_part, push_bar_part, row_count,
};
use super::{loaded_strategy_files, serve_child};
use crate::backtesthub_server::StudyDirs;
use crate::backtesthub_server::dispatch::answer_here;
use crate::named_run::NamedRunLane;
use crate::studio_run::StrategyFileBuild;

/// The build both ends state here (the binary hands in `vike_buildinfo::summary`; a library links none).
const BUILD: &str = "in_flight_tests";
const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// A UTC midnight, where the run's window opens.
const S: i64 = 20_000 * DAY;
/// How many counted milliseconds a held read waits for the read it waits on (this crate reads no wall clock).
const PATIENCE_MS: u32 = 10_000;

/// The file the run names, and the text the hub read for it: [`DayFollower`]'s header.
const TEXT: &str = "//! Day follower: long under the last daily close, short over it.\n//!\n//! ```toml\n\
                    //! [strategy]\n//! runs_on = \"backtest\"\n//! ```\n";

/// The strategy the file stands for, compiled in: it reads BTCUSDT's daily bars (an input with its whole history,
/// named `input`: `1d` in a run of that one symbol, in full in a run of several), and every 45th minute turns long
/// while the minute closes under the last closed day's close, else short.
struct DayFollower {
    input: &'static str,
    seen: usize,
    long: Option<bool>,
}

impl Strategy<OrderBrokerSimulator> for DayFollower {
    fn inputs(&self) -> Vec<String> {
        vec![self.input.to_string()]
    }
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.seen += 1;
        let Some(day) = broker.input(self.input).map(|day| day.close) else { return };
        let long = bar.close < day;
        if self.seen.is_multiple_of(45) && self.long != Some(long) {
            let qty = if self.long.is_some() { 2.0 } else { 1.0 };
            broker.place(
                "BTCUSDT",
                "",
                if long { 1 } else { -1 },
                qty,
                OrderPlacement::Market,
                false,
            );
            self.long = Some(long);
        }
    }
}

/// The stand-in a child (or this process) loads in place of the built artifact, its input named `input`.
fn the_file(input: &'static str) -> StrategyFileBuild {
    Box::new(move |_params_toml: &str, _facts: &str| {
        Ok(Box::new(DayFollower { input, seen: 0, long: None })
            as Box<dyn Strategy<OrderBrokerSimulator>>)
    })
}

/// Two days of binance BTCUSDT minutes from [`S`] for [`DayFollower`], whose USDT is booked on the dollar ledger at
/// the rate the run finds by itself (`ListSeries`, then the pair).
fn profile() -> String {
    format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"{S}\"\nto = \"{}\"\n\n[engine]\ncash = 100000.0\nattach_funding = false\n\n\
         [strategy]\nfile = \"other/day_follower.rs\"\n",
        S + 2 * DAY - 1
    )
}

/// `n` bars `step` apart from `start`, their closes walking a fixed pattern.
fn bars(start: i64, step: i64, n: i64) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let close = 100.0 + ((i * 37) % 101) as f64 / 10.0 - ((i * 11) % 53) as f64 / 5.0;
            Bar::ohlcv(
                start + i * step,
                close,
                close + 0.5,
                close - 0.5,
                close,
                1.0 + (i % 7) as f64,
            )
        })
        .collect()
}

/// The run's minutes, forty-two days of its daily bars before and through the window, and a user's USDT-USD lane.
fn planted() -> HistoryStoreMemory {
    let store = HistoryStoreMemory::new();
    store.append_bars("binance", "BTCUSDT", "1m", &bars(S, MINUTE, 2 * 1440), None).expect("1m");
    store.append_bars("binance", "BTCUSDT", "1d", &bars(S - 40 * DAY, DAY, 42), None).expect("1d");
    let rates: Vec<Bar> = (-2..3)
        .map(|d| {
            let rate = 1.0 + d as f64 / 1000.0;
            Bar::ohlcv(S + d * DAY, rate, rate, rate, rate, 1.0)
        })
        .collect();
    store
        .append_bars_sourced("coinbase", "user-coinbase", "USDTUSD", "1d", &rates, None)
        .expect("rates");
    store
}

/// The hub's store: `inner`'s answers, each fetch logged (`SYMBOL:INTERVAL`, or `list`) in the order it STARTED. A
/// read `held.0` names waits (counted) until one `held.1` names has started, `released` saying it did: a relay that
/// answers one read before it fetches the next can never start `held.1`'s fetch while `held.0`'s answer is unwritten.
/// A read `refused` names is refused after its delay in ms; a read `panics` names panics. `oversized_splits` answers
/// the kept rows with a split no frame can carry (its symbol over `MAX_FRAME_LEN`).
struct Parent {
    inner: HistoryStoreMemory,
    held: Option<(&'static str, &'static str)>,
    refused: Vec<(&'static str, u64)>,
    panics: Option<&'static str>,
    oversized_splits: bool,
    started: Mutex<Vec<String>>,
    released: AtomicBool,
}

impl Parent {
    fn new(inner: HistoryStoreMemory) -> Self {
        Self {
            inner,
            held: None,
            refused: Vec::new(),
            panics: None,
            oversized_splits: false,
            started: Mutex::default(),
            released: AtomicBool::new(false),
        }
    }

    /// How many fetches of `label` started.
    fn fetches(&self, label: &str) -> usize {
        self.started.lock().expect("log").iter().filter(|s| *s == label).count()
    }

    fn fetch<T>(
        &self,
        label: String,
        read: impl FnOnce() -> Result<T, HistoryStoreError>,
    ) -> Result<T, HistoryStoreError> {
        self.started.lock().expect("log").push(label.clone());
        assert!(self.panics != Some(label.as_str()), "a planted panic of {label}");
        if let Some((held, until)) = self.held
            && held == label
        {
            for _ in 0..PATIENCE_MS {
                if self.fetches(until) > 0 {
                    self.released.store(true, Ordering::SeqCst);
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        match self.refused.iter().find(|(named, _)| *named == label) {
            Some((_, ms)) => {
                thread::sleep(Duration::from_millis(*ms));
                Err(HistoryStoreError::Query(format!("the planted refusal of {label}")))
            }
            None => read(),
        }
    }
}

impl HistoryStore for Parent {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.fetch(format!("{symbol}:{interval}"), || {
            self.inner.load_bars(venue, symbol, interval, range)
        })
    }
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.fetch(format!("{symbol}:{interval}"), || {
            self.inner.load_bars_sourced_capped(venue, source, symbol, interval, range, budget)
        })
    }
    fn list_series(&self) -> Result<Vec<vike_data::SeriesId>, HistoryStoreError> {
        self.fetch("list".to_string(), || self.inner.list_series())
    }
    fn stock_splits(&self) -> Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError> {
        let split = || vike_model::StockSplit {
            venue: "binance".into(),
            symbol: "X".repeat(65 << 20),
            date: "2020-01-02".into(),
            ratio: 2.0,
        };
        self.fetch("splits".to_string(), || Ok(self.oversized_splits.then(|| vec![split()])))
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// `body` on a thread of its own, failed as a DEADLOCK when it has not finished within `seconds`.
fn within(seconds: u64, body: impl FnOnce() + Send + 'static) {
    let (finished, done) = mpsc::channel();
    let worker = thread::spawn(move || {
        body();
        let _ = finished.send(());
    });
    match done.recv_timeout(Duration::from_secs(seconds)) {
        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("deadlocked: not done within {seconds} s"),
    }
}

/// `request` answered by THIS process over `store` (the dispatch a child answers with, the same stand-in loaded, its
/// input named `input`): the reads one after another, as before reads could be in flight.
fn in_this_process(
    input: &'static str,
    request: Request,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Response {
    let study = StudyDirs {
        strategies: loaded_strategy_files(
            &request,
            vec![(Ok(the_file(input)), None)],
            vec![(TEXT.into(), BTreeMap::new())],
        ),
        ..StudyDirs::working_directory()
    };
    answer_here(request, &store, &study, NamedRunLane::DISARMED, None).0
}

/// `request` answered by a child (`serve_child` over two OS pipes, on a thread of its own, [`the_file`] loaded, its
/// input named `input`), its reads answered from `parent` by this side's relay.
fn through_a_child(input: &'static str, request: &Request, parent: &Arc<Parent>) -> Response {
    let (mut from_child, child_out) = std::io::pipe().expect("pipe up");
    let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
    let child = thread::spawn(move || {
        let built = vec![(Ok(the_file(input)), None)];
        let _ = serve_child(Box::new(child_in), Box::new(child_out), BUILD, built);
    });
    let store: &(dyn HistoryStore + Send + Sync) = parent.as_ref();
    let relayed = relay::relay(
        &mut to_child,
        &mut from_child,
        BUILD,
        request,
        TEXT,
        &BTreeMap::new(),
        store,
        &|| {},
        None,
    );
    drop(to_child);
    child.join().expect("the child's half");
    relayed.unwrap_or_else(|stopped| panic!("the relay stopped: {}", stopped.why)).response
}

/// A stand-in child's opening, as `serve_child` opens: its build out, the hub's checked, the start message read.
fn started(child_in: &mut impl std::io::Read, child_out: &mut impl std::io::Write) {
    send_build(child_out, BUILD).expect("its build");
    check_build(child_in, BUILD, "the hub").expect("the hub's build");
    let _: ChildRequest<Request, String, BTreeMap<String, String>> =
        read_message(child_in).expect("the start message");
}

/// A child's `PipeHistStore` over two OS pipes of the default size, its start message read as `serve_child` reads
/// it, and this side's relay answering it from `parent` on a thread of its own until the child's answer.
fn child_over_pipes(
    parent: &Arc<Parent>,
) -> (PipeHistStore, thread::JoinHandle<Result<Response, String>>) {
    let (mut from_child, mut child_out) = std::io::pipe().expect("pipe up");
    let (mut child_in, mut to_child) = std::io::pipe().expect("pipe down");
    let parent = Arc::clone(parent);
    let hub = thread::spawn(move || {
        let store: &(dyn HistoryStore + Send + Sync) = parent.as_ref();
        relay::relay(
            &mut to_child,
            &mut from_child,
            BUILD,
            &Request::Ping,
            "",
            &BTreeMap::new(),
            store,
            &|| {},
            None,
        )
        .map(|answered| answered.response)
        .map_err(|stopped| stopped.why)
    });
    started(&mut child_in, &mut child_out);
    (PipeHistStore::new(Box::new(child_in), Box::new(child_out)), hub)
}

/// A planned bar read of binance `symbol` at `interval`, every bar of it.
fn whole(symbol: &str, interval: &str) -> PlannedRead {
    PlannedRead::Bars {
        venue: "binance".into(),
        symbol: symbol.into(),
        interval: interval.into(),
        range: TimestampRange::all(),
    }
}

/// An answer's text, hashed: the trades, the report and every number in it.
fn digest(response: &Response) -> u64 {
    let mut digest = DefaultHasher::new();
    format!("{response:?}").hash(&mut digest);
    digest.finish()
}

/// **The same result, bit for bit**: a run whose reads are in flight together (the 1d input held at the hub until
/// the price bars' fetch has started) answers the digest the run in this process answers with its reads one after
/// another, and every read crossed ONCE (a verb takes the answer its prefetch asked for).
#[test]
fn a_run_with_its_reads_in_flight_answers_what_this_process_answers() {
    within(300, || {
        let here = in_this_process(
            "1d",
            Request::RunBacktest(profile()),
            Arc::new(Parent::new(planted())),
        );
        assert!(matches!(&here, Response::Report(_)), "the reference run must run: {here:?}");
        let parent =
            Arc::new(Parent { held: Some(("BTCUSDT:1d", "BTCUSDT:1m")), ..Parent::new(planted()) });
        let piped = through_a_child("1d", &Request::RunBacktest(profile()), &parent);
        assert_eq!(digest(&piped), digest(&here), "the same answer: {piped:?}");
        assert!(
            parent.released.load(Ordering::SeqCst),
            "the price bars must be fetched before the input's answer is written"
        );
        for label in ["BTCUSDT:1d", "list", "USDTUSD:1d", "BTCUSDT:1m"] {
            assert_eq!(parent.fetches(label), 1, "{label} crosses once");
        }
    });
}

/// **A multi-symbol run answers what this process answers**: six symbols, each with its minutes, and BTCUSDT's daily
/// input named in full, so the child names more reads ahead than the hub fetches at once. The digest is the run in
/// this process's, and every series crossed once.
#[test]
fn a_multi_symbol_run_answers_what_this_process_answers() {
    within(300, || {
        let symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "ADAUSDT", "BNBUSDT"];
        let store = || {
            let store = planted();
            for symbol in &symbols[1..] {
                store
                    .append_bars("binance", symbol, "1m", &bars(S, MINUTE, 2 * 1440), None)
                    .expect("1m");
            }
            store
        };
        let profile =
            profile().replace("symbols = [\"BTCUSDT\"]", &format!("symbols = {symbols:?}"));
        let input = "binance:BTCUSDT:1d";
        let request = Request::RunBacktest(profile);
        let here = in_this_process(input, request.clone(), Arc::new(Parent::new(store())));
        assert!(matches!(&here, Response::Report(_)), "the reference run must run: {here:?}");
        let parent = Arc::new(Parent::new(store()));
        let piped = through_a_child(input, &request, &parent);
        println!(
            "multi-symbol run digest: in this process {:016x}, through a child {:016x}",
            digest(&here),
            digest(&piped)
        );
        assert_eq!(digest(&piped), digest(&here), "the same answer: {piped:?}");
        for symbol in symbols {
            assert_eq!(parent.fetches(&format!("{symbol}:1m")), 1, "{symbol} crosses once");
        }
    });
}

/// **Two reads fail, in either order of time**: the input and the price bars, each refused by the hub's store, one at
/// once and the other later. The run refuses as the run in this process does, the INPUT's refusal (the run judges
/// its reads in its own order whatever order the answers were ready in), and the failed input was cached by nobody:
/// its verb asked it again after the prefetch's answer failed. The price read went ahead and is never the run's
/// refusal.
#[test]
fn two_failed_reads_refuse_the_run_as_this_process_does_whichever_fails_first() {
    within(300, || {
        for (input_ms, price_ms) in [(0, 80), (80, 0)] {
            let failing = || Parent {
                refused: vec![("BTCUSDT:1d", input_ms), ("BTCUSDT:1m", price_ms)],
                ..Parent::new(planted())
            };
            let here = in_this_process("1d", Request::RunBacktest(profile()), Arc::new(failing()));
            assert!(
                matches!(&here, Response::Error(why) if why.contains("the input \"1d\"")),
                "{here:?}"
            );
            let parent = Arc::new(failing());
            let piped = through_a_child("1d", &Request::RunBacktest(profile()), &parent);
            assert_eq!(
                format!("{piped:?}"),
                format!("{here:?}"),
                "input refused after {input_ms} ms, price after {price_ms} ms"
            );
            assert_eq!(parent.fetches("BTCUSDT:1d"), 2, "a failed read is cached by nobody");
            assert_eq!(parent.fetches("BTCUSDT:1m"), 1, "the price read went ahead");
        }
    });
}

/// **A store read that PANICS refuses the run in words, it never hangs it**: the hub's fetch of the input panics on
/// its read thread, that read is answered as a refusal in its turn (the reads asked after it still go down), and the
/// run refuses as a failed input refuses it, naming the panic: once for the prefetch's read and once for its verb's.
/// Unwound instead, every later answer would wait for the panicked one's turn, and the child and the relay on them.
#[test]
fn a_store_read_that_panics_refuses_the_run_in_words() {
    within(120, || {
        let parent = Arc::new(Parent { panics: Some("BTCUSDT:1d"), ..Parent::new(planted()) });
        let piped = through_a_child("1d", &Request::RunBacktest(profile()), &parent);
        assert!(
            matches!(&piped, Response::Error(why) if why.contains("the input \"1d\"")
                && why.contains("panicked answering this read: a planted panic of BTCUSDT:1d")),
            "{piped:?}"
        );
        assert_eq!(parent.fetches("BTCUSDT:1d"), 2, "each panic was its read's refusal alone");
    });
}

/// **A slow first read does not hold back the reads asked after it**: the hub's store holds the day read until the
/// minute read has STARTED, which it can only do if the hub fetches the reads in flight at once. The answers still
/// come in request order, each the store's own.
#[test]
fn a_slow_read_does_not_hold_back_the_reads_asked_after_it() {
    within(120, || {
        let parent =
            Arc::new(Parent { held: Some(("BTCUSDT:1d", "BTCUSDT:1m")), ..Parent::new(planted()) });
        let (pipe, hub) = child_over_pipes(&parent);
        pipe.prefetch(&[whole("BTCUSDT", "1d"), whole("BTCUSDT", "1m")]);
        let all = TimestampRange::all();
        let days = pipe.load_bars("binance", "BTCUSDT", "1d", all).expect("the days");
        assert!(
            parent.released.load(Ordering::SeqCst),
            "the minutes were fetched while the days were held"
        );
        assert_eq!(days, parent.inner.load_bars("binance", "BTCUSDT", "1d", all).expect("store"));
        let minutes = pipe.load_bars("binance", "BTCUSDT", "1m", all).expect("the minutes");
        assert_eq!(
            minutes,
            parent.inner.load_bars("binance", "BTCUSDT", "1m", all).expect("store")
        );
        assert_eq!((parent.fetches("BTCUSDT:1d"), parent.fetches("BTCUSDT:1m")), (1, 1));
        pipe.send_answer(Response::Error("the end".into())).expect("the answer");
        assert!(matches!(hub.join().expect("the hub"), Ok(Response::Error(_))));
    });
}

/// **Reads in flight, taken in any order, over pipes of the default size**: five reads named ahead (one answer
/// 12 MB, far over the pipe down; one the hub refuses), then the verbs in another order with a read nobody named
/// in between. Each answer is the store's own, each read crossed once but the failed one (cached by nobody, so its
/// verb asked again and failed in the store's words), and the child's answer goes up once every prefetched answer
/// was read.
#[test]
fn reads_in_flight_are_taken_in_any_order_and_each_crosses_once() {
    within(300, || {
        let store = planted();
        store
            .append_bars("binance", "ETHUSDT", "1m", &bars(S, MINUTE, 120_000), None)
            .expect("eth");
        store
            .append_bars("binance", "BTCUSDT", "5m", &bars(S, 5 * MINUTE, 30_000), None)
            .expect("5m");
        let parent = Arc::new(Parent { refused: vec![("BTCUSDT:4h", 0)], ..Parent::new(store) });
        let (pipe, hub) = child_over_pipes(&parent);
        pipe.prefetch(&[
            whole("ETHUSDT", "1m"),
            whole("BTCUSDT", "4h"),
            whole("BTCUSDT", "5m"),
            PlannedRead::ListSeries,
            whole("BTCUSDT", "1d"),
        ]);
        let all = TimestampRange::all();
        let own = |symbol: &str, interval: &str| {
            parent.inner.load_bars("binance", symbol, interval, all).expect("the store's own")
        };
        for (symbol, interval) in
            [("BTCUSDT", "1d"), ("BTCUSDT", "1m"), ("BTCUSDT", "5m"), ("ETHUSDT", "1m")]
        {
            let piped = pipe.load_bars("binance", symbol, interval, all).expect("a piped read");
            assert!(piped == own(symbol, interval), "{symbol} {interval}");
        }
        assert_eq!(pipe.list_series().expect("piped"), parent.inner.list_series().expect("store"));
        match pipe.load_bars("binance", "BTCUSDT", "4h", all) {
            Err(HistoryStoreError::Query(m)) => assert!(m.contains("planted refusal"), "{m}"),
            other => panic!("the hub's refusal must reach the verb: {other:?}"),
        }
        for (label, times) in [
            ("ETHUSDT:1m", 1),
            ("BTCUSDT:5m", 1),
            ("list", 1),
            ("BTCUSDT:1d", 1),
            ("BTCUSDT:1m", 1),
            ("BTCUSDT:4h", 2),
        ] {
            assert_eq!(parent.fetches(label), times, "{label}");
        }
        pipe.send_answer(Response::Error("the end".into())).expect("the answer");
        assert!(matches!(hub.join().expect("the hub"), Ok(Response::Error(_))));
    });
}

/// **Both pipes full at once**: a child writes a read whose answer is far over the pipe down (10 MB on 64 KiB), then
/// more reads than the pipe up holds (six of 24 KiB), before it reads a byte of any answer. The relay keeps reading
/// the child while its answers wait to go down, so neither side waits on the other, and the answers then come in
/// request order. A relay that wrote each answer before it read the next frame deadlocks here.
#[test]
fn a_child_still_writing_reads_while_the_pipe_down_is_full_is_not_deadlocked() {
    within(120, || {
        let store = HistoryStoreMemory::new();
        store
            .append_bars("binance", "BTCUSDT", "1m", &bars(S, MINUTE, 100_000), None)
            .expect("seed");
        let parent = Arc::new(Parent::new(store));
        let (mut from_child, mut child_out) = std::io::pipe().expect("pipe up");
        let (mut child_in, mut to_child) = std::io::pipe().expect("pipe down");
        let hub_store = Arc::clone(&parent);
        let hub = thread::spawn(move || {
            let store: &(dyn HistoryStore + Send + Sync) = hub_store.as_ref();
            relay::relay(
                &mut to_child,
                &mut from_child,
                BUILD,
                &Request::Ping,
                "",
                &BTreeMap::new(),
                store,
                &|| {},
                None,
            )
            .map(|answered| answered.response)
            .map_err(|stopped| stopped.why)
        });
        started(&mut child_in, &mut child_out);
        let read = |symbol: String| {
            ChildFrame::Read(StoreRead::LoadBars {
                venue: "binance".into(),
                symbol,
                interval: "1m".into(),
                range: TimestampRange::all(),
            })
        };
        write_message(&mut child_out, &read("BTCUSDT".into())).expect("the big read");
        for n in 0..6 {
            let unheld = format!("{n}{}", "X".repeat(24 * 1024));
            write_message(&mut child_out, &read(unheld))
                .expect("a read the hub takes while its pipe down is full");
        }
        let mut answers: Vec<Vec<Bar>> = Vec::new();
        for _ in 0..7 {
            let mut rows = Vec::new();
            loop {
                let body = read_frame_raw(&mut child_in).expect("an answer frame");
                if is_columns_part(&body) {
                    push_bar_part(&body, &mut rows).expect("a bar part");
                } else if row_count(&body).is_none() {
                    let frame: ReadAnswerFrame<serde::de::IgnoredAny> =
                        rmp_serde::from_slice(&body).expect("an answer frame");
                    assert!(matches!(frame, ReadAnswerFrame::Done), "{frame:?}");
                    break;
                }
            }
            answers.push(rows);
        }
        let own =
            parent.inner.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).expect("own");
        assert!(answers[0] == own, "the big answer first, whole");
        assert!(answers[1..].iter().all(Vec::is_empty), "then the six, in request order");
        write_message(
            &mut child_out,
            &ChildFrame::Answer(Box::new(Response::Error("the end".into()))),
        )
        .expect("the answer");
        assert!(matches!(hub.join().expect("the hub"), Ok(Response::Error(_))));
    });
}

/// **A child that asks and never reads cannot run the hub's fetches away**: it writes more reads than a run may leave
/// unanswered, each answered with far more than the pipe down holds, and reads no answer. The hub fetches no more
/// than its workers do (each holds its answer until its turn, and the first answer's turn never ends), stops the
/// child past the ceiling in words that name it, drops the reads still queued, and serves the next run.
#[test]
fn a_child_that_never_reads_is_held_to_the_workers_and_refused_past_the_ceiling() {
    within(120, || {
        let store = HistoryStoreMemory::new();
        store
            .append_bars("binance", "BTCUSDT", "1m", &bars(S, MINUTE, 100_000), None)
            .expect("seed");
        let parent = Arc::new(Parent::new(store));
        let (mut from_child, mut child_out) = std::io::pipe().expect("pipe up");
        let (child_in, mut to_child) = std::io::pipe().expect("pipe down");
        // The child's read end, which the relay's stop closes, as killing a child does.
        let child_in = Arc::new(Mutex::new(Some(child_in)));
        let child = {
            let child_in = Arc::clone(&child_in);
            thread::spawn(move || {
                started(child_in.lock().expect("in").as_mut().expect("open"), &mut child_out);
                let read = ChildFrame::Read(StoreRead::LoadBars {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    interval: "1m".into(),
                    range: TimestampRange::all(),
                });
                for _ in 0..relay::MAX_UNANSWERED_READS + 100 {
                    if write_message(&mut child_out, &read).is_err() {
                        break;
                    }
                }
            })
        };
        let store: &(dyn HistoryStore + Send + Sync) = parent.as_ref();
        let stop = || drop(child_in.lock().expect("in").take());
        let stopped = relay::relay(
            &mut to_child,
            &mut from_child,
            BUILD,
            &Request::Ping,
            "",
            &BTreeMap::new(),
            store,
            &stop,
            None,
        )
        .map(|_| ())
        .expect_err("a child past the ceiling is stopped");
        let ceiling = format!("at most {} reads", relay::MAX_UNANSWERED_READS);
        assert!(stopped.why.contains(&ceiling), "{}", stopped.why);
        let fetched = parent.fetches("BTCUSDT:1m");
        assert!(
            fetched <= relay::READ_WORKERS,
            "{fetched} fetches, {} workers",
            relay::READ_WORKERS
        );
        drop(from_child);
        child.join().expect("the child");
        assert_eq!(parent.fetches("BTCUSDT:1m"), fetched, "no queued read is fetched once stopped");
        let next = through_a_child(
            "1d",
            &Request::RunBacktest(profile()),
            &Arc::new(Parent::new(planted())),
        );
        assert!(matches!(&next, Response::Report(_)), "the next run is served: {next:?}");
    });
}

/// **A prefetch batch over the pipe up completes**: a child names 300 reads ahead of 1 KiB each (far over the 64 KiB
/// pipe up), the first answered with far more than the pipe down holds. Its prefetch reads no answer until the whole
/// batch is written, so the hub keeps reading it while that answer waits to go down; each answer is the store's own.
#[test]
fn a_prefetch_batch_over_the_pipe_up_completes() {
    within(120, || {
        let store = HistoryStoreMemory::new();
        store
            .append_bars("binance", "ETHUSDT", "1m", &bars(S, MINUTE, 120_000), None)
            .expect("eth");
        let parent = Arc::new(Parent::new(store));
        let (pipe, hub) = child_over_pipes(&parent);
        let unheld: Vec<String> = (0..300).map(|n| format!("{n}{}", "X".repeat(1024))).collect();
        let mut ahead = vec![whole("ETHUSDT", "1m")];
        ahead.extend(unheld.iter().map(|symbol| whole(symbol, "1m")));
        pipe.prefetch(&ahead);
        let all = TimestampRange::all();
        let eth = pipe.load_bars("binance", "ETHUSDT", "1m", all).expect("the minutes");
        assert!(eth == parent.inner.load_bars("binance", "ETHUSDT", "1m", all).expect("own"));
        for symbol in &unheld {
            assert!(
                pipe.load_bars("binance", symbol, "1m", all).expect("an empty read").is_empty()
            );
        }
        assert_eq!(parent.fetches("ETHUSDT:1m"), 1, "each read crossed once");
        pipe.send_answer(Response::Error("the end".into())).expect("the answer");
        assert!(matches!(hub.join().expect("the hub"), Ok(Response::Error(_))));
    });
}

/// **An answer over the frame cap fails its ask in words, and the NEXT ask gets its own answer**: the hub's kept rows
/// carry a split no frame can hold, which nothing writes. The ask is refused in the cap's words and the days asked
/// next are the store's own: a turn that moved on without a `Failed` handed them to the kept ask, and every later ask
/// one answer late.
#[test]
fn an_answer_over_the_frame_cap_fails_its_ask_and_the_next_ask_gets_its_own() {
    within(120, || {
        let parent = Arc::new(Parent { oversized_splits: true, ..Parent::new(planted()) });
        let (pipe, hub) = child_over_pipes(&parent);
        let (rows, splits) = pipe.kept_instruments_and_splits();
        for refused in [rows.map(|_| ()), splits.map(|_| ())] {
            let why = refused.expect_err("the kept ask fails").to_string();
            assert!(
                why.contains("could not answer this read") && why.contains("MAX_FRAME_LEN"),
                "{why}"
            );
        }
        let all = TimestampRange::all();
        let days = pipe.load_bars("binance", "BTCUSDT", "1d", all).expect("the days");
        assert_eq!(days, parent.inner.load_bars("binance", "BTCUSDT", "1d", all).expect("own"));
        pipe.send_answer(Response::Error("the end".into())).expect("the answer");
        assert!(matches!(hub.join().expect("the hub"), Ok(Response::Error(_))));
    });
}
