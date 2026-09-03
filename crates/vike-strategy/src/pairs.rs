//! Cost-gated pairs (statistical-arbitrage) strategy — a portable
//! `impl<B: Broker> Strategy<B>` in the `funding_capture` / `grid_dca` shape, so the
//! SAME type runs in the backtest sweep harness and live unchanged.
//!
//! NET-NEW Rust surface — no Python twin, so NOT parity-gated. Structure adapted
//! from Hummingbot's `controllers/generic/stat_arb.py` (Apache-2.0), with its two
//! substantive defects deliberately fixed:
//!
//! 1. **Beta SIZES the legs.** Hummingbot fits `beta` by OLS, uses it only to
//!    define the residual, then splits the legs 50/50 by dollar value
//!    (`total * (1/(1+pos_hedge_ratio))`, `pos_hedge_ratio = 1.0`) — beta-blind, so
//!    the position is not beta-neutral despite beta being computed. Here the fitted
//!    beta sizes the hedge leg ([`beta_neutral_qtys`]). This is also the single most
//!    common real-world pairs bug: the same inconsistency appears independently in
//!    several published implementations.
//! 2. **Cost-gated, symmetric barriers.** Hummingbot's `+1% / −5%` global barriers
//!    are EV-neutral BY CONSTRUCTION on a driftless process (`5/6·1% − 1/6·5% = 0`
//!    exactly), so all of its expectancy has to come from its z-score — which is an
//!    OLS residual of one non-stationary cumulative-return path regressed on
//!    another, z-scored against IN-SAMPLE moments. This strategy instead requires
//!    the expected convergence to EXCEED a full round trip INCLUDING CARRY before
//!    entering, and exits symmetrically on reversion or sign flip.
//!
//! ## This is a measurement instrument, not a product
//!
//! Published evidence says a crypto 2-asset pair does not clear its own cost floor:
//! measured on ten Binance perpetuals at 2bp/crossing (an order of magnitude cheaper
//! than a retail taker pays), a round trip decomposes as gross `+0.15%`, trading cost
//! `−0.12%`, funding `−0.18%` ⇒ net `−0.15%`. The signal is real and it is ~15bp.
//! The purpose of this module is to reproduce that verdict on vike's OWN data, with
//! vike's OWN fees and venues, through vike's OWN overfitting gates — rather than
//! inherit someone else's. Expect a negative; that is the successful outcome.
//!
//! It is also the only consumer that verifies the Phase-1 cost helper is wired into
//! a decision path at all: `spread_quote`'s unit tests prove the arithmetic in
//! isolation, but only a strategy calling it over real bars proves the gate rejects
//! what it should.

use toml::Value;

use vike_model::{spread_total_cost, Bar, Broker, SpreadLeg, Strategy};

