//! `binutil` — the shared, dependency-free argv glue for the analytics-family bins.
//!
//! Every store-driven CLI in this family (`vike-backtest`'s `backtest`, `cheap_np_run`,
//! `cheap_np_depth`, `cheap_np_askgate`, `sport_taker_run`, plus `vike-report`'s `tearsheet`)
//! used to hand-copy the same `arg`/`has_flag` helpers. This module is their single home.
//!
//! **Why the argv half lives HERE and `store_root` does not.** The hist-store-root resolver
//! (`vike_backtest::binutil::store_root`) reads `$VIKE_HIST_STORE`, and the workspace-wide
//! settings registry (`vike_ops::settings::SETTINGS`) keys every declared env read on the pair
//! `(name, krate)`. Moving that one function would have retagged its row from `vike-backtest` to
//! `vike-analytics` — a change to a crate this refactor does not own. So the split is drawn at
//! exactly the env boundary: the four PURE argv parsers live here (usable by any bin, no env, no
//! deps), `store_root` stays in `vike-backtest`, and `vike_backtest::binutil` re-exports these
//! four so **every existing `vike_backtest::binutil::…` path still resolves unchanged**.

/// The value following `flag` in `args` (`--flag value`), if both tokens are present.
pub fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

/// Whether the bare `flag` token is present in `args`.
pub fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Parse `--flag value` as `f64`; an absent flag OR a malformed value yields `default`
/// (byte-identical to the hand-rolled helper the bins carried — the tuning-knob idiom).
/// Use [`parse_num`] where a malformed value must error instead of silently defaulting.
pub fn f64_arg(args: &[String], flag: &str, default: f64) -> f64 {
    arg(args, flag).and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Parse `--flag value` as any `FromStr` type, erroring clearly (rather than silently
/// defaulting) on a malformed value; the flag's absence yields `Ok(default)`.
pub fn parse_num<T: std::str::FromStr>(
    args: &[String],
    flag: &str,
    default: T,
) -> Result<T, String> {
    match arg(args, flag) {
        None => Ok(default),
        Some(s) => s.parse::<T>().map_err(|_| format!("invalid {flag} value {s:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arg_returns_the_value_after_the_flag() {
        let a = argv(&["bin", "--store", "dir", "--json"]);
        assert_eq!(arg(&a, "--store").as_deref(), Some("dir"));
        assert_eq!(arg(&a, "--missing"), None);
        // A trailing flag with no value token yields None, not a panic.
        assert_eq!(arg(&a, "--json"), None);
        // First occurrence wins.
        let dup = argv(&["bin", "--x", "one", "--x", "two"]);
        assert_eq!(arg(&dup, "--x").as_deref(), Some("one"));
    }

    #[test]
    fn has_flag_matches_exact_tokens_only() {
        let a = argv(&["bin", "--json", "--store", "dir"]);
        assert!(has_flag(&a, "--json"));
        assert!(has_flag(&a, "--store"));
        assert!(!has_flag(&a, "--js")); // no prefix matching
        assert!(!has_flag(&a, "json")); // exact token, dashes included
    }

    #[test]
    fn f64_arg_parses_and_silently_defaults() {
        let a = argv(&["bin", "--theta", "0.25", "--bad", "abc"]);
        assert_eq!(f64_arg(&a, "--theta", 1.0), 0.25);
        assert_eq!(f64_arg(&a, "--bad", 1.5), 1.5); // malformed -> default (documented idiom)
        assert_eq!(f64_arg(&a, "--absent", 2.0), 2.0);
    }

    #[test]
    fn parse_num_defaults_on_absence_and_errors_on_malformed() {
        let a = argv(&["bin", "--seed", "42", "--bad", "abc"]);
        assert_eq!(parse_num::<f64>(&a, "--seed", 1.0), Ok(42.0));
        assert_eq!(parse_num::<f64>(&a, "--absent", 7.0), Ok(7.0));
        let err = parse_num::<f64>(&a, "--bad", 0.0).unwrap_err();
        assert!(err.contains("--bad"), "error must name the flag: {err}");
    }
}
