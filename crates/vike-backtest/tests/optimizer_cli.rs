//! `backtest --optimizer grid|euler|tpe|genetic` — the argv triage gate for the parameter-search
//! flags.
//!
//! Ruling 13 of `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md` refused a separate
//! `optimize` verb: the search flags stay on `backtest` and the word "optimizer" lives in the FLAG.
//! Ruling 14 made the profile POSITIONAL. This file is the shipped-binary half of both, and of the
//! four defects the hand-written flag ladder carried — every one of which exited **0**, which is
//! why nothing here can be gated on an exit code alone.
//!
//! What the ladder did, measured on `origin/main` before this file existed:
//!
//! * **(a)** `--optimizer tpe --search bogus` ran tpe and exited 0. The `--optimizer` arm returned
//!   before `--search` was ever parsed, so a bogus searcher name was accepted.
//! * **(b)** `--search euler --trials 0` exited 0, ignoring a `0` the tpe arm treats as fatal. One
//!   flag value, two opposite fates, decided by a flag the operator may not have connected to it.
//! * **(c)** `--search grid --euler-depth 99` exited 0 silently — a flag meaningful only to euler,
//!   accepted and discarded by the grid. So did `--euler-depth abc`: the malformed-value check sat
//!   inside the branch that was not taken.
//! * **(d)** `--optimizer=tpe` **ran the grid**, printed a full ranked report and exited 0.
//!   `crates/vike-analytics/src/binutil.rs`'s `arg` matched an exact token only, so the inline
//!   spelling matched nothing at all. The worst of the four: not a refusal, a DIFFERENT ANSWER.
//! * **(e)**, not in the original list and found while reading: every one of these flags sat inside
//!   `if profile.is_sweep()`, so `backtest run.toml --optimizer tpe --trials 500` ran ONE ordinary
//!   backtest and exited 0 — 500 trials of Bayesian search asked for, one run delivered, no
//!   diagnostic.
//!
//! ⚠ **Most tests here need no store and no profile**, and that is a PROPERTY rather than a
//! convenience: argv is now triaged before `DataFusionHist::open`, so a mistyped flag costs no
//! store. `a_refused_flag_never_opens_the_store` is what makes that assertable rather than assumed.
//!
//! ⚠ **One test here is the WIDENING'S OWN AUDIT and reaches past the search flags.** Fixing
//! (d) meant teaching `arg` the `--flag=value` spelling, and that is not the pure widening it looks
//! like: for a flag whose ABSENCE carries meaning — a wildcard dimension, a lower resolver rung, a
//! required-flag refusal — a `--flag=` token that every caller previously IGNORED now answers
//! `Some("")` and changes the answer. `--store=` was caught while the fix was written (it would
//! have minted a store in the working directory); `--produced-by=` on the IRREVERSIBLE `data rm`
//! verb was not, and it is the one that deletes data. Hence
//! `a_blank_produced_by_is_refused_rather_than_asserting_nothing`, in this file rather than a new
//! one, because the hazard belongs to the widening rather than to that verb.
//!
//! ⚠ **The last section is the FOURTH method, and it tests a rule the first four defects never
//! had to express.** `--seed` is the first flag in [`METHOD_FLAGS`]-land with more than one owner
//! (tpe AND genetic), so the ownership table that makes those defects stay fixed had to widen
//! without weakening: a single-owner flag must still refuse exactly as it did, and a two-owner one
//! must refuse everywhere outside its pair. Both directions are asserted there, and so is the one
//! place the two methods deliberately DIFFER — genetic refuses to run without a seed where tpe
//! defaults to 0.
//!
//! `#![cfg(feature = "datafusion-store")]` because the `backtest` bin carries
//! `required-features = ["datafusion-store"]`: without it the binary is not built and
//! `env!("CARGO_BIN_EXE_backtest")` would not COMPILE. That gate is also why this is its own test
//! binary rather than a member of `tests/parity.rs`'s group — `crates/vike-backtest/CLAUDE.md`'s
//! grouping rule. CI runs it in the `datafusion-store` lane (`scripts/ci_feature_suite.sh`).
#![cfg(feature = "datafusion-store")]

use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_backtest"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run backtest {args:?}: {e}"))
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Assert a FLAG-LEVEL refusal: exit 2, nothing on stdout, no `USAGE` dump, and every needle
/// named on stderr.
///
/// ⚠ **The `usage:` assertion is the load-bearing one, and it was added because five of these
/// tests passed against the UNFIXED binary.** Exit 2 proves nothing here — a nonexistent profile
/// also exits 2 — and neither do the needles on their own: the old binary answered every argv
/// below with `--profile <path> is required` followed by the whole `USAGE` const, and that const
/// contains the literal strings `--optimizer`, `--search`, `--trials`, `--euler-depth`, `grid`,
/// `euler` and `tpe`. A `contains` check over a wall of help text is a test that cannot fail.
///
/// So the shape is also a DESIGN decision, stated here because this is where it is enforced: a
/// refusal that names ONE flag answers with one line naming that flag and the fix, never with the
/// help text. The `USAGE` dump stays on the two arms where the operator genuinely supplied nothing
/// to talk about — a bare invocation and a bad `--addr` — which is what
/// `crates/vike-backtest/tests/help_cli.rs`'s `a_missing_profile_still_exits_non_zero_on_stderr`
/// pins from the other side.
fn refuses(args: &[&str], needles: &[&str]) {
    let out = run(args);
    let stderr = stderr_of(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "`backtest {args:?}` must exit 2 (a usage error); stderr: {stderr:?}"
    );
    assert!(
        out.stdout.is_empty(),
        "…and print nothing on stdout: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !stderr.contains("usage:"),
        "`backtest {args:?}` must answer with the ONE flag it is refusing, not with the whole \
         USAGE block — every needle below appears somewhere in that block, so a refusal that \
         dumps it makes this assertion unfalsifiable; stderr: {stderr:?}"
    );
    for needle in needles {
        assert!(
            stderr.contains(needle),
            "`backtest {args:?}` must name {needle:?} in its refusal; stderr: {stderr:?}"
        );
    }
}

