//! The corpus: every covered function folded into one row, and the one table they are pinned to.

use super::harness::{EV_SAMPLES, Row, SAMPLES, finish, lcg, or_nan};
use vike_marketdata::OptionKind;
use vike_options::{
    Basket, EvGrid, Leg, TerminalDist, black_scholes_greeks, black_scholes_price, implied_vol,
};

// =================================================================================================
// The corpus.
// =================================================================================================

/// Every covered function, folded over its own corpus.
///
/// Returns rows in a FIXED order; [`PLATFORM_INVARIANT`] is compared positionally, so a reordering
/// is a re-record rather than a silent re-pairing.
pub(super) fn production_rows() -> Vec<Row> {
    let mut out = Vec::new();

    // A shared option-parameter domain: near-the-money to deep wings, a week to two years, 5% to
    // 200% vol, and rates either side of zero (a negative rate is a real quote on some underlyings
    // and it flips the sign inside the `e^{-rt}` discount).
    fn params(s: &mut u64) -> (f64, f64, f64, f64, f64, OptionKind) {
        let spot = 20.0 + lcg(s) * 120_000.0;
        let strike = spot * (0.4 + lcg(s) * 1.4);
        let t = 0.02 + lcg(s) * 2.0;
        let sigma = 0.05 + lcg(s) * 1.95;
        let r = -0.02 + lcg(s) * 0.12;
        let kind = if lcg(s) < 0.5 { OptionKind::Call } else { OptionKind::Put };
        (spot, strike, t, sigma, r, kind)
    }

    // --- greeks::black_scholes_price — `libm::log` (in d1/d2) + `libm::exp` (the discount) +
    // `libm::erf` (norm_cdf, twice).
    {
        let mut s = 0x1234_5678_9abc_def0u64;
        let mut vals = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let (sp, k, t, sig, r, kind) = params(&mut s);
            vals.push(or_nan(black_scholes_price(sp, k, t, sig, kind, r)));
        }
        out.push(finish("greeks::black_scholes_price (log + exp + erf)", &vals));
    }

    // --- greeks::black_scholes_greeks — the same log, TWO `libm::exp` (norm_pdf and the theta
    // discount) and `libm::erf`. All four components are hashed: delta comes from `norm_cdf` while
    // gamma/vega come from `norm_pdf`, so a row that hashed only one would cover only one of the
    // two helpers this whole crate rests on.
    {
        let mut s = 0x0fed_cba9_8765_4321u64;
        let mut vals = Vec::with_capacity(SAMPLES * 4);
        for _ in 0..SAMPLES {
            let (sp, k, t, sig, r, kind) = params(&mut s);
            match black_scholes_greeks(sp, k, t, sig, kind, r) {
                Some((d, g, th, v)) => {
                    vals.push(d);
                    vals.push(g);
                    vals.push(th);
                    vals.push(v);
                }
                None => vals.push(f64::NAN),
            }
        }
        out.push(finish("greeks::black_scholes_greeks (log + exp x2 + erf)", &vals));
    }

    // --- greeks::implied_vol — up to 64 `black_scholes_price` evaluations per solve, and the ONE
    // site in this crate where a last bit can amplify (a flipped `(pm - price).abs() < 1e-6` exit
    // returns a neighbouring `mid`).
    //
    // ⚠ The probe price is DERIVED from a forward `black_scholes_price` call rather than drawn at
    // random, and that is what makes the row non-vacuous. `implied_vol` returns `None` unless
    // `p_lo <= price <= p_hi` — the no-arbitrage band at sigma in `[1e-4, 5.0]` — so a random price
    // is almost always outside it, and a corpus of random prices would be a wall of NaNs: stable on
    // every platform, and pinning nothing. Round-tripping a real price keeps every sample inside
    // the band by construction and makes the row a genuine test of the SEARCH.
    //
    // ⚠ This row uses a NARROWER domain than the two above, and the reason is the same one: a deep
    // wing with a small `sigma·sqrt(t)` prices to EXACTLY 0.0 (`norm_cdf(-83)` underflows), and
    // `implied_vol` refuses a non-positive price on its first line. Such a probe is not a hard case
    // for the bisection — it is not a case for the bisection at all, and a corpus full of them
    // would dilute the row with samples that cannot report anything. The moneyness is held inside
    // ±40% and `sigma`/`t` away from their floors so essentially every probe reaches the search.
    // The WIDE domain still gets exercised: `black_scholes_price` is what produces these probes,
    // and its own row above sweeps the full range.
    {
        let mut s = 0x2222_3333_4444_5555u64;
        let mut vals = Vec::with_capacity(SAMPLES);
        let mut solved = 0usize;
        for _ in 0..SAMPLES {
            let sp = 20.0 + lcg(&mut s) * 120_000.0;
            let k = sp * (0.6 + lcg(&mut s) * 0.8);
            let t = 0.08 + lcg(&mut s) * 1.9;
            let sig = 0.10 + lcg(&mut s) * 1.4;
            let r = -0.02 + lcg(&mut s) * 0.12;
            let kind = if lcg(&mut s) < 0.5 { OptionKind::Call } else { OptionKind::Put };
            let price = or_nan(black_scholes_price(sp, k, t, sig, kind, r));
            let iv = if price.is_finite() && price > 0.0 {
                implied_vol(price, sp, k, t, kind, r)
            } else {
                None
            };
            if iv.is_some() {
                solved += 1;
            }
            vals.push(or_nan(iv));
        }
        assert!(
            solved * 2 > SAMPLES,
            "only {solved}/{SAMPLES} implied-vol probes solved — a corpus dominated by `None` \
             hashes a wall of canonical NaNs, which is identical on every platform and pins \
             nothing. Check the price round-trip before re-recording."
        );
        out.push(finish("greeks::implied_vol (bisection over price)", &vals));
    }

    // A three-leg basket with two distinct strikes, so `expected_value`'s window is split at real
    // kinks and `probability_of_profit` has more than one break-even to walk.
    fn basket(s: &mut u64) -> (Basket, TerminalDist) {
        let spot = 50.0 + lcg(s) * 4_000.0;
        let k1 = spot * (0.75 + lcg(s) * 0.2);
        let k2 = spot * (1.05 + lcg(s) * 0.2);
        let b = Basket::new(
            vec![
                Leg::new(1.0, k1, OptionKind::Call, spot * 0.03 * (0.5 + lcg(s))),
                Leg::new(-2.0, k2, OptionKind::Call, spot * 0.01 * (0.5 + lcg(s))),
                Leg::new(1.0, k1, OptionKind::Put, spot * 0.02 * (0.5 + lcg(s))),
            ],
            spot,
        );
        let dist = TerminalDist {
            spot,
            sigma: 0.08 + lcg(s) * 1.2,
            tau: 0.02 + lcg(s) * 2.0,
            r: -0.02 + lcg(s) * 0.12,
        };
        (b, dist)
    }

    // --- basket::probability_of_profit — `libm::log` in `log_params` plus one `libm::log` and one
    // `libm::erf` per `cdf` call, several per basket.
    {
        let mut s = 0x5555_6666_7777_8888u64;
        let mut vals = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let (b, dist) = basket(&mut s);
            vals.push(or_nan(b.probability_of_profit(&dist)));
        }
        out.push(finish("basket::probability_of_profit (log x2 + erf)", &vals));
    }

    // --- basket::expected_value — `libm::log` for the window and `libm::exp` at EVERY quadrature
    // node. The single value in this crate that depends on the most transcendental calls.
    //
    // ⚠ The grid is deliberately COARSE (`steps: 64` against the default 4096, `n_sigma: 6.0`
    // against 10.0) and the sample count small — see `EV_SAMPLES`. That does NOT weaken the pin:
    // the question is whether both boxes perform the same arithmetic, and 65 nodes per panel is
    // already 65 `exp` calls per sample. It does mean this row must not be read as a statement
    // about the quadrature's CONVERGENCE, which is what `EvGrid`'s own doc and this crate's EV
    // tests are for.
    {
        let mut s = 0x9999_aaaa_bbbb_ccccu64;
        let grid = EvGrid { n_sigma: 6.0, steps: 64 };
        let mut vals = Vec::with_capacity(EV_SAMPLES);
        for _ in 0..EV_SAMPLES {
            let (b, dist) = basket(&mut s);
            vals.push(or_nan(b.expected_value(&dist, &grid)));
        }
        out.push(finish("basket::expected_value (log + exp per node)", &vals));
    }

    out
}

