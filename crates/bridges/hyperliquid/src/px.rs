//! Hyperliquid price/size **wire formatting** — the precision rules that reject-magnet every
//! adapter (CCXT #26132/#23516/#25457/#25539). Pure functions, property-tested; no I/O, no `unsafe`.
//!
//! Ports the wire-level ground truth cross-validated in
//! `docs/research/2026-07-16-hyperliquid-adapters/README.md` §3 (`float_to_wire`) + §4 (the
//! sig-fig→decimal price clamp). The oracle for §3 is the official Python SDK
//! `signing.py::float_to_wire`; for §4 it is CCXT's `price_to_precision` — the `max(5,
//! integer_digits)` sig-fig → `MAX_DECIMALS - szDecimals` decimal two-stage clamp, NOT Hummingbot's
//! `{:.5g}` heuristic (which mis-rounds integer prices > 99999 and over-tightens spot — research §4).
//!
//! The emitted string is the SAME string that gets msgpacked into the signed L1 action hash
//! (research §2a/§3), so the JSON body and the signed bytes can never diverge — which is why it must
//! be canonical: no trailing zeros (`"100.0"` would flip the signature), no `-0`, no scientific
//! notation, integers bare (`"92572"`). `rust_decimal` is the one exact-arithmetic site here, the
//! twin of `vike_bridge_core::format::format_to_step` (the Binance wire site).
//!
//! - [`float_to_wire`] — `format!("{:.8}")`, then **reject if the 8-dp round-trip shifts the value
//!   ≥ `1e-12`** (the guard the Python reference carries and the official Rust SDK omits — §3),
//!   then strip trailing zeros via `Decimal::normalize` (no exponent form, `-0`→`0`).
//! - [`clamp_price`] — HL is tickless: round to `max(5, integer_digits)` significant figures, THEN
//!   to `MAX_DECIMALS - szDecimals` decimals (`MAX_DECIMALS` = 8 spot / 6 perp; [`crate::consts`]).
//!   Integer prices are always valid — the `max(5, …)` is exactly what keeps `123456` (6 sig figs)
//!   legal despite the 5-sig-fig cap.
//! - [`round_size`] — floor (truncate toward zero) to `szDecimals`; a rounded size never
//!   oversizes. Fenced by [`grid_multiple_image`], because a truncation is an AMPLIFIER as well as
//!   a floor: a size ALREADY snapped onto the lot grid can reach it one f64 ULP below that grid
//!   point, where the floor costs a WHOLE LOT rather than a last bit — inside the signed hash.

use rust_decimal::{RoundingStrategy, prelude::*};

use crate::consts::{PERP_MAX_DECIMALS, PRICE_MAX_SIG_FIGS, SPOT_MAX_DECIMALS};

/// Why a value cannot be placed on the wire. Returned only by [`float_to_wire`]; [`clamp_price`] and
/// [`round_size`] are total over finite non-negative inputs (clamping/flooring always succeeds) and
/// return a `String` directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PxError {
    /// The fixed 8-decimal wire form rounds the value by ≥ `1e-12`, so the JSON body and the signed
    /// msgpack bytes would encode a *different* number than the caller intended. Carries the
    /// offending input. Ported from Python `signing.py`; the official Rust SDK omits this guard
    /// (research §3) — we keep it so an unrepresentable price is rejected, never silently corrupted.
    Rounding(f64),
    /// Input was NaN or ±∞ — never a valid price or size.
    NotFinite(f64),
}

impl std::fmt::Display for PxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PxError::Rounding(x) => {
                write!(
                    f,
                    "float_to_wire: 8-dp wire form shifts {x} by >= 1e-12 (not representable)"
                )
            }
            PxError::NotFinite(x) => write!(f, "float_to_wire: non-finite value {x}"),
        }
    }
}

impl std::error::Error for PxError {}

/// Format a float as Hyperliquid's canonical decimal wire string, or reject it.
///
/// `format!("{x:.8}")` (the spot/perp floor of 8 decimals), reject if that 8-dp form round-trips
/// ≥ `1e-12` away from `x`, then strip trailing zeros via `Decimal::normalize` (non-exponent form,
/// `-0`→`0`). Faithful to Python `signing.py::float_to_wire` (research §3). Used for BOTH price and
/// size when the caller already holds an exact, in-grid value.
pub fn float_to_wire(x: f64) -> Result<String, PxError> {
    if !x.is_finite() {
        return Err(PxError::NotFinite(x));
    }
    // Fixed 8-decimal form (Python `f"{x:.8f}"`). Reject if it corrupts the value: the wire string
    // is what gets signed, so a silent round here would sign a number the user never asked for.
    let rounded = format!("{x:.8}");
    let parsed = rounded.parse::<f64>().map_err(|_| PxError::NotFinite(x))?;
    if (parsed - x).abs() >= 1e-12 {
        return Err(PxError::Rounding(x));
    }
    // Strip trailing zeros / canonicalise -0. `Decimal::from_str` only fails for magnitudes past
    // Decimal's 28-digit range (far outside any price/size) — treat that as non-representable too.
    let dec = Decimal::from_str(&rounded).map_err(|_| PxError::Rounding(x))?;
    Ok(normalize_wire(dec))
}

