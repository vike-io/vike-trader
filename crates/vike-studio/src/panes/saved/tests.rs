use super::*;
use vike_analytics::TradingRecord;
use vike_analytics::results::{
    DAILY_PERIODS_PER_YEAR, TradingResults, periods_per_year_for_interval,
};
use vike_backtesthub_client::summary::summarize;
use vike_backtesthub_client::wire_record::{from_wire_record, to_wire_record};
use vike_model::params::ParamValue;

/// The factor a 1m slice earns — DERIVED, never written as a number here, so these tests
/// exercise the same function the pane does rather than a second copy of the arithmetic.
fn periods_per_year_1m() -> f64 {
    periods_per_year_for_interval("1m")
}

fn strategy(name: &str, code: &str) -> StudioSavedStrategy {
    StudioSavedStrategy::rhai(name, code)
}

/// A file from before rows had a `source` (`{name, code}` only) is REFUSED with the generic hint,
/// never read as a Rhai row with empty fields, and left as it is.
#[test]
fn a_file_without_a_source_field_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    let old = r#"[
  {
    "name": "sma-cross",
    "code": "fn on_bar() {}"
  }
]"#;
    std::fs::write(&path, old).unwrap();
    let hint = load_strategies(&path).expect_err("refused");
    assert!(hint.contains("missing field `source`") && hint.contains("nothing is saved"), "{hint}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), old, "the file is left as it was");
}

/// A strategy-file entry round-trips through the file under the word `file`, and its `spec()`
/// carries the typed params.
#[test]
fn file_entry_round_trips_and_specs_with_typed_params() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    let entry = StudioSavedStrategy::file(
        "hold-2",
        "buy_hold",
        vec![("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())],
    );
    save_strategies(&path, std::slice::from_ref(&entry)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains(r#""source": "file""#) && text.contains(r#""file": "buy_hold""#),
        "{text}"
    );
    let loaded = load_strategies(&path).expect("the file reads");
    assert_eq!(loaded, vec![entry]);

    match loaded[0].spec() {
        StrategySpec::File { file: name, params, .. } => {
            assert_eq!(name, "buy_hold");
            assert_eq!(params.get("size").and_then(ParamValue::as_integer), Some(2));
            assert_eq!(params.get("symbol").and_then(ParamValue::as_str), Some("BTCUSDT"));
        }
        other => panic!("expected a file spec, got {other:?}"),
    }
}

/// THE OWNER'S RULING OF 2026-11-17: a `studio_strategies.json` an older build wrote — a Rhai row
/// carrying the old `native` key, and a strategy-file row saying `"source": "native"` — is REFUSED
/// with a one-line hint naming the file and the remedy; nothing in it is read, the bytes stay as
/// they were, and the Saved pane's Save stays disabled, so nothing is written over it.
#[test]
fn an_old_strategies_file_saying_native_is_refused_and_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    for (old, word) in [
        (
            r#"[
  { "name": "old", "code": "fn on_bar() {}", "source": "rhai", "native": "", "params": [],
    "rust_file": "", "rust_sha": null }
]"#,
            "native",
        ),
        (
            r#"[
  { "name": "new", "code": "", "source": "native", "native": "buy_hold",
    "params": [["size", "1"]], "rust_file": "", "rust_sha": null }
]"#,
            "native",
        ),
    ] {
        std::fs::write(&path, old).unwrap();
        let hint = load_strategies(&path).expect_err("an old file must be refused");
        assert_eq!(
            hint,
            format!(
                "{} was written by an older Vike Trader (it says `{word}`) and is not read: move \
                 it away and save your strategies again in the Saved tab. Nothing is saved over it \
                 until then.",
                path.display()
            )
        );
        let pane = StudioSavedPane::load(&path);
        assert!(pane.strategies.is_empty(), "nothing was read");
        assert_eq!(pane.refused.as_deref(), Some(hint.as_str()), "Save is disabled with the hint");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old, "the file is left as it was");
    }
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    let list = vec![
        strategy("sma-cross", "fn on_bar(strategy) {}"),
        strategy("rsi-mean-revert", "let x = 1;"),
    ];

    save_strategies(&path, &list).unwrap();
    let loaded = load_strategies(&path).expect("the file reads");

    assert_eq!(loaded, list);
}

