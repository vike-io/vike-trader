//! Pure per-side flow-toxicity aggregator (Wave 5c, producer side) — the PRODUCER of the
//! [`vike_model::FlowToxicity`] reading a mounted market maker consumes on its `on_flow` hook.
//!
//! This is the pure core that sits between the wallet-classified [`crate::rtds::ActivityTrade`] tape
//! ([`crate::rtds::ActivityTradeSink`]) and the maker's toxicity guard: it folds each classified
//! trade into a per-side, time-decayed estimate of what FRACTION of the recent flow on that side is
//! TOXIC (edge-carrying / size-moving), and reports it as a [`vike_model::FlowToxicity`] in `[0, 1]`.
//! No I/O, no `TickSender`, no strategy — depends only on `vike_model` + this crate's own
//! [`crate::rtds::TradeSide`] / [`crate::wallet_class::WalletClass`]. The emit side (an
//! `ActivityTradeSink` that drives one of these and pushes the reading onto the core's flow lane)
//! lives in `vike-run`, so the layering stays down-only.
//!
//! ## Side mapping (authoritative — [`vike_model::FlowToxicity`]'s own contract)
//! A toxic BUY taker LIFTS our resting ASK, so a `Buy` trade feeds the ASK accumulator
//! ([`FlowToxicity::ask`]); a toxic SELL taker HITS our resting BID, so a `Sell` trade feeds the BID
//! accumulator ([`FlowToxicity::bid`]).
//!
//! ## The decay/normalization model (documented, monotone, bounded)
//! Each side keeps two exponentially time-decayed accumulators — TOXIC size and TOTAL size — plus the
//! timestamp of the last trade folded into it. The decay is an exponential with time-constant
//! `window_ms`: on each `observe`, the existing accumulators are first multiplied by
//! `exp(-Δt / window_ms)` (Δt = ms since that side's last trade) before the new size is added, so both
//! accumulators are recency-weighted EWMA sums — BOUNDED for any bounded trade rate, and MONOTONE in
//! the sense that a pure time advance never raises them.
//!
//! The reading on a side is the toxic FRACTION weighted by a RECENCY freshness factor:
//! `reading = clamp( (toxic / total) · exp(-Δt_now / window_ms), 0, 1 )`, where `Δt_now` is the ms
//! from that side's last trade to `now_ms`. Two decay mechanisms, each doing a distinct job:
//! - decay-at-`observe` makes `toxic / total` reflect the RECENT toxic share, so a regime change from
//!   toxic to benign flow lowers the reading as benign volume accumulates (a plain lifetime fraction
//!   would stay stuck on history);
//! - the freshness factor fades a reading to `0` once flow STOPS ENTIRELY (no new trades keep `now`
//!   pulling away from `last_ts`), which the fraction alone — decay-invariant when both accumulators
//!   scale together — could never do.
//!
//! `toxic ≤ total` by construction (toxic size is a subset of total size), so the fraction is already
//! in `[0, 1]`; the freshness factor is in `(0, 1]`; the `clamp` is belt-and-suspenders. A side whose
//! `total` has never been fed (or has decayed to ~0) reads exactly `0.0`.

use vike_model::FlowToxicity;

use crate::rtds::TradeSide;
use crate::wallet_class::WalletClass;

/// A side's `total` at or below this (decayed EWMA size) reads as "no flow" ⇒ `0.0`, avoiding a
/// `0 / 0` on an unfed or fully-decayed side.
const TOTAL_EPS: f64 = 1e-9;

/// One side's pair of time-decayed accumulators + the last-trade stamp the decay keys off.
#[derive(Debug, Clone, Copy)]
struct SideAccum {
    /// EWMA-decayed sum of TOXIC trade sizes, as of `last_ts`.
    toxic: f64,
    /// EWMA-decayed sum of ALL trade sizes, as of `last_ts` (`toxic ≤ total` always).
    total: f64,
    /// Epoch-ms of the most recent trade folded into this side (`0` = never fed).
    last_ts: i64,
}

impl SideAccum {
    /// A never-fed side (both accumulators zero, no last trade) ⇒ reads `0.0`.
    fn new() -> Self {
        SideAccum { toxic: 0.0, total: 0.0, last_ts: 0 }
    }

