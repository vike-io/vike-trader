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
use vike_marketdata::TradeTick;

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

    /// [`on_trade`](Self::on_trade) convenience over a `vike_marketdata::TradeTick` (price+size only;
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

#[path = "vpin_tests.rs"]
#[cfg(test)]
mod vpin_tests;

#[path = "bvc_tests.rs"]
#[cfg(test)]
mod bvc_tests;
