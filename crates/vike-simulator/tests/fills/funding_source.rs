//! Gate for the opt-in [`StrategyEngineConfig::funding_source`] seam: a perp held across a funding window
//! with a `funding_source` supplied is CHARGED funding; the same run WITHOUT the source charges
//! ZERO (the pre-seam behavior, where `Bar::funding` is the only source and no production data path
//! sets it). Also pins the precedence rule: a recorded `Bar::funding` WINS and the source is not
//! consulted for that step. Mirrors the run-construction pattern in `properties_fills.rs`.
use std::sync::Arc;

use vike_marketdata::Bar;
use vike_marketdata::FundingRatePoint;
use vike_simulator::{StrategyEngine, StrategyEngineConfig};

use crate::properties_fills::OpenClose;

/// The `StrategyEngineConfig::funding_source` closure type — `(venue, symbol, timestamp_ms) -> Some(rate)`.
type FundingSource = Arc<dyn Fn(&str, &str, i64) -> Option<f64> + Send + Sync>;

const BASE_TIMESTAMP: i64 = 1_700_000_000_000;
const STEP_MS: i64 = 60_000;

/// Flat-price bars (so the only equity mover is funding), with an optional per-index `Bar::funding`.
fn mk_bars(symbol: &str, n: usize, price: f64, bar_funding: &[(usize, f64)]) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let funding = bar_funding.iter().find(|(index, _)| *index == i).map(|(_, r)| *r);
            Bar {
                timestamp: BASE_TIMESTAMP + i as i64 * STEP_MS,
                open: price,
                high: price,
                low: price,
                close: price,
                volume: 0.0,
                funding,
                bid: None,
                ask: None,
                symbol: Some(symbol.into()),
            }
        })
        .collect()
}

fn params(funding_source: Option<FundingSource>) -> StrategyEngineConfig {
    StrategyEngineConfig {
        cash: 10_000.0,
        default_venue: Some("TEST".into()),
        funding_source,
        ..Default::default()
    }
}

fn run(bar_funding: &[(usize, f64)], source: Option<f64>, source_at_timestamp: i64) -> f64 {
    let symbol_name = "PERP0";
    // idx0: submit open (fills idx1). Held over idx1..=idx3. idx3: submit close (fills idx4).
    let bars = mk_bars(symbol_name, 5, 100.0, bar_funding);
    let series = vec![(symbol_name.to_string(), bars)];
    let strat =
        OpenClose { symbol: symbol_name.to_string(), open_at: 0, close_at: 3, side: 1, size: 2.0 };
    let src: Option<FundingSource> = source.map(|rate| {
        let f: FundingSource =
            Arc::new(move |_v, _s, ts| if ts == source_at_timestamp { Some(rate) } else { None });
        f
    });
    StrategyEngine::new(series, strat, params(src)).run().final_equity
}

/// A long held across a single funding window: WITH a `funding_source` it pays funding; WITHOUT it
/// pays nothing (byte-identical to the pre-seam engine). Flat price + zero fees means the entire
/// equity gap is the funding charge.
#[test]
fn funding_source_charges_held_perp_and_absence_is_zero() {
    // The funding event is stamped at the third bar's ts (index2), a step the 2.0-long is held.
    let funding_timestamp = BASE_TIMESTAMP + 2 * STEP_MS;
    // linear(1.0).funding(size=2.0, mark=100.0, rate=0.01) = 2.0 * 100 * 0.01 = 2.0.
    let expected_charge = 2.0 * 100.0 * 0.01;

    let no_source = run(&[], None, funding_timestamp);
    let with_source = run(&[], Some(0.01), funding_timestamp);

    // No source -> flat price, no fees, no funding -> equity is exactly the starting cash.
    assert!((no_source - 10_000.0).abs() < 1e-9, "unfunded equity={no_source}");
    // Source supplied -> the long paid one funding charge.
    assert!(
        (with_source - (10_000.0 - expected_charge)).abs() < 1e-9,
        "funded equity={with_source}, expected={}",
        10_000.0 - expected_charge
    );
    assert!(with_source < no_source, "funded run must have LOWER equity than the unfunded run");
}