#[test]
fn load_of_a_missing_path_is_an_empty_vec_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("does_not_exist.json");
    assert_eq!(load_strategies(&path), Ok(Vec::new()));
}

/// A file that does not parse is REFUSED with its reason rather than read as empty: an empty list
/// was the answer, and the next Save wrote over every row the file held.
#[test]
fn load_of_a_corrupt_file_is_refused_not_read_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage.json");
    std::fs::write(&path, "{ not: valid json ]]]").unwrap();
    let hint = load_strategies(&path).expect_err("refused");
    assert!(hint.contains("could not be read") && hint.contains("nothing is saved"), "{hint}");
}

/// **An old file with the retired Rust tag is refused with a one-line hint, never converted**, and
/// the pane that loaded it keeps nothing to write over it: its Rhai rows are still on disk, intact.
#[test]
fn a_file_with_the_retired_rust_tag_is_refused_with_a_hint_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    let old = format!(
        r#"[
  {{ "name": "built", "code": "", "source": "{RETIRED_RUST_TAG}" }},
  {{ "name": "keep-me", "code": "fn on_bar() {{}}" }}
]"#
    );
    std::fs::write(&path, &old).unwrap();
    let hint = load_strategies(&path).expect_err("refused, not converted");
    assert!(
        hint.contains(&format!("it says `{RETIRED_RUST_TAG}`")) && hint.contains("move it away"),
        "{hint}"
    );
    assert_eq!(hint.lines().count(), 1, "one line: {hint}");
    let pane = StudioSavedPane::load(&path);
    assert!(pane.strategies.is_empty());
    assert_eq!(pane.refused.as_deref(), Some(hint.as_str()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), old, "the file is left as it was");
}

#[test]
fn save_overwrites_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    save_strategies(&path, &[strategy("a", "1")]).unwrap();
    save_strategies(&path, &[strategy("b", "2")]).unwrap();
    assert_eq!(load_strategies(&path), Ok(vec![strategy("b", "2")]));
}

/// A save never truncates and rewrites the previous file in place (it writes a sibling and renames
/// it over), so a save interrupted at any point before that rename leaves the previous list whole.
/// Measured through a handle opened on the previous file before the save: an in-place rewrite
/// (`std::fs::write`, the old body) changes what it reads; a replace leaves it the old bytes.
#[cfg(unix)]
#[test]
fn an_interrupted_save_leaves_the_previous_list_intact() {
    use std::io::Read;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("studio_strategies.json");
    save_strategies(&path, &[strategy("a", "1")]).unwrap();
    let old = std::fs::read_to_string(&path).unwrap();
    let mut previous = std::fs::File::open(&path).unwrap();
    save_strategies(&path, &[strategy("b", "2")]).unwrap();
    let mut kept = String::new();
    previous.read_to_string(&mut kept).unwrap();
    assert_eq!(kept, old, "the previous file was rewritten in place");
    assert_eq!(load_strategies(&path), Ok(vec![strategy("b", "2")]));
}

fn result(equity_curve: Vec<f64>, n_trades: usize) -> TradingRecord {
    let final_equity = *equity_curve.last().unwrap();
    TradingRecord { equity_curve, final_equity, n_trades, ..Default::default() }
}

/// `r`'s summary as the backtesthub answers it for a Compare-all row (`summarize` over the wire result,
/// decision 0078's amendment of 2026-11-18), annualized at `periods_per_year`.
fn summary_of(r: &TradingRecord, periods_per_year: f64) -> RunSummaryWire {
    summarize(to_wire_record(r), periods_per_year).0
}

