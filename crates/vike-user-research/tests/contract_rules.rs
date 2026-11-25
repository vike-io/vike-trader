//! The rules [`vike_user_research::StudyOutcome`] enforces, tested apart from the pipeline because
//! they are properties of the CONTRACT rather than of the scan.
//!
//! The one that matters most is the artifact name: it becomes a FILE NAME in a run directory the
//! caller creates, so a study must not be able to name a path the writer then has to defend
//! against. Refusing at the point of RECORDING keeps that check in one place instead of in every
//! consumer that ever writes a run out.

use std::assert_matches;
use std::sync::Arc;

use vike_data::{
    ChainRow, CohortRow, ExecFillRow, HistoryStore, HistoryStoreMemory, PerpMetricRow,
    TimestampRange,
};
use vike_fills::costs::costs_of;
use vike_marketdata::OptionKind;
use vike_model::params::Params;
use vike_model::{Commission, Costs};
use vike_user_research::{
    MAX_ARTIFACT_NAME, StudyContext, StudyError, StudyOutcome, costs_of_recipe, valid_artifact_name,
};

#[test]
fn metrics_keep_emission_order_and_refuse_a_duplicate() {
    let mut out = StudyOutcome::new();
    out.metric("sharpe", 5.454).unwrap();
    out.metric("n_trades", 812.0).unwrap();
    let names: Vec<&str> = out.metrics().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["sharpe", "n_trades"], "emission order, not sorted");
    assert_eq!(out.metric_value("sharpe"), Some(5.454));
    assert!(out.metric_value("nope").is_none());

    let err = out.metric("sharpe", 1.0).expect_err("two rows called sharpe are ambiguous");
    assert_matches!(err, StudyError::Study(ref m) if m.contains("sharpe"), "{err}");
}

/// A non-finite metric is ACCEPTED. A Sharpe over a fold that never traded is `NaN`, and
/// `vike_analytics::signal_backtest::neg_sharpe_mini_backtest` deliberately scores a degenerate
/// slice `INFINITY`; rounding either to zero here would be the `unwrap_or(0.0)` that turns a gap
/// into an observation.
#[test]
fn a_non_finite_metric_is_recorded_rather_than_rounded() {
    let mut out = StudyOutcome::new();
    out.metric("sharpe", f64::NAN).unwrap();
    out.metric("objective", f64::INFINITY).unwrap();
    assert!(out.metric_value("sharpe").unwrap().is_nan());
    assert_eq!(out.metric_value("objective"), Some(f64::INFINITY));
}

#[test]
fn an_artifact_name_that_is_not_a_safe_file_name_is_refused() {
    assert!(valid_artifact_name("per_rule.tsv").is_ok());
    assert!(valid_artifact_name("model.txt").is_ok());
    assert!(valid_artifact_name("report-2.html").is_ok());

    for bad in [
        "",
        "..",
        ".hidden",
        "../../secrets.env",
        "sub/dir.tsv",
        r"sub\dir.tsv",
        "with space.tsv",
        "naïve.tsv",
    ] {
        assert!(valid_artifact_name(bad).is_err(), "{bad:?} must be refused");
    }
    let too_long = "a".repeat(MAX_ARTIFACT_NAME + 1);
    assert!(valid_artifact_name(&too_long).is_err());
    assert!(valid_artifact_name(&"a".repeat(MAX_ARTIFACT_NAME)).is_ok());
}

#[test]
fn recording_a_bad_or_duplicate_artifact_is_an_error_naming_it() {
    let mut out = StudyOutcome::new();
    out.artifact("per_rule.tsv", "a\tb\n").unwrap();
    assert_eq!(out.artifacts().len(), 1);

    let dup = out.artifact("per_rule.tsv", "x").expect_err("two files cannot share a name");
    assert_matches!(dup, StudyError::Study(ref m) if m.contains("per_rule.tsv"), "{dup}");

    let bad = out.artifact("../escape.tsv", "x").expect_err("a traversal must not be recordable");
    assert_matches!(bad, StudyError::Study(ref m) if m.contains("escape"), "{bad}");
    assert_eq!(out.artifacts().len(), 1, "a refused artifact is not half-recorded");
}