/// A profile with a two-point numeric `[sweep]` grid over `buy_hold`. Numeric and
/// type-homogeneous, so euler and tpe both ACCEPT it — a non-numeric axis would make them refuse
/// for a reason that has nothing to do with what is under test.
const SWEEP_PROFILE: &str = r#"
name = "optimizer-cli-fixture"

[data]
venue = "demo"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2025-01-01T00"
to = "2025-07-01T00"

[engine]
cash = 10000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"

[sweep]
size = [1.0, 2.0]
"#;

/// The same profile with its `[sweep]` table removed — a single-point run, for defect (e).
const SINGLE_PROFILE: &str = r#"
name = "optimizer-cli-single"

[data]
venue = "demo"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2025-01-01T00"
to = "2025-07-01T00"

[engine]
cash = 10000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// A scratch directory holding one profile file, plus the store path beside it. `tempfile`, never a
/// fixed name under the system temp directory — `crates/vike-ops/tests/temp_path_gate.rs` says why.
fn scratch(profile: &str) -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join("profile.toml");
    std::fs::write(&path, profile).expect("write the fixture profile");
    let store = dir.path().join("store");
    (
        dir,
        path.to_str().expect("a UTF-8 scratch path").to_string(),
        store.to_str().expect("a UTF-8 scratch path").to_string(),
    )
}

// ---------------------------------------------------------------- defect (d), end to end

/// **Defect (d).** The inline spelling must reach the method selector — and the method that RUNS
/// must be the one that was named.
///
/// The discriminator is the METHOD's own stderr summary, not the exit code: every one of these
/// exits 0 and prints a valid ranked report either way, which is exactly why the defect was
/// invisible. `tpe` and `euler` each print a cost line; the grid prints none, and the third case is
/// what keeps the first two from passing on a binary that prints both lines unconditionally.
///
/// The store is EMPTY on purpose. Every sweep point then fails and its row carries an error rather
/// than a report — which changes nothing under test here: all three methods still run their loops
/// (`crates/vike-backtest/src/search.rs`'s `euler_search_batched` pins
/// `all_nan_search_still_returns_a_best_and_terminates`) and still print their summaries. It keeps
/// this test at one store open instead of a demo-tape seed.
#[test]
fn an_inline_optimizer_value_selects_that_method_not_the_grid() {
    let (_dir, profile, store) = scratch(SWEEP_PROFILE);

    let tpe = run(&[&profile, "--store", &store, "--optimizer=tpe", "--trials=3"]);
    let stderr = stderr_of(&tpe);
    assert!(
        tpe.status.success(),
        "a valid tpe run must exit 0; status {:?}, stderr: {stderr:?}",
        tpe.status.code()
    );
    assert!(
        stderr.contains("tpe: 3 trials"),
        "`--optimizer=tpe --trials=3` must RUN tpe — before the inline spelling was understood \
         this silently ran the grid and printed no summary at all; stderr: {stderr:?}"
    );

    let euler = run(&[&profile, "--store", &store, "--optimizer=euler", "--euler-depth=1"]);
    let stderr = stderr_of(&euler);
    assert!(euler.status.success(), "a valid euler run must exit 0; stderr: {stderr:?}");
    assert!(
        // `crates/vike-backtest/src/harness/euler.rs`'s `EulerBudget` `Display` opens with
        // `euler:`; the colon is what makes the grid's NEGATIVE assertion below meaningful rather
        // than a match on any line that happens to mention the word.
        stderr.contains("euler:"),
        "`--optimizer=euler` must RUN euler and print its budget line; stderr: {stderr:?}"
    );

    let grid = run(&[&profile, "--store", &store, "--optimizer=grid"]);
    let stderr = stderr_of(&grid);
    assert!(grid.status.success(), "a valid grid run must exit 0; stderr: {stderr:?}");
    assert!(
        !stderr.contains("tpe:") && !stderr.contains("euler:"),
        "…and the grid must print NEITHER method's summary, or the two assertions above would \
         pass on a binary that always prints both; stderr: {stderr:?}"
    );
}

/// **Defect (d), the refusal half.** A bogus method spelled inline is refused BY NAME rather than
/// falling through to the grid — and the refusal happens before the profile is even looked for,
/// which is what makes this need no fixture.
#[test]
fn an_inline_optimizer_with_a_bogus_value_is_refused() {
    refuses(
        &["no-such-profile.toml", "--optimizer=bogus"],
        &["--optimizer", "grid", "euler", "tpe", "genetic"],
    );
    // The spaced spelling too, so the fix is not bought by handling only the new form.
    refuses(&["no-such-profile.toml", "--optimizer", "bogus"], &["--optimizer"]);
    // ⚠ And a BLANK inline value. `arg` answers `Some("")` for `--optimizer=` deliberately; if it
    // answered `None` this would read as "no flag" and run the GRID — defect (d) inside its own fix.
    refuses(&["no-such-profile.toml", "--optimizer="], &["--optimizer"]);
    // ⚠ …and the BARE TRAILING token, the last spelling in which the selector could still read as
    // ABSENT: `arg` answers `None` for it (the token is found, there is no `=`, there is no next
    // token), so `backtest sweep.toml --optimizer` ran the exhaustive GRID at exit 0 with no
    // diagnostic — a different answer, not a refusal, which is defect (d) exactly. Reached by the
    // same shell idiom as the inline one: `--optimizer $METHOD` with `METHOD` unset collapses to
    // the bare token. `crates/vike-backtest/src/backtest_cli.rs`'s `required_value` is the guard.
    refuses(&["no-such-profile.toml", "--optimizer"], &["--optimizer", "grid", "euler", "tpe"]);
}

