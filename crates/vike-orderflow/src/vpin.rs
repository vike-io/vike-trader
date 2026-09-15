//! VPIN — Volume-synchronized Probability of INformed trading (Easley / López de Prado;
//! mechanism per the VisualHFT studies + standard literature, independent implementation).
//! The trade stream is chopped into fixed-VOLUME buckets of `bucket_volume`; a trade that
//! overflows the open bucket spills its remainder into the next (same splitting convention
//! as `bars::VolumeBarBuilder`). Each completed bucket scores an order-flow toxicity
//! imbalance |Vbuy − Vsell| / Vbucket ∈ [0, 1]; VPIN is the rolling mean of the last
//! `window` (default 50) completed buckets, maintained as an O(1) rolling sum
//! (add-newest/subtract-evicted — never a window re-scan on the hot path).
//!
//! Classification: with `has_aggressor` (the default) each trade routes by `classify`
//! (`is_buyer_maker` — the crate's one load-bearing semantic). Feeds without a real
//! aggressor flag construct with `has_aggressor = false` and pass the prevailing mid to
//! `push`: `price >= mid` → Buy, else Sell (the quote-midpoint test). A missing mid falls
//! back to the aggressor flag so no trade is ever dropped.
//!
//! Forming/committed convention (mirrors `bars`): `committed()` reads completed buckets
//! only; `forming_bucket()`/`interim()` are pure reads folding the open bucket's partial
//! imbalance (|b − s| / filled) in as the NEWEST window sample (evicting the oldest when
//! the window is full), without mutating state.
//!
//! # `BvcVpin` — the canonical Easley/López de Prado/O'Hara VPIN
//!
//! Cite: Easley, López de Prado & O'Hara (2012), "Flow Toxicity and Liquidity in a
//! High-Frequency World", *Review of Financial Studies* 25(5):1457–1493 — the paper that
//! defines VPIN and **Bulk Volume Classification (BVC)**. `BvcVpin` is the aggressor-free
//! twin of `Vpin`: it needs no `is_buyer_maker` flag, only price + volume.
//!
//! - **Buckets:** the tape is chopped into equal-VOLUME buckets of size `bucket_volume`
//!   (VBS); a trade overflowing the open bucket spills its remainder into the next (same
//!   convention as `Vpin`/`bars::VolumeBarBuilder`). Each bucket's CLOSE price is the price
//!   of the trade that filled it.
//! - **BVC:** a bucket's directional split is derived from its price change, not from
//!   trade-level aggressor tags. `buy_frac = Φ((P_i − P_{i−1}) / σ_ΔP)`, where `Φ` is the
//!   standard-normal CDF (`norm_cdf`, `libm` erf — mirrors `vike-options`) and `σ_ΔP` is
//!   the (population) stdev of bucket price changes `ΔP` over the last `window` buckets.
//!   Then `V_buy = VBS·buy_frac`, `V_sell = VBS·(1 − buy_frac)`, and the bucket imbalance is
//!   `|V_buy − V_sell| / VBS = |2·buy_frac − 1| ∈ [0, 1]`.
//! - **VPIN:** `value() = (1/N)·Σ imbalance_i` over the last `N = window` completed buckets,
//!   an O(1) rolling mean; **`None` until `window` bucket imbalances exist** (the first
//!   bucket close only sets the `P_{i−1}` reference — a price CHANGE needs two closes, so N
//!   imbalances need N+1 closes).
//! - **Tick-rule option:** `VolumeClassifier::TickRule` classifies each bucket wholly by the
//!   SIGN of `ΔP` (`up → all buy`, `down → all sell`, `flat → balanced`) — the degenerate
//!   limit BVC itself falls back to when `σ_ΔP` is undefined.
//! - **Degenerate guards:** `bucket_volume ≤ 0`/non-finite and `window < 1` panic at
//!   construction; a non-positive-size trade is skipped; `σ_ΔP ≤ 0` (or `< 2` ΔP samples,
//!   i.e. the warm-up) → per-bucket tick-rule fallback, so a flat tape yields `0.0` (no NaN
//!   from `Φ(0/0)`) and a one-way tape yields `1.0`.
//! - **Directional interface:** `value()` is a SIDE-LESS toxicity magnitude; `directional_bias()`
//!   is the signed rolling mean of `(2·buy_frac − 1) ∈ [−1, 1]` (>0 buy-toxic → ask side, <0
//!   sell-toxic → bid side) so a caller can route the magnitude onto a per-side gate.
use std::collections::VecDeque;

use crate::classify::{Side, classify};
use vike_model::TradeTick;

pub const DEFAULT_VPIN_WINDOW: usize = 50;

/// The open (unfinished) volume bucket — buy/sell attribution so far.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VpinBucket {
    pub buy_vol: f64,
    pub sell_vol: f64,
}