// ---------------------------------------------------------------------------------------------
// the costs a study charges: the backtest's one cost type
// ---------------------------------------------------------------------------------------------

fn recipe(text: &str) -> toml::Value {
    toml::from_str(text).expect("a recipe")
}

/// A recipe's cost keys mean what a strategy's params mean: the same text read as a recipe and as a backtest's
/// params gives the same `Costs`, so a study charges what the backtest charges. No cost key is no cost, whatever
/// else the recipe holds.
#[test]
fn a_recipe_states_its_costs_in_the_backtests_grammar() {
    let none = costs_of_recipe(&recipe("venue = \"v\"\nsymbols = [\"A\"]\n[learner]\nsteps = 8\n"))
        .expect("no cost key");
    let free = Costs {
        commission: Commission::None,
        commission_set: false,
        slippage: None,
        slippage_set: false,
    };
    assert_eq!(none, free);
    for text in [
        "commission = 0.05",
        "commission = 0.5\ncommission_unit = \"per_contract\"",
        "commission = 1\ncommission_unit = \"per_order\"",
        "commission = 0.02\ncommission_unit = \"probability_scaled\"",
        "slippage = 0.03",
        "slippage = 2\nslippage_unit = \"ticks\"\nslippage_tick = 0.01",
        "commission = 0.05\nslippage = 0.03\ncash = 10000.0",
    ] {
        let params: Params = toml::from_str(text).expect("params");
        let backtest = costs_of(&params, None, None).expect("the backtest reads it");
        assert_eq!(costs_of_recipe(&recipe(text)).expect("the study reads it"), backtest, "{text}");
    }
}

/// A `[stated_slippage]` table is a stated slippage (`impact_fit`'s `stated_slippage.toml`, pasted): charged as
/// stated with no slippage key, and a `slippage` with no unit over it is refused in the recipe's words, as the
/// backtest refuses one over a strategy's stated model. A bad cost key is refused naming the recipe's key.
#[test]
fn a_recipe_states_a_slippage_table_and_is_refused_in_its_own_words() {
    let impact = "[stated_slippage]\nmodel = \"market_impact\"\nbase = 0.0002\nwindow = 5\n";
    let stated = costs_of_recipe(&recipe(impact)).expect("a stated market impact");
    let table = stated.slippage.expect("the stated table");
    assert_eq!(
        (table.model.as_str(), table.base, table.window),
        ("market_impact", Some(0.0002), Some(5))
    );
    assert!(!stated.slippage_set);

    let unitless = costs_of_recipe(&recipe(&format!("slippage = 0.01\n{impact}")))
        .expect_err("a unit-less slippage over a stated model");
    assert_matches!(
        unitless,
        StudyError::Study(ref m) if m.contains("recipe.slippage_unit")
            && m.contains("[stated_slippage]")
            && !m.contains("strategy.params"),
        "{unitless}"
    );
    let negative = costs_of_recipe(&recipe("commission = -1")).expect_err("a negative commission");
    assert_matches!(
        negative,
        StudyError::Study(ref m) if m.contains("recipe.commission must be a number >= 0"),
        "{negative}"
    );
    let unit = costs_of_recipe(&recipe("commission = 1\ncommission_unit = \"per_lot\""))
        .expect_err("a unit no grammar names");
    assert_matches!(unit, StudyError::Study(ref m) if m.contains("recipe.commission_unit"), "{unit}");
    let not_a_table =
        costs_of_recipe(&recipe("[stated_slippage]\nmodel = \"percent\"\nspeed = 2\n"))
            .expect_err("an unknown key of a slippage table");
    assert_matches!(not_a_table, StudyError::Study(ref m) if m.contains("[stated_slippage]"), "{not_a_table}");
}

