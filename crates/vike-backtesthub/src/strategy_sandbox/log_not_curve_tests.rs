//! **A pinned bar run's child sends its event log, not its curve** (decision 0078's amendment of 2026-11-19 on what
//! the hub holds), through a child's `serve_child` over two OS pipes, this side's relay and the hold a hub makes of it
//! (`super::hold_from_child`, `super::run_summarized_in_child`'s): a summarized single bar run over a store that stamps
//! every day it holds, settled, comes up as ONE `ChildFrame::HeldLog` and no curve part, the hub holds the log, and
//! the curve it rebuilds from it for `Request::ReadCurve` is the one the child sent before, to the bit. Over a store that
//! stamps no day, or stamps one provisional, the child sends its curve as it always did and leaves no log beside it, so
//! the saver keeps the curve. [`measure_the_curve_up_the_pipe`] (ignored) is what it saves at 263,521 one-minute bars.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vike_backtest::run::equity_log::{EquityLog, curve_fnv};
use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::summary::CURVE_PAGE_SIZE_MAX;
use vike_data::{BarDay, HistoryStore, HistoryStoreMemory};
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, Strategy};
use vike_runs::RunEquityCurve;
use vike_simulator::OrderBrokerSimulator;
use vike_utility::errors::CarriedRefusal;

use super::{hold_from_child, loaded_strategy_files, relay, serve_child};
use crate::backtesthub_server::StudyDirs;
use crate::backtesthub_server::dispatch::answer_whole_here;
use crate::named_run::NamedRunLane;
use crate::run_summary::{
    CurveRebuild, HeldLog, Summary, curve_page, held_curve, held_log, hold_summary,
};
use crate::studio_run::StrategyFileBuild;
use crate::test_support::{DayStamps, stamping};

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// A UTC midnight, where the run's window opens.
const S: i64 = 20_000 * DAY;
/// The hub's build, as the saver records it.
const BUILD: &str = "aaaaaaa";
/// Two days of minutes, a flip every 45.
const MINUTES: i64 = 2 * 1440;
const EVERY: usize = 45;

/// The file the run names, and the text the hub read for it: [`Pulse`]'s header.
const TEXT: &str = "//! Pulse: flips side every few hundred bars.\n//!\n//! ```toml\n//! [strategy]\n\
                    //! runs_on = \"backtest\"\n//! ```\n";

/// The strategy the file stands for, compiled in: a market order on `symbol` every `every` bars, each the other side,
/// so the run closes a round trip every second order.
struct Pulse {
    symbol: &'static str,
    every: usize,
    seen: usize,
    long: bool,
}

impl Strategy<OrderBrokerSimulator> for Pulse {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, _bar: &Bar) {
        self.seen += 1;
        if self.seen.is_multiple_of(self.every) {
            let side = if self.long { -1 } else { 1 };
            broker.place(self.symbol, "", side, 1.0, OrderPlacement::Market, false);
            self.long = !self.long;
        }
    }
}

