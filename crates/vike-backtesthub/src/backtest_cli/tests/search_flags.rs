use super::*;
use crate::backtest_cli::args::RunArgs;

pub(super) fn argv(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

/// `words` as `backtest`'s command line, read by the REAL `clap` types through the real
/// [`Program`](vike_util::cli::Program): the run verb's flags, or `clap`'s refusal as the text it
/// would print. Test-only: production reads the line once, in [`run`].
pub(super) fn parsed(words: &[String]) -> Result<crate::backtest_cli::args::BacktestArgs, String> {
    let version = version_line();
    program(&version).try_parse_from(words).map_err(|exit| exit.text)
}

pub(super) fn run_of(words: &[String]) -> Result<RunArgs, String> {
    parsed(words).map(|p| p.run)
}

/// The search flags `words` resolve to — `clap`'s refusal or the resolver's, as text.
fn search_flags_of(words: &[String]) -> Result<SearchFlags, String> {
    parse_search_flags(&run_of(words)?)
}

/// `--keep-trials` as the parser resolves it.
fn keep_trials_of(words: &[String]) -> Result<KeepTrials, String> {
    parse_keep_trials(run_of(words)?.keep_trials.as_deref())
}

/// The profile `words` name, or the refusal.
fn profile_of(words: &[String]) -> Result<Option<String>, String> {
    profile_from_args(&run_of(words)?)
}

/// `--sort` as `backtest trials <id> …` resolves it.
fn trial_sort_of(words: &[String]) -> Result<TrialSort, String> {
    let mut line = argv(&["trials", "a-b"]);
    line.extend(words.iter().cloned());
    match parsed(&line)?.verb {
        Some(Verb::Trials(trials)) => parse_trial_sort(trials.sort.as_deref()),
        other => Err(format!("not the trials verb: {other:?}")),
    }
}

/// Every name `--optimizer` accepts, paired with the argv that makes the SELECTION COMPLETE.
///
/// Only `genetic` needs anything, and the pair exists because of it: its `--seed` is REQUIRED
/// ([`require_seed`]), so `["--optimizer", "genetic"]` alone is an error and a bare list of
/// names could no longer drive these tests. Written as a table rather than special-cased at
/// four call sites, so a fifth method with its own required input joins by adding a row.
///
/// ⚠ **The NAMES are no longer written here.** They come from
/// `vike_model::run_request::search_methods::SEARCH_METHODS` — the ONE roster the engine, `vike-cli`, the wire and
/// the MCP schema all read (§15.1 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`). What stays local is the
/// per-method EXTRA argv, which is a FIXTURE rather than a roster: a fifth method with its own
/// required input joins by adding one arm to [`extra_argv_for`], and a fifth method with none
/// joins by adding nothing at all.
fn extra_argv_for(method: &str) -> &'static [&'static str] {
    if method == "genetic" { &["--seed", "7"] } else { &[] }
}

/// The roster paired with each spelling's completing argv.
fn methods() -> Vec<(&'static str, &'static [&'static str])> {
    SEARCH_METHODS.iter().map(|name| (*name, extra_argv_for(name))).collect()
}

/// [`methods`] with the extra argv dropped — for the checks that only need the NAMES, and
/// where naming a method incompletely is the point (an ownership refusal fires before any
/// method is constructed, so it must not depend on the selection being complete).
fn method_names() -> Vec<&'static str> {
    SEARCH_METHODS.to_vec()
}

/// Every method named in [`METHOD_KNOBS`] is a method [`parse_search_flags`] actually accepts.
///
/// ⚠ **This became load-bearing when the owner column became a SET, and it did not exist
/// before.** With one owner per flag a typo'd name ("tpee") merely made the flag universally
/// refused, and the matrix test below still passed — every method reaches its `else` branch and
/// the refusal names the typo, so the assertion holds. In a TWO-element set the same typo is
/// worse and just as quiet: one method keeps its access, the other silently loses it, and the
/// matrix test is satisfied either way because it reads the owner set as the definition of
/// truth. Checking the names against the selector closes the loop.
#[test]
fn every_method_flag_names_only_real_methods() {
    for (flag, owners) in METHOD_KNOBS {
        assert!(!owners.is_empty(), "{flag} must be owned by at least one method");
        for owner in owners.iter() {
            let extra = methods()
                .iter()
                .find(|(name, _)| name == owner)
                .unwrap_or_else(|| panic!("{flag} names {owner:?}, which is not a method"))
                .1;
            let mut a: Vec<&str> = vec!["--optimizer", owner];
            a.extend_from_slice(extra);
            let flags = search_flags_of(&argv(&a))
                .unwrap_or_else(|e| panic!("{flag}'s owner {owner:?} must select: {e}"));
            assert_eq!(
                select::optimizer_for(flags.method).name(),
                *owner,
                "{flag} must name the method whose Optimizer::name is {owner:?}"
            );
        }
    }
}

/// Every method name `--optimizer` accepts builds the method whose `Optimizer::name` answers
/// with that same string — and the absent flag builds the grid.
///
/// ⚠ This is what REPLACES reading `EulerSearch::NAME` and `TpeSearch::NAME`, which are private
/// consts deliberately (`harness/mod.rs`: a method's knowledge belongs in the method's file).
/// Widening them for the sake of a match would reverse that; pinning the round trip keeps the
/// two literals in [`parse_search_flags`] from drifting away from the methods they name.
#[test]
fn every_optimizer_spelling_builds_the_method_it_names() {
    for (name, extra) in &methods() {
        let mut a: Vec<&str> = vec!["--optimizer", name];
        a.extend_from_slice(extra);
        let flags = search_flags_of(&argv(&a)).expect("a valid method");
        assert_eq!(
            select::optimizer_for(flags.method).name(),
            *name,
            "--optimizer {name} must build the method whose name() is {name:?}"
        );
        assert!(flags.requested, "an explicit --optimizer is a REQUESTED search");
    }
    let default = search_flags_of(&argv(&[])).expect("no flags is the default");
    assert_eq!(select::optimizer_for(default.method).name(), DEFAULT_SEARCH_METHOD);
    assert_eq!(
        harness::sweep::GridSearch.name(),
        DEFAULT_SEARCH_METHOD,
        "…and the protocol's default names the method this crate implements"
    );
    assert!(!default.requested, "no search flag is no search REQUEST");
}