// ---------------------------------------------------------------------------------------------
// the cohort read verb
// ---------------------------------------------------------------------------------------------

const VENUE: &str = "hyperliquid";

fn cohort_row(
    ts_ms: i64,
    axis: &str,
    cohort: &str,
    grading: &str,
    long: f64,
    total: f64,
) -> CohortRow {
    CohortRow {
        ts: ts_ms,
        asset: "BTC".into(),
        axis: axis.into(),
        cohort: cohort.into(),
        grading: grading.into(),
        label_basis: "point_in_time".into(),
        long_usd: long,
        total_usd: total,
    }
}

/// A context over a store seeded with `rows` for `(hyperliquid, BTC)`.
fn context_over(rows: &[CohortRow]) -> StudyContext {
    let store = HistoryStoreMemory::new();
    store
        .append_cohort(VENUE, "BTC", rows, Some("seed"))
        .expect("HistoryStoreMemory accepts the batch");
    StudyContext::new(Arc::new(store), TimestampRange::all(), std::env::temp_dir())
}

/// The verb forwards `scan_cohort`: what the store holds comes back, and the RANGE is the store's
/// own filter rather than something this crate re-implements.
#[test]
fn the_cohort_verb_forwards_the_store_read_and_its_range() {
    let rows = vec![
        cohort_row(1_000, "size", "Whale", "realized", 60.0, 100.0),
        cohort_row(2_000, "size", "Whale", "realized", 70.0, 100.0),
        cohort_row(3_000, "size", "Whale", "realized", 80.0, 100.0),
    ];
    let ctx = context_over(&rows);

    assert_eq!(ctx.cohort(VENUE, "BTC", TimestampRange::all()).unwrap(), rows);
    let mid = ctx.cohort(VENUE, "BTC", TimestampRange::of(2_000, 2_000)).unwrap();
    assert_eq!(mid.len(), 1);
    assert_eq!(mid[0].ts, 2_000);

    // A series the store does not hold is EMPTY, not an error — the ordinary uncollected state.
    assert!(ctx.cohort(VENUE, "ETH", TimestampRange::all()).unwrap().is_empty());
    assert!(ctx.cohort("binance", "BTC", TimestampRange::all()).unwrap().is_empty());
}

/// ⚠ THE PROPERTY A STUDY MUST HANDLE, gated rather than only documented: `(venue, asset)` does
/// not identify a series here. One scan returns every axis, label, grading and label basis, and
/// the three gradings produce SHAPE-IDENTICAL rows over the same hour for the same asset — so a
/// study that sums whatever comes back reports one number over two tapes.
///
/// This test is the reason the verb passes rows through undiminished instead of filtering: the
/// four discriminating dimensions are all present on the returned rows, which is what makes the
/// study's own filter writable. A verb that quietly picked one grading would make the wrong answer
/// unobservable.
#[test]
fn one_scan_returns_every_axis_and_grading_so_the_study_must_filter() {
    let rows = vec![
        cohort_row(1_000, "size", "Whale", "realized", 60.0, 100.0),
        cohort_row(1_000, "pnl", "Smart", "realized", 10.0, 40.0),
        cohort_row(1_000, "pnl", "Smart", "unrealized", 30.0, 40.0),
    ];
    let ctx = context_over(&rows);
    let got = ctx.cohort(VENUE, "BTC", TimestampRange::of(1_000, 1_000)).unwrap();
    assert_eq!(got.len(), 3, "one hour, one asset, three rows — the panel is LONG");

    // Every dimension the path cannot carry is on the row, so a study can select on it.
    let pnl_realized: Vec<&CohortRow> =
        got.iter().filter(|r| r.axis == "pnl" && r.grading == "realized").collect();
    assert_eq!(pnl_realized.len(), 1);
    assert_eq!(pnl_realized[0].long_usd, 10.0);

    // ...and the aliasing hazard is real on this exact data: summing the pnl axis without the
    // grading filter double-counts one hour's cohort.
    let unfiltered: f64 = got.iter().filter(|r| r.axis == "pnl").map(|r| r.total_usd).sum();
    assert_eq!(unfiltered, 80.0, "40 of open interest reported as 80 — this is the wrong answer");
    assert_eq!(pnl_realized.iter().map(|r| r.total_usd).sum::<f64>(), 40.0);
}

