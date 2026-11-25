//! `[data.options]` end to end (`crates/vike-backtest/src/run/dispatch/options.rs`'s module doc is the
//! contract): option legs read from a stored chain trade on the event lane beside their underlying, an
//! order on a leg fills only at a row the chain holds (no fill without a price), and a leg still held at
//! its expiry settles to cash at its intrinsic value, though its own tape stops before it.
//!
//! The store is deribit-shaped and hand-built: a USDC perpetual's hourly bars as the underlying (a linear
//! series, read through `underlying_interval`), and two weekly coin-book calls whose chain rows are
//! RAGGED (a few priced hours each, stamped at the hour's last millisecond as the derived chain is). The
//! strategy is the shipped file `strategies/options/covered_call.rs`. Every expected number is worked by
//! hand in the test that asserts it.

use std::sync::Arc;

use vike_backtest::run::{profile::ProfileBacktest, run_backtest};
use vike_data::{ChainRow, HistoryStore, HistoryStoreMemory};
use vike_marketdata::{Bar, OptionKind};

const VENUE: &str = "deribit";
const UNDERLYING: &str = "BTCUSDC.P";
const CHAIN: &str = "BTC";
const H: i64 = 3_600_000;
const D: i64 = 24 * H;
/// 2024-01-05 00:00 UTC, a Friday.
const T0: i64 = 1_704_412_800_000;
/// The first call: expires Friday 2024-01-12 08:00 UTC.
const CALL_1: &str = "BTC-12JAN24-55000-C";
const EXPIRY_1: i64 = T0 + 7 * D + 8 * H;
/// The second call: expires Friday 2024-01-19 08:00 UTC.
const CALL_2: &str = "BTC-19JAN24-60000-C";
const EXPIRY_2: i64 = T0 + 14 * D + 8 * H;
/// The half-spread the derived (quote-less) chain trades at, REQUIRED there (review M7): 1/16 of the mark, a
/// binary fraction, so every price below is exact: a call marked 420 sells at 393.75.
const ASSUMED: &str = "assumed_half_spread = 0.0625";

/// The underlying's close for the hour opening at `open`: 50,000 the first week, 56,000 on the first
/// expiry day (so the first call ends 1,000 in the money), 51,000 after (the second ends out of it).
fn underlying_close(open: i64) -> f64 {
    if open < T0 + 7 * D {
        50_000.0
    } else if open < T0 + 8 * D {
        56_000.0
    } else {
        51_000.0
    }
}

fn row(instrument: &str, expiry_ms: i64, strike: f64, ts: i64, mark: f64) -> ChainRow {
    ChainRow {
        ts,
        underlying: CHAIN.to_string(),
        instrument: instrument.to_string(),
        expiry_ms,
        strike,
        kind: OptionKind::Call,
        bid: None,
        ask: None,
        mark: Some(mark),
        iv: Some(0.5),
        open_interest: None,
        volume: Some(1.0),
        delta: None,
        gamma: None,
        theta: None,
        vega: None,
    }
}

/// The underlying's fifteen days of hourly bars, and the two calls' ragged rows (plus one call no
/// profile names, which must not leak in).
fn seed_store() -> Arc<dyn HistoryStore + Send + Sync> {
    let rows = vec![
        row(CALL_1, EXPIRY_1, 55_000.0, T0 + 6 * H - 1, 400.0),
        row("BTC-12JAN24-60000-C", EXPIRY_1, 60_000.0, T0 + 6 * H - 1, 90.0),
        row(CALL_1, EXPIRY_1, 55_000.0, T0 + 11 * H - 1, 420.0),
        row(CALL_1, EXPIRY_1, 55_000.0, T0 + 12 * H - 1, 430.0),
        row(CALL_1, EXPIRY_1, 55_000.0, T0 + 3 * D + 6 * H - 1, 380.0),
        row(CALL_2, EXPIRY_2, 60_000.0, T0 + 5 * D - 1, 250.0),
        row(CALL_2, EXPIRY_2, 60_000.0, T0 + 7 * D + 2 * H - 1, 310.0),
        row(CALL_1, EXPIRY_1, 55_000.0, T0 + 7 * D + 7 * H - 1, 990.0),
        row(CALL_2, EXPIRY_2, 60_000.0, T0 + 7 * D + 10 * H - 1, 300.0),
        row(CALL_2, EXPIRY_2, 60_000.0, T0 + 10 * D - 1, 200.0),
        row(CALL_2, EXPIRY_2, 60_000.0, T0 + 14 * D + 7 * H - 1, 5.0),
    ];
    store_with(&rows)
}