/// Every knob is refused by every method that does not own it, and accepted by EVERY method
/// that does — driven from [`METHOD_KNOBS`] × [`methods`], so a fifth method or a fifth knob
/// joins this check by adding one row rather than by somebody remembering.
///
/// ⚠ **The widening to owner SETS is exactly where this test could have stopped meaning
/// anything, so read what it now asserts.** For each flag, the method list is partitioned by
/// the flag's own owner set: every owner must ACCEPT it and every non-owner must REFUSE it.
/// A single-owner flag therefore gets the identical three-refusals-one-accept check it always
/// had — `--trials` and `--euler-depth` are here to keep proving the rule did not soften into
/// "somebody owns it, so let it through" — while `--seed` gets two accepts and two refusals.
/// The refusal must name EVERY owner, not just the first: a two-owner flag whose refusal named
/// one method would be a worse message than the one it replaced.
///
/// ⚠ The values are VALID for the owning method on purpose (`3`, `8`, `7`), so nothing but the
/// ownership rule can produce these refusals: a "fix" that merely hoisted the range checks out
/// of the branches would leave this red.
///
/// ⚠ The owning-method ACCEPT is spelled with that method's completing argv from [`methods`],
/// because `--optimizer genetic --seed 7` is a complete selection and `--optimizer genetic`
/// alone is not. The REFUSAL half deliberately is NOT: ownership is checked before any method
/// is constructed, so an incomplete selection must still produce the ownership error — which is
/// the ordering `a_knob_another_method_owns_is_refused_when_genetic_was_named` pins from the
/// shipped binary's side.
#[test]
fn a_knob_is_refused_by_every_method_that_does_not_own_it() {
    for (flag, owners) in METHOD_KNOBS {
        let value = if *flag == "--euler-depth" { "3" } else { "8" };
        for (name, extra) in &methods() {
            let owns = owners.contains(name);
            let mut a: Vec<&str> = vec!["--optimizer", name];
            // The method's completing argv — unless the knob under test IS that argv, in which
            // case adding both would put one flag in argv twice.
            if owns && !extra.contains(flag) {
                a.extend_from_slice(extra);
            }
            a.extend_from_slice(&[flag, value]);
            let got = search_flags_of(&argv(&a));
            if owns {
                assert!(got.is_ok(), "{flag} must be accepted by its owner {name}: {got:?}");
            } else {
                let msg = got.expect_err(&format!("{flag} under {name} must be refused"));
                assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
                for owner in owners.iter() {
                    assert!(
                        msg.contains(owner),
                        "…and EVERY method that owns it, {owner} included: {msg}"
                    );
                }
            }
        }
        // No `--optimizer` at all is the GRID, and a knob for an unchosen method is still one.
        if !owners.contains(&"grid") {
            assert!(search_flags_of(&argv(&[flag, value])).is_err());
        }
        // The INLINE spelling too — the exact place an ownership rule written with `has_flag`
        // leaks, and a bare trailing knob, the place one written with `arg` alone leaks. Both
        // under a method that does NOT own the flag, picked out of the method list rather than
        // hard-coded, because `--seed`'s arrival means no single name is a non-owner of
        // everything any more.
        let stranger = method_names()
            .into_iter()
            .find(|n| !owners.contains(n))
            .expect("no knob is owned by every method");
        assert!(
            search_flags_of(&argv(&["--optimizer", stranger, &format!("{flag}={value}")])).is_err()
        );
        assert!(search_flags_of(&argv(&["--optimizer", stranger, flag])).is_err());
    }
}

/// `--trials 0` is refused under EVERY method. Under `tpe` the reason is the positive-integer
/// check (which was always right); under `grid`/`euler` it is the ownership refusal. The point
/// of the test is that the ANSWER is uniform — before this, the same value was fatal on one
/// path and silently discarded on two.
#[test]
fn a_zero_trial_budget_is_never_silently_accepted() {
    for name in method_names() {
        let msg = search_flags_of(&argv(&["--optimizer", name, "--trials", "0"]))
            .expect_err("--trials 0 must be refused under every method");
        assert!(msg.contains("--trials"), "the refusal must name the flag: {msg}");
    }
}

/// **The selector's own value-less spelling, and every knob's under its OWNING method.**
///
/// `arg` answers `None` for a trailing bare token, so `--optimizer` with nothing after it read
/// as "no `--optimizer` flag" and therefore as the GRID: the flag's default silently overruling
/// the flag. Its two siblings were already covered — bare `--search` by
/// `the_retired_search_flag_is_refused_in_every_spelling`, `--optimizer=` by `arg` answering
/// `Some("")` — which is exactly why this one was easy to miss.
///
/// The knob half is the same hole and had the same shape as defect (b): under a NON-owning
/// method a bare trailing knob was already refused (ownership goes through [`flag_given`]),
/// while under its OWNER it silently ran the default. Driven from [`METHOD_KNOBS`], so a fourth
/// method's knob joins by adding a row.
#[test]
fn a_value_less_flag_is_refused_rather_than_read_as_absent() {
    let msg = search_flags_of(&argv(&["--optimizer"]))
        .expect_err("a bare --optimizer must not read as the GRID");
    assert!(msg.contains("--optimizer"), "the refusal must name the flag: {msg}");
    for method in method_names() {
        assert!(msg.contains(method), "…and the methods it takes: {msg}");
    }
    // …and it is refused wherever it sits, not only as the last token: a profile after it is
    // eaten as its VALUE and refused as a bogus method, which is also not a silent grid.
    assert!(search_flags_of(&argv(&["--optimizer", "my.toml"])).is_err());

    // ⚠ EVERY owner, not the first: a two-owner knob has two arms that could each default it
    // silently, and `--seed` is the one where an owner genuinely has no default to fall back
    // on — a bare `--seed` under `genetic` must refuse as a value-less FLAG, never collapse
    // into `require_seed`'s missing-seed message, because those say different things.
    for (flag, owners) in METHOD_KNOBS {
        for owner in owners.iter() {
            let msg = search_flags_of(&argv(&["--optimizer", owner, flag]))
                .expect_err("a value-less knob under its own method must not default silently");
            assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
            assert!(
                msg.contains("value is required"),
                "…and say the flag was written WITHOUT ONE, rather than reporting it as \
                     absent: {msg}"
            );
        }
    }
}

