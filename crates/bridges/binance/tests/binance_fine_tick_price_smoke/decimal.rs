//! The exact-decimal integer arithmetic the fine-tick smoke judges a price with.

// ---------------------------------------------------------------------------------------------
// Exact decimal arithmetic, in integers.
//
// Everything in this section is integer-only on purpose. The subject of this test is a value that
// is wrong by less than an ULP right up until it is wrong by a whole step, so a float comparison
// anywhere in the verification would be the defect judging itself. `rust_decimal` is deliberately
// not reached for either: it is not a dependency of this crate, and adding one would strand
// `Cargo.lock`.
// ---------------------------------------------------------------------------------------------

/// A terminating decimal held exactly: `value == digits / 10^scale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dec {
    pub digits: i128,
    pub scale: u32,
}

fn all_ascii_digits(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_digit())
}

/// Parse a PLAIN decimal string (no exponent) into its exact integer pair.
///
/// An exponent form returns `None` rather than being interpreted. Every string compared in this
/// file is either a venue field or `py_f64_str` output, both positional; guessing at an unexpected
/// shape is how a verification quietly stops verifying.
pub fn parse_dec(s: &str) -> Option<Dec> {
    let s = s.trim();
    if s.is_empty() || s.contains(['e', 'E']) {
        return None;
    }
    let (negative, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((a, b)) => (a, b),
        None => (body, ""),
    };
    if !all_ascii_digits(int_part) || !all_ascii_digits(frac_part) {
        return None;
    }
    let magnitude = format!("{int_part}{frac_part}").parse::<i128>().ok()?;
    let digits = if negative { -magnitude } else { magnitude };
    Some(Dec { digits, scale: u32::try_from(frac_part.len()).ok()? })
}

/// `d` re-expressed at a scale at least as deep as its own. `None` on overflow, or on a shallower
/// target — every caller passes the deeper of the scales involved.
fn at_scale(d: Dec, scale: u32) -> Option<i128> {
    let deepen = scale.checked_sub(d.scale)?;
    d.digits.checked_mul(10i128.checked_pow(deepen)?)
}

/// EXACT decimal equality — never a tolerance, and never a float compare.
///
/// Trailing zeros are not a difference: Binance echoes a price padded to the symbol's own
/// precision (`"0.00012300"` where this side spelled `"0.000123"`), and a padded zero is not a
/// different price. Both sides are lifted to the deeper scale and compared as integers.
pub fn decimal_eq(a: &str, b: &str) -> bool {
    let (Some(x), Some(y)) = (parse_dec(a), parse_dec(b)) else {
        return false;
    };
    let scale = x.scale.max(y.scale);
    match (at_scale(x, scale), at_scale(y, scale)) {
        (Some(xs), Some(ys)) => xs == ys,
        _ => false,
    }
}

/// How many whole `step`s `got` sits away from `want`, when that is a whole number of them.
///
/// This is what makes a red run readable. The failure hunted here is not "some wrong number", it
/// is "exactly one step low", and a message that says `-1` names the defect while a message
/// quoting two long decimals leaves the reader to subtract them.
pub fn steps_off(got: &str, want: &str, step: Dec) -> Option<i128> {
    let (g, w) = (parse_dec(got)?, parse_dec(want)?);
    let scale = g.scale.max(w.scale).max(step.scale);
    let difference = at_scale(g, scale)?.checked_sub(at_scale(w, scale)?)?;
    let unit = at_scale(step, scale)?;
    if unit == 0 || difference % unit != 0 {
        return None;
    }
    Some(difference / unit)
}

/// The EXACT decimal `n · step`, rendered at the step's own scale — which is the scale
/// `quantize_to_step` rescales its output to, so this is the string the wire format owes us.
pub fn render_multiple(n: i128, step: Dec) -> Option<String> {
    let product = n.checked_mul(step.digits)?;
    if step.scale == 0 {
        return Some(product.to_string());
    }
    let unit = 10i128.checked_pow(step.scale)?;
    let sign = if product < 0 { "-" } else { "" };
    let magnitude = product.checked_abs()?;
    let width = step.scale as usize;
    let fraction = format!("{:0width$}", magnitude % unit);
    Some(format!("{sign}{}.{fraction}", magnitude / unit))
}

