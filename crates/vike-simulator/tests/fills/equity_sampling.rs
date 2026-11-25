//! Gate for [`vike_simulator::EquitySampling`] — the opt-in tick-lane equity-curve density knob
//! (perf: the curve is two `Vec`s grown 16 bytes per priced tick, unconditionally, so a 100M-tick
//! sweep point carries 1.6 GB of samples nothing may ever read).
//!
//! The knob is allowed to change ONE thing — the recorded curve — and nothing else. So every test
//! here differences runs of the SAME tape and the SAME strategy across sampling modes:
//!
//! 1. `EveryTick` is the DEFAULT: `StrategyEngineConfig::default()` and an explicit `EveryTick` produce
//!    bit-identical curves, and the curve is one sample per PRICED tick (book events carry no
//!    price semantics and are not sampled) — the frozen shape.
//! 2. `EveryN(n)` keeps a strict SUBSEQUENCE of the default curve — the same `(ts, equity)` pairs
//!    the dense run recorded, at ticks `0, n, 2n, …` — plus a CLOSING sample at the run's last
//!    priced tick, so `equity_curve.last()` still agrees with `final_equity`.
//! 3. `EveryN(0 | 1)` names the same sampling as `EveryTick` and is bit-identical to it.
//! 4. `Off` records nothing at all — both vectors empty.
//! 5. **The run itself never moves.** Across all four modes `final_equity` is bit-identical, and
//!    so are the trade log, `n_trades` and `per_symbol_pnl` — none of them is derived from the
//!    curve. This is the assertion that would fail if the knob had leaked into the fold.
//! 6. `ByTime(ms)` (a tick profile's default since 2026-11-20) keeps the dense curve's FIRST sample and
//!    the last sample of each step, the record's `sampling` holds the dense curve's own maximum
//!    drawdown to the bit (a trough between two closes included), and the report's Sharpe is the
//!    closes' Sharpe at the step. A step of `0` or less is the dense curve.
//!
//! The tape is deliberately ACTIVE (a strategy that opens and closes, so equity genuinely moves
//! tick to tick); a flat curve would make the subsequence claims vacuous.

use vike_analytics::TradingRecord;
use vike_analytics::metrics;
use vike_analytics::results::TradingResults;
use vike_marketdata::{TickQuote, TickTrade};
use vike_model::{OrderBroker, OrderPlacement, Strategy};
use vike_simulator::{
    EquitySampling, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, TickEvent,
};

const SYM: &str = "TOK";
const T0: i64 = 1_700_000_000_000;
/// priced ticks on the tape (quotes + trades); book events are extra and are NOT sampled
const N_PRICED: usize = 60;

fn q(ts: i64, bid: f64, ask: f64) -> TickEvent {
    vike_simulator::test_support::quote(SYM, ts, bid, ask, 50.0, 50.0)
}

fn tr(ts: i64, price: f64) -> TickEvent {
    vike_simulator::test_support::trade(SYM, ts, price, 5.0)
}

/// Alternating quote/trade tape on a zig-zag price, so held inventory is marked to a DIFFERENT
/// price on every tick and the equity curve is genuinely non-constant.
fn tape() -> Vec<TickEvent> {
    (0..N_PRICED)
        .map(|i| {
            let ts = T0 + i as i64 * 1_000;
            let mid = 100.0 + (i % 5) as f64 * 0.25 + (i / 5) as f64 * 0.10;
            if i.is_multiple_of(2) { q(ts, mid - 0.05, mid + 0.05) } else { tr(ts, mid) }
        })
        .collect()
}

/// Buys once early, sells once late — so the curve has a flat prefix, a marked-to-market middle
/// and a flat suffix, and the trade log is non-empty (it is one of the invariants under test).
struct BuyThenSell {
    seen: usize,
}

impl Strategy<OrderBrokerSimulator> for BuyThenSell {
    fn on_quote_tick(&mut self, b: &mut OrderBrokerSimulator, _q: &TickQuote) {
        self.step(b);
    }
    fn on_trade_tick(&mut self, b: &mut OrderBrokerSimulator, _t: &TickTrade) {
        self.step(b);
    }
}

impl BuyThenSell {
    fn step(&mut self, b: &mut OrderBrokerSimulator) {
        self.seen += 1;
        if self.seen == 4 {
            OrderBroker::place(b, SYM, "", 1, 3.0, OrderPlacement::Market, false);
        } else if self.seen == N_PRICED - 6 {
            OrderBroker::place(b, SYM, "", -1, 3.0, OrderPlacement::Market, false);
        }
    }
}

fn run(sampling: EquitySampling) -> TradingRecord {
    run_on(sampling, tape())
}

