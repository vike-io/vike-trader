//! The multi-asset (time×symbol) portfolio kernel — the vectorized fast path for
//! cross-sectional / target-weight strategies. Exact port of
//! `core/portfolio_fastsim.py::_portfolio_kernel` / `fast_portfolio_backtest`.
//!
//! SCOPE (the parity boundary — deliberately narrow): the `cash_gate=False` market-order
//! target-weight path ONLY — next-open fills, COLUMN-order shared-cash application (cash may
//! go negative; no gate, no reduce-first, no weight priority), taker fees, slippage, funding,
//! cost-basis averaging. Out of scope (use the event engine): stops/brackets, cash gate,
//! volume/position caps, leverage capping, membership masks, liquidation, sub-bars, sizers.
//!
//! **The costs are the event engine's, through the same vike-fills calls** (the owner's word of 2026-11-19:
//! one cost type, `vike_model::Costs`, which [`VectorBacktestEngine::run`] takes): a fee schedule
//! through `vike_fills::broker_sim::commission` (the flat taker rate without one), the slippage fraction
//! plus the market-impact model's (`vike_fills::market_impact`'s `measure_series` over the `window` bars that
//! closed before the fill bar, as `window_stats_before` reads them), then the fixed price amount
//! (`adverse_fill_amount`). Linear contracts only, no underlying (an option's fee on its underlying stays the
//! event engine's). With none of them set every fill is the frozen r3 arithmetic, bit for bit. A research study
//! runs its decisions through it over stored bars ([`VectorBacktestEngine::run_over_bars`]), charged the recipe's
//! `Costs`, so a study pays what a backtest pays.
//!
//! THE TARGET-WEIGHTS CONTRACT: `weights[t, s]` is the weight DECIDED at bar `t` (closes ≤ t),
//! realized at `t+1`'s open; NaN = "no rebalance decided" (queue nothing); weight 0.0 for a
//! held symbol drives a full close; the last bar's row is never acted on.
//!
//! **Maker/taker law (fee model follow-up 3):** every fill this kernel produces is a REBALANCE
//! crossing the market at the next bar's open (§1 above) — there is no resting-limit path here, so
//! `is_maker` is always `false` and every fill is charged `taker_fee`. That agrees with the event
//! engine's own classification (`is_maker := kind == Limit`) for the SAME strategy verb this kernel
//! mirrors (`OrderBrokerSimulator::strategy_order_target_percent`, which only ever submits `OrderKind::Market`
//! rebalances) — the two engines are reconciliation partners precisely because neither one has a
//! maker-classified fill on this path. `maker_fee` is threaded through the public API anyway — DEAD
//! at every call site today — so a future maker-classified fill path here cannot silently default
//! to `taker_fee` by omission; it must consciously wire this parameter in. `engine_kernel_parity`'s
//! fee-schedule-armed row proves the two engines agree bit-for-bit under a schedule with maker !=
//! taker precisely BECAUSE this kernel never reads its maker side.

use vike_marketdata::Bar;
use vike_model::{Costs, FeeSchedule, OrderFees, Trade, TradeFold};

use vike_analytics::{TradingRecord, TradingRecordSimulator, TradingRecordSource};
use vike_fills::broker_sim::{adverse_fill_amount, adverse_fill_price, commission};
use vike_fills::market_impact::{ImpactInputs, ProfileEngineSlippageTerms, measure_series};

const DEAD_BAND: f64 = 1e-12; // |delta shares| <= this -> skip (mirrors Strategy._engine_target)

/// A row-major (T, S) matrix of f64.
#[derive(Debug, Clone)]
pub struct Matrix {
    pub data: Vec<f64>,
    pub t: usize,
    pub s: usize,
}

impl Matrix {
    pub fn new(data: Vec<f64>, t: usize, s: usize) -> Self {
        assert_eq!(data.len(), t * s, "matrix shape mismatch");
        Matrix { data, t, s }
    }
    #[inline]
    pub fn at(&self, t: usize, s: usize) -> f64 {
        self.data[t * self.s + s]
    }
}