/// **`genetic` requires `--seed`; `tpe` still defaults one.** The ONE place the two owners of
/// a single flag deliberately differ, pinned as a PAIR so neither can drift onto the other's
/// disposition unnoticed — a "consistency" edit that gave genetic a default, or one that made
/// tpe refuse, reddens here rather than in a shipped wire. [`require_seed`] carries the
/// argument for the asymmetry.
///
/// ⚠ The `0` row is the reason [`parse_seed`] answers `Option` instead of baking the default
/// in: a WRITTEN `--seed 0` and an ABSENT `--seed` are different argv that must stay
/// distinguishable, and a `u64`-returning parser collapses them before any arm can tell.
#[test]
fn genetic_requires_a_seed_and_tpe_still_defaults_one() {
    let msg = search_flags_of(&argv(&["--optimizer", "genetic"]))
        .expect_err("a genetic run with no seed must be refused");
    assert!(msg.contains("--seed"), "the refusal must name the flag: {msg}");
    assert!(msg.contains("genetic"), "…and the method that requires it: {msg}");

    // The operator's value REACHES the config, and a different value reaches it differently —
    // the pin against a hard-coded `GeneticConfig::new(0)` that every other test here passes.
    for seed in [0u64, 7, u64::MAX] {
        let a = ["--optimizer", "genetic", "--seed", &seed.to_string()].map(str::to_string);
        let flags = search_flags_of(&a).expect("an explicit seed completes the selection");
        assert_eq!(
            flags.method,
            SearchMethod::Genetic(vike_backtest::search::genetic::GeneticConfig::new(seed)),
            "--seed {seed} must be the seed the searcher is built with"
        );
    }

    // …and tpe's absent-seed default is UNCHANGED at 0. Not a preference: it is a shipped wire
    // — `crates/vike-cli/src/cmd/backtest.rs` forwards `--seed` only when the operator wrote
    // one, so a spawned `--optimizer tpe --trials 128` arrives here with no seed at all.
    assert_eq!(
        search_flags_of(&argv(&["--optimizer", "tpe"])).expect("tpe needs no seed").method,
        SearchMethod::Tpe(vike_ml::search::tpe::TpeConfig::new(
            vike_ml::search::tpe::TpeConfig::DEFAULT_TRIALS,
            0
        ))
    );
}

/// `--search` is refused in every spelling, including the bare trailing one that `arg` alone
/// answers `None` for — and including the argv that IS the first defect.
#[test]
fn the_retired_search_flag_is_refused_in_every_spelling() {
    for a in [
        vec!["--optimizer", "tpe", "--search", "bogus"],
        vec!["--search", "euler"],
        vec!["--search=grid"],
        vec!["--search"],
    ] {
        let msg = search_flags_of(&argv(&a)).expect_err("--search is retired");
        assert!(msg.contains("--search"), "the refusal echoes what was written: {msg}");
        assert!(msg.contains("--optimizer"), "…and names the replacement: {msg}");
    }
}

/// The euler depth range check, on both edges of the cap. `0` is legal (coarse-grid-only).
#[test]
fn an_out_of_range_euler_depth_is_refused_rather_than_clamped() {
    for depth in ["0", "1"] {
        assert!(search_flags_of(&argv(&["--optimizer", "euler", "--euler-depth", depth])).is_ok());
    }
    let cap = EulerConfig::MAX_DEPTH_CAP.to_string();
    assert!(search_flags_of(&argv(&["--optimizer", "euler", "--euler-depth", &cap])).is_ok());
    let over = (EulerConfig::MAX_DEPTH_CAP + 1).to_string();
    let msg = search_flags_of(&argv(&["--optimizer", "euler", "--euler-depth", &over]))
        .expect_err("past the cap is a refusal, not a clamp");
    assert!(msg.contains(&cap), "the refusal must name the cap: {msg}");
    assert!(search_flags_of(&argv(&["--optimizer", "euler", "--euler-depth", "abc"])).is_err());
}

/// `--rank-by` is NOT a search request: it names how to ORDER results, and its ignore on a
/// non-sweep profile is documented behaviour this PR deliberately leaves alone.
#[test]
fn rank_by_is_parsed_but_does_not_count_as_a_search_request() {
    let flags = search_flags_of(&argv(&["--rank-by", "return"])).expect("a valid metric");
    assert_eq!(flags.rank, RankChoice::Metric(RankMetric::TotalReturn));
    assert!(!flags.requested, "--rank-by alone must not make a non-sweep profile refuse");
    assert_eq!(
        search_flags_of(&argv(&["--rank-by", "MULTI"])).expect("case-insensitive").rank,
        RankChoice::Multi
    );
    assert!(search_flags_of(&argv(&["--rank-by", "bogus"])).is_err());
}

