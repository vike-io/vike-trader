//! Liquidity-rewards-aware quoting (steal/mm-rewards-quoting) — the PURE reward MODEL plus the
//! [`SpreadMaker`](crate::SpreadMaker)'s reward-EV price/size fold. Isolated here (like
//! `skew`/`book`/`refresh`/`avellaneda`) so the whole decision unit-tests without a runtime or a
//! broker. Net-new Rust surface — no Python twin, so "parity" is self-consistent reference-value
//! pinning + neutral-reduction, not oracle bits. Naïve `f64` folds (no `mul_add`), per the crate rule.
//!
//! ## The reward MODEL (Polymarket sampling-reward SHAPE)
//!
//! A rewardable market pays a proportional share of a per-market daily pool for RESTING liquidity
//! near the midpoint, sampled per minute. Per order the closeness score is
//!
//! ```text
//!   S(v, s) = ((v − s) / v)²          [ quadratic: closer to the mid ⇒ SUPERLINEARLY more ]
//! ```
//!
//! where `v` = the market's `max_spread` from the midpoint in CENTS and `s` = the order's spread
//! from the (size-cutoff-adjusted) midpoint in cents. An order counts only when it is WITHIN the band
//! (`s ≤ v`) AND at least `min_size` shares. The per-side sum `Σ S·size` is combined two-sidedly:
//!
//! ```text
//!   Q_min = midpoint ∈ [0.10, 0.90] : max( min(Q_one, Q_two), max(Q_one, Q_two) / c )   [ c = 3 ]
//!           midpoint <0.10 or >0.90 : min(Q_one, Q_two)                      [ BOTH sides REQUIRED ]
//! ```
//!
//! so quoting ONE side scores `1/c` of it in the mid-range and NOTHING in the tails — the
//! two-sidedness a rewards maker must honour. The absolute payout (the pool share) is irrelevant to
//! the maker; only this SHAPE — quadratic closeness, two-sided min, moas floor — drives its quote.
//!
//! ## The maker's FOLD
//!
//! [`clamp_into_band`] pulls a candidate two-sided quote into the band (each side `≤ v` from the
//! mid), `weight`-deep toward the mid, trading reward closeness against the A-S adverse-selection
//! spread; [`moas_holds`] is the min-order-age gate that keeps a still-in-band quote resting so it
//! does not churn below the reward floor. BOTH are OFF when `weight ≤ 0` (or the band is non-positive)
//! — returning the input unchanged / `false` — which is what keeps the default maker byte-identical.
//! The A-S near-resolution blackout / fill-rate breaker stay the SAFETY authority: the caller skips
//! the fold entirely inside a blackout, and a suppressed side is pulled regardless.

use vike_model::RewardParams;

/// Lower midpoint band edge: at/above it (and at/below [`REWARD_MIDPOINT_HI`]) one side alone still
/// earns the `1/c` single-sided credit; strictly outside the band BOTH sides are required.
pub const REWARD_MIDPOINT_LO: f64 = 0.10;
/// Upper midpoint band edge (mirror of [`REWARD_MIDPOINT_LO`]).
pub const REWARD_MIDPOINT_HI: f64 = 0.90;
/// The single-sided divisor `c`: inside `[LO, HI]` a market quoted on only ONE side scores `1/c` of
/// that side's value (`max(Q_one, Q_two) / c`).
pub const REWARD_SINGLE_SIDED_DIVISOR: f64 = 3.0;

/// Per-order spread score `S(v, s) = ((v − s)/v)²` — the quadratic closeness score: `1.0` at the
/// midpoint (`s = 0`), decaying to `0.0` at the band edge (`s = v`). ZERO outside the band (`s > v`),
/// for a negative spread, or a non-positive band `v` (nothing rewardable). PURE; naïve folds.
pub fn spread_score(max_spread_cents: f64, spread_cents: f64) -> f64 {
    // `<= 0.0` (not `!(.. > 0.0)`) is the crate convention for a positive-or-off f64 guard — it also
    // keeps clippy's `neg_cmp_op_on_partial_ord` happy (a negated f64 comparison is a lint).
    if max_spread_cents <= 0.0 || spread_cents < 0.0 || spread_cents > max_spread_cents {
        return 0.0;
    }
    let x = (max_spread_cents - spread_cents) / max_spread_cents;
    x * x
}

