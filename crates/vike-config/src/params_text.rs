//! The text edge of strategy parameters: one free-form cell of text → one [`ParamValue`].
//!
//! Studio's parameter rows hold a value as TEXT the operator typed. [`parse_scalar`] is the ONE
//! reading of that text: a TOML scalar
//! (`2.5`, `4`, `true`, `"x"`, `[1, 2]`, `{ a = 1 }`) when it parses as one, else the text itself as
//! a string. It is the shared home of the copies that used to sit in `vike-studio-core` and
//! `vike-studio`.
//!
//! `vike-cli` does NOT call it for a single value: the CLI edits whole profile DOCUMENTS and splices
//! values into arbitrary profile keys (`--set data.from=2024-01-01`), where a TOML datetime must
//! stay a datetime. A strategy parameter has no datetime kind, so here one reads as a string.
//!
//! A sweep AXIS is the exception, and [`parse_axis`] is its one reading: the Studio's Sweep grid
//! and the CLI's `--set paramscan.<k>=…` both call it, so one text sweeps the same typed values from
//! either door.

use vike_model::params::{ParamValue, Params};

/// One cell of text as a parameter value.
///
/// The text is trimmed; empty text is the empty string. It is parsed as a one-key DOCUMENT
/// (`v = <text>`) because `toml` has no scalar entry point that trims or accepts a bare value in
/// every position. Anything that fails to parse — including a TOML datetime, which `ParamValue`
/// cannot hold — is the trimmed text as a string.
pub fn parse_scalar(text: &str) -> ParamValue {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ParamValue::Str(String::new());
    }
    match toml::from_str::<Params>(&format!("v = {trimmed}")) {
        Ok(doc) => doc.get("v").cloned().unwrap_or_else(|| ParamValue::Str(trimmed.to_string())),
        Err(_) => ParamValue::Str(trimmed.to_string()),
    }
}

/// One sweep AXIS of text → its values: the ONE reading of an axis, for the Studio's Sweep grid and
/// `vike-cli backtest run --set paramscan.<k>=…` alike.
///
/// The text is a TOML array (`[5, 10, 20]`) or a bare comma list (`5, 10, 20`); each element reads
/// through [`parse_scalar`] and is then typed by [`typed_by_base`]. An empty element is skipped, so
/// `5, , 10` is two values and blank text is no axis at all.
///
/// ⚠ The comma split is plain: an element cannot itself hold a comma (a string with one, a nested
/// list). Spell such an axis as a TOML array.
pub fn parse_axis(text: &str, base: Option<&ParamValue>) -> Vec<ParamValue> {
    let elements = match parse_scalar(text) {
        ParamValue::List(items) => items,
        _ => text.split(',').filter(|e| !e.trim().is_empty()).map(parse_scalar).collect(),
    };
    elements.into_iter().map(|v| typed_by_base(v, base)).collect()
}

/// An axis value typed by the BASE knob it will replace — decision 0083's rule
/// (`docs/decisions/0083-the-runtime-plugin-join-lands.md`: an `as_integer()` reader sees a swept
/// `Float` as ABSENT and runs on its fallback, so two grid points trade identically).
///
/// * The base is an `Int`: an integral `Float` in `i64` range becomes `Int` (`4.0` → `4`); a
///   fractional one stays a `Float`, judged value by value.
/// * The base is a `Float`: an `Int` becomes the `Float` of the same value (`5` → `5.0`).
/// * No base knob (a key the table does not hold, a Rhai script, a strategy file): the value keeps its own
///   TOML kind, so integral text is an `Int` — the CLI's rule, ruled the one rule.
pub fn typed_by_base(value: ParamValue, base: Option<&ParamValue>) -> ParamValue {
    match (base, value) {
        (Some(ParamValue::Int(_)), ParamValue::Float(f)) => match exact_i64(f) {
            Some(i) => ParamValue::Int(i),
            None => ParamValue::Float(f),
        },
        // `i as f64` is exact for every |i| < 2^53, which covers any knob a grid is typed for.
        (Some(ParamValue::Float(_)), ParamValue::Int(i)) => ParamValue::Float(i as f64),
        (_, value) => value,
    }
}