    /// Decay both accumulators forward to `ts` (no-op on the first-ever fold or an out-of-order
    /// older trade — the decay only runs strictly forward, so a backwards stamp never re-inflates).
    fn decay_to(&mut self, ts: i64, window_ms: i64) {
        if self.last_ts != 0 && ts > self.last_ts {
            let f = libm::exp(-((ts - self.last_ts) as f64) / window_ms as f64);
            self.toxic *= f;
            self.total *= f;
        }
    }

    /// This side's reading at `now_ms`: the recency-decayed toxic fraction (see the module doc), or
    /// `0.0` when the side carries no meaningful flow.
    fn reading(&self, now_ms: i64, window_ms: i64) -> f64 {
        if self.total <= TOTAL_EPS {
            return 0.0;
        }
        let fraction = self.toxic / self.total;
        let freshness = if now_ms > self.last_ts {
            libm::exp(-((now_ms - self.last_ts) as f64) / window_ms as f64)
        } else {
            1.0
        };
        (fraction * freshness).clamp(0.0, 1.0)
    }
}

/// The pure per-side flow-toxicity aggregator. Fold classified trades in with [`Self::observe`]; read
/// the current per-side toxicity out with [`Self::current`]. Holds a decay time-constant, the set of
/// [`WalletClass`]es counted as toxic (default `Sharp` + `Whale`), and the two [`SideAccum`]s. No I/O.
#[derive(Debug, Clone)]
pub struct ToxicityAggregator {
    /// Exponential decay time-constant (ms) — larger ⇒ a longer memory / slower fade.
    window_ms: i64,
    /// Which [`WalletClass`]es count as TOXIC; every other class contributes only to `total`.
    toxic_classes: Vec<WalletClass>,
    /// BID-side accumulator — fed by toxic SELL takers ([`FlowToxicity::bid`]).
    bid: SideAccum,
    /// ASK-side accumulator — fed by toxic BUY takers ([`FlowToxicity::ask`]).
    ask: SideAccum,
}

impl ToxicityAggregator {
    /// A new aggregator with the default toxic set — [`WalletClass::Sharp`] and [`WalletClass::Whale`]
    /// — and the given decay time-constant. `window_ms` is floored to `1` so the decay divisor is
    /// never zero (a non-positive window is a caller error; flooring keeps the pure core total).
    pub fn new(window_ms: i64) -> Self {
        Self::with_toxic_classes(window_ms, vec![WalletClass::Sharp, WalletClass::Whale])
    }

    /// A new aggregator with an EXPLICIT toxic-class set (the test/tuning seam) — e.g. only
    /// [`WalletClass::Whale`], or an empty set (⇒ nothing is ever toxic ⇒ every reading stays `0.0`).
    /// `window_ms` is floored to `1` exactly as in [`Self::new`].
    pub fn with_toxic_classes(window_ms: i64, toxic_classes: Vec<WalletClass>) -> Self {
        ToxicityAggregator {
            window_ms: window_ms.max(1),
            toxic_classes,
            bid: SideAccum::new(),
            ask: SideAccum::new(),
        }
    }

    /// Fold ONE classified trade. `side` is the TAKER's side ([`TradeSide::Buy`] ⇒ ASK accumulator,
    /// [`TradeSide::Sell`] ⇒ BID accumulator — the side the taker crosses INTO our resting quote),
    /// `class` its wallet's [`WalletClass`], `size` the trade size (shares), `ts` its epoch-ms stamp.
    /// A non-finite or non-positive `size` is ignored (never negates an accumulator). The chosen side
    /// is decayed forward to `ts`, then `size` is added to its `total` and — if `class` is toxic — to
    /// its `toxic`; `last_ts` advances (never backwards).
    pub fn observe(&mut self, side: TradeSide, class: WalletClass, size: f64, ts: i64) {
        if !size.is_finite() || size <= 0.0 {
            return;
        }
        let toxic = self.toxic_classes.contains(&class);
        let window_ms = self.window_ms;
        let acc = match side {
            // A toxic BUY taker lifts our resting ASK; a toxic SELL taker hits our resting BID.
            TradeSide::Buy => &mut self.ask,
            TradeSide::Sell => &mut self.bid,
        };
        acc.decay_to(ts, window_ms);
        acc.total += size;
        if toxic {
            acc.toxic += size;
        }
        acc.last_ts = ts.max(acc.last_ts);
    }

