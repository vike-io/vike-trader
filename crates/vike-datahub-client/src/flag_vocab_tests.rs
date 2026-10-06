use super::*;
use crate::proto::DEFAULT_SEARCH_METHOD;

/// Well-formedness, so a row cannot describe a spelling no parser can ever see: long form,
/// dashes included, and no `=` — both parsers split the inline spelling on the FIRST `=`, so a
/// row carrying one could never be matched.
#[test]
fn every_row_is_a_long_flag_with_no_inline_value_baked_in() {
    for s in BACKTEST_FLAGS {
        let f = s.flag;
        assert!(f.starts_with("--"), "{f}: long flags only (see BACKTEST_FLAGS' doc)");
        assert!(f.len() > 2, "{f}: `--` alone is not a flag");
        assert!(!f.contains('='), "{f}: the inline spelling is a PARSER's business");
        assert!(!s.why.is_empty(), "{f}: a row without its reason is a row nobody may change");
    }
}

/// One row per spelling. Two rows for one flag is how a vocabulary starts disagreeing with
/// itself, which is the defect it was built to remove one layer up.
#[test]
fn no_flag_has_two_rows() {
    for (i, s) in BACKTEST_FLAGS.iter().enumerate() {
        assert!(
            BACKTEST_FLAGS.iter().skip(i + 1).all(|o| o.flag != s.flag),
            "{} has a second row",
            s.flag
        );
    }
}

/// A bare boolean may not declare a value roster: the two facts contradict each other, and the
/// contradiction would reach an operator as "`--json` takes no value" beside a list of the
/// values it takes.
#[test]
fn a_bare_flag_declares_no_values() {
    for s in BACKTEST_FLAGS.iter().filter(|s| s.arity == Arity::Bare) {
        assert!(s.values.is_empty(), "{}: Bare and a roster cannot both be true", s.flag);
    }
}

/// The method roster IS the protocol const, not a copy that happens to agree today.
#[test]
fn the_optimizer_roster_is_the_protocol_const() {
    assert_eq!(value_roster("--optimizer"), &SEARCH_METHODS[..]);
    assert!(
        canonical_value("--optimizer", DEFAULT_SEARCH_METHOD).is_some(),
        "the default method must be a member of its own roster"
    );
}

/// ⚠ The ORDER of [`RANK_METRICS`], pinned as a RENDERED string, because
/// `crates/vike-backtest/src/harness/search_select.rs`'s `resolve_rank` renders this roster into
/// a shipped refusal and reordering it would move that message.
#[test]
fn the_rank_by_roster_renders_the_shipped_refusal_text() {
    assert_eq!(value_roster("--rank-by").join("|"), "sharpe|return|max_dd|equity|multi");
    assert_eq!(
        refuse_value("--rank-by", "nope"),
        "invalid --rank-by \"nope\" (expected sharpe|return|max_dd|equity|multi)"
    );
}

/// **The measured disagreement, as a test.** An upper-case roster value was accepted by the
/// engine and refused by the client; here it is accepted once, for both.
///
/// ⚠ "For both" is a claim about the DATA and the PREDICATE, which is all this crate can see —
/// it sits below both parsers and can name neither. The client half is pinned where the client
/// lives, by `crates/vike-cli/src/cmd/backtest/tests/route_and_spine.rs`'s
/// `an_upper_case_selector_is_accepted_and_canonicalised_by_the_shared_vocabulary`, which
/// drives the real argv parser over `--rank-by SHARPE`. Until that test existed this one read
/// as though it proved a route it never touched.
#[test]
fn a_roster_value_is_accepted_in_any_case_and_canonicalised() {
    for (flag, typed, canonical) in [
        ("--rank-by", "SHARPE", "sharpe"),
        ("--rank-by", "Max_Dd", "max_dd"),
        ("--rank-by", "multi", "multi"),
        ("--optimizer", "TPE", "tpe"),
        ("--optimizer", "Genetic", "genetic"),
    ] {
        assert_eq!(accept_value(flag, typed).as_deref(), Ok(canonical), "{flag} {typed}");
        assert!(accepts_value(flag, typed), "{flag} {typed}");
    }
}

/// …and the refusal still fires, naming the roster. Case-insensitivity widens the SPELLINGS of
/// a member, never the membership.
#[test]
fn a_value_outside_the_roster_is_refused_and_the_message_renders_the_roster() {
    let e = accept_value("--optimizer", "bayes").expect_err("not a method");
    assert!(e.contains("--optimizer") && e.contains("bayes"), "{e}");
    for name in SEARCH_METHODS {
        assert!(e.contains(name), "the refusal must offer {name}: {e}");
    }
    // A near-miss of a real member is still a refusal, not a prefix hit.
    assert!(accept_value("--rank-by", "sharp").is_err());
    assert!(accept_value("--rank-by", "").is_err());
}

/// A free-form flag admits its value and hands it back unchanged — a path is not a roster.
#[test]
fn a_free_form_flag_passes_its_value_through() {
    assert_eq!(accept_value("--profile", "Run.TOML").as_deref(), Ok("Run.TOML"));
    assert_eq!(accept_value("--seed", "7").as_deref(), Ok("7"));
    // …and so does a flag this vocabulary does not describe, rather than a refusal a caller
    // cannot act on.
    assert_eq!(accept_value("--nonesuch", "x").as_deref(), Ok("x"));
    assert!(value_roster("--nonesuch").is_empty());
}