/// One side's reward score `Σ S(v, sᵢ)·sizeᵢ` over its QUALIFYING orders — within the band AND
/// `size ≥ min_size` (anything else contributes `0`). `orders` are `(spread_cents, size)` pairs. PURE,
/// naïve left-to-right fold: the slice order is deterministic (no dict-order / `py_sum` concern here —
/// net-new Rust, no Python oracle to match).
pub fn side_score(orders: &[(f64, f64)], max_spread_cents: f64, min_size: f64) -> f64 {
    let mut q = 0.0;
    for &(spread_cents, size) in orders {
        if size < min_size {
            continue;
        }
        q += spread_score(max_spread_cents, spread_cents) * size;
    }
    q
}

/// Combine the two side scores into the market's `Q_min`, per the sampling shape:
/// - `midpoint ∈ [REWARD_MIDPOINT_LO, REWARD_MIDPOINT_HI]` (inclusive):
///   `max( min(q_one, q_two), max(q_one, q_two) / c )` — a single side still scores `1/c` of itself;
/// - `midpoint` strictly outside that band: `min(q_one, q_two)` — BOTH sides required (a zero side ⇒
///   zero). PURE.
pub fn q_min(q_one: f64, q_two: f64, midpoint: f64) -> f64 {
    let both = q_one.min(q_two);
    if (REWARD_MIDPOINT_LO..=REWARD_MIDPOINT_HI).contains(&midpoint) {
        both.max(q_one.max(q_two) / REWARD_SINGLE_SIDED_DIVISOR)
    } else {
        both
    }
}

/// Pull a candidate two-sided quote `(bid_px, ask_px)` INTO the reward band around `mid`, trading
/// reward closeness against the A-S adverse-selection spread. Returns the adjusted `(bid_px, ask_px)`.
///
/// OFF (`weight ≤ 0`, or a non-positive band) ⇒ returns the input UNCHANGED, bit-for-bit — the
/// byte-identical default the caller relies on. Otherwise each side's spread from the mid is capped
/// at `s_target = band·(1 − weight)` (`band = max_spread_cents/100`, in 0..1 price units): the maker
/// never quotes WIDER than the A-S candidate wanted (honouring its safety where it is already tight),
/// but pulls a too-wide side IN to at most `s_target` — which is `≤ band`, so the side always
/// qualifies. A larger `weight` ⇒ a tighter cap ⇒ closer to the mid ⇒ more reward and more adverse
/// selection (`weight` IS the reward-vs-A-S trade-off dial). Never widens a side, so it can only ever
/// raise the reward score; the A-S grid-snap / wall-clamp already ran upstream, and the venue mount
/// re-snaps, so this deliberately leaves the pulled price un-snapped (like the `Mid` style already is).
pub(crate) fn clamp_into_band(mid: f64, bid_px: f64, ask_px: f64, p: RewardParams) -> (f64, f64) {
    let band = p.max_spread_cents / 100.0;
    // OFF: a non-positive weight or band (`<= 0.0` — the crate's positive-or-off f64 guard, also
    // clippy `neg_cmp_op_on_partial_ord`-clean) ⇒ return the candidate unchanged, bit-for-bit.
    if p.weight <= 0.0 || band <= 0.0 {
        return (bid_px, ask_px);
    }
    let s_target = band * (1.0 - p.weight.clamp(0.0, 1.0));
    // candidate spreads from the mid, floored at 0 so a degenerate crossed candidate can't WIDEN us
    let s_as_bid = (mid - bid_px).max(0.0);
    let s_as_ask = (ask_px - mid).max(0.0);
    // take the tighter of the A-S candidate and the reward pull target (⇒ never wider than the band)
    let s_bid = s_as_bid.min(s_target);
    let s_ask = s_as_ask.min(s_target);
    (mid - s_bid, mid + s_ask)
}

