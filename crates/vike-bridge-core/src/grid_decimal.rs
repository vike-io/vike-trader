//! Exact decimal arithmetic, in integers, for the live fine-tick smokes (`test-support` only: never in a
//! normal build, the `scripted` shape).
//!
//! A fine-tick smoke asks a venue what price it holds for an order whose price is an exact multiple of a
//! fine tick. The defect it hunts is a value wrong by less than an f64 ULP right up until it is wrong by a
//! whole step, so the verification may not use a float anywhere: a float compare would be the defect
//! judging itself. Everything here is `i128` arithmetic over `value == digits / 10^scale`.
//! `rust_decimal` is deliberately not reached for either: it would be the code under test judging itself
//! (`crate::format`'s quantizer is built on it), and the venue crates do not all depend on it.
//!
//! The users are the smokes of `crates/bridges/binance/tests/binance_fine_tick_price_smoke.rs`,
//! `crates/bridges/binance/tests/binance_perp_fine_tick_price_smoke.rs`,
//! `crates/bridges/okx/tests/okx_fine_tick_price_smoke.rs` and
//! `crates/bridges/deribit/tests/deribit_fine_tick_price_smoke.rs`. A lane test cannot share a file with a
//! path attribute (`scripts/refuse_live_credentials.sh` refuses it), which is why this is a crate module.

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
/// An exponent form returns `None` rather than being interpreted. Every string a smoke compares is
/// either a venue field or `crate::format::py_f64_str` output (Rust's `Display`, positional), and
/// guessing at an unexpected shape is how a verification quietly stops verifying.
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
/// target: every caller passes the deeper of the scales involved.
fn at_scale(d: Dec, scale: u32) -> Option<i128> {
    let deepen = scale.checked_sub(d.scale)?;
    d.digits.checked_mul(10i128.checked_pow(deepen)?)
}

/// EXACT decimal equality, never a tolerance and never a float compare.
///
/// Trailing zeros are not a difference: a venue echoes a price padded to the symbol's own precision
/// (`"0.00012300"` where this side spelled `"0.000123"`), and a padded zero is not a different price.
/// Both sides are lifted to the deeper scale and compared as integers. A string that does not parse
/// is not equal to anything.
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
/// This is what makes a red run readable. The failure hunted is not "some wrong number", it is
/// "exactly one step low", and a message that says `-1` names the defect while one quoting two long
/// decimals leaves the reader to subtract them.
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

/// The EXACT decimal `n x step`, rendered at the step's own scale: the scale
/// `crate::format`'s `quantize_to_step` rescales its output to, so this is the string the wire format
/// owes the venue.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_decimal_is_an_exact_integer_pair_and_anything_else_is_none() {
        assert_eq!(parse_dec("0.000123"), Some(Dec { digits: 123, scale: 6 }));
        assert_eq!(parse_dec("-12.50"), Some(Dec { digits: -1250, scale: 2 }));
        assert_eq!(parse_dec("7"), Some(Dec { digits: 7, scale: 0 }));
        for bad in ["", "1e-6", "1E6", "1.2.3", "abc", "1,5", "--1"] {
            assert_eq!(parse_dec(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn equality_ignores_padding_and_nothing_else() {
        assert!(decimal_eq("0.00012300", "0.000123"));
        assert!(decimal_eq("5", "5.000"));
        assert!(!decimal_eq("0.000123", "0.000122"));
        assert!(!decimal_eq("0.000123", "1e-6"), "an unparseable side is never equal");
    }

    #[test]
    fn a_multiple_renders_at_the_steps_own_scale_and_a_miss_counts_steps() {
        let step = parse_dec("0.000001").unwrap();
        assert_eq!(render_multiple(1234, step).as_deref(), Some("0.001234"));
        assert_eq!(render_multiple(1_000_000, step).as_deref(), Some("1.000000"));
        let whole = parse_dec("5").unwrap();
        assert_eq!(render_multiple(3, whole).as_deref(), Some("15"));
        assert_eq!(steps_off("0.001233", "0.001234", step), Some(-1));
        assert_eq!(steps_off("0.001236", "0.001234", step), Some(2));
        assert_eq!(steps_off("0.0012345", "0.001234", step), None, "not a whole number of steps");
    }
}