impl VpinBucket {
    /// Total volume filled so far.
    pub fn filled(&self) -> f64 {
        self.buy_vol + self.sell_vol
    }
    /// Partial imbalance |b − s| / filled (the interim per-bucket estimate).
    pub fn imbalance(&self) -> f64 {
        (self.buy_vol - self.sell_vol).abs() / self.filled()
    }
}

pub struct Vpin {
    bucket_volume: f64,
    window: usize,
    has_aggressor: bool,
    /// forming bucket
    buy: f64,
    sell: f64,
    /// last ≤ `window` completed-bucket imbalances (front = oldest)
    ring: VecDeque<f64>,
    /// O(1) rolling Σ of `ring`
    sum: f64,
}

impl Vpin {
    /// Aggressor-classified, `window = DEFAULT_VPIN_WINDOW`.
    pub fn new(bucket_volume: f64) -> Self {
        Self::with_params(bucket_volume, DEFAULT_VPIN_WINDOW, true)
    }

    pub fn with_params(bucket_volume: f64, window: usize, has_aggressor: bool) -> Self {
        assert!(
            bucket_volume > 0.0 && bucket_volume.is_finite(),
            "Vpin bucket_volume must be finite and > 0"
        );
        assert!(window >= 1, "Vpin window must be >= 1");
        Vpin {
            bucket_volume,
            window,
            has_aggressor,
            buy: 0.0,
            sell: 0.0,
            ring: VecDeque::with_capacity(window),
            sum: 0.0,
        }
    }

    /// Fold one trade; returns the imbalances of every bucket this trade COMPLETED
    /// (usually empty; ≥ 2 when one big trade spans several buckets — mirrors
    /// `VolumeBarBuilder::push` returning the closed bars).
    pub fn push(&mut self, t: &TradeTick, mid: Option<f64>) -> Vec<f64> {
        let mut out = Vec::new();
        if t.size <= 0.0 {
            return out;
        }
        let is_buy = if self.has_aggressor {
            classify(t) == Side::Buy
        } else if let Some(m) = mid {
            t.price >= m
        } else {
            classify(t) == Side::Buy // no mid available — best effort, never drop
        };
        let mut remaining = t.size;
        while remaining > 0.0 {
            let room = self.bucket_volume - (self.buy + self.sell);
            let take = remaining.min(room);
            if is_buy {
                self.buy += take;
            } else {
                self.sell += take;
            }
            remaining -= take;
            if self.buy + self.sell >= self.bucket_volume {
                let imb = (self.buy - self.sell).abs() / self.bucket_volume;
                self.commit(imb);
                out.push(imb);
                self.buy = 0.0;
                self.sell = 0.0;
            }
        }
        out
    }

    /// O(1): evict the oldest sample when full, fold the new one into the rolling sum.
    fn commit(&mut self, imb: f64) {
        if self.ring.len() == self.window {
            let old = self.ring.pop_front().expect("window >= 1");
            self.sum -= old;
        }
        self.ring.push_back(imb);
        self.sum += imb;
    }

    pub fn completed_buckets(&self) -> usize {
        self.ring.len()
    }

    /// Committed VPIN — mean of the last ≤ `window` COMPLETED bucket imbalances.
    /// None until the first bucket completes.
    pub fn committed(&self) -> Option<f64> {
        if self.ring.is_empty() { None } else { Some(self.sum / self.ring.len() as f64) }
    }

    /// The open bucket, if any volume has accrued. Pure read.
    pub fn forming_bucket(&self) -> Option<VpinBucket> {
        (self.buy + self.sell > 0.0)
            .then_some(VpinBucket { buy_vol: self.buy, sell_vol: self.sell })
    }

    /// Interim VPIN: `committed()` with the forming bucket's partial imbalance appended
    /// as the newest sample (evicting the oldest when the window is full). Falls back to
    /// `committed()` when no bucket is forming; None only before ANY volume arrived.
    pub fn interim(&self) -> Option<f64> {
        let f = match self.forming_bucket() {
            Some(f) => f,
            None => return self.committed(),
        };
        let f_imb = f.imbalance();
        if self.ring.len() == self.window {
            let evict = *self.ring.front().expect("window >= 1");
            Some((self.sum - evict + f_imb) / self.window as f64)
        } else {
            Some((self.sum + f_imb) / (self.ring.len() + 1) as f64)
        }
    }

    /// Batch twin (aggressor-classified): the full completed-bucket imbalance series.
    pub fn bucket_imbalances(trades: &[TradeTick], bucket_volume: f64) -> Vec<f64> {
        assert!(bucket_volume > 0.0 && bucket_volume.is_finite());
        let mut out = Vec::new();
        let (mut buy, mut sell) = (0.0_f64, 0.0_f64);
        for t in trades.iter().filter(|t| t.size > 0.0) {
            let is_buy = classify(t) == Side::Buy;
            let mut remaining = t.size;
            while remaining > 0.0 {
                let take = remaining.min(bucket_volume - (buy + sell));
                if is_buy {
                    buy += take;
                } else {
                    sell += take;
                }
                remaining -= take;
                if buy + sell >= bucket_volume {
                    out.push((buy - sell).abs() / bucket_volume);
                    buy = 0.0;
                    sell = 0.0;
                }
            }
        }
        out // trailing partial bucket dropped
    }

