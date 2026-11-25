//! Composing a report: `metrics::` parity, the `Display` table, zero-trade, periods-per-year.

use super::*;
use vike_model::OrderDrop;

#[test]
fn matches_direct_metrics_calls() {
    let r = sample_result();
    let report = TradingResults::from_record(Some("demo".to_string()), &r, 252.0);

    assert_eq!(report.name.as_deref(), Some("demo"));
    assert_eq!(report.final_equity, r.final_equity);
    assert_eq!(report.n_trades, r.n_trades);
    assert_eq!(report.per_symbol_pnl, r.per_symbol_pnl);
    assert_eq!(report.total_return, metrics::total_return(&r.equity_curve));
    assert_eq!(report.max_drawdown, metrics::max_drawdown(&r.equity_curve));
    assert_eq!(report.win_rate, metrics::win_rate(&r.trades));
    assert_eq!(report.sharpe, metrics::sharpe(&r.equity_curve, 252.0));
    assert_eq!(report.profit_factor, metrics::profit_factor(&r.trades));

    // Sanity on the composed values themselves, not just that they match — a report that
    // merely "matches metrics" by both being wrong the same way would still pass the
    // assertions above.
    assert_eq!(report.win_rate, 0.5); // 1 win / 2 trades
    assert!((report.total_return - 0.005).abs() < 1e-12); // 1005/1000 - 1
    assert!((report.max_drawdown - (1010.0 - 990.0) / 1010.0).abs() < 1e-12);
}

#[test]
fn display_is_a_compact_human_table() {
    let r = sample_result();
    let report = TradingResults::from_record(None, &r, 252.0);
    let s = report.to_string();
    assert!(s.contains("(unnamed)"));
    assert!(s.contains("final_equity:"));
    assert!(s.contains("win_rate:"));
    assert!(s.contains("sharpe:"));
    assert!(s.contains("max_drawdown:"));
    assert!(s.contains("BTCUSDT"));
}

/// **A run whose equity crosses zero is INVALID** (owner 2026-11-16): the report names the first day the
/// equity reached 0, its table prints no return, ratio or percentage, and its rendered rows open with the
/// words and keep only money and counts. A run that stays above zero is not marked and keeps every row.
#[test]
fn a_run_whose_equity_crosses_zero_is_marked_invalid() {
    const DAY_MS: i64 = 86_400_000;
    let r = TradingRecord {
        equity_curve: vec![1000.0, 400.0, -50.0, 20.0],
        final_equity: 20.0,
        // 2017-08-17, the day after, ...
        equity_timestamp: (0..4).map(|d| 1_502_928_000_000 + d * DAY_MS).collect(),
        ..sample_result()
    };
    let report = TradingResults::from_record(None, &r, 252.0);
    let why = "equity reached 0 on 2017-08-19: metrics are not meaningful";
    assert_eq!(report.invalid.as_deref(), Some(why));

    let table = report.to_string();
    assert!(table.contains(&format!("INVALID:       {why}")), "{table}");
    for gone in ["sharpe:", "total_return:", "win_rate:", "max_drawdown:"] {
        assert!(!table.contains(gone), "{gone} printed for an invalid run:\n{table}");
    }
    assert!(table.contains("final_equity:") && table.contains("n_trades:"), "{table}");

    let rows = report.rendered_rows();
    assert_eq!(rows[0], (crate::INVALID_LABEL, why.to_string()));
    let ids: Vec<&str> = rows.iter().map(|(id, _)| *id).collect();
    assert!(!ids.contains(&"sharpe") && !ids.contains(&"cagr"), "{ids:?}");
    assert!(ids.contains(&"final_equity") && ids.contains(&"n_trades"), "{ids:?}");

    let healthy = TradingResults::from_record(None, &sample_result(), 252.0);
    assert_eq!(healthy.invalid, None);
    let rows = healthy.rendered_rows();
    assert!(
        rows[0].0 != crate::INVALID_LABEL && rows.iter().any(|(id, _)| *id == "sharpe"),
        "{rows:?}"
    );

    // the HTML tearsheet of the invalid run: the line, money and counts, no ratio or percentage anywhere
    let sheet = crate::TradingResultsTable::from_report(&report);
    let html = crate::render_html(&sheet, &[], &r.equity_curve, &r.equity_timestamp);
    assert!(html.contains(&format!("<tr><th>INVALID</th><td>{why}</td></tr>")), "{html}");
    assert!(html.contains("<th>final_equity</th>") && html.contains("<th>n_trades</th>"));
    for gone in
        ["<th>sharpe</th>", "<th>total_return</th>", "<th>win_rate</th>", "Underwater", "Monthly"]
    {
        assert!(!html.contains(gone), "{gone} in the HTML of an invalid run");
    }
}

