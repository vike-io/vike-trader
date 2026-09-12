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

/// The value of `flag` in `args`, in EITHER spelling: `--flag value` (two tokens) or `--flag=value`
/// (one).
///
/// ⚠ **The `=` half is a CORRECTNESS fix, not a convenience.** This function matched an EXACT token
/// only, so `backtest --optimizer=tpe` matched nothing, the TPE arm was never entered, and the GRID
/// ran to completion and exited 0. The user asked for one search and silently got another — the
/// worst failure an argv parser has: not a refusal, a different answer. `--rank-by=return` ranked
/// by Sharpe and `--store=DIR` read a different store, by the same mechanism, in the same argv.
/// `crates/vike-cli/src/cmd/args.rs`'s `Flags` has always accepted the inline form, so the two argv
/// parsers in this workspace disagreed about one of the two spellings every operator writes — and
/// `crates/vike-backtest/src/backtest_cli.rs`'s `parse_addr_flag` already hand-rolls it for `--addr`
/// alone, arguing in its own doc why a systemd `ExecStart=` line needs it.
///
/// # The three rules, each load-bearing
///
/// * **Anchored on the `=`, never on the flag alone.** A bare `starts_with(flag)` would make a flag
///   answer for a LONGER flag beginning with it, trading this defect for a worse one: `--sizes`
///   sits beside `--size` across the `cheap_np_*` family, and
///   `crates/vike-backtest/src/backtest_cli.rs`'s `RETIRED_DATA_FLAGS` carries `--seed-demo` and
///   `--fetch-starter` beside `--seed` and `--fetch` — ⚠ the SECOND pair used to sit in that
///   file's `USAGE` and now sits in its retirement table, which is a live adjacency either way:
///   a refusal that fired on the wrong row would name the wrong replacement.
/// * **ONE pass, so FIRST OCCURRENCE still wins across both spellings.** Trying the exact token
///   over the whole slice first and the inline form second would answer from a LATER occurrence,
///   silently changing the rule `arg_returns_the_value_after_the_flag` pins.
/// * **`--flag=` answers `Some("")`, never `None`.** Every caller validates its own value; handing
///   the empty string back is what lets each refuse it by name. Answering `None` would mean "the
///   flag is absent", i.e. the DEFAULT — `--optimizer=` would silently mean grid, which is this
///   defect surviving inside its own fix.
///
/// The exact-token arm is unchanged, so the only argv whose meaning moves is one carrying a
/// `--flag=…` token — which every caller of this function previously ignored outright (none of the
/// six rejects unknown arguments).
///
/// ⚠ **"Previously ignored" is not the same as "previously harmless", and that is a CALLER's
/// obligation rather than this function's.** Wherever a flag's ABSENCE carries meaning — a wildcard
/// dimension, a lower rung of a resolver chain, a required-flag refusal — a `--flag=` token that
/// used to read as absent now reads as `Some("")` and changes the answer. Two live cases were found
/// and both are closed at the site that owns the meaning, never here (answering `None` for a blank
/// would reintroduce the very defect this function was widened to fix):
/// `crates/vike-backtest/src/binutil.rs`'s `store_root_resolved` filters a blank `--store` back to
/// the next rung, because `vike_model::store_path::resolve_store_root` honours an explicit path
/// unfiltered and `DataFusionHist::open` `create_dir_all`s it; and
/// `vike_data::store_kind::resolve_produced_by` refuses a blank `--produced-by`, because an empty
/// prefix matches EVERY commit key — it turns `backtest data rm`'s provenance assertion into a
/// vacuous one AND satisfies the gate that requires provenance before a wildcard delete.
/// **Auditing a new caller's flags for that class is part of adopting this function.**
pub fn arg(args: &[String], flag: &str) -> Option<String> {
    let inline = format!("{flag}=");
    let hit = args.iter().position(|a| a == flag || a.starts_with(&inline))?;
    match args[hit].strip_prefix(&inline) {
        // `--flag=value` — split on the FIRST `=` only, so a value may itself contain one.
        Some(value) => Some(value.to_string()),
        // `--flag value` — the next token, whatever it is, exactly as before.
        None => args.get(hit + 1).cloned(),
    }
}