    /// Batch VPIN over a slice (aggressor-classified): the imbalance series' last
    /// ≤ `window` entries, naive-fold mean — a fresh sum, NOT the streaming rolling sum.
    pub fn from_trades(trades: &[TradeTick], bucket_volume: f64, window: usize) -> Option<f64> {
        assert!(window >= 1);
        let imbs = Self::bucket_imbalances(trades, bucket_volume);
        if imbs.is_empty() {
            return None;
        }
        let n = imbs.len().min(window);
        let mut sum = 0.0;
        for &x in &imbs[imbs.len() - n..] {
            sum += x;
        }
        Some(sum / n as f64)
    }
}

// ---------------------------------------------------------------------------
// BVC (Bulk Volume Classification) VPIN — Easley/López de Prado/O'Hara (2012).
// ---------------------------------------------------------------------------

/// Standard-normal CDF `Φ(x) = ½·(1 + erf(x/√2))`. erf via `libm` (pure-Rust FDLIBM),
/// mirroring `vike-options`' `norm_cdf` — deterministic across platforms, no new dep.
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + libm::erf(x / std::f64::consts::SQRT_2))
}

/// Tick-rule buy fraction from a bucket price change: `ΔP > 0 → 1.0` (all buy),
/// `ΔP < 0 → 0.0` (all sell), `ΔP == 0 → 0.5` (balanced). Pure, side-defining, and the
/// limit BVC degenerates to when `σ_ΔP` is undefined.
pub fn tick_rule_buy_fraction(delta_p: f64) -> f64 {
    if delta_p > 0.0 {
        1.0
    } else if delta_p < 0.0 {
        0.0
    } else {
        0.5
    }
}

/// BVC buy fraction `Φ(ΔP / σ_ΔP) ∈ [0, 1]`. A non-positive / non-finite `σ_ΔP` (flat
/// window or warm-up) falls back to [`tick_rule_buy_fraction`] — never `Φ(x/0) → NaN`.
pub fn bvc_buy_fraction(delta_p: f64, sigma: f64) -> f64 {
    if sigma > 0.0 && sigma.is_finite() {
        norm_cdf(delta_p / sigma)
    } else {
        tick_rule_buy_fraction(delta_p)
    }
}

/// Which volume-classification scheme a [`BvcVpin`] applies to each bucket.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VolumeClassifier {
    /// Bulk Volume Classification: `buy_frac = Φ(ΔP / σ_ΔP)` (the paper's default). DEFAULT.
    #[default]
    Bvc,
    /// Tick rule on `sign(ΔP)`: up → all buy, down → all sell, flat → balanced.
    TickRule,
}

/// Population stdev of the ΔP window (naive fold, mean-subtracted). `< 2` samples → `0.0`
/// (undefined → the BVC tick-rule fallback fires). The fold order is the `VecDeque`'s
/// insertion order, shared verbatim by the streaming and batch paths ⇒ bit-identical σ.
fn dp_stdev(xs: &VecDeque<f64>) -> f64 {
    let n = xs.len();
    if n < 2 {
        return 0.0;
    }
    let nf = n as f64;
    let mean = xs.iter().copied().sum::<f64>() / nf;
    let var = xs
        .iter()
        .map(|&x| {
            let d = x - mean;
            d * d
        })
        .sum::<f64>()
        / nf;
    var.sqrt()
}

/// BVC-classified VPIN estimator: aggressor-free (price + volume only), streaming, pure.
///
/// Fold trades with [`BvcVpin::on_trade`] (internal VBS bucketing) or feed pre-formed
/// equal-volume buckets' close prices with [`BvcVpin::on_bucket`]. Read the toxicity
/// magnitude with [`BvcVpin::value`] (`None` until `window` bucket imbalances exist) and
/// the signed side-bias with [`BvcVpin::directional_bias`]. See the module doc for the model.
pub struct BvcVpin {
    bucket_volume: f64,
    window: usize,
    classifier: VolumeClassifier,
    /// volume accrued in the OPEN bucket (VBS bucketing for `on_trade`)
    filled: f64,
    /// previous CLOSED bucket's close price — the `P_{i−1}` in `ΔP = P_i − P_{i−1}`
    prev_close: Option<f64>,
    /// last ≤ `window` bucket ΔPs (front = oldest) — the σ window
    dp_window: VecDeque<f64>,
    /// last ≤ `window` SIGNED per-bucket imbalances `(2·buy_frac − 1) ∈ [−1, 1]`
    ring: VecDeque<f64>,
    /// O(1) rolling Σ|signed| (the VPIN numerator) and Σ signed (the side-bias numerator)
    abs_sum: f64,
    signed_sum: f64,
}

