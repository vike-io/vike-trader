//! The Cartea–Jaimungal short-term-alpha slot the A–S reservation currently lacks — the additive
//! drift term `a·h − φ·q·h` and its two microstructure signal inputs, isolated so they unit-test
//! without a runtime. PURE f64, NAIVE folds (no `mul_add` — the reservation stack pins fold order),
//! std only.
//!
//! Sources:
//! - Cartea, Jaimungal & Penalva, "Algorithmic and High-Frequency Trading" (CUP, 2015) — the
//!   inventory-penalized short-alpha market-making formulation: the optimal quotes ride a reservation
//!   price shifted by an additive signal term `a·(T−t)` and a running inventory penalty `−φ·q·(T−t)`,
//!   the CJ analog of the Avellaneda–Stoikov reservation shift.
//! - Cont, Kukanov & Stoikov, "The Price Impact of Order Book Events", Journal of Financial
//!   Econometrics 12(1):47–88 (SSRN 1712822) — the signed order-flow-imbalance (OFI) variable `e_n`
//!   built from best-quote queue changes; the linear price-impact predictor these signals feed.

/// Top-of-book imbalance ∈ [−1, 1] — the normalized queue lean at the touch (Cont–Kukanov–Stoikov's
/// static book-pressure signal). `(bid_sz − ask_sz) / (bid_sz + ask_sz)`: `+1` all bid, `−1` all ask,
/// `0` balanced. A non-positive denominator (both sides empty/degenerate) ⇒ `0.0` (no signal), which
/// also guards the division.
pub(crate) fn book_imbalance(bid_sz: f64, ask_sz: f64) -> f64 {
    let denom = bid_sz + ask_sz;
    if denom <= 0.0 {
        return 0.0;
    }
    (bid_sz - ask_sz) / denom
}

/// The Cont–Kukanov–Stoikov OFI accumulator: an EWMA-decayed rolling sum of the signed best-quote
/// queue change `e_n` per book update. Holds the PREVIOUS best `(bid, bid_sz, ask, ask_sz)` (as
/// `Option`, `None` before the first update), the decayed running `ofi`, and the per-update `decay`
/// factor in `[0, 1)`. Each update decays the carried sum then folds in this event's `e_n`, so recent
/// flow dominates and a stale reading relaxes toward `0`.
pub(crate) struct OfiTracker {
    /// previous best `(bid, bid_sz, ask, ask_sz)`; `None` until the first `on_book`
    last: Option<(f64, f64, f64, f64)>,
    /// EWMA-decayed running order-flow imbalance
    ofi: f64,
    /// per-update decay factor, `[0, 1)`
    decay: f64,
}

impl OfiTracker {
    /// `decay` is the per-update EWMA factor in `[0, 1)`: `ofi *= decay` then `ofi += e_n` each update
    /// (`0` = memoryless, only the latest `e_n`; near `1` = a long-memory sum).
    pub(crate) fn new(decay: f64) -> Self {
        OfiTracker { last: None, ofi: 0.0, decay }
    }

    /// Fold one book update through the CKS signed queue-change `e_n`, using the PREVIOUS best
    /// `(b', bs', a', as')`:
    ///
    /// ```text
    /// e_bid = (bid >= b' ? bid_sz : 0) − (bid <= b' ? bs' : 0)
    /// e_ask = (ask <= a' ? ask_sz : 0) − (ask >= a' ? as' : 0)
    /// e_n   = e_bid − e_ask
    /// ```
    ///
    /// A bid stepping up or growing adds size (buy pressure, `e_n > 0`); a bid stepping down or an ask
    /// stepping down/growing subtracts (sell pressure, `e_n < 0`). The FIRST update has no previous
    /// best, so `e_n = 0` — it only seeds state. Then `ofi = ofi·decay + e_n`.
    pub(crate) fn on_book(&mut self, bid: f64, bid_sz: f64, ask: f64, ask_sz: f64) {
        let e_n = match self.last {
            None => 0.0,
            Some((b_prev, bs_prev, a_prev, as_prev)) => {
                let e_bid = (if bid >= b_prev { bid_sz } else { 0.0 })
                    - (if bid <= b_prev { bs_prev } else { 0.0 });
                let e_ask = (if ask <= a_prev { ask_sz } else { 0.0 })
                    - (if ask >= a_prev { as_prev } else { 0.0 });
                e_bid - e_ask
            }
        };
        self.ofi = self.ofi * self.decay + e_n;
        self.last = Some((bid, bid_sz, ask, ask_sz));
    }

    /// The current EWMA-decayed order-flow imbalance.
    pub(crate) fn ofi(&self) -> f64 {
        self.ofi
    }
}