/// Whether the bare `flag` token is present in `args`.
///
/// ⚠ **Exact-token, DELIBERATELY, and it did not follow [`arg`] into the inline `=` spelling.**
/// This answers a `bool` and therefore cannot refuse anything, so accepting `--json=false` would
/// have to read it as `true` — a new silent-wrong-answer defect bought in exchange for the one
/// [`arg`] was fixed for. The residual is real and is declared rather than widened: `--json=1`,
/// `--yes=true` and `--dry-run=yes` are still silently ignored on every bin in this family. Closing
/// it belongs with an unknown-argument triage (the shape `crates/vike-backfill/src/cli.rs`'s
/// `CliSpec` already has), which would newly refuse argv a spawner sends and is a change of a
/// different size.
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

    /// **Defect (d), at the primitive.** `arg` matched an EXACT token only, so
    /// `backtest --optimizer=tpe` matched nothing, the tpe arm was never entered, and the GRID ran
    /// to completion and exited 0 — the user asked for one search and silently got another.
    /// `crates/vike-cli/src/cmd/args.rs`'s `Flags` has always accepted the inline form, so the two
    /// argv parsers in this workspace disagreed about one of the two spellings every operator
    /// writes; `crates/vike-backtest/src/backtest_cli.rs`'s `parse_addr_flag` hand-rolls it for
    /// `--addr` in the very same argv, and argues why in its own doc.
    #[test]
    fn arg_accepts_the_inline_equals_spelling() {
        assert_eq!(arg(&argv(&["bin", "--optimizer=tpe"]), "--optimizer").as_deref(), Some("tpe"));
        assert_eq!(arg(&argv(&["bin", "--store=dir"]), "--store").as_deref(), Some("dir"));
        // The two spellings mixed in one argv, either way round: FIRST OCCURRENCE still wins, the
        // rule `arg_returns_the_value_after_the_flag` already pins for the spaced form alone.
        assert_eq!(arg(&argv(&["bin", "--x", "one", "--x=two"]), "--x").as_deref(), Some("one"));
        assert_eq!(arg(&argv(&["bin", "--x=two", "--x", "one"]), "--x").as_deref(), Some("two"));
        // The value may itself contain `=` — split on the FIRST one only, as `Flags` does.
        assert_eq!(arg(&argv(&["bin", "--k=a=b"]), "--k").as_deref(), Some("a=b"));
    }

    /// The half that a careless fix gets wrong, and it is the WORSE defect: anchoring on the flag
    /// alone rather than on the `=` makes a flag answer for a longer flag that begins with it.
    /// Both pairs below are real and live in ONE bin's `USAGE`
    /// (`crates/vike-backtest/src/backtest_cli.rs`'s `USAGE`), and `--size`/`--sizes` is a third
    /// across the `cheap_np_*` family.
    #[test]
    fn an_inline_value_is_anchored_on_the_equals_sign() {
        assert_eq!(arg(&argv(&["bin", "--seed-demo"]), "--seed"), None);
        assert_eq!(arg(&argv(&["bin", "--fetch-starter"]), "--fetch"), None);
        assert_eq!(arg(&argv(&["bin", "--sizes=3"]), "--size"), None);
        // …and the longer flag still answers for ITSELF in both spellings.
        assert_eq!(arg(&argv(&["bin", "--sizes=3"]), "--sizes").as_deref(), Some("3"));
        assert_eq!(arg(&argv(&["bin", "--seed-demo", "7"]), "--seed-demo").as_deref(), Some("7"));
    }

    /// ⚠ `--flag=` answers `Some("")`, never `None`. Load-bearing: if a blank inline value fell
    /// through to `None`, `--optimizer=` would mean "no optimizer flag" and therefore GRID — defect
    /// (d) surviving inside its own fix. Every caller validates its own value, so handing the
    /// empty string back is what lets each of them refuse it by name;
    /// `crates/vike-backtest/src/backtest_cli.rs`'s `parse_addr_flag` already applies that exact
    /// rule to `--addr=`.
    #[test]
    fn an_empty_inline_value_is_some_empty_rather_than_none() {
        assert_eq!(arg(&argv(&["bin", "--optimizer="]), "--optimizer").as_deref(), Some(""));
        assert_eq!(arg(&argv(&["bin", "--store="]), "--store").as_deref(), Some(""));
    }

    /// The widening reaches the two derived parsers as well, since both delegate to [`arg`].
    #[test]
    fn the_derived_parsers_inherit_the_inline_spelling() {
        assert_eq!(f64_arg(&argv(&["bin", "--theta=0.25"]), "--theta", 1.0), 0.25);
        assert_eq!(parse_num::<u32>(&argv(&["bin", "--n=7"]), "--n", 1), Ok(7));
    }
}