/// The underlying's fifteen days of hourly bars and `rows`, each its own snapshot.
fn store_with(rows: &[ChainRow]) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    let bars: Vec<Bar> = (0..15 * 24)
        .map(|i| {
            let open = T0 + i * H;
            let close = underlying_close(open);
            vike_marketdata::test_support::flat_bar_unit_volume_on(UNDERLYING, open, close)
        })
        .collect();
    store.append_bars(VENUE, UNDERLYING, "1h", &bars, None).unwrap();
    for r in rows {
        store.append_chain_snapshot(VENUE, CHAIN, std::slice::from_ref(r), None).unwrap();
    }
    Arc::new(store)
}

/// The covered call over the two weeks: sell the first call from Friday 08:00, the second from the next.
fn profile(engine_extra: &str, options_extra: &str) -> String {
    format!(
        r#"
[data]
kind = "tick"
venue = "{VENUE}"
symbols = ["{UNDERLYING}"]
from = "2024-01-05"
to = "2024-01-20"

[data.options]
venue = "{VENUE}"
chain = "{CHAIN}"
underlying = "{UNDERLYING}"
underlying_interval = "1h"
legs = ["{CALL_1}", "{CALL_2}"]
{ASSUMED}
{options_extra}

[engine]
cash = 100000.0
{engine_extra}

[strategy]
file = "options/covered_call.rs"
[strategy.params]
underlying = "{UNDERLYING}"
quantity = 1.0
calls = ["{CALL_1}", "{CALL_2}"]
strikes = [55000.0, 60000.0]
expiries = [{EXPIRY_1}, {EXPIRY_2}]
sell_at = [{sell_1}, {sell_2}]
"#,
        sell_1 = T0 + 8 * H,
        sell_2 = T0 + 7 * D + 8 * H,
    )
}

fn run(text: &str) -> Result<vike_analytics::TradingRecord, String> {
    run_on(text, seed_store())
}

fn run_on(
    text: &str,
    store: Arc<dyn HistoryStore + Send + Sync>,
) -> Result<vike_analytics::TradingRecord, String> {
    let profile = ProfileBacktest::from_toml_str(text)
        .map_err(|e| e.to_string())?
        .with_stand_in(vike_strategy_lab::stand_in);
    run_backtest(&profile, store).map_err(|e| e.to_string())
}

/// The whole ledger, by hand (fee-free):
/// - the cover: bought on the underlying's first event, filled at its NEXT one, at 50,000;
/// - the first call: its time comes at Friday 08:00, the order is placed on the first event at or after
///   (the underlying's 08:59:59.999 close), and the chain holds no row of the call until 10:59:59.999, so
///   it fills THERE, on that row's 420 mark, not on the 400 of the last row before (no fill without a
///   price), at the mark less the ASSUMED half-spread of 1/16 (the chain carries no quote): 393.75;
/// - it expires 1,000 in the money (the underlying's last close before 08:00 on the 12th is 56,000), and
///   settles though its last row was an hour before its expiry: cash −1,000;
/// - the second call (the week's only candidate, priced at its 01:59:59.999 row, 310): placed at the 12th's
///   08:59:59.999 close, filled at its next row, marked 300 at 09:59:59.999, so 281.25; it expires out of the
///   money (51,000 < 60,000) and settles at 0;
/// - the end: cash 100,000 − 50,000 + 393.75 − 1,000 + 281.25 = 49,675, plus the cover at 51,000: 100,675.
#[test]
fn a_covered_call_sells_each_leg_at_its_next_priced_row_and_settles_it_at_expiry() {
    let r = run(&profile("", "")).unwrap();
    assert_eq!(r.final_equity, 100_675.0, "{:?}", r.trades);
    assert_eq!(r.n_trades, 2, "the two calls closed (the cover is still held): {:?}", r.trades);
    let first = r.trades.iter().find(|t| t.symbol == CALL_1).expect("the first call traded");
    assert!(!first.is_long);
    assert_eq!(first.entry_price, 393.75, "the next priced row's 420 less 1/16, never 400");
    assert_eq!(first.entry_timestamp, T0 + 11 * H - 1);
    assert_eq!(first.exit_price, 1_000.0, "max(0, 56,000 − 55,000)");
    assert_eq!(first.pnl, 393.75 - 1_000.0);
    let second = r.trades.iter().find(|t| t.symbol == CALL_2).expect("the second call traded");
    assert_eq!(second.entry_price, 281.25, "300 less 1/16");
    assert_eq!(second.exit_price, 0.0, "out of the money");
    assert!(
        r.data_warnings.iter().any(|w| w.contains(CALL_1) && w.contains("dollars")),
        "a coin-book leg says it is booked in dollars: {:?}",
        r.data_warnings
    );
    assert!(
        r.data_warnings
            .iter()
            .any(|w| w.contains("no bid or ask") && w.contains("ASSUMED half-spread")),
        "a chain derived from trades says its legs trade at an assumed spread: {:?}",
        r.data_warnings
    );
}