/// Every `(name, result)` as a Compare-all row's outcome at the 1m factor, a failure kept as its sentence.
fn summarized(
    results: Vec<(&str, Result<TradingRecord, &str>)>,
    periods_per_year: f64,
) -> Vec<(String, Result<RunSummaryWire, String>)> {
    results
        .into_iter()
        .map(|(name, r)| {
            let outcome =
                r.map(|r| summary_of(&r, periods_per_year)).map_err(|why| why.to_string());
            (name.to_string(), outcome)
        })
        .collect()
}

#[test]
fn comparison_rows_sorts_by_sharpe_descending_and_carries_fields() {
    // a strongly uptrending curve (high sharpe), a flat/noisy one (low/negative sharpe).
    let good = result(vec![100.0, 105.0, 110.0, 116.0, 123.0, 131.0], 4);
    let bad = result(vec![100.0, 98.0, 101.0, 97.0, 100.0, 96.0], 9);
    let results =
        summarized(vec![("bad", Ok(bad)), ("good", Ok(good.clone()))], periods_per_year_1m());

    let rows = saved_compare_rows(&results, |_| StudioStrategySource::Rhai);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "good", "higher sharpe should rank first");
    assert_eq!(rows[1].name, "bad");
    assert!(summary(&rows[0]).sharpe > summary(&rows[1]).sharpe);
    assert_eq!(summary(&rows[0]).final_equity, *good.equity_curve.last().unwrap());
    assert_eq!(summary(&rows[0]).n_trades, 4);
    assert_eq!(summary(&rows[1]).n_trades, 9);
}

/// **A run whose equity reached zero is INVALID in the compare table**: it ranks after a valid run whatever its Sharpe
/// (this one's curve climbs back steeply), and its Sharpe and drawdown cells say `INVALID: equity reached 0 on <day>…`
/// where a missing cell would say `not recorded`.
#[test]
fn an_invalid_run_ranks_last_and_its_cells_say_invalid() {
    let dead = TradingRecord {
        equity_timestamp: vec![0, 60_000, 120_000, 180_000],
        ..result(vec![100.0, -10.0, 50.0, 400.0], 2)
    };
    let slow = result(vec![100.0, 100.5, 101.0, 100.8], 1);
    let results = summarized(vec![("dead", Ok(dead)), ("slow", Ok(slow))], periods_per_year_1m());
    let rows = saved_compare_rows(&results, |_| StudioStrategySource::Rhai);
    assert_eq!(rows[0].name, "slow", "the valid run first");
    assert_eq!(rows[1].name, "dead");
    let invalid = "INVALID: equity reached 0 on 1970-01-01: metrics are not meaningful";
    let [sharpe, equity, drawdown, trades] = &rows[1].cells;
    assert_eq!([&sharpe[0], &drawdown[0]], [invalid, invalid], "{:?}", rows[1].cells);
    assert_eq!([&equity[0], &trades[0]], ["400.00", "2"], "{:?}", rows[1].cells);
    // Against the valid top row: money and counts have a difference, an INVALID Sharpe or drawdown none.
    assert_eq!([&equity[1], &trades[1], &sharpe[1], &drawdown[1]], ["299.20", "1", "", ""]);
}