/// The positional profile, and the declared flags that keep a flag's VALUE from being read as one.
///
/// ⚠ The loop is the point: it iterates every VALUED flag the `clap` command declares, so a valued
/// flag added to `RunArgs` can never turn its value into a profile path — which the hand-rolled
/// scan needed a hand-kept table (`PROFILE_PATH_VALUED`) and an audit test for.
#[test]
fn no_flag_value_can_be_mistaken_for_the_positional_profile() {
    assert_eq!(profile_of(&argv(&["my.toml"])).unwrap().as_deref(), Some("my.toml"));
    assert_eq!(
        profile_of(&argv(&["--profile", "my.toml"])).unwrap().as_deref(),
        Some("my.toml"),
        "the older spelling two `crates/vike-cli/` arms SPAWN with must keep working"
    );
    assert_eq!(profile_of(&argv(&[])).unwrap(), None, "none given is not an error here");
    assert_eq!(
        profile_of(&argv(&["--json", "my.toml"])).unwrap().as_deref(),
        Some("my.toml"),
        "a valueless toggle consumes nothing"
    );

    let version = version_line();
    let command = program(&version).command::<crate::backtest_cli::args::BacktestArgs>();
    let valued: Vec<String> = vike_util::cli::describe(&command)
        .into_iter()
        .filter(|r| r.command.is_empty() && !r.positional && r.takes_value && r.name != "profile")
        .map(|r| format!("--{}", r.name))
        .collect();
    assert!(valued.len() >= 10, "the walk must see the valued flags: {valued:?}");
    // ⚠ `--profile` is skipped: its value IS the profile, which is the case asserted above.
    for flag in &valued {
        assert_eq!(
            profile_of(&argv(&[flag, "VALUE"])).unwrap(),
            None,
            "{flag}'s VALUE must never be read as the profile"
        );
        // …and the profile still resolves when it sits after that flag's pair.
        assert_eq!(
            profile_of(&argv(&[flag, "VALUE", "my.toml"])).unwrap().as_deref(),
            Some("my.toml"),
            "{flag} VALUE my.toml"
        );
        let inline = format!("{flag}=VALUE");
        assert_eq!(
            profile_of(&argv(&[inline.as_str(), "my.toml"])).unwrap().as_deref(),
            Some("my.toml"),
            "{inline} my.toml"
        );
    }
}

/// Two profiles — in either combination of spellings — is a REFUSAL naming both, never a
/// precedence rule. Picking a winner silently answers a question the operator did not know they
/// had asked, which is the same defect class as two searcher selectors.
#[test]
fn giving_the_profile_twice_is_refused_and_names_both() {
    let msg = profile_of(&argv(&["--profile", "a.toml", "b.toml"]))
        .expect_err("both spellings is a refusal");
    assert!(msg.contains("a.toml") && msg.contains("b.toml"), "names both: {msg}");
    let msg = profile_of(&argv(&["a.toml", "b.toml"])).expect_err("two positionals is a refusal");
    assert!(msg.contains("a.toml") && msg.contains("b.toml"), "names both: {msg}");
}

/// `--archive DIR` is a VALUED flag the profile path reads (`open_store`), so its value is not a
/// second positional — in either order, and in the inline spelling that already worked.
///
/// ⚠ The defect this pins: `--archive` had no row in the hand-kept table of valued flags (since deleted), so
/// `backtest sweep.toml --archive dir` read `dir` as a second profile and was refused for giving
/// the profile twice, naming a file the operator never meant as one. Only `--archive=dir` worked.
#[test]
fn an_archive_value_is_never_read_as_the_positional_profile() {
    for a in [
        &["sweep.toml", "--archive", "dir"][..],
        &["--archive", "dir", "sweep.toml"],
        &["sweep.toml", "--archive=dir"],
        &["--archive=dir", "sweep.toml"],
    ] {
        assert_eq!(profile_of(&argv(a)), Ok(Some("sweep.toml".to_string())), "{a:?}");
    }
}

// ───────────────────────────────────────────────────── stage 5: the search ARTIFACT's flags

/// `--keep-trials` takes `none|scalars|returns`, defaults to `scalars`, and `series` is a NAMED
/// refusal rather than an invalid-value one — an operator who asked for curves must learn WHY
/// they are not there, not merely that the word was wrong.
#[test]
fn keep_trials_defaults_to_scalars_and_refuses_series_by_name() {
    assert_eq!(keep_trials_of(&argv(&[])).unwrap(), KeepTrials::Scalars);
    assert_eq!(keep_trials_of(&argv(&["--keep-trials", "none"])).unwrap(), KeepTrials::None);
    assert_eq!(
        keep_trials_of(&argv(&["--keep-trials=SCALARS"])).unwrap(),
        KeepTrials::Scalars,
        "the inline spelling and the case are both accepted, like every sibling flag"
    );
    assert_eq!(
        keep_trials_of(&argv(&["--keep-trials", "RETURNS"])).unwrap(),
        KeepTrials::Returns,
        "the mode that arms the trial matrix, case-insensitively like its siblings"
    );

    let msg = keep_trials_of(&argv(&["--keep-trials", "series"])).expect_err("series is refused");
    assert!(msg.contains("equity CURVE"), "says WHAT is still refused: {msg}");
    assert!(
        !msg.contains("expected none|scalars|returns"),
        "and it is NOT the generic invalid-value line, which would read as a typo: {msg}"
    );

    let msg = keep_trials_of(&argv(&["--keep-trials", "everything"])).expect_err("typo");
    assert!(
        msg.contains("expected none|scalars|returns"),
        "a real typo gets the value line, and it names the NEW roster: {msg}"
    );

    let msg = keep_trials_of(&argv(&["--keep-trials"])).expect_err("a bare flag");
    assert!(
        msg.contains("value is required"),
        "a trailing bare flag is refused rather than read as ABSENT — `clap`'s rule now"
    );
}