/// Review M7, the coordinator's rule: a chain that carries NO quote is never traded at its bare mark. Without
/// `assumed_half_spread` the run is refused in words naming the key; a value outside (0, 1) is refused at load.
#[test]
fn a_quote_less_chain_without_an_assumed_half_spread_is_refused() {
    let why = run(&profile("", "").replace(ASSUMED, "")).expect_err("refused");
    assert!(why.contains("assumed_half_spread") && why.contains("bare mark"), "{why}");
    for bad in ["0.0", "1.0", "-0.01"] {
        let text = profile("", "").replace("0.0625", bad);
        let why = run(&text).expect_err("refused");
        assert!(
            why.contains("assumed_half_spread") && why.contains("above 0 and below 1"),
            "{why}"
        );
    }
}

/// A chain that carries the venue's quotes is traded at them (review M7): the seed's rows with a 20-wide
/// quote around each mark, except the first call's 10:59:59.999 row, which holds a bid and no ask and so is
/// NO price. The first call's sale therefore fills at its NEXT two-sided row, 11:59:59.999, at its BID, 420
/// (not that row's 430 mark); the second's at its 09:59:59.999 row's bid, 290. Fee-free, by hand: cash
/// 100,000 − 50,000 + 420 − 1,000 + 290 = 49,710, plus the cover at 51,000: 100,710, and no warning that the
/// spread is missing.
#[test]
fn a_chain_with_quotes_sells_at_the_bid_and_a_one_sided_row_is_no_price() {
    let one_sided = T0 + 11 * H - 1;
    let mut rows = Vec::new();
    for (instrument, expiry, strike, ts, mark) in [
        (CALL_1, EXPIRY_1, 55_000.0, T0 + 6 * H - 1, 400.0),
        ("BTC-12JAN24-60000-C", EXPIRY_1, 60_000.0, T0 + 6 * H - 1, 90.0),
        (CALL_1, EXPIRY_1, 55_000.0, one_sided, 420.0),
        (CALL_1, EXPIRY_1, 55_000.0, T0 + 12 * H - 1, 430.0),
        (CALL_1, EXPIRY_1, 55_000.0, T0 + 3 * D + 6 * H - 1, 380.0),
        (CALL_2, EXPIRY_2, 60_000.0, T0 + 5 * D - 1, 250.0),
        (CALL_2, EXPIRY_2, 60_000.0, T0 + 7 * D + 2 * H - 1, 310.0),
        (CALL_1, EXPIRY_1, 55_000.0, T0 + 7 * D + 7 * H - 1, 990.0),
        (CALL_2, EXPIRY_2, 60_000.0, T0 + 7 * D + 10 * H - 1, 300.0),
        (CALL_2, EXPIRY_2, 60_000.0, T0 + 10 * D - 1, 200.0),
        (CALL_2, EXPIRY_2, 60_000.0, T0 + 14 * D + 7 * H - 1, 5.0),
    ] {
        let ask = (ts != one_sided).then_some(mark + 10.0);
        rows.push(ChainRow {
            bid: Some(mark - 10.0),
            ask,
            ..row(instrument, expiry, strike, ts, mark)
        });
    }
    let r = run_on(&profile("", ""), store_with(&rows)).unwrap();
    let first = r.trades.iter().find(|t| t.symbol == CALL_1).expect("the first call traded");
    assert_eq!(first.entry_price, 420.0, "the next two-sided row's bid");
    assert_eq!(first.entry_timestamp, T0 + 12 * H - 1, "the one-sided row is no price");
    let second = r.trades.iter().find(|t| t.symbol == CALL_2).expect("the second call traded");
    assert_eq!(second.entry_price, 290.0, "its row's bid, never the 300 mark");
    assert_eq!(r.final_equity, 100_710.0, "{:?}", r.trades);
    assert!(r.data_warnings.iter().all(|w| !w.contains("no bid or ask")), "{:?}", r.data_warnings);
    assert!(
        r.data_warnings.iter().any(|w| w.contains("assumed_half_spread") && w.contains("not read")),
        "the profile's assumed spread is said unused on a quoted chain: {:?}",
        r.data_warnings
    );
}