/// The compare table's cells are the ONE comparison's (`TradingResultsComparison`, what
/// `vike-cli report --compare` prints), over the results tables the summaries carry: each row held against
/// the top-ranked one, its value the row's own text and its difference `row − top` in the metric's unit;
/// the top row's difference is its own zero, which the pane does not draw.
#[test]
fn each_cell_carries_its_difference_from_the_top_ranked_row() {
    let good = result(vec![100.0, 105.0, 110.0, 116.0, 123.0, 131.0], 4);
    let bad = result(vec![100.0, 98.0, 101.0, 97.0, 100.0, 96.0], 9);
    let results = summarized(vec![("bad", Ok(bad)), ("good", Ok(good))], periods_per_year_1m());
    let rows = saved_compare_rows(&results, |_| StudioStrategySource::Rhai);
    let table = |row: &StudioSavedCompareRow| summary(row).results.clone().expect("a table");
    let comparison = TradingResultsComparison::of(table(&rows[0]), table(&rows[1]));
    let rendered = comparison.rendered_rows();
    for (id, [value, difference]) in COMPARE_IDS.iter().zip(&rows[1].cells) {
        let (_, [_, b, b_minus_a]) = rendered.iter().find(|(k, _)| k == id).expect("a shown row");
        assert_eq!((value, difference), (b, b_minus_a), "{id}");
    }
    let [_, equity, _, trades] = &rows[1].cells;
    assert_eq!([&equity[1], &trades[1]], ["-35.00", "5"], "96 − 131 and 9 − 4");
    assert!(rows[1].cells[0][1].starts_with('-'), "the worse row's Sharpe is below the top's");
    assert_eq!(rows[0].cells.clone().map(|[_, difference]| difference)[3], "0");
}

/// The table names what its differences are taken from: one line over it with the top-ranked row's
/// summary `source_label` (a backtest, its strategy file and the slice's period), and none while fewer than
/// two rows ran.
#[test]
fn the_table_names_the_row_every_difference_is_taken_from() {
    // 2026-01-01 00:00 UTC, one sample a minute.
    let minutes = |n: i64| (0..n).map(|i| 1_767_225_600_000 + i * 60_000).collect::<Vec<i64>>();
    let good = TradingRecord {
        equity_timestamp: minutes(6),
        ..result(vec![100.0, 105.0, 110.0, 116.0, 123.0, 131.0], 4)
    };
    let bad = TradingRecord {
        equity_timestamp: minutes(6),
        ..result(vec![100.0, 98.0, 101.0, 97.0, 100.0, 96.0], 9)
    };
    let rhai = |_: &str| StudioStrategySource::Rhai;
    let both =
        summarized(vec![("bad", Ok(bad)), ("good", Ok(good.clone()))], periods_per_year_1m());
    let rows = saved_compare_rows(&both, rhai);
    assert_eq!(
        difference_header(&rows).as_deref(),
        Some(
            "Under each value: its difference from backtest (unnamed) 2026-01-01 00:00 \u{2013} \
             2026-01-01 00:05 UTC"
        )
    );
    let alone =
        summarized(vec![("good", Ok(good)), ("broken", Err("boom"))], periods_per_year_1m());
    assert_eq!(difference_header(&saved_compare_rows(&alone, rhai)), None);
}

/// **An older backtesthub's summaries carry no results table**: "Compare all" still ranks and prints every
/// row's values (the summary's rendered text, the same as with a table), draws no difference, and says so in
/// the one line over the table. It asks for nothing more.
#[test]
fn a_summary_without_its_results_table_prints_its_values_without_differences() {
    let good = result(vec![100.0, 105.0, 110.0, 116.0, 123.0, 131.0], 4);
    let bad = result(vec![100.0, 98.0, 101.0, 97.0, 100.0, 96.0], 9);
    let with = summarized(vec![("bad", Ok(bad)), ("good", Ok(good))], periods_per_year_1m());
    let older: Vec<(String, Result<RunSummaryWire, String>)> = with
        .iter()
        .map(|(name, outcome)| {
            let stripped = outcome.clone().map(|s| RunSummaryWire { results: None, ..s });
            (name.clone(), stripped)
        })
        .collect();
    let rhai = |_: &str| StudioStrategySource::Rhai;
    let (tabled, plain) = (saved_compare_rows(&with, rhai), saved_compare_rows(&older, rhai));
    let names =
        |rows: &[StudioSavedCompareRow]| rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&plain), names(&tabled), "the same ranking");
    for (plain, tabled) in plain.iter().zip(&tabled) {
        let values = |row: &StudioSavedCompareRow| row.cells.clone().map(|[value, _]| value);
        assert_eq!(values(plain), values(tabled), "{}: the same values", plain.name);
        assert!(
            plain.cells.iter().all(|[_, difference]| difference.is_empty()),
            "{:?}",
            plain.cells
        );
    }
    assert_eq!(difference_header(&plain).as_deref(), Some(NO_RESULTS_TABLE));
}

