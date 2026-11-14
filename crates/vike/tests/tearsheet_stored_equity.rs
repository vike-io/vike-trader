//! Store-backed gate for `default_equity_curve` (decision 0243): the stored `portfolio/TOTAL` equity
//! series is plotted when the journal records one currency, and NOT when it records a ledger that is
//! not the dollar's, because the series is one currency's and states none (the sheet would read a
//! euro curve as dollars beside dollar trades). And `sheet_from_store`, the `--datahub` sheet, reads
//! that series one close per UTC day like the journal's sheet.
//!
//! Seeds the shared `vike_data::HistoryStoreMemory` double through `append_equity`, keyed on
//! `("portfolio", "TOTAL")` exactly as the code under test reads it.
//!
//! ⚠ Whole-file gated on the `tearsheet` feature, like `tests/mtm_store.rs`.
#![cfg(feature = "tearsheet")]

use vike_analytics::reconstruct_trades;
use vike_backend::tearsheet_cli::store::default_equity_curve;
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_model::EquitySample;
use vike_model::events::{FillEvent, PositionSide, TradeId};

fn fill(side: i32, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("t", ts),
        order_client_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: 1.0,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: ts,
        mark_price: Some(px),
        position_side: PositionSide::Both,
    }
}

/// A store holding a three-point `portfolio/TOTAL` series (5000, 5100, 5200) that no realized-only
/// curve over [`fills`] can equal.
fn store_with_total() -> HistoryStoreMemory {
    let store = HistoryStoreMemory::new();
    let rows: Vec<EquitySample> = [(1, 5_000.0), (2, 5_100.0), (3, 5_200.0)]
        .into_iter()
        .map(|(ts, equity)| EquitySample {
            timestamp: ts,
            venue: "TOTAL".into(),
            equity,
            realized: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
        })
        .collect();
    store
        .append_equity("portfolio", "TOTAL", &rows, None)
        .expect("HistoryStoreMemory accepts the seed");
    store
}

/// One round trip, +10: the realized-only curve is `[seed, seed + 10]`.
fn fills() -> Vec<FillEvent> {
    vec![fill(1, 100.0, 10), fill(-1, 110.0, 20)]
}

#[test]
fn a_dollar_journal_plots_the_stored_total_series() {
    let trades = reconstruct_trades(&fills());
    let (eq, ts) = default_equity_curve(&store_with_total(), &trades, 1_000.0, None);
    assert_eq!(eq, vec![5_000.0, 5_100.0, 5_200.0], "the stored series, unchanged");
    assert_eq!(ts, vec![1, 2, 3]);
}

#[test]
fn a_journal_with_a_non_dollar_ledger_does_not_plot_the_stored_total_series() {
    let trades = reconstruct_trades(&fills());
    let (eq, ts) = default_equity_curve(&store_with_total(), &trades, 1_000.0, Some("fxcm EUR"));
    assert_eq!(
        eq,
        vec![1_000.0, 1_010.0],
        "the realized-only curve of the dollar fills: the stored TOTAL may be euros"
    );
    assert_eq!(ts, vec![20, 20], "the seed point is stamped with the first exit");
}

#[test]
fn a_dollar_journal_with_no_stored_series_falls_back_as_before() {
    let trades = reconstruct_trades(&fills());
    let (eq, _) = default_equity_curve(&HistoryStoreMemory::new(), &trades, 1_000.0, None);
    assert_eq!(eq, vec![1_000.0, 1_010.0]);
}

/// **The `--datahub` sheet reads its curve one close per UTC day, like the journal's** (2026-11-09,
/// `vike_analytics::tearsheet_from_curve`): a `portfolio/TOTAL` series sampled every 4 hours over six
/// UTC days, the fourth empty, is read as the opening plus six day closes (the empty day carries the
/// day before) and annualised by calendar days, 365 for a binance book. Before, the sheet annualised
/// all 30 samples at 252, as if each were a trading day.
#[test]
fn the_datahub_sheet_reads_the_stored_series_one_close_per_utc_day() {
    use vike_analytics::report::periods_per_year_for_days;
    use vike_analytics::{Tearsheet, metrics};
    use vike_backend::tearsheet_cli::sheet_from_store;

    const DAY: i64 = 86_400_000;
    const FOUR_HOURS: i64 = 4 * 3_600_000;
    let days: [&[f64]; 6] = [
        &[10_000.0, 10_040.0, 9_960.0, 10_020.0, 10_080.0, 10_060.0],
        &[10_100.0, 9_990.0, 10_130.0, 10_150.0, 10_120.0, 10_170.0],
        &[10_140.0, 10_090.0, 10_200.0, 10_180.0, 10_230.0, 10_110.0],
        &[],
        &[10_190.0, 10_260.0, 10_240.0, 10_300.0, 10_280.0, 10_320.0],
        &[10_290.0, 10_350.0, 10_330.0, 10_310.0, 10_380.0, 10_400.0],
    ];
    let rows: Vec<EquitySample> = days
        .iter()
        .enumerate()
        .flat_map(|(d, values)| {
            values.iter().enumerate().map(move |(k, &equity)| EquitySample {
                timestamp: d as i64 * DAY + k as i64 * FOUR_HOURS,
                venue: "TOTAL".into(),
                equity,
                realized: 0.0,
                unrealized: 0.0,
                missing_prices: 0,
            })
        })
        .collect();
    let store = HistoryStoreMemory::new();
    store
        .append_equity("portfolio", "TOTAL", &rows, None)
        .expect("HistoryStoreMemory accepts the seed");

    let periods_per_year =
        periods_per_year_for_days(vike_catalog::trades_every_day("binance", None));
    assert_eq!(periods_per_year, 365.0, "a binance book trades every day");
    let (sheet, eq, ts, runtime) = sheet_from_store(
        &store,
        &fills(),
        10_000.0,
        periods_per_year,
        "binance",
        "1m",
        false,
        None,
    );
    assert!(runtime.is_none(), "no --mtm, no runtime section");
    assert_eq!(eq, vec![10_000.0, 10_060.0, 10_170.0, 10_110.0, 10_110.0, 10_320.0, 10_400.0]);
    assert_eq!(ts, [0].into_iter().chain((1..=6).map(|d| d * DAY - 1)).collect::<Vec<i64>>());
    assert_eq!(sheet.metric("sharpe"), Some(metrics::sharpe(&eq, 365.0)));

    // Before: the 30 stored samples as they were read, at 252 (the old last line, verbatim).
    let raw: Vec<f64> = rows.iter().map(|r| r.equity).collect();
    let raw_timestamp: Vec<i64> = rows.iter().map(|r| r.timestamp).collect();
    let old =
        Tearsheet::from_result_parts(None, reconstruct_trades(&fills()), raw, raw_timestamp, 252.0);
    let near = |got: Option<f64>, want: f64| got.is_some_and(|g| (g / want - 1.0).abs() < 1e-9);
    // The drawdown halves: the 10,230 intraday peak of day 2 is no close, only 10,170 -> 10,110 is.
    for (id, before, after) in [
        ("sharpe", 3.5504201866544665, 13.725218695329009),
        ("sortino", 5.813292585167889, 47.74036655786248),
        ("cagr", 0.4060923484514203, 9.869130605335993),
        ("calmar", 34.61937270548358, 1672.8176376044507),
        ("max_drawdown", 0.011730205278592375, 0.0058997050147492625),
    ] {
        assert!(near(old.metric(id), before), "{id} before: {:?}", old.metric(id));
        assert!(near(sheet.metric(id), after), "{id} after: {:?}", sheet.metric(id));
    }
}