/// Clamp a price to Hyperliquid's tickless precision rule and return the canonical wire string.
///
/// Two stages (CCXT `price_to_precision`, research §4): **(1)** round to `max(5, integer_digits)`
/// significant figures — the `max` is what makes integer prices like `123456` legal despite > 5 sig
/// figs, and forces any > 5-sig-fig non-integer (illegal) up to an integer; **(2)** round to
/// `MAX_DECIMALS - szDecimals` decimal places (`MAX_DECIMALS` = 8 spot / 6 perp). Both stages round
/// half away from zero. Non-finite / non-positive input → `"0"` (the exec/risk layer never submits
/// such a price; this is purely defensive).
///
/// ⚠ **IMMUNE to the one-ULP grid loss [`grid_multiple_image`] fences, and deliberately left
/// alone** — MEASURED 0 losses in 2000 grid multiples at every `allowed_decimals` in `0..=8`,
/// where the size path lost 602 and 578 at two of its rungs. The mechanism is the strategy: both
/// stages round `MidpointAwayFromZero`, so a tick point that arrives one ULP low is rounded back
/// ONTO the grid rather than truncated off it. Do not "finish the job" by extending the witness
/// here; a fence over a function that is already exact can only move prices.
pub fn clamp_price(px: f64, sz_decimals: u32, is_spot: bool) -> String {
    if !px.is_finite() || px <= 0.0 {
        return "0".to_string();
    }
    let max_decimals = if is_spot { SPOT_MAX_DECIMALS } else { PERP_MAX_DECIMALS };
    let allowed_decimals = max_decimals.saturating_sub(sz_decimals);

    // Parse via the shortest round-trip string (Rust `Display` for f64 never uses exponent form) so
    // the Decimal reflects the number the caller *meant* (`0.0012345`), not its f64 noise
    // (`0.00123449999…`) — the latter would round the wrong way at an exact midpoint. This is the
    // entry point CCXT reaches through `number_to_string`.
    let dec = match Decimal::from_str(&format!("{px}")) {
        Ok(d) if !d.is_zero() => d,
        _ => return "0".to_string(),
    };

    // e = floor(log10(px)), computed EXACTLY on the Decimal (an f64 `log10` is off-by-one at exact
    // powers of ten). integer_digits = e + 1 for px ≥ 1; for px < 1, e + 1 ≤ 0 so max(5, …) = 5.
    let ten = Decimal::from(10u32);
    let mut e = 0i32;
    if dec >= Decimal::ONE {
        let mut pow = ten; // 10^(e+1)
        while dec >= pow {
            e += 1;
            pow *= ten;
        }
    } else {
        let mut pow = Decimal::ONE; // 10^(e+1)
        while dec < pow {
            e -= 1;
            pow /= ten;
        }
    }

    // Stage 1 — significant figures. Keeping `max(5, e+1)` sig figs of a number whose leading digit
    // sits at 10^e means rounding to `max(5, e+1) - 1 - e = max((SIG-1) - e, 0)` decimal places
    // (for e ≥ 4, i.e. integer_digits ≥ 5, this is 0 dp — an integer, always valid).
    let dp_for_sig = ((PRICE_MAX_SIG_FIGS as i32 - 1) - e).max(0) as u32;
    let sig_rounded =
        dec.round_dp_with_strategy(dp_for_sig, RoundingStrategy::MidpointAwayFromZero);

    // Stage 2 — decimal-place clamp: ≤ MAX_DECIMALS - szDecimals decimals.
    let clamped = sig_rounded
        .round_dp_with_strategy(allowed_decimals, RoundingStrategy::MidpointAwayFromZero);

    normalize_wire(clamped)
}

