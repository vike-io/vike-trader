//! **A backtest split-adjusts a share's series from the operator's split rows** (owner 2026-11-16, decision 0289), as
//! TradingView shows it: every bar before a split has its prices divided by the ratio and its volume multiplied by it,
//! the stored bars stay as they are, the report says `split-adjusted: AAPLUSUSD 2020-08-31 ×4`, and a series with no
//! split is read exactly as before.
//!
//! The worked numbers are Dukascopy's AAPL CFD across Apple's 4-for-1 split (IMP-D2B): 499.19 on 2020-08-28 and
//! 127.64 on 2020-08-31, the first day at the new price.

use super::*;
use crate::run::KeptRows;
use vike_model::StockSplit;
use vike_store::HistoryStoreDataFusion;

/// 2020-08-27, 2020-08-28 (before the split), 2020-08-31 and 2020-09-01 (after), 00:00 UTC.
const DAYS: [i64; 4] = [1_598_486_400_000, 1_598_572_800_000, 1_598_832_000_000, 1_598_918_400_000];
/// The closes on those days, as the CFD's store holds them: not adjusted.
const CLOSES: [f64; 4] = [500.04, 499.19, 129.04, 134.18];

fn split(symbol: &str, date: &str, ratio: f64) -> StockSplit {
    StockSplit::new("dukascopy", symbol, date, ratio).expect("a split")
}

fn kept(splits: Vec<StockSplit>) -> KeptRows {
    KeptRows { splits, ..KeptRows::default() }
}

/// The four stored daily bars, each flat at its close with a volume of 100.
fn stored() -> Vec<Bar> {
    DAYS.iter().zip(CLOSES).map(|(ts, c)| Bar::ohlcv(*ts, c, c, c, c, 100.0)).collect()
}

fn store() -> (tempfile::TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    store.append_bars("dukascopy", "AAPLUSUSD", "1d", &stored(), None).unwrap();
    (dir, Arc::new(store))
}

fn profile(kind: &str, from: i64, splits: Vec<StockSplit>) -> ProfileBacktest {
    let mut profile = ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "dukascopy"
symbols = ["AAPLUSUSD"]
kind = "{kind}"
interval = "1d"
from = "{from}"
to = "{}"

[engine]
cash = 100000.0

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 10.0
symbol = "AAPLUSUSD"
"#,
        DAYS[3]
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in);
    profile.kept = kept(splits);
    profile
}

/// The bars the run reads, and its data warnings.
fn loaded(profile: &ProfileBacktest) -> (Vec<Bar>, Vec<String>) {
    let (_dir, store) = store();
    let mut warnings = Vec::new();
    let (mut series, _funding) = load_profile_bars(profile, &store, None, &mut warnings).unwrap();
    (series.remove(0).1, warnings)
}

#[test]
fn a_series_is_adjusted_before_its_split_and_the_report_says_so() {
    let (bars, warnings) =
        loaded(&profile("bar", DAYS[0], vec![split("AAPLUSUSD", "2020-08-31", 4.0)]));
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    assert_eq!(closes, [500.04 / 4.0, 499.19 / 4.0, 129.04, 134.18]);
    let volumes: Vec<f64> = bars.iter().map(|b| b.volume).collect();
    assert_eq!(volumes, [400.0, 400.0, 100.0, 100.0]);
    assert!(bars.iter().all(|b| b.open == b.close && b.high == b.close && b.low == b.close));
    assert_eq!(warnings, ["split-adjusted: AAPLUSUSD 2020-08-31 ×4"]);
}