/// **The knob half of the same hole.** A value-less knob under the method that OWNS it silently ran
/// that knob's DEFAULT — `--trials` → `TpeConfig::DEFAULT_TRIALS`, `--euler-depth` →
/// `EulerConfig::DEFAULT_MAX_DEPTH`, `--seed` → `0` — while the same argv under a non-owning method
/// was already refused, because the ownership check goes through `flag_given`. One flag, two fates,
/// decided by which optimizer was named: defect (b)'s shape wearing a different flag.
#[test]
fn a_value_less_knob_under_its_own_method_is_refused() {
    for (method, flag) in [
        ("tpe", "--trials"),
        ("tpe", "--seed"),
        ("euler", "--euler-depth"),
        // ⚠ `--seed`'s SECOND owner. The value-less spelling is refused HERE, above the genetic
        // arm's own missing-seed rule — a written-but-empty `--seed` must never read as "no seed
        // given" and collapse into that refusal, because the two say different things to the
        // operator and only one of them is true.
        ("genetic", "--seed"),
    ] {
        refuses(&["no-such-profile.toml", "--optimizer", method, flag], &[flag]);
    }
}

// ---------------------------------------------------------------- defect (a)

/// **Defect (a).** `--search` is RETIRED, and it is refused rather than aliased.
///
/// `--optimizer tpe --search bogus` used to exit 0 having run tpe: the `--optimizer` arm returned
/// before `--search` was parsed. The fix is structural rather than a new check — there is only ONE
/// selector now, so there is no second spelling left to disagree with the first.
///
/// The bare, value-less `--search` case is the one that pins detection by TOKEN: a refusal written
/// through `arg` alone would answer `None` for it and let it through.
#[test]
fn the_retired_search_flag_is_refused_and_names_its_replacement() {
    for argv in [
        vec!["no-such-profile.toml", "--optimizer", "tpe", "--search", "bogus"],
        vec!["no-such-profile.toml", "--search", "euler"],
        vec!["no-such-profile.toml", "--search", "grid"],
        vec!["no-such-profile.toml", "--search=euler"],
        vec!["no-such-profile.toml", "--search"],
    ] {
        refuses(&argv, &["--search", "--optimizer"]);
    }
}

// ---------------------------------------------------------------- defects (b) and (c)

/// **Defect (b).** `--trials` belongs to tpe. Handed to another method it was silently discarded —
/// and `--trials 0`, which the tpe arm treats as fatal, was among them.
///
/// ⚠ This doc read "`--trials` and `--seed` belong to tpe" until the genetic method was wired.
/// `--seed` has TWO owners now and its rows moved to
/// [`the_seed_belongs_to_tpe_and_genetic_and_to_nobody_else`]; what stays here is the
/// SINGLE-OWNER case, which is the property the widening had to leave intact.
///
/// The values below are VALID for tpe on purpose (`8`), so nothing but the ownership rule can
/// make these refusals happen: a fix that merely hoisted the positive-integer check would leave
/// them green.
#[test]
fn a_tpe_only_flag_is_refused_when_another_method_was_named() {
    refuses(
        &["no-such-profile.toml", "--optimizer", "euler", "--trials", "8"],
        &["--trials", "tpe"],
    );
    refuses(
        &["no-such-profile.toml", "--optimizer", "grid", "--trials", "0"],
        &["--trials", "tpe"],
    );
    // ⚠ `--seed` LEFT this test when it gained a second owner (#1751's follow-up): it is no longer
    // a tpe-only flag, and its whole matrix — accepted by tpe AND genetic, refused by grid, by
    // euler and by the implicit grid — is
    // [`the_seed_belongs_to_tpe_and_genetic_and_to_nobody_else`]. `--trials` is unchanged and is
    // still the single-owner case, which is the property that had to survive the widening.
    // No `--optimizer` at all is the GRID, and a knob for an unchosen method is still that.
    refuses(&["no-such-profile.toml", "--trials", "8"], &["--trials", "tpe"]);
    // The inline spelling, which is exactly where an ownership check written with `has_flag` leaks.
    refuses(&["no-such-profile.toml", "--optimizer", "grid", "--trials=8"], &["--trials", "tpe"]);
    // …and the POSITIVE half: tpe's own `0` refusal must survive, or the fix bought uniformity by
    // relaxing the one arm that was already right.
    refuses(&["no-such-profile.toml", "--optimizer", "tpe", "--trials", "0"], &["--trials"]);
}

/// **Defect (c).** `--euler-depth` belongs to euler. `--search grid --euler-depth 99` exited 0
/// having discarded it, and so did `--euler-depth abc` — the malformed-value check lived inside the
/// branch that was not taken.
///
/// `3` is a VALID euler depth, so only the ownership rule can produce these refusals.
#[test]
fn an_euler_only_flag_is_refused_when_another_method_was_named() {
    refuses(
        &["no-such-profile.toml", "--optimizer", "grid", "--euler-depth", "3"],
        &["--euler-depth", "euler"],
    );
    refuses(
        &["no-such-profile.toml", "--optimizer", "tpe", "--euler-depth", "3"],
        &["--euler-depth", "euler"],
    );
    refuses(&["no-such-profile.toml", "--euler-depth", "3"], &["--euler-depth", "euler"]);
    refuses(
        &["no-such-profile.toml", "--optimizer", "grid", "--euler-depth=3"],
        &["--euler-depth"],
    );
}

