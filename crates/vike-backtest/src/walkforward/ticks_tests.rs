use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_analytics::TradingRecordSource;
use vike_analytics::results::TradingResults;
use vike_data::{HistoryStoreError, TimestampRange};
use vike_marketdata::TickTrade;
use vike_model::params::ParamValue;

use crate::run::report::periods_per_year;
use crate::run::run_backtest;
use crate::search::paramscan::profile_with_overrides;
use crate::walkforward::combinatorial::run_walkforward_combinatorial_profile;
use crate::walkforward::runner::{run_walkforward, run_walkforward_optimized};

/// The seed step of every walk below.
const MINUTE: i64 = 60_000;

/// The last millisecond of the two days every walk below reads.
const TO: i64 = 2 * 86_400_000 - 1;

/// Two days of prints for any series, as a store answers them (inside the range asked, both ends inclusive),
/// counting the trade scans: a print every 5 s on a 0.1 tick around 108,000, a sudden 30–150 move every 300 of
/// them, and a SECOND print on every minute's first millisecond, so each cut on the minute meets two events
/// sharing a stamp.
struct WalkTape {
    trades: Vec<TickTrade>,
    scans: AtomicUsize,
}

impl WalkTape {
    fn two_days() -> Arc<WalkTape> {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 33
        };
        let mut price = 108_000.0;
        let mut trades = Vec::new();
        for i in 0..(TO + 1) / 5_000 {
            price += (next() % 3) as f64 * 0.1 - 0.1;
            if i % 300 == 299 {
                let jump = 30.0 + (next() % 121) as f64;
                price += if next() % 2 == 0 { jump } else { -jump };
            }
            let timestamp = i * 5_000;
            let print = |price: f64| TickTrade {
                timestamp,
                local_timestamp: 0,
                price,
                size: 0.01,
                is_buyer_maker: false,
                symbol: Default::default(),
            };
            trades.push(print(price));
            if timestamp % MINUTE == 0 {
                trades.push(print(price + 0.1));
            }
        }
        Arc::new(WalkTape { trades, scans: AtomicUsize::new(0) })
    }

    fn scans(&self) -> usize {
        self.scans.load(Ordering::SeqCst)
    }
}