#[test]
fn a_series_with_no_split_is_read_exactly_as_stored() {
    for splits in [Vec::new(), vec![split("SPYUSUSD", "2020-08-31", 4.0)]] {
        let (bars, warnings) = loaded(&profile("bar", DAYS[0], splits));
        assert_eq!(bars, stored(), "no split of this series, no change");
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}

#[test]
fn a_split_before_the_window_moves_no_bar_and_says_nothing() {
    let (bars, warnings) =
        loaded(&profile("bar", DAYS[2], vec![split("AAPLUSUSD", "2020-08-31", 4.0)]));
    assert_eq!(bars, stored()[2..]);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn later_splits_compound_and_a_reverse_split_multiplies() {
    let rows =
        kept(vec![split("AAPLUSUSD", "2020-08-28", 7.0), split("AAPLUSUSD", "2020-08-31", 4.0)]);
    let mut bars = stored();
    bars[0].bid = Some(500.0);
    let lines = rows.adjust_for_splits("dukascopy", "AAPLUSUSD", &mut bars).unwrap();
    assert_eq!(bars[0].close, 500.04 / 28.0);
    assert_eq!(bars[0].bid, Some(500.0 / 28.0));
    assert_eq!(bars[0].volume, 2800.0);
    assert_eq!(bars[1].close, 499.19 / 4.0, "the 7-for-1 is dated this day: only the later split");
    assert_eq!(
        lines,
        ["split-adjusted: AAPLUSUSD 2020-08-28 ×7", "split-adjusted: AAPLUSUSD 2020-08-31 ×4"]
    );

    let reverse = kept(vec![split("AAPLUSUSD", "2020-08-31", 0.1)]);
    let mut bars = stored();
    reverse.adjust_for_splits("dukascopy", "AAPLUSUSD", &mut bars).unwrap();
    assert_eq!(bars[0].close, 500.04 / 0.1);
    assert_eq!(bars[0].volume, 100.0 * 0.1);
}

#[test]
fn a_run_books_the_adjusted_prices_and_reports_the_split() {
    let (_dir, store) = store();
    let adjusted = run_backtest(
        &profile("bar", DAYS[0], vec![split("AAPLUSUSD", "2020-08-31", 4.0)]),
        Arc::clone(&store),
    )
    .unwrap();
    assert!(adjusted.data_warnings.iter().any(|l| l == "split-adjusted: AAPLUSUSD 2020-08-31 ×4"));
    assert!(adjusted.final_equity > 100_000.0, "10 shares bought near 125 and marked at 134.18");
    let raw = run_backtest(&profile("bar", DAYS[0], Vec::new()), store).unwrap();
    assert!(
        raw.final_equity < 100_000.0,
        "unadjusted, 10 shares bought near 500 and marked at 134.18"
    );
    assert!(!raw.data_warnings.iter().any(|l| l.starts_with("split-adjusted")));
}

#[test]
fn a_tick_run_whose_window_a_split_falls_in_is_refused_in_words() {
    let (_dir, store) = store();
    let err =
        run_backtest(&profile("tick", DAYS[0], vec![split("AAPLUSUSD", "2020-08-31", 4.0)]), store)
            .expect_err("the tick lane adjusts no price");
    assert!(err.message().contains("tick lane adjusts no price"), "{}", err.message());
}

/// **The rows a refresh reads from Yahoo make the two measured CFD series continuous** (decision 0289's amendment of
/// 2026-11-16): Dukascopy's AAPL closes 499.19 on 2020-08-28 and opens 127.64 on 2020-08-31, its NFLX closes 1113.655
/// on 2025-11-14 and opens 110.78 on 2025-11-17; Yahoo lists AAPL 2020-08-31 ×4 and NFLX 2025-11-17 ×10
/// (`vike_backfill::testing`'s answers). Adjusted, each step across its split is an ordinary day's move.
#[test]
fn the_cfd_series_are_continuous_across_yahoos_splits() {
    const NFLX_DAYS: [i64; 2] = [1_763_078_400_000, 1_763_337_600_000];
    for (symbol, days, before, after, date, ratio) in [
        ("AAPLUSUSD", [DAYS[1], DAYS[2]], 499.19, 127.64, "2020-08-31", 4.0),
        ("NFLXUSUSD", NFLX_DAYS, 1113.655, 110.78, "2025-11-17", 10.0),
    ] {
        let mut bars = vec![
            Bar::ohlcv(days[0], before, before, before, before, 100.0),
            Bar::ohlcv(days[1], after, after, after, after, 100.0),
        ];
        kept(vec![split(symbol, date, ratio)])
            .adjust_for_splits("dukascopy", symbol, &mut bars)
            .unwrap();
        let step = bars[1].open / bars[0].close - 1.0;
        assert!(step.abs() < 0.05, "{symbol}: {:.2}% across the split", step * 100.0);
    }
}

#[test]
fn a_row_that_is_no_split_refuses_the_run() {
    let bad = StockSplit {
        venue: "dukascopy".into(),
        symbol: "AAPLUSUSD".into(),
        date: "2020-02-30".into(),
        ratio: 4.0,
    };
    let rows = kept(vec![bad]);
    let err =
        rows.adjust_for_splits("dukascopy", "AAPLUSUSD", &mut stored()).expect_err("no split");
    assert!(err.message().contains("is no split"), "{}", err.message());
}