/// Precedence: a recorded `Bar::funding` is the on-the-bar truth and WINS — the source is not
/// consulted for that step. Here the bar carries 0.02 at index2 and the source would return 0.01;
/// the charge reflects the BAR rate, proving the bar wins.
#[test]
fn bar_funding_wins_over_source() {
    let funding_timestamp = BASE_TIMESTAMP + 2 * STEP_MS;
    // Bar carries rate 0.02 at index2; the source would offer 0.01 at the same ts.
    let with_bar_and_source = run(&[(2, 0.02)], Some(0.01), funding_timestamp);
    // linear(1.0).funding(2.0, 100.0, 0.02) = 4.0 -> the BAR rate, not the source's 2.0.
    let expected = 10_000.0 - (2.0 * 100.0 * 0.02);
    assert!(
        (with_bar_and_source - expected).abs() < 1e-9,
        "equity={with_bar_and_source}, expected={expected} (bar rate must win)"
    );
}

/// The net funding cashflow is surfaced DISTINCTLY on [`TradingRecord::funding_paid`] (the backtest
/// twin of `vike_execution::Account.funding_paid`), not merely folded into `final_equity`. A long paying a
/// positive-rate funding charge shows a NEGATIVE `funding_paid` equal to the equity it cost; and with
/// NO source `funding_paid` is exactly `0.0` (the byte-identical no-funding path).
#[test]
fn funding_paid_is_surfaced_on_the_result() {
    let symbol_name = "PERP0";
    let funding_timestamp = BASE_TIMESTAMP + 2 * STEP_MS;
    let build = |src: Option<FundingSource>| {
        let bars = mk_bars(symbol_name, 5, 100.0, &[]);
        let series = vec![(symbol_name.to_string(), bars)];
        let strat = OpenClose {
            symbol: symbol_name.to_string(),
            open_at: 0,
            close_at: 3,
            side: 1,
            size: 2.0,
        };
        StrategyEngine::new(series, strat, params(src)).run()
    };

    let src: FundingSource =
        Arc::new(move |_v, _s, ts| if ts == funding_timestamp { Some(0.01) } else { None });
    let funded = build(Some(src));
    // linear(1.0).funding(2.0, 100.0, 0.01) = 2.0; the long PAYS it → funding_paid = -2.0.
    let expected_charge = 2.0 * 100.0 * 0.01;
    assert!(
        (funded.funding_paid - (-expected_charge)).abs() < 1e-9,
        "funding_paid={}, expected {}",
        funded.funding_paid,
        -expected_charge
    );
    // Flat price + zero fees ⇒ funding is the WHOLE equity gap: funding_paid == final_equity − cash0.
    assert!(
        (funded.funding_paid - (funded.final_equity - 10_000.0)).abs() < 1e-9,
        "funding_paid must equal the equity gap"
    );

    // Absent a source, funding_paid is EXACTLY 0.0 (no funding charged, byte-identical path).
    let unfunded = build(None);
    assert_eq!(unfunded.funding_paid, 0.0, "no source ⇒ funding_paid is exactly 0.0");
}

// --- `StrategyEngineConfig::funding_schedule`: the venue's own instants, on the position held AT them ---

fn charge(timestamp_index: i64, rate: f64, mark: Option<f64>) -> FundingRatePoint {
    FundingRatePoint {
        timestamp_ms: BASE_TIMESTAMP + timestamp_index * STEP_MS,
        rate,
        premium: None,
        mark,
    }
}

/// The OpenClose run (open submitted idx0, filled at idx1's open; close submitted idx3, filled at
/// idx4's open) over bars opening at `open` and closing at `close`, with a funding schedule.
fn run_scheduled(
    open: f64,
    close: f64,
    schedule: Vec<FundingRatePoint>,
    bar_funding: &[(usize, f64)],
) -> f64 {
    let symbol_name = "PERP0";
    let mut bars = mk_bars(symbol_name, 6, open, bar_funding);
    for b in &mut bars {
        b.close = close;
    }
    let strat =
        OpenClose { symbol: symbol_name.to_string(), open_at: 0, close_at: 3, side: 1, size: 2.0 };
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        funding_schedule: vec![(symbol_name.to_string(), schedule.into())],
        ..Default::default()
    };
    StrategyEngine::new(vec![(symbol_name.to_string(), bars)], strat, params).run().funding_paid
}