/// The adjacency landmine: an exact lookup, so a longer flag never answers a shorter one's row.
#[test]
fn lookup_is_exact_so_an_adjacent_name_never_answers() {
    assert!(spec("--seed").is_some());
    assert!(spec("--seed-demo").is_none(), "--seed-demo is a retired DATA flag, not --seed");
    assert!(spec("--json").is_some());
    assert!(spec("--js").is_none());
    assert!(spec("json").is_none(), "exact token, dashes included");
    // ⚠ **The landmine is INSIDE the table now, and it was theoretical before.** `--list` and
    // `--list-optimizers` are both rows, so a prefix lookup would answer the bare listing's
    // `Bare`/no-roster row for the optimizer listing and vice versa — the first pair in
    // [`BACKTEST_FLAGS`] where one flag's spelling is a prefix of another's. They are separate
    // rows with separate `why`s, and each must answer only for itself.
    assert_eq!(spec("--list").map(|s| s.flag), Some("--list"));
    assert_eq!(spec("--list-optimizers").map(|s| s.flag), Some("--list-optimizers"));
    assert!(spec("--list-").is_none(), "neither row answers for a truncation of the other");
}

/// The three ENGINE-ONLY rows that landed with their argv doors, described the way a triage
/// would read them: `--progress` renders [`PROGRESS_MODES`] and refuses a non-member,
/// `--min-trades` declares NO roster (so a number is admitted and the engine judges its range),
/// and `--list-optimizers` is a bare switch.
///
/// ⚠ The mutation this fails on, in PRODUCTION: give `--min-trades` a `values` roster of its
/// own — the tempting "0|1|2" shape somebody reaches for when a table wants a list — and the
/// `accepts_value` assertions below refuse the integers the engine's `resolve_min_trades`
/// accepts. Declaring `--progress` [`Arity::Bare`] fails the first block for the same reason
/// `every_method_knob_is_a_valued_row_in_the_shared_vocabulary` exists one crate up: a triage
/// would then refuse `--progress=json`, which both parsers accept.
#[test]
fn the_engine_only_observer_rows_describe_what_their_doors_accept() {
    let progress = spec("--progress").expect("--progress has a door and needs a row");
    assert_eq!(progress.arity, Arity::Valued, "--progress takes a mode");
    assert_eq!(progress.route, Route::EngineOnly, "the sink writes to this process's stderr");
    assert_eq!(value_roster("--progress"), &PROGRESS_MODES[..], "the row IS the const");
    for typed in ["auto", "NONE", "Json"] {
        assert!(accepts_value("--progress", typed), "{typed} is a mode in any case");
    }
    let e = accept_value("--progress", "verbose").expect_err("not a mode");
    for name in PROGRESS_MODES {
        assert!(e.contains(name), "the refusal must offer {name}: {e}");
    }

    let floor = spec("--min-trades").expect("--min-trades has a door and needs a row");
    assert_eq!(floor.arity, Arity::Valued, "--min-trades takes a count");
    assert_eq!(floor.route, Route::EngineOnly, "WireSearch carries no fifth field");
    assert!(floor.values.is_empty(), "a count is free-form; the ENGINE owns its range");
    for typed in ["0", "50", "18446744073709551616"] {
        assert_eq!(accept_value("--min-trades", typed).as_deref(), Ok(typed));
    }

    let listing = spec("--list-optimizers").expect("--list-optimizers has a door");
    assert_eq!(listing.arity, Arity::Bare, "it names no value");
    assert_eq!(listing.route, Route::EngineOnly, "the client publishes the roster as an asset");
}

/// The exclusions stay excluded. See [`EXCLUDED`] for why each one must.
#[test]
fn every_excluded_flag_stays_out_of_the_table() {
    for &(flag, reason) in EXCLUDED {
        assert!(spec(flag).is_none(), "{flag} is EXCLUDED: {reason}");
        assert!(!reason.is_empty(), "{flag}: an exclusion without its reason is a gap");
    }
}

/// Every flag the client's `--local` arm forwards to the engine is a `Both` row. That arm is the
/// one place a spelling is written by one parser and read by the other, so a forwarded flag
/// classified `EngineOnly` would mean the client sends what it claims not to accept.
#[test]
fn every_forwarded_flag_is_shared_by_both_routes() {
    for flag in
        ["--profile", "--rank-by", "--optimizer", "--euler-depth", "--trials", "--seed", "--json"]
    {
        let s = spec(flag).unwrap_or_else(|| panic!("{flag} is forwarded and needs a row"));
        assert_eq!(s.route, Route::Both, "{flag} is forwarded by execute_local");
    }
}

/// The command the `--store` refusal hands back is one that RUNS: the shipped multicall, then
/// its `datahub` tool. [`store_flag_removed`]'s doc carries the day it named a binary no
/// release ships. Checked in the sentence AND in the `--store` exclusion row, which restates
/// it.
#[test]
fn the_store_refusal_names_the_shipped_binary() {
    let (_, row) = EXCLUDED
        .iter()
        .find(|(flag, _)| *flag == "--store")
        .expect("--store keeps its EXCLUDED row");
    for text in [store_flag_removed("backtest"), row.to_string()] {
        assert!(text.contains("vike-backend datahub"), "it names the shipped binary: {text}");
        // Not a substring of the right spelling — `vike` is followed by `-` there — so this
        // refuses the pre-rename one wherever it comes back without refusing the fix.
        assert!(!text.contains("vike datahub"), "the pre-rename spelling is back: {text}");
    }
}
