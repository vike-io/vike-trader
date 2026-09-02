//! Gate for [`vike_backtest::EquitySampling`] — the opt-in tick-lane equity-curve density knob
//! (perf: the curve is two `Vec`s grown 16 bytes per priced tick, unconditionally, so a 100M-tick
//! sweep point carries 1.6 GB of samples nothing may ever read).
//!
//! The knob is allowed to change ONE thing — the recorded curve — and nothing else. So every test
//! here differences runs of the SAME tape and the SAME strategy across sampling modes:
//!
//! 1. `EveryTick` is the DEFAULT: `EngineParams::default()` and an explicit `EveryTick` produce
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
//!
//! The tape is deliberately ACTIVE (a strategy that opens and closes, so equity genuinely moves
//! tick to tick); a flat curve would make the subsequence claims vacuous.

use vike_backtest::{
    BacktestResult, EngineParams, EquitySampling, SimBroker, StrategyEngine, Tick,
};
use vike_model::{Broker, QuoteTick, Strategy, TradeTick};

const SYM: &str = "TOK";
const T0: i64 = 1_700_000_000_000;
/// priced ticks on the tape (quotes + trades); book events are extra and are NOT sampled
const N_PRICED: usize = 60;

fn q(ts: i64, bid: f64, ask: f64) -> Tick {
    Tick::Quote(QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size: 50.0,
        ask_size: 50.0,
        symbol: SYM.to_string(),
    })
}

fn tr(ts: i64, price: f64) -> Tick {
    Tick::Trade(TradeTick {
        ts,
        local_ts: 0,
        price,
        size: 5.0,
        is_buyer_maker: false,
        symbol: SYM.to_string(),
    })
}

/// Alternating quote/trade tape on a zig-zag price, so held inventory is marked to a DIFFERENT
/// price on every tick and the equity curve is genuinely non-constant.
fn tape() -> Vec<Tick> {
    (0..N_PRICED)
        .map(|i| {
            let ts = T0 + i as i64 * 1_000;
            let mid = 100.0 + (i % 5) as f64 * 0.25 + (i / 5) as f64 * 0.10;
            if i.is_multiple_of(2) {
                q(ts, mid - 0.05, mid + 0.05)
            } else {
                tr(ts, mid)
            }
        })
        .collect()
}

/// Buys once early, sells once late — so the curve has a flat prefix, a marked-to-market middle
/// and a flat suffix, and the trade log is non-empty (it is one of the invariants under test).
struct BuyThenSell {
    seen: usize,
}

impl Strategy<SimBroker> for BuyThenSell {
    fn on_quote_tick(&mut self, b: &mut SimBroker, _q: &QuoteTick) {
        self.step(b);
    }
    fn on_trade_tick(&mut self, b: &mut SimBroker, _t: &TradeTick) {
        self.step(b);
    }
}

impl BuyThenSell {
    fn step(&mut self, b: &mut SimBroker) {
        self.seen += 1;
        if self.seen == 4 {
            Broker::submit_market(b, SYM, 1, 3.0);
        } else if self.seen == N_PRICED - 6 {
            Broker::submit_market(b, SYM, -1, 3.0);
        }
    }
}

fn run(sampling: EquitySampling) -> BacktestResult {
    let params = EngineParams { equity_sampling: sampling, ..Default::default() };
    let mut eng =
        StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], BuyThenSell { seen: 0 }, params);
    eng.run_ticks(&[(SYM.to_string(), tape())])
}

/// The default run, built WITHOUT naming the knob — the "before this type existed" shape.
fn run_default() -> BacktestResult {
    let mut eng = StrategyEngine::new(
        vec![(SYM.to_string(), Vec::new())],
        BuyThenSell { seen: 0 },
        EngineParams::default(),
    );
    eng.run_ticks(&[(SYM.to_string(), tape())])
}

/// Bitwise equality — this is a parity crate; `==` on f64 would accept a `-0.0`/`0.0` swap and
/// reject a legitimate NaN match, and neither is what "the run did not move" means.
fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

#[test]
fn default_is_every_tick_and_bit_identical_to_naming_it() {
    let d = run_default();
    let e = run(EquitySampling::EveryTick);
    assert_eq!(bits(&d.equity_curve), bits(&e.equity_curve));
    assert_eq!(d.equity_ts, e.equity_ts);
    // one sample per PRICED tick — the frozen shape (this tape carries no book events, so every
    // tick is priced)
    assert_eq!(d.equity_curve.len(), N_PRICED);
    assert_eq!(d.equity_ts.len(), N_PRICED);
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
    let mut want_ts: Vec<i64> = (0..N_PRICED).step_by(n).map(|i| dense.equity_ts[i]).collect();
    let last = N_PRICED - 1;
    if !last.is_multiple_of(n) {
        want_eq.push(dense.equity_curve[last]);
        want_ts.push(dense.equity_ts[last]);
    }

    assert_eq!(bits(&thin.equity_curve), bits(&want_eq));
    assert_eq!(thin.equity_ts, want_ts);
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
        assert_eq!(same.equity_ts, dense.equity_ts, "EveryN({n})");
    }
}

#[test]
fn off_records_nothing() {
    let off = run(EquitySampling::Off);
    assert!(off.equity_curve.is_empty());
    assert!(off.equity_ts.is_empty());
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