/// The largest f64 that is still an exact integer count — `2^53`. Above it consecutive integers
/// are not representable, so "which multiple of the lot step is this" has no f64 answer and
/// [`grid_multiple_image`] declines rather than guessing. The twin of the constant of the same name
/// in `crates/vike-bridge-core/src/format.rs`, whose `grid_multiple_image` this one is ported from.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// The largest `szDecimals` [`grid_multiple_image`] will answer for — the SMALLER of two
/// independent limits rather than a taste:
///
/// * `rust_decimal` caps a Decimal's scale at 28, so past that there is no exact `n · 10⁻ᵈ` left
///   for the witness to return at all; and
/// * `crates/bridges/hyperliquid/src/instruments.rs`'s `pow10_neg` is platform-portable only while
///   `10ᵈ` is itself exactly representable in f64, i.e. `d ≤ 22` — the measured domain in
///   `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`.
///
/// The venue's own ceiling is [`SPOT_MAX_DECIMALS`], an order of magnitude inside this, so no real
/// market approaches it. The bound exists because [`round_size`] is a `pub fn` taking a bare `u32`:
/// a value large enough to overflow `pow10_neg`'s `-(n as i32)` has to be turned away BEFORE that
/// function sees it, and declining is the right answer for one that large anyway.
const MAX_GRID_WITNESS_DECIMALS: u32 = 22;

/// ⚠ A WITNESS, not a tolerance. Returns the EXACT decimal `n · 10⁻ᵈ` when `sz` is BIT-EXACTLY the
/// f64 image of an integer multiple of the `szDecimals` lot grid; `None` otherwise, and
/// [`round_size`] then truncates the value's own shortest-round-trip decimal exactly as before.
///
/// # The defect it fences
///
/// `crates/vike-model/src/scalar.rs`'s `round_to_step` — the snap every outgoing quantity goes
/// through, reached from `crates/vike-exec/src/risk.rs`'s `RiskGate` — ends in a MULTIPLICATION,
/// `n · step`, and that product is not always the f64 nearest the decimal grid point. For a step
/// whose own f64 image sits BELOW its decimal it can land one ULP under, and the
/// shortest-round-trip decimal of THAT float is then below `n · 10⁻ᵈ` — so [`round_size`]'s
/// `ToZero` truncation reads a ratio of `n - ε` and emits `n - 1`. **The error is not one ULP, it
/// is a WHOLE LOT**, and on this venue that lot is inside the SIGNED L1 action hash rather than
/// merely on the wire.
///
/// MEASURED over 2000 multiples per rung, the same sweep that produced the twin's recording:
/// `szDecimals` 6 lost 602 (first at `n = 5`) and `szDecimals` 7 lost 578 (first at `n = 13`);
/// every other rung in `0..=8` lost none. The identical measurement at the Binance wire site, and
/// the ULP histogram that locates the boundary, are in
/// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`.
///
/// ⚠ It is DORMANT: no hyperliquid market, core or across the HIP-3 builder dexs, declares
/// `szDecimals` 6 or 7 today. Nothing prevents one tomorrow and no gate would notice, which is the
/// whole argument for closing it while it costs nothing.
///
/// # Why a witness and not a tolerance
///
/// If `n · step` reproduces `sz` bit-for-bit there is no SECOND intent an f64 could be carrying, so
/// emitting the exact decimal `n · 10⁻ᵈ` is not a guess. A tolerance was considered and rejected at
/// the twin site for a concrete reason: `fixtures/r6/format.json` freezes one instance of this
/// defect, and an honestly-derived bound (`2·f64::EPSILON`) would have moved eleven of its
/// seventeen rows.
///
/// # Why the step comes from `pow10_neg` rather than being re-spelled here
///
/// The witness has to ask its question against the SAME `10⁻ᵈ` the grid was built from —
/// `crates/bridges/hyperliquid/src/instruments.rs`'s `pow10_neg`, which is what `properties_for`
/// writes into `SymbolProperties::step_size` and therefore what `round_to_step` multiplied by. A
/// second spelling disagreeing by one bit would make this decline exactly where it is needed.
///
/// It could never make it emit a WRONG number, and that asymmetry is worth stating: `n` is derived
/// from `sz / step`, so `n · 10⁻ᵈ` is within one ULP of `sz` whatever the step turns out to be. A
/// wrong step costs HITS, never correctness.
fn grid_multiple_image(sz: f64, sz_decimals: u32) -> Option<Decimal> {
    if sz_decimals > MAX_GRID_WITNESS_DECIMALS || !sz.is_finite() || sz <= 0.0 {
        return None;
    }
    let step = crate::instruments::pow10_neg(sz_decimals);
    let n = (sz / step).round_ties_even();
    if !(1.0..=MAX_EXACT_INT).contains(&n) {
        return None;
    }
    if n * step != sz {
        return None;
    }
    Decimal::try_new(n as i64, sz_decimals).ok()
}

