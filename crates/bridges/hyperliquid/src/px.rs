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
/// That condition is NECESSARY and not sufficient: `fl(10⁻ᵈ) >= 10⁻ᵈ` makes the rung provably
/// clean (the product is `>= n·10⁻ᵈ` in real arithmetic and rounding is monotone), while nothing
/// says how many multiples of an exposed rung drift. The set is therefore MEASURED.
///
/// # The exposed set, and why it is five rungs rather than two
///
/// ⚠ **This doc recorded `szDecimals` 6 and 7 and said "every other rung in `0..=8` lost none",
/// and that sweep stopped at 8 while this function ACCEPTS up to 22.**
/// [`MAX_GRID_WITNESS_DECIMALS`] admits `d` up to 22, and over that whole domain
/// `crates/bridges/hyperliquid/src/instruments.rs`'s `pow10_neg` returns exactly `fl(10⁻ᵈ)` — its
/// own doc is the authority for why (`10ᵈ` is exactly representable for `d <= 22`, so `10⁻ᵈ` is one
/// correctly-rounded division of exact operands).
///
/// MEASURED on the CI box, 2026-09-14, by driving THIS function over 2000 lot multiples at every
/// `d` in `0..=22`: the unwitnessed floor loses a whole lot at `szDecimals` **6** (602), **7**
/// (578), **11** (767 — the deepest loss rate measured anywhere in the workspace), **12** (269) and
/// **14** (16). FIVE rungs, not two. The twin measurement at the Binance wire site, and the exact
/// integer comparison that decides each rung's side, are in
/// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs` and
/// `crates/vike-bridge-core/tests/format_props.proptest-regressions`.
///
/// ⚠ **`d` past 14 reads ZERO and that is NOT a clean bill of health — it is the measurement
/// running out of room, and the distinction cost a wrong number in this very doc's first
/// correction.** In exact arithmetic the exposed set continues (16, 19, 20 and 21 are all BELOW
/// rungs), but a drifted size at those depths has a shortest-round-trip decimal image of ~31
/// digits, past `rust_decimal`'s 28-digit scale — so the image cannot be parsed at all and the
/// comparison never happens. What the UNWITNESSED path would do there is worse than losing a lot,
/// not better: [`round_size`]'s `Decimal::from_str` arm fails and it returns `"0"`. The witness
/// fires on every grid point at every rung (2000/2000 at all 23), so none of this is reachable
/// today; it is written down so the next reader does not read those zeros as coverage.
///
/// ⚠ **It is DORMANT, and the dormancy is now argued against the LIVE CEILING rather than against
/// two rungs.** MEASURED from the public `/info` endpoint on 2026-09-14: core perps top out at
/// `szDecimals` 5; all ten HIP-3 builder dexs top out at 5 as well, across 285 markets; spot
/// tokens reach 8, on one token, and 8 is a provably clean rung. The live set is {0,1,2,3,4,5,8}
/// and its intersection with the exposed set above is EMPTY. Nothing prevents a 6 or a 7 — or an
/// 11 — tomorrow and no gate would notice, which is the whole argument for closing it while it
/// costs nothing. What changed is the size of what is being closed: the old wording implied two
/// reachable rungs where five are measurable and more sit past the measurement's own ceiling.
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

#[path = "px_tests.rs"]
#[cfg(test)]
mod px_tests;
