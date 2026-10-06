use super::*;

/// The OLD `math::sma`, as it stood before #1200 de-accumulated it: one running sum, slid one
/// bar at a time. Kept here ONLY as the thing being measured against — nothing in the tree
/// computes this way any more.
fn sma_accumulated(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() < n || n == 0 {
        return out;
    }
    let mut sum: f64 = c[..n].iter().sum();
    out[n - 1] = sum / n as f64;
    for i in n..c.len() {
        sum += c[i] - c[i - n];
        out[i] = sum / n as f64;
    }
    out
}

/// A deterministic OHLC random walk. No `rand` dependency and no seed plumbing: an LCG with a
/// fixed constant is reproducible across platforms, which a float-hashing scheme would not be.
///
/// ⚠ The BODY DISTRIBUTION is what this measurement is sensitive to, so it is chosen rather
/// than accepted: `|close - open|` spans three orders of magnitude here (a `u >= 0.82` arm
/// makes ~18% of bars near-doji), because a flip can only happen to a bar whose body sits
/// within the two kernels' disagreement of a threshold. A synthetic series of uniformly-fat
/// candles would report a flip rate near zero and would be measuring its own fixture.
fn walk(n: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    let (mut o, mut h, mut l, mut c) = (vec![], vec![], vec![], vec![]);
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        // ⚠ FULL 52-bit mantissa, not a 31-bit slice. A coarse LCG slice makes every body a
        // dyadic rational of ~32 significant bits, and a ten-term sum of those is EXACT in
        // f64, so both kernels agree bit-for-bit and the measurement silently reports zero.
        ((state >> 11) as f64) / ((1u64 << 53) as f64)
    };
    let mut px = 64_000.0_f64;
    for _ in 0..n {
        let open = px;
        let u = next();
        // Three regimes, so the body distribution is wide: near-doji, ordinary, and fat.
        // Non-dyadic scales, for the same reason: a power-of-two scale preserves exactness.
        let scale = if u >= 0.82 {
            0.019
        } else if u >= 0.35 {
            3.7
        } else {
            21.3
        };
        let close = open + (next() - 0.5) * scale;
        let wick = next() * 6.0;
        o.push(open);
        c.push(close);
        h.push(open.max(close) + wick);
        l.push(open.min(close) - wick * next());
        px = close;
    }
    (o, h, l, c)
}

/// ⚠ **The number #1200 did not produce.** It stated that de-accumulating `sma` "WILL flip some
/// +/-100 signals on bars that sat exactly on a threshold" — true, and unactionable, because
/// nobody could tell whether that meant one bar in a million or one in twenty.
///
/// Every one of the ~40 `avg_body` comparison sites in this file has the SAME shape — `body`
/// against `k * avg` for one of nine constants `k` — so the flip condition is exact and needs
/// no pattern kernel to evaluate: bar `i` flips for multiplier `k` iff `body[i]` falls strictly
/// between `k * avg_old[i]` and `k * avg_new[i]`. Counting that over every `k` measures every
/// site at once.
///
/// `#[ignore]`d: it is a MEASUREMENT, not a gate. The kernel it compares against no longer
/// exists, so a permanent assertion here would be pinning a historical artifact rather than a
/// property of the code. Run it with `--ignored --nocapture`.
#[test]
#[ignore = "measurement, not a gate: run with --ignored --nocapture"]
fn measure_the_threshold_flip_rate_from_deaccumulating_sma() {
    // The nine multipliers actually used, with their site counts (`grep -o '[0-9.]*\s*\*\s*a'`).
    const KS: &[(f64, usize)] = &[
        (0.1, 13),
        (0.7, 11),
        (0.5, 8),
        (0.3, 6),
        (0.03, 2),
        (0.01, 2),
        (1.3, 1),
        (0.05, 1),
        (0.02, 1),
    ];

    for &n in &[10_000usize, 200_000] {
        let (o, h, l, c) = walk(n);
        let bodies: Vec<f64> = (0..n).map(|i| (c[i] - o[i]).abs()).collect();
        let new = sma(&bodies, CTX);
        let old = sma_accumulated(&bodies, CTX);

        let differing_avg =
            (0..n).filter(|&i| !new[i].is_nan() && new[i].to_bits() != old[i].to_bits()).count();

        let mut total_flips = 0usize;
        let mut weighted = 0usize;
        println!("\n=== {n} bars ===");
        println!("avg_body values differing in bits: {differing_avg} / {n}");
        // ⚠ NON-VACUITY, asserted before any zero above is believed — and asserted on an
        // ADVERSARIAL input rather than on the realistic one, which is the whole subtlety here.
        //
        // The obvious guard, `assert!(differing_avg > 0)`, is WRONG: it presumes the realistic
        // fixture must produce a divergence, so a genuine finding of "these agree on real data"
        // is indistinguishable from a broken generator. The first two versions of this test hit
        // both sides of that. What must be proven is that the DETECTOR works — that
        // `sma_accumulated` really accumulates — and that is a property of the reconstruction,
        // provable on an input built for it.
        //
        // 1e16 enters the window, swamps the small terms, then leaves: the classic
        // catastrophic-cancellation shape a running sum cannot survive and a fresh sum does not
        // see. If these agreed, the reconstruction would not be an accumulator at all.
        let mut adv = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
        adv.push(1e16);
        adv.extend([0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0]);
        let (an, ao) = (sma(&adv, CTX), sma_accumulated(&adv, CTX));
        let adv_diff = (0..adv.len())
            .filter(|&i| !an[i].is_nan() && an[i].to_bits() != ao[i].to_bits())
            .count();
        assert!(
            adv_diff > 0,
            "the DETECTOR is broken, so no count here means anything: `sma_accumulated` agrees \
                 with `sma` even on an input engineered for catastrophic cancellation, which an \
                 accumulating kernel cannot do. Fix the reconstruction before reading any number."
        );
        println!("detector check (adversarial input): {adv_diff} / {} bars differ", adv.len());
        for &(k, sites) in KS {
            let flips = (0..n)
                .filter(|&i| {
                    !new[i].is_nan()
                        && new[i] > 0.0
                        && (bodies[i] <= k * old[i]) != (bodies[i] <= k * new[i])
                })
                .count();
            total_flips += flips;
            weighted += flips * sites;
            println!("  k={k:<5} ({sites:2} sites): {flips} flip(s)");
        }
        println!("distinct (bar, k) flips: {total_flips}; site-weighted: {weighted}");

        // `is_doji` is the single most-used gate (13 sites); evaluate it end-to-end rather than
        // via the generic condition, so the arithmetic above is cross-checked by real code.
        let doji_flips = (0..n)
            .filter(|&i| {
                is_doji(o[i], h[i], l[i], c[i], old[i]) != is_doji(o[i], h[i], l[i], c[i], new[i])
            })
            .count();
        println!("is_doji() disagreements (real kernel): {doji_flips} / {n}");
    }
}