// Kernel output is the shared `TradingRecord` (see `vike_analytics::record`).

/// One-pass time×symbol simulation mirroring `MultiSymbolEngine.run` (cash_gate=False).
/// Per bar t: (1) fill deltas queued at t-1 at `opens[t,s]` in COLUMN order on shared cash;
/// (2) funding on held positions (marked at close); (3) mark equity once (shared snapshot);
/// (4) decide next deltas `w·eq/(close·multiplier) − pos` (dead-band 1e-12) — skipped on the last
/// bar, and held on a close that is no usable price.
///
/// The costs are the event engine's resolved fields (`StrategyEngineConfig`'s `fee_rate`/`fee_schedule`,
/// and `slippage`/`slippage_amount`/`impact` as `slippage`): a schedule overrides the flat rates, as there.
/// `volumes` is what the impact model measures its window's mean against; it is read only with a model.
#[expect(clippy::too_many_arguments)]
// index loops (not iterators) are deliberate: they mirror the Python kernel's `for s in
// range(S)` across six parallel state arrays, keeping the parity mapping line-for-line.
#[expect(clippy::needless_range_loop)]
pub fn fast_portfolio_backtest(
    opens: &Matrix,
    closes: &Matrix,
    volumes: &Matrix,
    funding: &Matrix,
    ts: &[i64],
    target_weights: &Matrix,
    // Threaded through but DEAD — see the module doc "Maker/taker law": this kernel never
    // produces a maker-classified fill, so `maker_fee` exists only to stop a future maker path
    // from silently reusing `taker_fee`.
    maker_fee: f64,
    taker_fee: f64,
    fee_schedule: Option<FeeSchedule>,
    slippage: &ProfileEngineSlippageTerms,
    init_cash: f64,
    multiplier: f64,
    symbols: Option<&[String]>,
    build_trades: bool,
) -> TradingRecord {
    let t_len = closes.t;
    let s_len = closes.s;
    assert_eq!(opens.t, t_len);
    assert_eq!(volumes.t, t_len);
    assert_eq!(funding.t, t_len);
    assert_eq!(target_weights.t, t_len);
    assert_eq!(ts.len(), t_len);

    let mut equity = vec![0.0_f64; t_len];
    let mut pos = vec![0.0_f64; s_len];
    let mut avg = vec![0.0_f64; s_len];
    let mut entry_fee = vec![0.0_f64; s_len];
    let mut entry_ts = vec![0_i64; s_len];
    let mut queued_side = vec![0_i32; s_len];
    let mut queued_quantity = vec![0.0_f64; s_len];
    let mut trades: Vec<Trade> = Vec::new();
    let mut nt = 0_usize;
    let mut cash = init_cash;
    let mut funding_paid = 0.0_f64; // NET funding cashflow (Σ -funding); surfaced on the result
    // Fills a configured impact model could not price (a window too short to measure): the
    // event engine's `OrderBrokerSimulator::impact_unpriced`, charged the flat fraction alone.
    let mut impact_unpriced = 0_u64;
    // The kernel's one contract: linear at the bare multiplier, so every product is the one it booked.
    let value = vike_model::ContractValue::linear(multiplier);

    for t in 0..t_len {
        // 1) Fill deltas queued at t-1 at THIS bar's open, in column order, on shared cash.
        for s in 0..s_len {
            if queued_quantity[s] <= 0.0 {
                continue;
            }
            let side = queued_side[s];
            let qty = queued_quantity[s];
            let raw_open = opens.at(t, s);
            // `OrderBrokerSimulator::slippage_for` on the bar lane: no model is the fraction itself (no
            // arithmetic, r3's bits); a model adds its estimate over the bars closed before this one.
            let slip = match &slippage.impact {
                None => slippage.fraction,
                Some((model, window)) => {
                    let lo = t.saturating_sub(*window);
                    let prices = (lo..t).map(|i| closes.at(i, s));
                    let sizes = (lo..t).map(|i| volumes.at(i, s));
                    match measure_series(prices, sizes, t - lo) {
                        Some(c) => {
                            let input = ImpactInputs {
                                qty,
                                average_volume: c.average_volume,
                                sigma: c.sigma,
                            };
                            slippage.fraction + model.impact_fraction(&input)
                        }
                        None => {
                            impact_unpriced += 1;
                            slippage.fraction
                        }
                    }
                }
            };
            let fill_price = adverse_fill_price(raw_open, side, slip);
            let (fill_price, _) = adverse_fill_amount(fill_price, side, slippage.amount, false);
            let fee = commission(
                fee_schedule,
                maker_fee,
                taker_fee,
                false,
                qty,
                fill_price,
                value,
                None,
                &mut OrderFees::default(),
            );
            let delta_signed = side as f64 * qty;
            cash -= fee; // transaction cost; the signed notional below moves the rest
            cash -= vike_model::signed_notional(delta_signed, fill_price, multiplier);

            // Drive the shared trade fold over this symbol's SoA state slot (copy in, fold, write
            // back) — one implementation shared with the event engine and the report.
            let mut fold = TradeFold {
                size: pos[s],
                average_price: avg[s],
                entry_fee: entry_fee[s],
                entry_timestamp: entry_ts[s],
            };
            let step = fold.apply(side, qty, fill_price, fee, ts[t], multiplier);
            pos[s] = fold.size;
            avg[s] = fold.average_price;
            entry_fee[s] = fold.entry_fee;
            entry_ts[s] = fold.entry_timestamp;
            if let Some(c) = step.closed {
                // reduce / close / flip -> a closed-portion Trade
                nt += 1;
                if build_trades {
                    trades.push(Trade {
                        entry_price: c.entry_price,
                        exit_price: c.exit_price,
                        size: c.size,
                        pnl: c.pnl,
                        fees: c.fees,
                        entry_timestamp: c.entry_timestamp,
                        exit_timestamp: c.exit_timestamp,
                        symbol: symbols.map(|sy| sy[s].clone()).unwrap_or_default(),
                        mae: 0.0,
                        mfe: 0.0,
                        is_long: c.is_long,
                    });
                }
            }
            // consume the queued order
            queued_quantity[s] = 0.0;
            queued_side[s] = 0;
        }

        // 2) Perp funding on held positions (marked at close).
        for s in 0..s_len {
            let f = funding.at(t, s);
            if f != 0.0 && pos[s] != 0.0 {
                let fc = value.funding(pos[s], closes.at(t, s), f);
                cash -= fc;
                funding_paid -= fc; // twin of the event engine's accrual (Σ MirrorFunding.amount)
            }
        }

        // 3) Mark-to-market equity (single shared snapshot — curve AND sizing).
        let mut eq = cash;
        for s in 0..s_len {
            eq += vike_model::signed_notional(pos[s], closes.at(t, s), multiplier);
        }
        equity[t] = eq;

        // 4) Decide next-bar deltas from this bar's target weights (skip the last bar).
        if t + 1 < t_len {
            for s in 0..s_len {
                let w = target_weights.at(t, s);
                if w.is_nan() {
                    continue; // NaN -> no rebalance decided for this symbol/bar
                }
                let c = closes.at(t, s);
                // No usable price: hold, the guard of `OrderBrokerSimulator::strategy_order_target_percent`
                // and of live. A NaN close holds too: it decided no delta before this guard.
                if c.is_nan() || c <= 0.0 {
                    continue;
                }
                let target_shares = value.units_worth(w * eq, c, 1.0).unwrap_or(0.0);
                let delta = target_shares - pos[s];
                if delta > DEAD_BAND {
                    queued_side[s] = 1;
                    queued_quantity[s] = delta;
                } else if delta < -DEAD_BAND {
                    queued_side[s] = -1;
                    queued_quantity[s] = -delta;
                }
                // else: |delta| <= dead-band -> no order
            }
        }
    }

    let final_equity = if t_len > 0 { equity[t_len - 1] } else { init_cash };
    TradingRecord {
        source: TradingRecordSource::Backtest(TradingRecordSimulator {
            impact_unpriced,
            ..Default::default()
        }),
        trades,
        equity_curve: equity,
        final_equity,
        n_trades: nt,
        funding_paid,
        ..Default::default()
    }
}