/// An out-of-range `--euler-depth` is REFUSED rather than silently clamped — §7.5 of the design,
/// signed off. `EulerConfig::with_depth` clamps at `MAX_DEPTH_CAP`, and the budget line then
/// reports a depth the operator never typed, in the very line whose job is to report the budget.
///
/// `0` stays legal (it means coarse-grid-only, per `with_depth`'s own doc), so the refusal is a
/// range check and not a "positive integer" one.
#[test]
fn an_out_of_range_euler_depth_is_refused_rather_than_clamped() {
    refuses(&["no-such-profile.toml", "--optimizer", "euler", "--euler-depth", "99"], &["16"]);
    refuses(
        &["no-such-profile.toml", "--optimizer", "euler", "--euler-depth", "abc"],
        &["--euler-depth"],
    );
    // The in-range values must NOT be refused — including `0` and the cap itself.
    let (_dir, profile, store) = scratch(SWEEP_PROFILE);
    for depth in ["0", "1", "16"] {
        let out =
            run(&[&profile, "--store", &store, "--optimizer", "euler", "--euler-depth", depth]);
        assert!(
            out.status.success(),
            "--euler-depth {depth} is in range and must run; stderr: {:?}",
            stderr_of(&out)
        );
    }
}

// ---------------------------------------------------------------- defect (e)

/// **Defect (e).** A search asked for against a profile with no `[sweep]` table used to run ONE
/// ordinary backtest and exit 0, silently discarding every search flag.
///
/// `--rank-by` deliberately keeps its DOCUMENTED ignore (`crates/vike-backtest/src/backtest_cli.rs`'s
/// module doc: "ignored — not an error — on a non-sweep profile"). The distinction is real rather
/// than a rationalisation: `--rank-by` names how to ORDER results and is meaningless-but-harmless
/// with nothing to order, while `--optimizer` names WHAT WORK TO DO.
#[test]
fn a_search_on_a_profile_with_no_sweep_table_is_refused_rather_than_ignored() {
    let (_dir, profile, store) = scratch(SINGLE_PROFILE);
    for argv in [
        vec![profile.as_str(), "--store", store.as_str(), "--optimizer", "tpe", "--trials", "500"],
        vec![profile.as_str(), "--store", store.as_str(), "--optimizer", "grid"],
        // ⚠ `--optimizer euler` is spelled out rather than leaving the method implicit: a bare
        // `--euler-depth 2` is refused EARLIER, by the ownership rule, because the implicit method
        // is the grid and the grid does not own that knob. That is the right answer to that argv —
        // it just is not this test's question.
        vec![
            profile.as_str(),
            "--store",
            store.as_str(),
            "--optimizer",
            "euler",
            "--euler-depth",
            "2",
        ],
    ] {
        refuses(&argv, &["[sweep]"]);
    }

    // …and `--rank-by` alone is still IGNORED on the same profile, exit 0.
    let out = run(&[&profile, "--store", &store, "--rank-by", "return"]);
    assert!(
        out.status.success(),
        "--rank-by keeps its documented ignore on a single-point profile; stderr: {:?}",
        stderr_of(&out)
    );
}

// ---------------------------------------------------------------- ruling 14, and the argv[0] shim

/// **Ruling 14: the profile is POSITIONAL** — and the shipped standalone binary must read it,
/// which is the ONLY thing that gates `crates/vike-backtest/src/bin/backtest.rs`'s `skip(1)`.
///
/// ⚠ That shim used to pass the FULL `std::env::args()`, argv[0] included, while
/// `crates/vike/src/main.rs`'s `backtest_main` passes the tail `crates/vike/src/lib.rs`'s `resolve`
/// already stripped. Nothing could observe the difference while every parser matched exact `--`
/// tokens; a positional is the first position-sensitive parser in that file, and under the old
/// shape it would have read the PROGRAM PATH as the profile here and the real profile there. There
/// is no compile-time guard for it — only this test.
///
/// `--profile` is KEPT as the older spelling, and that is not a preference:
/// `crates/vike-cli/src/cmd/backtest.rs`'s `--local` arm SPAWNS this binary with it.
#[test]
fn the_profile_is_positional_and_the_flag_still_works() {
    // The bare token is READ as the profile — so the failure names the FILE, not a missing flag.
    let out = run(&["no-such-profile.toml"]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "a nonexistent profile exits 2; stderr: {stderr:?}");
    assert!(
        stderr.contains("failed to load profile") && stderr.contains("no-such-profile.toml"),
        "a bare token must be READ as the profile (and the PROGRAM PATH must not be); \
         stderr: {stderr:?}"
    );

    // A valued flag's VALUE is not a positional.
    let out = run(&["--store", "some-dir", "no-such-profile.toml"]);
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("failed to load profile") && stderr.contains("no-such-profile.toml"),
        "`--store DIR` puts a bare token in argv that is emphatically not a profile; \
         stderr: {stderr:?}"
    );

    // Both spellings at once is a REFUSAL, not a precedence rule: two spellings that may name two
    // different files is the same defect class as two searcher selectors.
    refuses(&["--profile", "a.toml", "b.toml"], &["a.toml", "b.toml"]);
    refuses(&["a.toml", "b.toml"], &["a.toml", "b.toml"]);

    // The KEPT pin: `--profile` alone is unchanged, because `vike-cli backtest --local` spawns
    // exactly that. `crates/vike-backtest/tests/help_cli.rs`'s
    // `a_missing_profile_still_exits_non_zero_on_stderr` holds the other half.
    let out = run(&["--profile", "no-such-profile.toml"]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr.contains("failed to load profile"), "stderr: {stderr:?}");
}

// ---------------------------------------------------------------- the two design properties