/// Floor a size to `szDecimals` and return the canonical wire string.
///
/// Sizes round DOWN (truncate toward zero) so a rounded size never exceeds what the caller holds
/// (research §4: "sizes round to szDecimals"; flooring, not rounding, keeps us from oversizing an
/// order past available balance/position). Non-finite / non-positive → `"0"`.
///
/// ⚠ **The truncation is an AMPLIFIER as well as a floor, and [`grid_multiple_image`] is the
/// fence.** A size that is already a whole number of lots can arrive one f64 ULP below its grid
/// point, where this floor drops a WHOLE LOT rather than a last bit — so a size the witness
/// recognises is emitted as the exact decimal `n · 10⁻ᵈ`, and everything else truncates
/// byte-for-byte as before. That is not a relaxation of the never-oversize rule: the witnessed
/// answer differs from `sz` by under one ULP and IS the multiple the caller asked for, whereas the
/// unfenced answer was a full lot SHORT. Mechanism, measurement and the witness-not-tolerance
/// argument are on [`grid_multiple_image`].
pub fn round_size(sz: f64, sz_decimals: u32) -> String {
    if !sz.is_finite() || sz <= 0.0 {
        return "0".to_string();
    }
    let dec = match grid_multiple_image(sz, sz_decimals) {
        Some(exact) => exact,
        None => {
            let Ok(from_image) = Decimal::from_str(&format!("{sz}")) else {
                return "0".to_string();
            };
            from_image
        }
    };
    // ToZero == truncate == floor for the non-negative sizes reaching here. A witnessed multiple
    // already carries scale `sz_decimals`, so this is an identity on that arm — the tail is SHARED
    // rather than short-circuited, so the two arms cannot drift about what a wire size looks like.
    let floored = dec.round_dp_with_strategy(sz_decimals, RoundingStrategy::ToZero);
    normalize_wire(floored)
}

