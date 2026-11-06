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
//! The users are the smokes of `crates/bridges/binance/tests/binance_smokes/binance_fine_tick_price_smoke.rs`,
//! `crates/bridges/binance/tests/binance_smokes/binance_perp_fine_tick_price_smoke.rs`,
//! `crates/bridges/okx/tests/okx_fine_tick_price_smoke.rs` and
//! `crates/bridges/deribit/tests/deribit_smokes/deribit_fine_tick_price_smoke.rs`. A lane test cannot share a file with a
//! path attribute (`scripts/refuse_live_credentials.sh` refuses it), which is why this is a crate module.
//!
//! The perp and swap smokes also share how they CHOOSE the grid point ([`grid_witness`]) and read the
//! venue's marks ([`marks_by`]). Choosing runs the production float chain on purpose (`round_to_step`,
//! then the wire's `py_f64_str` spelling of the tick): it looks for the multiple that chain drifts on.
//! The verdict on the venue's echo stays integer arithmetic.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde_json::Value;

use crate::format::py_f64_str;
use crate::json::json_num;

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

/// The grid point a fine-tick smoke means to put on the wire, as [`grid_witness`] chose it.
#[derive(Clone, Debug)]
pub struct GridWitness {
    /// The tick as the WIRE path spells it: `format_to_step_f` routes the f64 through `py_f64_str`.
    pub step: Dec,
    /// The multiple chosen: the `n` in `n x tick`.
    pub n: i128,
    /// `round_to_step`'s own output for that multiple: the value production carries.
    pub price: f64,
    /// The exact decimal `n x tick`: what the venue must come back with.
    pub grid_decimal: String,
    /// How many f64 ULPs below the grid point's own float `price` landed (`>= 1` reproduces the
    /// defect's precondition).
    pub ulps_below: i64,
    /// `tick / mark`: how fine the grid is relative to the price.
    pub granularity: f64,
}

impl GridWitness {
    /// The order a smoke tries its candidates in: a drift witness before a plain multiple, then the
    /// finest grid first.
    pub fn try_order(&self, other: &Self) -> Ordering {
        (other.ulps_below >= 1)
            .cmp(&(self.ulps_below >= 1))
            .then(self.granularity.partial_cmp(&other.granularity).unwrap_or(Ordering::Equal))
    }
}

/// The multiple of `tick` a far-from-market BUY rests at, near `far_from_market x mark`: the first,
/// walking out from there up to `window` multiples either side, whose `round_to_step` image sits at
/// least one f64 ULP below the grid point's own float (a drift witness), else the first usable one.
/// A candidate price stays between half the mark and `never_fills x mark`. `None` when the tick or
/// the mark is not a positive finite number, or no multiple in the window is usable.
pub fn grid_witness(
    tick: f64,
    mark: f64,
    far_from_market: f64,
    never_fills: f64,
    window: i128,
) -> Option<GridWitness> {
    if !(tick.is_finite() && tick > 0.0 && mark.is_finite() && mark > 0.0) {
        return None;
    }
    let step = parse_dec(&py_f64_str(tick))?;
    let (low, high) = (mark * 0.5, mark * never_fills);
    let n0 = (mark * far_from_market / tick).round_ties_even();
    if !(n0.is_finite() && n0 >= 1.0) {
        return None;
    }
    let n0 = n0 as i128;
    let (mut fallback, mut witness) = (None, None);
    for offset in std::iter::once(0).chain((1..=window).flat_map(|d| [d, -d])) {
        let n = n0 + offset;
        if n < 1 {
            continue;
        }
        // The PRODUCTION chain: the snap the risk gate performs, on the multiple we mean to order.
        let price = vike_model::round_to_step(n as f64 * tick, tick);
        if !(low..=high).contains(&price) {
            continue;
        }
        let Some(grid) = render_multiple(n, step) else { continue };
        let Ok(grid_float) = grid.parse::<f64>() else { continue };
        let ulps = grid_float.to_bits() as i64 - price.to_bits() as i64;
        fallback.get_or_insert((n, price, grid.clone(), ulps));
        if ulps >= 1 {
            witness = Some((n, price, grid, ulps));
            break;
        }
    }
    let (n, price, grid_decimal, ulps_below) = witness.or(fallback)?;
    Some(GridWitness { step, n, price, grid_decimal, ulps_below, granularity: tick / mark })
}

/// A venue's mark rows (a JSON array) as a symbol to mark map: each row's `id_key` string and its
/// `mark_key` number or decimal string; a row missing either is skipped.
pub fn marks_by(rows: &Value, id_key: &str, mark_key: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for row in rows.as_array().into_iter().flatten() {
        let id = row.get(id_key).and_then(|s| s.as_str());
        let mark = row.get(mark_key).and_then(json_num);
        if let (Some(id), Some(mark)) = (id, mark) {
            out.insert(id.to_string(), mark);
        }
    }
    out
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

    #[test]
    fn the_witness_is_a_rendered_multiple_inside_the_band_and_an_unusable_grid_is_none() {
        let w = grid_witness(0.1, 100.0, 0.85, 0.9, 256).expect("a 0.1 grid under a 100 mark");
        assert!((50.0..=90.0).contains(&w.price), "{w:?}");
        assert_eq!(render_multiple(w.n, w.step).as_deref(), Some(w.grid_decimal.as_str()));
        let float = w.grid_decimal.parse::<f64>().unwrap();
        assert_eq!(w.ulps_below, float.to_bits() as i64 - w.price.to_bits() as i64);
        assert_eq!(w.granularity, 0.1 / 100.0);
        for (tick, mark) in
            [(0.0, 100.0), (-1.0, 100.0), (f64::NAN, 100.0), (0.1, 0.0), (0.1, f64::INFINITY)]
        {
            assert!(grid_witness(tick, mark, 0.85, 0.9, 256).is_none(), "{tick} / {mark}");
        }
    }

    #[test]
    fn a_drift_witness_is_tried_first_then_the_finest_grid() {
        let w = |ulps_below, granularity| GridWitness {
            step: Dec { digits: 1, scale: 0 },
            n: 1,
            price: 1.0,
            grid_decimal: "1".into(),
            ulps_below,
            granularity,
        };
        let mut all = [w(0, 1e-6), w(2, 1e-3), w(0, 1e-9), w(1, 1e-4)];
        all.sort_by(GridWitness::try_order);
        let got: Vec<_> = all.iter().map(|x| (x.ulps_below, x.granularity)).collect();
        assert_eq!(got, [(1, 1e-4), (2, 1e-3), (0, 1e-9), (0, 1e-6)]);
    }

    #[test]
    fn the_mark_map_reads_a_string_or_a_number_and_skips_an_incomplete_row() {
        let rows = serde_json::json!([
            {"instId": "A", "markPx": "1.5"},
            {"instId": "B", "markPx": 2},
            {"instId": "C"},
            {"markPx": "3"},
        ]);
        let marks = marks_by(&rows, "instId", "markPx");
        assert_eq!(marks.len(), 2);
        assert_eq!((marks["A"], marks["B"]), (1.5, 2.0));
    }
}