/// **A refused flag never opens the store.** `DataFusionHist::open` `create_dir_all`s its root
/// unconditionally, so the directory's ABSENCE afterwards is a hard, checkable proof that argv is
/// triaged before any I/O — the property every cheap test in this file rests on, and the thing that
/// makes a mistyped flag cost nothing.
#[test]
fn a_refused_flag_never_opens_the_store() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 scratch path");
    refuses(
        &["no-such-profile.toml", "--store", never_str, "--optimizer", "bogus"],
        &["--optimizer"],
    );
    assert!(
        !Path::new(never_str).exists(),
        "an argv refusal must precede the store open, but {never_str:?} was created"
    );
}

/// **The widening's blank-value audit, at the one flag that guards an IRREVERSIBLE delete.**
///
/// Teaching `arg` the inline spelling is not a pure widening. For a flag whose ABSENCE carries
/// meaning, a `--flag=` token that every caller previously ignored now answers `Some("")` and
/// changes the answer — and `--produced-by` is the worst case in the tree, because a blank prefix
/// does not match nothing, it matches EVERYTHING. `vike_data::store_kind::key_matches_prefix` is
/// `starts_with`, so `vike_data::removal::RemovalPlan::verdict` finds no foreign key in any series
/// and the provenance assertion passes VACUOUSLY; and because the value is `Some` rather than
/// `None`, `crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series` sweep gate — the rule that
/// REQUIRES provenance before a wildcard delete — stands down too. `data rm --kind bar --venue
/// binance --produced-by=$PREFIX --yes` with `$PREFIX` unset used to refuse; it would have deleted
/// every matched series instead.
///
/// The refusal lives in `vike_data::store_kind::resolve_produced_by`, one site below every caller,
/// which is why the SPACED form — reachable long before this PR — is closed by the same edit.
///
/// `--json` because the `data rm` arm leads with its resolved store root on STDOUT for a human
/// and on STDERR under `--json`, and [`refuses`] requires an empty stdout. The store path must
/// still not exist afterwards: this refusal is resolved BEFORE `DataFusionHist::open`, which
/// `create_dir_all`s whatever it is handed.
#[test]
fn a_blank_produced_by_is_refused_rather_than_asserting_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join("never");
    let never = path.to_str().expect("a UTF-8 scratch path");
    // A WILDCARD selector — no symbol, no group, no interval — which is the shape `--produced-by`
    // is REQUIRED for and therefore the shape a vacuous assertion sets loose. `--json` puts the
    // arm's leading `store:` line on stderr, which is what [`refuses`] needs to see an empty stdout.
    let base = ["data", "rm", "--kind", "bar", "--venue", "binance", "--store", never, "--json"];
    for blank in [vec!["--produced-by="], vec!["--produced-by", ""]] {
        let argv: Vec<&str> = base.iter().chain(blank.iter()).copied().collect();
        refuses(&argv, &["--produced-by", "matches every key"]);
        assert!(
            !Path::new(never).exists(),
            "a blank --produced-by must be refused before the store is opened, but {never:?} was \
             created by {argv:?}"
        );
    }

    // …and the flag's ABSENCE on the same wildcard selector is still the OTHER refusal, by name —
    // so the blank check did not merely relabel a rule that was already firing.
    refuses(&base, &["--produced-by is REQUIRED"]);
}

/// **The default sweep wire must not move.** `grid` with one of the four classic `--rank-by`
/// metrics is the ONE combination whose rows have their `score` CLEARED
/// (`crates/vike-backtest/src/harness/optimize.rs`'s `report_from_outcome`, `RankBy::Metric` arm),
/// and it is EXACTLY what `crates/vike-cli/src/cmd/backtest.rs`'s `--local` arm spawns and prints
/// verbatim.
///
/// ⚠ Two things would move if the collapse built one uniform objective evaluator: every row would
/// gain a `score` key, AND `rank_by` would change STRING for three of the four metrics —
/// `RankMetric` serializes `snake_case` (`total_return`) while `RankMetric::name` answers with the
/// CLI short name (`return`), and `RankBy` is `#[serde(untagged)]`. Hence the evaluator constructor
/// is keyed on the PAIR (method, rank), not on `--rank-by` alone.
///
/// This one needs REAL DATA: a failed point keeps `score: None` on every path, so an empty store
/// would make the assertion vacuous. `data seed-demo` writes the tape this fixture's `[data]` names.
#[test]
fn the_classic_grid_metric_wire_is_unchanged_and_the_others_still_score() {
    let (_dir, profile, store) = scratch(SWEEP_PROFILE);
    let seed = run(&["data", "seed-demo", "--store", &store]);
    assert!(seed.status.success(), "seeding the demo tape: {:?}", stderr_of(&seed));

    let classic = run(&[&profile, "--store", &store, "--rank-by", "return", "--json"]);
    let stdout = String::from_utf8_lossy(&classic.stdout).to_string();
    assert!(
        classic.status.success(),
        "a classic grid sweep must exit 0: {:?}",
        stderr_of(&classic)
    );
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("--json is a JSON document");
    assert_eq!(
        doc.get("rank_by").and_then(serde_json::Value::as_str),
        Some("total_return"),
        "the classic grid path serializes `rank_by` as the METRIC (snake_case), not as the CLI \
         short name — this is the wire `vike-cli backtest --local` prints verbatim; doc: {doc}"
    );
    let rows = doc.get("rows").and_then(serde_json::Value::as_array).expect("a rows array");
    assert!(!rows.is_empty(), "the seeded tape must produce rows; doc: {doc}");
    assert!(
        rows.iter().all(|r| r.get("score").is_none()),
        "…and carries NO `score` key on any row; doc: {doc}"
    );

    // euler under the SAME metric has always stamped a score, and still must: its rank is an
    // OBJECTIVE labelled with the metric's name. Keying the constructor on `--rank-by` alone would
    // have silently retracted that instead.
    let euler = run(&[
        &profile,
        "--store",
        &store,
        "--rank-by",
        "return",
        "--optimizer",
        "euler",
        "--euler-depth",
        "1",
        "--json",
    ]);
    let stdout = String::from_utf8_lossy(&euler.stdout).to_string();
    assert!(euler.status.success(), "a euler sweep must exit 0: {:?}", stderr_of(&euler));
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("--json is a JSON document");
    assert_eq!(
        doc.get("rank_by").and_then(serde_json::Value::as_str),
        Some("return"),
        "euler labels its objective with `RankMetric::name`, which is the CLI short name; doc: {doc}"
    );
    let rows = doc.get("rows").and_then(serde_json::Value::as_array).expect("a rows array");
    assert!(
        rows.iter().any(|r| r.get("score").is_some()),
        "…and euler's rows carry a `score`, which they always have; doc: {doc}"
    );
}

