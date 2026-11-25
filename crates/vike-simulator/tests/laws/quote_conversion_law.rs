//! The QUOTE-CURRENCY CONVERSION law, backtest side (decision 0243, items 2 to 4;
//! `crates/vike-simulator/src/engine/sim_broker/conversion.rs`'s module doc is the rule): a symbol quoted in
//! another currency than the run's ledger is priced in its own currency and BOOKED at the rate of the
//! [`vike_simulator::ConversionSeries`] that names it. Pinned:
//!
//! 1. **the worked example**: EURJPY on a dollar ledger, rated by a USDJPY series. 1,000 EURJPY are bought
//!    at 160.000 while USDJPY stands at 150, and sold at 161.000 after USDJPY moved to 160. The realized
//!    PnL is the 1,000 yen of quote PnL at the CLOSE-time rate, 1,000 / 160 = 6.25 dollars (never
//!    1,000 / 150 = 6.67, the entry rate's, and never 1,000 "dollars", par); each fee is its own yen at the
//!    rate of its own fill; the open position is marked at the rate NOW (a margin account: the entry rate
//!    is stored nowhere);
//! 2. an OPENING order is refused `quote-rate-stale` before the pair's first close and while the rate is
//!    older than twice the pair's interval; a covered reduce is never held up;
//! 3. a rate of ONE books the bytes of a run that converts nothing, the gate armed and a sizer at work
//!    (every dollar-quoted symbol runs the empty table itself, which the parity fixtures and `r7_gate` hold);
//! 4. a percent-of-equity order sizes in the ledger's money: dollars over the yen price times the rate;
//! 5. the tick lane books each fee at the rate in force at its own tick.

use vike_analytics::TradingRecord;
use vike_marketdata::{Bar, TickQuote};
use vike_model::{AccountPerQuote, AccountPerQuoteLeg, Fill, OrderBroker, Strategy};
use vike_simulator::{
    ConversionSeries, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig,
};

const EURJPY: &str = "EURJPY";
const HOUR: i64 = 3_600_000;
const TAKER: f64 = 0.0001;
/// Within this of the hand-written dollar figures (they are written as decimals, the engine's are
/// products of a binary `1/150`).
const CENT_FRACTION: f64 = 1e-9;

type Act = Box<dyn Fn(&mut OrderBrokerSimulator)>;

/// Runs each `(bar index, action)` from that bar's `on_bar`, and records every fill.
struct Steps {
    steps: Vec<(usize, Act)>,
    fills: Vec<Fill>,
    /// Whether the tick hook has sent its one action (the first step's).
    ticked: bool,
}

impl Steps {
    fn new(steps: Vec<(usize, Act)>) -> Self {
        Steps { steps, fills: Vec::new(), ticked: false }
    }
}

impl Strategy<OrderBrokerSimulator> for Steps {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        for (at, act) in &self.steps {
            if *at == ctx.index {
                act(ctx);
            }
        }
    }
    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, _q: &TickQuote) {
        if let Some((_, act)) = self.steps.first()
            && !self.ticked
        {
            self.ticked = true;
            act(ctx);
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

fn act(f: impl Fn(&mut OrderBrokerSimulator) + 'static) -> Act {
    Box::new(f)
}

/// A market order of `qty` (signed by `side`), raw (no sizer).
fn market(side: i32, qty: f64) -> Act {
    act(move |b| b.submit(EURJPY, side, qty, 0.0, true, None))
}

/// USDJPY at `mid` as the dollar ledger's rate for a yen quote: `1/mid` dollars per yen
/// (`AccountPerQuote::of_mid`, the one orientation rule).
fn usdjpy(mid: f64) -> AccountPerQuote {
    AccountPerQuote::of_mid(AccountPerQuoteLeg::AccountIsBase, mid).expect("a rate")
}

/// The pair `name` of `interval_ms` bars, each `(opened at, close)`, booking [`EURJPY`]: its rate at `t`
/// is the close of the last of them that had CLOSED by `t` (`opened at + interval_ms <= t`).
fn pair(
    name: &str,
    leg: AccountPerQuoteLeg,
    interval_ms: i64,
    bars: &[(i64, f64)],
) -> ConversionSeries {
    let bar = |&(ts, close): &(i64, f64)| {
        vike_simulator::test_support::bar(ts, close, close, close, close, 1.0)
    };
    ConversionSeries {
        quote_currency: "JPY".into(),
        pair: format!("test:{name}:1h"),
        leg,
        interval_ms,
        bars: std::sync::Arc::new(bars.iter().map(bar).collect()),
        symbols: vec![EURJPY.into()],
        par_where_missing: false,
    }
}

/// USDJPY hourly bars: on the dollar ledger a yen is worth `1/close` dollars.
fn usdjpy_hourly(bars: &[(i64, f64)]) -> ConversionSeries {
    pair("USDJPY", AccountPerQuoteLeg::AccountIsBase, HOUR, bars)
}

fn bar(i: i64, open: f64, close: f64) -> Bar {
    let (high, low) = (open.max(close), open.min(close));
    vike_simulator::test_support::bar(i * HOUR, open, high, low, close, 1_000_000.0)
}

fn params(conversion: ConversionSeries) -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 10_000.0,
        account_currency: Some("USD".into()),
        conversions: vec![conversion],
        maker_fee: Some(0.0),
        taker_fee: Some(TAKER),
        ..StrategyEngineConfig::default()
    }
}