/// The venue charges the position held AT the instant, which is the one from before the bar's fills:
/// the entry filled at idx1's open does not pay idx1's funding, the exit filled at idx4's open does
/// pay idx4's (the bar rule charged them the other way round).
#[test]
fn a_scheduled_funding_charges_the_position_from_before_the_bars_fills() {
    let at_entry = run_scheduled(100.0, 100.0, vec![charge(1, 0.01, None)], &[]);
    assert_eq!(at_entry, 0.0, "flat at the instant: nothing owed");
    let at_exit = run_scheduled(100.0, 100.0, vec![charge(4, 0.01, None)], &[]);
    assert!(
        (at_exit - (-2.0)).abs() < 1e-9,
        "long 2 at the instant: 2 x 100 x 0.01, got {at_exit}"
    );
}

/// Valued at the charge's mark, else at the bar's OPEN (the price at the instant), never the close.
#[test]
fn a_scheduled_funding_is_valued_at_its_mark_else_the_open() {
    let at_mark = run_scheduled(100.0, 110.0, vec![charge(2, 0.01, Some(105.0))], &[]);
    assert!((at_mark - (-2.0 * 105.0 * 0.01)).abs() < 1e-9, "{at_mark}");
    let at_open = run_scheduled(100.0, 110.0, vec![charge(2, 0.01, None)], &[]);
    assert!((at_open - (-2.0 * 100.0 * 0.01)).abs() < 1e-9, "{at_open}");
}

/// An instant between two bars is charged at the next bar, every one of several is charged, and a
/// scheduled symbol's `Bar.funding` is the strategy's to read, never a second charge.
#[test]
fn every_instant_is_charged_once_and_bar_funding_is_signal_only() {
    let between = vec![
        FundingRatePoint {
            timestamp_ms: BASE_TIMESTAMP + 2 * STEP_MS - 1,
            rate: 0.01,
            premium: None,
            mark: None,
        },
        FundingRatePoint {
            timestamp_ms: BASE_TIMESTAMP + 2 * STEP_MS,
            rate: 0.01,
            premium: None,
            mark: None,
        },
    ];
    let got = run_scheduled(100.0, 100.0, between, &[(2, 0.5)]);
    assert!((got - (-4.0)).abs() < 1e-9, "two instants due by idx2 at 2 x 100 x 0.01 each: {got}");
}

/// The tick lane charges the schedule on the symbol's first tick at or after the instant, before
/// that tick's fills, at the tick's price; it charged nothing at all before.
#[test]
fn the_tick_lane_charges_the_schedule() {
    use vike_marketdata::TickQuote;
    use vike_simulator::TickEvent;
    let symbol_name = "PERP0";
    let ticks: Vec<TickEvent> = (0..8)
        .map(|i| {
            TickEvent::Quote(TickQuote {
                timestamp: BASE_TIMESTAMP + i * 1_000,
                local_timestamp: 0,
                bid: 99.0,
                ask: 101.0,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: symbol_name.into(),
            })
        })
        .collect();
    let schedule = vec![FundingRatePoint {
        timestamp_ms: BASE_TIMESTAMP + 4_500,
        rate: 0.01,
        premium: None,
        mark: None,
    }];
    let params = StrategyEngineConfig {
        cash: 10_000.0,
        fill_model: vike_simulator::FillModelKind::Tick,
        funding_schedule: vec![(symbol_name.to_string(), schedule.into())],
        ..Default::default()
    };
    let strat = TickBuyer { symbol: symbol_name.to_string(), size: 2.0, done: false };
    let mut e = StrategyEngine::new(vec![(symbol_name.to_string(), Vec::new())], strat, params);
    let r = e.run_ticks(&[(symbol_name.to_string(), ticks)]);
    assert!(
        (r.funding_paid - (-2.0)).abs() < 1e-9,
        "2 x mid 100 x 0.01 at the 5s tick: {}",
        r.funding_paid
    );
}

/// Buys `size` on its first quote tick.
struct TickBuyer {
    symbol: String,
    size: f64,
    done: bool,
}

impl vike_model::Strategy<vike_simulator::OrderBrokerSimulator> for TickBuyer {
    fn on_quote_tick(
        &mut self,
        ctx: &mut vike_simulator::OrderBrokerSimulator,
        _q: &vike_marketdata::TickQuote,
    ) {
        if !self.done {
            self.done = true;
            ctx.submit(&self.symbol, 1, self.size, 0.0, true, None);
        }
    }
}