/// The one public vectorized backtest kernel — target-weight, any S (S=1 is the single case).
/// Zero-size struct grouping the batch entry point; the free `fast_portfolio_backtest` fn is the
/// implementation detail (and the r3 parity-gate target).
pub struct VectorBacktestEngine;

impl VectorBacktestEngine {
    /// One-pass vectorized backtest over a T×S target-weight matrix, charged `costs` (THE cost type,
    /// resolved as a backtest resolves it for the event engine: `Commission::engine_fee` and
    /// `vike_fills::market_impact::slippage_terms`). See `fast_portfolio_backtest`.
    ///
    /// # Errors
    ///
    /// A slippage table its own check refuses, in its words.
    #[expect(clippy::too_many_arguments)]
    pub fn run(
        opens: &Matrix,
        closes: &Matrix,
        volumes: &Matrix,
        funding: &Matrix,
        ts: &[i64],
        target_weights: &Matrix,
        costs: &Costs,
        init_cash: f64,
        multiplier: f64,
        symbols: Option<&[String]>,
        build_trades: bool,
    ) -> Result<TradingRecord, String> {
        let (fee_rate, fee_schedule) = costs.commission.engine_fee();
        let slippage = vike_fills::market_impact::slippage_terms(costs.slippage.as_ref())?;
        Ok(fast_portfolio_backtest(
            opens,
            closes,
            volumes,
            funding,
            ts,
            target_weights,
            fee_rate,
            fee_rate,
            fee_schedule,
            &slippage,
            init_cash,
            multiplier,
            symbols,
            build_trades,
        ))
    }

