//! Engine <-> kernel reconciliation as a GATE (was: the vike-bench parity table, printed only).
//! The event engine (StrategyEngine) and the vectorized kernel (VectorBacktestEngine) run the
//! ONE unified strategy on identical synthetic bars and must agree: same trade count, and
//! final equity bit-identical (both are naive-fold f64 paths over the same inputs — the repo's
//! second-oracle discipline; never widen this to a tolerance).
use vike_fills::costs::costs_of;
use vike_fills::market_impact::{ProfileEngineSlippageTerms, slippage_terms};
use vike_marketdata::Bar;
use vike_model::params::{ParamValue, Params};
use vike_model::{Costs, ProfileEngineSlippage, Strategy};
use vike_simulator::test_support::{W_OFF, target_at};
use vike_simulator::{
    Matrix, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, VectorBacktestEngine,
    fast_portfolio_backtest,
};

const T: usize = 2000;

/// The kernel's slippage terms for a flat fraction alone: what `StrategyEngineConfig::slippage` is.
fn flat(fraction: f64) -> ProfileEngineSlippageTerms {
    ProfileEngineSlippageTerms { fraction, amount: 0.0, impact: None }
}

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
                        symbol: Some(name.as_str().into()),
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
    let zeros = Matrix::new(vec![0.0; T * n_symbols], T, n_symbols);
    let r = fast_portfolio_backtest(
        &Matrix::new(om, T, n_symbols),
        &Matrix::new(cm, T, n_symbols),
        &zeros,
        &zeros,
        &ts,
        &Matrix::new(w, T, n_symbols),
        0.0007, // maker side is dead in this kernel — see vector_engine's module doc
        0.0007,
        None,
        &flat(0.0003),
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
    let zeros = Matrix::new(vec![0.0; T * n_symbols], T, n_symbols);
    let r = fast_portfolio_backtest(
        &Matrix::new(om, T, n_symbols),
        &Matrix::new(cm, T, n_symbols),
        &zeros,
        &zeros,
        &ts,
        &Matrix::new(w, T, n_symbols),
        maker_fee,
        taker_fee,
        None,
        &flat(0.0003),
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
        let r = fast_portfolio_backtest(
            &matrix(closes.to_vec()),
            &matrix(closes.to_vec()),
            &matrix(vec![0.0; 4]),
            &matrix(vec![0.0; 4]),
            &[0, 1, 2, 3],
            &matrix(vec![0.5, nan, 0.5, nan]),
            0.0,
            0.0,
            None,
            &flat(0.0),
            10_000.0,
            1.0,
            None,
            true,
        );
        assert_eq!(r.n_trades, 0, "kernel at {px}: nothing closed");
    }
}

/// [`target_at`] with symbol `k` shifted `k · W_OFF` bars: `SYM1` enters on the bar `SYM0` exits, so no two
/// positions are ever held at once. ⚠ That is what lets EVERY equity point match on two symbols: the engine
/// marks `cash + compensated_sum(legs)` and the kernel `(cash + leg0) + leg1` (each pinned by its own frozen
/// fixtures), which agree to the bit only while at most one leg is non-zero.
fn staggered_target(symbol: usize, i: usize) -> Option<f64> {
    target_at(i + symbol * W_OFF)
}

/// [`staggered_target`] for the event engine: `SYM{k}`'s target each bar.
struct Staggered;
impl Strategy<OrderBrokerSimulator> for Staggered {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = bar.symbol.as_deref().unwrap_or("SYM0").to_string();
        let k: usize = symbol.trim_start_matches("SYM").parse().expect("a SYM{k} name");
        if let Some(w) = staggered_target(k, ctx.index) {
            ctx.strategy_order_target_percent(&symbol, w);
        }
    }
}

/// Bars an impact model can measure: an integer pattern (the open steps through seven levels on a rising
/// drift of 1/8 a bar, so the long-flat schedule earns before costs), a close a quarter-step pattern off the
/// open, and a volume above zero that varies.
fn priced_series(n_symbols: usize) -> Vec<(String, Vec<Bar>)> {
    (0..n_symbols)
        .map(|si| {
            let name = format!("SYM{si}");
            let bars = (0..T)
                .map(|i| {
                    let open = 100.0 + (i % 7) as f64 + 0.125 * i as f64 + 3.0 * si as f64;
                    let close = open + 0.25 * ((i % 5) as f64 - 2.0);
                    Bar {
                        timestamp: 1_700_000_000_000 + i as i64 * 60_000,
                        open,
                        high: open.max(close) + 0.5,
                        low: open.min(close) - 0.5,
                        close,
                        volume: 1_000.0 + 100.0 * ((i * 7 + si) % 11) as f64,
                        funding: None,
                        bid: None,
                        ask: None,
                        symbol: Some(name.as_str().into()),
                    }
                })
                .collect();
            (name, bars)
        })
        .collect()
}