fn run_on(sampling: EquitySampling, tape: Vec<TickEvent>) -> TradingRecord {
    let params = StrategyEngineConfig { equity_sampling: sampling, ..Default::default() };
    let mut engine =
        StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], BuyThenSell { seen: 0 }, params);
    engine.run_ticks(&[(SYM.to_string(), tape)])
}

/// The step `ByTime` is driven at here: ten of the tape's one-second ticks.
const STEP_MS: i64 = 10_000;

/// What `ByTime(STEP_MS)` must keep of the dense curve, worked from it by hand: its first sample, then the last sample
/// whose time falls in each step, each sample once.
fn closes_of(dense: &TradingRecord) -> (Vec<f64>, Vec<i64>) {
    let ts = &dense.equity_timestamp;
    let mut keep: Vec<usize> = vec![0];
    for i in 0..ts.len() {
        let last_of_step =
            i + 1 == ts.len() || ts[i + 1].div_euclid(STEP_MS) != ts[i].div_euclid(STEP_MS);
        if last_of_step && i != 0 {
            keep.push(i);
        }
    }
    (keep.iter().map(|&i| dense.equity_curve[i]).collect(), keep.iter().map(|&i| ts[i]).collect())
}

/// The default run, built WITHOUT naming the knob — the "before this type existed" shape.
fn run_default() -> TradingRecord {
    let mut engine = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        BuyThenSell { seen: 0 },
        StrategyEngineConfig::default(),
    );
    engine.run_ticks(&[(SYM.to_string(), tape())])
}

/// Bitwise equality — this is a parity crate; `==` on f64 would accept a `-0.0`/`0.0` swap and
/// reject a legitimate NaN match, and neither is what "the run did not move" means.
fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// A curve's highest and lowest value, as bits.
fn extremes_of(curve: &[f64]) -> (u64, u64) {
    let peak = curve.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let trough = curve.iter().copied().fold(f64::INFINITY, f64::min);
    (peak.to_bits(), trough.to_bits())
}

/// The peak and trough a curve kept by time says its path had, as bits.
fn extremes(sampling: &vike_analytics::TradingRecordSampling) -> (u64, u64) {
    (sampling.max_equity.to_bits(), sampling.min_equity.to_bits())
}

#[test]
fn default_is_every_tick_and_bit_identical_to_naming_it() {
    let d = run_default();
    let e = run(EquitySampling::EveryTick);
    assert_eq!(bits(&d.equity_curve), bits(&e.equity_curve));
    assert_eq!(d.equity_timestamp, e.equity_timestamp);
    // one sample per PRICED tick — the frozen shape (this tape carries no book events, so every
    // tick is priced)
    assert_eq!(d.equity_curve.len(), N_PRICED);
    assert_eq!(d.equity_timestamp.len(), N_PRICED);
    // the curve genuinely moves — otherwise the subsequence assertions below prove nothing
    assert!(
        d.equity_curve.windows(2).any(|w| w[0].to_bits() != w[1].to_bits()),
        "tape must produce a non-constant equity curve"
    );
}

#[test]
fn every_n_is_a_subsequence_of_the_dense_curve_plus_a_closing_sample() {
    let dense = run_default();
    let n = 7usize;
    let thin = run(EquitySampling::EveryN(n));

    // Expected: the dense samples at priced-tick indices 0, n, 2n, … then a closing sample at the
    // LAST priced tick when the stride did not already land on it.
    let mut want_eq: Vec<f64> = (0..N_PRICED).step_by(n).map(|i| dense.equity_curve[i]).collect();
    let mut want_timestamp: Vec<i64> =
        (0..N_PRICED).step_by(n).map(|i| dense.equity_timestamp[i]).collect();
    let last = N_PRICED - 1;
    if !last.is_multiple_of(n) {
        want_eq.push(dense.equity_curve[last]);
        want_timestamp.push(dense.equity_timestamp[last]);
    }

    assert_eq!(bits(&thin.equity_curve), bits(&want_eq));
    assert_eq!(thin.equity_timestamp, want_timestamp);
    // the closing sample is what keeps this invariant true in every mode
    assert_eq!(
        thin.equity_curve.last().map(|x| x.to_bits()),
        Some(thin.final_equity.to_bits()),
        "equity_curve.last() must still agree with final_equity"
    );
    assert!(thin.equity_curve.len() < dense.equity_curve.len(), "the curve must actually thin");
}

#[test]
fn every_n_of_zero_or_one_is_the_dense_curve() {
    let dense = run_default();
    for n in [0usize, 1] {
        let same = run(EquitySampling::EveryN(n));
        assert_eq!(bits(&same.equity_curve), bits(&dense.equity_curve), "EveryN({n})");
        assert_eq!(same.equity_timestamp, dense.equity_timestamp, "EveryN({n})");
    }
}