impl BvcVpin {
    /// BVC-classified, `window = DEFAULT_VPIN_WINDOW`.
    pub fn new(bucket_volume: f64) -> Self {
        Self::with_params(bucket_volume, DEFAULT_VPIN_WINDOW, VolumeClassifier::Bvc)
    }

    pub fn with_params(bucket_volume: f64, window: usize, classifier: VolumeClassifier) -> Self {
        assert!(
            bucket_volume > 0.0 && bucket_volume.is_finite(),
            "BvcVpin bucket_volume must be finite and > 0"
        );
        assert!(window >= 1, "BvcVpin window must be >= 1");
        BvcVpin {
            bucket_volume,
            window,
            classifier,
            filled: 0.0,
            prev_close: None,
            dp_window: VecDeque::with_capacity(window),
            ring: VecDeque::with_capacity(window),
            abs_sum: 0.0,
            signed_sum: 0.0,
        }
    }

    /// Fold one trade (price + size), VBS-bucketing internally. Returns the `|imbalance|` of
    /// every bucket this trade COMPLETED (empty for the very first bucket — it only sets the
    /// `P_{i−1}` reference; ≥ 2 when one big trade spans several buckets).
    pub fn on_trade(&mut self, price: f64, size: f64) -> Vec<f64> {
        let mut out = Vec::new();
        if size <= 0.0 {
            return out; // non-positive (and NaN, which fails `<= 0.0` but also `> 0.0` below)
        }
        let mut remaining = size;
        while remaining > 0.0 {
            let room = (self.bucket_volume - self.filled).max(0.0);
            let take = remaining.min(room);
            self.filled += take;
            remaining -= take;
            if self.filled >= self.bucket_volume {
                if let Some(imb) = self.on_bucket(price) {
                    out.push(imb);
                }
                self.filled = 0.0;
            }
        }
        out
    }

    /// [`on_trade`](Self::on_trade) convenience over a `vike_model::TradeTick` (price+size only;
    /// the aggressor flag is deliberately unused — BVC needs none).
    pub fn on_trade_tick(&mut self, t: &TradeTick) -> Vec<f64> {
        self.on_trade(t.price, t.size)
    }

    /// Close a bucket at `close` price directly (for callers who did their own equal-volume
    /// bucketing). Returns `Some(|imbalance|)`, or `None` for the FIRST bucket (which only
    /// records the reference close — a ΔP needs a prior close). Do not interleave with
    /// [`on_trade`](Self::on_trade)'s VBS accounting on the same instance.
    pub fn on_bucket(&mut self, close: f64) -> Option<f64> {
        let prev = match self.prev_close {
            Some(p) => p,
            None => {
                self.prev_close = Some(close);
                return None;
            }
        };
        let dp = close - prev;
        self.prev_close = Some(close);
        // Roll the σ window (evict oldest before pushing so σ spans the last `window` ΔPs,
        // INCLUDING this bucket).
        if self.dp_window.len() == self.window {
            self.dp_window.pop_front();
        }
        self.dp_window.push_back(dp);
        let buy_frac = match self.classifier {
            VolumeClassifier::Bvc => bvc_buy_fraction(dp, dp_stdev(&self.dp_window)),
            VolumeClassifier::TickRule => tick_rule_buy_fraction(dp),
        };
        let signed = 2.0 * buy_frac - 1.0; // (V_buy − V_sell) / VBS ∈ [−1, 1]
        if self.ring.len() == self.window {
            let old = self.ring.pop_front().expect("window >= 1");
            self.abs_sum -= old.abs();
            self.signed_sum -= old;
        }
        self.ring.push_back(signed);
        self.abs_sum += signed.abs();
        self.signed_sum += signed;
        Some(signed.abs())
    }

    /// Number of completed bucket imbalances currently in the window (`0..=window`).
    pub fn completed_buckets(&self) -> usize {
        self.ring.len()
    }

    /// VPIN — the side-less toxicity magnitude in `[0, 1]`, the rolling mean of the last
    /// `window` bucket imbalances. `None` until `window` imbalances exist (see the module doc).
    pub fn value(&self) -> Option<f64> {
        if self.ring.len() >= self.window { Some(self.abs_sum / self.window as f64) } else { None }
    }

    /// Signed side-bias in `[−1, 1]` — the rolling mean of the SIGNED per-bucket imbalances
    /// `(2·buy_frac − 1)`. `> 0` ⇒ buy-toxic (ask side), `< 0` ⇒ sell-toxic (bid side). Same
    /// readiness gate as [`value`](Self::value). Lets a caller route VPIN to a per-side gate.
    pub fn directional_bias(&self) -> Option<f64> {
        if self.ring.len() >= self.window {
            Some(self.signed_sum / self.window as f64)
        } else {
            None
        }
    }

