//! Engine <-> kernel reconciliation as a GATE (was: the vike-bench parity table, printed only).
//! The event engine (StrategyEngine) and the vectorized kernel (VectorBacktestEngine) run the
//! ONE unified strategy on identical synthetic bars and must agree: same trade count, and
//! final equity bit-identical (both are naive-fold f64 paths over the same inputs — the repo's
//! second-oracle discipline; never widen this to a tolerance).
use vike_marketdata::Bar;
use vike_model::Strategy;
use vike_simulator::test_support::target_at;
use vike_simulator::{
    Matrix, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, VectorBacktestEngine,
};

const T: usize = 2000;

struct MultiTarget;
impl Strategy<OrderBrokerSimulator> for MultiTarget {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        if let Some(w) = target_at(ctx.index) {
            let symbol = bar.symbol.as_deref().unwrap_or("_").to_string();
            ctx.strategy_order_target_percent(&symbol, w);
        }
    }
}

fn synth_series(n_symbols: usize) -> Vec<(String, Vec<Bar>)> {
    (0..n_symbols)
        .map(|si| {
            let name = format!("SYM{si}");
            let bars = (0..T)
                .map(|i| {
                    let px = 100.0 + i as f64 * 0.01 + si as f64 * 0.5;
                    Bar {
                        timestamp: 1_700_000_000_000 + i as i64 * 60_000,
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

fn run_both(n_symbols: usize, multiplier: f64) -> ((f64, usize), (f64, usize)) {
    let series = synth_series(n_symbols);
    let params = StrategyEngineConfig {
        fee_rate: 0.0007,
        cash: 100_000.0,
        slippage: 0.0003,
        multiplier,
        ..Default::default()
    };
    // event engine
    let mut e = StrategyEngine::new(series.clone(), MultiTarget, params);
    let r = e.run();
    let event = (r.final_equity, r.trades.len());
    // vectorized kernel
    let names: Vec<String> = series.iter().map(|(n, _)| n.clone()).collect();
    let ts: Vec<i64> = series[0].1.iter().map(|b| b.timestamp).collect();
    let (mut om, mut cm) = (vec![0.0; T * n_symbols], vec![0.0; T * n_symbols]);
    for (si, (_, bars)) in series.iter().enumerate() {
        for (t, b) in bars.iter().enumerate() {
            om[t * n_symbols + si] = b.open;
            cm[t * n_symbols + si] = b.close;
        }
    }
    let mut w = vec![f64::NAN; T * n_symbols];
    for i in 0..T {
        if let Some(v) = target_at(i) {
            for s in 0..n_symbols {
                w[i * n_symbols + s] = v;
            }
        }
    }
    let r = VectorBacktestEngine::run(
        &Matrix::new(om, T, n_symbols),
        &Matrix::new(cm, T, n_symbols),
        &Matrix::new(vec![0.0; T * n_symbols], T, n_symbols),
        &ts,
        &Matrix::new(w, T, n_symbols),
        0.0007, // maker side is dead in this kernel — see vector_engine's module doc
        0.0007,
        0.0003,
        100_000.0,
        multiplier,
        Some(&names),
        true,
    );
    let vector = (r.final_equity, r.n_trades);
    (event, vector)
}

/// Same shape as [`run_both`], but the event engine is armed with
/// [`StrategyEngineConfig::fee_schedule`] instead of the flat `fee_rate` — the fee-model wiring this
/// gate exists to prove. `maker_bps` != `taker_bps` DELIBERATELY: the event engine derives
/// `(maker, taker)` from `FeeSchedule::maker_taker_rates()` (fee model follow-up 3) and every
/// fill here is a `strategy_order_target_percent` MARKET order — always classified taker
/// (`is_maker := kind == Limit`) — so only the taker rate is ever actually charged on EITHER
/// side; the vector kernel is fed the same schedule's `(maker, taker)` pair (its maker side is
/// dead — see `vector_engine`'s module doc). If either engine's fee derivation were wrong this
/// would diverge; agreement here proves the schedule wiring is byte-identical to the flat-rate
/// path AND that a real maker/taker split doesn't leak into the vector kernel's dead maker slot.
fn run_both_with_fee_schedule(n_symbols: usize) -> ((f64, usize), (f64, usize)) {
    use vike_model::FeeSchedule;

    let schedule = FeeSchedule::PercentMakerTaker { maker_bps: 1.0, taker_bps: 7.0 };
    let (maker_fee, taker_fee) = schedule.maker_taker_rates();

    let series = synth_series(n_symbols);
    let params = StrategyEngineConfig {
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
    let ts: Vec<i64> = series[0].1.iter().map(|b| b.timestamp).collect();
    let (mut om, mut cm) = (vec![0.0; T * n_symbols], vec![0.0; T * n_symbols]);
    for (si, (_, bars)) in series.iter().enumerate() {
        for (t, b) in bars.iter().enumerate() {
            om[t * n_symbols + si] = b.open;
            cm[t * n_symbols + si] = b.close;
        }
    }
    let mut w = vec![f64::NAN; T * n_symbols];
    for i in 0..T {
        if let Some(v) = target_at(i) {
            for s in 0..n_symbols {
                w[i * n_symbols + s] = v;
            }
        }
    }
    let r = VectorBacktestEngine::run(
        &Matrix::new(om, T, n_symbols),
        &Matrix::new(cm, T, n_symbols),
        &Matrix::new(vec![0.0; T * n_symbols], T, n_symbols),
        &ts,
        &Matrix::new(w, T, n_symbols),
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
    let ((ee, et), (ve, vt)) = run_both(1, 1.0);
    assert_eq!(et, vt, "trade counts diverged");
    assert_eq!(ee.to_bits(), ve.to_bits(), "final equity diverged: engine {ee} vs kernel {ve}");
}

#[test]
fn engine_and_kernel_agree_multi_symbol() {
    let ((ee, et), (ve, vt)) = run_both(5, 1.0);
    assert_eq!(et, vt, "trade counts diverged");
    assert_eq!(ee.to_bits(), ve.to_bits(), "final equity diverged: engine {ee} vs kernel {ve}");
}

/// ⚠ **A contract multiplier sizes the kernel as it sizes the engine**: one sizing law,
/// `vike_model::ContractValue::units_worth`. At 50 the kernel's own `w * eq / c` asked for 50 times the
/// units. One symbol, because `engine_and_kernel_agree_multi_symbol` covers the fold order and a
/// compensated sum against a naive one has no bit-identity to promise past `1.0`.
#[test]
fn engine_and_kernel_size_alike_at_a_contract_multiplier() {
    let ((ee, et), (ve, vt)) = run_both(1, 50.0);
    assert_eq!(et, vt, "trade counts diverged");
    assert_eq!(ee.to_bits(), ve.to_bits(), "final equity diverged: engine {ee} vs kernel {ve}");
}

/// Asks for half the equity on step 0, and for the same target again on step 2.
struct HalfOnStepsZeroAndTwo;
impl Strategy<OrderBrokerSimulator> for HalfOnStepsZeroAndTwo {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        if ctx.index == 0 || ctx.index == 2 {
            ctx.strategy_order_target_percent("SYM0", 0.5);
        }
    }
}

/// ⚠ **With no usable price a target order HOLDS**, the guard of live's
/// `LiveBroker::order_target_percent`. 50 units are held when the price turns zero or negative
/// (`vike_model::gross_notional`'s doc: this workspace ships negative-priced series). The engine read
/// that as target 0 and CLOSED the position; on a negative close the kernel ordered on a negative
/// target. Both now keep the 50.
#[test]
fn a_target_order_at_no_usable_price_holds_in_both_engines() {
    for px in [0.0, -5.0] {
        let closes = [100.0, 100.0, px, px];
        let bars: Vec<Bar> = closes
            .iter()
            .enumerate()
            .map(|(i, &c)| vike_simulator::test_support::bar(i as i64, c, c, c, c, 0.0))
            .collect();
        let params = StrategyEngineConfig { cash: 10_000.0, ..Default::default() };
        let mut e =
            StrategyEngine::new(vec![("SYM0".to_string(), bars)], HalfOnStepsZeroAndTwo, params);
        let r = e.run();
        assert_eq!(e.core.symbol[0].pos.size, 50.0, "engine at {px}: the position is held");
        assert!(r.trades.is_empty(), "engine at {px}: nothing closed: {:?}", r.trades);

        let matrix = |v: Vec<f64>| Matrix::new(v, 4, 1);
        let nan = f64::NAN;
        let r = VectorBacktestEngine::run(
            &matrix(closes.to_vec()),
            &matrix(closes.to_vec()),
            &matrix(vec![0.0; 4]),
            &[0, 1, 2, 3],
            &matrix(vec![0.5, nan, 0.5, nan]),
            0.0,
            0.0,
            0.0,
            10_000.0,
            1.0,
            None,
            true,
        );
        assert_eq!(r.n_trades, 0, "kernel at {px}: nothing closed");
    }
}