fn run(
    p: StrategyEngineConfig,
    bars: Vec<Bar>,
    steps: Vec<(usize, Act)>,
) -> (StrategyEngine<Steps>, TradingRecord) {
    let mut eng = StrategyEngine::new(vec![(EURJPY.into(), bars)], Steps::new(steps), p);
    let result = eng.run();
    (eng, result)
}

fn near(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() <= CENT_FRACTION, "{what}: {got} vs the hand figure {want}");
}

/// The worked example (item 1 of the module doc), step by step:
///
/// | bar (open at) | EURJPY open / close | USDJPY in force | what happens |
/// |---|---|---|---|
/// | 0 (1h) | 160.0 / 160.0 | 150 | the strategy buys 1,000 |
/// | 1 (2h) | 160.0 / 160.5 | 150 | filled at 160.0; fee 16 yen = 16/150 dollars |
/// | 2 (3h) | 160.4 / 160.8 | 160 (its 2h bar closed at 3h) | marked at 1/160; the strategy sells |
/// | 3 (4h) | 161.0 / 161.0 | 160 | filled at 161.0; fee 16.1 yen = 16.1/160; realized 1,000/160 |
#[test]
fn a_eurjpy_round_trip_on_a_dollar_ledger_books_dollars_at_the_close_time_rate() {
    let usdjpy_bars =
        usdjpy_hourly(&[(0, 150.0), (HOUR, 150.0), (2 * HOUR, 160.0), (3 * HOUR, 160.0)]);
    let bars = vec![
        bar(1, 160.0, 160.0),
        bar(2, 160.0, 160.5),
        bar(3, 160.4, 160.8),
        bar(4, 161.0, 161.0),
    ];
    let (eng, result) =
        run(params(usdjpy_bars), bars, vec![(0, market(1, 1_000.0)), (2, market(-1, 1_000.0))]);

    // The fills are the fill law's, in YEN prices; their fees are booked in DOLLARS.
    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "a round trip: {fills:?}");
    assert_eq!((fills[0].price, fills[0].timestamp), (160.0, 2 * HOUR));
    assert_eq!((fills[1].price, fills[1].timestamp), (161.0, 4 * HOUR));
    // Bit for bit the yen fee times the rate of its own fill (`AccountPerQuote::to_account`).
    assert_eq!(
        fills[0].fee.to_bits(),
        usdjpy(150.0).to_account(1_000.0 * 160.0 * TAKER * 1.0).to_bits()
    );
    assert_eq!(
        fills[1].fee.to_bits(),
        usdjpy(160.0).to_account(1_000.0 * 161.0 * TAKER * 1.0).to_bits()
    );
    near(fills[0].fee, 16.0 / 150.0, "the entry fee");
    near(fills[1].fee, 0.100_625, "the exit fee");

    // Realized at the CLOSE-time rate: 1,000 yen / 160, not / 150 and not par.
    assert_eq!(result.trades.len(), 1);
    near(result.trades[0].pnl, 6.25, "the realized PnL");
    near(result.trades[0].fees, 16.0 / 150.0 + 0.100_625, "the trade's fees");

    // The open position at the end of bar 2 is marked at the rate NOW (1/160): 0.8 yen x 1,000 / 160.
    // The entry-rate (cash-holding) reading would book 160.8 x 1,000 / 160 - 160 x 1,000 / 150, about
    // -61.67 dollars, which is the FX revaluation of the notional a margin account never books.
    near(result.equity_curve[1], 10_000.0 - 16.0 / 150.0 + 0.5 * 1_000.0 / 150.0, "bar 1's mark");
    near(result.equity_curve[2], 10_000.0 - 16.0 / 150.0 + 0.8 * 1_000.0 / 160.0, "bar 2's mark");
    near(result.final_equity, 10_000.0 - 16.0 / 150.0 - 0.100_625 + 6.25, "the final equity");
    near(result.fees_paid, 16.0 / 150.0 + 0.100_625, "the fees paid");
    assert_eq!(eng.core.position_of(EURJPY).size, 0.0);
}

