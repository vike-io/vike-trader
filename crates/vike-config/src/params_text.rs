//! The text edge of strategy parameters: one free-form cell of text → one [`ParamValue`].
//!
//! Studio's parameter rows hold a value as TEXT the operator typed. [`parse_scalar`] is the ONE
//! reading of that text: a TOML scalar
//! (`2.5`, `4`, `true`, `"x"`, `[1, 2]`, `{ a = 1 }`) when it parses as one, else the text itself as
//! a string. It is the shared home of the copies that used to sit in `vike-studio-core` and
//! `vike-studio`.
//!
//! `vike-cli` does NOT call it: the CLI edits whole profile DOCUMENTS and splices values into
//! arbitrary profile keys (`--set data.from=2024-01-01`), where a TOML datetime must stay a
//! datetime. A strategy parameter has no datetime kind, so here one reads as a string.

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

#[cfg(test)]
mod tests {
    use super::*;

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
