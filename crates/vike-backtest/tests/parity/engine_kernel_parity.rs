//! Engine <-> kernel reconciliation as a GATE (was: the vike-bench parity table, printed only).
//! The event engine (StrategyEngine) and the vectorized kernel (VectorBacktestEngine) run the
//! ONE unified strategy on identical synthetic bars and must agree: same trade count, and
//! final equity bit-identical (both are naive-fold f64 paths over the same inputs — the repo's
//! second-oracle discipline; never widen this to a tolerance).
use vike_backtest::{EngineParams, Matrix, SimBroker, StrategyEngine, VectorBacktestEngine};
use vike_model::{Bar, Strategy};

const W_ON: usize = 20;
const W_OFF: usize = 10;
const W_PCT: f64 = 0.5;
const T: usize = 2000;

fn target_at(i: usize) -> Option<f64> {
    if i.is_multiple_of(W_ON) {
        Some(W_PCT)
    } else if i % W_ON == W_OFF {
        Some(0.0)
    } else {
        None
    }
}

struct MultiTarget;
impl Strategy<SimBroker> for MultiTarget {
    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        if let Some(w) = target_at(ctx.index) {
            let sym = bar.symbol.as_deref().unwrap_or("_").to_string();
            ctx.strategy_order_target_percent(&sym, w);
        }
    }
}

fn synth_series(n_syms: usize) -> Vec<(String, Vec<Bar>)> {
    (0..n_syms)
        .map(|si| {
            let name = format!("SYM{si}");
            let bars = (0..T)
                .map(|i| {
                    let px = 100.0 + i as f64 * 0.01 + si as f64 * 0.5;
                    Bar {
                        ts: 1_700_000_000_000 + i as i64 * 60_000,
                        open: px,
                        high: px * 1.001,
                        low: px * 0.999,
                        close: px,
                        volume: 0.0,
                        funding: None,
                        bid: None,
                        ask: None,
                        symbol: Some(name.clone()),
                    }
                })
                .collect();
            (name, bars)
        })
        .collect()
}

fn run_both(n_syms: usize) -> ((f64, usize), (f64, usize)) {
    let series = synth_series(n_syms);
    let params = EngineParams {
        fee_rate: 0.0007,
        cash: 100_000.0,
        slippage: 0.0003,
        multiplier: 1.0,
        ..Default::default()
    };
    // event engine
    let mut e = StrategyEngine::new(series.clone(), MultiTarget, params);
    let r = e.run();
    let event = (r.final_equity, r.trades.len());
    // vectorized kernel
    let names: Vec<String> = series.iter().map(|(n, _)| n.clone()).collect();
    let ts: Vec<i64> = series[0].1.iter().map(|b| b.ts).collect();
    let (mut om, mut cm) = (vec![0.0; T * n_syms], vec![0.0; T * n_syms]);
    for (si, (_, bars)) in series.iter().enumerate() {
        for (t, b) in bars.iter().enumerate() {
            om[t * n_syms + si] = b.open;
            cm[t * n_syms + si] = b.close;
        }
    }
    let mut w = vec![f64::NAN; T * n_syms];
    for i in 0..T {
        if let Some(v) = target_at(i) {
            for s in 0..n_syms {
                w[i * n_syms + s] = v;
            }
        }
    }
    let r = VectorBacktestEngine::run(
        &Matrix::new(om, T, n_syms),
        &Matrix::new(cm, T, n_syms),
        &Matrix::new(vec![0.0; T * n_syms], T, n_syms),
        &ts,
        &Matrix::new(w, T, n_syms),
        0.0007, // maker side is dead in this kernel — see vector_engine's module doc
        0.0007,
        0.0003,
        100_000.0,
        1.0,
        Some(&names),
        true,
    );
    let vector = (r.final_equity, r.n_trades);
    (event, vector)
}

/// Same shape as [`run_both`], but the event engine is armed with
/// [`EngineParams::fee_schedule`] instead of the flat `fee_rate` — the fee-model wiring this
/// gate exists to prove. `maker_bps` != `taker_bps` DELIBERATELY: the event engine derives
/// `(maker, taker)` from `FeeSchedule::maker_taker_rates()` (fee model follow-up 3) and every
/// fill here is a `strategy_order_target_percent` MARKET order — always classified taker
/// (`is_maker := kind == Limit`) — so only the taker rate is ever actually charged on EITHER
/// side; the vector kernel is fed the same schedule's `(maker, taker)` pair (its maker side is
/// dead — see `vector_engine`'s module doc). If either engine's fee derivation were wrong this
/// would diverge; agreement here proves the schedule wiring is byte-identical to the flat-rate
/// path AND that a real maker/taker split doesn't leak into the vector kernel's dead maker slot.
fn run_both_with_fee_schedule(n_syms: usize) -> ((f64, usize), (f64, usize)) {
    use vike_model::FeeSchedule;

    let schedule = FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 7.0 };
    let (maker_fee, taker_fee) = schedule.maker_taker_rates();

    let series = synth_series(n_syms);
    let params = EngineParams {
        cash: 100_000.0,
        slippage: 0.0003,
        multiplier: 1.0,
        fee_schedule: Some(schedule),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(series.clone(), MultiTarget, params);
    let r = e.run();
    let event = (r.final_equity, r.trades.len());

    let names: Vec<String> = series.iter().map(|(n, _)| n.clone()).collect();
    let ts: Vec<i64> = series[0].1.iter().map(|b| b.ts).collect();
    let (mut om, mut cm) = (vec![0.0; T * n_syms], vec![0.0; T * n_syms]);
    for (si, (_, bars)) in series.iter().enumerate() {
        for (t, b) in bars.iter().enumerate() {
            om[t * n_syms + si] = b.open;
            cm[t * n_syms + si] = b.close;
        }
    }
    let mut w = vec![f64::NAN; T * n_syms];
    for i in 0..T {
        if let Some(v) = target_at(i) {
            for s in 0..n_syms {
                w[i * n_syms + s] = v;
            }
        }
    }
    let r = VectorBacktestEngine::run(
        &Matrix::new(om, T, n_syms),
        &Matrix::new(cm, T, n_syms),
        &Matrix::new(vec![0.0; T * n_syms], T, n_syms),
        &ts,
        &Matrix::new(w, T, n_syms),
        maker_fee,
        taker_fee,
        0.0003,
        100_000.0,
        1.0,
        Some(&names),
        true,
    );
    let vector = (r.final_equity, r.n_trades);
    (event, vector)
}

#[test]
fn engine_and_kernel_agree_with_fee_schedule_armed() {
    let ((ee, et), (ve, vt)) = run_both_with_fee_schedule(5);
    assert_eq!(et, vt, "trade counts diverged");
    assert_eq!(
        ee.to_bits(),
        ve.to_bits(),
        "final equity diverged with fee_schedule armed: engine {ee} vs kernel {ve}"
    );
}

#[test]
fn engine_and_kernel_agree_single_symbol() {
    let ((ee, et), (ve, vt)) = run_both(1);
    assert_eq!(et, vt, "trade counts diverged");
    assert_eq!(ee.to_bits(), ve.to_bits(), "final equity diverged: engine {ee} vs kernel {ve}");
}

#[test]
fn engine_and_kernel_agree_multi_symbol() {
    let ((ee, et), (ve, vt)) = run_both(5);
    assert_eq!(et, vt, "trade counts diverged");
    assert_eq!(ee.to_bits(), ve.to_bits(), "final equity diverged: engine {ee} vs kernel {ve}");
}