/// A profile that names no commission, over a strategy that states none, pays none on a leg either: no
/// venue table, neither the options row nor the delivery fee at expiry (the owner's ruling of 2026-11-18;
/// a strategy that states deribit's options schedule pays both, `vike_model::Strategy::stated_fee`).
#[test]
fn a_leg_pays_no_venue_fee_when_none_is_named() {
    let r = run(&profile("", "")).unwrap();
    assert_eq!(r.fees_paid, 0.0, "no venue table");
    assert_eq!(r.final_equity, 100_675.0, "the fee-free ledger");
}

/// A leg the chain never prices in the window is no symbol of the run: the run goes ahead and says so.
#[test]
fn a_leg_the_chain_never_prices_is_named_and_left_out() {
    let text = profile("", "").replace(
        &format!("legs = [\"{CALL_1}\", \"{CALL_2}\"]"),
        &format!("legs = [\"{CALL_1}\", \"{CALL_2}\", \"BTC-26JAN24-70000-C\"]"),
    );
    let r = run(&text).unwrap();
    assert_eq!(r.final_equity, 100_675.0, "the priced legs ran as before");
    assert!(
        r.data_warnings
            .iter()
            .any(|w| w.contains("BTC-26JAN24-70000-C") && w.contains("no priced row")),
        "{:?}",
        r.data_warnings
    );
}

/// The refusals at load: the bar lane, an underlying that is no series, a leg that is a series, and a
/// ledger that is not the dollar.
#[test]
fn the_table_is_refused_where_it_cannot_mean_what_it_says() {
    let cases = [
        (profile("", "").replace("kind = \"tick\"", "kind = \"bar\""), "tick-mode only"),
        (
            profile("", "").replace(
                &format!("underlying = \"{UNDERLYING}\"\nunderlying_interval"),
                "underlying = \"ETH_USDC-PERPETUAL\"\nunderlying_interval",
            ),
            "is not a series of this run",
        ),
        (
            profile("", "")
                .replace(&format!("legs = [\"{CALL_1}\""), &format!("legs = [\"{UNDERLYING}\"")),
            "is also a series of the run",
        ),
        (profile("account_currency = \"EUR\"", ""), "must be the dollar"),
    ];
    for (text, words) in cases {
        let why = run(&text).expect_err("refused");
        assert!(why.contains(words), "want {words:?} in: {why}");
    }
}

/// A coin-margined underlying makes the run a coin ledger, and the legs are dollars: refused in words.
#[test]
fn a_coin_ledger_underlying_is_refused() {
    let text = profile("", "").replace(UNDERLYING, "BTCUSD.P");
    let why = run(&text).expect_err("refused");
    assert!(why.contains("coin ledger"), "{why}");
}

