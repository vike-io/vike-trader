//! A live mount's INPUTS through this daemon's opener (decision 0271's live half, `tradehub_cli/inputs.rs`):
//! a venue series and a user's lane read from the datahub at mount, the venue series handed to the bar
//! keeper and the lane read again after its newest bar, and a real core mounting a strategy that reads both.
//! The datahub is a recorded stand-in for `datahub_input_reader`; nothing here dials anything.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use vike_core::{CoreConfig, Mount, spawn_core};
use vike_data::SeriesId;
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, BarSeed, BarUpdate, Engine, RiskGate};
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, RiskLimits, Strategy};

use super::super::inputs::{INPUT_HISTORY_BARS, InputKeep, InputReader, input_opener, within};

const H: i64 = 3_600_000;
const H4: i64 = 4 * H;
const DAY: i64 = 24 * H;
const HIGHER: &str = "binance:BTCUSDT:4h";
const LANE: &str = "user-mine:binance:USDT-USD:1d";
/// "Now" for every read here: two days in.
const NOW: i64 = 2 * DAY;

fn now() -> i64 {
    NOW
}

fn bar(ts: i64, close: f64) -> Bar {
    Bar::ohlcv(ts, close, close, close, close, 1.0)
}

/// One read the stand-in was asked: the series' lane (if any) and venue, then the window.
type Asked = Arc<Mutex<Vec<(Option<String>, String, i64, i64)>>>;

/// A stand-in datahub holding a venue's 4h candles and a user's daily lane of the same venue; any other
/// series is refused. It answers what the store holds inside the window it is asked.
fn datahub(asked: &Asked, lane_bars: Arc<Mutex<Vec<Bar>>>) -> InputReader {
    let asked = Arc::clone(asked);
    Arc::new(move |id: &SeriesId, start: i64, end: i64| {
        asked.lock().unwrap().push((id.source.clone(), id.venue.clone(), start, end));
        let held = match (id.source.as_deref(), id.symbol.as_str()) {
            (None, "BTCUSDT") => (0..12).map(|k| bar(k * H4, 10.0 * (k + 1) as f64)).collect(),
            (Some("user-mine"), "USDT-USD") => lane_bars.lock().unwrap().clone(),
            _ => return Err(format!("datahub: no series {}:{}", id.venue, id.symbol)),
        };
        Ok(held.into_iter().filter(|b: &Bar| (start..=end).contains(&b.timestamp)).collect())
    })
}

/// The opener as both arms build it, with no `[inputs]` row.
fn opener_at(keep: &Arc<InputKeep>, read: InputReader, now: fn() -> i64) -> vike_core::InputOpener {
    input_opener(Arc::clone(keep), read, now, kept, BTreeMap::new())
}

/// The kept rows this file's series are found by (`vike_catalog::kept_list`'s stand-in): binance's spot `BTCUSDT`,
/// a hyperliquid builder-dex perpetual `xyz:TSLA` (user name `XYZ:TSLAUSDC.P`, stored `XYZ-TSLAUSDC.P`) and
/// dukascopy's share CFD `SPY.US/USD` (user and store name `SPY.CFD`).
fn kept(venue: &str) -> Option<Arc<Vec<vike_model::Instrument>>> {
    use vike_model::{Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct};
    use vike_model::{ContractCfdUnderlying, ContractPerp, Instrument, Margining};
    let row = match venue {
        "binance" => Instrument::new(venue, "BTCUSDT", "BTC", "USDT", Contract::CryptoSpot),
        "hyperliquid" => Instrument::new(
            venue,
            "xyz:TSLA",
            "TSLA",
            "USDC",
            Contract::Perp(ContractPerp {
                margining: Margining::Linear,
                settle: "USDC".to_string(),
                value_currency: None,
                funding_interval_ms: None,
                settling_ms: None,
            }),
        ),
        "dukascopy" => Instrument::new(
            venue,
            "SPY.US/USD",
            "",
            "USD",
            Contract::Cfd(ContractCfd {
                underlying_kind: ContractCfdUnderlying::Share,
                underlying: None,
                product: ContractCfdProduct::Cfd,
                expiry: ContractCfdExpiry::Unknown,
                margin_rate: None,
                swap_long: None,
                swap_short: None,
            }),
        ),
        _ => return None,
    };
    // named as the datahub's write stores them (`vike_catalog::spelled`)
    Some(Arc::new(vike_catalog::spelled(vec![row])))
}

