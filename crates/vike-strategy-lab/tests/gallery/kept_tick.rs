//! A gallery-born file's backtest takes its instrument's tick from the CURRENT kept row where nothing dated
//! states one (the owner's ruling of 2026-11-19, `vike_simulator::StrategyEngineConfig::kept_ticks`), through the
//! door a product run takes (`ProfileBacktest::read_kept_rows`) over a store that holds bars and NO properties
//! record: a file that returns on a bar with no tick now trades, the row's lot and minimums stay unread, the run
//! says once where its tick came from, and a file whose orders state no level trades to the same bits with the
//! row's tick or without it. Over a store that holds a dated record, a run that does not snap reads the record's
//! tick over the row's (`vike_simulator::StrategyEngineConfig::dated_ticks`), and `[risk]`'s over both.

use std::sync::Arc;

use vike_analytics::TradingRecord;
use vike_backtest::run::profile::ProfileBacktest;
use vike_backtest::run::run_backtest;
use vike_backtest::run::strategy_builder::StrategyBuilder;
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_model::{Contract, Instrument, Strategy, SymbolProperties};
use vike_simulator::OrderBrokerSimulator;
use vike_strategy_lab::gallery;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, RuleRun, RuleRunOptions, Tactics};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const MINUTE: i64 = 60_000;
/// Thirteen hours of minutes from 00:00 UTC.
const BARS: i64 = 13 * 60;
/// What the run's data warnings say of a tick a strategy read from the kept row.
const KEPT: &str = "current instrument list";

/// A triangle of 0.7 a minute, up for an hour and down for the next, every price 0.003 off a 0.01 grid:
/// it rises through 02:00 and 08:00, the hours [`StopOverTheHigh`] rests its stop in.
fn bars() -> Vec<Bar> {
    let open = |i: i64| {
        let leg = i % 120;
        1_000.003 + 0.7 * (if leg < 60 { leg } else { 120 - leg }) as f64
    };
    let bar = |i: i64| {
        let (o, c) = (open(i), open(i + 1));
        Bar::ohlcv(i * MINUTE, o, o.max(c) + 0.3, o.min(c) - 0.3, c, 1_000.0)
    };
    (0..BARS).map(bar).collect()
}

/// On every bar of the 02:00 and 08:00 UTC hours while flat, a buy stop of 1 one tick over that bar's high,
/// always under one name; a long held on the 06:15 or 12:15 bar closes at the next open. Like the gallery's
/// files that read a tick, it returns on a bar with none.
struct StopOverTheHigh;

impl Rule for StopOverTheHigh {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        const NAME: &str = "rebuilt";
        let Some(tick) = strategy.tick() else { return };
        let (hour, minute) = clock(bar);
        let held = strategy.position();
        if (hour == 2 || hour == 8) && held == 0.0 {
            strategy.entry(NAME, Long).size(1.0).stop(bar.high + tick);
        }
        if ((hour, minute) == (6, 15) || (hour, minute) == (12, 15)) && held > 0.0 {
            strategy.close(NAME);
        }
    }
}

/// [`StopOverTheHigh`] as the rule a profile's builder is handed.
fn stop_over_the_high(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(StopOverTheHigh)
}

/// The run of `file`'s `rule` over [`bars`] as a product run is resolved, its symbol's kept row stating
/// `tick` (`0.0` states none) beside a lot of 10 and a minimum notional no order of size 1 meets, so a
/// run that read either would round or refuse every order.
fn run(file: &str, rule: fn(&Params) -> Box<dyn Rule + Send>, tick: f64) -> TradingRecord {
    run_over(file, rule, tick, None, "")
}

