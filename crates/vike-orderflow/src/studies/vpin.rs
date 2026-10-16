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
use std::collections::VecDeque;

use vike_marketdata::TradeTick;
use vike_marketdata::aggressor::{Side, classify};

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

#[path = "vpin_tests.rs"]
#[cfg(test)]
mod vpin_tests;