/// OFF / byte-identical path: a run WITH trades is never diagnosed, so its report grows no
/// `zero_trade` field in JSON and its `Display` is the unchanged metrics table.
#[test]
fn zero_trade_is_absent_and_output_unchanged_for_a_run_with_trades() {
    let r = sample_result(); // n_trades = 2, moving equity curve
    let report = TradingResults::from_record(Some("demo".to_string()), &r, 252.0);
    assert!(report.zero_trade.is_none(), "a run with trades is never diagnosed");

    // JSON: no `zero_trade` key (skip_serializing_if) -> byte-identical to before the field.
    let json = serde_json::to_string(&report).unwrap();
    assert!(
        !json.contains("zero_trade"),
        "a normal-run JSON must not grow a zero_trade field: {json}"
    );

    // Display: the full metrics table (the diagnosis branch is skipped).
    let s = report.to_string();
    assert!(s.contains("win_rate:"));
    assert!(s.contains("sharpe:"));
    assert!(s.contains("max_drawdown:"));
    assert!(!s.contains("probable cause"));
}

/// A contrived zero-trade / flat-equity run surfaces the correct ranked cause: its `Display`
/// prints the diagnosis INSTEAD OF the all-zero metrics rows, and its JSON carries `zero_trade`.
#[test]
fn zero_trade_diagnosis_replaces_the_table_for_a_flat_zero_trade_run() {
    let r = TradingRecord {
        n_trades: 0,
        equity_curve: vec![1000.0, 1000.0, 1000.0],
        final_equity: 1000.0,
        source: crate::record::TradingRecordSource::Backtest(
            crate::record::TradingRecordSimulator {
                dropped: vec![
                    OrderDrop {
                        symbol: "BTCUSDT".to_string(),
                        reason: "insufficient-margin".to_string(),
                        size: 1.0,
                        weight: 0.0,
                    },
                    OrderDrop {
                        symbol: "BTCUSDT".to_string(),
                        reason: "insufficient-margin".to_string(),
                        size: 2.0,
                        weight: 0.0,
                    },
                ],
                ..Default::default()
            },
        ),
        ..Default::default()
    };
    let report = TradingResults::from_record(None, &r, 252.0);

    let zt = report.zero_trade.as_ref().expect("a flat zero-trade run must be diagnosed");
    assert_eq!(zt.causes[0].code, "orders-denied");

    // Display: diagnosis present, the metric rows (win_rate/sharpe) replaced by it.
    let s = report.to_string();
    assert!(s.contains("probable cause"));
    assert!(s.contains("rejected before filling"));
    assert!(!s.contains("win_rate:"), "the bare metrics table must be replaced: {s}");
    assert!(!s.contains("sharpe:"));

    // JSON now carries the diagnosis.
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("zero_trade"), "a zero-trade run's JSON carries the diagnosis: {json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["zero_trade"]["causes"][0]["code"], "orders-denied");
}

/// The daily anchor is preserved BIT-for-bit — the whole reason the intraday fix could ship
/// without moving a single existing daily report.
#[test]
fn the_daily_anchor_is_preserved_exactly() {
    assert_eq!(periods_per_year_for_interval("1d"), DAILY_PERIODS_PER_YEAR);
}

/// The report's default seed is the owner's 10 000 (2026-11-21): every door that defaults one reads it, so
/// a new number here is a decision about every published `total_return`, not an edit.
#[test]
fn the_default_report_seed_is_ten_thousand() {
    assert_eq!(DEFAULT_REPORT_SEED, 10_000.0);
}

/// The two values the walk-forward divergence was measured at: `1h` is the interval the
/// Studio door reported `sqrt(24)` low, `1m` the one the CI roundtrip fixture uses and the
/// original intraday bug understated by `sqrt(1440)`.
#[test]
fn intraday_intervals_scale_off_the_daily_anchor() {
    assert_eq!(periods_per_year_for_interval("1h"), 6_048.0);
    assert_eq!(periods_per_year_for_interval("1m"), 362_880.0);
    // ...and the scale is exactly "how many of that interval fit in a day".
    assert_eq!(periods_per_year_for_interval("4h"), DAILY_PERIODS_PER_YEAR * 6.0);
}

