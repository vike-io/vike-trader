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

use vike_model::{Trade, TradeFold};

use vike_analytics::BacktestResult;
use vike_fills::broker_sim::adverse_fill_price;

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

// Kernel output is the shared `BacktestResult` (see `vike_analytics::result`).

/// One-pass time×symbol simulation mirroring `MultiSymbolEngine.run` (cash_gate=False).
/// Per bar t: (1) fill deltas queued at t-1 at `opens[t,s]` in COLUMN order on shared cash;
/// (2) funding on held positions (marked at close); (3) mark equity once (shared snapshot);
/// (4) decide next deltas `w·eq/(close·multiplier) − pos` (dead-band 1e-12) — skipped on the last
/// bar, and held on a close that is no usable price.
#[expect(clippy::too_many_arguments)]
// index loops (not iterators) are deliberate: they mirror the Python kernel's `for s in
// range(S)` across six parallel state arrays, keeping the parity mapping line-for-line.
#[expect(clippy::needless_range_loop)]
pub fn fast_portfolio_backtest(
    opens: &Matrix,
    closes: &Matrix,
    funding: &Matrix,
    ts: &[i64],
    target_weights: &Matrix,
    // Threaded through but DEAD — see the module doc "Maker/taker law": this kernel never
    // produces a maker-classified fill, so `_maker_fee` exists only to stop a future maker path
    // from silently reusing `taker_fee`.
    _maker_fee: f64,
    taker_fee: f64,
    slippage: f64,
    init_cash: f64,
    multiplier: f64,
    symbols: Option<&[String]>,
    build_trades: bool,
) -> BacktestResult {
    let t_len = closes.t;
    let s_len = closes.s;
    assert_eq!(opens.t, t_len);
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
            let fill_price = adverse_fill_price(raw_open, side, slippage);
            let fee = value.fee(qty, fill_price, taker_fee);
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
    BacktestResult {
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
    /// One-pass vectorized backtest over a T×S target-weight matrix. See `fast_portfolio_backtest`.
    #[expect(clippy::too_many_arguments)]
    pub fn run(
        opens: &Matrix,
        closes: &Matrix,
        funding: &Matrix,
        ts: &[i64],
        target_weights: &Matrix,
        maker_fee: f64,
        taker_fee: f64,
        slippage: f64,
        init_cash: f64,
        multiplier: f64,
        symbols: Option<&[String]>,
        build_trades: bool,
    ) -> BacktestResult {
        fast_portfolio_backtest(
            opens,
            closes,
            funding,
            ts,
            target_weights,
            maker_fee,
            taker_fee,
            slippage,
            init_cash,
            multiplier,
            symbols,
            build_trades,
        )
    }
}