/// ⚠ **A refusal that has become FALSE is worse than no refusal**, so the rewritten `series`
/// message is pinned on both sides: it must NAME the mode that now does the job an operator was
/// reaching for, and it must no longer claim the statistics are unreachable because
/// `row_from_outcome` drops the curve — which stopped being the whole truth the moment
/// `harness::sweep::ReturnBuckets` reached into that same function.
#[test]
fn the_series_refusal_names_what_is_now_possible() {
    let msg = keep_trials_of(&argv(&["--keep-trials", "series"])).expect_err("refused");
    assert!(msg.contains("--keep-trials returns"), "points at the mode that works: {msg}");
    assert!(
        msg.contains("PBO") && msg.contains("deflated Sharpe"),
        "…and says what that mode produces, which is why series was wanted: {msg}"
    );
    assert!(
        msg.contains("overfit.pbo"),
        "…and the gateable KEY, so a CI step can act on it: {msg}"
    );
    assert!(
        !msg.contains("row_from_outcome"),
        "the old argument — 'the curve is dropped before a row exists, so this is impossible' \
             — is no longer true and must not be repeated: {msg}"
    );
    assert!(
        msg.contains(&harness::sweep::ReturnBuckets::DEFAULT_BUCKETS.to_string())
            && msg.contains(&harness::sweep::threads::DEFAULT_SWEEP_THREADS.to_string()),
        "both numbers are INTERPOLATED from their consts, never typed: {msg}"
    );
}

/// ⚠ **`returns` writes the same LEDGER as `scalars`, and a resume must know that.** The
/// resume check keys on [`KeepTrials::keeps_ledger`] rather than on `== Scalars`; if it went
/// back to comparing spellings, a `returns` search would be refused with "it kept no ledger",
/// which is false and whose only stated remedy is re-running hours of work.
#[test]
fn returns_keeps_a_ledger_and_only_none_does_not() {
    assert!(!KeepTrials::None.keeps_ledger());
    assert!(KeepTrials::Scalars.keeps_ledger());
    assert!(KeepTrials::Returns.keeps_ledger(), "a returns search IS resumable");

    // The recorded spelling round-trips for every mode, and an unknown one is `None` rather
    // than a guess — `reopen_search_run` turns that into its own refusal.
    for mode in [KeepTrials::None, KeepTrials::Scalars, KeepTrials::Returns] {
        assert_eq!(KeepTrials::from_recorded(mode.as_str()), Some(mode));
    }
    assert_eq!(KeepTrials::from_recorded("series"), None);
    assert_eq!(KeepTrials::from_recorded("Scalars"), None, "the DOCUMENT spelling is exact");
}

/// ⚠ **Only `returns` retains anything**, which is what makes every other invocation
/// byte-identical to one from before the mode existed.
#[test]
fn only_the_returns_mode_arms_the_capture() {
    assert!(KeepTrials::Returns.return_buckets().is_armed());
    assert_eq!(
        KeepTrials::Returns.return_buckets().buckets(),
        harness::sweep::ReturnBuckets::DEFAULT_BUCKETS
    );
    assert!(!KeepTrials::Scalars.return_buckets().is_armed(), "the DEFAULT pays nothing");
    assert!(!KeepTrials::None.return_buckets().is_armed());
}

/// `--keep-trials` and `--resume` both ASK FOR A SEARCH, so a profile with no `[paramscan]` table
/// refuses instead of silently running one backtest — defect (e)'s rule, widened.
#[test]
fn the_new_search_flags_count_as_requesting_a_search() {
    assert!(search_flags_of(&argv(&["--keep-trials", "none"])).unwrap().requested);
    assert!(search_flags_of(&argv(&["--resume", "1756000000-1-0"])).unwrap().requested);
    assert!(
        !search_flags_of(&argv(&["--rank-by", "sharpe"])).unwrap().requested,
        "…and --rank-by still does not, which is its documented behaviour"
    );
    let msg = search_flags_of(&argv(&["--resume", "  "]))
        .expect_err("an empty id is refused rather than read as a run");
    assert!(msg.contains("--resume"), "names the flag: {msg}");
}

/// A sample value for a [`SEARCH_PROPERTY_FLAGS`] member. The ONE place these tests know a
/// member's grammar; the ROSTER is that array, which the parser and [`run`]'s refusal both
/// read, so the two cannot drift.
///
/// The catch-all PANICS rather than defaulting, the idiom
/// `crates/vike-cli/src/cmd/backtest/tests/route_and_spine.rs`'s `sample_value_for` uses: a fifth member added
/// without a grammar here stops the suite with the reason instead of being skipped.
fn sample_for_property_flag(flag: &str) -> &'static str {
    match flag {
        "--keep-trials" => "scalars",
        "--resume" => "1756000000-1-0",
        "--min-trades" => "50",
        "--progress" => "none",
        other => panic!(
            "{other} joined SEARCH_PROPERTY_FLAGS without a sample value here — add its arm, \
                 do not delete the check"
        ),
    }
}

/// ⚠ **EVERY flag that sets `requested` is one the no-`[paramscan]` refusal can NAME**, and
/// that holds only because the parser and [`run`]'s `asked` line read ONE array.
///
/// Both directions are driven: each [`SEARCH_PROPERTY_FLAGS`] member sets `requested` through
/// the real parser, and the rendered roster — `--optimizer`, then [`METHOD_KNOBS`], then that
/// array, which is exactly what [`run`] chains — contains every member, so the sentence cannot
/// go short. It is the defect stage 5 fixed once BY HAND for `--keep-trials` and `--resume`,
/// closed structurally this time.
///
/// ⚠ The mutation this fails on, in PRODUCTION: drop `"--min-trades"` from
/// [`SEARCH_PROPERTY_FLAGS`]. `requested` then stays false for `--min-trades 50`, the first
/// assertion reddens, and a run on a profile with no `[paramscan]` table would have executed
/// one ordinary backtest while silently discarding the floor. Dropping `--optimizer` from the
/// chain below instead reddens the second block.
#[test]
fn every_search_requesting_flag_is_one_the_refusal_can_name() {
    for flag in SEARCH_PROPERTY_FLAGS {
        let value = sample_for_property_flag(flag);
        let parsed = search_flags_of(&argv(&[*flag, value]))
            .unwrap_or_else(|e| panic!("{flag} {value} must parse: {e}"));
        assert!(parsed.requested, "{flag} asks for a search and must set `requested`");
    }

    // The roster `run` renders, assembled the way `run` assembles it.
    let rendered: Vec<&str> = std::iter::once("--optimizer")
        .chain(METHOD_KNOBS.iter().map(|(flag, _)| *flag))
        .chain(SEARCH_PROPERTY_FLAGS.iter().copied())
        .collect();
    assert_eq!(rendered[0], "--optimizer", "the selector leads, as the shipped message does");
    for (flag, _) in METHOD_KNOBS {
        assert!(rendered.contains(flag), "a method knob sets `requested` and must be named");
    }
    for flag in SEARCH_PROPERTY_FLAGS {
        assert!(rendered.contains(flag), "{flag} sets `requested` and must be named");
    }
    assert_eq!(
        rendered.len(),
        1 + METHOD_KNOBS.len() + SEARCH_PROPERTY_FLAGS.len(),
        "the rendered roster is the three sources and nothing else"
    );
}