/// An interval the parser cannot read falls back rather than fabricating a scale. This is a
/// reporting knob, not a validation site.
#[test]
fn an_unparseable_interval_falls_back_instead_of_fabricating_a_scale() {
    assert_eq!(periods_per_year_for_interval(""), DEFAULT_PERIODS_PER_YEAR);
    assert_eq!(periods_per_year_for_interval("not-an-interval"), DEFAULT_PERIODS_PER_YEAR);
}

/// The tick-or-bar rule: a bar interval scales, no interval (a tick stream) is the default.
#[test]
fn a_stream_with_no_bar_interval_takes_the_default_factor() {
    assert_eq!(periods_per_year_for(Some("1h")), periods_per_year_for_interval("1h"));
    assert_eq!(periods_per_year_for(None), DEFAULT_PERIODS_PER_YEAR);
}

/// `rendered_rows` IS the tearsheet's rendering of the same report: every catalog row, in catalog
/// order, `funding_paid` included (the row the Studio's own eleven never showed). A run that did not
/// arm closed-trade accounting carries no `MetricHome::ClosedTrades` row, so those rows are not shown.
#[test]
fn rendered_rows_are_the_tearsheets_rows_in_catalog_order() {
    let report = TradingResults::from_record(None, &sample_result(), 252.0);
    let rows = report.rendered_rows();
    let ids: Vec<&str> = rows.iter().map(|(id, _)| *id).collect();
    let catalog: Vec<&str> = crate::metric_catalog::METRICS
        .iter()
        .filter(|m| m.home != crate::metric_catalog::MetricHome::ClosedTrades)
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, catalog);
    assert_eq!(
        rows,
        crate::results_table::TradingResultsTable::from_report(&report).rendered_rows()
    );
    assert!(rows.contains(&("funding_paid", "0.00".to_string())));
}

// --- the long-form catalog, the honesty counters and the realism stamp ---------------------

/// The same property `matches_direct_metrics_calls` asserts for the compact eight, for the
/// thirty that joined them: every field is one `metrics::` call and nothing here is new math,
/// so a divergence is a wiring bug.
#[test]
fn extended_matches_direct_metrics_calls() {
    let r = sample_result();
    let e = TradingResultsExtendedMetrics::from_result(&r, 252.0);
    let eq = &r.equity_curve;
    let tr = &r.trades;

    assert_eq!(e.net_profit, metrics::net_profit(tr));
    assert_eq!(e.gross_profit, metrics::gross_profit(tr));
    assert_eq!(e.gross_loss, metrics::gross_loss(tr));
    assert_eq!(e.total_fees, metrics::total_fees(tr));
    assert_eq!(e.average_win, metrics::average_win(tr));
    assert_eq!(e.average_loss, metrics::average_loss(tr));
    assert_eq!(e.largest_win, metrics::largest_win(tr));
    assert_eq!(e.largest_loss, metrics::largest_loss(tr));
    assert_eq!(e.payoff_ratio, metrics::payoff_ratio(tr));
    assert_eq!(e.expected_payoff, metrics::expected_payoff(tr));
    assert_eq!(e.consecutive_wins, metrics::consecutive_wins(tr));
    assert_eq!(e.consecutive_losses, metrics::consecutive_losses(tr));
    assert_eq!(e.sqn, metrics::sqn(tr));
    assert_eq!(e.long_ratio, metrics::long_ratio(tr));
    assert_eq!(e.sortino, metrics::sortino(eq, 252.0));
    assert_eq!(e.calmar, metrics::calmar(eq, 252.0));
    assert_eq!(e.cagr, metrics::cagr(eq, 252.0));
    assert_eq!(e.recovery_factor, metrics::recovery_factor(eq));
    assert_eq!(e.ulcer_index, metrics::ulcer_index(eq));
    assert_eq!(e.ulcer_performance_index, metrics::ulcer_performance_index(eq, 252.0));
    assert_eq!(e.k_ratio, metrics::k_ratio(eq));
    assert_eq!(e.risk_return_ratio, metrics::risk_return_ratio(eq));
    assert_eq!(e.returns_volatility, metrics::returns_volatility(eq, 252.0));
    assert_eq!(e.returns_skewness, metrics::returns_skewness(eq));
    assert_eq!(e.returns_kurtosis, metrics::returns_kurtosis(eq));
    assert_eq!(e.tail_ratio, metrics::tail_ratio(eq));
    assert_eq!(e.omega, metrics::omega(eq, OMEGA_THRESHOLD));
    assert_eq!(e.value_at_risk_95, metrics::value_at_risk(eq, TAIL_CONFIDENCE));
    assert_eq!(e.expected_shortfall_95, metrics::expected_shortfall(eq, TAIL_CONFIDENCE));
}