/// The short side stays DERIVED across the crossing: the store carries `total`, and
/// `CohortRow::short_usd` is the one subtraction — never a stored third number that could disagree.
#[test]
fn the_short_side_is_derived_from_the_row_the_verb_returned() {
    let ctx = context_over(&[cohort_row(1_000, "size", "Whale", "realized", 60.0, 100.0)]);
    let got = ctx.cohort(VENUE, "BTC", TimestampRange::all()).unwrap();
    assert_eq!(got[0].short_usd(), 40.0);
}

// ---------------------------------------------------------------------------------------------
// the perp-metrics read verb
// ---------------------------------------------------------------------------------------------

/// A context over a store seeded with perp market-context rows for `(hyperliquid, BTC)`.
fn perp_context_over(rows: &[PerpMetricRow]) -> StudyContext {
    let store = HistoryStoreMemory::new();
    store
        .append_perp_metrics(VENUE, "BTC", rows, Some("seed-perp"))
        .expect("HistoryStoreMemory accepts the batch");
    StudyContext::new(Arc::new(store), TimestampRange::all(), std::env::temp_dir())
}

/// The verb forwards `scan_perp_metrics`: what the store holds comes back, and the RANGE is the
/// store's own filter rather than something this crate re-implements.
#[test]
fn the_perp_metrics_verb_forwards_the_store_read_and_its_range() {
    let rows = vec![
        PerpMetricRow { ts: 1_000, premium: 0.000_335, open_interest: None },
        PerpMetricRow { ts: 2_000, premium: -0.000_623, open_interest: None },
        PerpMetricRow { ts: 3_000, premium: 0.0, open_interest: None },
    ];
    let ctx = perp_context_over(&rows);

    assert_eq!(ctx.perp_metrics(VENUE, "BTC", TimestampRange::all()).unwrap(), rows);
    let mid = ctx.perp_metrics(VENUE, "BTC", TimestampRange::of(2_000, 2_000)).unwrap();
    assert_eq!(mid.len(), 1);
    assert_eq!(mid[0].ts, 2_000);
    assert!(mid[0].premium < 0.0, "a NEGATIVE premium survives the round trip");

    // A series the store does not hold is EMPTY, not an error — the ordinary uncollected state.
    assert!(ctx.perp_metrics(VENUE, "ETH", TimestampRange::all()).unwrap().is_empty());
    assert!(ctx.perp_metrics("binance", "BTC", TimestampRange::all()).unwrap().is_empty());
}

/// ⚠ The premium is its OWN series, NOT a column on the funding bar — so a store holding perp
/// metrics for a symbol says nothing about that symbol's `interval="funding"` bars, and a study
/// that wants both reads both. This pins the split rather than only documenting it.
#[test]
fn perp_metrics_and_the_funding_bars_are_separate_series() {
    let ctx =
        perp_context_over(&[PerpMetricRow { ts: 1_000, premium: 0.000_335, open_interest: None }]);
    assert_eq!(ctx.perp_metrics(VENUE, "BTC", TimestampRange::all()).unwrap().len(), 1);
    assert!(
        ctx.bars(VENUE, "BTC", "funding", TimestampRange::all()).unwrap().is_empty(),
        "the premium series carries no funding rate — they are filled by two appends"
    );
}