/// **The `--min-trades` door.** The value reaches `select::resolve_min_trades` through
/// the REAL argv parser, in both spellings, and `0` stays the DISARMED answer rather than being
/// refused — which is the half a sweep matrix depends on (`--min-trades $FLOOR` with `FLOOR=0`
/// for the control arm).
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the `--min-trades` read from
/// [`parse_search_flags`], or stop threading its answer onto [`SearchFlags::floor`]. Every
/// assertion below then reads `TradeFloor::DISARMED` for an argv that armed a floor of 50 —
/// which is exactly what this binary did before the door existed, silently.
#[test]
fn the_trade_floor_reaches_the_parser_in_both_spellings() {
    for spelling in [vec!["--min-trades", "50"], vec!["--min-trades=50"]] {
        let f = search_flags_of(&argv(&spelling)).expect("a count parses");
        assert!(f.floor.is_armed(), "{spelling:?} must arm the floor");
        assert_eq!(f.floor.min_trades(), 50, "{spelling:?}");
    }
    assert_eq!(
        search_flags_of(&argv(&[])).expect("no flags").floor,
        TradeFloor::DISARMED,
        "unwritten is disarmed, which is what makes arming it unconditionally a no-op"
    );
    assert_eq!(
        search_flags_of(&argv(&["--min-trades", "0"])).expect("0 parses").floor,
        TradeFloor::DISARMED,
        "an explicit 0 IS disarmed — refusing it would force a conditional argv"
    );
    // …and the REFUSAL is the resolver's, naming the flag and what 0 means.
    let e = search_flags_of(&argv(&["--min-trades", "-1"])).expect_err("not a count");
    assert!(e.contains("--min-trades"), "{e}");
    assert!(e.contains("0 disarms the floor"), "…and which end of the range: {e}");
}

/// **The `--progress` door.** Every spelling `ProgressMode::NAMES` offers reaches
/// `select::resolve_progress` through the real parser, in any ASCII case, and a typo is
/// refused with a sentence that RENDERS the roster.
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the `--progress` read from
/// [`parse_search_flags`]. Every mode then resolves to `ProgressMode::Auto` and the first
/// assertion block reddens on `none`, which is the mode whose whole job is to be different from
/// the default on a terminal.
#[test]
fn the_progress_stream_reaches_the_parser_and_a_typo_is_refused() {
    assert_eq!(
        search_flags_of(&argv(&[])).expect("no flags").progress,
        ProgressMode::Auto,
        "unwritten is auto — on IF a human is watching, which is not the same as on"
    );
    for name in ProgressMode::NAMES {
        let want = ProgressMode::from_str_ci(name).expect("a NAMES member parses");
        for spelling in [name.to_string(), name.to_ascii_uppercase()] {
            let f = search_flags_of(&argv(&["--progress", spelling.as_str()]))
                .unwrap_or_else(|e| panic!("--progress {spelling} must parse: {e}"));
            assert_eq!(f.progress, want, "--progress {spelling}");
        }
        // …and the inline spelling, which `has_flag` alone could never see.
        let inline = format!("--progress={name}");
        let f = search_flags_of(&argv(&[inline.as_str()])).expect("the inline spelling parses");
        assert_eq!(f.progress, want, "{inline}");
    }
    let e = search_flags_of(&argv(&["--progress", "verbose"])).expect_err("not a mode");
    assert!(e.contains("--progress"), "{e}");
    for name in ProgressMode::NAMES {
        assert!(e.contains(name), "the refusal must offer {name}: {e}");
    }
}

/// ⚠ **A value-less observer flag is REFUSED, not read as absent** — the rule the hand-rolled guard
/// argument, applied to the two flags that joined this parser last. A trailing `--min-trades`
/// is a script's spelling (`--min-trades $FLOOR` with `FLOOR` unset), and reading it as "no
/// flag" would run the DISARMED default while the operator believed a floor was armed.
///
/// ⚠ The mutation this fails on, in PRODUCTION: declare either flag with `num_args(0..=1)` in
/// `args.rs`. The bare token then reads as an absent value, as the old `arg` did for a trailing
/// bare token, the parse succeeds with the default, and both assertions redden.
#[test]
fn a_value_less_observer_flag_is_refused_rather_than_read_as_absent() {
    for flag in ["--min-trades", "--progress"] {
        let msg = search_flags_of(&argv(&[flag]))
            .expect_err("a trailing bare observer flag must not run the default silently");
        assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
        assert!(
            msg.contains("value is required"),
            "…and say the flag was written WITHOUT ONE, rather than reporting it as absent: \
                 {msg}"
        );
        // …and the INLINE empty spelling is refused by the RESOLVER instead, because `arg`
        // answers `Some("")` there deliberately — a different message for a different mistake,
        // and neither of them a silent default.
        let inline = format!("{flag}=");
        let msg = search_flags_of(&argv(&[inline.as_str()]))
            .expect_err("an inline empty value must not run the default either");
        assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
    }
}