/// **The returns built once answer what each metric's own door answers, bit for bit** (perf audit
/// item 9): `from_record` builds the curve's returns and their sorted copy once for the nine metrics
/// that read them, where each door (`metrics::sortino(eq, ..)` and the rest) builds its own and sorts
/// its own, as `from_record` did before. Over a long curve with zero equity (a skipped return and a
/// `-1` one), flat stretches (tied zero returns) and every shorter length a guard reads. The PSR's
/// moments read off the block (`TradingResultsExtendedMetrics::sharpe_moments`) are
/// `overfit::sharpe_moments`' too, PSR included. With `--nocapture` it prints the nine doors' time
/// against `from_record`'s whole catalog.
#[test]
fn extended_on_a_long_curve_is_every_door_bit_for_bit() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut equity = 100_000.0;
    let long: Vec<f64> = (0..300_000_u64)
        .map(|i| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let draw = (state >> 11) as f64 / (1_u64 << 53) as f64 - 0.5;
            match i % 10_007 {
                5_000 => equity = 0.0,
                5_001 => equity = 50_000.0,
                _ if i % 97 < 3 => {}
                _ => equity *= 1.0 + draw * 0.004,
            }
            equity
        })
        .collect();
    let mut curves: Vec<Vec<f64>> = (0..6).map(|n| long[..n].to_vec()).collect();
    curves.push(vec![0.0, 0.0, 100.0, 100.0]);
    curves.push(long);
    for curve in curves {
        let r = TradingRecord { equity_curve: curve, ..sample_result() };
        let eq = &r.equity_curve;
        let started = std::time::Instant::now();
        let doors = [
            ("sortino", metrics::sortino(eq, 252.0)),
            ("risk_return_ratio", metrics::risk_return_ratio(eq)),
            ("returns_volatility", metrics::returns_volatility(eq, 252.0)),
            ("returns_skewness", metrics::returns_skewness(eq)),
            ("returns_kurtosis", metrics::returns_kurtosis(eq)),
            ("tail_ratio", metrics::tail_ratio(eq)),
            ("omega", metrics::omega(eq, OMEGA_THRESHOLD)),
            ("value_at_risk_95", metrics::value_at_risk(eq, TAIL_CONFIDENCE)),
            ("expected_shortfall_95", metrics::expected_shortfall(eq, TAIL_CONFIDENCE)),
        ];
        let by_door = started.elapsed();
        let started = std::time::Instant::now();
        let e = TradingResultsExtendedMetrics::from_result(&r, 252.0);
        let once = started.elapsed();
        for (id, door) in doors {
            let got = e.value_of(id).map(f64::to_bits);
            assert_eq!(got, Some(door.to_bits()), "{id} over {} points", eq.len());
        }
        let bits = |m: crate::overfit::OverfitSharpeMoments| {
            (m.sr_per_obs.to_bits(), m.n_obs, m.skew.to_bits(), m.kurt.to_bits(), m.psr().to_bits())
        };
        let read = e.sharpe_moments(metrics::returns(eq).len());
        let derived = crate::overfit::sharpe_moments(eq);
        assert_eq!(bits(read), bits(derived), "the PSR moments over {} points", eq.len());
        if eq.len() > 10 {
            eprintln!("{} points: the nine doors {by_door:?}, from_record {once:?}", eq.len());
        }
    }
}

