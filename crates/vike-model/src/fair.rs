//! Bachelier-style up-probability of a lognormal spot over a bounded window — the shared fair-value
//! primitive. Ports `fair_value_bot/strategy.py:162` (`p_up_scalar`). Lives here in vike-model (not
//! vike-backtest, where the cheap_np strategy first defined it) so BOTH the offline backtest AND the
//! live Avellaneda–Stoikov maker call ONE definition — no duplication. Pure `f64` over `libm::erf`
//! (the same erf the vike-options greeks port pins bit-identical to CPython's `math.erf`); naïve fold.

/// Probability the window closes UP, under drift-sensitivity `beta`. Port of `strategy.py:162`
/// (`p_up_scalar`).
///
/// `s_now`/`s_open` are spot prices, `sigma` the per-second log-return stddev, `t` seconds into the
/// window and `h` the window length. The `max(h - t, 1e-9)` floor is the oracle's — it keeps the
/// denominator finite in the last instant of the window rather than dividing by zero.
/// ⚠ **This function was HALF-converted for weeks, and the half that was missing is the reason to
/// read the next paragraph rather than skim it.** The `erf` on the last line went through
/// `libm` in the original port; the logarithm ONE LINE ABOVE IT stayed `f64::ln`, i.e. the
/// PLATFORM's libm, which IEEE 754 does not require to be correctly rounded. A reader — and
/// `docs/decisions/0032`'s own site audit, which counted this file's `erf` and not its `ln` — saw
/// `libm::` in the body and stopped looking. A half-converted function reads as done, which is
/// what makes it worse than an unconverted one.
///
/// # What a last bit here actually costs — four quantisers, none of which absorbs it
///
/// 1. **A posted price, by a whole tick.** `crates/vike-mm/src/avellaneda.rs`'s `model_fair` ->
///    `blended_anchor` -> `as_quotes` -> `assemble_quote`, which ends in `snap_to_tick`
///    (`crates/vike-model/src/scalar.rs`'s `round_to_step`). On a 0.01 outcome grid that is 1% of
///    notional on a live resting order.
/// 2. **A quote into NO quote.** `assemble_quote`'s guard is the strict three-way
///    `bid < ask && bid < s && s < ask`. One ulp at the straddle boundary makes the maker rest
///    nothing that tick — coarser than any price move.
/// 3. **Which beta wins.** `crates/vike-backtest/src/fair_value.rs`'s `wc_betas` folds this
///    across the beta set on a strict `<` minimum.
/// 4. **The trade SET.** `cheap_gate` ends `(e > theta).then_some(e)`, and that gate's 1-ulp
///    sensitivity is already a PINNED property — `crates/vike-backtest/src/cheap_np.rs`'s own
///    test builds `f64::from_bits(e.to_bits() - 1)` to prove it.
///
/// # MEASURED 2026-08-26 before converting, because (4) could have made this a STOP
///
/// `crates/vike-backtest/tests/cheap_np_parity.rs` asserts EXACT integer counts against the
/// retired Python oracle — `after_band_tte`, `after_theta`, `entries` — and `p_up` feeds the
/// threshold that produces them. A moved bit could therefore have changed a COUNT, not a last
/// digit, which `docs/decisions/0021` governs rather than a re-record.
///
/// It did not. With this conversion applied, `sample_windows_reproduce_the_python_gate_entry_for_entry`
/// and `sample_fixture_holds_whole_windows` both pass unchanged over the committed 10,973-row
/// fixture, entry for entry.
///
/// ⚠ **DECLARED LIMIT, because the reassurance above is smaller than it looks.**
/// `full_reference_set_reproduces_every_published_number` — the 7.1M-row case, whose counts
/// (`785_619` / `681_403` / `12_858`) are asserted just as exactly — is `#[ignore]`d and reads
/// `.cheapnp_ref/signals.bin`, which this checkout does not have (only the `.parquet` sources,
/// and the converter is a Python script this workspace does not run). A one-ulp shift is far more
/// likely to flip one row of 7.1 million than one of 10,973, so that case is UNVERIFIED rather
/// than passing. If it is ever run and a count has moved, the answer is 0021, not a re-record.
#[inline]
pub fn p_up(beta: f64, s_now: f64, s_open: f64, sigma: f64, t: f64, h: f64) -> f64 {
    let denom = sigma * (h - t).max(1e-9).sqrt();
    // ⚠ `libm::log`, not `.ln()` — see the note above. `.sqrt()` on the line before stays as it
    // is: IEEE 754 requires sqrt to be correctly rounded, so it never diverged.
    let x = libm::log(s_now / s_open) * beta / denom;
    0.5 * (1.0 + libm::erf(x / std::f64::consts::SQRT_2))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Home coverage for the hoisted primitive (the cheap_np parity gate exercises the SAME fn from
    // the consumer side). These pin the two defining properties rather than re-pinning the oracle set.
    #[test]
    fn flat_spot_is_a_coin_flip() {
        // s_now == s_open ⇒ ln(1) = 0 ⇒ x = 0 ⇒ 0.5 exactly, for any β/σ/t/h.
        assert_eq!(p_up(0.83, 100.0, 100.0, 1e-4, 100.0, 300.0), 0.5);
        assert_eq!(p_up(2.0, 5.0, 5.0, 0.01, 0.0, 300.0), 0.5);
    }

    #[test]
    fn monotone_up_in_spot_and_capped_by_sigma() {
        // A higher spot than the open ⇒ p_up > 0.5, and monotone increasing in s_now.
        let up = p_up(0.83, 100.5, 100.0, 1e-4, 100.0, 300.0);
        let up_more = p_up(0.83, 101.0, 100.0, 1e-4, 100.0, 300.0);
        assert!(up > 0.5 && up_more > up);
        // A larger σ pulls the same favourite back toward the coin flip.
        let noisier = p_up(0.83, 100.5, 100.0, 1e-3, 100.0, 300.0);
        assert!(noisier < up && noisier > 0.5);
    }
}