/// ⚠ **The shared vocabulary's `--keep-trials` roster, held against what THIS parser accepts —
/// and it was SHORT BY ONE when this test was written.**
///
/// `vike_model::run_request::flag_vocab`'s row declared `["none", "scalars"]` while
/// [`parse_keep_trials`] has accepted `returns` since the anti-overfitting statistics shipped,
/// so `flag_vocab::accepts_value("--keep-trials", "returns")` answered FALSE about a spelling
/// this binary accepts and documents in [`KEEP_TRIALS_EXPECTED`]. Nothing held the two
/// together: the row is `Route::EngineOnly`, so no client parser exercised it, and the
/// vocabulary crate cannot name [`KeepTrials`] — it sits BELOW this one.
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the `"returns"` arm from
/// [`parse_keep_trials`], or take `"returns"` back out of that `values` row. Both directions
/// are checked, because both have a live failure mode — a member the parser refuses would let a
/// client accept a value this binary then rejects after a spawn, and a value the parser accepts
/// that the roster omits is the defect this test found.
///
/// ⚠ `series` is deliberately NOT asserted as a member: it is refused BY NAME with its own
/// reason, so it belongs in neither the roster nor this loop. The last assertion pins that its
/// named refusal still fires rather than falling back to the generic invalid-value line.
#[test]
fn the_keep_trials_roster_is_exactly_what_the_parser_accepts() {
    let roster = vike_model::run_request::flag_vocab::value_roster("--keep-trials");
    assert!(!roster.is_empty(), "an empty roster would make this test vacuous");
    for &name in roster {
        let parsed = keep_trials_of(&argv(&["--keep-trials", name]))
            .unwrap_or_else(|e| panic!("{name} is on the roster and must parse: {e}"));
        assert_eq!(parsed.as_str(), name, "{name} must round-trip to its own spelling");
    }
    // …and the other direction: every spelling the parser accepts is on the roster. Driven off
    // [`KeepTrials::as_str`], which is an exhaustive `match` on the variant, so a fourth mode
    // cannot exist without a spelling here — the idiom
    // `harness::optimize`'s `every_progress_spelling_parses_and_the_roster_is_complete` uses,
    // where the array is what makes the assertion ITERATE and the exhaustive match is what
    // makes a new variant a COMPILE error.
    const ALL_KEEP: [KeepTrials; 3] = [KeepTrials::None, KeepTrials::Scalars, KeepTrials::Returns];
    for mode in ALL_KEEP {
        assert!(
            roster.contains(&mode.as_str()),
            "{mode:?} spells {:?}, which the shared vocabulary does not offer — a client would \
                 refuse a value this binary accepts",
            mode.as_str()
        );
    }
    // The LENGTH agreement is what makes the first loop non-vacuous in the other direction: a
    // roster longer than the mode set carries a spelling nothing accepts, and `series` — which
    // is refused BY NAME with its own reason — must not be one of its members.
    assert_eq!(
        roster.len(),
        ALL_KEEP.len(),
        "the roster and the accepted mode set must be the same size; `series` belongs to \
             neither, because it is refused by name"
    );
    let e =
        keep_trials_of(&argv(&["--keep-trials", "series"])).expect_err("series is refused by name");
    assert!(e.contains("returns"), "…and the named refusal points at what IS available: {e}");
}

/// ⚠ **The help text spells every `--progress` mode the parser accepts.** A doc comment cannot call a
/// function, so that text is the one hand copy of `ProgressMode::NAMES` in this file — and this
/// is what stops it going stale: a fourth mode added to the roster reddens here rather than
/// being accepted by a parser whose own help text does not mention it.
///
/// ⚠ The mutation this fails on, in PRODUCTION: add a mode to `ProgressMode::NAMES` (and its
/// `from_str_ci` arm) without editing the `--progress` entry in `args.rs`. Deleting `json`
/// from that entry reddens it too.
///
/// The same property for `--optimizer` and `--rank-by` rides on their own rosters one crate
/// over; this covers the one whose roster lives in this crate and had no usage line at all
/// until its door landed.
#[test]
fn the_usage_spells_every_progress_mode_the_parser_accepts() {
    let version = version_line();
    let help = program(&version)
        .command::<crate::backtest_cli::args::BacktestArgs>()
        .render_long_help()
        .to_string();
    for name in ProgressMode::NAMES {
        assert!(help.contains(name), "--progress accepts {name} and the usage must offer it");
    }
    assert!(help.contains("--progress"), "the flag itself is advertised");
    assert!(help.contains("--min-trades"), "…and so is its sibling observer");
    assert!(help.contains(LIST_OPTIMIZERS_FLAG), "…and the optimizer listing");
}

/// ⚠ **THE OPTIMIZER LISTING IS THREE SURFACES AND ONE SPELLING.** [`run`]'s arm reads
/// [`LIST_OPTIMIZERS_FLAG`], the `clap` command declares it (asserted just above), and
/// `vike_model::run_request::flag_vocab` carries the ROW that says which route it is reachable on.
/// Nothing but this holds the arm's spelling against the vocabulary's, and the failure it
/// prevents is silent in the worst way: a flag the help text promises, the published vocabulary
/// describes, and the binary answers "a profile is required" to.
///
/// ⚠ The mutation this fails on, in PRODUCTION: change either spelling — the const here or the
/// `flag` field of that row — without changing the other.
///
/// It also pins the two facts a triage reads off the row, because a listing that was declared
/// `Valued` would tell one to eat the next token as its value: on
/// `backtest --list-optimizers --json` that token is `--json`, and the JSON half of the listing
/// would stop working.
#[test]
fn the_optimizer_listing_flag_is_the_spelling_the_vocabulary_declares() {
    let row =
        vike_model::run_request::flag_vocab::spec(LIST_OPTIMIZERS_FLAG).unwrap_or_else(|| {
            panic!("{LIST_OPTIMIZERS_FLAG} is parsed by `run` and needs a vocabulary row")
        });
    assert_eq!(row.flag, LIST_OPTIMIZERS_FLAG, "one spelling, three surfaces");
    assert_eq!(row.arity, vike_model::run_request::flag_vocab::Arity::Bare, "it names no value");
    assert!(row.values.is_empty(), "a bare switch declares no roster");
    // …and it is NOT a truncation of `--list`, whose row sits beside it: both are in the table
    // now, so this is the first pair in it where one flag's name is a prefix of another's.
    assert_ne!(LIST_OPTIMIZERS_FLAG, "--list");
    assert!(LIST_OPTIMIZERS_FLAG.starts_with("--list"), "…which is why `spec` is exact-match");
    // …and the `clap` command declares exactly that long name, as a switch (the third surface).
    let run = run_of(&argv(&[LIST_OPTIMIZERS_FLAG, "--json"])).expect("the listing parses");
    assert!(run.list_optimizers && run.json && !run.list, "one spelling, one switch");
}