    /// The current per-side toxicity reading at `now_ms`, stamped `ts = now_ms` — the value emitted to
    /// the maker's `on_flow` hook. Each side is the recency-decayed toxic fraction in `[0, 1]` (see the
    /// module doc); a side with no (or fully decayed) flow reads `0.0`. Pure `&self` read — never
    /// mutates the accumulators, so repeated `current` calls at the same `now_ms` are identical.
    pub fn current(&self, now_ms: i64) -> FlowToxicity {
        FlowToxicity {
            bid: self.bid.reading(now_ms, self.window_ms),
            ask: self.ask.reading(now_ms, self.window_ms),
            ts: now_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 30 s decay window — the bin's `--tox-window-secs` default, so the tests exercise realistic
    // constants.
    const WINDOW_MS: i64 = 30_000;

    /// An all-RETAIL tape carries no toxic size, so BOTH sides read EXACTLY `0.0` (bit-exact — this is
    /// the one place an `== 0.0` is legitimate, asserted via `to_bits`).
    #[test]
    fn all_retail_tape_reads_zero() {
        let mut agg = ToxicityAggregator::new(WINDOW_MS);
        for i in 0..10 {
            let ts = 1_000 + i * 100;
            agg.observe(TradeSide::Buy, WalletClass::Retail, 50.0, ts);
            agg.observe(TradeSide::Sell, WalletClass::Retail, 50.0, ts);
        }
        let flow = agg.current(2_000);
        assert_eq!(flow.bid.to_bits(), 0.0_f64.to_bits(), "no toxic sell flow ⇒ bid exactly 0");
        assert_eq!(flow.ask.to_bits(), 0.0_f64.to_bits(), "no toxic buy flow ⇒ ask exactly 0");
        assert_eq!(flow.ts, 2_000, "ts is stamped from now_ms");
    }

    /// A burst of Sharp BUYs drives the ASK reading high (all buy flow is toxic ⇒ fraction ~1) and
    /// leaves the BID at exactly `0.0` (no sell flow at all).
    #[test]
    fn sharp_buy_burst_lifts_ask_not_bid() {
        let mut agg = ToxicityAggregator::new(WINDOW_MS);
        for i in 0..8 {
            agg.observe(TradeSide::Buy, WalletClass::Sharp, 100.0, 1_000 + i * 10);
        }
        let flow = agg.current(1_070); // read at the last trade's stamp ⇒ freshness ~1
        assert!(
            flow.ask > 0.9,
            "an all-Sharp buy burst is near-fully toxic on the ask: {}",
            flow.ask
        );
        assert!(flow.ask <= 1.0, "reading is clamped into [0, 1]: {}", flow.ask);
        assert_eq!(flow.bid.to_bits(), 0.0_f64.to_bits(), "no sell flow ⇒ bid exactly 0");
    }

    /// A stale reading FADES: after a Sharp buy burst, reading the SAME accumulator further and
    /// further past the last trade yields a strictly DECREASING ask, approaching `0`.
    #[test]
    fn a_stale_reading_decays_toward_zero() {
        let mut agg = ToxicityAggregator::new(WINDOW_MS);
        agg.observe(TradeSide::Buy, WalletClass::Sharp, 500.0, 1_000);

        let fresh = agg.current(1_000).ask; // Δt = 0 ⇒ freshness 1
        let one_window = agg.current(1_000 + WINDOW_MS).ask; // Δt = τ ⇒ e^-1
        let five_windows = agg.current(1_000 + 5 * WINDOW_MS).ask; // Δt = 5τ ⇒ e^-5

        assert!(fresh > 0.9, "fresh all-Sharp ask is high: {fresh}");
        assert!(
            one_window < fresh,
            "one window later the reading has decayed: {one_window} < {fresh}"
        );
        assert!(five_windows < one_window, "five windows later even lower: {five_windows}");
        assert!(five_windows < 0.05, "a long-stale reading is ~0: {five_windows}");
    }

    /// Decay-at-observe makes the fraction track RECENT flow: after a toxic burst, a run of benign
    /// (Retail) volume pulls the same side's reading DOWN even though the toxic trades are still "in"
    /// the lifetime history.
    #[test]
    fn a_regime_change_to_benign_flow_lowers_the_reading() {
        let mut agg = ToxicityAggregator::new(WINDOW_MS);
        agg.observe(TradeSide::Buy, WalletClass::Sharp, 500.0, 1_000);
        let toxic_peak = agg.current(1_000).ask;
        // A long run of retail buys, each a window apart, decays the toxic share away.
        for i in 1..=6 {
            agg.observe(TradeSide::Buy, WalletClass::Retail, 500.0, 1_000 + i * WINDOW_MS);
        }
        let after = agg.current(1_000 + 6 * WINDOW_MS).ask;
        assert!(
            after < toxic_peak,
            "benign volume lowers the toxic fraction: {after} < {toxic_peak}"
        );
    }

    /// A partly-toxic mix (half Sharp, half Retail, same size) reads around `0.5`, and every reading
    /// stays within `[0, 1]` (the clamp/bound contract).
    #[test]
    fn a_mixed_tape_reads_a_bounded_partial_fraction() {
        let mut agg = ToxicityAggregator::new(WINDOW_MS);
        // Interleave equal-size Sharp and Retail buys at the same stamp ⇒ toxic fraction ~1/2.
        for _ in 0..20 {
            agg.observe(TradeSide::Buy, WalletClass::Sharp, 25.0, 5_000);
            agg.observe(TradeSide::Buy, WalletClass::Retail, 25.0, 5_000);
        }
        let flow = agg.current(5_000);
        assert!(flow.ask > 0.4 && flow.ask < 0.6, "half-toxic ask ≈ 0.5: {}", flow.ask);
        assert!((0.0..=1.0).contains(&flow.ask), "reading bounded to [0, 1]: {}", flow.ask);
        assert_eq!(flow.bid.to_bits(), 0.0_f64.to_bits(), "no sell flow ⇒ bid 0");
    }

    /// The toxic set is configurable: with only `Whale` toxic, a burst of Sharp trades is NOT toxic,
    /// so the reading stays `0.0`; a Whale burst on the other side lights that side up.
    #[test]
    fn with_toxic_classes_restricts_what_counts_as_toxic() {
        let mut agg = ToxicityAggregator::with_toxic_classes(WINDOW_MS, vec![WalletClass::Whale]);
        for i in 0..5 {
            agg.observe(TradeSide::Buy, WalletClass::Sharp, 100.0, 6_000 + i * 10); // NOT toxic here
            agg.observe(TradeSide::Sell, WalletClass::Whale, 100.0, 6_000 + i * 10);
            // toxic
        }
        let flow = agg.current(6_050);
        assert_eq!(flow.ask.to_bits(), 0.0_f64.to_bits(), "Sharp is not in the toxic set ⇒ ask 0");
        assert!(flow.bid > 0.9, "Whale sell flow is toxic on the bid: {}", flow.bid);
    }

    /// An empty toxic set ⇒ nothing is ever toxic ⇒ every reading is `0.0`, even under heavy flow.
    #[test]
    fn an_empty_toxic_set_never_reads_toxic() {
        let mut agg = ToxicityAggregator::with_toxic_classes(WINDOW_MS, Vec::new());
        agg.observe(TradeSide::Buy, WalletClass::Sharp, 100.0, 7_000);
        agg.observe(TradeSide::Sell, WalletClass::Whale, 100.0, 7_000);
        let flow = agg.current(7_000);
        assert_eq!(flow.ask.to_bits(), 0.0_f64.to_bits(), "empty toxic set ⇒ ask 0");
        assert_eq!(flow.bid.to_bits(), 0.0_f64.to_bits(), "empty toxic set ⇒ bid 0");
    }

    /// Non-finite and non-positive sizes are ignored — they never negate or NaN-poison an accumulator.
    #[test]
    fn non_finite_and_non_positive_sizes_are_ignored() {
        let mut agg = ToxicityAggregator::new(WINDOW_MS);
        agg.observe(TradeSide::Buy, WalletClass::Sharp, f64::NAN, 8_000);
        agg.observe(TradeSide::Buy, WalletClass::Sharp, -50.0, 8_000);
        agg.observe(TradeSide::Buy, WalletClass::Sharp, 0.0, 8_000);
        let flow = agg.current(8_000);
        assert_eq!(flow.ask.to_bits(), 0.0_f64.to_bits(), "no usable size folded ⇒ ask 0");
    }
}