/// `x == mantissa · 2^exponent`, EXACTLY, for a finite positive `x`. Plain IEEE-754 field
/// extraction — no approximation anywhere, which is the whole point of using it below.
pub fn decompose(x: f64) -> Option<(u128, i32)> {
    if !(x.is_finite() && x > 0.0) {
        return None;
    }
    let bits = x.to_bits();
    let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = u128::from(bits & 0x000f_ffff_ffff_ffff);
    if raw_exponent == 0 {
        Some((fraction, -1074))
    } else {
        Some((fraction | (1u128 << 52), raw_exponent - 1075))
    }
}

/// `x << shift`, or `None` if that would lose a bit.
pub fn shl_exact(x: u128, shift: u32) -> Option<u128> {
    if shift >= 128 || x.leading_zeros() < shift {
        return None;
    }
    Some(x << shift)
}

/// **THE SELECTION CRITERION.** Does the f64 image of this step sit STRICTLY BELOW the decimal it
/// spells?
///
/// This is the whole reason a symbol is chosen or passed over, so it is COMPUTED rather than
/// looked up in a list of known-bad ticks. The measured exposed set is a MANTISSA family, not a
/// magnitude one — a list of it would be both incomplete and unable to explain itself, while the
/// property is what the defect actually keys on.
///
/// **Why it decides the outcome.** `round_to_step` returns `n as f64 * fl(step)`. When
/// `fl(step) < step` that product is pulled toward the low side of the true grid point `n·step`,
/// and once it lands below that point's own float, its shortest-round-trip decimal is below
/// `n·step` too — which is exactly the input on which the pre-fix `quantize_to_step` truncated a
/// whole step away. When `fl(step) >= step` the product is `>= n·step` in real arithmetic and
/// rounding is monotone, so the result can never fall below the grid point's float and the
/// truncation has nothing to eat.
///
/// ⚠ **This is the NECESSARY half only, and the selection below relies on that being understood.**
/// The monotonicity argument makes a `false` here conclusive — such a tick provably cannot
/// demonstrate the defect, so passing it over costs no coverage. A `true` proves nothing on its
/// own: which multiples of an exposed tick actually drift is measured, not derived, which is why
/// selection goes on to require a multiple inside the resting band that genuinely lands below the
/// grid point's float (`ulps_below`) rather than resting on this verdict alone. The two most
/// common live exposed grids are 1e-6 and 1e-7; the per-rung counts are in
/// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`.
///
/// **How it is decided, exactly.** `step_f == mantissa · 2^exponent` and the decimal is
/// `m / 10^k`, both exact, so `step_f < m / 10^k` is the integer question
/// `mantissa · 10^k · 2^exponent < m`, cross-multiplied into `u128` with every operation checked.
///
/// `None` means the comparison overflowed and the answer is UNKNOWN. That is deliberately not
/// folded into `false`: an undecidable step is counted as such in the survey, so a skip report can
/// never quietly claim a grid was examined when part of it was not.
pub fn f64_image_sits_below_its_decimal(step_f: f64, step: Dec) -> Option<bool> {
    let (mantissa, exponent) = decompose(step_f)?;
    let m = u128::try_from(step.digits).ok()?;
    let mut lhs = mantissa.checked_mul(10u128.checked_pow(step.scale)?)?;
    let mut rhs = m;
    if exponent >= 0 {
        lhs = shl_exact(lhs, u32::try_from(exponent).ok()?)?;
    } else {
        rhs = shl_exact(rhs, u32::try_from(-exponent).ok()?)?;
    }
    Some(lhs < rhs)
}