/// The reward MIN-ORDER-AGE (moas) hold for one side: `true` ⇒ leave the resting quote to keep
/// ageing toward the reward floor (SKIP its re-price) instead of churning it. Holds ONLY while reward
/// chasing is active with a positive moas, the side has rested LESS than `min_order_age_ms`
/// (`now − quoted_ts`), AND the resting quote is still safely IN-BAND around `mid` — `spread`
/// (`mid − resting_px` for a bid, `resting_px − mid` for an ask) in `[0, band]`. Out-of-band or
/// crossed ⇒ `false`, so a safety re-price (a mid that ran the quote out of the band) always fires.
/// PURE. Never consulted on a place / pull / fill path (the caller yields those first).
pub(crate) fn moas_holds(
    p: RewardParams,
    resting_px: f64,
    quoted_ts: i64,
    mid: f64,
    now: i64,
    is_bid: bool,
) -> bool {
    // `weight <= 0.0` (not `!(.. > 0.0)`) keeps clippy's `neg_cmp_op_on_partial_ord` happy.
    if p.weight <= 0.0 || p.min_order_age_ms <= 0 || quoted_ts <= 0 {
        return false;
    }
    if now - quoted_ts >= p.min_order_age_ms {
        return false; // rested long enough — allow the re-price
    }
    let band = p.max_spread_cents / 100.0;
    if band <= 0.0 {
        return false;
    }
    let spread = if is_bid { mid - resting_px } else { resting_px - mid };
    (0.0..=band).contains(&spread)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rewardable bag (weight, band, min_size, moas) for the fold tests.
    fn reward(weight: f64, max_spread_cents: f64, min_size: f64, moas: i64) -> RewardParams {
        RewardParams { weight, max_spread_cents, min_size, min_order_age_ms: moas }
    }

    // S(v,s) = ((v−s)/v)²: 1 at the mid, 0 at the edge, ZERO outside; and CLOSER earns SUPERLINEARLY
    // more (the same tightening step is worth more the nearer you already are).
    #[test]
    fn spread_score_is_quadratic_closeness() {
        assert_eq!(spread_score(4.0, 0.0).to_bits(), 1.0_f64.to_bits(), "at the mid ⇒ 1");
        assert_eq!(spread_score(4.0, 4.0).to_bits(), 0.0_f64.to_bits(), "at the edge ⇒ 0");
        assert_eq!(spread_score(4.0, 2.0).to_bits(), 0.25_f64.to_bits(), "half-band ⇒ 0.25");
        // outside the band / degenerate ⇒ 0
        assert_eq!(spread_score(4.0, 5.0).to_bits(), 0.0_f64.to_bits(), "beyond the edge ⇒ 0");
        assert_eq!(spread_score(4.0, -1.0).to_bits(), 0.0_f64.to_bits(), "negative spread ⇒ 0");
        assert_eq!(spread_score(0.0, 0.0).to_bits(), 0.0_f64.to_bits(), "no band ⇒ 0");
        // SUPERLINEAR: the SAME 1¢ tightening is worth more nearer the mid than nearer the edge
        let near_mid = spread_score(4.0, 0.0) - spread_score(4.0, 1.0); // 1 − 0.5625 = 0.4375
        let near_edge = spread_score(4.0, 3.0) - spread_score(4.0, 4.0); // 0.0625 − 0 = 0.0625
        assert!(near_mid > near_edge, "closer ⇒ superlinearly more: {near_mid} vs {near_edge}");
        // monotone ↓ in s across the band
        let mut prev = f64::INFINITY;
        for i in 0..=8 {
            let s = i as f64 * 0.5;
            let sc = spread_score(4.0, s);
            assert!(sc <= prev, "monotone ↓ in s at s={s}: {sc} > {prev}");
            prev = sc;
        }
    }

    // side_score sums S·size only over QUALIFYING orders (in-band AND size ≥ min_size).
    #[test]
    fn side_score_filters_band_and_min_size() {
        // (spread_cents, size): in-band big, in-band too-small, out-of-band big
        let orders = [(0.0, 10.0), (2.0, 1.0), (5.0, 10.0)];
        // v=4, min_size=5: only the first qualifies (10 shares, s=0 → S=1) → 1·10 = 10
        assert!((side_score(&orders, 4.0, 5.0) - 10.0).abs() < 1e-12, "min_size + band filter");
        // min_size=0: first (1·10=10) + second (S(4,2)=0.25·1=0.25) = 10.25; third out-of-band ⇒ 0
        assert!((side_score(&orders, 4.0, 0.0) - 10.25).abs() < 1e-12, "band filter only");
        // no qualifying orders ⇒ 0
        assert_eq!(side_score(&[], 4.0, 0.0).to_bits(), 0.0_f64.to_bits(), "empty ⇒ 0");
        assert_eq!(
            side_score(&orders, 4.0, 100.0).to_bits(),
            0.0_f64.to_bits(),
            "all too small ⇒ 0"
        );
    }

    // q_min: mid-range gives a single side 1/c credit; the TAILS require both sides (min).
    #[test]
    fn q_min_two_sided_vs_single_sided_and_tails() {
        // mid-range (0.5): both equal ⇒ that value; one side only ⇒ 1/c of it
        assert!((q_min(1.0, 1.0, 0.5) - 1.0).abs() < 1e-12, "both sides ⇒ full");
        assert!((q_min(3.0, 0.0, 0.5) - 1.0).abs() < 1e-12, "single side ⇒ max/c (3/3=1)");
        assert!((q_min(0.0, 3.0, 0.5) - 1.0).abs() < 1e-12, "single side (other) ⇒ max/c");
        // the 1/c credit only helps when it beats min: min(2,1)=1 > 2/3 ⇒ 1
        assert!((q_min(2.0, 1.0, 0.5) - 1.0).abs() < 1e-12, "min wins when both sides present");
        // boundary midpoints are INCLUSIVE (single-sided credit still applies)
        assert!((q_min(3.0, 0.0, 0.10) - 1.0).abs() < 1e-12, "0.10 boundary ⇒ credit");
        assert!((q_min(3.0, 0.0, 0.90) - 1.0).abs() < 1e-12, "0.90 boundary ⇒ credit");
        // the TAILS require BOTH sides — a missing side ⇒ zero
        assert_eq!(q_min(3.0, 0.0, 0.05).to_bits(), 0.0_f64.to_bits(), "<0.10 ⇒ both required");
        assert_eq!(q_min(3.0, 0.0, 0.95).to_bits(), 0.0_f64.to_bits(), ">0.90 ⇒ both required");
        // both sides present in a tail ⇒ min
        assert!((q_min(2.0, 5.0, 0.05) - 2.0).abs() < 1e-12, "tail with both sides ⇒ min");
    }

    // clamp_into_band OFF (weight ≤ 0 / no band) returns the candidate UNCHANGED bit-for-bit.
    #[test]
    fn clamp_into_band_off_is_identity() {
        for off in [reward(0.0, 3.0, 0.0, 0), reward(-1.0, 3.0, 0.0, 0), reward(0.5, 0.0, 0.0, 0)] {
            let (b, a) = clamp_into_band(0.5, 0.45, 0.55, off);
            assert_eq!(b.to_bits(), 0.45_f64.to_bits(), "OFF ⇒ bid unchanged ({off:?})");
            assert_eq!(a.to_bits(), 0.55_f64.to_bits(), "OFF ⇒ ask unchanged ({off:?})");
        }
    }

    // clamp_into_band pulls a too-wide candidate INTO the band; one already tighter is kept.
    #[test]
    fn clamp_into_band_pulls_wide_quote_in() {
        // band = 3¢ = 0.03; weight 0.5 ⇒ s_target = 0.03·0.5 = 0.015
        let p = reward(0.5, 3.0, 0.0, 0);
        // candidate wide (spread 0.05 each side, out of band) ⇒ pulled to s_target 0.015
        let (b, a) = clamp_into_band(0.50, 0.45, 0.55, p);
        assert!((b - 0.485).abs() < 1e-12, "bid pulled to mid − 0.015: {b}");
        assert!((a - 0.515).abs() < 1e-12, "ask pulled to mid + 0.015: {a}");
        // both are now in-band (spread ≤ 0.03)
        assert!((0.50 - b) <= 0.03 + 1e-12, "bid in-band: {b}");
        assert!((a - 0.50) <= 0.03 + 1e-12, "ask in-band: {a}");
        // a candidate already TIGHTER than s_target is left alone (min keeps the tighter A-S spread):
        // it stays at ~0.495/0.505, NOT pulled to the 0.485/0.515 s_target band-depth
        let (b2, a2) = clamp_into_band(0.50, 0.495, 0.505, p); // spread 0.005 < 0.015
        assert!((b2 - 0.495).abs() < 1e-12, "already-tight bid kept: {b2}");
        assert!((a2 - 0.505).abs() < 1e-12, "already-tight ask kept: {a2}");
        // higher weight ⇒ tighter (more reward): weight 0.8 ⇒ s_target 0.006 < 0.015
        let (b3, _) = clamp_into_band(0.50, 0.45, 0.55, reward(0.8, 3.0, 0.0, 0));
        assert!(b3 > b, "a higher weight pulls the bid closer to the mid: {b3} > {b}");
    }

    // moas_holds: holds a YOUNG in-band quote; releases once old enough, out-of-band, or disabled.
    #[test]
    fn moas_holds_young_in_band_only() {
        let p = reward(1.0, 3.0, 0.0, 30_000); // band 0.03, moas 30s
        // young (age 10s < 30s), in-band (bid 0.49, mid 0.50 → spread 0.01 ≤ 0.03) ⇒ HOLD
        assert!(moas_holds(p, 0.49, 1_000, 0.50, 11_000, true), "young in-band bid holds");
        // aged past the moas ⇒ release
        assert!(!moas_holds(p, 0.49, 1_000, 0.50, 31_000, true), "aged past moas ⇒ release");
        // out of band (bid 0.45, mid 0.50 → spread 0.05 > 0.03) ⇒ release (safety re-price)
        assert!(!moas_holds(p, 0.45, 1_000, 0.50, 11_000, true), "out-of-band bid ⇒ release");
        // crossed (bid above the mid) ⇒ release
        assert!(!moas_holds(p, 0.51, 1_000, 0.50, 11_000, true), "crossed bid ⇒ release");
        // ask side mirror: spread = resting − mid, so a 0.51 ask at mid 0.50 is in-band
        assert!(moas_holds(p, 0.51, 1_000, 0.50, 11_000, false), "young in-band ask holds");
        // disabled: weight 0, moas 0, or never-quoted (ts 0)
        assert!(
            !moas_holds(reward(0.0, 3.0, 0.0, 30_000), 0.49, 1_000, 0.50, 11_000, true),
            "weight 0"
        );
        assert!(!moas_holds(reward(1.0, 3.0, 0.0, 0), 0.49, 1_000, 0.50, 11_000, true), "moas 0");
        assert!(!moas_holds(p, 0.49, 0, 0.50, 11_000, true), "never-quoted (ts 0)");
    }
}