/// **A venue input's price lane is asked by the instrument's USER name** (decision 0290, audit flag DUK-4): the
/// series is read by its STORE name and kept fresh under its kept row's user name, which the venue's bridge
/// translates: a builder-dex `XYZ-TSLAUSDC.P` is `XYZ:TSLAUSDC.P`, a stock CFD `SPY.CFD` is itself. A stored name
/// no kept row carries is the opener's refusal in words, and nothing is kept fresh for it.
#[test]
fn a_venue_input_is_kept_fresh_under_its_user_name() {
    let keep = Arc::new(InputKeep::default());
    let stored: InputReader = Arc::new(|_: &SeriesId, _, _| Ok(vec![bar(0, 1.0)]));
    let mut open = opener_at(&keep, stored, now);
    let dex =
        open("hyperliquid:XYZ-TSLAUSDC.P:1h", "binance", "BTCUSDT", "1h").expect("a dex perp");
    assert_eq!(dex.key, ("hyperliquid".into(), "XYZ:TSLAUSDC.P".into(), "1h".into()));
    let cfd = open("dukascopy:SPY.CFD:1h", "binance", "BTCUSDT", "1h").expect("a stock CFD");
    assert_eq!(cfd.key, ("dukascopy".into(), "SPY.CFD".into(), "1h".into()));
    let old = open("dukascopy:SPYUSUSD:1h", "binance", "BTCUSDT", "1h").expect_err("no row");
    assert!(old.contains("SPYUSUSD") && old.contains("dukascopy"), "{old}");
    assert_eq!(
        keep.venue_series(),
        vec![
            ("hyperliquid".to_string(), "XYZ:TSLAUSDC.P".to_string(), "1h".to_string()),
            ("dukascopy".to_string(), "SPY.CFD".to_string(), "1h".to_string()),
        ]
    );
}

/// **The opener reads a venue series and a user's lane through the datahub's read**, as deep as a backtest
/// reads it: the 4h candles on an hourly mount (a higher timeframe) over their WHOLE stored history, the
/// day lane on a daily mount over its last `INPUT_HISTORY_BARS` bars. Each is filed under the key its later
/// bars arrive by and kept fresh: the venue series by the bar keeper, the lane by the refresher.
#[test]
fn the_opener_reads_a_venue_series_and_a_user_lane_from_the_datahub() {
    let asked = Asked::default();
    let keep = Arc::new(InputKeep::default());
    let lane = Arc::new(Mutex::new(vec![bar(0, 1.0)]));
    let mut open = opener_at(&keep, datahub(&asked, lane), now);

    let higher = open(HIGHER, "binance", "BTCUSDT", "1h").expect("the venue series opens");
    assert_eq!(higher.key, ("binance".into(), "BTCUSDT".into(), "4h".into()));
    assert_eq!(higher.interval_ms, H4);
    assert_eq!(higher.bars.len(), 12, "every stored bar up to now");
    let daily = open(LANE, "binance", "BTCUSDT", "1d").expect("the lane opens");
    assert_eq!(daily.key, ("user-mine:binance".into(), "USDT-USD".into(), "1d".into()));
    assert_eq!((daily.interval_ms, daily.bars.len()), (DAY, 1));

    assert_eq!(
        *asked.lock().unwrap(),
        vec![
            (None, "binance".to_string(), 0, NOW),
            (
                Some("user-mine".to_string()),
                "binance".to_string(),
                NOW - INPUT_HISTORY_BARS * DAY,
                NOW
            ),
        ]
    );
    assert_eq!(
        keep.venue_series(),
        vec![("binance".to_string(), "BTCUSDT".to_string(), "4h".to_string())],
        "the venue series joins the bar keeper's list; the lane does not"
    );
}

/// What the refresher handed the core: each lane's key and its new bars.
type Sent = Arc<Mutex<Vec<(vike_execution::SeriesKey, Vec<Bar>)>>>;