/// ⚠ `--resume` with `--keep-trials none` is refused at ARGV TRIAGE, so the combination that
/// would rewrite a resumed run's header to say it kept no ledger never reaches a filesystem.
/// The DEFAULT must not be refused — only the written flag.
#[test]
fn resuming_while_keeping_no_trials_is_refused_and_the_default_is_not() {
    let msg = search_flags_of(&argv(&["--resume", "1756000000-1-0", "--keep-trials", "none"]))
        .expect_err("the combination is refused");
    assert!(msg.contains("--resume") && msg.contains("--keep-trials"), "names both: {msg}");
    assert!(msg.contains("search.json"), "…and says what it would have damaged: {msg}");

    assert!(
        search_flags_of(&argv(&["--resume", "1756000000-1-0"])).is_ok(),
        "a resume with the DEFAULT keep-trials is the ordinary case and must not be refused"
    );
    assert!(
        search_flags_of(&argv(&["--resume", "1756000000-1-0", "--keep-trials", "scalars"])).is_ok(),
        "…and so is one that writes the flag with the value it already had"
    );
    assert!(
        search_flags_of(&argv(&["--keep-trials", "none"])).is_ok(),
        "`none` without a resume is untouched — it is how a search keeps no ledger"
    );
}

/// ⚠ **Every flag a search table names is one [`RunArgs::search_value`] can read AND `clap`
/// declares as valued.** The ownership rule and `run`'s refusal iterate [`METHOD_KNOBS`] and
/// [`SEARCH_PROPERTY_FLAGS`] BY NAME; a flag a table gains that `search_value` lacks would read as
/// never written — silently, so `backtest sweep.toml --newknob 5` would run one ordinary backtest —
/// and a flag the struct lacks would be refused by `clap` as unknown. Driven from the tables, so
/// a fifth member joins this check by existing.
///
/// This is also what replaces `every_new_valued_flag_is_declared_to_the_positional_scanner`: a
/// valued flag's VALUE can no longer be read as the profile because `clap` knows which flags take
/// one, so the thing left to hold is that the struct DECLARES the table's flags.
#[test]
fn every_flag_a_search_table_names_is_readable_and_declared_as_valued() {
    let version = version_line();
    let command = program(&version).command::<crate::backtest_cli::args::BacktestArgs>();
    let rows = vike_util::cli::describe(&command);
    let flags: Vec<&str> = std::iter::once("--optimizer")
        .chain(METHOD_KNOBS.iter().map(|(flag, _)| *flag))
        .chain(SEARCH_PROPERTY_FLAGS.iter().copied())
        .collect();
    for flag in flags {
        let run = run_of(&argv(&[flag, "VALUE"]))
            .unwrap_or_else(|e| panic!("{flag} VALUE must parse as a flag with a value: {e}"));
        assert_eq!(run.search_value(flag), Some("VALUE"), "{flag} must be read by search_value");
        assert!(run.profile.is_empty(), "{flag}'s VALUE must never be read as the profile");
        let row = rows
            .iter()
            .find(|r| r.command.is_empty() && !r.positional && format!("--{}", r.name) == flag)
            .unwrap_or_else(|| panic!("{flag} is named by a search table and not declared"));
        assert!(row.takes_value && !row.repeatable, "{flag} is a single-valued flag: {row:?}");
    }
}

/// A SUBCOMMAND word is never read as the profile — `data` since ruling 12, `trials` since
/// stage 5, and the refusal is now table-driven so a third joins by adding a row.
#[test]
fn a_subcommand_word_is_never_read_as_the_profile() {
    for (word, shape) in SUBCOMMANDS {
        let msg =
            profile_of(&argv(&["--json", word])).expect_err("a subcommand word is not a profile");
        assert!(msg.contains(word), "names the word: {msg}");
        assert!(msg.contains(shape), "and the shape that follows it: {msg}");
        assert!(msg.contains("--profile"), "and the escape hatch for a file of that name: {msg}");
    }
}

/// `--sort` names one field of ONE table, so the parse, the spelling list and the metric key
/// cannot drift apart.
#[test]
fn the_trial_sort_table_is_the_one_authority_for_its_own_fields() {
    assert_eq!(trial_sort_of(&argv(&[])).unwrap(), TrialSort::Score, "the default");
    for (name, variant, key, _) in TrialSort::ROWS {
        assert_eq!(
            trial_sort_of(&argv(&["--sort", name])).unwrap(),
            *variant,
            "{name} must parse to its own row's variant"
        );
        assert_eq!(variant.metric().map(|(k, _)| k), *key, "{name}'s metric key");
        assert!(TrialSort::names().contains(name), "{name} must appear in the usage list");
    }
    let msg = trial_sort_of(&argv(&["--sort", "nonsense"])).expect_err("an unknown field");
    assert!(msg.contains("nonsense") && msg.contains("score"), "names both: {msg}");
}