#[test]
fn off_records_nothing() {
    let off = run(EquitySampling::Off);
    assert!(off.equity_curve.is_empty());
    assert!(off.equity_timestamp.is_empty());
}

#[test]
fn sampling_never_moves_the_run() {
    let dense = run_default();
    assert!(dense.n_trades > 0, "the fixture must trade, else this gate is vacuous");
    for mode in [
        EquitySampling::EveryTick,
        EquitySampling::EveryN(7),
        EquitySampling::EveryN(1000),
        EquitySampling::Off,
        EquitySampling::ByTime(STEP_MS),
        EquitySampling::ByTime(60_000),
    ] {
        let r = run(mode);
        assert_eq!(r.final_equity.to_bits(), dense.final_equity.to_bits(), "{mode:?}");
        assert_eq!(r.n_trades, dense.n_trades, "{mode:?}");
        assert_eq!(r.trades.len(), dense.trades.len(), "{mode:?}");
        for (a, b) in r.trades.iter().zip(dense.trades.iter()) {
            assert_eq!(a.pnl.to_bits(), b.pnl.to_bits(), "{mode:?}");
            assert_eq!(a.entry_price.to_bits(), b.entry_price.to_bits(), "{mode:?}");
            assert_eq!(a.exit_price.to_bits(), b.exit_price.to_bits(), "{mode:?}");
            assert_eq!(a.size.to_bits(), b.size.to_bits(), "{mode:?}");
        }
        assert_eq!(
            bits(&r.per_symbol_pnl.iter().map(|(_, v)| *v).collect::<Vec<_>>()),
            bits(&dense.per_symbol_pnl.iter().map(|(_, v)| *v).collect::<Vec<_>>()),
            "{mode:?}"
        );
    }
}

#[test]
fn by_time_keeps_the_first_sample_and_each_steps_last_one() {
    let dense = run_default();
    let by_time = run(EquitySampling::ByTime(STEP_MS));
    let (want_eq, want_timestamp) = closes_of(&dense);
    assert_eq!(want_eq.len(), 7, "the tape's 60 one-second ticks: its first, then six steps' last");
    assert_eq!(bits(&by_time.equity_curve), bits(&want_eq));
    assert_eq!(by_time.equity_timestamp, want_timestamp);
    assert_eq!(
        by_time.equity_curve.last().map(|x| x.to_bits()),
        Some(by_time.final_equity.to_bits()),
        "the curve ends on the run's last priced tick"
    );
    assert_eq!(dense.sampling, None, "a dense curve is the path");
    let sampling = by_time.sampling.expect("a curve kept by time says so");
    assert_eq!(sampling.interval_ms, STEP_MS);
    assert_eq!(
        sampling.max_drawdown.to_bits(),
        metrics::max_drawdown(&dense.equity_curve).to_bits()
    );
    assert_eq!(sampling.reached_zero_at, None);
    assert_eq!(
        extremes(&sampling),
        extremes_of(&dense.equity_curve),
        "every tick's peak and trough"
    );
    assert_eq!(sampling.events, dense.equity_curve.len(), "every priced tick folded");
    assert!(!sampling.flat, "the account moved");
    for step in [0, -1] {
        let same = run(EquitySampling::ByTime(step));
        assert_eq!(bits(&same.equity_curve), bits(&dense.equity_curve), "ByTime({step})");
        assert_eq!(same.equity_timestamp, dense.equity_timestamp, "ByTime({step})");
        assert_eq!(same.sampling, None, "ByTime({step})");
    }
}

