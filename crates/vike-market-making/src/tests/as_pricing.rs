//! The Avellaneda–Stoikov pricing layer (audit mm-quote): analytic bit-exact anchors, golden
//! numeric vectors from an INDEPENDENT reference, and invariant sweeps. The mounted-maker half (the
//! A-S maker quoting, the A-S-off reduction) moved with the maker to the spread maker file's tests
//! (`strategies/market_making/spread_maker.rs`).

use super::*;

// ---- Avellaneda–Stoikov pricing layer (audit mm-quote) ----
//
// A-S is net-new Rust (no Python oracle), so correctness is pinned three ways per the mm-quote
// spec §6: (a) ANALYTIC bit-exact anchors on the pure fns, (b) GOLDEN numeric vectors computed by
// an INDEPENDENT reference (a throwaway `rustc` scratch using the TEXTBOOK groupings, never
// vike-trader-app) at ≤1e-12 relative, and (c) invariant sweeps + neutral-reduction.

/// True when `a` is within `rel` RELATIVE of `b` (spec's ≤1e-12 tolerance class for the
/// exp/ln-derived pieces). Guards tiny `b` with a `max(1.0)` denominator.
fn approx_rel(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs().max(1.0)
}

// (a) reservation-price anchors: q==0 ⇒ r==s BIT-EXACT; r monotone ↓ in q; +q/−q mirror about s.
#[test]
fn as_reservation_price_anchors() {
    // q == 0 ⇒ r == s exactly, for a sweep of s/γ/V (0·γ·V folds to 0, s−0 == s bit-for-bit)
    for &s in &[0.05_f64, 0.3, 0.5, 0.72, 0.99] {
        for &(g, v) in &[(0.1_f64, 0.25_f64), (0.9, 0.1), (2.0, 0.2)] {
            let r = as_reservation_price(s, 0.0, g, v);
            assert_eq!(r.to_bits(), s.to_bits(), "q=0 ⇒ r==s exactly (s={s}, γ={g}, V={v})");
        }
    }
    // strictly monotone ↓ in q (γ,V > 0)
    let (s, g, v) = (0.5, 0.5, 0.2);
    let mut previous = f64::INFINITY;
    let mut q = -3.0;
    while q <= 3.0 {
        let r = as_reservation_price(s, q, g, v);
        assert!(r < previous, "r strictly decreasing in q at q={q}: {r} !< {previous}");
        previous = r;
        q += 0.5;
    }
    // symmetry: the offset from s is exactly odd in q — r(+q)−s == −(r(−q)−s), bit-for-bit
    let off_pos = as_reservation_price(s, 1.5, g, v) - s;
    let off_neg = as_reservation_price(s, -1.5, g, v) - s;
    assert_eq!(off_pos.to_bits(), (-off_neg).to_bits(), "reservation offset is odd in q");
}

// (a)+(b) half-spread: V==0 ⇒ intensity FLOOR bit-exact (and == the independent golden bits);
// monotone ↑ in γ and V, ↓ in κ.
#[test]
fn as_half_spread_floor_and_monotonicity() {
    // V == 0 ⇒ the vol term vanishes and δ_half == (1/γ)·ln(1+γ/κ) bit-for-bit
    let (g, k) = (0.5_f64, 50.0_f64);
    let floor = as_intensity_halfspread(g, k);
    assert_eq!(
        as_optimal_half_spread(g, 0.0, k).to_bits(),
        floor.to_bits(),
        "V=0 ⇒ δ_half is exactly the intensity floor"
    );
    // and that floor matches the INDEPENDENT rustc-scratch golden bits 0x3f9460d6ccca367c
    assert_eq!(floor.to_bits(), 0x3f94_60d6_ccca_367c, "intensity floor golden bits");

    // ↑ in V
    let base = as_optimal_half_spread(0.5, 0.10, 50.0);
    assert!(as_optimal_half_spread(0.5, 0.20, 50.0) > base, "δ_half ↑ in V");
    // ↑ in γ (at p=0.5-scale V the vol term dominates)
    assert!(
        as_optimal_half_spread(0.9, 0.25, 50.0) > as_optimal_half_spread(0.3, 0.25, 50.0),
        "δ_half ↑ in γ"
    );
    // ↓ in κ (easier fills ⇒ tighter spread)
    assert!(
        as_optimal_half_spread(0.5, 0.2, 100.0) < as_optimal_half_spread(0.5, 0.2, 20.0),
        "δ_half ↓ in κ"
    );
}