/// The ran row's summary, which every one of these tests expects to exist.
fn summary(row: &StudioSavedCompareRow) -> &RunSummaryWire {
    row.outcome.as_ref().expect("the row ran")
}

/// D8: a compare row prints the Performance tab's text for the same run — the catalog's units,
/// a drawdown as the positive fraction the catalog stores (`25.0000%`, not `-25.0%`).
#[test]
fn a_compare_rows_cells_are_the_performance_tabs_cells() {
    let r = result(vec![100.0, 120.0, 90.0, 110.0], 3);
    let rows =
        saved_compare_rows(&summarized(vec![("dd", Ok(r.clone()))], periods_per_year_1m()), |_| {
            StudioStrategySource::Rhai
        });
    let perf = crate::panes::results::perf_rows(&r, periods_per_year_1m());
    let want: Vec<String> = COMPARE_IDS
        .iter()
        .map(|id| perf.iter().find(|(k, _)| k == id).unwrap().1.clone())
        .collect();
    let values: Vec<String> = rows[0].cells.clone().map(|[value, _]| value).to_vec();
    assert_eq!(values, want);
    assert_eq!(values[2], "25.0000%");
}

#[test]
fn comparison_rows_puts_nan_sharpe_last() {
    // an equity curve with a NaN point (e.g. a div-by-zero upstream) makes every return NaN,
    // so `sharpe` (metrics.rs) itself comes back NaN — a flat curve is NOT this case; `sharpe`
    // explicitly guards zero variance to 0.0, not NaN (see metrics.rs's own doc). The summary carries
    // it as `None`, and `None` ranks where the NaN did.
    let broken = result(vec![100.0, f64::NAN, 100.0], 0);
    let trending = result(vec![100.0, 102.0, 104.5, 107.0], 2);
    let results =
        summarized(vec![("broken", Ok(broken)), ("trending", Ok(trending))], periods_per_year_1m());

    let rows = saved_compare_rows(&results, |_| StudioStrategySource::Rhai);

    assert_eq!(rows[0].name, "trending");
    assert_eq!(rows[1].name, "broken");
    assert_eq!(summary(&rows[1]).sharpe, None);
}

#[test]
fn comparison_rows_on_empty_input_is_empty() {
    assert!(saved_compare_rows(&[], |_| StudioStrategySource::Rhai).is_empty());
}

/// The compare table's half of the annualization fix, in two claims that must BOTH hold.
///
/// The values MOVE with the factor — a 1m comparison is on a `sqrt(1440) ≈ 37.9x` larger scale
/// than the daily one this function used to hard-code, which is the whole defect — and the
/// ORDER does not, because a positive constant multiplier cannot reorder anything. Without the
/// second half, a future "simplification" back to a fixed constant would look harmless: the
/// table would still rank correctly while every number in it disagreed with the Performance
/// tab beside it.
#[test]
fn the_ranking_is_identical_on_any_factor_while_the_values_move() {
    let good = result(vec![100.0, 105.0, 110.0, 116.0, 123.0, 131.0], 4);
    let bad = result(vec![100.0, 98.0, 101.0, 97.0, 100.0, 96.0], 9);
    let runs = || vec![("bad", Ok(bad.clone())), ("good", Ok(good.clone()))];

    let daily = saved_compare_rows(&summarized(runs(), DAILY_PERIODS_PER_YEAR), |_| {
        StudioStrategySource::Rhai
    });
    let minute = saved_compare_rows(&summarized(runs(), periods_per_year_1m()), |_| {
        StudioStrategySource::Rhai
    });

    let daily_names: Vec<&str> = daily.iter().map(|r| r.name.as_str()).collect();
    let minute_names: Vec<&str> = minute.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(daily_names, minute_names, "the factor must not reorder the table");

    let scale = (periods_per_year_1m() / DAILY_PERIODS_PER_YEAR).sqrt();
    for (d, m) in daily.iter().zip(&minute) {
        let name = &d.name;
        assert_eq!(d.cells[2][0], m.cells[2][0], "{name}: the drawdown cell");
        let (d, m) = (summary(d), summary(m));
        let ratio = m.sharpe.unwrap() / d.sharpe.unwrap();
        assert!((ratio - scale).abs() < 1e-9, "{name}: Sharpe scaled {ratio}, want {scale}");
        // ...and nothing else in the row is annualized, so nothing else may move.
        assert_eq!(d.final_equity, m.final_equity);
        assert_eq!(d.n_trades, m.n_trades);
    }
}