/// **A lane is read again after its newest bar, and only what is new reaches the core, under its key**; a
/// read that fails sends nothing and is asked again; a core that is gone ends the pass.
#[test]
fn a_lane_is_read_again_after_its_newest_bar() {
    let asked = Asked::default();
    let keep = Arc::new(InputKeep::default());
    let lane = Arc::new(Mutex::new(vec![bar(0, 1.0)]));
    let read = datahub(&asked, Arc::clone(&lane));
    let mut open = opener_at(&keep, Arc::clone(&read), now);
    open(LANE, "binance", "BTCUSDT", "1h").expect("the lane opens");
    asked.lock().unwrap().clear();

    let sent: Sent = Arc::default();
    let into = Arc::clone(&sent);
    let send = move |key, bars| {
        into.lock().unwrap().push((key, bars));
        true
    };
    assert!(keep.refresh_lanes(&read, &send, NOW));
    assert!(sent.lock().unwrap().is_empty(), "nothing new");

    lane.lock().unwrap().push(bar(DAY, 2.0));
    assert!(keep.refresh_lanes(&read, &send, NOW));
    assert_eq!(
        *sent.lock().unwrap(),
        vec![(("user-mine:binance".into(), "USDT-USD".into(), "1d".into()), vec![bar(DAY, 2.0)])]
    );
    let starts: Vec<i64> = asked.lock().unwrap().iter().map(|a| a.2).collect();
    assert_eq!(starts, vec![1, 1], "each read starts after the newest bar sent");
    assert!(keep.refresh_lanes(&read, &send, NOW));
    assert_eq!(asked.lock().unwrap().last().map(|a| a.2), Some(DAY + 1));

    let failing: InputReader = Arc::new(|_: &SeriesId, _, _| Err("datahub down".to_string()));
    assert!(keep.refresh_lanes(&failing, &send, NOW), "a failed read is asked again");
    lane.lock().unwrap().push(bar(2 * DAY, 3.0));
    assert!(!keep.refresh_lanes(&read, &|_, _| false, NOW), "a core that is gone ends the pass");
}

/// **A one-part name is the mount's own symbol at that bar length** (`vike_data::store::series::parse_input_name_for`,
/// the backtest's grammar): `"4h"` on a binance BTCUSDT mount reads `binance:BTCUSDT:4h`.
#[test]
fn a_one_part_name_reads_the_mounts_own_symbol() {
    let asked = Asked::default();
    let keep = Arc::new(InputKeep::default());
    let mut open = opener_at(&keep, datahub(&asked, Arc::default()), now);
    let own = open("4h", "binance", "BTCUSDT", "1h").expect("the mount's own 4-hour bars");
    assert_eq!(own.key, ("binance".into(), "BTCUSDT".into(), "4h".into()));
    assert_eq!((own.interval_ms, own.bars.len()), (H4, 12));
    let bad = open("fortnight", "binance", "BTCUSDT", "1h").expect_err("no bar length");
    assert!(bad.contains("is not one"), "{bad}");
}

/// **A name the grammar refuses, or a series the datahub cannot read, is the opener's error** (the core
/// refuses the mount with it), and nothing is kept fresh for it.
#[test]
fn a_name_that_cannot_be_read_is_the_openers_error() {
    let asked = Asked::default();
    let keep = Arc::new(InputKeep::default());
    let mut open = opener_at(&keep, datahub(&asked, Arc::default()), now);
    let bad = open("binance:BTCUSDT", "binance", "BTCUSDT", "1h").expect_err("not the grammar");
    assert!(bad.contains("is not VENUE:SYMBOL:INTERVAL"), "{bad}");
    let unread =
        open("binance:SOLUSDT:4h", "binance", "BTCUSDT", "1h").expect_err("the datahub holds none");
    assert!(unread.contains("datahub: no series binance:SOLUSDT"), "{unread}");
    let empty = open(LANE, "binance", "BTCUSDT", "1h")
        .expect("a lane the store holds nothing of reads empty");
    assert!(empty.bars.is_empty(), "the core refuses an empty input");
    assert!(keep.venue_series().is_empty());
}

/// **The profile's `[inputs]` row decides the series read** (the backtest profile's remap): the code's
/// name is the key, the row's series is read and filed; a row naming a series the grammar refuses is the
/// opener's error, saying the row maps it there; a name with no row reads itself.
#[test]
fn an_inputs_row_remaps_the_series_read() {
    let asked = Asked::default();
    let keep = Arc::new(InputKeep::default());
    let remap = BTreeMap::from([
        ("binance:SOLUSDT:4h".to_string(), HIGHER.to_string()),
        ("binance:ETHUSDT:4h".to_string(), "binance:ETHUSDT".to_string()),
    ]);
    let mut open =
        input_opener(Arc::clone(&keep), datahub(&asked, Arc::default()), now, kept, remap);
    let read = open("binance:SOLUSDT:4h", "binance", "BTCUSDT", "1h").expect("remapped to BTCUSDT");
    assert_eq!(read.key, ("binance".into(), "BTCUSDT".into(), "4h".into()));
    assert_eq!(read.bars.len(), 12);
    let bad = open("binance:ETHUSDT:4h", "binance", "BTCUSDT", "1h").expect_err("not the grammar");
    assert!(bad.starts_with("[inputs] maps it to \"binance:ETHUSDT\": "), "{bad}");
    open(HIGHER, "binance", "BTCUSDT", "1h").expect("no row reads the code's own name");
}