/// The cost rows, each as a strategy file sets it in `[strategy.params]` over what it states: the
/// market-impact model is STATED (no param names it), with a base fraction under it and a 5-bar window.
fn cost_rows() -> Vec<(&'static str, Params, Option<ProfileEngineSlippage>)> {
    let set = |pairs: Vec<(&str, ParamValue)>| pairs.into_iter().collect::<Params>();
    let word = |w: &str| ParamValue::Str(w.to_string());
    let impact = ProfileEngineSlippage {
        model: "market_impact".to_string(),
        value: None,
        ticks: None,
        tick: None,
        base: Some(0.0002),
        exec_time: None,
        window: Some(5),
        gamma: None,
        eta: None,
    };
    vec![
        ("none", Params::new(), None),
        ("0.05 percent", set(vec![("commission", ParamValue::Float(0.05))]), None),
        (
            "0.5 per_contract",
            set(vec![
                ("commission", ParamValue::Float(0.5)),
                ("commission_unit", word("per_contract")),
            ]),
            None,
        ),
        (
            "1.0 per_order",
            set(vec![
                ("commission", ParamValue::Float(1.0)),
                ("commission_unit", word("per_order")),
            ]),
            None,
        ),
        ("0.03 percent slippage", set(vec![("slippage", ParamValue::Float(0.03))]), None),
        (
            "2 ticks of 0.01",
            set(vec![
                ("slippage", ParamValue::Float(2.0)),
                ("slippage_unit", word("ticks")),
                ("slippage_tick", ParamValue::Float(0.01)),
            ]),
            None,
        ),
        // The impact row's base alone, right before it: the impact row must end below it.
        ("0.02 percent slippage", set(vec![("slippage", ParamValue::Float(0.02))]), None),
        ("a stated market_impact", Params::new(), Some(impact)),
    ]
}

/// One engine's answer: its equity at every bar, its closed trades and its unpriced impact fills.
struct Charged {
    equity: Vec<f64>,
    trades: usize,
    impact_unpriced: u64,
}

/// [`Staggered`] on [`priced_series`] through both engines, each charged `costs`: the event engine
/// configured as a backtest configures it (`vike_backtest`'s `bar_engine_params`: `engine_fee` and the
/// slippage table's terms), and the kernel handed the `Costs` itself over the bars, as a research study
/// hands them (`VectorBacktestEngine::run_over_bars`).
fn run_charged(costs: &Costs, n_symbols: usize, multiplier: f64) -> (Charged, Charged) {
    let series = priced_series(n_symbols);
    let (fee_rate, fee_schedule) = costs.commission.engine_fee();
    let slip =
        slippage_terms(costs.slippage.as_ref()).expect("each row's slippage passes its check");
    let mut params = StrategyEngineConfig {
        cash: 100_000.0,
        multiplier,
        fee_rate,
        fee_schedule,
        slippage: slip.fraction,
        slippage_amount: slip.amount,
        ..Default::default()
    };
    if let Some((model, window)) = slip.impact {
        params.impact = Some(model);
        params.impact_window = window;
    }
    let r = StrategyEngine::new(series.clone(), Staggered, params).run();
    let unpriced =
        |r: &vike_analytics::TradingRecord| r.simulator().map_or(0, |s| s.impact_unpriced);
    let event =
        Charged { impact_unpriced: unpriced(&r), trades: r.trades.len(), equity: r.equity_curve };

    let legs: Vec<&[Bar]> = series.iter().map(|(_, bars)| bars.as_slice()).collect();
    let r = VectorBacktestEngine::run_over_bars(
        &legs,
        &staggered_weights(n_symbols),
        costs,
        100_000.0,
        multiplier,
    )
    .expect("each row's slippage passes its check");
    let vector =
        Charged { impact_unpriced: unpriced(&r), trades: r.n_trades, equity: r.equity_curve };
    (event, vector)
}