/// The strategy PICKS ITS STRIKE BY DELTA (target 0.3) among three calls of the 12th's expiry, by hand.
///
/// It decides at the first event at or after Friday 08:00, the underlying's 08:59:59.999 close (spot
/// 50,000), on the calls' 05:59:59.999 rows. Time to expiry T = (12th 08:00 − 5th 08:59:59.999) / 365 days
/// = 601,200,001 / 31,536,000,000 = 0.0190639 years, so at 50% volatility σ√T = 0.5 · 0.138072 = 0.069036
/// and σ²T/2 = 0.0023830. Each mark below is the Black–Scholes price at σ = 0.5, so its implied
/// volatility is 0.5 and its delta is N(d1), d1 = (ln(50,000/K) + 0.0023830) / 0.069036:
///
/// | strike | d1 | delta N(d1) | price at σ 0.5 (the mark) | gap to 0.3 |
/// |---|---|---|---|---|
/// | 50,000 | 0.0345 | 0.514 | 50,000 · (0.51376 − 0.48624) ≈ 1,376 | 0.214 |
/// | 52,000 | −0.5336 | 0.297 | 50,000 · 0.29681 − 52,000 · 0.27337 ≈ 625 | 0.003 |
/// | 55,000 | −1.3461 | 0.089 | 50,000 · 0.08914 − 55,000 · 0.07852 ≈ 138 | 0.211 |
///
/// (A mark a dollar off moves its implied volatility by about 1/vega ≈ 0.0004 and its delta by less than
/// 0.0002, so the pick does not hang on the table's rounding.)
///
/// So it sells the 52,000 call, which fills at that call's next row, 650 at 10:59:59.999, and expires
/// 4,000 in the money (56,000 − 52,000). Fee-free, over the 5th to the 13th: cash 100,000 − 50,000 (the
/// cover) + 650 − 4,000 = 46,650, plus the cover at 56,000: 102,650.
#[test]
fn the_strategy_sells_the_call_nearest_the_target_delta() {
    let (k50, k52, k55) = ("BTC-12JAN24-50000-C", "BTC-12JAN24-52000-C", CALL_1);
    let rows = [
        row(k50, EXPIRY_1, 50_000.0, T0 + 6 * H - 1, 1_376.0),
        row(k52, EXPIRY_1, 52_000.0, T0 + 6 * H - 1, 625.0),
        row(k55, EXPIRY_1, 55_000.0, T0 + 6 * H - 1, 138.0),
        row(k50, EXPIRY_1, 50_000.0, T0 + 11 * H - 1, 1_450.0),
        row(k52, EXPIRY_1, 52_000.0, T0 + 11 * H - 1, 650.0),
        row(k55, EXPIRY_1, 55_000.0, T0 + 11 * H - 1, 140.0),
    ];
    let text = format!(
        r#"
[data]
kind = "tick"
venue = "{VENUE}"
symbols = ["{UNDERLYING}"]
from = "2024-01-05"
to = "2024-01-13"

[data.options]
venue = "{VENUE}"
chain = "{CHAIN}"
underlying = "{UNDERLYING}"
underlying_interval = "1h"
legs = ["{k50}", "{k52}", "{k55}"]
{ASSUMED}

[engine]
cash = 100000.0

[strategy]
file = "options/covered_call.rs"
[strategy.params]
underlying = "{UNDERLYING}"
target_delta = 0.3
calls = ["{k50}", "{k52}", "{k55}"]
strikes = [50000.0, 52000.0, 55000.0]
expiries = [{EXPIRY_1}, {EXPIRY_1}, {EXPIRY_1}]
sell_at = [{sale}]
"#,
        sale = T0 + 8 * H,
    );
    let r = run_on(&text, store_with(&rows)).unwrap();
    assert_eq!(r.n_trades, 1, "{:?}", r.trades);
    let sold = &r.trades[0];
    assert_eq!(sold.symbol, k52, "the 0.297-delta call");
    assert!(!sold.is_long);
    assert_eq!(sold.entry_price, 609.375, "its next priced row's 650, less the assumed 1/16");
    assert_eq!(sold.exit_price, 4_000.0, "max(0, 56,000 − 52,000)");
    assert_eq!(r.final_equity, 102_609.375);
}

/// **A sweep's option legs are part of the one tape its search prepares** (the legs' events key it,
/// `crates/vike-backtest/src/run/tick_replay/tapes.rs`'s `TapeKey`): each point of a covered-call sweep on the
/// bounded pool is, record for record, that point run alone.
#[test]
fn each_point_of_an_option_leg_sweep_is_that_point_run_alone() {
    use vike_backtest::search::optimize::{SearchEvaluator, SearchEvaluatorStore};
    use vike_backtest::search::paramscan::{
        ParamscanExecution, RankMetric, expand_paramscan_overrides, profile_with_overrides,
    };
    let text = profile("", "\n[paramscan]\nquantity = [1.0, 0.5]\n");
    let base = ProfileBacktest::from_toml_str(&text)
        .expect("the sweep loads")
        .with_stand_in(vike_strategy_lab::stand_in);
    let store = seed_store();
    let (rank, parallel) = (RankMetric::Sharpe, ParamscanExecution::Parallel);
    let sweep =
        SearchEvaluatorStore::classic(&base, Arc::clone(&store), rank, parallel).with_results();
    let points = expand_paramscan_overrides(&base).expect("the grid expands");
    for point in sweep.evaluate(points.clone()) {
        assert!(point.row.error.is_none(), "{:?}: {:?}", point.row.overrides, point.row.error);
    }
    let mut swept = sweep.take_results();
    let mut records = std::collections::BTreeSet::new();
    for point in points {
        let (profile, _) = profile_with_overrides(&base, point.clone());
        let alone = run_backtest(&profile, Arc::clone(&store)).expect("it runs alone");
        let alone = format!("{alone:?}");
        let point_of_sweep = swept.remove(&point).expect("the sweep kept the point");
        assert_eq!(format!("{point_of_sweep:?}"), alone, "{point:?}");
        records.insert(alone);
    }
    assert_eq!(records.len(), 2, "the two quantities part");
}