/// An opening order is refused while the rate is stale; a covered reduce is not.
#[test]
fn a_stale_rate_refuses_an_opening_order_and_never_holds_up_a_reduce() {
    // One hourly bar, closed at 0h, good for twice its interval: bar 0 (1h) may open, bar 2 (3h) may not
    // add, bar 3 (4h) may reduce.
    let bars = (1..=5).map(|i| bar(i, 160.0, 160.0)).collect();
    let (eng, _) = run(
        params(usdjpy_hourly(&[(-HOUR, 150.0)])),
        bars,
        vec![(0, market(1, 1_000.0)), (2, market(1, 1_000.0)), (3, market(-1, 500.0))],
    );
    let dropped: Vec<(&str, f64)> =
        eng.core.dropped.iter().map(|d| (d.reason.as_str(), d.size)).collect();
    assert_eq!(dropped, [("quote-rate-stale", 1_000.0)], "the add is refused, the reduce is not");
    assert_eq!(eng.core.position_of(EURJPY).size, 500.0);
}

/// Before the pair's first close there is no rate: nothing opens (fail closed, never par).
#[test]
fn no_rate_yet_refuses_an_opening_order() {
    let bars = (1..=3).map(|i| bar(i, 160.0, 160.0)).collect();
    let later = usdjpy_hourly(&[(10 * HOUR, 150.0)]);
    let (eng, _) = run(params(later), bars, vec![(0, market(1, 1_000.0))]);
    assert_eq!(eng.core.dropped.len(), 1);
    assert_eq!(eng.core.dropped[0].reason, "quote-rate-stale");
    assert!(eng.strategy.fills.is_empty());
}

/// A percent-of-equity order is sized in the ledger's money: 50 % of 10,000 dollars over one unit's
/// worth, 160 yen at 1/150 dollar per yen, is 4,687.5 units (never 5,000 / 160 = 31.25, the yen reading).
#[test]
fn a_percent_order_sizes_dollars_over_the_converted_price() {
    let bars = (1..=3).map(|i| bar(i, 160.0, 160.0)).collect();
    let target = act(|b| b.strategy_order_target_percent(EURJPY, 0.5));
    let (eng, _) = run(params(usdjpy_hourly(&[(-HOUR, 150.0)])), bars, vec![(0, target)]);
    near(eng.core.position_of(EURJPY).size, 5_000.0 / (160.0 / 150.0), "the sized position");
}

