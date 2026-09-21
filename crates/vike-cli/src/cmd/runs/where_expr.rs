//! `--where EXPR` — one predicate over a run, in place of a wall of typed min/max flags (spec §6.1).
//!
//! ```text
//! EXPR := TERM ("," TERM)*          # comma is AND. There is no OR and no grouping.
//! TERM := FIELD OP VALUE
//! OP   := ">=" | "<=" | "!=" | "~" | "=" | ">" | "<"
//! ```
//!
//! `FIELD` is exactly `crate::cmd::runs::ls::field_of`'s vocabulary — the SAME resolver `--sort` and
//! `--cols` read through, because a second one is how `--sort sharpe` and `--where sharpe>1` come to
//! disagree about a name.
//!
//! # Three rules worth stating outright
//!
//! * A comparison is **numeric** when both sides parse as `f64`, and a case-insensitive string
//!   comparison otherwise. `~` is always a case-insensitive substring test.
//! * A **MISSING** field fails every operator except `!=`. A `null` is not less than anything, and
//!   `sharpe>1` admitting a run that computed no sharpe would put an empty row at the top of a
//!   leaderboard.
//! * **Two-character operators are tried first.** Otherwise `>=` parses as `>` against a value of
//!   `=1`, which parses as no number, which silently becomes a string comparison — a filter that
//!   answers, wrongly, with no error anywhere.
//!
//! # What this grammar deliberately does NOT have, and why
//!
//! **No OR, no grouping, no tags, no resolved parameters.** Tags need a `tag` verb (spec stage 6)
//! and resolved parameters need the run record to STORE one — today a manifest carries the
//! profile's PATH and NAME, not its content. Every predicate worth writing against what exists is a
//! conjunction, so a one-level grammar with no precedence question is the whole of it. A `|` is
//! REFUSED by name rather than falling into a substring test that returns nothing.

use crate::cmd::runs::ls::{FieldValue, field_of};
use crate::cmd::runs::scan::ScannedRun;
use crate::exit::CliError;

/// The operator set, LONGEST FIRST — the order is the parse, not a preference. See the module doc.
const OPS: [&str; 7] = [">=", "<=", "!=", "~", "=", ">", "<"];

/// The operator roster as an operator reads it, for every refusal message. One spelling.
const OP_HELP: &str = ">= | <= | != | ~ | = | > | <";

pub(crate) fn filter<'a>(
    runs: Vec<&'a ScannedRun>,
    expr: &str,
) -> Result<Vec<&'a ScannedRun>, CliError> {
    let terms = parse(expr)?;
    Ok(runs.into_iter().filter(|r| terms.iter().all(|t| t.holds(r))).collect())
}

/// One `FIELD OP VALUE` comparison.
struct Term {
    field: String,
    op: &'static str,
    value: String,
}

impl Term {
    fn holds(&self, run: &ScannedRun) -> bool {
        let got = field_of(run, &self.field).unwrap_or(FieldValue::Missing);
        // ⚠ A MISSING field fails every operator except `!=`. See the module doc.
        let (lhs_num, lhs_text) = match &got {
            FieldValue::Missing => return self.op == "!=",
            FieldValue::Num(n) => (Some(*n), render(*n)),
            FieldValue::Text(t) => (t.parse::<f64>().ok(), t.clone()),
        };
        let rhs_num = self.value.parse::<f64>().ok();

        if self.op == "~" {
            return lhs_text.to_lowercase().contains(&self.value.to_lowercase());
        }
        match (lhs_num, rhs_num) {
            (Some(a), Some(b)) => match self.op {
                ">=" => a >= b,
                "<=" => a <= b,
                "!=" => a != b,
                "=" => a == b,
                ">" => a > b,
                "<" => a < b,
                _ => false,
            },
            _ => {
                let (a, b) = (lhs_text.to_lowercase(), self.value.to_lowercase());
                match self.op {
                    ">=" => a >= b,
                    "<=" => a <= b,
                    "!=" => a != b,
                    "=" => a == b,
                    ">" => a > b,
                    "<" => a < b,
                    _ => false,
                }
            }
        }
    }
}

/// A numeric field rendered for a STRING comparison — the same spelling `ls`'s table uses, so
/// `--where` and `--cols` cannot show one thing and compare another.
fn render(n: f64) -> String {
    FieldValue::Num(n).render()
}