/// Hand-verified against `metrics::max_drawdown`'s own contract (running-peak fractional
/// decline, NOT a negative number): peak hits 120 after the second point, then the deepest
/// dip to 90 is `(120-90)/120 == 0.25`; the later partial recovery to 110 is a smaller
/// drawdown (`(120-110)/120 ≈ 0.083`) so 0.25 stays the worst. The sparkline's thinned curve
/// draws down exactly as far.
#[test]
fn comparison_rows_computes_max_drawdown_correctly() {
    let r = result(vec![100.0, 120.0, 90.0, 110.0], 3);
    let rows =
        saved_compare_rows(&summarized(vec![("dd-check", Ok(r))], periods_per_year_1m()), |_| {
            StudioStrategySource::Rhai
        });

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].cells[2][0], "25.0000%");
    let drawdown = max_drawdown(&rows[0].equity);
    assert!((drawdown - 0.25).abs() < 1e-12, "max_drawdown = {drawdown}");
}

/// A successful row carries the summary's curve (for the sparkline: every point of a short run);
/// a failed row carries none.
#[test]
fn comparison_rows_carries_the_equity_curve_for_successful_rows() {
    let curve = vec![100.0, 101.0, 99.0, 103.0];
    let r = result(curve.clone(), 1);
    let results = summarized(vec![("ok", Ok(r)), ("broken", Err("boom"))], periods_per_year_1m());

    let rows = saved_compare_rows(&results, |_| StudioStrategySource::Rhai);

    let ok_row = rows.iter().find(|r| r.name == "ok").unwrap();
    assert_eq!(ok_row.equity, curve);
    let failed_row = rows.iter().find(|r| r.name == "broken").unwrap();
    assert!(failed_row.equity.is_empty());
}

/// A strategy that failed to compile/run is surfaced as a row with `outcome: Err(..)` rather
/// than silently dropped, and sorts after every successful row regardless of the successful
/// rows' Sharpe ranking.
#[test]
fn comparison_rows_surfaces_a_failed_strategy_and_sorts_it_last() {
    let good = result(vec![100.0, 105.0, 110.0, 116.0], 2);
    let results = summarized(
        vec![("broken", Err("compile error: unexpected token")), ("good", Ok(good))],
        periods_per_year_1m(),
    );

    let rows = saved_compare_rows(&results, |_| StudioStrategySource::Rhai);

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "good", "the successful row ranks first");
    assert!(rows[0].outcome.is_ok());
    assert_eq!(rows[1].name, "broken");
    assert_eq!(
        rows[1].outcome.as_ref().err().map(String::as_str),
        Some("compile error: unexpected token")
    );
}

/// A deterministic multiplicative walk of `n` points from `start`, its step drawn from `seed`.
fn walk(n: usize, start: f64, drift: f64, seed: u64) -> Vec<f64> {
    let mut state = seed;
    let mut v = start;
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let step = ((state >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.01 + drift;
            v *= 1.0 + step;
            v
        })
        .collect()
}

