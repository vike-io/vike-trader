//! Cost-gated pairs (statistical-arbitrage) strategy — a portable
//! `impl<B: Broker> Strategy<B>`, so the SAME type runs in the backtest sweep harness and live.
//! No Python twin, so NOT parity-gated.
//!
//! Structure adapted from Hummingbot's `controllers/generic/stat_arb.py` (Apache-2.0), with its
//! two substantive defects fixed:
//!
//! 1. **Beta SIZES the legs.** Hummingbot fits `beta` by OLS, then splits the legs 50/50 by dollar
//!    value (`pos_hedge_ratio = 1.0`), so it is not beta-neutral — the most common real-world
//!    pairs bug. Here the fitted beta sizes the hedge leg ([`beta_neutral_qtys`]).
//! 2. **Cost-gated, symmetric barriers.** Hummingbot's `+1% / −5%` barriers are EV-neutral BY
//!    CONSTRUCTION on a driftless process (`5/6·1% − 1/6·5% = 0`). Here entry requires the expected
//!    convergence to EXCEED a full round trip INCLUDING CARRY, and exit is symmetric (reversion or
//!    sign flip).
//!
//! ## A measurement instrument, not a product
//!
//! Published evidence (ten Binance perpetuals at 2bp/crossing) decomposes a round trip as gross
//! `+0.15%`, cost `−0.12%`, funding `−0.18%` ⇒ net `−0.15%`. This module reproduces that verdict
//! on vike's OWN data, fees and overfitting gates: expect a negative. It is also the only consumer
//! proving the `spread_quote` cost helper is wired into a decision path.

use toml::Value;

use vike_model::{Bar, Broker, SpreadLeg, Strategy, spread_total_cost};

use crate::controller::as_f64;

/// Unit quantities for a beta-neutral pair: `notional` worth of leg A against `beta` units of B
/// per unit of A (the spread `A − beta·B`), NOT a 50/50 dollar split. `None` for a non-positive
/// or non-finite beta, price or notional: refusing beats silently inverting a leg.
pub fn beta_neutral_qtys(
    notional: f64,
    price_a: f64,
    price_b: f64,
    beta: f64,
) -> Option<(f64, f64)> {
    let usable = notional.is_finite()
        && notional > 0.0
        && price_a.is_finite()
        && price_a > 0.0
        && price_b.is_finite()
        && price_b > 0.0
        && beta.is_finite()
        && beta > 0.0;
    if !usable {
        return None;
    }
    let qty_a = notional / price_a;
    Some((qty_a, beta * qty_a))
}

/// Population mean + standard deviation of the trailing `period` values, at the LAST element of
/// `values`; `None` when the window is short or any value is non-finite.
///
/// ⚠ Deliberately duplicates `vike_indicators::pairs::spread_zscore`'s kernel BIT-FOR-BIT, so the
/// full traversal is load-bearing: the same running sum from index 0 (a fresh trailing-window sum
/// differs at ~1e-12), then `E[s²] − E[s]²` clamped at zero.
/// `inline_zscore_matches_the_indicator_seam` asserts it bitwise, over the SAME slice (the
/// strategy's buffer is bounded).
pub fn rolling_mean_sd_last(values: &[f64], period: usize) -> Option<(f64, f64)> {
    if period == 0 || values.len() < period {
        return None;
    }
    let p = period as f64;
    let (mut run_sum, mut run_sum2) = (0.0f64, 0.0f64);
    let mut out: Option<(f64, f64)> = None;
    for (i, &v) in values.iter().enumerate() {
        if !v.is_finite() {
            return None;
        }
        run_sum += v;
        run_sum2 += v * v;
        if i >= period {
            let old = values[i - period];
            run_sum -= old;
            run_sum2 -= old * old;
        }
        if i + 1 >= period {
            let mean = run_sum / p;
            let var = run_sum2 / p - mean * mean;
            out = Some((mean, var.max(0.0).sqrt()));
        }
    }
    out
}

/// Enter only when `|z| > entry_z` AND the expected convergence `edge` exceeds the FULL cost —
/// crossings **plus carry**. A `None` cost (crossed or missing book) never enters: an
/// unpriceable trade is not a free one.
pub fn should_enter(z: f64, entry_z: f64, edge: f64, total_cost: Option<f64>) -> bool {
    match total_cost {
        Some(c) => z.is_finite() && z.abs() > entry_z && edge.is_finite() && edge > c,
        None => false,
    }
}

/// Exit on reversion INTO `exit_z`, or on a sign flip vs the z the position opened at. Symmetric
/// on purpose: an asymmetric `+1% / −5%` pair is EV-neutral on a driftless process yet LOOKS
/// excellent on a short sample (five winners per loser).
pub fn should_exit(z: f64, entry_z_at_open: f64, exit_z: f64) -> bool {
    if !z.is_finite() {
        return false;
    }
    z.abs() <= exit_z || (entry_z_at_open * z < 0.0)
}