/// [`run`] over a store that also holds a dated properties record `(observed at, tick)`, its lot and
/// minimums the kept row's (which a run that read them would round or refuse every order by), and with
/// `tail` closing the profile.
fn run_over(
    file: &str,
    rule: fn(&Params) -> Box<dyn Rule + Send>,
    tick: f64,
    record: Option<(i64, f64)>,
    tail: &str,
) -> TradingRecord {
    let store = Arc::new(HistoryStoreMemory::new());
    store.append_bars(VENUE, SYMBOL, "1m", &bars(), None).expect("the bars");
    let grid = |tick_size: f64| SymbolProperties {
        tick_size,
        step_size: 10.0,
        min_qty: 10.0,
        min_notional: 1e12,
        ..Default::default()
    };
    if let Some((at, tick)) = record {
        store
            .append_symbol_properties(VENUE, SYMBOL, &[(at, grid(tick))], None)
            .expect("the record");
    }
    let row = Instrument::new(VENUE, SYMBOL, "BTC", "USDT", Contract::CryptoSpot);
    store.serve_kept_instruments(vike_catalog::spelled(vec![row.with_properties(grid(tick))]));
    let toml = format!(
        "[data]\nkind = \"bar\"\nvenue = \"{VENUE}\"\nsymbols = [\"{SYMBOL}\"]\ninterval = \"1m\"\n\
         from = \"0\"\nto = \"{}\"\n\n[engine]\ncash = 1000000.0\nmargin_mode = \"cross\"\n\n\
         [strategy]\nfile = \"{file}\"\n{tail}",
        BARS * MINUTE
    );
    let mut profile = ProfileBacktest::from_toml_str(&toml).expect("the profile");
    profile.strategy.as_mut().unwrap().builder = Some(StrategyBuilder::new(move |params| {
        let run: Box<dyn Strategy<OrderBrokerSimulator>> =
            Box::new(RuleRun::new(rule(params), SYMBOL).with(RuleRunOptions::OFF));
        Ok(run)
    }));
    profile.read_kept_rows(&*store).expect("the kept rows");
    run_backtest(&profile, store).expect("the run")
}

/// The lines of `record`'s data warnings that name a tick read from the kept row.
fn kept_lines(record: &TradingRecord) -> Vec<&str> {
    record.data_warnings.iter().map(String::as_str).filter(|l| l.contains(KEPT)).collect()
}

/// THE CASE (`zero_trade_strategies.md`'s s_888 and Kelly files): a file that opens its bar with
/// `let Some(tick) = strategy.tick() else { return };` traded nothing in a backtest with no `[risk]` tick and
/// no dated record. With its kept row's tick it rests its stop one tick over a bar's high, on the row's grid,
/// at the size it states (the row's lot of 10 unread), once in each of its two hours. The language reads its
/// own symbol's tick as `""` here (the simulator's list opens with it), so this is the unnamed read.
#[test]
fn a_file_that_returns_without_a_tick_trades_on_the_kept_rows_tick() {
    const FILE: &str = STOP_FILE;
    let rule = stop_over_the_high;
    let none = run(FILE, rule, 0.0);
    assert_eq!(none.n_trades, 0, "no tick anywhere: it returns on every bar, as before");
    assert!(kept_lines(&none).is_empty(), "{:?}", none.data_warnings);

    let kept = run(FILE, rule, 0.01);
    assert_eq!(kept.n_trades, 2, "one long in each window: {:?}", kept.trades);
    for trade in &kept.trades {
        assert_eq!(trade.size, 1.0, "the size it states, not the row's lot: {trade:?}");
        let ticks = trade.entry_price / 0.01;
        assert!((ticks - ticks.round()).abs() < 1e-6, "its stop is on the row's grid: {trade:?}");
    }
    assert_eq!(
        kept_lines(&kept),
        [format!(
            "{SYMBOL}: the instrument grid this run read carried tick 0.01 from the current \
             instrument list (no properties record in force when these UTC days began)"
        )],
        "said once, though read on every bar"
    );
}

/// Whether `price` is a whole number of `tick`s.
fn on_grid(price: f64, tick: f64) -> bool {
    let ticks = price / tick;
    (ticks - ticks.round()).abs() < 1e-6
}