    /// Batch twin: the full per-bucket `|imbalance|` series from a slice of bucket CLOSE
    /// prices — a genuinely separate slice computation (NOT `on_bucket` in disguise), so
    /// streaming↔batch parity is a real test. The first close is the reference (no imbalance).
    pub fn imbalances_from_closes(
        closes: &[f64],
        window: usize,
        classifier: VolumeClassifier,
    ) -> Vec<f64> {
        assert!(window >= 1);
        let mut out = Vec::new();
        let mut dp_window: VecDeque<f64> = VecDeque::with_capacity(window);
        let mut prev: Option<f64> = None;
        for &close in closes {
            let p = match prev {
                Some(p) => p,
                None => {
                    prev = Some(close);
                    continue;
                }
            };
            let dp = close - p;
            prev = Some(close);
            if dp_window.len() == window {
                dp_window.pop_front();
            }
            dp_window.push_back(dp);
            let buy_frac = match classifier {
                VolumeClassifier::Bvc => bvc_buy_fraction(dp, dp_stdev(&dp_window)),
                VolumeClassifier::TickRule => tick_rule_buy_fraction(dp),
            };
            out.push((2.0 * buy_frac - 1.0).abs());
        }
        out
    }

    /// Batch VPIN over a slice of bucket close prices: the imbalance series' last ≤ `window`
    /// entries, naive-fold mean (a fresh sum, NOT the streaming rolling sum). `None` until
    /// `window` imbalances exist — the same readiness gate as the streaming [`value`](Self::value).
    pub fn from_bucket_closes(
        closes: &[f64],
        window: usize,
        classifier: VolumeClassifier,
    ) -> Option<f64> {
        assert!(window >= 1);
        let imbs = Self::imbalances_from_closes(closes, window, classifier);
        if imbs.len() < window {
            return None;
        }
        let mut sum = 0.0;
        for &x in &imbs[imbs.len() - window..] {
            sum += x;
        }
        Some(sum / window as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tk(price: f64, size: f64, ibm: bool) -> TradeTick {
        TradeTick { ts: 0, local_ts: 0, price, size, is_buyer_maker: ibm, symbol: String::new() }
    }

    // V=4: bucket1 = buy 2 + buy 2 → |4−0|/4 = 1.0; bucket2 = sell 1 + buy 1 + sell 2
    // → |1−3|/4 = 0.5. VPIN = (1.0 + 0.5)/2 = 0.75.
    #[test]
    fn vpin_expected_values() {
        let mut v = Vpin::with_params(4.0, 50, true);
        assert_eq!(v.committed(), None);
        assert_eq!(v.interim(), None);
        assert_eq!(v.push(&tk(1.0, 2.0, false), None), Vec::<f64>::new());
        assert_eq!(v.push(&tk(1.0, 2.0, false), None), vec![1.0]);
        assert_eq!(v.committed(), Some(1.0));
        v.push(&tk(1.0, 1.0, true), None);
        v.push(&tk(1.0, 1.0, false), None);
        assert_eq!(v.push(&tk(1.0, 2.0, true), None), vec![0.5]);
        assert_eq!(v.committed(), Some(0.75));
        assert_eq!(v.completed_buckets(), 2);
        assert_eq!(v.forming_bucket(), None);
    }

    // One buy of 10 into V=4 buckets: two completed (imb 1.0 each) + forming buy=2.
    // Interim folds the partial bucket (|2−0|/2 = 1.0) in: (1+1+1)/3 = 1.0; then a sell 2
    // completes it at |2−2|/4 = 0 → committed (1+1+0)/3 = 2/3.
    #[test]
    fn vpin_overflow_spills_and_interim() {
        let mut v = Vpin::with_params(4.0, 50, true);
        assert_eq!(v.push(&tk(1.0, 10.0, false), None), vec![1.0, 1.0]);
        let f = v.forming_bucket().unwrap();
        assert_eq!((f.buy_vol, f.sell_vol, f.filled()), (2.0, 0.0, 2.0));
        assert_eq!(v.interim(), Some(1.0));
        assert_eq!(v.committed(), Some(1.0)); // forming bucket not committed
        assert_eq!(v.push(&tk(1.0, 2.0, true), None), vec![0.0]);
        assert_eq!(v.committed(), Some(2.0 / 3.0));
        assert_eq!(v.interim(), Some(2.0 / 3.0)); // nothing forming → interim == committed
    }

    // window=2 evicts: buckets score 1.0, 0.0, 1.0 → after the 3rd, VPIN = (0+1)/2 = 0.5.
    // Dyadic values → the O(1) rolling sum is exact.
    #[test]
    fn vpin_window_eviction_rolling_sum() {
        let mut v = Vpin::with_params(2.0, 2, true);
        v.push(&tk(1.0, 2.0, false), None); // bucket1: 1.0
        v.push(&tk(1.0, 1.0, false), None);
        v.push(&tk(1.0, 1.0, true), None); // bucket2: 0.0
        assert_eq!(v.committed(), Some(0.5));
        v.push(&tk(1.0, 2.0, true), None); // bucket3: 1.0 → evicts bucket1
        assert_eq!(v.committed(), Some(0.5));
        assert_eq!(v.completed_buckets(), 2);
        // interim with a FULL window evicts the oldest committed sample for the calc:
        v.push(&tk(1.0, 1.0, false), None); // forming: buy 1 → partial imb 1.0
        // window holds [0.0, 1.0]; interim = (1.0 + 1.0)/2 = 1.0 (0.0 evicted)
        assert_eq!(v.interim(), Some(1.0));
        assert_eq!(v.committed(), Some(0.5)); // untouched
    }

    // has_aggressor=false + mid: price>=mid → Buy, else Sell — the aggressor flags are set
    // to CONTRADICT the mid test, proving mid wins; mid=None falls back to the flag.
    #[test]
    fn vpin_mid_fallback_classification() {
        let mut v = Vpin::with_params(2.0, 50, false);
        // both trades would classify Sell by flag; mid says 101→Buy, 99→Sell
        v.push(&tk(101.0, 1.0, true), Some(100.0));
        assert_eq!(v.push(&tk(99.0, 1.0, true), Some(100.0)), vec![0.0]); // buy1 sell1
        // mid=None → aggressor flag (false → Buy): two buys → imb 1.0
        v.push(&tk(99.0, 1.0, false), None);
        assert_eq!(v.push(&tk(99.0, 1.0, false), None), vec![1.0]);
    }

    #[test]
    fn vpin_zero_size_skipped() {
        let mut v = Vpin::new(1.0);
        assert!(v.push(&tk(1.0, 0.0, false), None).is_empty());
        assert_eq!(v.forming_bucket(), None);
        assert_eq!(v.interim(), None);
    }

    // Streaming (rolling sum) vs batch (naive-fold tail mean): exact-dyadic sizes so the
    // eviction arithmetic is exact → bit parity holds even past the window.
    #[test]
    fn vpin_streaming_equals_batch_dyadic() {
        let trades = vec![
            tk(1.0, 2.0, false),
            tk(1.0, 1.5, true),
            tk(1.0, 0.5, false),
            tk(1.0, 4.0, true),
            tk(1.0, 1.0, false),
            tk(1.0, 3.0, true),
        ];
        let window = 3;
        let batch = Vpin::from_trades(&trades, 2.0, window).unwrap();
        let mut v = Vpin::with_params(2.0, window, true);
        let mut imbs = Vec::new();
        for t in &trades {
            imbs.extend(v.push(t, None));
        }
        assert_eq!(imbs, Vpin::bucket_imbalances(&trades, 2.0));
        assert!(imbs.len() > window); // eviction actually exercised
        assert_eq!(v.committed().unwrap().to_bits(), batch.to_bits());
    }

    // Non-dyadic sizes: per-bucket imbalances still bit-identical (same fold order); the
    // rolling-sum mean is allowed to differ from the fresh fold only at rounding noise.
    #[test]
    fn vpin_streaming_matches_batch_nondyadic() {
        let mut trades = Vec::new();
        for i in 0..40 {
            let size = 0.1 + (i as f64) * 0.07;
            trades.push(tk(1.0, size, i % 3 == 0));
        }
        let window = 5;
        let batch = Vpin::from_trades(&trades, 1.3, window).unwrap();
        let mut v = Vpin::with_params(1.3, window, true);
        let mut imbs = Vec::new();
        for t in &trades {
            imbs.extend(v.push(t, None));
        }
        assert_eq!(imbs, Vpin::bucket_imbalances(&trades, 1.3));
        let s = v.committed().unwrap();
        assert!((s - batch).abs() <= 1e-12, "streaming {s} vs batch {batch}");
    }
}

#[cfg(test)]
mod bvc_tests {
    use super::*;

    // Φ(0) = ½ exactly (erf(0) = 0); Φ is symmetric: Φ(x) + Φ(−x) = 1; Φ(1) ≈ 0.8413447.
    #[test]
    fn norm_cdf_pins() {
        assert_eq!(norm_cdf(0.0).to_bits(), 0.5_f64.to_bits());
        assert!((norm_cdf(1.0) - 0.8413447460685429).abs() < 1e-12);
        assert!((norm_cdf(2.0) + norm_cdf(-2.0) - 1.0).abs() < 1e-15);
        assert_eq!(norm_cdf(40.0), 1.0); // saturation, no NaN
        assert_eq!(norm_cdf(-40.0), 0.0);
    }

    // Tick rule: up → all buy (1.0), down → all sell (0.0), flat → balanced (0.5).
    #[test]
    fn tick_rule_pins() {
        assert_eq!(tick_rule_buy_fraction(0.5), 1.0);
        assert_eq!(tick_rule_buy_fraction(-0.5), 0.0);
        assert_eq!(tick_rule_buy_fraction(0.0), 0.5);
    }

    // BVC: Φ(ΔP/σ); σ ≤ 0 (or non-finite) falls back to the tick rule — never Φ(x/0)=NaN.
    #[test]
    fn bvc_buy_fraction_and_sigma_zero_fallback() {
        assert_eq!(bvc_buy_fraction(0.0, 1.0).to_bits(), 0.5_f64.to_bits()); // Φ(0)
        assert!((bvc_buy_fraction(1.0, 1.0) - 0.8413447460685429).abs() < 1e-12);
        // σ = 0 → tick-rule fallback (no NaN)
        assert_eq!(bvc_buy_fraction(0.5, 0.0), 1.0);
        assert_eq!(bvc_buy_fraction(-0.5, 0.0), 0.0);
        assert_eq!(bvc_buy_fraction(0.0, 0.0), 0.5);
        assert_eq!(bvc_buy_fraction(1.0, f64::NAN), 1.0);
        assert!(!bvc_buy_fraction(0.0, 0.0).is_nan());
    }

    // A hand-computed BVC bucket-imbalance series. closes = [100, 101, 100]:
    //   • close 100 → reference (no ΔP, no sample)
    //   • close 101 → ΔP=+1, σ-window=[1] (n=1<2) ⇒ σ=0 ⇒ tick-rule(+1)=1.0 ⇒ |2·1−1| = 1.0
    //   • close 100 → ΔP=−1, σ-window=[1,−1]: mean 0, var (1+1)/2 = 1, σ = 1; z = −1;
    //     buy_frac = Φ(−1); imbalance = |2·Φ(−1) − 1| = erf(1/√2) = 0.6826894921370859.
    #[test]
    fn bvc_imbalance_series_hand_computed() {
        let imbs =
            BvcVpin::imbalances_from_closes(&[100.0, 101.0, 100.0], 3, VolumeClassifier::Bvc);
        assert_eq!(imbs.len(), 2);
        assert_eq!(imbs[0], 1.0); // σ=0 warm-up fallback, one-way ⇒ 1.0
        assert!((imbs[1] - 0.6826894921370859).abs() < 1e-12);
    }

    // Limiting case — a monotone one-way tape: every ΔP equal ⇒ σ=0 every bucket ⇒ tick-rule
    // all-buy ⇒ each imbalance 1.0 ⇒ VPIN = 1.0 (maximally toxic). directional_bias = +1.0 (ask).
    #[test]
    fn bvc_all_buy_is_one() {
        let closes = [100.0, 101.0, 102.0, 103.0];
        let mut v = BvcVpin::with_params(1.0, 3, VolumeClassifier::Bvc);
        for &c in &closes {
            v.on_bucket(c);
        }
        assert_eq!(v.completed_buckets(), 3);
        assert_eq!(v.value(), Some(1.0));
        assert_eq!(v.directional_bias(), Some(1.0));
        assert_eq!(BvcVpin::from_bucket_closes(&closes, 3, VolumeClassifier::Bvc), Some(1.0));
    }

    // Limiting case — a flat tape: every ΔP = 0 ⇒ tick-rule balanced (0.5) ⇒ each imbalance 0.0
    // ⇒ VPIN = 0.0 (no toxicity), directional_bias = 0.0. Also proves the Φ(0/0) NaN guard.
    #[test]
    fn bvc_flat_is_zero_no_nan() {
        let closes = [50.0, 50.0, 50.0, 50.0];
        let mut v = BvcVpin::with_params(1.0, 3, VolumeClassifier::Bvc);
        for &c in &closes {
            v.on_bucket(c);
        }
        assert_eq!(v.value(), Some(0.0));
        assert_eq!(v.directional_bias(), Some(0.0));
        assert!(!v.value().unwrap().is_nan());
    }

    // Ready/not-ready boundary: window N=2 needs N imbalances = N+1 = 3 bucket closes.
    #[test]
    fn bvc_ready_boundary() {
        let mut v = BvcVpin::with_params(1.0, 2, VolumeClassifier::Bvc);
        assert_eq!(v.on_bucket(100.0), None); // reference only
        assert_eq!(v.value(), None);
        assert_eq!(v.on_bucket(101.0), Some(1.0)); // imbalance #1 (σ=0 warm-up)
        assert_eq!(v.completed_buckets(), 1);
        assert_eq!(v.value(), None); // 1 < window 2 → not ready
        let imb2 = v.on_bucket(100.0).unwrap(); // imbalance #2
        assert!((imb2 - 0.6826894921370859).abs() < 1e-12);
        assert_eq!(v.value().unwrap(), (1.0 + imb2) / 2.0);
        // side-bias: signed = [+1.0, −0.6826894921370859] ⇒ mean = +0.15865525393145707
        assert!((v.directional_bias().unwrap() - 0.15865525393145707).abs() < 1e-12);
    }

    // The directional side-bias sign flips with net flow: a mostly-DOWN tape ⇒ sell-toxic (< 0).
    #[test]
    fn bvc_directional_bias_sign() {
        let closes = [100.0, 99.0, 98.0, 99.0]; // net down
        let mut v = BvcVpin::with_params(1.0, 3, VolumeClassifier::Bvc);
        for &c in &closes {
            v.on_bucket(c);
        }
        assert!(v.directional_bias().unwrap() < 0.0, "net-down ⇒ sell-toxic (bid side)");
        assert!(v.value().unwrap() > 0.0);
    }

    // on_trade's VBS bucketing (with spill) reduces to the on_bucket close series and is
    // bit-identical to feeding the closes directly.
    #[test]
    fn bvc_on_trade_equals_on_bucket() {
        // VBS=4: three exact buckets closing at 100, 101, 100 → 2 imbalances (window 2 ⇒ ready).
        let mut a = BvcVpin::with_params(4.0, 2, VolumeClassifier::Bvc);
        assert!(a.on_trade(100.0, 4.0).is_empty()); // first bucket = reference
        assert_eq!(a.on_trade(101.0, 4.0), vec![1.0]);
        let last = a.on_trade(100.0, 4.0);
        assert_eq!(last.len(), 1);

        let mut b = BvcVpin::with_params(4.0, 2, VolumeClassifier::Bvc);
        for &c in &[100.0, 101.0, 100.0] {
            b.on_bucket(c);
        }
        assert_eq!(a.value().unwrap().to_bits(), b.value().unwrap().to_bits());
        assert_eq!(
            a.directional_bias().unwrap().to_bits(),
            b.directional_bias().unwrap().to_bits()
        );
    }

    // A single big trade spills across several VBS buckets (all closing at the same price).
    #[test]
    fn bvc_on_trade_spills() {
        let mut v = BvcVpin::with_params(4.0, 50, VolumeClassifier::Bvc);
        v.on_trade(50.0, 4.0); // reference bucket
        let closed = v.on_trade(51.0, 12.0); // 12 / 4 = 3 buckets, all close @ 51
        assert_eq!(closed.len(), 3);
        assert_eq!(v.completed_buckets(), 3);
    }

    #[test]
    fn bvc_zero_size_skipped() {
        let mut v = BvcVpin::new(1.0);
        assert!(v.on_trade(100.0, 0.0).is_empty());
        assert!(v.on_trade(100.0, -5.0).is_empty());
        assert_eq!(v.completed_buckets(), 0);
        assert_eq!(v.value(), None);
    }

    // Streaming↔batch: the per-bucket imbalance SERIES is bit-identical (shared σ fold order),
    // even past window eviction; the rolling-sum VPIN matches the fresh-fold mean to rounding.
    #[test]
    fn bvc_streaming_equals_batch() {
        // A wandering close series that exercises + and − ΔP and the σ window.
        let closes: Vec<f64> =
            (0..40).map(|i| 100.0 + ((i * 7) % 11) as f64 - ((i * 3) % 5) as f64 * 0.5).collect();
        let window = 5;
        let batch_series = BvcVpin::imbalances_from_closes(&closes, window, VolumeClassifier::Bvc);
        let mut v = BvcVpin::with_params(1.0, window, VolumeClassifier::Bvc);
        let mut stream_series = Vec::new();
        for &c in &closes {
            if let Some(imb) = v.on_bucket(c) {
                stream_series.push(imb);
            }
        }
        assert!(stream_series.len() > window); // eviction actually exercised
        for (a, b) in stream_series.iter().zip(batch_series.iter()) {
            assert_eq!(a.to_bits(), b.to_bits()); // bit-identical imbalance series
        }
        let batch = BvcVpin::from_bucket_closes(&closes, window, VolumeClassifier::Bvc).unwrap();
        let s = v.value().unwrap();
        assert!((s - batch).abs() <= 1e-12, "streaming {s} vs batch {batch}");
    }

    // The TickRule classifier == BVC with σ forced to 0 (both route through tick_rule_buy_fraction).
    #[test]
    fn bvc_tickrule_classifier() {
        let closes = [10.0, 11.0, 10.5, 9.0, 9.0];
        let imbs = BvcVpin::imbalances_from_closes(&closes, 3, VolumeClassifier::TickRule);
        // ΔP = +1, −0.5, −1.5, 0 ⇒ tick-rule imbalances 1.0, 1.0, 1.0, 0.0
        assert_eq!(imbs, vec![1.0, 1.0, 1.0, 0.0]);
    }
}