/// `value` as an `i64` when it is integral and in range, else `None`. `i64::MAX as f64` rounds UP
/// to 2^63, which is out of range, hence the strict upper bound.
fn exact_i64(value: f64) -> Option<i64> {
    (value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value < i64::MAX as f64)
        .then_some(value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one axis rule: base-knob typing where the base has the knob, the value's own kind where
    /// it has none, both spellings of the list, and empty elements skipped.
    #[test]
    fn an_axis_is_typed_by_its_base_knob_and_by_itself_without_one() {
        use ParamValue::{Float, Int, Str};
        let int = Int(4);
        let float = Float(1.5);
        assert_eq!(
            parse_axis("6, -2, 4.5, 1e19, -0.0", Some(&int)),
            vec![Int(6), Int(-2), Float(4.5), Float(1e19), Int(0)]
        );
        assert_eq!(parse_axis("2, 2.5", Some(&float)), vec![Float(2.0), Float(2.5)]);
        // No knob: integral text is an Int, a decimal a Float, a word a string.
        assert_eq!(parse_axis("5, 10, 20", None), vec![Int(5), Int(10), Int(20)]);
        assert_eq!(parse_axis("5.0, 7.5", None), vec![Float(5.0), Float(7.5)]);
        assert_eq!(parse_axis("trend, chop", None), vec![Str("trend".into()), Str("chop".into())]);
        // The TOML array spelling reads the same, and blanks are no values.
        assert_eq!(parse_axis("[5, 10]", Some(&float)), vec![Float(5.0), Float(10.0)]);
        assert_eq!(parse_axis("5, , 10,", None), vec![Int(5), Int(10)]);
        assert!(parse_axis("  ", None).is_empty());
        assert!(matches!(parse_axis("nan", Some(&int))[0], Float(f) if f.is_nan()));
    }

    #[test]
    fn scalars_keep_their_kind() {
        assert_eq!(parse_scalar("4"), ParamValue::Int(4));
        assert_eq!(parse_scalar("2.5"), ParamValue::Float(2.5));
        assert_eq!(parse_scalar("2.0"), ParamValue::Float(2.0));
        assert_eq!(parse_scalar("true"), ParamValue::Bool(true));
        assert_eq!(parse_scalar("\"x\""), ParamValue::Str("x".into()));
    }

    #[test]
    fn composites_parse() {
        assert_eq!(parse_scalar("[1, 2]"), ParamValue::List(vec![1_i64.into(), 2_i64.into()]));
        assert_eq!(
            parse_scalar("{ a = 1 }"),
            ParamValue::Table([("a", ParamValue::Int(1))].into_iter().collect())
        );
    }

    #[test]
    fn text_that_is_not_toml_is_a_string() {
        assert_eq!(parse_scalar("okx"), ParamValue::Str("okx".into()));
        assert_eq!(parse_scalar("BTC-USDT"), ParamValue::Str("BTC-USDT".into()));
    }

    #[test]
    fn the_text_is_trimmed_and_empty_is_the_empty_string() {
        assert_eq!(parse_scalar(" 5 "), ParamValue::Int(5));
        assert_eq!(parse_scalar("\n5"), ParamValue::Int(5));
        assert_eq!(parse_scalar(" abc\n"), ParamValue::Str("abc".into()));
        assert_eq!(parse_scalar(""), ParamValue::Str(String::new()));
        assert_eq!(parse_scalar("   "), ParamValue::Str(String::new()));
    }

    #[test]
    fn a_datetime_has_no_kind_here_and_reads_as_a_string() {
        assert_eq!(parse_scalar("1979-05-27"), ParamValue::Str("1979-05-27".into()));
    }
}