// ---------------------------------------------------------------- ruling 12: the five flags moved

/// **Every retired data flag refuses by name and names its replacement**, in BOTH written
/// spellings, and opens nothing.
///
/// ⚠ The inline half is the one that matters for a script: `--fetch=binance:BTCUSDT:1h` is a
/// spelling `arg` accepts and `has_flag` never sees, so a retirement written with `has_flag` alone
/// would let exactly the automated callers through — silently running the old behaviour under a
/// binary that claims the flag is gone. `refuse_a_retired_data_flag` goes through `flag_given`
/// for that reason and this is the proof.
///
/// ⚠ **The engine-side needle is the WHOLE `backtest data <sub>` phrase, and it has to be.** This
/// row read `sub` alone and was satisfied by the wrong answer: the message derived its subcommand
/// as `flag.trim_start_matches("--")`, so `--rm-series` named `backtest data rm-series` — not a
/// subcommand — and `"rm-series"` contains `"rm"`, so a substring assertion passed on a refusal
/// that sent an operator to a command the same binary refuses. The per-row half of the check is
/// `crates/vike-backtest/src/backtest_cli.rs`'s `the_retired_sub_is_a_real_data_subcommand`, which
/// compares against `DATA_SUBS` rather than against this message.
#[test]
fn every_retired_data_flag_refuses_and_names_the_verb_it_moved_to() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 scratch path");
    for (flag, sub) in [
        ("--fetch", "fetch"),
        ("--fetch-starter", "fetch-starter"),
        ("--seed-demo", "seed-demo"),
        ("--export", "export"),
        ("--rm-series", "rm"),
    ] {
        let engine_spelling = format!("backtest data {sub}`");
        let inline = format!("{flag}=x");
        for written in [flag.to_string(), inline] {
            refuses(
                &[written.as_str(), "--store", never_str],
                &["is retired", "vike-cli data", engine_spelling.as_str()],
            );
            assert!(
                !Path::new(never_str).exists(),
                "a retirement is argv triage and must open nothing, but {never_str:?} was created \
                 by {written:?}"
            );
        }
    }
}

/// ⚠ **The delete's retirement says NOTHING WAS DELETED in as many words.** An operator whose
/// cleanup script starts exiting 2 must not be left reading the refusal as "it may have partially
/// run" — which is the one thing a *fetch*'s refusal does not owe and a *delete*'s does.
#[test]
fn the_rm_retirement_says_nothing_was_deleted() {
    let out = run(&["--rm-series", "--kind", "bar", "--venue", "binance", "--yes"]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr:?}");
    assert!(stderr.contains("NOTHING WAS DELETED"), "stderr: {stderr:?}");
}

/// **`data` routes only in FIRST position, and anywhere else it is refused by name.** Ruling 12's
/// subcommand and ruling 14's positional profile collide on exactly one word; without this arm
/// `backtest --json data rm …` loads a profile called `data` and fails on a path the operator
/// never typed.
#[test]
fn a_data_positional_that_is_not_first_is_refused_by_name() {
    let out = run(&["--json", "data"]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr:?}");
    assert!(
        stderr.contains("SUBCOMMAND") && stderr.contains("must come FIRST"),
        "the refusal says where the word goes rather than reporting a missing file: {stderr:?}"
    );
    assert!(
        !stderr.contains("failed to load profile"),
        "…and it must not have tried to LOAD it: {stderr:?}"
    );
}

/// ⚠ **The `--dry-run=1` hole, end to end on the shipped binary.**
///
/// `vike_analytics::binutil::has_flag` is exact-token, so `--dry-run=1` was SILENTLY IGNORED while
/// the `--yes` beside it was honoured: a command line written as a rehearsal performed the
/// deletion. `triage_data_argv` refuses it now, before the store is opened — which is why this
/// asserts the store path was never created as well as the exit code.
#[test]
fn a_boolean_written_with_a_value_is_refused_before_the_store_opens() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 scratch path");
    let out = run(&[
        "data",
        "rm",
        "--kind",
        "bar",
        "--venue",
        "binance",
        "--produced-by",
        "panel_bars:",
        "--store",
        never_str,
        "--dry-run=1",
        "--yes",
    ]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr:?}");
    assert!(stderr.contains("takes no value"), "stderr: {stderr:?}");
    assert!(stderr.contains("rehearsal"), "the message says what it used to do: {stderr:?}");
    assert!(
        !Path::new(never_str).exists(),
        "argv triage must precede the store open, but {never_str:?} was created"
    );
}

// ---------------------------------------------------------------- the FOURTH method (#1751's follow-up)