// (b) GOLDEN numeric vectors for (r, δ_half) from the INDEPENDENT rustc scratch (textbook
// groupings — a genuinely different computation than the impl's decomposition), pinned as EXACT
// reference bits and compared at ≤1e-12 relative (allowing the ~1-ulp grouping difference).
#[test]
fn as_golden_vectors() {
    // (s, q, γ, V, κ, r_ref_bits, dhalf_ref_bits) — reference decimals in the trailing comment
    let cases = [
        // r=0.4000000000000000, δ_half=0.06990066170634
        (0.5, 1.0, 0.5, 0.2, 50.0, 0x3fd9_9999_9999_999a_u64, 0x3fb1_e502_7fff_5a6c_u64),
        // r=0.8600000000000000, δ_half=0.09937333382421
        (0.62, -2.0, 0.8, 0.15, 25.0, 0x3feb_851e_b851_eb85, 0x3fb9_7087_e2de_96d0),
        // r=0.1740000000000000, δ_half=0.13840717707813
        (0.30, 0.5, 1.2, 0.21, 80.0, 0x3fc6_45a1_cac0_8312, 0x3fc1_b753_8d8a_8681),
    ];
    for (s, q, g, v, k, r_bits, d_bits) in cases {
        let (r_ref, d_ref) = (f64::from_bits(r_bits), f64::from_bits(d_bits));
        let r = as_reservation_price(s, q, g, v);
        let d = as_optimal_half_spread(g, v, k);
        assert!(approx_rel(r, r_ref, 1e-12), "r golden (s={s},q={q}): {r} vs {r_ref}");
        assert!(approx_rel(d, d_ref, 1e-12), "δ_half golden (γ={g},V={v},κ={k}): {d} vs {d_ref}");
    }
}

// (a)+(b) bounded variance: the three modes; walls ⇒ 0 bit-exact for the capped/pure forms;
// peak at p=0.5; the ≥0 clamp; golden bits for a capped case.
#[test]
fn bounded_variance_walls_and_modes() {
    use VarianceMode::*;
    // walls ⇒ V == 0 exactly (capped + pure); RawLocal ignores the walls
    for &p in &[0.0_f64, 1.0] {
        assert_eq!(
            bounded_variance(0.01, 100.0, p, LocalCapped).to_bits(),
            0.0_f64.to_bits(),
            "LocalCapped V=0 at wall p={p}"
        );
        assert_eq!(
            bounded_variance(0.01, 100.0, p, PureBernoulli).to_bits(),
            0.0_f64.to_bits(),
            "PureBernoulli V=0 at wall p={p}"
        );
    }
    assert!(bounded_variance(0.01, 100.0, 0.0, RawLocal) > 0.0, "RawLocal ignores the wall");
    // Bernoulli peaks at 0.5: p(1−p) larger at 0.5 than at 0.9
    assert!(
        bounded_variance(0.0, 0.0, 0.5, PureBernoulli)
            > bounded_variance(0.0, 0.0, 0.9, PureBernoulli),
        "Bernoulli variance peaks at p=0.5"
    );
    // LocalCapped = min(σ²H, p(1−p)): σ²H=0.1 < 0.25 ⇒ picks the local term (golden bits for 0.1)
    assert_eq!(
        bounded_variance(0.001, 100.0, 0.5, LocalCapped).to_bits(),
        0x3fb9_9999_9999_999a,
        "capped picks the smaller local term 0.001·100=0.1"
    );
    // and the cap bites when the local term exceeds the Bernoulli ceiling
    assert_eq!(
        bounded_variance(1.0, 100.0, 0.5, LocalCapped).to_bits(),
        0.25_f64.to_bits(),
        "capped at the Bernoulli ceiling 0.25"
    );
    // pure-Bernoulli golden bits at p=0.9
    assert_eq!(
        bounded_variance(0.0, 0.0, 0.9, PureBernoulli).to_bits(),
        0x3fb7_0a3d_70a3_d709,
        "Bernoulli 0.9·0.1 golden bits"
    );
}

// update_sigma2 EWMA recurrence: golden bits (same expression as the reference) + the Δt≤0 guard.
#[test]
fn update_sigma2_and_alpha_goldens() {
    // prev=0.04, Δ=0.01, Δt=2ms, α=0.1 → inst=5e-5, next=α·inst+(1−α)·prev (golden bits)
    let next = update_sigma2(0.04, 0.01, 2.0, 0.1);
    assert_eq!(next.to_bits(), 0x3fa2_6f3f_52fc_2657, "σ̂² EWMA golden bits");
    // Δt ≤ 0 ⇒ unchanged
    assert_eq!(update_sigma2(0.04, 0.01, 0.0, 0.1).to_bits(), 0.04_f64.to_bits(), "Δt=0 holds");
    assert_eq!(update_sigma2(0.04, 0.01, -3.0, 0.1).to_bits(), 0.04_f64.to_bits(), "Δt<0 holds");
    // ewma_alpha(half_life=32) golden bits; and the ≤0 half-life ⇒ α=1.0
    assert_eq!(ewma_alpha(32.0).to_bits(), 0x3f95_f134_9237_5800, "α from half-life 32 golden");
    assert_eq!(ewma_alpha(0.0).to_bits(), 1.0_f64.to_bits(), "half_life≤0 ⇒ α=1");
}

