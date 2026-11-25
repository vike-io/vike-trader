use super::*;
use std::cell::Cell;

use vike_data::{HistoryStore, HistoryStoreError};
use vike_model::Strategy;
use vike_simulator::{FillModelKind, OrderBrokerSimulator, StrategyEngineConfig};

use crate::run::KeptRows;
use crate::run::tick_replay::{SeriesKind, TickReplayError, replay_ticks_kept};

fn quote(ts: i64, bid: f64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask: bid + 1.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "TKN".into(),
    }
}

fn trade(ts: i64, price: f64) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "TKN".into(),
    }
}

/// A tape holding nothing, which is all the memo's own rules need.
fn empty_tape() -> TickTape {
    TickTape { ticks: Vec::new(), bars: Vec::new(), data_warnings: Vec::new() }
}

/// A config of one symbol on binance and nothing heard.
fn one_symbol(symbol: &str) -> TickReplayConfig {
    TickReplayConfig {
        venue: "binance".to_string(),
        symbols: vec![symbol.to_string()],
        range: TimestampRange { start: Some(0), end: Some(10_000) },
        seed_bar_interval_ms: None,
        params: StrategyEngineConfig::default(),
        snap_to_properties: false,
        feed_latency: false,
        series: None,
        reference: Vec::new(),
        mark: Vec::new(),
        feed_status: false,
    }
}

/// **A failed preparation keeps nothing; the next run prepares, and the one after borrows**: the single flight's
/// failure rule, one call at a time.
#[test]
fn a_failed_preparation_keeps_nothing_then_one_prepares_and_the_next_borrows() {
    let tapes = TickTapes::default();
    let config = one_symbol("BTCUSDT");
    let prepared = Cell::new(0usize);
    let failed = tapes.tape(&config, Vec::new(), |_| {
        prepared.set(prepared.get() + 1);
        Err("the datahub went away")
    });
    assert!(failed.is_err(), "the failure is the run's");
    assert_eq!(tapes.prepared(), 0, "a failed preparation keeps nothing");
    let second = tapes.tape(&config, Vec::new(), |_| {
        prepared.set(prepared.get() + 1);
        Ok::<_, &str>(empty_tape())
    });
    let second = second.expect("the next run prepares");
    let third = tapes.tape(&config, Vec::new(), |_| -> Result<TickTape, &str> {
        panic!("a held tape is borrowed, never prepared again")
    });
    let third = third.expect("the third borrows");
    assert_eq!(prepared.get(), 2, "one failed preparation and one good");
    assert!(Arc::ptr_eq(second.tape(), third.tape()), "the third run's tape is the second's");
    assert_eq!(tapes.prepared(), 1);
}

/// A store serving three quotes of any symbol.
struct ThreeQuotes;