/// Assemble the scalar alpha signal from its two inputs: `beta·imbalance + lambda·ofi`. `beta` weights
/// the static top-of-book lean, `lambda` the dynamic order-flow imbalance. Linear so the caller tunes
/// each channel independently (either weight `0` drops that channel).
pub(crate) fn combine_alpha(imbalance: f64, beta: f64, ofi: f64, lambda: f64) -> f64 {
    beta * imbalance + lambda * ofi
}

/// Synthesize a flow-toxicity INTENSITY in `[0, 1]` from an order-flow-imbalance magnitude — the
/// internal fallback the [`SpreadMaker`](crate::SpreadMaker) feeds its flow-toxicity guard when no
/// external [`Strategy::on_flow`](vike_model::Strategy) reading exists (Group-B, PR-3):
/// `tox = 1 − exp(−|ofi| / scale)`. `0` at `ofi = 0`, monotonically rising toward `1` as `|ofi| → ∞`,
/// reaching ~0.63 (`1 − e⁻¹`) at `|ofi| = scale`. `scale <= 0` ⇒ `0.0` (disabled, and guards the
/// division). Depends on `|ofi|` only, so it is symmetric in the sign of the OFI (the CALLER derives
/// the toxic SIDE from that sign). Naïve folds, no `mul_add`.
pub(crate) fn ofi_toxicity(ofi: f64, scale: f64) -> f64 {
    if scale <= 0.0 {
        return 0.0;
    }
    1.0 - libm::exp(-ofi.abs() / scale)
}