// ---------------------------------------------------------------------------------------------
// the option-chain read verbs
// ---------------------------------------------------------------------------------------------

/// One call of `BTCUSDT` at `strike`, known at `ts`, its mark `mark`.
fn chain_row(ts: i64, strike: f64, mark: f64) -> ChainRow {
    ChainRow {
        ts,
        underlying: "BTCUSDT".into(),
        instrument: format!("BTC-231027-{strike}-C"),
        expiry_ms: 10_000,
        strike,
        kind: OptionKind::Call,
        bid: None,
        ask: Some(mark + 1.0),
        mark: Some(mark),
        iv: Some(0.6),
        open_interest: Some(1.0),
        volume: Some(0.0),
        delta: Some(0.5),
        gamma: Some(0.0001),
        theta: Some(-10.0),
        vega: Some(5.0),
    }
}

/// A context over two hourly snapshots of two calls, the second snapshot missing one of them.
fn chain_context() -> StudyContext {
    let store = HistoryStoreMemory::new();
    let first = [chain_row(3_599, 30_000.0, 100.0), chain_row(3_599, 33_000.0, 10.0)];
    let second = [chain_row(7_199, 30_000.0, 120.0)];
    store.append_chain_snapshot("binance", "BTCUSDT", &first, Some("seed-1")).unwrap();
    store.append_chain_snapshot("binance", "BTCUSDT", &second, Some("seed-2")).unwrap();
    StudyContext::new(Arc::new(store), TimestampRange::all(), std::env::temp_dir())
}

/// `chain` forwards the store's range scan, ts-ascending, an absent bid still absent.
#[test]
fn the_chain_verb_forwards_the_store_read_and_its_range() {
    let ctx = chain_context();
    let all = ctx.chain("binance", "BTCUSDT", TimestampRange::all()).unwrap();
    assert_eq!(all.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![3_599, 3_599, 7_199]);
    assert_eq!(all[0].bid, None);
    assert_eq!(ctx.chain("binance", "BTCUSDT", TimestampRange::of(4_000, 8_000)).unwrap().len(), 1);
    assert!(ctx.chain("binance", "ETHUSDT", TimestampRange::all()).unwrap().is_empty());
}

/// `chain_as_of` is POINT-IN-TIME: before the first snapshot nothing is known, between the two each
/// option answers its first row, and after the second each answers its LAST row inside the lookback.
/// A row stamped after the decision time never answers.
#[test]
fn the_chain_as_of_verb_never_reads_a_row_known_after_its_time() {
    let ctx = chain_context();
    assert!(ctx.chain_as_of("binance", "BTCUSDT", 3_598, 10_000).unwrap().is_empty());
    let between = ctx.chain_as_of("binance", "BTCUSDT", 7_198, 10_000).unwrap();
    assert_eq!(
        between.iter().map(|r| (r.strike, r.mark)).collect::<Vec<_>>(),
        vec![(30_000.0, Some(100.0)), (33_000.0, Some(10.0))]
    );
    let after = ctx.chain_as_of("binance", "BTCUSDT", 7_199, 10_000).unwrap();
    assert_eq!(
        after.iter().map(|r| (r.strike, r.mark)).collect::<Vec<_>>(),
        vec![(30_000.0, Some(120.0)), (33_000.0, Some(10.0))]
    );
    let narrow = ctx.chain_as_of("binance", "BTCUSDT", 7_199, 1_000).unwrap();
    assert_eq!(narrow.len(), 1, "the lookback drops an option not seen inside it");
}

const HOUR: i64 = 3_600_000;

/// One account fill of `trade_id` at `ts`, a fee of 1 USD stated with it.
fn account_fill(trade_id: &str, ts: i64) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: trade_id.into(),
        order_client_id: format!("c-{trade_id}"),
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 10.0,
        px: 200.0,
        commission: 1.0,
        mark_price: None,
        liquidity_side: String::new(),
        commission_asset: "USD".into(),
        venue_order_id: None,
        account: None,
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    }
}