/// A trough INSIDE a step, gone by the step's close: the closes never see it, and the run's drawdown is still the
/// dense curve's, to the bit, in the record and in the report.
#[test]
fn a_trough_between_two_closes_is_still_the_runs_drawdown() {
    // flat at 100, one print at 80 in the middle of the second step, long three since the fifth tick
    let dip = || -> Vec<TickEvent> {
        (0..N_PRICED)
            .map(|i| {
                let ts = T0 + i as i64 * 1_000;
                let mid = if i == 15 { 80.0 } else { 100.0 };
                if i.is_multiple_of(2) { q(ts, mid - 0.05, mid + 0.05) } else { tr(ts, mid) }
            })
            .collect()
    };
    let dense = run_on(EquitySampling::EveryTick, dip());
    let by_time = run_on(EquitySampling::ByTime(STEP_MS), dip());
    assert!(!by_time.equity_timestamp.contains(&(T0 + 15_000)), "the trough is no close");
    let path = metrics::max_drawdown(&dense.equity_curve);
    let closes = metrics::max_drawdown(&by_time.equity_curve);
    assert!(path > closes, "the planted trough is deeper than any close: {path} vs {closes}");
    let sampling = by_time.sampling.expect("kept by time");
    assert_eq!(sampling.max_drawdown.to_bits(), path.to_bits());
    assert_eq!(extremes(&sampling), extremes_of(&dense.equity_curve), "the trough no close sees");
    let lowest_close = by_time.equity_curve.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(sampling.min_equity < lowest_close, "{} vs {lowest_close}", sampling.min_equity);
    let report = TradingResults::from_record(None, &by_time, 252.0);
    let before = TradingResults::from_record(None, &dense, 252.0);
    assert_eq!(report.max_drawdown.to_bits(), before.max_drawdown.to_bits(), "the per-tick figure");
    assert_eq!(report.final_equity.to_bits(), before.final_equity.to_bits());
    assert_eq!(
        report.total_return.to_bits(),
        before.total_return.to_bits(),
        "the first and the last tick are both kept"
    );
}

/// Feed latency delivers ticks by ARRIVAL, so a venue stamp can go back across a step's edge. The step never goes
/// back: the closes stay one a step with rising stamps, and the run and its drawdown are still every tick's.
#[test]
fn a_late_tick_joins_the_open_step_and_the_closes_stay_in_time_order() {
    // each step's first tick arrives before the previous step's last one: stamps 0..8, 10, 9, 11..18, 20, 19, …
    let late = || {
        let mut t = tape();
        for edge in (10..N_PRICED).step_by(10) {
            t.swap(edge - 1, edge);
        }
        t
    };
    let dense = run_on(EquitySampling::EveryTick, late());
    assert!(dense.equity_timestamp.windows(2).any(|w| w[1] < w[0]), "the tape must go back");
    let by_time = run_on(EquitySampling::ByTime(STEP_MS), late());
    let stamps = &by_time.equity_timestamp;
    assert!(stamps.windows(2).all(|w| w[0] < w[1]), "rising stamps: {stamps:?}");
    let steps: Vec<i64> = stamps.iter().map(|t| t.div_euclid(STEP_MS)).collect();
    assert_eq!(steps.len(), 7, "the first tick and one close a step: {stamps:?}");
    assert!(steps[1..].windows(2).all(|w| w[0] < w[1]), "one close a step: {stamps:?}");
    assert_eq!(by_time.final_equity.to_bits(), dense.final_equity.to_bits());
    assert_eq!(by_time.n_trades, dense.n_trades);
    let sampling = by_time.sampling.expect("kept by time");
    assert_eq!(
        sampling.max_drawdown.to_bits(),
        metrics::max_drawdown(&dense.equity_curve).to_bits()
    );
    assert_eq!(sampling.events, dense.equity_curve.len());
}

/// The report's Sharpe of a run kept by time is the closes' Sharpe at the step: the closes taken off the dense run by
/// hand, annualized at 252 trading days of `86_400_000 / STEP_MS` steps.
#[test]
fn the_reports_sharpe_is_the_closes_sharpe_at_the_step() {
    let dense = run_default();
    let by_time = run(EquitySampling::ByTime(STEP_MS));
    let (closes, _) = closes_of(&dense);
    let periods_per_year = 252.0 * (86_400_000.0 / STEP_MS as f64);
    let report = TradingResults::from_record(None, &by_time, 252.0);
    assert_eq!(report.returns_interval_ms, Some(STEP_MS));
    // the library's Sharpe over the hand-taken closes, to the bit
    assert_eq!(report.sharpe.to_bits(), metrics::sharpe(&closes, periods_per_year).to_bits());
    // and the textbook one: the mean over the sample standard deviation, times the root of the steps a year
    let returns: Vec<f64> = closes.windows(2).map(|w| w[1] / w[0] - 1.0).collect();
    let n = returns.len() as f64;
    let mean = returns.iter().fold(0.0, |sum, r| sum + r) / n;
    let var = returns.iter().fold(0.0, |sum, r| sum + (r - mean) * (r - mean)) / (n - 1.0);
    let by_hand = mean / var.sqrt() * periods_per_year.sqrt();
    assert!(by_hand != 0.0, "the closes must move, else this proves nothing");
    assert!(((report.sharpe - by_hand) / by_hand).abs() < 1e-12, "{} vs {by_hand}", report.sharpe);
    // and not the per-tick figure a tick run reported before
    let per_tick = TradingResults::from_record(None, &dense, 252.0);
    assert_ne!(report.sharpe.to_bits(), per_tick.sharpe.to_bits());
}