/// **A read at mount is bounded** (the read runs on the core's fold thread): one that has not answered
/// within its bound is refused naming the bound, and one that answers in time passes through unchanged.
#[test]
fn a_read_that_outlives_its_bound_is_refused() {
    let slow: InputReader = Arc::new(|_: &SeriesId, _, _| {
        std::thread::sleep(std::time::Duration::from_secs(2));
        Ok(vec![bar(0, 1.0)])
    });
    let id = vike_data::store::series::parse_input_name(HIGHER).unwrap().0;
    let bound = std::time::Duration::from_millis(50);
    let began = std::time::Instant::now();
    let e = within(slow, bound)(&id, 0, NOW).expect_err("past its bound");
    assert!(began.elapsed() < std::time::Duration::from_secs(1), "the caller is not held");
    assert!(e.contains("did not answer within 50ms"), "{e}");
    let quick: InputReader = Arc::new(|_: &SeriesId, _, _| Ok(vec![bar(0, 1.0)]));
    assert_eq!(
        within(quick, std::time::Duration::from_secs(5))(&id, 0, NOW),
        Ok(vec![bar(0, 1.0)])
    );
}

type Look = (i64, Option<f64>, Option<f64>);

struct Probe(Arc<Mutex<Vec<Look>>>);

impl<B: OrderBroker> Strategy<B> for Probe {
    fn inputs(&self) -> Vec<String> {
        vec![HIGHER.to_string(), LANE.to_string()]
    }
    fn on_bar(&mut self, b: &mut B, bar: &Bar) {
        let look =
            (bar.timestamp, b.input(HIGHER).map(|x| x.close), b.input(LANE).map(|x| x.close));
        self.0.lock().unwrap().push(look);
    }
}

/// **A live mount reads both inputs through this daemon's opener**: a real core, its strategy declaring a
/// venue's 4h candles and a user's daily lane, the history read at mount, a 4h close arriving on the bar
/// lane as the bar keeper's subscription delivers it, and the lane's next day handed over by the refresher.
#[test]
fn a_live_mount_reads_a_venue_series_and_a_user_lane() {
    let asked = Asked::default();
    let keep = Arc::new(InputKeep::default());
    let lane = Arc::new(Mutex::new(vec![bar(0, 1.0)]));
    let read = datahub(&asked, Arc::clone(&lane));
    let looks = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    // The store holds the 4h bars up to the one opened at 20:00, as at a mount at 20:00.
    let at_mount = || 20 * H;
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: Some("reads".into()),
            underlying_symbol: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            strategy: Box::new(Probe(Arc::clone(&looks))),
        }),
        input_opener: Some(opener_at(&keep, Arc::clone(&read), at_mount)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    assert_eq!(
        keep.venue_series(),
        vec![("binance".to_string(), "BTCUSDT".to_string(), "4h".to_string())],
        "the bar keeper is asked to hold the 4h candles open"
    );
    let bars = handle.bar_sender();
    let own = |ts: i64| BarUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        bar: bar(ts, 100.0),
        sides: None,
    };
    // 23:00 decides at 24:00: the 4h bar opened at 20:00 (stored) and the day bar both close then.
    bars.close(own(23 * H)).unwrap();
    // The 4h bar opened at 24:00 closes at 28:00 and arrives on the bar lane; the decision at 28:00 sees it.
    bars.close(BarUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "4h".into(),
        bar: bar(6 * H4, 70.0),
        sides: None,
    })
    .unwrap();
    bars.close(own(27 * H)).unwrap();
    // The user imports the next day; the refresher hands it over, and the decision at 48:00 sees it.
    lane.lock().unwrap().push(bar(DAY, 2.0));
    let seed = |(venue, symbol, interval): vike_execution::SeriesKey, seed: Vec<Bar>| {
        bars.seed(BarSeed { venue, symbol, interval, bars: seed }).is_ok()
    };
    assert!(keep.refresh_lanes(&read, &seed, NOW));
    bars.close(own(2 * DAY - H)).unwrap();
    handle.shutdown_and_join();

    assert_eq!(
        *looks.lock().unwrap(),
        vec![
            (23 * H, Some(60.0), Some(1.0)),
            (27 * H, Some(70.0), Some(1.0)),
            (2 * DAY - H, Some(70.0), Some(2.0)),
        ]
    );
}