/// ⚠ **ONE set of constants for EVERY platform — that is the entire claim.**
///
/// ⚠ **Every hash below is the literal `"RECORD"`, a PLACEHOLDER that fails by construction, and
/// that is deliberate.** The convention is the analytics twin's
/// (`crates/vike-analytics/tests/libm_platform_probe.rs`'s `PLATFORM_INVARIANT`): rows are
/// sometimes written on a box that cannot run cargo, and a plausible-looking hex literal invented
/// there is INDISTINGUISHABLE from a recorded one while pinning nothing. `"RECORD"` cannot match
/// any FNV output, so [`converted_functions_are_platform_invariant`] stays red until a human RUNS
/// this file on Linux/glibc AND on Windows/MSVC, confirms the two runs agree bit-for-bit, and
/// pastes the agreed hash in. **A red row spelled `"RECORD"` is an UNFINISHED RECORDING, not a
/// regression.**
///
/// ⚠ **Re-record by RUNNING, never by editing a digit**, and never from ONE box.
///
/// ⚠ **A per-platform table (`#[cfg(windows)]` / `#[cfg(unix)]`) is NOT a repair and must never be
/// added.**
///
/// ⚠ **A red row here is NOT automatically a re-record, and this crate is the one where that
/// matters most.** `docs/decisions/0021` keeps every frozen fixture's comparison EXACT, and
/// `crates/vike-options/tests/oracle_parity.rs` is such a fixture set. If a row here moves AND
/// `oracle_parity` stays green, the change is inside that suite's `1e-12` tier and this table is
/// the only thing that saw it. If BOTH move, 0021 governs — read it before touching either.
pub(super) const PLATFORM_INVARIANT: [(&str, &str); 5] = [
    ("greeks::black_scholes_price (log + exp + erf)", "e08bbb78440b980f"),
    ("greeks::black_scholes_greeks (log + exp x2 + erf)", "5be39ee5680c1aaa"),
    ("greeks::implied_vol (bisection over price)", "63e57bc1f80c9b06"),
    ("basket::probability_of_profit (log x2 + erf)", "ed9797fcbb81bc91"),
    ("basket::expected_value (log + exp per node)", "84ba698dcf42a020"),
];