/// Read a TOML value as `f64`, accepting a float OR integer (`beta = 1` ==
/// `beta = 1.0`) — the lenient numeric reader the other registry strategies use.
fn as_f64(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// Unit quantities for a beta-neutral pair position: `notional` worth of leg A
/// against `beta` units of B per unit of A, matching the spread `A − beta·B`.
///
/// The hedge leg is scaled by the FITTED beta, not split 50/50 by dollar value —
/// see the module doc for why that distinction is the whole point.
///
/// `None` for a non-positive or non-finite beta, price or notional: a hedge ratio
/// that is not a positive number is not a hedge, and refusing beats silently
/// inverting a leg.
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

/// Population mean + standard deviation of the trailing `period` values, at the LAST
/// element of `values`.
///
/// Mirrors `vike_indicators::pairs::spread_zscore`'s kernel EXACTLY — not merely its
/// formula but its ALGORITHM: a running sum accumulated from index 0 with the
/// outgoing element subtracted as the window slides, then population variance
/// `E[s²] − E[s]²` clamped at zero before the square root.
///
/// The full traversal is load-bearing and is NOT an oversight. Summing only the
/// trailing window afresh is mathematically identical but numerically different —
/// a running sum carries accumulated rounding from every earlier element, and the
/// two disagree at ~1e-12. Reproducing the recurrence makes this BIT-IDENTICAL to
/// the seam, which `inline_zscore_matches_the_indicator_seam` asserts bitwise
/// rather than within a tolerance.
///
/// Note the strategy feeds this a BOUNDED buffer, so its z is bit-identical to the
/// seam computed over that same slice — not to a seam plotted over unbounded
/// history. The contract pinned here is the definition, which is what could
/// silently drift.
///
/// `None` when the window is short or any value is non-finite.
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

/// Enter only when BOTH hold: the z-score is outside `entry_z`, AND the expected
/// convergence `edge` exceeds the FULL cost — crossings **plus carry**.
///
/// `total_cost` is `None` when the cost is unknowable (a crossed or missing book),
/// in which case this is always `false`: an unpriceable trade is not a free one.
/// The cost gate is what separates this from a naive z-score strategy, and it is
/// expected to reject the large majority of band breaks.
pub fn should_enter(z: f64, entry_z: f64, edge: f64, total_cost: Option<f64>) -> bool {
    match total_cost {
        Some(c) => z.is_finite() && z.abs() > entry_z && edge.is_finite() && edge > c,
        None => false,
    }
}

/// Exit on reversion INTO `exit_z`, or on a sign flip relative to the z the
/// position was opened at.
///
/// Symmetric by construction — deliberately NOT an asymmetric take-profit/stop pair
/// like Hummingbot's `+1% / −5%`, which is EV-neutral on a driftless process
/// (`5/6·1% − 1/6·5% = 0`) while LOOKING excellent over a short sample (five winners
/// per loser), exactly the shape PSR/deflated-Sharpe exist to catch.
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

/// The cost-gated z-score pairs strategy. See the module doc for the contract and
/// for why this exists as a measurement instrument; params via
/// [`PairsZScore::from_params`].
#[derive(Debug, Clone)]
pub struct PairsZScore {
    /// Dependent leg — the `A` in the spread `A − beta·B`. PINNED, never swept:
    /// which leg is dependent is a researcher degree of freedom that materially
    /// changes the hedge ratio (a verified example: 0.74 one way vs 1.27 the other,
    /// and `1/0.74 ≈ 1.35 ≠ 1.27`).
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
    /// Assumed half-spread per leg, in bps, used to build the executable quote when
    /// bars carry no book. NOT optional in spirit: leaving it at zero asserts a
    /// frictionless book, which is the assumption this strategy exists to test.
    pub half_spread_bps: f64,
    /// Expected funding intervals held, for the carry half of the cost floor.
    pub hold_intervals: f64,
    /// Per-interval funding rate for each leg, used when a bar carries none.
    pub funding_a: f64,
    pub funding_b: f64,
    /// Opt-in regime gate: refuse entry when the spread's OU half-life exceeds this
    /// many bars (or is NaN, i.e. not mean-reverting at all). `<= 0.0` disables it.
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
    /// Read the sweep-able knobs from the harness TOML params table (the `BuyHold`
    /// reader convention — unknown keys ignored, missing keys fall back to
    /// defaults). `symbol_a`/`symbol_b` are REQUIRED in practice: with either empty
    /// the strategy never routes an order (there is no single-symbol fallback for a
    /// two-leg trade).
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

    /// The two executable legs at the current prices, using `half_spread_bps` as the
    /// assumed book width (bars carry no L2).
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

    /// The opt-in mean-reversion regime gate. A NaN half-life means the window is
    /// NOT mean-reverting, which is a stand-down, not a pass.
    fn half_life_ok(&self) -> bool {
        use vike_indicators::pairs::HalfLife;
        use vike_indicators::PairIndicator;
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

    /// One completed step: both legs have advanced, so recompute the spread stats
    /// and act.
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
        // Expected convergence: from the entry band back to the exit band, in spread
        // price units. Deliberately NOT the full |z|·sd — you exit at exit_z, not at
        // the mean.
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
        // `on_bar` fires ONCE PER SYMBOL PER STEP and the bar carries its symbol tag,
        // so a two-leg strategy must buffer each leg and only act once BOTH have
        // advanced. Acting on a single leg's bar would evaluate the spread against a
        // stale opposite leg.
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
        // Bound the buffers: the stats and the half-life fit both read at most
        // `period` (+1 for the half-life's lagged difference).
        let cap = self.period.saturating_add(2);
        if self.closes_a.len() > cap {
            let drop = self.closes_a.len() - cap;
            self.closes_a.drain(0..drop);
            self.closes_b.drain(0..drop);
        }
        self.evaluate(broker, a, b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spread `A − β·B` means one unit of spread is 1 unit of A against β units
    /// of B. Sizing must use that β, not a 50/50 dollar split (the Hummingbot bug).
    #[test]
    fn hedge_leg_is_scaled_by_beta_not_split_evenly() {
        let (qa, qb) = beta_neutral_qtys(1000.0, 100.0, 50.0, 2.0).unwrap();
        assert!((qa - 10.0).abs() < 1e-12, "qty_a {qa}");
        assert!((qb - 20.0).abs() < 1e-12, "qty_b {qb}"); // 2.0 * 10.0
    }

    /// beta = 1 is the degenerate equal-UNITS case, NOT equal-dollars. A 50/50
    /// dollar split would give qty_b = 20 here; beta-neutral gives 10.
    #[test]
    fn beta_one_is_equal_units_not_equal_dollars() {
        let (qa, qb) = beta_neutral_qtys(1000.0, 100.0, 50.0, 1.0).unwrap();
        assert!((qa - 10.0).abs() < 1e-12);
        assert!((qb - 10.0).abs() < 1e-12, "equal UNITS: {qb}");
    }

    /// A non-positive or non-finite beta is not a hedge — refuse rather than invert
    /// a leg behind the caller's back.
    #[test]
    fn non_positive_or_nonfinite_beta_is_refused() {
        assert!(beta_neutral_qtys(1000.0, 100.0, 50.0, 0.0).is_none());
        assert!(beta_neutral_qtys(1000.0, 100.0, 50.0, -1.5).is_none());
        assert!(beta_neutral_qtys(1000.0, 100.0, 50.0, f64::NAN).is_none());
    }

    #[test]
    fn bad_prices_or_notional_are_refused() {
        assert!(beta_neutral_qtys(1000.0, 0.0, 50.0, 1.0).is_none());
        assert!(beta_neutral_qtys(1000.0, 100.0, 0.0, 1.0).is_none());
        assert!(beta_neutral_qtys(0.0, 100.0, 50.0, 1.0).is_none());
        assert!(beta_neutral_qtys(f64::INFINITY, 100.0, 50.0, 1.0).is_none());
    }

    /// Dollar-neutrality is NOT the same as beta-neutrality, and the difference
    /// grows with beta — pinned so a future "simplification" back to a 50/50 split
    /// fails loudly.
    #[test]
    fn beta_neutral_differs_from_dollar_neutral_when_beta_is_not_one() {
        let notional = 1000.0;
        let (qa, qb) = beta_neutral_qtys(notional, 100.0, 50.0, 3.0).unwrap();
        let dollar_neutral_qb = notional / 50.0; // what a 50/50 split would size
        assert!((qa - 10.0).abs() < 1e-12);
        assert!((qb - 30.0).abs() < 1e-12);
        assert!(
            (qb - dollar_neutral_qb).abs() > 1e-9,
            "beta-neutral {qb} must differ from dollar-neutral {dollar_neutral_qb}"
        );
    }

    // --- entry / exit rules ---

    /// A band break alone is NOT enough: the expected convergence must also clear a
    /// full round trip. This is the gate that makes the harness honest.
    #[test]
    fn entry_requires_both_a_band_break_and_a_cleared_cost_floor() {
        // |z| = 3 breaks a 2.0 band, but an edge of 1.0 is below a 40.0 round trip.
        assert!(!should_enter(3.0, 2.0, 1.0, Some(40.0)));
        // Same band break, edge now exceeds the round trip.
        assert!(should_enter(3.0, 2.0, 41.0, Some(40.0)));
        // Ample edge but the band is not broken.
        assert!(!should_enter(1.0, 2.0, 41.0, Some(40.0)));
    }

    /// An unknowable cost is never a free one — a missing/crossed book must block
    /// entry rather than be treated as zero cost.
    #[test]
    fn entry_is_refused_when_the_cost_is_unknowable() {
        assert!(!should_enter(5.0, 2.0, 1e9, None));
    }

    #[test]
    fn entry_is_refused_on_non_finite_inputs() {
        assert!(!should_enter(f64::NAN, 2.0, 100.0, Some(1.0)));
        assert!(!should_enter(5.0, 2.0, f64::NAN, Some(1.0)));
    }

    /// Exit is symmetric: reversion INTO the band, or a sign flip vs the entry z.
    #[test]
    fn exit_on_reversion_or_sign_flip() {
        assert!(should_exit(0.4, 2.5, 0.5), "reverted inside the exit band");
        assert!(!should_exit(1.2, 2.5, 0.5), "still outside the exit band");
        assert!(should_exit(-0.9, 2.5, 0.5), "sign flip vs the entry z");
        assert!(!should_exit(f64::NAN, 2.5, 0.5), "NaN holds rather than churns");
    }

    // --- z-score definition parity with the indicator seam ---

    /// The strategy computes its z inline (it needs the sd for the edge estimate
    /// anyway), so this pins that the inline definition matches
    /// `vike_indicators::pairs::spread_zscore` EXACTLY. Without this gate the z the
    /// strategy trades on could silently drift from the z the seam plots — a
    /// population-vs-sample variance change would be invisible otherwise.
    #[test]
    fn inline_zscore_matches_the_indicator_seam() {
        use vike_indicators::pairs::SpreadZscore;
        use vike_indicators::PairIndicator;

        let period = 20usize;
        let beta = 1.0f64;
        let a: Vec<f64> = (0..80).map(|i| 100.0 + (i as f64 * 0.13).sin() * 6.0).collect();
        let b: Vec<f64> = (0..80).map(|i| 50.0 + (i as f64 * 0.09).cos() * 3.0).collect();

        let ind = SpreadZscore::with_params(&[period as f64, beta]);
        let seam = ind.vectorize_pair(&bars_from(&a), &bars_from(&b));
        let seam_z = &seam[0];

        let spreads: Vec<f64> = a.iter().zip(b.iter()).map(|(x, y)| x - beta * y).collect();
        for end in period..=spreads.len() {
            let (mean, sd) = rolling_mean_sd_last(&spreads[..end], period).unwrap();
            let mine = (spreads[end - 1] - mean) / sd;
            let theirs = seam_z[end - 1];
            // BIT-exact, not a tolerance: the inline kernel reproduces the seam's
            // running-sum recurrence, so any drift in definition OR summation order
            // fails here rather than hiding under an epsilon.
            assert_eq!(
                mine.to_bits(),
                theirs.to_bits(),
                "idx {}: inline z {mine} != seam z {theirs}",
                end - 1
            );
        }
    }

    // --- strategy wiring ---

    /// A minimal `Broker` double: records orders and folds them into positions, so
    /// the tests can assert on what the strategy actually ROUTED.
    #[derive(Default)]
    struct MockBroker {
        orders: Vec<(String, i32, f64)>,
        positions: std::collections::HashMap<String, f64>,
        empty: Vec<Bar>,
    }

    impl Broker for MockBroker {
        fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
            self.orders.push((symbol.to_string(), side, qty));
            *self.positions.entry(symbol.to_string()).or_insert(0.0) += f64::from(side) * qty;
        }
        fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
        fn position(&self, symbol: &str) -> f64 {
            self.positions.get(symbol).copied().unwrap_or(0.0)
        }
        fn price(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _symbol: &str) -> &[Bar] {
            &self.empty
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            0
        }
    }

    fn tagged_bar(sym: &str, close: f64) -> Bar {
        Bar {
            ts: 0,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(sym.to_string()),
        }
    }

    /// A two-leg strategy must not act on one leg's bar: until BOTH legs advance the
    /// spread would be evaluated against a STALE opposite leg. `on_bar` fires once
    /// per symbol per step, so this is the core wiring contract.
    #[test]
    fn a_step_requires_both_legs_to_advance() {
        let mut s =
            PairsZScore { symbol_a: "A".into(), symbol_b: "B".into(), ..Default::default() };
        let mut br = MockBroker::default();
        for i in 0..5 {
            s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i)));
        }
        assert_eq!(s.closes_a.len(), 0, "no step may complete on one leg alone");
        s.on_bar(&mut br, &tagged_bar("B", 50.0));
        assert_eq!(s.closes_a.len(), 1, "the step completes once B arrives");
        assert_eq!(s.closes_b.len(), 1);
    }

    /// Bars for an unrelated symbol must be ignored entirely.
    #[test]
    fn unknown_symbols_are_ignored() {
        let mut s =
            PairsZScore { symbol_a: "A".into(), symbol_b: "B".into(), ..Default::default() };
        let mut br = MockBroker::default();
        s.on_bar(&mut br, &tagged_bar("ZZZ", 1.0));
        s.on_bar(&mut br, &tagged_bar("A", 100.0));
        assert_eq!(s.closes_a.len(), 0, "an unrelated symbol must not complete a step");
    }

    /// Drive a diverging spread through the strategy twice — once with a book so
    /// wide the round trip cannot be cleared, once with a thin book. THE COST GATE
    /// IS THE ONLY DIFFERENCE, so this proves it is genuinely wired into the
    /// decision path rather than merely unit-tested in isolation.
    #[test]
    fn the_cost_gate_actually_blocks_entry() {
        fn run(half_spread_bps: f64, taker_fee: f64) -> usize {
            let mut s = PairsZScore {
                symbol_a: "A".into(),
                symbol_b: "B".into(),
                period: 10,
                entry_z: 1.5,
                half_spread_bps,
                taker_fee,
                hold_intervals: 0.0,
                ..Default::default()
            };
            let mut br = MockBroker::default();
            // 10 flat steps to fill the window, then a large one-sided divergence.
            for i in 0..10 {
                s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
                s.on_bar(&mut br, &tagged_bar("B", 50.0));
            }
            for _ in 0..3 {
                s.on_bar(&mut br, &tagged_bar("A", 130.0));
                s.on_bar(&mut br, &tagged_bar("B", 50.0));
            }
            br.orders.len()
        }

        let thin = run(0.5, 0.0);
        let wide = run(5_000.0, 0.05); // a 50% half-spread + 5% fee: nothing can clear it
        assert!(thin > 0, "a thin book with a big divergence should trade (got {thin} orders)");
        assert_eq!(wide, 0, "a prohibitive book must block entry entirely (got {wide} orders)");
    }

    /// Both legs must be routed on entry, in OPPOSITE directions, with the hedge leg
    /// sized by beta — the beta-neutrality contract, checked end-to-end.
    #[test]
    fn entry_routes_two_opposite_legs_sized_by_beta() {
        let mut s = PairsZScore {
            symbol_a: "A".into(),
            symbol_b: "B".into(),
            period: 10,
            entry_z: 1.5,
            beta: 2.0,
            notional: 1000.0,
            half_spread_bps: 0.5,
            taker_fee: 0.0,
            hold_intervals: 0.0,
            ..Default::default()
        };
        let mut br = MockBroker::default();
        for i in 0..10 {
            s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        for _ in 0..3 {
            s.on_bar(&mut br, &tagged_bar("A", 130.0));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        assert!(br.orders.len() >= 2, "expected a two-leg entry, got {:?}", br.orders);
        let (sym_a, side_a, qty_a) = br.orders[0].clone();
        let (sym_b, side_b, qty_b) = br.orders[1].clone();
        assert_eq!(sym_a, "A");
        assert_eq!(sym_b, "B");
        assert_eq!(side_a, -side_b, "legs must be routed in opposite directions");
        // qty_a = notional/price_a = 1000/130; qty_b = beta * qty_a
        assert!(
            (qty_b - 2.0 * qty_a).abs() < 1e-9,
            "hedge leg must be beta-scaled: {qty_a}/{qty_b}"
        );
    }

    /// A rich spread (z > 0) is SOLD: short the dependent leg, long the hedge.
    #[test]
    fn a_rich_spread_is_sold() {
        let mut s = PairsZScore {
            symbol_a: "A".into(),
            symbol_b: "B".into(),
            period: 10,
            entry_z: 1.5,
            half_spread_bps: 0.5,
            taker_fee: 0.0,
            hold_intervals: 0.0,
            ..Default::default()
        };
        let mut br = MockBroker::default();
        for i in 0..10 {
            s.on_bar(&mut br, &tagged_bar("A", 100.0 + f64::from(i % 2)));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        // A jumps: the spread A - B is RICH, so A should be SHORTED.
        for _ in 0..3 {
            s.on_bar(&mut br, &tagged_bar("A", 130.0));
            s.on_bar(&mut br, &tagged_bar("B", 50.0));
        }
        assert!(!br.orders.is_empty(), "expected an entry");
        assert_eq!(br.orders[0].1, -1, "a rich spread must SHORT the dependent leg");
    }

    /// Defaults must resolve from an empty params table — the contract
    /// `registry_lists_every_match_arm` enforces for every roster entry.
    #[test]
    fn from_params_resolves_with_an_empty_table() {
        let empty: Value = toml::from_str("").unwrap();
        let s = PairsZScore::from_params(&empty);
        assert_eq!(s.period, 20);
        assert!((s.entry_z - 2.0).abs() < 1e-12);
        assert!((s.beta - 1.0).abs() < 1e-12);
        assert!(s.symbol_a.is_empty(), "no symbol default — a two-leg trade needs both named");
    }

    /// Params actually override, and `period` is floored at 2 (a 1-bar window has no
    /// dispersion).
    #[test]
    fn from_params_reads_overrides() {
        let t: Value = toml::from_str(
            r#"symbol_a = "ETH"
symbol_b = "BTC"
period = 50
entry_z = 2.5
beta = 1.7
"#,
        )
        .unwrap();
        let s = PairsZScore::from_params(&t);
        assert_eq!(s.symbol_a, "ETH");
        assert_eq!(s.symbol_b, "BTC");
        assert_eq!(s.period, 50);
        assert!((s.entry_z - 2.5).abs() < 1e-12);
        assert!((s.beta - 1.7).abs() < 1e-12);

        let floored: Value = toml::from_str("period = 1").unwrap();
        assert_eq!(PairsZScore::from_params(&floored).period, 2, "period floored at 2");
    }
}