/// The Cartea–Jaimungal additive reservation shift `a·h − φ·q·h` added onto the A–S reservation price
/// before quoting. `alpha_signal` is `a` (from [`combine_alpha`]), `horizon` is the normalized
/// remaining time `h = (T − t)` the caller supplies, `q_norm` is the unit-normalized inventory `q`, and
/// `phi` (≥ 0) is the running inventory-penalty coefficient. Positive alpha RAISES the reservation
/// (lean into predicted up-drift); long inventory (`q_norm > 0`) with `phi > 0` LOWERS it (lean to
/// unwind). `horizon = 0` (terminal) ⇒ both terms vanish.
pub(crate) fn cj_reservation_shift(alpha_signal: f64, horizon: f64, q_norm: f64, phi: f64) -> f64 {
    alpha_signal * horizon - phi * q_norm * horizon
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imbalance_signs_range_and_zero_denom() {
        // balanced → 0
        assert_eq!(book_imbalance(5.0, 5.0), 0.0);
        // bid-heavy → positive, ask-heavy → negative
        assert!(book_imbalance(9.0, 1.0) > 0.0);
        assert!(book_imbalance(1.0, 9.0) < 0.0);
        // all bid / all ask saturate at ±1
        assert_eq!(book_imbalance(7.0, 0.0), 1.0);
        assert_eq!(book_imbalance(0.0, 7.0), -1.0);
        // stays in range
        let v = book_imbalance(3.0, 8.0);
        assert!((-1.0..=1.0).contains(&v));
        // zero / negative denom → 0 (guarded)
        assert_eq!(book_imbalance(0.0, 0.0), 0.0);
        assert_eq!(book_imbalance(-2.0, 1.0), 0.0);
    }

    #[test]
    fn ofi_first_update_is_zero() {
        let mut t = OfiTracker::new(0.5);
        t.on_book(100.0, 4.0, 101.0, 4.0);
        assert_eq!(t.ofi(), 0.0);
    }

    #[test]
    fn ofi_accumulates_positive_on_rising_bid() {
        // decay 1.0 = pure sum, easy to reason about
        let mut t = OfiTracker::new(1.0);
        t.on_book(100.0, 4.0, 101.0, 4.0); // seed, e_n = 0
        t.on_book(100.5, 5.0, 101.5, 5.0); // bid up, ask up → buy pressure
        t.on_book(101.0, 6.0, 102.0, 6.0); // bid up again
        assert!(t.ofi() > 0.0, "rising bid must build positive OFI, got {}", t.ofi());
    }

    #[test]
    fn ofi_accumulates_negative_on_falling_bid() {
        let mut t = OfiTracker::new(1.0);
        t.on_book(101.0, 6.0, 102.0, 6.0); // seed
        t.on_book(100.5, 5.0, 101.5, 5.0); // bid down, ask down → sell pressure
        t.on_book(100.0, 4.0, 101.0, 4.0); // bid down again
        assert!(t.ofi() < 0.0, "falling bid must build negative OFI, got {}", t.ofi());
    }

    #[test]
    fn ofi_decay_shrinks_stale_reading_toward_zero() {
        let mut t = OfiTracker::new(0.5);
        t.on_book(100.0, 4.0, 101.0, 4.0); // seed
        t.on_book(100.5, 9.0, 101.0, 4.0); // build some positive OFI
        let hot = t.ofi();
        assert!(hot > 0.0);
        // feed neutral (unchanged) updates: e_n stays 0, decay halves the carried sum each time
        for _ in 0..5 {
            t.on_book(100.5, 9.0, 101.0, 4.0);
        }
        let stale = t.ofi();
        assert!(stale < hot, "decay must shrink a stale OFI: hot {hot}, stale {stale}");
        assert!(stale > 0.0 && stale < hot * 0.5, "still decaying toward 0");
    }

    #[test]
    fn combine_alpha_is_linear() {
        // superposition: f(a+c, b) channels add independently
        let both = combine_alpha(0.4, 2.0, 0.3, 5.0);
        let just_imb = combine_alpha(0.4, 2.0, 0.0, 5.0);
        let just_ofi = combine_alpha(0.0, 2.0, 0.3, 5.0);
        assert!((both - (just_imb + just_ofi)).abs() < 1e-12);
        // explicit value
        assert!((combine_alpha(0.5, 2.0, 0.25, 4.0) - (2.0 * 0.5 + 4.0 * 0.25)).abs() < 1e-12);
        // zero weights drop channels
        assert_eq!(combine_alpha(0.9, 0.0, 0.9, 0.0), 0.0);
    }

    #[test]
    fn cj_shift_alpha_positive_raises() {
        // positive alpha, flat inventory → reservation rises
        let s = cj_reservation_shift(0.8, 0.5, 0.0, 0.1);
        assert!(s > 0.0);
        // negative alpha → lowers
        assert!(cj_reservation_shift(-0.8, 0.5, 0.0, 0.1) < 0.0);
    }

    #[test]
    fn cj_shift_long_inventory_penalty_lowers() {
        // no alpha, long inventory, phi > 0 → penalty pushes reservation down
        assert!(cj_reservation_shift(0.0, 0.5, 1.0, 0.4) < 0.0);
        // short inventory mirrors up
        assert!(cj_reservation_shift(0.0, 0.5, -1.0, 0.4) > 0.0);
        // long inventory drags a positive-alpha shift below the alpha-only shift
        let alpha_only = cj_reservation_shift(0.6, 0.5, 0.0, 0.4);
        let with_inv = cj_reservation_shift(0.6, 0.5, 1.0, 0.4);
        assert!(with_inv < alpha_only);
    }

    #[test]
    fn cj_shift_zero_horizon_vanishes() {
        assert_eq!(cj_reservation_shift(0.9, 0.0, 1.0, 0.5), 0.0);
        assert_eq!(cj_reservation_shift(-0.3, 0.0, -2.0, 0.5), 0.0);
    }

    #[test]
    fn ofi_toxicity_disabled_and_zero_are_zero() {
        // scale <= 0 ⇒ disabled ⇒ 0 for any ofi (guards the division too).
        assert_eq!(ofi_toxicity(5.0, 0.0), 0.0);
        assert_eq!(ofi_toxicity(5.0, -1.0), 0.0);
        // ofi == 0 ⇒ 1 − e^0 = 0.
        assert_eq!(ofi_toxicity(0.0, 2.0), 0.0);
    }

    #[test]
    fn ofi_toxicity_in_unit_range_and_monotone_in_magnitude() {
        let scale = 2.0;
        let mut prev = ofi_toxicity(0.0, scale);
        for ofi in [0.5, 1.0, 2.0, 4.0, 8.0, 100.0] {
            let v = ofi_toxicity(ofi, scale);
            assert!((0.0..=1.0).contains(&v), "tox {v} out of [0,1] at ofi={ofi}");
            assert!(v > prev, "must be strictly increasing in |ofi|: {v} !> {prev}");
            prev = v;
        }
        // saturates toward 1 for a large magnitude.
        assert!(ofi_toxicity(100.0, scale) > 0.99);
    }

    #[test]
    fn ofi_toxicity_symmetric_in_sign_and_063_at_scale() {
        // depends on |ofi| only ⇒ symmetric in the sign (the caller derives the toxic side).
        assert_eq!(ofi_toxicity(3.0, 2.0).to_bits(), ofi_toxicity(-3.0, 2.0).to_bits());
        // ~0.63 (= 1 − 1/e) at |ofi| = scale.
        assert!((ofi_toxicity(2.0, 2.0) - (1.0 - std::f64::consts::E.recip())).abs() < 1e-12);
    }
}