/// What the paper probe saw: each decision's bar open and the 4h input's close, and every refusal reason.
#[derive(Default)]
struct PaperSeen {
    looks: Vec<(i64, Option<f64>)>,
    denied: Vec<String>,
}

/// Reads the 4h input at each of its own bars and buys one at market (an OPENING order), hearing every
/// refusal.
struct PaperProbe(Arc<Mutex<PaperSeen>>);

impl Strategy<vike_core::LiveBroker> for PaperProbe {
    fn inputs(&self) -> Vec<String> {
        vec![HIGHER.to_string()]
    }
    fn on_bar(&mut self, b: &mut vike_core::LiveBroker, bar: &Bar) {
        let close = b.input(HIGHER).map(|x| x.close);
        self.0.lock().unwrap().looks.push((bar.timestamp, close));
        b.place("", "", 1, 1.0, OrderPlacement::Market, false);
    }
    fn on_order_event(&mut self, _b: &mut vike_core::LiveBroker, event: &vike_model::OrderEvent) {
        if let vike_model::OrderEventKind::Denied { reason } = &event.kind {
            self.0.lock().unwrap().denied.push(reason.clone());
        }
    }
}

/// **A PAPER mount reads its inputs as a live one does** (the paper arm, `flags.tradehub_live` off): the
/// production paper builder, handed this daemon's opener through `StrategyCorePaperConfig::input_opener`, MOUNTS a
/// strategy that declares the 4h candles (before, it was refused by name), reads their stored history at
/// mount, answers `OrderBroker::input` as of each decision, and HOLDS the strategy's opening order once the input
/// is stale (its newest bar closed more than two of its bars before the decision), by name.
#[test]
fn a_paper_mount_reads_its_input_and_holds_opening_orders_when_it_is_stale() {
    let asked = Asked::default();
    let keep = Arc::new(InputKeep::default());
    let read = datahub(&asked, Arc::default());
    // The store holds the 4h bars up to the one opened at 20:00 (it closes at 24:00), as at a mount at 20:00.
    let at_mount = || 20 * H;
    let seen = Arc::new(Mutex::new(PaperSeen::default()));
    let mut config = vike_tradehub_core::MountPlacement::new("binance", "BTCUSDT", 1_000.0, 0.01);
    (config.interval, config.interval_ms) = ("1h".to_string(), H);
    let mut spec = config.clone();
    spec.controller_id = Some("paper-reads".into());
    // A sentinel this test owns and never creates: the paper book's HALT stays off.
    let sentinel = std::env::temp_dir()
        .join(format!("vike-tradehub-paper-inputs-{}", std::process::id()))
        .join("HALT");
    let opts = vike_tradehub_core::StrategyCorePaperConfig {
        halt: vike_tradehub_core::StrategyCorePaperHalt::Pinned(sentinel),
        input_opener: Some(opener_at(&keep, read, at_mount)),
        ..Default::default()
    };
    let core = vike_tradehub_core::build_paper_strategy_core_with(
        Box::new(PaperProbe(Arc::clone(&seen))),
        &spec,
        opts,
    );
    assert_eq!(
        asked.lock().unwrap().first().map(|a| (a.2, a.3)),
        Some((0, 20 * H)),
        "the 4h input's whole stored history, read at mount"
    );
    let bars = core.handle.bar_sender();
    let own = |ts: i64| BarUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        bar: bar(ts, 100.0),
        sides: None,
    };
    // 23:00 decides at 24:00, the 20:00 4h bar closing then: fresh, the buy is not held.
    bars.close(own(23 * H)).unwrap();
    // 40:00 decides at 41:00: the newest 4h bar closed at 24:00, 17 hours (more than two 4h bars) before.
    bars.close(own(40 * H)).unwrap();
    core.handle.shutdown_and_join();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.looks, vec![(23 * H, Some(60.0)), (40 * H, Some(60.0))]);
    let held: Vec<&String> = seen.denied.iter().filter(|r| r.starts_with("input-stale")).collect();
    assert_eq!(held.len(), 1, "only the stale decision's buy is held: {:?}", seen.denied);
    assert!(held[0].contains("the input `binance:BTCUSDT:4h`"), "{held:?}");
}