/// One mount's file: the [`Pulse`] on a symbol, flipping every so many bars.
type Pulsed = (&'static str, usize);

/// The one file of a `[strategy]` run here.
const BTC: Pulsed = ("BTCUSDT", EVERY);

/// The stand-in a child (or this process) loads in place of the built artifact of each mount (`pulses`, in mount
/// order).
fn the_files(pulses: &[Pulsed]) -> Vec<(Result<StrategyFileBuild, CarriedRefusal>, Option<u32>)> {
    let file = |(symbol, every): Pulsed| -> StrategyFileBuild {
        Box::new(move |_params_toml: &str, _facts: &str| {
            Ok(Box::new(Pulse { symbol, every, seen: 0, long: false })
                as Box<dyn Strategy<OrderBrokerSimulator>>)
        })
    };
    pulses.iter().map(|&pulsed| (Ok(file(pulsed)), None)).collect()
}

/// The hub's store: [`planted`]'s `n` minutes, each UTC day of them stamped as `days` says.
fn store_of(n: i64, days: DayStamps) -> Arc<dyn HistoryStore + Send + Sync> {
    stamping(Arc::new(planted(n)), days)
}

/// `n` binance BTCUSDT minutes from [`S`], their closes walking a fixed pattern, and a user's daily USDT-USD lane
/// around them, whose rate the run books its USDT at (its revalues are in the log).
fn planted(n: i64) -> HistoryStoreMemory {
    let store = HistoryStoreMemory::new();
    let minutes: Vec<Bar> = (0..n)
        .map(|i| {
            let close = 100.0 + ((i * 37) % 101) as f64 / 10.0 - ((i * 11) % 53) as f64 / 5.0;
            Bar::ohlcv(S + i * MINUTE, close, close + 0.5, close - 0.5, close, 1.0 + (i % 7) as f64)
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &minutes, None).expect("1m");
    let rates: Vec<Bar> = (-2..=n * MINUTE / DAY + 2)
        .map(|d| {
            let rate = 1.0 + (d % 5) as f64 / 1000.0;
            Bar::ohlcv(S + d * DAY, rate, rate, rate, rate, 1.0)
        })
        .collect();
    store
        .append_bars_sourced("coinbase", "user-coinbase", "USDTUSD", "1d", &rates, None)
        .expect("rates");
    store
}

/// A summarized profile run (`vike-cli backtest run`'s default) of `n` minutes from [`S`], naming the file.
fn summarized(n: i64) -> Request {
    let profile = format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"{S}\"\nto = \"{}\"\n\n[engine]\ncash = 100000.0\nattach_funding = false\n\n\
         [strategy]\nfile = \"other/pulse.rs\"\n",
        S + n * MINUTE - 1
    );
    Request::Summarized(Box::new(Request::RunBacktest(profile)))
}

/// One frame the child wrote up: its `ChildFrame` variant (a read by the read it asks: `Read BarDays` is the pin
/// check), its length with the prefix, and when it was written.
#[derive(Debug)]
struct Sent {
    kind: String,
    bytes: usize,
    at: Instant,
}

/// The variant a frame `body` names: `rmp_serde`'s named encoding writes an enum as a one-entry map from the variant's
/// name, so the name is the bytes after the map's and the string's headers.
fn kind_of(body: &[u8]) -> String {
    /// The name of a one-entry map's key at `at`: a fixmap of one, then a fixstr.
    fn name(body: &[u8], at: usize) -> Option<&str> {
        let (map, string) = (*body.get(at)?, *body.get(at + 1)?);
        if map != 0x81 || string & 0xe0 != 0xa0 {
            return None;
        }
        let len = usize::from(string & 0x1f);
        std::str::from_utf8(body.get(at + 2..at + 2 + len)?).ok()
    }
    match name(body, 0) {
        Some("Read") => format!("Read {}", name(body, 6).unwrap_or("?")),
        Some(other) => other.to_string(),
        None => "?".to_string(),
    }
}

/// The child's channel up, each frame recorded as it is written (`super::wire::write_message` writes ONE frame a
/// call): what crossed the pipe, and when.
struct Recorded {
    out: io::PipeWriter,
    sent: Arc<Mutex<Vec<Sent>>>,
}

impl Write for Recorded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.write(buf)
    }
    fn write_all(&mut self, frame: &[u8]) -> io::Result<()> {
        let kind = kind_of(frame.get(4..).unwrap_or_default());
        let sent = Sent { kind, bytes: frame.len(), at: Instant::now() };
        self.sent.lock().expect("sent").push(sent);
        self.out.write_all(frame)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// `request` answered by a child (`serve_child` over two OS pipes, on a thread of its own, [`the_files`] loaded, one
/// a mount) with its reads answered from `store` by this side's relay: what came up, and every frame the child wrote
/// ([`Sent`]).
fn through_a_child(
    request: &Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    pulses: &[Pulsed],
) -> (relay::Answered, Vec<Sent>) {
    let (mut from_child, child_out) = io::pipe().expect("pipe up");
    let (child_in, mut to_child) = io::pipe().expect("pipe down");
    let sent = Arc::new(Mutex::new(Vec::new()));
    let up = Recorded { out: child_out, sent: Arc::clone(&sent) };
    let builds = the_files(pulses);
    let child = thread::spawn(move || {
        let _ = serve_child(Box::new(child_in), Box::new(up), BUILD, builds);
    });
    let no_models = BTreeMap::new();
    let relayed = relay::relay_witnessed(
        &mut to_child,
        &mut from_child,
        BUILD,
        request,
        vec![(TEXT, &no_models); pulses.len()],
        store.as_ref(),
        &|| {},
        None,
        None,
    );
    drop(to_child);
    child.join().expect("the child's half");
    let answered = relayed.unwrap_or_else(|stopped| panic!("the relay stopped: {}", stopped.why));
    let sent = std::mem::take(&mut *sent.lock().expect("sent"));
    (answered, sent)
}

/// How many of `sent` are `kind` frames.
fn count(sent: &[Sent], kind: &str) -> usize {
    sent.iter().filter(|s| s.kind == kind).count()
}

/// `request`'s whole curve as THIS process runs it over `store` (the dispatch a child answers with, the same stand-in).
fn in_this_process(
    request: &Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    pulses: &[Pulsed],
) -> RunEquityCurve {
    let Request::Summarized(inner) = request else { panic!("a summarized request") };
    let strategies = loaded_strategy_files(
        request,
        the_files(pulses),
        vec![(TEXT.into(), BTreeMap::new()); pulses.len()],
    );
    let study = StudyDirs { strategies, ..StudyDirs::working_directory() };
    match answer_whole_here((**inner).clone(), store, &study, NamedRunLane::DISARMED, None).0 {
        Response::RunResult(whole) => {
            RunEquityCurve { equity: whole.equity_curve, timestamp: whole.equity_timestamp }
        }
        other => panic!("the run answers its whole result: {other:?}"),
    }
}

/// Every page of held run `run`'s whole curve through `curve_page`, joined: rebuilt over `store` where it must be.
fn read_back(
    run: &str,
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> Result<RunEquityCurve, String> {
    read_back_under(run, store, Path::new("."))
}

/// [`read_back`] of a held run, or of a saved run under `runs_root` by its id.
fn read_back_under(
    run: &str,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    runs_root: &Path,
) -> Result<RunEquityCurve, String> {
    let rebuild = CurveRebuild { store, build: Some(BUILD) };
    let mut curve = RunEquityCurve::default();
    for page in 0.. {
        match curve_page(run, page, CURVE_PAGE_SIZE_MAX, runs_root, Some(&rebuild)) {
            Response::CurvePage(part) if part.equity.is_empty() => break,
            Response::CurvePage(part) => {
                curve.equity.extend(part.equity);
                curve.timestamp.extend(part.timestamp);
            }
            Response::Error(why) => return Err(why),
            other => panic!("a held curve pages: {other:?}"),
        }
    }
    Ok(curve)
}

/// A curve's every equity by its bits, and its times.
fn bits(curve: &RunEquityCurve) -> (Vec<u64>, &[i64]) {
    (curve.equity.iter().map(|v| v.to_bits()).collect(), &curve.timestamp)
}

/// The run's name in the summary `held` answers.
fn run_of(held: &Response) -> String {
    match held {
        Response::RunSummary(summary) => summary.run.clone(),
        other => panic!("a single run answers its summary: {other:?}"),
    }
}

/// **The log in the curve's place, and the same curve back**: over a store that pins every bar, the child asks the
/// pin check once and sends its log (its recipe stamped and carrying the curve's hash) and no curve part; the hub holds
/// the log under its own build and no curve, and the curve it rebuilds is the one a store that pins nothing gets sent,
/// and the one the run answers in this process, to the bit.
#[test]
fn a_pinned_runs_child_sends_its_log_and_the_curve_rebuilt_from_it_is_the_one_sent_before() {
    let request = summarized(MINUTES);
    let whole = in_this_process(&request, &store_of(MINUTES, DayStamps::Settled), &[BTC]);
    assert_eq!(whole.equity.len(), MINUTES as usize, "one step a bar");

    let (before, _) = through_a_child(&request, &store_of(MINUTES, DayStamps::Unknown), &[BTC]);
    assert!(before.log.is_none(), "a store that stamps no day pins nothing");
    assert_eq!(bits(&before.curve), bits(&whole), "the curve a child sent before");

    let pinned = store_of(MINUTES, DayStamps::Settled);
    let (answered, sent) = through_a_child(&request, &pinned, &[BTC]);
    assert!(answered.curve.equity.is_empty(), "no curve part came up");
    let log = answered.log.as_ref().expect("the log came up in the curve's place");
    assert_eq!((count(&sent, "HeldLog"), count(&sent, "HeldCurve")), (1, 0), "{sent:?}");
    assert_eq!(count(&sent, "Read BarDays"), 1, "one pin check: {sent:?}");
    assert_eq!(log.log.recipe.curve_fnv, curve_fnv(&whole.equity, &whole.timestamp));
    let stamped = log.log.recipe.series.iter().all(|series| {
        series.days.as_ref().is_some_and(|d| !d.is_empty() && d.iter().all(|d| !d.provisional))
    });
    assert!(stamped, "every marked day stamped by the hub's store, none provisional");
    let saved = answered.studio_run.as_ref().expect("the saved run came up");
    assert!(saved.equity_log.is_none(), "the log crosses once, in the held frame");
    let logged: usize = sent.iter().filter(|s| s.kind == "HeldLog").map(|s| s.bytes).sum();

    let (held, _) = hold_from_child(answered, &request, Some(BUILD));
    let run = run_of(&held);
    let kept = held_log(&run).expect("the hub holds the log");
    assert!(held_curve(&run).is_none(), "and no curve");
    assert_eq!(kept.log.recipe.build.as_deref(), Some(BUILD), "under this daemon's build");
    let rebuilt = read_back(&run, &pinned).expect("the curve rebuilt from the held log");
    assert_eq!(bits(&rebuilt), bits(&before.curve), "to the bit, every step's time too");
    println!("SIZE: {} steps; {logged} bytes of log up the pipe", whole.equity.len());
}

/// **A run the store cannot pin sends and keeps its curve**: a store that stamps no day, and one that stamps the last
/// day provisional, get the child's curve parts and no log, the saved run the child leaves carries no log either (the
/// saver then keeps the curve file, as when the hub dropped the log after the child), and the hub holds the curve,
/// which reads back as the run's.
#[test]
fn a_run_the_store_cannot_pin_sends_and_keeps_its_curve() {
    let request = summarized(MINUTES);
    let whole = in_this_process(&request, &store_of(MINUTES, DayStamps::Settled), &[BTC]);
    for days in [DayStamps::Unknown, DayStamps::LastProvisional] {
        let store = store_of(MINUTES, days);
        let (answered, sent) = through_a_child(&request, &store, &[BTC]);
        assert!(answered.log.is_none(), "{days:?}: no log came up");
        assert_eq!(bits(&answered.curve), bits(&whole), "{days:?}: the whole curve came up");
        assert!(count(&sent, "HeldCurve") > 0 && count(&sent, "HeldLog") == 0, "{days:?}");
        assert_eq!(count(&sent, "Read BarDays"), 1, "{days:?}: the pin check was asked");
        let saved = answered.studio_run.as_ref().expect("the saved run came up");
        assert!(saved.equity_log.is_none(), "{days:?}: an unpinned log is not left to save");

        let (held, _) = hold_from_child(answered, &request, Some(BUILD));
        let run = run_of(&held);
        assert!(held_log(&run).is_none(), "{days:?}");
        let curve = held_curve(&run).expect("the hub holds the curve");
        assert_eq!(bits(&curve), bits(&whole), "{days:?}");
        assert_eq!(bits(&read_back(&run, &store).expect("read")), bits(&whole), "{days:?}");
    }
}

/// **A held or saved profile that names a file is refused, the file never opened** (a sandboxed child writes the profile
/// a rebuild reads, and the rebuild runs in this daemon): a hostile child's `HeldLog` whose profile names `data.file` (a
/// file that exists), read by its held name, and a saved run whose `config.toml` names it, read by the run's id, answer
/// the rebuild's refusal. And a summarized `RunBacktest` is held with the profile this process SENT, whatever profile its
/// child wrote.
#[test]
fn a_profile_naming_a_file_is_refused_held_or_saved_and_the_sent_profile_wins() {
    let request = summarized(MINUTES);
    let pinned = store_of(MINUTES, DayStamps::Settled);
    let (answered, _) = through_a_child(&request, &pinned, &[BTC]);
    let came_up = *answered.log.expect("the log came up in the curve's place");
    let log = came_up.log;
    let Request::Summarized(inner) = &request else { unreachable!("a summarized request") };
    let Request::RunBacktest(sent) = &**inner else { unreachable!("a profile run") };
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    assert!(Path::new(path).is_file(), "the premise: the named file exists");
    let hostile =
        sent.replace("interval = \"1m\"\n", &format!("interval = \"1m\"\nfile = {path:?}\n"));
    assert_ne!(&hostile, sent, "the fixture's text moved");
    let refused =
        |why: &str| why.contains("`data.file`") && why.contains("never rebuilt from a file");

    // Held: a hostile child's log, read by its held name.
    let hostile_log = HeldLog::new(hostile.clone(), log.clone());
    let held = hold_summary(Summary::Run(
        Box::default(),
        Vec::new(),
        RunEquityCurve::default(),
        Some(Box::new(hostile_log)),
    ));
    let why = read_back(&run_of(&held), &pinned).expect_err("a held profile naming a file");
    assert!(refused(&why), "{why}");

    // Saved: the run's `config.toml` names the file, read by the run's id.
    let runs = tempfile::tempdir().expect("a runs root");
    let run_id = saved_with_log(runs.path(), &hostile, &log);
    let rebuild = CurveRebuild { store: &pinned, build: Some(BUILD) };
    match curve_page(&run_id, 0, 10, runs.path(), Some(&rebuild)) {
        Response::Error(why) => assert!(refused(&why), "{why}"),
        other => panic!("a saved config.toml naming a file must be refused: {other:?}"),
    }

    // The profile this process sent wins over the one a summarized `RunBacktest`'s child wrote.
    let answered = relay::Answered {
        response: Response::RunSummary(Box::default()),
        points: Vec::new(),
        trades: Vec::new(),
        curve: RunEquityCurve::default(),
        log: Some(Box::new(HeldLog::new(hostile, log))),
        studio_run: None,
        rows: Vec::new(),
    };
    let (held, _) = hold_from_child(answered, &request, Some(BUILD));
    let kept = held_log(&run_of(&held)).expect("the hub holds the log");
    assert_eq!(&kept.profile_toml, sent, "the sent profile, not the child's");
}

/// One finished run under `runs_root` keeping `log` in its curve's place (its recipe inline, as an older run keeps it)
/// and `profile` as its `config.toml`: answers its id.
fn saved_with_log(runs_root: &Path, profile: &str, log: &EquityLog) -> String {
    let dir = vike_runs::create_run_dir(runs_root, 1_800_000_000, None).expect("a run dir");
    let manifest = vike_runs::RunManifest {
        schema: vike_runs::MANIFEST_SCHEMA,
        run_id: dir.run_id.clone(),
        kind: vike_runs::BACKTEST_RUN_KIND.to_string(),
        produced_by: "test".to_string(),
        started_at: vike_model::time::utc_rfc3339(1_800_000_000),
        finished_at: vike_model::time::utc_rfc3339(1_800_000_001),
        git_sha: None,
        strategy_sdk: None,
        fingerprint: None,
        config: vike_runs::RunConfig { path: None, name: None },
        detail: serde_json::json!({ "protocol": "single", "equity_log": log.recipe }),
        kept_rows: Vec::new(),
    };
    let extras = vike_runs::RunExtras {
        config_toml: Some(profile),
        equity_log: Some(&log.events),
        ..Default::default()
    };
    vike_runs::write_run_with(&dir.path, &manifest, &serde_json::json!({}), &extras)
        .expect("the run written");
    dir.run_id
}

/// **A portfolio whose mounts list its slots in another order than `[data]` rebuilds its curve, held and saved**: the
/// mounts name ETHUSDT then BTCUSDT while `[data]` lists BTCUSDT first, so the engine holds its slots in mount order.
/// Its child sends the log (recording the slots in that order, each series beside its own bars), and the curve rebuilt
/// from it, held by the run's name (the summarized `RunBacktest`) and saved by the run's id (`Request::ReadCurve` after
/// the held copy is gone), is the run's own, to the bit.
#[test]
fn a_portfolio_whose_mounts_reorder_its_slots_rebuilds_its_curve_held_and_saved() {
    let profile = format!(
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\", \"ETHUSDT\"]\nkind = \"bar\"\ninterval = \"1m\"\n\
         from = \"{S}\"\nto = \"{}\"\n\n[engine]\ncash = 100000.0\nattach_funding = false\n\n\
         [[mounts]]\nsymbols = [\"ETHUSDT\"]\n[mounts.strategy]\nfile = \"other/pulse_eth.rs\"\n\n\
         [[mounts]]\nsymbols = [\"BTCUSDT\"]\n[mounts.strategy]\nfile = \"other/pulse_btc.rs\"\n",
        S + MINUTES * MINUTE - 1
    );
    let request = Request::Summarized(Box::new(Request::RunBacktest(profile.clone())));
    let mounts: [Pulsed; 2] = [("ETHUSDT", 30), ("BTCUSDT", EVERY)];
    let store = planted(MINUTES);
    let eth: Vec<Bar> = (0..MINUTES)
        .map(|i| {
            let close = 2_000.0 + ((i * 13) % 89) as f64 - ((i * 7) % 31) as f64 / 3.0;
            Bar::ohlcv(S + i * MINUTE, close, close + 1.0, close - 1.0, close, 2.0)
        })
        .collect();
    store.append_bars("binance", "ETHUSDT", "1m", &eth, None).expect("ETHUSDT");
    let pinned = stamping(Arc::new(store), DayStamps::Settled);
    let whole = in_this_process(&request, &pinned, &mounts);
    assert_eq!(whole.equity.len(), MINUTES as usize, "one step a bar");

    let (answered, _) = through_a_child(&request, &pinned, &mounts);
    let log = answered.log.as_ref().expect("the log came up in the curve's place").log.clone();
    assert_eq!(log.recipe.symbols, ["ETHUSDT", "BTCUSDT"], "the engine's slots, in mount order");
    let named: Vec<&str> = log.recipe.series.iter().map(|s| s.symbol.as_str()).collect();
    assert_eq!(named, ["ETHUSDT", "BTCUSDT"], "each series recorded beside its own bars");
    let (held, _) = hold_from_child(answered, &request, Some(BUILD));
    let rebuilt = read_back(&run_of(&held), &pinned).expect("the held run's curve rebuilt");
    assert_eq!(bits(&rebuilt), bits(&whole), "held: to the bit, every step's time too");

    let runs = tempfile::tempdir().expect("a runs root");
    let run_id = saved_with_log(runs.path(), &profile, &log);
    let saved = read_back_under(&run_id, &pinned, runs.path()).expect("the saved run's curve");
    assert_eq!(bits(&saved), bits(&whole), "saved: to the bit, every step's time too");
}

/// One measured round of [`measure_the_curve_up_the_pipe`].
struct Round {
    /// Every byte the child wrote up, its reads included.
    up: usize,
    /// The bytes of the curve's parts, or of the log in their place.
    curve_or_log: usize,
    /// What the hub holds for the run's curve: 16 bytes a step, or the log, its day stamps and its profile.
    held: usize,
    /// From the child's first frame after its run (the pin check) to the hub holding the run, the hash the saver made
    /// of a held curve until now (`curve_fnv`, after the child exited) included.
    tail: Duration,
    /// The whole curve read back through `curve_page`: the held curve paged, or rebuilt from the log first.
    read: Duration,
}

/// **What sending the log instead of the curve saves** at the the CI box reference run's length (263,521 one-minute bars,
/// six months; 216 round trips, a daily USDT-USD rate), the same build both ways: BEFORE, a store that pins nothing,
/// so the child sends its curve (and the hub hashed it again for the saver, timed here); AFTER, a store that pins
/// every day, so the child sends its log. One warm-up and five rounds each, interleaved, medians: the bytes up the
/// pipe, the hub's held bytes, the tail (the child's last frames to the hub holding the run) and the first read of the
/// whole curve. The two curves read back must be equal to the bit. Run in release, alone.
#[test]
#[ignore = "a measurement: cargo test --release -p vike-backtesthub --lib measure_the_curve_up_the_pipe -- --ignored --nocapture"]
fn measure_the_curve_up_the_pipe() {
    const BARS: i64 = 263_521;
    let request = summarized(BARS);
    let modes =
        [("before: the curve up", DayStamps::Unknown), ("after: the log up", DayStamps::Settled)];
    let stores = modes.map(|(_, days)| store_of(BARS, days));
    let mut rounds: [Vec<Round>; 2] = [Vec::new(), Vec::new()];
    let mut curves: [Option<RunEquityCurve>; 2] = [None, None];
    for round in 0..6 {
        for (mode, store) in stores.iter().enumerate() {
            let (answered, sent) = through_a_child(&request, store, &[("BTCUSDT", 1_220)]);
            let up = sent.iter().map(|s| s.bytes).sum();
            let curve_or_log = sent
                .iter()
                .filter(|s| s.kind == "HeldCurve" || s.kind == "HeldLog")
                .map(|s| s.bytes)
                .sum();
            let tail_from =
                sent.iter().find(|s| s.kind == "Read BarDays").expect("the pin check").at;
            let held = match &answered.log {
                Some(log) => {
                    let days = log.log.recipe.series.iter().filter_map(|s| s.days.as_ref());
                    let days: usize = days.map(Vec::len).sum();
                    log.log.events.len() + days * size_of::<BarDay>() + log.profile_toml.len()
                }
                None => 16 * answered.curve.equity.len(),
            };
            // The hash the saver made of a held curve until now, after the child: part of the before's tail.
            if answered.log.is_none() {
                std::hint::black_box(curve_fnv(&answered.curve.equity, &answered.curve.timestamp));
            }
            let (summary, _) = hold_from_child(answered, &request, Some(BUILD));
            let tail = tail_from.elapsed();
            let started = Instant::now();
            let curve = read_back(&run_of(&summary), store).expect("the whole curve");
            let read = started.elapsed();
            if round > 0 {
                rounds[mode].push(Round { up, curve_or_log, held, tail, read });
            }
            curves[mode] = Some(curve);
        }
    }
    let [Some(before), Some(after)] = &curves else { panic!("both modes ran") };
    assert_eq!(before.equity.len(), BARS as usize, "one step a bar");
    assert_eq!(bits(after), bits(before), "the curve rebuilt from the log is the one sent before");
    for ((what, _), mut runs) in modes.into_iter().zip(rounds) {
        runs.sort_by_key(|r| r.tail);
        let mut reads: Vec<Duration> = runs.iter().map(|r| r.read).collect();
        reads.sort();
        let (median, best) = (&runs[runs.len() / 2], &runs[0]);
        println!(
            "{what}: up the pipe {} bytes ({} of them the curve or its log), the hub holds {} bytes, tail median \
             {:?} (best {:?}), first whole read median {:?}",
            median.up,
            median.curve_or_log,
            median.held,
            median.tail,
            best.tail,
            reads[reads.len() / 2],
        );
    }
}