impl HistoryStore for ThreeQuotes {
    fn scan_quotes(
        &self,
        _venue: &str,
        symbol: &str,
        _range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let tape = [quote(1_000, 150.0), quote(2_000, 150.1), quote(3_000, 149.9)];
        Ok(tape.into_iter().map(|q| TickQuote { symbol: symbol.into(), ..q }).collect())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// **A run that borrows a held tape is judged by its OWN params**: `USDJPY` books on a yen ledger and prepares
/// the tape; the same data on the dollar ledger borrows it and is refused, as it would be alone.
#[test]
fn a_borrower_whose_params_are_refused_is_refused() {
    struct Noop;
    impl Strategy<OrderBrokerSimulator> for Noop {}
    let tapes = TickTapes::default();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(ThreeQuotes);
    let on_oanda = || TickReplayConfig { venue: "oanda".to_string(), ..one_symbol("USDJPY") };
    let mut yen = on_oanda();
    yen.params.account_currency = Some("JPY".to_string());
    let kept = KeptRows::default();
    let ran = replay_ticks_kept(Arc::clone(&store), Noop, yen, &kept, Vec::new(), Some(&tapes));
    assert!(ran.is_ok(), "the yen ledger books it: {:?}", ran.err());
    let dollar = on_oanda();
    let refused = replay_ticks_kept(store, Noop, dollar, &kept, Vec::new(), Some(&tapes));
    assert!(matches!(refused, Err(TickReplayError::ForeignQuote(_))), "{:?}", refused.err());
    assert_eq!(tapes.prepared(), 1, "the refused run borrowed the tape and prepared none");
}

/// **A NaN in the derived events is the same input as itself**: compared by bits, the second run borrows.
#[test]
fn derived_events_holding_a_nan_share_one_tape() {
    let tapes = TickTapes::default();
    let config = one_symbol("BTCUSDT");
    let leg = || vec![("LEG".to_string(), vec![quote(1, f64::NAN)])];
    let first = tapes.tape(&config, leg(), |_| Ok::<_, ()>(empty_tape())).expect("prepared");
    let second = tapes.tape(&config, leg(), |_| -> Result<TickTape, ()> {
        panic!("the same events must borrow")
    });
    assert!(Arc::ptr_eq(first.tape(), second.expect("borrowed").tape()));
    assert_eq!(tapes.prepared(), 1);
}

/// **A tape's key is every data input and nothing else** (`tapes::TapeKey`): a config that differs from another in
/// ONE data field gets a tape of its own, never the other's, and asked again borrows that one; a config that
/// differs only in the RUN half (the engine params, the snapped grid) borrows the first. The data fields: `venue`,
/// `symbols`, `series` (its venue, symbol, kind and account), `range`, `seed_bar_interval_ms`, `feed_latency`,
/// `feed_status`, `reference` (venue, symbol, kind, trade, delay_ms), `mark` (venue, symbol, kind, delay_ms) and
/// the derived `[data.options]` events.
#[test]
fn a_tape_is_shared_by_equal_data_and_never_across_a_data_input() {
    let config = || TickReplayConfig {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        range: TimestampRange { start: Some(0), end: Some(1_000) },
        seed_bar_interval_ms: Some(60_000),
        params: StrategyEngineConfig::default(),
        snap_to_properties: false,
        feed_latency: false,
        series: None,
        reference: vec![ProfileDataReference {
            venue: "bybit".to_string(),
            symbol: "BTCUSDT".to_string(),
            kind: SeriesKind::Quote,
            trade: false,
            delay_ms: None,
        }],
        mark: vec![ProfileDataMark {
            venue: "okx".to_string(),
            symbol: "BTC-USDT".to_string(),
            kind: SeriesKind::Trade,
            delay_ms: Some(25),
        }],
        feed_status: false,
    };
    // `[data.options]`' events, as `replay_ticks_kept` takes them.
    type Derived = Vec<(String, Vec<TickQuote>)>;
    let leg = |bid: f64| vec![("LEG".to_string(), vec![quote(1, bid)])];
    let tapes = TickTapes::default();
    let prepared = std::cell::Cell::new(0usize);
    let tape = |config: &TickReplayConfig, derived: Derived| {
        let prepare = |_: Derived| {
            prepared.set(prepared.get() + 1);
            Ok::<_, ()>(TickTape { ticks: Vec::new(), bars: Vec::new(), data_warnings: Vec::new() })
        };
        Arc::clone(tapes.tape(config, derived, prepare).expect("prepared").tape())
    };
    let first = tape(&config(), Vec::new());

    type Change = fn(&mut TickReplayConfig);
    let data: [(&str, Change); 21] = [
        ("venue", |c| c.venue = "bybit".to_string()),
        ("symbols", |c| c.symbols = vec!["ETHUSDT".to_string()]),
        ("series: the same pair under its account", |c| {
            let mut s = ProfileDataSeries::new("binance", "BTCUSDT");
            s.account = Some("binance/DEMO".to_string());
            c.series = Some(vec![s]);
        }),
        ("series: another venue", |c| {
            c.series = Some(vec![ProfileDataSeries::new("okx", "BTCUSDT")])
        }),
        ("series: one lane", |c| {
            let mut s = ProfileDataSeries::new("binance", "BTCUSDT");
            s.kind = SeriesKind::Trade;
            c.series = Some(vec![s]);
        }),
        ("range: its start", |c| c.range.start = Some(1)),
        ("range: its end", |c| c.range.end = Some(2_000)),
        ("seed_bar_interval_ms: none", |c| c.seed_bar_interval_ms = None),
        ("seed_bar_interval_ms: another", |c| c.seed_bar_interval_ms = Some(1_000)),
        ("feed_latency", |c| c.feed_latency = true),
        ("feed_status", |c| c.feed_status = true),
        ("reference: its venue", |c| c.reference[0].venue = "okx".to_string()),
        ("reference: its symbol", |c| c.reference[0].symbol = "ETHUSDT".to_string()),
        ("reference: its kind", |c| c.reference[0].kind = SeriesKind::Tick),
        ("reference: traded", |c| c.reference[0].trade = true),
        ("reference: its delay", |c| c.reference[0].delay_ms = Some(5)),
        ("mark: its venue", |c| c.mark[0].venue = "bybit".to_string()),
        ("mark: its symbol", |c| c.mark[0].symbol = "ETH-USDT".to_string()),
        ("mark: its kind", |c| c.mark[0].kind = SeriesKind::Quote),
        ("mark: its delay", |c| c.mark[0].delay_ms = Some(26)),
        ("reference: none", |c| c.reference.clear()),
    ];
    let mut cases: Vec<(String, TickReplayConfig, Derived)> = data
        .into_iter()
        .map(|(named, change)| {
            let mut changed = config();
            change(&mut changed);
            (named.to_string(), changed, Vec::new())
        })
        .collect();
    cases.push(("derived: an option leg's events".to_string(), config(), leg(1.0)));
    cases.push(("derived: one event moved".to_string(), config(), leg(2.0)));
    for (named, changed, derived) in cases {
        let before = prepared.get();
        let own = tape(&changed, derived.clone());
        assert_eq!(prepared.get(), before + 1, "{named}: a tape of its own");
        assert!(!Arc::ptr_eq(&own, &first), "{named}: never the first config's tape");
        assert!(Arc::ptr_eq(&tape(&changed, derived), &own), "{named}: asked again, it borrows");
    }

    let run_half: [(&str, Change); 4] = [
        ("params: the cash", |c| c.params.cash = 5.0),
        ("params: the fill model", |c| c.params.fill_model = FillModelKind::L2Book),
        ("params: the ledger", |c| c.params.account_currency = Some("EUR".to_string())),
        ("snap_to_properties", |c| c.snap_to_properties = true),
    ];
    let before = prepared.get();
    for (named, change) in run_half {
        let mut changed = config();
        change(&mut changed);
        assert!(Arc::ptr_eq(&tape(&changed, Vec::new()), &first), "{named} is the run's");
    }
    assert_eq!(prepared.get(), before, "the run half prepares nothing");
    assert_eq!(tapes.prepared(), 1 + 23, "one tape per distinct data input, held for the request");
}

/// A day-shaped tape for any series, counting its lane scans: a print every 4 s on a 0.1 tick around 108,000, a
/// sudden 30–150 move every 400 of them (so momentum's thresholds part), and a quote every 10th
/// print that the recording box received 50 ms after it.
struct SweepTape {
    trades: Vec<TickTrade>,
    scans: std::sync::atomic::AtomicUsize,
}

impl SweepTape {
    fn day() -> Arc<SweepTape> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 33
        };
        let mut price = 108_000.0;
        let trades = (0..20_000i64)
            .map(|i| {
                price += (next() % 3) as f64 * 0.1 - 0.1;
                if i % 400 == 399 {
                    let jump = 30.0 + (next() % 121) as f64;
                    price += if next() % 2 == 0 { jump } else { -jump };
                }
                TickTrade { size: 0.01, is_buyer_maker: next() % 2 == 0, ..trade(i * 4_000, price) }
            })
            .collect();
        Arc::new(SweepTape { trades, scans: Default::default() })
    }