/// **A TICK record kept by time** (decision 0078's amendment of 2026-11-20): its Sharpe is the closes' Sharpe at its
/// step, equal to the bit to one worked by hand; its drawdown and recovery factor are the PATH's (a trough between two
/// closes, deeper than the closes' own), its invalid day the path's first zero, and the report names its step. The
/// caller's `periods_per_year` is not read.
#[test]
fn a_record_kept_by_time_reports_its_closes_at_its_step_and_its_paths_drawdown() {
    let minute = |n: i64| 1_788_220_800_000 + n * 60_000; // 2026-09-01 00:00 UTC
    let r = TradingRecord {
        // closes whose returns are exact: +100%, -50%, +100%
        equity_curve: vec![100.0, 200.0, 100.0, 200.0],
        equity_timestamp: (0..4).map(minute).collect(),
        final_equity: 200.0,
        sampling: Some(crate::record::TradingRecordSampling {
            interval_ms: 60_000,
            max_drawdown: 0.75,
            max_equity: 200.0,
            min_equity: 50.0,
            reached_zero_at: None,
            events: 240,
            flat: false,
        }),
        ..Default::default()
    };
    let report = TradingResults::from_record(None, &r, 252.0);
    // by hand: returns 1, -0.5, 1; mean 0.5; sample variance (0.25 + 1 + 0.25) / 2 = 0.75; 252 · 1440 minutes a year
    let by_hand = (0.5 / 0.75_f64.sqrt()) * (252.0_f64 * 1440.0).sqrt();
    assert_eq!(report.sharpe.to_bits(), by_hand.to_bits(), "{} vs {by_hand}", report.sharpe);
    assert_eq!(
        report.sharpe,
        metrics::sharpe(&r.equity_curve, periods_per_year_for_interval("1m"))
    );
    assert_ne!(report.sharpe, metrics::sharpe(&r.equity_curve, 252.0), "not the caller's 252");
    assert_eq!(report.returns_interval_ms, Some(60_000));
    assert_eq!(metrics::max_drawdown(&r.equity_curve), 0.5, "the closes' own drawdown");
    assert_eq!(report.max_drawdown, 0.75, "the path's");
    let extended = report.extended.as_ref().unwrap();
    assert_eq!(extended.recovery_factor, metrics::total_return(&r.equity_curve) / 0.75);
    assert_eq!(
        extended.returns_volatility,
        metrics::returns_volatility(&r.equity_curve, 362_880.0)
    );
    assert_eq!(report.invalid, None);
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains(r#""returns_interval_ms":60000"#), "{json}");

    // the path touched zero between two closes that never did
    let dipped = TradingRecord {
        sampling: Some(crate::record::TradingRecordSampling {
            reached_zero_at: Some(minute(1) + 5_000),
            max_drawdown: 1.0,
            ..r.sampling.unwrap()
        }),
        ..r.clone()
    };
    assert!(dipped.equity_curve.iter().all(|e| *e > 0.0));
    let why = "equity reached 0 on 2026-09-01: metrics are not meaningful";
    assert_eq!(TradingResults::from_record(None, &dipped, 252.0).invalid.as_deref(), Some(why));

    // a run that never moved is diagnosed over the PATH's events (its warm-up against them), not the closes
    let idle = TradingRecord {
        equity_curve: vec![100.0, 100.0],
        final_equity: 100.0,
        sampling: Some(crate::record::TradingRecordSampling {
            max_drawdown: 0.0,
            events: 5_000,
            flat: true,
            ..r.sampling.unwrap()
        }),
        source: crate::record::TradingRecordSource::Backtest(
            crate::record::TradingRecordSimulator { warmup: 1_000, ..Default::default() },
        ),
        ..Default::default()
    };
    let diagnosis = crate::zero_trade::ZeroTradeReport::analyze(&idle).expect("an idle run");
    assert!(diagnosis.causes.iter().all(|c| c.code != "warmup-shortfall"), "{diagnosis:?}");
    let moved = TradingRecord {
        sampling: Some(crate::record::TradingRecordSampling {
            flat: false,
            ..idle.sampling.unwrap()
        }),
        ..idle.clone()
    };
    assert!(crate::zero_trade::ZeroTradeReport::analyze(&moved).is_none(), "the path moved");

    // a bar run's report carries no step, and one without the key (every tick report before) reads as none
    let bar = TradingResults::from_record(None, &sample_result(), 252.0);
    assert_eq!(bar.returns_interval_ms, None);
    assert!(!serde_json::to_string(&bar).unwrap().contains("returns_interval_ms"));
    let old: TradingResults = serde_json::from_str(&oldest_report(None)).unwrap();
    assert_eq!(old.returns_interval_ms, None);
}