/// The venue's later statement of `fill`'s fee, `extra` over it, as the materializer writes it: stamped
/// at the FILL's time, received at `received_ms`.
fn fee_correction(fill: &ExecFillRow, extra: f64, received_ms: i64) -> ExecFillRow {
    let revision = vike_model::events::FeeAdjustmentEvent {
        trade_id: vike_model::events::TradeId::new(&fill.trade_id).unwrap(),
        order_client_id: fill.order_client_id.clone(),
        venue: fill.venue.as_str().into(),
        symbol: fill.symbol.as_str().into(),
        last_price: fill.px,
        extra_commission: extra,
        commission_asset: "USD".into(),
        timestamp: fill.ts,
    };
    ExecFillRow::fee_correction_row(&revision, 1, received_ms, None, None)
}

/// **A study never folds a fee its read's end did not know** (the lookahead review's finding 10): a fill
/// at 10:00 whose commission the venue revises at 18:00. The correction row is stamped at its FILL's
/// time, so the range alone handed it to a read ending at 12:00. A correction written before its row
/// carried a receipt time (`t2`'s) is never known early, and a read with no end knows every one.
#[test]
fn a_fee_correction_received_after_the_read_ends_is_not_folded() {
    let (revised, old) = (account_fill("t1", 10 * HOUR), account_fill("t2", 11 * HOUR));
    let rows = [
        revised.clone(),
        fee_correction(&revised, 0.5, 18 * HOUR),
        old.clone(),
        ExecFillRow { received_ms: None, ..fee_correction(&old, 0.25, 0) },
    ];
    let store = HistoryStoreMemory::new();
    store.append_exec_fills("ibkr", "AAPL", &rows, Some("seed")).unwrap();
    let ctx = StudyContext::new(Arc::new(store), TimestampRange::all(), std::env::temp_dir());
    let fees = |range: TimestampRange| -> Vec<f64> {
        let rows = ctx.execution_fills("ibkr", "AAPL", range).unwrap();
        ExecFillRow::fill_events(&rows).iter().map(|f| f.commission).collect()
    };
    let through = |end: i64| TimestampRange { start: None, end: Some(end) };
    assert_eq!(fees(through(12 * HOUR)), [1.0, 1.0], "the fees stated with the fills");
    assert_eq!(
        fees(through(18 * HOUR)),
        [1.5, 1.0],
        "the revision, from the instant it was received"
    );
    assert_eq!(fees(through(19 * HOUR)), [1.5, 1.0], "an unknown receipt is never known");
    assert_eq!(
        fees(TimestampRange::all()),
        [1.5, 1.25],
        "a read with no end knows every correction"
    );
}

/// The host-side refusals print as ONE sentence: a string continuation, not an embedded newline and
/// a run of indentation (`NoSim` once carried a literal `\n` where `NoLearner` had a `\`).
#[test]
fn the_host_refusals_display_as_one_line() {
    for e in [StudyError::NoLearner("x".into()), StudyError::NoSim("x".into())] {
        let text = e.to_string();
        assert!(!text.contains('\n'), "{text:?}");
        assert!(!text.contains("  "), "{text:?}");
    }
}

/// A store read failure travels as itself, so a caller can tell a query fault from an I/O one
/// without parsing a sentence — and `?` works inside a study body because of the `From` impl.
#[test]
fn a_store_error_converts_and_keeps_its_source() {
    let e: StudyError = vike_data::HistoryStoreError::Io("disk went away".into()).into();
    assert_matches!(e, StudyError::Data(_));
    assert!(e.to_string().contains("disk went away"), "{e}");
    assert!(std::error::Error::source(&e).is_some(), "the HistoryStoreError stays reachable");
    assert!(std::error::Error::source(&StudyError::Study("x".into())).is_none());
}