    fn scans(&self) -> usize {
        self.scans.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl HistoryStore for SweepTape {
    fn scan_trades(
        &self,
        _venue: &str,
        symbol: &str,
        _range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let named = |t: &TickTrade| TickTrade { symbol: symbol.into(), ..t.clone() };
        Ok(self.trades.iter().map(named).collect())
    }
    fn scan_quotes(
        &self,
        _venue: &str,
        symbol: &str,
        _range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.scans.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let touch = |t: &TickTrade| TickQuote {
            local_timestamp: t.timestamp + 50,
            bid: t.price - 0.05,
            ask: t.price + 0.05,
            symbol: symbol.into(),
            ..quote(t.timestamp, 0.0)
        };
        Ok(self.trades.iter().step_by(10).map(touch).collect())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// The 8-point momentum sweep of the reuse measurement (`threshold` × `tp`) over [`SweepTape`], `heard` added to
/// its `[data]`.
fn momentum_sweep(heard: &str) -> crate::run::profile::ProfileBacktest {
    let text = format!(
        r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "tick"
interval = "1d"
from = "0"
to = "100000000"
{heard}

[engine]
cash = 1000000.0
seed_bar_interval_ms = 60000

[strategy]
file = "trend/momentum.rs"
[strategy.params]
qty = 0.05
sl = 450.0
time_limit_ms = 28800000
cooldown_ms = 300000

[paramscan]
threshold = [40.0, 60.0, 80.0, 120.0]
tp = [600.0, 900.0]
"#
    );
    crate::run::profile::ProfileBacktest::from_toml_str(&text)
        .expect("the sweep profile loads")
        .with_stand_in(vike_strategy_lab::stand_in)
}

/// `base`'s sweep on the bounded pool against each of its points run alone over the same tape: every point's whole
/// record is the point's alone, and the WHOLE sweep reads the store's lanes as often as ONE run does (one tape,
/// prepared once while the other workers waited). Answers how many distinct records the points made.
fn sweep_against_each_point_alone(base: &crate::run::profile::ProfileBacktest) -> usize {
    use crate::search::optimize::{SearchEvaluator, SearchEvaluatorStore};
    use crate::search::paramscan::{
        ParamscanExecution, RankMetric, expand_paramscan_overrides, profile_with_overrides,
    };
    let tape = SweepTape::day();
    let store: Arc<dyn HistoryStore + Send + Sync> = tape.clone();
    let (rank, parallel) = (RankMetric::Sharpe, ParamscanExecution::Parallel);
    let sweep =
        SearchEvaluatorStore::classic(base, Arc::clone(&store), rank, parallel).with_results();
    let points = expand_paramscan_overrides(base).expect("the grid expands");
    assert_eq!(points.len(), 8);
    for point in sweep.evaluate(points.clone()) {
        assert!(point.row.error.is_none(), "{:?}: {:?}", point.row.overrides, point.row.error);
    }
    let swept_scans = tape.scans();
    let mut swept = sweep.take_results();
    let mut records = std::collections::BTreeSet::new();
    for point in points {
        let before = tape.scans();
        let (profile, _) = profile_with_overrides(base, point.clone());
        let alone = crate::run::run_backtest(&profile, Arc::clone(&store)).expect("it runs alone");
        assert_eq!(tape.scans() - before, swept_scans, "the whole sweep read what one run reads");
        let point_of_sweep = swept.remove(&point).expect("the sweep kept the point");
        let alone_record = format!("{alone:?}");
        assert_eq!(format!("{point_of_sweep:?}"), alone_record, "{point:?}");
        records.insert(alone_record);
    }
    records.len()
}

/// **A tick sweep prepares its tape once, and each point is that point run alone**, record for record (trades,
/// curve and its stamps, equity, fees, warnings): the guard against a tape key that misses a data input.
#[test]
fn a_tick_sweep_prepares_one_tape_and_each_point_is_that_point_run_alone() {
    // The points part (a strategy param reaches the run), so the comparison is no comparison of one record.
    assert!(sweep_against_each_point_alone(&momentum_sweep("")) > 1);
}

/// ...and with heard streams in it (`[[data.reference]]`'s touch, `[[data.mark]]`'s prints): they are part of
/// the one tape, and every point hears them as it would alone.
#[test]
fn a_tick_sweep_with_heard_streams_shares_them_and_each_point_is_that_point_run_alone() {
    let heard = r#"
[[data.reference]]
venue = "bybit"
symbol = "BTCUSDT"
kind = "quote"

[[data.mark]]
venue = "okx"
symbol = "BTC-USDT"
kind = "trade"
delay_ms = 25
"#;
    assert!(sweep_against_each_point_alone(&momentum_sweep(heard)) > 1);
}