// κ MLE: empty/thin window ⇒ κ_default; degenerate Σwδ ⇒ default; clamps hold; and a synthetic
// exponential tape recovers the known κ (≈50) within tolerance.
#[test]
fn fit_kappa_fallback_clamp_and_recovers_known_kappa() {
    // below n_min ⇒ κ_default
    assert_eq!(
        fit_kappa(100.0, 2.0, 5, 20, 50.0, 1.0, 1000.0).to_bits(),
        50.0_f64.to_bits(),
        "n<n_min ⇒ κ_default"
    );
    // degenerate window (Σwδ==0) ⇒ κ_default even with enough samples
    assert_eq!(
        fit_kappa(100.0, 0.0, 50, 20, 50.0, 1.0, 1000.0).to_bits(),
        50.0_f64.to_bits(),
        "Σwδ=0 ⇒ κ_default"
    );
    // clamps: a huge ratio pins κ_max, a tiny ratio pins κ_min
    assert_eq!(
        fit_kappa(100.0, 0.001, 50, 20, 50.0, 1.0, 500.0).to_bits(),
        500.0_f64.to_bits(),
        "over κ_max clamps down"
    );
    assert_eq!(
        fit_kappa(1.0, 100.0, 50, 20, 50.0, 5.0, 1000.0).to_bits(),
        5.0_f64.to_bits(),
        "under κ_min clamps up"
    );
    // recovery: stratified exponential sample (rate 50) → κ̂ ≈ 50.0867 (golden)
    let (kappa, n) = (50.0_f64, 200usize);
    let (mut sum_w, mut sum_w_delta) = (0.0_f64, 0.0_f64);
    for i in 0..n {
        let u = (i as f64 + 0.5) / n as f64;
        let delta = -(1.0 - u).ln() / kappa;
        sum_w += 1.0;
        sum_w_delta += delta;
    }
    let khat = fit_kappa(sum_w, sum_w_delta, n, 20, 50.0, 1.0, 1000.0);
    assert!(approx_rel(khat, 50.08674153558405, 1e-9), "κ̂ golden: {khat}");
    assert!((khat - 50.0).abs() < 0.2, "κ̂ recovers the known κ within tolerance: {khat}");
}