impl HistoryStore for WalkTape {
    fn scan_trades(
        &self,
        _venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        let inside = |t: &&TickTrade| {
            range.start.is_none_or(|s| t.timestamp >= s)
                && range.end.is_none_or(|e| t.timestamp <= e)
        };
        let named = |t: &TickTrade| TickTrade { symbol: symbol.into(), ..t.clone() };
        Ok(self.trades.iter().filter(inside).map(named).collect())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_quotes, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// [`WalkTape`]'s prints where ONE `properties_as_of` of the store's life fails (`failing`, counted from 0) and
/// every other finds no record: under `snap_to_properties` the walk's pre-flight asks once, then each run asks.
struct OneReadFails {
    tape: Arc<WalkTape>,
    reads: AtomicUsize,
    failing: usize,
}

impl HistoryStore for OneReadFails {
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.tape.scan_trades(venue, symbol, range)
    }
    fn properties_as_of(
        &self,
        _venue: &str,
        _symbol: &str,
        _ts: i64,
    ) -> Result<Option<vike_model::SymbolProperties>, HistoryStoreError> {
        if self.reads.fetch_add(1, Ordering::SeqCst) == self.failing {
            return Err(HistoryStoreError::Query("the store went away".to_string()));
        }
        Ok(None)
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_quotes, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// Momentum over the two days, seeded on the minute, with `tail` (a `[paramscan]` grid, a `[walkforward]` table)
/// after its params.
fn walk_text(tail: &str) -> String {
    format!(
        r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "tick"
from = "0"
to = "{TO}"

[engine]
cash = 1000000.0
seed_bar_interval_ms = {MINUTE}

[strategy]
file = "trend/momentum.rs"
[strategy.params]
qty = 0.05
sl = 450.0
time_limit_ms = 3600000
cooldown_ms = 300000

{tail}
"#
    )
}

/// [`walk_text`] loaded.
fn profile_of(text: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(text)
        .expect("the walk profile loads")
        .with_stand_in(vike_strategy_lab::stand_in)
}

fn walk_profile(tail: &str) -> ProfileBacktest {
    profile_of(&walk_text(tail))
}

const GRID: &str = "[paramscan]\nthreshold = [40.0, 80.0]\ntp = [300.0, 600.0]\n";

/// `base` at `chosen` over seed bars `steps` alone: a plain tick backtest of that time range (computed here, not
/// by the walk), its record labelled as the walk labels a step, and its results as a step carries them.
fn alone(
    base: &ProfileBacktest,
    store: &Arc<WalkTape>,
    steps: (usize, usize),
    chosen: &Option<Vec<(String, ParamValue)>>,
) -> TradingResults {
    let (mut profile, _) = profile_with_overrides(base, chosen.clone().unwrap_or_default());
    profile.data.from = (steps.0 as i64 * MINUTE).to_string();
    profile.data.to = (steps.1 as i64 * MINUTE - 1).min(TO).to_string();
    profile.walkforward = None;
    let shared: Arc<dyn HistoryStore + Send + Sync> = store.clone();
    let mut record = run_backtest(&profile, shared).expect("the window runs alone");
    record.source = TradingRecordSource::WalkforwardStep { window: steps, params: chosen.clone() };
    TradingResults::from_record(None, &record, periods_per_year(base))
}

/// Every step of `report` against its window run alone, digest for digest (`results` serialized whole), and the
/// number of steps whose return moved.
fn each_step_is_its_window_alone(
    base: &ProfileBacktest,
    store: &Arc<WalkTape>,
    report: &WalkforwardResults,
    compact: bool,
) -> usize {
    let mut moved = 0;
    for step in &report.steps {
        let mut lone = alone(base, store, step.test_range, &step.chosen_params);
        if compact {
            lone.extended = None;
            lone.closed_trades = None;
        }
        let walked = serde_json::to_string(&step.results).expect("serializes");
        assert_eq!(
            walked,
            serde_json::to_string(&lone).expect("serializes"),
            "{:?}",
            step.test_range
        );
        moved += usize::from(step.results.total_return.abs() > 0.0);
    }
    moved
}

/// The stitch of windows whose curves are kept by time (the tick lane's, at the seed step) is kept by time too:
/// `overall` says its step, annualizes its Sharpe at it as every step does, and reads a drawdown no shallower than
/// any window's whole path.
fn overall_is_kept_by_time(report: &WalkforwardResults) {
    let at_the_step = vike_analytics::results::periods_per_year_for_step_ms(MINUTE);
    let sharpe = vike_analytics::metrics::sharpe(&report.equity_curve, at_the_step);
    assert_eq!(report.overall.returns_interval_ms, Some(MINUTE));
    assert_eq!(report.overall.sharpe.to_bits(), sharpe.to_bits(), "the step's annualization");
    for step in &report.steps {
        assert_eq!(step.results.returns_interval_ms, Some(MINUTE));
        let deepest = (report.overall.max_drawdown, step.results.max_drawdown);
        assert!(deepest.0 >= deepest.1, "{:?}: {deepest:?}", step.test_range);
    }
}

/// **The fixed tick walk reads its tape ONCE, and each window is its time range run alone**, digest for digest:
/// `n_splits` cuts the two days on the minute, through prints that share a stamp.
#[test]
fn a_fixed_tick_walk_prepares_one_tape_and_each_window_is_its_range_alone() {
    let tape = WalkTape::two_days();
    let base = walk_profile("[walkforward]\nn_splits = 3\n");
    let report = run_walkforward(&base, tape.clone()).expect("the walk runs");
    assert_eq!(tape.scans(), 1, "one tape prepared for the whole walk");
    assert_eq!(report.steps.len(), 3);
    assert!(each_step_is_its_window_alone(&base, &tape, &report, false) > 0, "the windows trade");
    overall_is_kept_by_time(&report);
    each_step_dates_its_window(&report, true);
}

/// Each step's window dates are its seed bars' opens on the grid (the walk starts at 0 on the minute): the first
/// and last bar of its test half, and of its train half when it has one.
fn each_step_dates_its_window(report: &WalkforwardResults, trains: bool) {
    for step in &report.steps {
        let times = step.window_times.expect("a tick window carries its dates");
        let (start, end) = step.test_range;
        let open = |i: usize| i as i64 * MINUTE;
        assert_eq!((times.test_start, times.test_end), (open(start), open(end - 1)));
        assert_eq!(times.train_start.is_some(), trains, "{:?}", step.test_range);
        if let (Some(from), Some(to)) = (times.train_start, times.train_end) {
            assert!(from <= to && to < times.test_start, "{times:?}");
        }
    }
}

/// **A searched, rolling tick walk over durations**: each window's grid is scored on its training range and its
/// winner traded on its validation range, every run a cut of the one tape, each step its range alone at the
/// point it chose.
#[test]
fn a_searched_rolling_tick_walk_trades_each_winner_as_its_range_alone() {
    let tape = WalkTape::two_days();
    let walk = "[walkforward]\ntrain = \"6h\"\ntest = \"6h\"\nmode = \"rolling\"\nsearch = \"paramscan\"\n";
    let base = walk_profile(&format!("{GRID}\n{walk}"));
    let report = run_walkforward_optimized(&base, tape.clone()).expect("the walk runs");
    assert_eq!(tape.scans(), 1, "one tape prepared for the whole walk");
    assert!(report.steps.len() > 2, "{} windows", report.steps.len());
    assert!(report.steps.iter().all(|s| s.chosen_params.is_some()), "every window chose");
    assert!(each_step_is_its_window_alone(&base, &tape, &report, false) > 0, "the windows trade");
    overall_is_kept_by_time(&report);
    each_step_dates_its_window(&report, true);
}

/// **A candidate whose read fails refuses the walk**, searched or cut combinatorially: dropped, it would leave a
/// window's winner chosen from fewer points than the grid named, in a report that says nothing of it (the bar
/// lane's rule). The read that fails is the one after the walk's pre-flight, a run of the first window's search;
/// the same walks over a store whose every read answers run.
#[test]
fn a_tick_walk_whose_candidate_cannot_read_is_refused() {
    let snap = format!("seed_bar_interval_ms = {MINUTE}\nsnap_to_properties = true");
    let searched = "[walkforward]\ntrain = \"6h\"\ntest = \"6h\"\nmode = \"rolling\"\nsearch = \"paramscan\"\n";
    let cut = "[walkforward]\ngroups = 4\ntest_groups = 2\nsearch = \"paramscan\"\n";
    for walk in [searched, cut] {
        let text = walk_text(&format!("{GRID}\n{walk}"));
        let base =
            profile_of(&text.replacen(&format!("seed_bar_interval_ms = {MINUTE}"), &snap, 1));
        let run = |failing: usize| -> Result<(), HarnessError> {
            let reads = AtomicUsize::new(0);
            let store = Arc::new(OneReadFails { tape: WalkTape::two_days(), reads, failing });
            match base.walkforward.as_ref().and_then(|w| w.groups) {
                Some(_) => run_walkforward_combinatorial_profile(&base, store, None).map(drop),
                None => run_walkforward_optimized(&base, store).map(drop),
            }
        };
        run(usize::MAX).expect("the walk runs when every read answers");
        let why = run(1).expect_err("one candidate's failed read refuses the walk");
        assert!(why.to_string().contains("could not read its data"), "{walk}: {why}");
    }
}

/// **The combinatorial tick walk**: every group of every path is that group's range alone at the point its split
/// chose, over one tape.
#[test]
fn a_combinatorial_tick_walk_trades_each_group_as_its_range_alone() {
    let tape = WalkTape::two_days();
    let walk = "[walkforward]\ngroups = 4\ntest_groups = 2\nsearch = \"paramscan\"\n";
    let base = walk_profile(&format!("{GRID}\n{walk}"));
    let report = run_walkforward_combinatorial_profile(&base, tape.clone(), None).expect("it runs");
    assert_eq!(tape.scans(), 1, "one tape prepared for the whole walk");
    assert_eq!(report.paths.len(), 3, "C(3, 1) paths");
    for path in &report.paths {
        each_step_is_its_window_alone(&base, &tape, path, true);
        each_step_dates_its_window(path, false);
    }
}

/// What a tick walk cannot cut is refused in words before its tape is read.
#[test]
fn a_tick_walk_it_cannot_cut_is_refused_in_words_before_any_read() {
    let tape = WalkTape::two_days();
    let text = walk_text("[walkforward]\nn_splits = 3\n");
    let reference =
        "[[data.reference]]\nvenue = \"bybit\"\nsymbol = \"BTCUSDT\"\nkind = \"quote\"\n\n[engine]";
    let cases = [
        (format!("seed_bar_interval_ms = {MINUTE}"), String::new(), "seed_bar_interval_ms"),
        (format!("to = \"{TO}\""), "to = \"\"".to_string(), "data.from and data.to"),
        ("[engine]".to_string(), reference.to_string(), "heard streams"),
    ];
    for (written, instead, needle) in cases {
        let base = profile_of(&text.replacen(&written, &instead, 1));
        let err = run_walkforward(&base, tape.clone()).expect_err("refused");
        assert!(err.to_string().contains(needle), "{needle}: {err}");
    }
    assert_eq!(tape.scans(), 0, "nothing was read");
}