/// A SMA cross over a generated walk, a percent sizer and the margin gate armed: what a rate of one
/// converts must be nothing, to the bit.
struct Cross {
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for Cross {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        let closes: Vec<f64> = ctx.bars(EURJPY).iter().map(|b| b.close).collect();
        if closes.len() < 20 {
            return;
        }
        let mean = |w: &[f64]| w.iter().sum::<f64>() / w.len() as f64;
        let up = mean(&closes[closes.len() - 5..]) > mean(&closes[closes.len() - 20..]);
        ctx.strategy_order_target_percent(EURJPY, if up { 0.8 } else { -0.8 });
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[test]
fn a_rate_of_one_books_the_bytes_of_a_run_that_converts_nothing() {
    let bars = vike_simulator::test_support::gen_bars(400, None);
    let base = || StrategyEngineConfig {
        cash: 10_000.0,
        slippage: 0.0002,
        maker_fee: Some(0.0002),
        taker_fee: Some(0.0007),
        leverage: Some(2.0),
        ..StrategyEngineConfig::default()
    };
    // A pair of ten-minute bars, every close 1.0 (`QUOTE/LEDGER`: the rate is the close), never stale.
    let ones: Vec<(i64, f64)> = (-1..41).map(|k| (k * 600_000, 1.0)).collect();
    let at_par = pair("JPYUSD", AccountPerQuoteLeg::QuoteIsBase, 600_000, &ones);
    let converted = StrategyEngineConfig { conversions: vec![at_par], ..base() };
    let mut plain = StrategyEngine::new(
        vec![(EURJPY.into(), bars.clone())],
        Cross { fills: Vec::new() },
        base(),
    );
    let mut at_one =
        StrategyEngine::new(vec![(EURJPY.into(), bars)], Cross { fills: Vec::new() }, converted);
    let (a, b) = (plain.run(), at_one.run());
    assert!(a.trades.len() > 5, "the run must trade: {} trades", a.trades.len());
    let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<u64>>();
    assert_eq!(bits(&a.equity_curve), bits(&b.equity_curve), "the equity curve");
    assert_eq!(a.final_equity.to_bits(), b.final_equity.to_bits(), "the final equity");
    assert_eq!(a.fees_paid.to_bits(), b.fees_paid.to_bits(), "the fees");
    assert_eq!(a.trades, b.trades, "the trades");
    assert_eq!(plain.strategy.fills, at_one.strategy.fills, "the fills");
    assert_eq!(a.simulator().unwrap().dropped, b.simulator().unwrap().dropped, "the refusals");
}

/// On the tick lane each fee is booked at the rate in force at its own tick.
#[test]
fn the_tick_lane_books_a_fee_at_the_rate_of_its_tick() {
    // Hourly USDJPY bars closing at 0 (150) and at 2.5 s (160).
    let usdjpy_bars = usdjpy_hourly(&[(-HOUR, 150.0), (2_500 - HOUR, 160.0)]);
    let mut p = params(usdjpy_bars);
    p.cash = 1_000_000.0;
    let mut eng = StrategyEngine::new(
        vec![(EURJPY.into(), Vec::new())],
        Steps::new(vec![(0, market(1, 1_000.0))]),
        p,
    );
    let quote = |ts, px| vike_simulator::test_support::quote(EURJPY, ts, px, px + 0.01, 1e9, 1e9);
    eng.run_ticks(&[(
        EURJPY.to_string(),
        vec![quote(1_000, 160.0), quote(2_000, 160.2), quote(3_000, 160.4)],
    )]);
    let fills = &eng.strategy.fills;
    assert!(!fills.is_empty(), "the order fills on the tape");
    for f in fills {
        let rate = if f.timestamp < 2_500 { usdjpy(150.0) } else { usdjpy(160.0) };
        let yen = f.size * f.price * TAKER * 1.0;
        assert_eq!(f.fee.to_bits(), rate.to_account(yen).to_bits(), "the fee of {f:?}");
    }
}

/// The declared exception (decision 0243, amended 2026-11-16): a pair marked `par_where_missing` books one
/// unit of the quote as one of the ledger before its first close, so an order opens there and is booked at
/// one; from the first close on, its closes book as any pair's.
#[test]
fn a_par_pair_opens_at_one_before_its_first_close() {
    let bars = (1..=3).map(|i| bar(i, 160.0, 160.0)).collect();
    let later =
        ConversionSeries { par_where_missing: true, ..usdjpy_hourly(&[(10 * HOUR, 150.0)]) };
    let (eng, _) = run(params(later), bars, vec![(0, market(1, 1_000.0))]);
    assert!(eng.core.dropped.is_empty(), "{:?}", eng.core.dropped);
    assert_eq!(eng.core.position_of(EURJPY).size, 1_000.0);
}