// as_quotes invariants (spec §6 sweep): on-grid, ordered, straddling s; monotone ↓ in q; None
// when the sides collapse / the market is pinned at a wall. Split by PRICE DOMAIN — the
// UnitInterval (Polymarket [0,1]) case keeps the `bid < s < ask ≤ 1−tick` wall bound BYTE-
// IDENTICALLY (bounds `(tick, 1−tick)`, no half-spread floor); the Unbounded/Band ($-scale) cases
// assert the generalized `lo ≤ bid < s < ask ≤ hi`.
#[test]
fn as_quotes_invariants_and_none_at_walls() {
    let tick = 0.01;
    // UnitInterval bounds `(tick, 1−tick)` + no half-spread floor = the pre-generalization path.
    let ui = |s: f64, q: f64, g: f64, v: f64, k: f64, standoff: f64| {
        as_quotes(s, q, g, v, k, tick, tick, 1.0 - tick, 0.0, 0.0, 0.0, standoff)
    };
    // a healthy mid-range book: valid, on-grid, straddling s, inside the [tick, 1−tick] walls
    for &q in &[-2.0_f64, 0.0, 2.0] {
        let (bid, ask) = ui(0.5, q, 0.3, 0.2, 50.0, 1.0).expect("valid quote");
        assert!(
            tick <= bid && bid < 0.5 && 0.5 < ask && ask <= 1.0 - tick,
            "UnitInterval bounded/ordered (q={q}): {bid}/{ask}"
        );
        // on-grid: an integer number of ticks
        assert!(((bid / tick).round() - bid / tick).abs() < 1e-9, "bid on grid: {bid}");
        assert!(((ask / tick).round() - ask / tick).abs() < 1e-9, "ask on grid: {ask}");
    }
    // monotone ↓ in q (both posted sides shift down as inventory grows long)
    let (b_short, a_short) = ui(0.5, -2.0, 0.3, 0.2, 50.0, 1.0).unwrap();
    let (b_flat, a_flat) = ui(0.5, 0.0, 0.3, 0.2, 50.0, 1.0).unwrap();
    let (b_long, a_long) = ui(0.5, 2.0, 0.3, 0.2, 50.0, 1.0).unwrap();
    assert!(b_long <= b_flat && b_flat <= b_short, "bid non-increasing in q");
    assert!(a_long <= a_flat && a_flat <= a_short, "ask non-increasing in q");
    // flat ⇒ symmetric about s (bid and ask equidistant from 0.5)
    assert!(((0.5 - b_flat) - (a_flat - 0.5)).abs() < 1e-12, "flat quotes symmetric about s");
    // pinned at a wall: s within a tick of 0 ⇒ no valid two-sided quote
    assert!(ui(0.005, 0.0, 0.3, 0.2, 50.0, 1.0).is_none(), "pinned near 0 ⇒ None");
    // sub-tick room (standoff 0, tiny spread on an on-grid mid) ⇒ sides collapse ⇒ None
    assert!(ui(0.5, 0.0, 0.0001, 1e-9, 1e6, 0.0).is_none(), "collapsed sides ⇒ None");

    // GENERALIZED domains ($-scale, the crypto lift): at a $64k mid on a $1 tick the intensity
    // half-spread (~0.02) is SUB-TICK, so the `min_half_spread_ticks` floor (2) is what lets a
    // two-sided quote form at all (without it bid/ask snap to the same tick → None).
    let big_tick = 1.0;
    let s = 64_767.0;
    // UNBOUNDED `(−∞, +∞)`: no wall clamp — the ask is free to sit ABOVE the old [0,1] ceiling.
    for &q in &[-3.0_f64, 0.0, 3.0] {
        let quote = as_quotes(
            s,
            q,
            0.1,
            0.0,
            50.0,
            big_tick,
            f64::NEG_INFINITY,
            f64::INFINITY,
            2.0,
            0.0,
            0.0,
            1.0,
        );
        let (bid, ask) = quote.expect("unbounded $-scale quote");
        assert!(bid < s && s < ask, "Unbounded straddles the mid (q={q}): {bid}/{ask}");
        assert!(bid < ask, "Unbounded ordered (q={q}): {bid}/{ask}");
        assert!(((bid / big_tick).round() - bid / big_tick).abs() < 1e-9, "bid on grid: {bid}");
        assert!(((ask / big_tick).round() - ask / big_tick).abs() < 1e-9, "ask on grid: {ask}");
        assert!(ask > 1.0, "Unbounded ask clears the old unit-interval ceiling: {ask}");
    }
    // BAND [64000, 65000]: clamps like the unit interval but onto explicit $-scale walls.
    let band = as_quotes(s, 0.0, 0.1, 0.0, 50.0, big_tick, 64_000.0, 65_000.0, 2.0, 0.0, 0.0, 1.0);
    let (bid, ask) = band.expect("band $-scale quote");
    assert!(bid < s && s < ask, "Band straddles the mid: {bid}/{ask}");
    assert!((64_000.0..=65_000.0).contains(&bid), "Band bid inside walls: {bid}");
    assert!((64_000.0..=65_000.0).contains(&ask), "Band ask inside walls: {ask}");
}

// as_quotes composes r and δ_half then snaps: with no clamping the sides are exactly
// snap(r∓δ_half) — pins the composition end-to-end. UnitInterval bounds `(tick, 1−tick)` + a
// `0.0` half-spread floor keep this the byte-identical pre-generalization arithmetic (`δ.max(0)
// == δ`), so the golden `.to_bits()` equalities below hold unchanged after the domain lift.
#[test]
fn as_quotes_composes_reservation_and_spread() {
    let (s, g, v, k, tick) = (0.5, 0.5, 0.2, 50.0, 0.01);
    // FLAT: δ_half ≫ the standoff, so neither side is clamped — sides are exactly snap(s ∓ δ_half)
    let d = as_optimal_half_spread(g, v, k);
    let (bid, ask) =
        as_quotes(s, 0.0, g, v, k, tick, tick, 1.0 - tick, 0.0, 0.0, 0.0, 1.0).unwrap();
    assert_eq!(bid.to_bits(), snap_to_tick(s - d, tick).to_bits(), "flat bid == snap(s − δ_half)");
    assert_eq!(ask.to_bits(), snap_to_tick(s + d, tick).to_bits(), "flat ask == snap(s + δ_half)");
    // LONG inventory ⇒ r < s ⇒ the whole quote shifts below the flat case (the ask here is pulled
    // to the s+standoff floor — the "never quote through the mid" clamp — which is < the flat ask)
    let (bid_long, ask_long) =
        as_quotes(s, 1.0, g, v, k, tick, tick, 1.0 - tick, 0.0, 0.0, 0.0, 1.0).unwrap();
    assert!(bid_long < bid && ask_long < ask, "long skews both sides down");
}