/// A three-axis numeric grid — 4 x 4 x 4 = 64 points.
///
/// ⚠ Wider than [`SWEEP_PROFILE`]'s two points ON PURPOSE, and the width is what makes the
/// assertions below mean anything. `GeneticConfig::resolve` derives every bound from the SPACE and
/// caps the result at the grid size, so over a two-point grid the genetic searcher evaluates both
/// points — exactly what the exhaustive grid does. "The genetic method ran" would then be
/// indistinguishable from "the grid ran", and a test that cannot tell them apart is a test that
/// pins nothing. At 64 points the derived budget is strictly smaller than the grid, which is what
/// [`the_genetic_method_runs_and_samples_rather_than_enumerating`] measures against the grid's own
/// row count over the same fixture.
///
/// `alpha` and `beta` are keys `buy_hold` IGNORES — `vike_strategy::BuyHold::from_params`'s own doc
/// says unrecognized keys are simply ignored, and `crates/vike-backtest/src/harness/sweep.rs`'s
/// `profile_with_overrides` INSERTS an override rather than requiring the key to pre-exist. That is
/// what lets this fixture carry three axes while the crate's one default-resolvable fixture
/// strategy has a single tunable param.
const GENETIC_PROFILE: &str = r#"
name = "optimizer-cli-genetic"

[data]
venue = "demo"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2025-01-01T00"
to = "2025-07-01T00"

[engine]
cash = 10000.0

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
alpha = 0.0
beta = 0.0

[sweep]
size = [1.0, 2.0, 3.0, 4.0]
alpha = [1.0, 2.0, 3.0, 4.0]
beta = [1.0, 2.0, 3.0, 4.0]
"#;

/// [`GENETIC_PROFILE`]'s grid size, as a number THIS FILE owns rather than one it reads back out of
/// the binary under test — a bound derived from the same code it is checking proves nothing.
const GENETIC_GRID_POINTS: usize = 64;

/// The `rows` array of a `--json` sweep report.
fn rows_of(out: &Output) -> Vec<serde_json::Value> {
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let doc: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json must be a JSON document ({e}); stdout: {stdout:?}"));
    doc.get("rows")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("a rows array; doc: {doc}"))
        .clone()
}

/// **The fourth method RUNS, and it SAMPLES.**
///
/// Two assertions, and the second is the one that cannot be faked. `genetic:` on stderr is the
/// method's own budget line (`crates/vike-backtest/src/harness/genetic.rs`'s `GeneticBudget`
/// `Display` opens with it) and the negative half keeps it from passing on a binary that prints
/// every method's summary. The ROW COUNT is the structural half: the genetic searcher's derived
/// budget over this fixture is strictly below the grid's 64 points, and the grid run beside it is
/// the control that makes that a statement about the METHOD rather than about the fixture — a
/// `--optimizer genetic` that silently fell through to `GridSearch` would produce all 64 and read
/// green on the stderr assertion alone if that line ever became unconditional.
///
/// The store is EMPTY, like this file's other end-to-end runs: every point fails and its row
/// carries an error rather than a report, which changes nothing here — the searcher still runs its
/// loop and still reports its budget (`genetic.rs`'s own all-NaN pins cover that path), and it
/// keeps this test at one store open instead of a demo-tape seed.
///
/// The INLINE spelling on both flags is deliberate: defect (d) was a selector that matched an exact
/// token only, and a fourth method added to that selector inherits the fix or does not.
#[test]
fn the_genetic_method_runs_and_samples_rather_than_enumerating() {
    let (_dir, profile, store) = scratch(GENETIC_PROFILE);

    let genetic = run(&[&profile, "--store", &store, "--optimizer=genetic", "--seed=7", "--json"]);
    let stderr = stderr_of(&genetic);
    assert!(
        genetic.status.success(),
        "a valid genetic run must exit 0; status {:?}, stderr: {stderr:?}",
        genetic.status.code()
    );
    assert!(
        stderr.contains("genetic:"),
        "`--optimizer=genetic` must RUN the genetic searcher and print its budget line; \
         stderr: {stderr:?}"
    );
    assert!(
        !stderr.contains("tpe:") && !stderr.contains("euler:"),
        "…and NEITHER sibling's summary, or the assertion above would pass on a binary that \
         prints them all; stderr: {stderr:?}"
    );

    let sampled = rows_of(&genetic).len();
    assert!(sampled > 0, "a genetic run must produce rows; stderr: {stderr:?}");
    assert!(
        sampled < GENETIC_GRID_POINTS,
        "the genetic searcher's derived budget is strictly below the {GENETIC_GRID_POINTS}-point \
         grid, so it must report FEWER rows than enumeration — {sampled} means it enumerated, \
         which is what a silent fall-through to GridSearch looks like"
    );

    let grid = run(&[&profile, "--store", &store, "--json"]);
    assert!(grid.status.success(), "the grid control must exit 0; stderr: {:?}", stderr_of(&grid));
    assert_eq!(
        rows_of(&grid).len(),
        GENETIC_GRID_POINTS,
        "…and the CONTROL: the exhaustive grid over the same fixture evaluates every point, which \
         is what makes {sampled} a fact about the METHOD rather than about the profile"
    );
}

/// **A genetic run is reproducible from its seed, and the seed it reports is the one that was
/// typed.**
///
/// The byte-equality half is the reproducibility claim as an operator can check it: same argv,
/// same document. It is scoped exactly as `genetic.rs`'s module doc scopes it — same box, same
/// store — which is all a single-box test can assert anyway.
///
/// ⚠ The second half is the one that catches the plausible WRONG implementation. A CLI arm that
/// built `GeneticConfig::new(0)` and threw the operator's `--seed` away would pass every assertion
/// above — it is perfectly reproducible, just not from the seed. `GeneticBudget`'s `Display`
/// carries `seed {}`, so the run states which one it used, and the needle carries the trailing `;`
/// from that format string rather than matching a bare `seed 1` that `seed 12` would also satisfy.
#[test]
fn a_genetic_run_is_reproducible_and_reports_the_seed_it_was_given() {
    let (_dir, profile, store) = scratch(GENETIC_PROFILE);
    let argv = [
        profile.as_str(),
        "--store",
        store.as_str(),
        "--optimizer",
        "genetic",
        "--seed",
        "7",
        "--json",
    ];
    let first = run(&argv);
    let second = run(&argv);
    assert!(
        first.status.success() && second.status.success(),
        "both runs must exit 0; stderr: {:?} / {:?}",
        stderr_of(&first),
        stderr_of(&second)
    );
    assert_eq!(
        first.stdout, second.stdout,
        "the same seed over the same store must produce a BYTE-IDENTICAL report"
    );

    for seed in ["1", "2"] {
        let out = run(&[&profile, "--store", &store, "--optimizer", "genetic", "--seed", seed]);
        let stderr = stderr_of(&out);
        assert!(
            stderr.contains(&format!("seed {seed};")),
            "the budget line must report the seed the operator TYPED — a hard-coded \
             GeneticConfig::new(0) at the call site passes every other assertion in this file; \
             stderr: {stderr:?}"
        );
    }
}