    /// [`Self::run`] over `legs`, one series of bars a symbol on ONE timestamp grid, at `target_weights`
    /// (row-major, a row a bar and a column a leg; `NaN` decides nothing), with no funding and no trade list:
    /// how a research study runs its decisions as a backtest runs them, charged `costs`, THE cost type.
    ///
    /// # Errors
    ///
    /// Legs that are not on one timestamp grid, weights of another shape, and what [`Self::run`] refuses.
    pub fn run_over_bars(
        legs: &[&[Bar]],
        target_weights: &[f64],
        costs: &Costs,
        init_cash: f64,
        multiplier: f64,
    ) -> Result<TradingRecord, String> {
        let ts: Vec<i64> =
            legs.first().map(|leg| leg.iter().map(|b| b.timestamp).collect()).unwrap_or_default();
        let (t_len, s_len) = (ts.len(), legs.len());
        if legs.iter().any(|leg| !leg.iter().map(|b| b.timestamp).eq(ts.iter().copied())) {
            return Err(
                "the legs are not on one timestamp grid: align them to the bars they all have"
                    .into(),
            );
        }
        if target_weights.len() != t_len * s_len {
            return Err(format!(
                "{} target weights for {t_len} bars of {s_len} legs: one a bar and leg",
                target_weights.len()
            ));
        }
        let column = |field: fn(&Bar) -> f64| {
            let values = (0..t_len).flat_map(|t| legs.iter().map(move |leg| field(&leg[t])));
            Matrix::new(values.collect(), t_len, s_len)
        };
        Self::run(
            &column(|b| b.open),
            &column(|b| b.close),
            &column(|b| b.volume),
            &Matrix::new(vec![0.0; t_len * s_len], t_len, s_len),
            &ts,
            &Matrix::new(target_weights.to_vec(), t_len, s_len),
            costs,
            init_cash,
            multiplier,
            None,
            false,
        )
    }
}
