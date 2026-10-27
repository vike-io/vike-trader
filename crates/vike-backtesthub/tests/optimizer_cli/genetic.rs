//! The fourth method, `genetic`: it runs, samples, reproduces from its seed, owns `--seed`.

use super::*;

// ---------------------------------------------------------------- the FOURTH method (#1751's follow-up)

/// A three-axis numeric grid — 4 x 4 x 4 = 64 points.
///
/// ⚠ Wider than [`PARAMSCAN_PROFILE`]'s two points ON PURPOSE, and the width is what makes the
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
/// method's own budget line (`crates/vike-backtest/src/search/genetic.rs`'s `GeneticBudget`
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
    let hub = local_datahub::serve(Path::new(&store));

    let genetic = run_via(&hub, &[&profile, "--optimizer=genetic", "--seed=7", "--json"]);
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

    let grid = run_via(&hub, &[&profile, "--json"]);
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
    let hub = local_datahub::serve(Path::new(&store));
    let argv = [profile.as_str(), "--optimizer", "genetic", "--seed", "7", "--json"];
    let first = run_via(&hub, &argv);
    let second = run_via(&hub, &argv);
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
        let out = run_via(&hub, &[&profile, "--optimizer", "genetic", "--seed", seed]);
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
    let (witness, addr) = silent_hub();
    refuses_via(&addr, &["no-such-profile.toml", "--optimizer", "genetic"], &["--seed", "genetic"]);
    assert_never_dialled(&witness, "the missing-seed refusal, which is argv triage,");
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