/// Minimal close-only bars, for feeding the 2-series indicator seam.
fn bars_from(closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .map(|&c| Bar {
            ts: 0,
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

/// The cost-gated z-score pairs strategy (module doc); params via [`PairsZScore::from_params`].
#[derive(Debug, Clone)]
pub struct PairsZScore {
    /// Dependent leg — the `A` in `A − beta·B`. PINNED, never swept: which leg is dependent
    /// materially changes the hedge ratio (0.74 one way vs 1.27 the other; `1/0.74 ≈ 1.35`).
    pub symbol_a: String,
    /// Hedge leg — the `B`.
    pub symbol_b: String,
    /// Rolling window for the spread's mean/sd.
    pub period: usize,
    /// Enter when `|z| > entry_z`.
    pub entry_z: f64,
    /// Exit when `|z| <= exit_z` (or z flips sign).
    pub exit_z: f64,
    /// Hedge ratio in `A − beta·B`. Sizes the hedge leg — see [`beta_neutral_qtys`].
    pub beta: f64,
    /// Notional of leg A per entry.
    pub notional: f64,
    /// Taker fee as a fraction of notional, PER CROSSING.
    pub taker_fee: f64,
    /// Assumed half-spread per leg, in bps, for the executable quote (bars carry no book). Zero
    /// asserts a frictionless book — the assumption this strategy exists to test.
    pub half_spread_bps: f64,
    /// Expected funding intervals held, for the carry half of the cost floor.
    pub hold_intervals: f64,
    /// Per-interval funding rate for each leg, used when a bar carries none.
    pub funding_a: f64,
    pub funding_b: f64,
    /// Opt-in regime gate: refuse entry when the spread's OU half-life exceeds this many bars
    /// (or is NaN: not mean-reverting). `<= 0.0` disables it.
    pub max_half_life: f64,

    closes_a: Vec<f64>,
    closes_b: Vec<f64>,
    pending_a: Option<f64>,
    pending_b: Option<f64>,
    last_funding_a: Option<f64>,
    last_funding_b: Option<f64>,
    entry_z_at_open: Option<f64>,
}

impl Default for PairsZScore {
    fn default() -> Self {
        PairsZScore {
            symbol_a: String::new(),
            symbol_b: String::new(),
            period: 20,
            entry_z: 2.0,
            exit_z: 0.5,
            beta: 1.0,
            notional: 1000.0,
            taker_fee: 0.0005,
            half_spread_bps: 1.0,
            hold_intervals: 3.0,
            funding_a: 0.0,
            funding_b: 0.0,
            max_half_life: 0.0,
            closes_a: Vec::new(),
            closes_b: Vec::new(),
            pending_a: None,
            pending_b: None,
            last_funding_a: None,
            last_funding_b: None,
            entry_z_at_open: None,
        }
    }
}

impl PairsZScore {
    /// Read the knobs (`BuyHold` reader convention). `symbol_a`/`symbol_b` are REQUIRED in
    /// practice: with either empty no order routes (no single-symbol fallback for two legs).
    pub fn from_params(params: &Value) -> Self {
        let d = PairsZScore::default();
        PairsZScore {
            symbol_a: params
                .get("symbol_a")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or(d.symbol_a),
            symbol_b: params
                .get("symbol_b")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or(d.symbol_b),
            period: params
                .get("period")
                .and_then(as_f64)
                .map(|v| v.round().max(2.0) as usize)
                .unwrap_or(d.period),
            entry_z: params.get("entry_z").and_then(as_f64).unwrap_or(d.entry_z),
            exit_z: params.get("exit_z").and_then(as_f64).unwrap_or(d.exit_z),
            beta: params.get("beta").and_then(as_f64).unwrap_or(d.beta),
            notional: params.get("notional").and_then(as_f64).unwrap_or(d.notional),
            taker_fee: params.get("taker_fee").and_then(as_f64).unwrap_or(d.taker_fee),
            half_spread_bps: params
                .get("half_spread_bps")
                .and_then(as_f64)
                .unwrap_or(d.half_spread_bps),
            hold_intervals: params
                .get("hold_intervals")
                .and_then(as_f64)
                .unwrap_or(d.hold_intervals),
            funding_a: params.get("funding_a").and_then(as_f64).unwrap_or(d.funding_a),
            funding_b: params.get("funding_b").and_then(as_f64).unwrap_or(d.funding_b),
            max_half_life: params.get("max_half_life").and_then(as_f64).unwrap_or(d.max_half_life),
            ..PairsZScore::default()
        }
    }

    /// The two executable legs at the current prices, `half_spread_bps` wide.
    fn legs(&self, price_a: f64, price_b: f64) -> [SpreadLeg; 2] {
        let h = self.half_spread_bps / 10_000.0;
        [
            SpreadLeg {
                ratio: 1.0,
                bid: price_a * (1.0 - h),
                ask: price_a * (1.0 + h),
                taker_fee: self.taker_fee,
            },
            SpreadLeg {
                ratio: -self.beta,
                bid: price_b * (1.0 - h),
                ask: price_b * (1.0 + h),
                taker_fee: self.taker_fee,
            },
        ]
    }

    /// The opt-in regime gate. A NaN half-life (NOT mean-reverting) stands down, not a pass.
    fn half_life_ok(&self) -> bool {
        use vike_indicators::PairIndicator;
        use vike_indicators::pairs::HalfLife;
        let ind = HalfLife::with_params(&[self.period as f64, self.beta]);
        let cols = ind.vectorize_pair(&bars_from(&self.closes_a), &bars_from(&self.closes_b));
        match cols.first().and_then(|l| l.last()) {
            Some(&hl) if hl.is_finite() && hl > 0.0 => hl <= self.max_half_life,
            _ => false,
        }
    }

    fn flatten<B: Broker>(&mut self, broker: &mut B) {
        let syms = [self.symbol_a.clone(), self.symbol_b.clone()];
        for sym in syms {
            let p = broker.position(&sym);
            if p != 0.0 {
                let side = vike_model::closing_side(p);
                broker.submit_market(&sym, side, p.abs());
            }
        }
        self.entry_z_at_open = None;
    }

    /// One completed step (both legs advanced): recompute the spread stats and act.
    fn evaluate<B: Broker>(&mut self, broker: &mut B, price_a: f64, price_b: f64) {
        let n = self.closes_a.len();
        if n < self.period {
            return;
        }
        let spreads: Vec<f64> =
            (0..n).map(|i| self.closes_a[i] - self.beta * self.closes_b[i]).collect();
        let Some((mean, sd)) = rolling_mean_sd_last(&spreads, self.period) else {
            return;
        };
        if sd <= 0.0 {
            return; // a degenerate spread has no z-score
        }
        let z = (spreads[n - 1] - mean) / sd;
        if !z.is_finite() {
            return;
        }

        if broker.position(&self.symbol_a) != 0.0 {
            let entry = self.entry_z_at_open.unwrap_or(z);
            if should_exit(z, entry, self.exit_z) {
                self.flatten(broker);
            }
            return;
        }

        if self.max_half_life > 0.0 && !self.half_life_ok() {
            return;
        }

        let legs = self.legs(price_a, price_b);
        let rates = [
            self.last_funding_a.unwrap_or(self.funding_a),
            self.last_funding_b.unwrap_or(self.funding_b),
        ];
        let cost = spread_total_cost(&legs, &rates, self.hold_intervals);
        // Expected convergence back to the EXIT band (not the full |z|·sd: you exit at exit_z).
        let edge = (z.abs() - self.exit_z) * sd;
        if !should_enter(z, self.entry_z, edge, cost) {
            return;
        }

        let Some((qa, qb)) = beta_neutral_qtys(self.notional, price_a, price_b, self.beta) else {
            return;
        };
        // z > 0 ⇒ the spread is rich ⇒ SELL it: short A, long B. z < 0 mirrors.
        let side_a = if z > 0.0 { -1 } else { 1 };
        broker.submit_market(&self.symbol_a, side_a, qa);
        broker.submit_market(&self.symbol_b, -side_a, qb);
        self.entry_z_at_open = Some(z);
    }
}

impl<B: Broker> Strategy<B> for PairsZScore {
    fn warmup(&self) -> usize {
        self.period
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        // `on_bar` fires ONCE PER SYMBOL PER STEP: buffer each leg and act only once BOTH have
        // advanced, or the spread is evaluated against a stale opposite leg.
        let Some(sym) = bar.symbol.as_deref() else {
            return;
        };
        if sym == self.symbol_a {
            self.pending_a = Some(bar.close);
            if let Some(f) = bar.funding {
                self.last_funding_a = Some(f);
            }
        } else if sym == self.symbol_b {
            self.pending_b = Some(bar.close);
            if let Some(f) = bar.funding {
                self.last_funding_b = Some(f);
            }
        } else {
            return;
        }

        let (Some(a), Some(b)) = (self.pending_a, self.pending_b) else {
            return;
        };
        self.pending_a = None;
        self.pending_b = None;
        let prices_usable = a.is_finite() && a > 0.0 && b.is_finite() && b > 0.0;
        if !prices_usable {
            return;
        }
        self.closes_a.push(a);
        self.closes_b.push(b);
        // Bound the buffers: stats and half-life read at most `period` (+1 lagged difference).
        let cap = self.period.saturating_add(2);
        if self.closes_a.len() > cap {
            let drop = self.closes_a.len() - cap;
            self.closes_a.drain(0..drop);
            self.closes_b.drain(0..drop);
        }
        self.evaluate(broker, a, b);
    }
}

#[path = "pairs_tests.rs"]
#[cfg(test)]
mod pairs_tests;