/// **Compare all ranks the saved runs as their WHOLE answers did** (decision 0078's amendment of 2026-11-18): the
/// same runs, ranked the way this pane ranked them until that day — each whole answer decoded
/// (`from_wire_record`), its `TradingResults` at the slice's factor, valid rows by that report's Sharpe through
/// `cmp_scores_desc`, an INVALID one after them, a failure last, ties in saved order — and ranked from their
/// SUMMARIES, come out in one order with the same cells. The runs are long enough to be thinned (5,000 and
/// 30,000 points against the summary's two thousand), one is flat, one reached zero, two are the same run (a tie),
/// and one failed.
#[test]
fn the_summary_ranks_the_saved_runs_as_their_whole_answers_did() {
    let periods = periods_per_year_1m();
    let dead = TradingRecord {
        equity_timestamp: (0..5_000).map(|i| i * 60_000).collect(),
        ..result(
            walk(5_000, 100.0, 0.0, 7)
                .into_iter()
                .enumerate()
                .map(|(i, v)| if i == 2_500 { -1.0 } else { v })
                .collect(),
            40,
        )
    };
    let runs: Vec<(&str, Result<TradingRecord, &str>)> = vec![
        ("drift-up", Ok(result(walk(30_000, 100.0, 0.000_2, 1), 310))),
        ("noise", Ok(result(walk(5_000, 100.0, 0.0, 2), 120))),
        ("failed", Err("compile error: unexpected token")),
        ("drift-down", Ok(result(walk(30_000, 100.0, -0.000_2, 3), 290))),
        ("flat", Ok(result(vec![100.0; 5_000], 0))),
        ("dead", Ok(dead)),
        ("noise-again", Ok(result(walk(5_000, 100.0, 0.0, 2), 120))),
        ("drift-up-slow", Ok(result(walk(30_000, 100.0, 0.000_05, 4), 305))),
    ];

    // Today's ranking, as `saved_compare_rows` computed it over the whole answers until 2026-11-18.
    let whole: Vec<(String, Result<TradingResults, String>)> = runs
        .iter()
        .map(|(name, r)| {
            let report = r
                .as_ref()
                .map(|r| {
                    let decoded = from_wire_record(to_wire_record(r));
                    TradingResults::from_record(Some(name.to_string()), &decoded, periods)
                })
                .map_err(|why| why.to_string());
            (name.to_string(), report)
        })
        .collect();
    let mut whole_order: Vec<&(String, Result<TradingResults, String>)> = whole.iter().collect();
    whole_order.sort_by(|a, b| match (&a.1, &b.1) {
        (Ok(a), Ok(b)) => match (a.invalid.is_some(), b.invalid.is_some()) {
            (false, false) => cmp_scores_desc(a.sharpe, b.sharpe),
            (a_invalid, b_invalid) => a_invalid.cmp(&b_invalid),
        },
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => std::cmp::Ordering::Equal,
    });
    let whole_names: Vec<&str> = whole_order.iter().map(|(name, _)| name.as_str()).collect();

    let rows =
        saved_compare_rows(&summarized(runs.clone(), periods), |_| StudioStrategySource::Rhai);
    let summary_names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(summary_names, whole_names, "the summaries rank as the whole answers did");
    assert_eq!(summary_names.last(), Some(&"failed"));
    assert_eq!(
        summary_names[summary_names.len() - 2],
        "dead",
        "the invalid run after every valid one"
    );

    // ...with the cells the whole answers gave, bit for bit: each whole report held against the top one
    // through the one comparison (`compare_cells`), as this pane printed them before the summaries carried
    // their results tables (the owner's word of 2026-11-19).
    let top = whole_order[0].1.as_ref().expect("the top row ran");
    for (row, (_, report)) in rows.iter().zip(&whole_order) {
        let (Ok(_), Ok(report)) = (&row.outcome, report) else { continue };
        assert_eq!(row.cells, compare_cells(top, report), "{}", row.name);
        assert!(row.equity.len() <= 2_004, "the sparkline draws the thinned curve");
        assert_eq!(
            max_drawdown(&row.equity).to_bits(),
            report.max_drawdown.to_bits(),
            "{}",
            row.name
        );
    }
}