fn parse(expr: &str) -> Result<Vec<Term>, CliError> {
    // ⚠ Refused BY NAME rather than falling through: a `|` in a term would land inside a VALUE and
    // the filter would silently return nothing, which reads as "no run matches" rather than as
    // "this grammar has no OR".
    if expr.contains('|') {
        return Err(CliError::usage(format!(
            "--where '{expr}': this grammar has no OR — terms are joined by a comma, which means \
             AND (`kind=backtest,sharpe>1`). Run `ls` twice for a disjunction."
        )));
    }
    let mut out = Vec::new();
    for raw in expr.split(',') {
        let term = raw.trim();
        if term.is_empty() {
            return Err(CliError::usage(format!(
                "--where '{expr}': an empty term — terms are joined by a single comma \
                 (FIELD OP VALUE, with OP one of: {OP_HELP})"
            )));
        }
        // Longest operator first, or `>=` parses as `>` against a value of `=1`.
        let found = OPS.iter().find_map(|op| term.split_once(*op).map(|(f, v)| (*op, f, v)));
        let Some((op, field, value)) = found else {
            return Err(CliError::usage(format!(
                "--where '{term}': no operator — a term is FIELD OP VALUE, with OP one of: \
                 {OP_HELP}"
            )));
        };
        let field = field.trim();
        if field.is_empty() {
            return Err(CliError::usage(format!(
                "--where '{term}': no field before '{op}' — a term is FIELD OP VALUE, with OP one \
                 of: {OP_HELP}"
            )));
        }
        out.push(Term { field: field.to_string(), op, value: value.trim().to_string() });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::runs::ls::test_with_report as with_report;
    use crate::cmd::runs::selector::test_run_at as run_at;

    fn kept(runs: &[&ScannedRun], expr: &str) -> Vec<String> {
        filter(runs.to_vec(), expr).unwrap().iter().map(|r| r.run_id.clone()).collect()
    }

    #[test]
    fn a_single_equality_over_a_manifest_field() {
        let a = run_at("a-1-0", "backtest");
        let b = run_at("b-1-0", "search");
        assert_eq!(kept(&[&a, &b], "kind=search"), vec!["b-1-0"]);
        assert_eq!(kept(&[&a, &b], "kind!=search"), vec!["a-1-0"]);
        // Case-insensitive, because a kind is a label a human types.
        assert_eq!(kept(&[&a, &b], "kind=SEARCH"), vec!["b-1-0"]);
    }

    #[test]
    fn a_substring_operator_matches_inside_a_value() {
        let a = run_at("a-1-0", "backtest");
        assert_eq!(kept(&[&a], "detail.strategy~cross"), vec!["a-1-0"]);
        assert!(kept(&[&a], "detail.strategy~nothing").is_empty());
    }

    #[test]
    fn a_numeric_comparison_is_numeric_not_lexical() {
        let tmp = tempfile::tempdir().unwrap();
        let a =
            with_report(run_at("a-1-0", "backtest"), r#"{"sharpe":9.0}"#, &tmp.path().join("a"));
        let b =
            with_report(run_at("b-1-0", "backtest"), r#"{"sharpe":10.0}"#, &tmp.path().join("b"));
        // A string comparison would put "10.0" below "9.0".
        assert_eq!(kept(&[&a, &b], "sharpe>9.5"), vec!["b-1-0"]);
        assert_eq!(kept(&[&a, &b], "sharpe>=9"), vec!["a-1-0", "b-1-0"]);
    }

    #[test]
    fn commas_are_and() {
        let tmp = tempfile::tempdir().unwrap();
        let a = with_report(
            run_at("a-1-0", "backtest"),
            r#"{"sharpe":9.0,"n_trades":2}"#,
            &tmp.path().join("a"),
        );
        let b = with_report(
            run_at("b-1-0", "backtest"),
            r#"{"sharpe":10.0,"n_trades":50}"#,
            &tmp.path().join("b"),
        );
        assert_eq!(kept(&[&a, &b], "sharpe>1,n_trades>=30"), vec!["b-1-0"]);
        assert!(kept(&[&a, &b], "sharpe>1,n_trades>=30,kind=search").is_empty());
    }

    /// ⚠ A MISSING field fails every operator but `!=`. `sharpe>1` admitting a run that computed no
    /// sharpe would put an empty row at the top of a leaderboard.
    #[test]
    fn a_missing_field_fails_every_operator_except_inequality() {
        let a = run_at("a-1-0", "backtest"); // no report at all
        assert!(kept(&[&a], "sharpe>1").is_empty());
        assert!(kept(&[&a], "sharpe<1").is_empty());
        assert!(kept(&[&a], "sharpe=0").is_empty());
        assert_eq!(kept(&[&a], "sharpe!=0"), vec!["a-1-0"]);
    }

    #[test]
    fn a_term_with_no_operator_is_a_usage_refusal_that_shows_the_grammar() {
        let a = run_at("a-1-0", "backtest");
        let e = filter(vec![&a], "sharpe").expect_err("no operator");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(e.msg.contains(">="), "it shows the operator set: {}", e.msg);
    }

    /// The grammar has no OR and the refusal SAYS so, rather than letting a `|` fall into a
    /// substring match that silently returns nothing.
    #[test]
    fn a_pipe_is_refused_by_name_rather_than_silently_meaning_nothing() {
        let a = run_at("a-1-0", "backtest");
        let e = filter(vec![&a], "kind=backtest|kind=search").expect_err("no OR");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(e.msg.contains("comma"), "{}", e.msg);
    }

    /// ⚠ The two-character operators are tried BEFORE the one-character ones, or `>=` parses as `>`
    /// against a value of `=1` and every comparison silently becomes a string test.
    #[test]
    fn two_character_operators_win_over_their_prefixes() {
        let tmp = tempfile::tempdir().unwrap();
        let a =
            with_report(run_at("a-1-0", "backtest"), r#"{"sharpe":1.0}"#, &tmp.path().join("a"));
        assert_eq!(kept(&[&a], "sharpe>=1"), vec!["a-1-0"]);
        assert_eq!(kept(&[&a], "sharpe<=1"), vec!["a-1-0"]);
        assert!(kept(&[&a], "sharpe>1").is_empty());
    }
}