/// The path the profile names for [`StopOverTheHigh`]: a label only, as the builder hands the rule and no file is read.
const STOP_FILE: &str = "kept_tick/stop_over_the_high.rs";

/// THE OWNER'S PRECEDENCE in a run that does not snap: a dated record whose tick differs from the kept row's
/// is the tick the file reads (its stops on the record's 0.05 grid, which the kept row's 0.01 grid misses),
/// the record's lot and minimums unread (the gate and the fills keep what they had), and no kept tick named.
#[test]
fn a_default_run_reads_the_dated_records_tick_over_the_kept_rows() {
    let rule = stop_over_the_high;
    let dated = run_over(STOP_FILE, rule, 0.01, Some((0, 0.05)), "");
    assert!(dated.n_trades > 0, "it trades: {:?}", dated.data_warnings);
    for trade in &dated.trades {
        assert_eq!(trade.size, 1.0, "the size it states, not the record's lot: {trade:?}");
        assert!(on_grid(trade.entry_price, 0.05), "its stop is on the record's grid: {trade:?}");
    }
    assert!(kept_lines(&dated).is_empty(), "{:?}", dated.data_warnings);
    let kept = run(STOP_FILE, rule, 0.01);
    assert!(kept.trades.iter().any(|t| !on_grid(t.entry_price, 0.05)), "{:?}", kept.trades);
}

/// A window before the first dated record (the CI box's binance records start on 2026-11-09) reads the kept row's
/// tick, trades to the same bits as a store with no record, and says once where its tick came from.
#[test]
fn a_window_before_the_first_dated_record_reads_the_kept_rows_tick() {
    let rule = stop_over_the_high;
    let next_day = vike_model::time::MS_PER_DAY;
    let before = run_over(STOP_FILE, rule, 0.01, Some((next_day, 0.05)), "");
    let kept = run(STOP_FILE, rule, 0.01);
    assert_eq!(format!("{:?}", before.trades), format!("{:?}", kept.trades));
    assert_eq!(before.final_equity.to_bits(), kept.final_equity.to_bits());
    assert_eq!(
        kept_lines(&before),
        [format!(
            "{SYMBOL}: the instrument grid this run read carried tick 0.01 from the current \
             instrument list (no properties record in force when these UTC days began)"
        )]
    );
}

/// `[risk]`'s tick wins over the dated record's and the kept row's.
#[test]
fn the_risk_tables_tick_wins_over_the_dated_record() {
    let rule = stop_over_the_high;
    let stated = run_over(STOP_FILE, rule, 0.01, Some((0, 0.05)), "\n[risk]\ntick_size = 0.1\n");
    assert!(stated.n_trades > 0, "it trades: {:?}", stated.data_warnings);
    for trade in &stated.trades {
        assert!(on_grid(trade.entry_price, 0.1), "its stop is on [risk]'s grid: {trade:?}");
    }
    assert!(kept_lines(&stated).is_empty(), "{:?}", stated.data_warnings);
}

/// A file whose orders are all market orders (a signal rule) reads no tick of its own: its trades and its
/// equity are the same bits with the kept row's tick or without it. Whether the language asked the grid at
/// all (a size's lot) decides whether the run names the kept tick, and then it names it once.
#[test]
fn a_file_whose_orders_state_no_level_trades_the_same_with_the_kept_rows_tick_or_without() {
    const FILE: &str = "trend/rsi_14_cross_50.rs";
    let rule = gallery::rsi_14_cross_50::rule;
    let (without, with) = (run(FILE, rule, 0.0), run(FILE, rule, 0.01));
    assert!(with.n_trades > 2, "it flips on the triangle: {}", with.n_trades);
    assert_eq!(format!("{:?}", with.trades), format!("{:?}", without.trades));
    let bits = |r: &TradingRecord| r.equity_curve.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&with), bits(&without));
    assert_eq!(with.final_equity.to_bits(), without.final_equity.to_bits());
    assert!(kept_lines(&with).len() <= 1, "{:?}", with.data_warnings);
    assert!(kept_lines(&without).is_empty());
}