/// **`--optimizer genetic` with no `--seed` is REFUSED, by name.**
///
/// `GeneticConfig::new(seed)` takes the seed as a required parameter deliberately — its own doc
/// names what a defaulted one costs — and this binary is the only caller that could hand it a
/// constant. Writing `GeneticConfig::new(0)` in `parse_search_flags` would re-introduce at the call
/// site exactly what that signature refuses, one layer up, and nothing anywhere would express the
/// requirement any more. So the CLI carries it: the absence of the flag is the refusal.
///
/// ⚠ The last assertion is what makes the first two mean "the SEED is missing" rather than "the
/// method is unknown" — which is also what today's binary answers to this argv, by refusing
/// `genetic` as an invalid `--optimizer` value. With a seed the same argv gets PAST argv triage and
/// fails on the profile instead.
#[test]
fn the_genetic_method_refuses_to_run_without_a_seed() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 scratch path");

    refuses(
        &["no-such-profile.toml", "--store", never_str, "--optimizer", "genetic"],
        &["--seed", "genetic"],
    );
    assert!(
        !Path::new(never_str).exists(),
        "the missing-seed refusal is argv triage and must open nothing, but {never_str:?} was \
         created"
    );
    // The inline spelling of the selector reaches the same rule.
    refuses(&["no-such-profile.toml", "--optimizer=genetic"], &["--seed"]);

    // …and WITH a seed, the identical argv is past triage — it fails on the PROFILE. This is what
    // separates "the seed is required" from "the method does not exist".
    let out = run(&["no-such-profile.toml", "--optimizer", "genetic", "--seed", "7"]);
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("failed to load profile"),
        "`--optimizer genetic --seed 7` must be a VALID selection that then fails on the missing \
         profile; stderr: {stderr:?}"
    );
}

/// **`--seed` is the first flag with TWO owners, and the ownership rule did not weaken to hold
/// it.**
///
/// Accepted by tpe AND genetic; refused by grid, by euler and by the implicit grid — and the
/// refusal names BOTH owners, so an operator who guessed wrong is told every method the flag would
/// have worked under rather than only the first row of the table.
///
/// The values are VALID for both owners on purpose, so nothing but the ownership rule can produce
/// the refusals.
#[test]
fn the_seed_belongs_to_tpe_and_genetic_and_to_nobody_else() {
    for method in ["tpe", "genetic"] {
        let out = run(&["no-such-profile.toml", "--optimizer", method, "--seed", "7"]);
        let stderr = stderr_of(&out);
        assert!(
            stderr.contains("failed to load profile"),
            "--seed must be ACCEPTED by its owner {method} — this argv must reach the profile \
             load, not an ownership refusal; stderr: {stderr:?}"
        );
    }
    for method in ["grid", "euler"] {
        refuses(
            &["no-such-profile.toml", "--optimizer", method, "--seed", "7"],
            &["--seed", "tpe", "genetic"],
        );
    }
    // No `--optimizer` at all is the GRID, and a knob for an unchosen method is still one.
    refuses(&["no-such-profile.toml", "--seed", "7"], &["--seed", "tpe", "genetic"]);
    // The inline spelling, which is exactly where an ownership check written with `has_flag` leaks
    // — and a two-owner row is a new chance to write that check wrong.
    refuses(&["no-such-profile.toml", "--optimizer", "euler", "--seed=7"], &["--seed", "genetic"]);
}

/// **A knob another method owns is still refused when `genetic` is named** — the fourth column of
/// the ownership matrix, which a table widened to owner SETS could plausibly have opened up.
///
/// `3` and `8` are valid values for euler and tpe respectively, so only the ownership rule can
/// refuse them here.
#[test]
fn a_knob_another_method_owns_is_refused_when_genetic_was_named() {
    refuses(
        &["no-such-profile.toml", "--optimizer", "genetic", "--seed", "7", "--euler-depth", "3"],
        &["--euler-depth", "euler"],
    );
    refuses(
        &["no-such-profile.toml", "--optimizer", "genetic", "--seed", "7", "--trials", "8"],
        &["--trials", "tpe"],
    );
    // ⚠ The ownership refusal comes BEFORE the missing-seed rule, and this pins WHICH of the two an
    // operator gets when both apply. Either answer is defensible; what is not defensible is the two
    // rules racing, which is how a refusal starts changing with an unrelated edit to the table.
    refuses(&["no-such-profile.toml", "--optimizer", "genetic", "--trials", "8"], &["--trials"]);

    // The control, and the reason this test is not vacuous: the SAME argv without the foreign knob
    // is past triage and fails on the profile, so each refusal above is about the knob rather than
    // about the method.
    let out = run(&["no-such-profile.toml", "--optimizer", "genetic", "--seed", "7"]);
    let stderr = stderr_of(&out);
    assert!(stderr.contains("failed to load profile"), "stderr: {stderr:?}");
}