/// Canonical wire form of an already-precise Decimal: strip trailing zeros (`Decimal::normalize`,
/// which never emits exponent notation) and collapse any signed/scaled zero (`-0`, `0.00000000`) to
/// a bare `"0"`. The shared tail of all three formatters.
fn normalize_wire(d: Decimal) -> String {
    let n = d.normalize();
    if n.is_zero() {
        // covers "0.00000000", "-0", "-0.0" → the one canonical zero.
        "0".to_string()
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- float_to_wire: exact canonical forms -------------------------------------------------

    #[test]
    fn wire_strips_trailing_zeros() {
        assert_eq!(float_to_wire(0.00076000).unwrap(), "0.00076");
        assert_eq!(float_to_wire(987654321.0).unwrap(), "987654321");
        assert_eq!(float_to_wire(92572.0).unwrap(), "92572");
    }

    #[test]
    fn wire_integers_render_bare() {
        // "100.0"/"2000.0" must NEVER leave the client — they would flip the signed action hash.
        assert_eq!(float_to_wire(100.0).unwrap(), "100");
        assert_eq!(float_to_wire(2000.0).unwrap(), "2000");
        assert_eq!(float_to_wire(1.0).unwrap(), "1");
        assert_eq!(float_to_wire(0.0).unwrap(), "0");
    }

    #[test]
    fn wire_negative_zero_maps_to_zero() {
        assert_eq!(float_to_wire(-0.0).unwrap(), "0");
    }

    #[test]
    fn wire_keeps_fractional_significance() {
        assert_eq!(float_to_wire(3.5).unwrap(), "3.5");
        assert_eq!(float_to_wire(0.12345678).unwrap(), "0.12345678"); // exactly 8 dp — the floor
        assert_eq!(float_to_wire(0.00000001).unwrap(), "0.00000001"); // 1e-8, smallest wire step
        assert_eq!(float_to_wire(12345.678).unwrap(), "12345.678");
        assert_eq!(float_to_wire(-2.5).unwrap(), "-2.5");
    }

    // ---- float_to_wire: the reject guard (Python's, which the official Rust SDK omits) ---------

    #[test]
    fn wire_rejects_sub_8dp_significance() {
        // A 9th decimal is load-bearing ⇒ the 8-dp form shifts the value by ≥ 1e-12.
        assert!(matches!(float_to_wire(0.123456789), Err(PxError::Rounding(_))));
        assert!(matches!(float_to_wire(1.000000001), Err(PxError::Rounding(_))));
    }

    #[test]
    fn wire_rejects_below_wire_resolution() {
        // 1e-9 rounds to "0.00000000" — a silent loss of the entire value.
        assert!(matches!(float_to_wire(1e-9), Err(PxError::Rounding(_))));
        assert!(matches!(float_to_wire(5e-10), Err(PxError::Rounding(_))));
    }

    #[test]
    fn wire_rejects_non_finite() {
        assert!(matches!(float_to_wire(f64::NAN), Err(PxError::NotFinite(_))));
        assert!(matches!(float_to_wire(f64::INFINITY), Err(PxError::NotFinite(_))));
        assert!(matches!(float_to_wire(f64::NEG_INFINITY), Err(PxError::NotFinite(_))));
    }

    // ---- clamp_price: the exact HL-doc examples (research §4) ----------------------------------

    #[test]
    fn clamp_hl_doc_sig_fig_examples() {
        // perp, szDecimals=0 ⇒ ≤ 6 decimals available; the 5-sig-fig rule is what bites.
        assert_eq!(clamp_price(1234.5, 0, false), "1234.5"); // 5 sig figs — valid
        assert_eq!(clamp_price(1234.56, 0, false), "1234.6"); // 6 sig figs — clamps
        assert_eq!(clamp_price(0.001234, 0, false), "0.001234"); // valid
        assert_eq!(clamp_price(0.0012345, 0, false), "0.001235"); // clamps (exact 6-dp midpoint, up)
    }

    #[test]
    fn clamp_hl_doc_szdecimals_example() {
        // szDecimals=1 (perp) ⇒ ≤ 5 decimals; now the decimal-place rule is the binding one.
        assert_eq!(clamp_price(0.01234, 1, false), "0.01234"); // 5 decimals — valid
        assert_eq!(clamp_price(0.012345, 1, false), "0.01235"); // 6 decimals — clamps
    }

    #[test]
    fn clamp_integer_prices_always_valid() {
        // max(5, integer_digits) keeps > 5-sig-fig INTEGERS legal (the reject-magnet §4 landmine).
        assert_eq!(clamp_price(123456.0, 0, false), "123456");
        assert_eq!(clamp_price(999999.0, 3, false), "999999");
        assert_eq!(clamp_price(100000.0, 0, true), "100000");
        assert_eq!(clamp_price(5.0, 0, false), "5");
    }

    #[test]
    fn clamp_rounds_large_noninteger_to_integer() {
        // A > 5-sig-fig NON-integer is illegal ⇒ stage 1 rounds it to 0 dp (a valid integer).
        assert_eq!(clamp_price(123456.7, 0, false), "123457");
        assert_eq!(clamp_price(99999.9, 0, false), "100000");
    }

    #[test]
    fn clamp_spot_uses_eight_max_decimals() {
        // spot MAX_DECIMALS=8: a 6-sig-fig value clamps to 5 sig figs even with room for 8 dp.
        assert_eq!(clamp_price(0.00123456, 0, true), "0.0012346");
        // szDecimals=2 spot ⇒ 6 dp — same budget as perp szDecimals=0.
        assert_eq!(clamp_price(0.0012345, 2, true), "0.001235");
    }

    #[test]
    fn clamp_perp_vs_spot_decimal_budget_differs() {
        // Same input, szDecimals=0: perp caps at 6 dp, spot at 8 dp (after the 5-sig-fig round).
        assert_eq!(clamp_price(0.0001234567, 0, false), "0.000123"); // perp: 6 dp
        assert_eq!(clamp_price(0.0001234567, 0, true), "0.00012346"); // spot: 8 dp
    }

    #[test]
    fn clamp_bad_input_is_zero() {
        assert_eq!(clamp_price(f64::NAN, 0, false), "0");
        assert_eq!(clamp_price(-1.0, 0, false), "0");
        assert_eq!(clamp_price(0.0, 0, false), "0");
        assert_eq!(clamp_price(f64::INFINITY, 0, false), "0");
    }

    // ---- round_size: floor to szDecimals -------------------------------------------------------

    #[test]
    fn size_floors_never_oversizes() {
        assert_eq!(round_size(1.239, 2), "1.23"); // floored, not rounded up to 1.24
        assert_eq!(round_size(0.999999, 3), "0.999");
        assert_eq!(round_size(1.2, 0), "1"); // floor to integer
        assert_eq!(round_size(5.0, 3), "5");
        assert_eq!(round_size(1.0, 2), "1");
        assert_eq!(round_size(12.34999, 2), "12.34");
        assert_eq!(round_size(1.123456789, 8), "1.12345678"); // spot-grade 8 dp
    }

    #[test]
    fn size_below_step_is_zero() {
        assert_eq!(round_size(0.0001, 2), "0"); // floored under the step
        assert_eq!(round_size(0.4, 0), "0");
    }

    #[test]
    fn size_bad_input_is_zero() {
        assert_eq!(round_size(f64::NAN, 2), "0");
        assert_eq!(round_size(-2.0, 2), "0");
        assert_eq!(round_size(0.0, 2), "0");
    }

    // ---- round_size: a grid multiple must survive the wire (the ULP lot-loss fence) ------------

    /// Grid multiples swept per rung: `n` in `1..=MULTIPLES`.
    ///
    /// 2000 is the recording's own sample count, kept so a red table can be read straight against
    /// [`grid_multiple_image`]'s. It is also well past the deepest recorded first loss (`n = 13`).
    const MULTIPLES: i64 = 2000;

    /// The canonical wire spelling of the exact decimal `n · 10⁻ᵈ`, built by STRING SURGERY on the
    /// integer `n` — no `f64` and no `Decimal` anywhere in it.
    ///
    /// The expectation must not be produced by the machinery under test, or the law below would be
    /// satisfiable by any self-consistent bug: an `f64` expectation would carry the same
    /// multiplication that causes the defect, and a `Decimal` one would carry the same constructor
    /// the fence emits through.
    fn exact_multiple_wire(n: i64, d: u32) -> String {
        let digits = n.to_string();
        if d == 0 {
            return digits;
        }
        let width = d as usize + 1;
        let padded = format!("{digits:0>width$}");
        let (int_part, frac_part) = padded.split_at(padded.len() - d as usize);
        let frac = frac_part.trim_end_matches('0');
        if frac.is_empty() { int_part.to_string() } else { format!("{int_part}.{frac}") }
    }

    /// THE LAW, in the shape of
    /// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`: snap `n` lots onto the
    /// venue's `szDecimals` grid the way the live path does, format for the wire, and the multiple
    /// that goes out is the multiple that was asked for.
    ///
    /// ⚠ **RED WITHOUT [`grid_multiple_image`]**, at rungs 6 and 7 and nowhere else — the two
    /// `szDecimals` whose `10⁻ᵈ` has an f64 image BELOW its decimal, so `round_to_step`'s closing
    /// multiplication lands one ULP under the grid point and the floor eats a lot. The failure
    /// message carries the whole table, so one red run is the evidence rather than the start of a
    /// hunt.
    ///
    /// Two venue laws ride along inside the loop, because they cost nothing here and no other test
    /// asserts them over a sweep: the emitted string is canonical (the signed form), and it never
    /// carries more than `szDecimals` decimal places.
    #[test]
    fn a_grid_multiple_survives_round_to_step_then_round_size() {
        let mut report = String::from(
            "\nA whole number of lots did not come back off the wire. Each lost row is a WHOLE\n\
             LOT short on a live order - and on this venue, inside the SIGNED action hash.\n\
             The mechanism is on px::grid_multiple_image.\n\n\
             szDecimals   lost/2000   first n\n",
        );
        let mut total_lost = 0usize;
        for d in 0..=8u32 {
            // The step the GRID was built from — the one and only spelling of it.
            let step = crate::instruments::pow10_neg(d);
            let mut lost = 0usize;
            let mut first_n: Option<i64> = None;
            for n in 1..=MULTIPLES {
                // The REAL chain: `crates/vike-exec/src/risk.rs`'s `RiskGate` snaps through this
                // exact call, and `crates/bridges/hyperliquid/src/exec.rs`'s `build_order_wire`
                // then hands the result straight to `round_size`.
                let v = vike_model::round_to_step(n as f64 * step, step);
                let got = round_size(v, d);
                assert_canonical(&got);
                assert!(
                    frac_len(&got) <= d as usize,
                    "szDecimals={d} n={n}: {got} has more than {d} decimal places"
                );
                if got != exact_multiple_wire(n, d) {
                    lost += 1;
                    if first_n.is_none() {
                        first_n = Some(n);
                    }
                }
            }
            total_lost += lost;
            let first = first_n.map_or_else(|| "-".to_string(), |n| n.to_string());
            report.push_str(&format!("{d:>10}   {lost:>9}   {first:>7}\n"));
        }
        println!("{report}");
        assert_eq!(total_lost, 0, "{report}");
    }

    /// The worked example, pinned on its own so a red run names ONE number instead of a table:
    /// `5 · fl(1e-6)` is `0.0000049999999999999996`, whose shortest decimal image the `ToZero`
    /// floor read as four micro-lots — and then SIGNED — where five were ordered.
    #[test]
    fn the_worked_example_reaches_the_wire_whole() {
        let step = crate::instruments::pow10_neg(6);
        let v = vike_model::round_to_step(5.0 * step, step);
        assert_eq!(round_size(v, 6), "0.000005", "round_to_step gave {v}");
    }

    /// THE CONTROL, and it is what stops the law above from being satisfiable by "round every size
    /// up".
    ///
    /// A size the caller genuinely meant mid-step — nowhere near a grid point — must STILL
    /// truncate toward zero, at the two fenced rungs as much as anywhere else. A cure that moved
    /// any of these has overshot, which is exactly what [`round_size`]'s never-oversize rule
    /// forbids; every row here passes before the fence as well as after it.
    #[test]
    fn a_genuine_mid_step_size_still_truncates() {
        assert_eq!(round_size(0.0000059, 6), "0.000005"); // a fenced rung, mid-step
        assert_eq!(round_size(0.00000059, 7), "0.0000005"); // the other one
        assert_eq!(round_size(1.239, 2), "1.23");
        assert_eq!(round_size(12.34999, 2), "12.34");
        assert_eq!(round_size(0.999999, 3), "0.999");
        assert_eq!(round_size(1.9, 0), "1");
    }

    /// ⚠ The SIGNING invariants the fence must not have moved.
    ///
    /// This string is msgpacked into the L1 action hash, so a changed spelling is not REJECTED by
    /// the venue the way a Binance precision error would be — it is SIGNED WRONG. A trailing zero
    /// (`"5.000"` where `"5"` was signed) or a `-0` flips the hash, which is why a test that only
    /// checked the defect would let a signature-breaking regression through: every row below takes
    /// the WITNESS arm, the arm that is new.
    #[test]
    fn the_witness_arm_still_emits_the_canonical_signed_form() {
        for (sz, d, want) in [
            (5.0_f64, 3u32, "5"), // trailing zeros stripped: never "5.000"
            (1.0, 2, "1"),
            (100.0, 0, "100"),
            (12.0, 1, "12"),
            (0.5, 1, "0.5"),
            (2.5, 2, "2.5"),
        ] {
            let out = round_size(sz, d);
            assert_eq!(out, want, "size {sz} at {d} dp");
            assert_canonical(&out);
        }
        // Zero and -0 collapse to the ONE canonical spelling, ahead of the witness being consulted
        // at all — the guard that does it is the function's first statement.
        assert_eq!(round_size(0.0, 6), "0");
        assert_eq!(round_size(-0.0, 6), "0");
        assert_eq!(round_size(-0.0, 0), "0");
        assert_eq!(round_size(-1.0, 6), "0");
    }

    /// The ONE `u32` whose negation actually overflows an `i32`, and therefore the only value that
    /// makes this test's own name true.
    ///
    /// `2^31 as i32` is `i32::MIN`, and `-i32::MIN` panics in a debug build — which every `cargo
    /// test` is. Neither `u32::MAX` (`as i32` → `-1`, negating to `1`) nor
    /// `MAX_GRID_WITNESS_DECIMALS + 1` reaches that: they would sail into `pow10_neg` and come
    /// back with a nonsense step rather than a crash, so a test built only from them would pass
    /// with the guard DELETED and prove nothing about it.
    const NEGATION_OVERFLOWS_I32: u32 = 1 << 31;

    /// The witness must DECLINE for a `szDecimals` past [`MAX_GRID_WITNESS_DECIMALS`] rather than
    /// reach `pow10_neg` with a `u32` whose negation overflows an `i32`, and `round_size` must go
    /// on answering there exactly as it did before the fence existed.
    #[test]
    fn an_absurd_szdecimals_declines_instead_of_overflowing() {
        // The load-bearing row: without the guard THIS one panics inside `pow10_neg`.
        assert!(grid_multiple_image(1.0, NEGATION_OVERFLOWS_I32).is_none());
        assert!(grid_multiple_image(1.0, MAX_GRID_WITNESS_DECIMALS + 1).is_none());
        assert!(grid_multiple_image(1.0, u32::MAX).is_none());
        // `round_dp_with_strategy` returns early when the value's scale is already <= dp, so the
        // unfenced path is an identity here — the same string it emitted before.
        assert_eq!(round_size(1.25, NEGATION_OVERFLOWS_I32), "1.25");
        assert_eq!(round_size(1.25, u32::MAX), "1.25");
        assert_eq!(round_size(1.25, MAX_GRID_WITNESS_DECIMALS + 1), "1.25");
    }

    // ---- test-only canonical-form + precision validators (pure string introspection) ----------

    fn frac_len(s: &str) -> usize {
        s.split_once('.').map_or(0, |(_, f)| f.len())
    }

    /// Significant figures of a CANONICAL non-integer wire string: it carries no trailing zeros, so
    /// sig figs = all digits minus leading zeros.
    fn sig_figs_nonint(s: &str) -> usize {
        s.chars().filter(|c| c.is_ascii_digit()).collect::<String>().trim_start_matches('0').len()
    }

    fn assert_canonical(s: &str) {
        assert!(!s.is_empty(), "empty wire string");
        assert!(!s.contains(['e', 'E']), "scientific notation: {s}");
        assert_ne!(s, "-0", "negative zero leaked");
        assert!(!s.starts_with('+'), "leading plus: {s}");
        assert!(!s.starts_with('.') && !s.starts_with("-."), "bare fraction: {s}");
        if s.contains('.') {
            assert!(!s.ends_with('0'), "trailing zero: {s}");
            assert!(!s.ends_with('.'), "dangling decimal point: {s}");
        }
        assert!(s.parse::<f64>().is_ok(), "unparseable wire string: {s}");
    }

    // ---- clamp_price: table-driven property across magnitudes [1e-4 … 1e6], perp AND spot -------

    #[test]
    fn clamp_property_across_magnitudes() {
        // bases chosen to avoid clippy::approx_constant (no PI/E/SQRT_2/… look-alikes).
        let bases = [1.0_f64, 1.23456789, 4.5, 9.87654321, 5.5, 2.0, 7.0, 8.125];
        for base in bases {
            for exp in -4i32..=6 {
                let px = base * 10f64.powi(exp);
                if !(1e-4..=1e7).contains(&px) {
                    continue;
                }
                for is_spot in [false, true] {
                    let max_dec = if is_spot { 8u32 } else { 6 };
                    for sz in [0u32, 1, 2, 3, 8] {
                        let allowed = max_dec.saturating_sub(sz);
                        let out = clamp_price(px, sz, is_spot);
                        assert_canonical(&out);
                        // (1) never more than MAX_DECIMALS - szDecimals decimal places
                        assert!(
                            frac_len(&out) <= allowed as usize,
                            "px={px} sz={sz} spot={is_spot}: {out} has > {allowed} dp"
                        );
                        // (2) integer (always valid) OR ≤ 5 significant figures
                        assert!(
                            !out.contains('.') || sig_figs_nonint(&out) <= 5,
                            "px={px} sz={sz} spot={is_spot}: {out} has > 5 sig figs"
                        );
                        // (3) clamping is idempotent — a value already in-grid is returned unchanged
                        let reparsed = out.parse::<f64>().unwrap();
                        assert_eq!(
                            clamp_price(reparsed, sz, is_spot),
                            out,
                            "px={px} sz={sz} spot={is_spot}: clamp not idempotent"
                        );
                    }
                }
            }
        }
    }

    // ---- float_to_wire: canonical + round-trip across magnitudes -------------------------------

    #[test]
    fn wire_property_canonical_and_round_trips() {
        let good = [
            0.0_f64,
            1.0,
            100.0,
            2000.0,
            92572.0,
            987654321.0,
            0.00076,
            0.5,
            3.5,
            -2.5,
            0.12345678,
            0.00000001,
            12345.678,
            0.1,
            0.25,
            64.32,
        ];
        for x in good {
            let s = float_to_wire(x).expect("representable");
            assert_canonical(&s);
            let back = s.parse::<f64>().unwrap();
            assert!((back - x).abs() < 1e-9, "round-trip {x} -> {s} -> {back}");
        }
        // Every power of ten in [1e-8 … 1e6] is exactly wire-representable and canonical.
        for exp in -8i32..=6 {
            let x = 10f64.powi(exp);
            let s = float_to_wire(x).expect("power of ten is representable");
            assert_canonical(&s);
            let back = s.parse::<f64>().unwrap();
            assert!((back - x).abs() < x.abs() * 1e-9 + 1e-15, "round-trip {x} -> {s} -> {back}");
        }
    }

    #[test]
    fn wire_and_clamp_agree_on_clean_prices() {
        // A price already within grid: clamp must equal the plain canonical wire form.
        for (px, sz, spot) in [(1234.5_f64, 0u32, false), (64.32, 2, true), (2.5, 1, false)] {
            assert_eq!(clamp_price(px, sz, spot), float_to_wire(px).unwrap());
        }
    }

    #[test]
    fn pxerror_display_is_informative() {
        assert!(format!("{}", PxError::Rounding(0.123456789)).contains("1e-12"));
        assert!(format!("{}", PxError::NotFinite(f64::NAN)).contains("non-finite"));
    }
}