/// [`staggered_target`] as the kernel's row-major weights over `n_symbols` legs.
fn staggered_weights(n_symbols: usize) -> Vec<f64> {
    let mut w = vec![f64::NAN; T * n_symbols];
    for i in 0..T {
        for s in 0..n_symbols {
            if let Some(v) = staggered_target(s, i) {
                w[i * n_symbols + s] = v;
            }
        }
    }
    w
}

/// The kernel over bars takes legs on ONE timestamp grid and one weight a bar and leg, and refuses anything
/// else in words, never pairing the bars of two different times.
#[test]
fn the_kernel_over_bars_refuses_legs_off_one_grid() {
    let series = priced_series(2);
    let free = costs_of(&Params::new(), None, None).expect("no cost");
    let (first, second) = (series[0].1.as_slice(), series[1].1.as_slice());
    let shifted: Vec<Bar> =
        second.iter().map(|b| Bar { timestamp: b.timestamp + 1, ..b.clone() }).collect();
    let off = VectorBacktestEngine::run_over_bars(
        &[first, shifted.as_slice()],
        &staggered_weights(2),
        &free,
        1.0,
        1.0,
    )
    .expect_err("a leg one millisecond off");
    assert!(off.contains("one timestamp grid"), "{off}");
    let short = VectorBacktestEngine::run_over_bars(
        &[first, &second[1..]],
        &staggered_weights(2),
        &free,
        1.0,
        1.0,
    )
    .expect_err("a leg a bar short");
    assert!(short.contains("one timestamp grid"), "{short}");
    let shape = VectorBacktestEngine::run_over_bars(
        &[first, second],
        &staggered_weights(1),
        &free,
        1.0,
        1.0,
    )
    .expect_err("one leg's weights for two legs");
    assert!(shape.contains("one a bar and leg"), "{shape}");
}

/// ⚠ **The kernel charges what the event engine charges, through one cost type**: every cost row a strategy
/// file can set, read by `vike_fills::costs::costs_of`, gives the two engines the same trades and the same
/// equity at EVERY bar, to the bit, on one symbol, at a contract multiplier of 10, and on two symbols. Each
/// charged row ends below the free one. The impact row leaves exactly one fill unpriced in both (the run's
/// first, whose window holds one bar) and ends below its base fraction alone, so the model priced the rest.
/// No tolerance (decision 0021).
#[test]
fn engine_and_kernel_charge_the_same_costs() {
    for (n_symbols, multiplier) in [(1, 1.0), (1, 10.0), (2, 1.0)] {
        let (mut free, mut previous) = (None, None);
        for (row, params, stated_slippage) in cost_rows() {
            let shape = format!("{row}, {n_symbols} symbol(s) at multiplier {multiplier}");
            let costs = costs_of(&params, None, stated_slippage)
                .unwrap_or_else(|e| panic!("{shape}: refused: {e}"));
            let (event, vector) = run_charged(&costs, n_symbols, multiplier);
            assert!(event.trades > 0, "{shape}: the schedule traded nothing");
            assert_eq!(event.trades, vector.trades, "{shape}: trade counts diverged");
            assert_eq!(event.equity.len(), vector.equity.len(), "{shape}: curve lengths diverged");
            for (t, (e, v)) in event.equity.iter().zip(&vector.equity).enumerate() {
                assert_eq!(
                    e.to_bits(),
                    v.to_bits(),
                    "{shape}: equity[{t}] diverged: engine {e} vs kernel {v}"
                );
            }
            assert_eq!(
                event.impact_unpriced, vector.impact_unpriced,
                "{shape}: impact_unpriced diverged"
            );
            let last = *vector.equity.last().expect("a curve");
            if costs.slippage.as_ref().is_some_and(|s| s.model == "market_impact") {
                assert_eq!(vector.impact_unpriced, 1, "{shape}: the impact model priced no fill");
                let base = previous.expect("the base-only row comes first");
                assert!(last < base, "{shape}: {last} is not below its base alone, {base}");
            }
            previous = Some(last);
            match free {
                None => free = Some(last),
                Some(f) => assert!(last < f, "{shape}: charged {last} is not below the free {f}"),
            }
        }
    }
}
