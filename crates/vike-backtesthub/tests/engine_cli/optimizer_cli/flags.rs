//! Argv triage: the search-flag defects (a)-(e), the positional profile, retired data flags.

use super::*;

// ---------------------------------------------------------------- defect (d), end to end

/// **Defect (d).** The inline spelling must reach the method selector — and the method that RUNS
/// must be the one that was named.
///
/// The discriminator is the METHOD's own stderr summary, not the exit code: every one of these
/// exits 0 and prints a valid ranked report either way, which is exactly why the defect was
/// invisible. `tpe` and `euler` each print a cost line; the grid prints none, and the third case is
/// what keeps the first two from passing on a binary that prints both lines unconditionally.
///
/// The store is EMPTY on purpose (served by a scratch datahub). Every sweep point then fails and its row carries an error rather
/// than a report — which changes nothing under test here: all three methods still run their loops
/// (`crates/vike-backtest/src/search.rs`'s `euler_search_batched` pins
/// `all_nan_search_still_returns_a_best_and_terminates`) and still print their summaries. It keeps
/// this test at one store open instead of a demo-tape seed.
#[test]
fn an_inline_optimizer_value_selects_that_method_not_the_grid() {
    let (_dir, profile, store) = scratch(PARAMSCAN_PROFILE);
    let hub = local_datahub::serve(Path::new(&store));

    let tpe = run_via(&hub, &[&profile, "--optimizer=tpe", "--trials=3"]);
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

    let euler = run_via(&hub, &[&profile, "--optimizer=euler", "--euler-depth=1"]);
    let stderr = stderr_of(&euler);
    assert!(euler.status.success(), "a valid euler run must exit 0; stderr: {stderr:?}");
    assert!(
        // `crates/vike-backtest/src/search/euler.rs`'s `SearchEulerBudget` `Display` opens with
        // `euler:`; the colon is what makes the grid's NEGATIVE assertion below meaningful rather
        // than a match on any line that happens to mention the word.
        stderr.contains("euler:"),
        "`--optimizer=euler` must RUN euler and print its budget line; stderr: {stderr:?}"
    );

    let grid = run_via(&hub, &[&profile, "--optimizer=grid"]);
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
    // the bare token. `clap` refuses the bare token itself now (the hand-rolled `required_value` guard is deleted).
    refuses(&["no-such-profile.toml", "--optimizer"], &["--optimizer", "grid", "euler", "tpe"]);
}

/// **The knob half of the same hole.** A value-less knob under the method that OWNS it silently ran
/// that knob's DEFAULT — `--trials` → `TpeConfig::DEFAULT_TRIALS`, `--euler-depth` →
/// `SearchEulerConfig::DEFAULT_MAX_DEPTH`, `--seed` → `0` — while the same argv under a non-owning method
/// was already refused, because the ownership check read the flag by presence. One flag, two fates,
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
/// signed off. `SearchEulerConfig::with_depth` clamps at `MAX_DEPTH_CAP`, and the budget line then
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
    let (_dir, profile, store) = scratch(PARAMSCAN_PROFILE);
    let hub = local_datahub::serve(Path::new(&store));
    for depth in ["0", "1", "16"] {
        let out = run_via(&hub, &[&profile, "--optimizer", "euler", "--euler-depth", depth]);
        assert!(
            out.status.success(),
            "--euler-depth {depth} is in range and must run; stderr: {:?}",
            stderr_of(&out)
        );
    }
}

// ---------------------------------------------------------------- defect (e)

/// **Defect (e).** A search asked for against a profile with no `[paramscan]` table used to run ONE
/// ordinary backtest and exit 0, silently discarding every search flag.
///
/// `--rank-by` deliberately keeps its DOCUMENTED ignore (`crates/vike-backtesthub/src/backtest_cli.rs`'s
/// module doc: "ignored — not an error — on a non-sweep profile"). The distinction is real rather
/// than a rationalisation: `--rank-by` names how to ORDER results and is meaningless-but-harmless
/// with nothing to order, while `--optimizer` names WHAT WORK TO DO.
///
/// ⚠ **The needle is `[paramscan]`, and the refusal said `[sweep]` until this branch.** The
/// section was renamed by ruling R2 while this binary's own usage text and refusals were left
/// naming the old spelling — deliberately at the time, and withdrawn here, because a surface that
/// tells an operator to write the section under a name it has renamed is a rename half done.
/// ⚠ It is a rename of what the binary SAYS and not of what it ACCEPTS: `[sweep]` is a permanent
/// serde alias (`harness::profile`'s `#[serde(default, alias = "sweep")]`), every profile spelling
/// it still loads, and `crates/vike-backtesthub/tests/engine_cli/search_persist_cli.rs`'s fixture keeps that
/// spelling on purpose so the alias has a live test.
#[test]
fn a_search_on_a_profile_with_no_sweep_table_is_refused_rather_than_ignored() {
    let (_dir, profile, store) = scratch(SINGLE_PROFILE);
    for argv in [
        vec![profile.as_str(), "--optimizer", "tpe", "--trials", "500"],
        vec![profile.as_str(), "--optimizer", "grid"],
        // ⚠ `--optimizer euler` is spelled out rather than leaving the method implicit: a bare
        // `--euler-depth 2` is refused EARLIER, by the ownership rule, because the implicit method
        // is the grid and the grid does not own that knob. That is the right answer to that argv —
        // it just is not this test's question.
        vec![profile.as_str(), "--optimizer", "euler", "--euler-depth", "2"],
    ] {
        refuses(&argv, &["[paramscan]"]);
    }

    // …and `--rank-by` alone is still IGNORED on the same profile, exit 0 — a real run, so it reads
    // its (empty) store through a scratch datahub.
    let hub = local_datahub::serve(Path::new(&store));
    let out = run_via(&hub, &[&profile, "--rank-by", "return"]);
    assert!(
        out.status.success(),
        "--rank-by keeps its documented ignore on a single-point profile; stderr: {:?}",
        stderr_of(&out)
    );
}

// ---------------------------------------------------------------- ruling 14, and the argv[0] shim

/// **Ruling 14: the profile is POSITIONAL** — and the shipped standalone binary must read it,
/// which is the ONLY thing that gates `crates/vike-backtesthub/src/bin/backtesthub.rs`'s `skip(1)`.
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

    // A valued flag's VALUE is not a positional. (`--store DIR` was the example until it was
    // refused on 2026-09-25; `--rank-by` is valued on the same path and still accepted.)
    let out = run(&["--rank-by", "sharpe", "no-such-profile.toml"]);
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("failed to load profile") && stderr.contains("no-such-profile.toml"),
        "`--rank-by sharpe` puts a bare token in argv that is emphatically not a profile; \
         stderr: {stderr:?}"
    );

    // Both spellings at once is a REFUSAL, not a precedence rule: two spellings that may name two
    // different files is the same defect class as two searcher selectors.
    refuses(&["--profile", "a.toml", "b.toml"], &["a.toml", "b.toml"]);
    refuses(&["a.toml", "b.toml"], &["a.toml", "b.toml"]);

    // The KEPT pin: `--profile` alone is unchanged, because `vike-cli backtest run --local` spawns
    // exactly that. `crates/vike-backtesthub/tests/engine_cli/help_cli.rs`'s
    // `a_missing_profile_still_exits_non_zero_on_stderr` holds the other half.
    let out = run(&["--profile", "no-such-profile.toml"]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr.contains("failed to load profile"), "stderr: {stderr:?}");
}

// ---------------------------------------------------------------- the two design properties

/// **A refused flag never dials the datahub.** The child is pointed at a [`silent_hub`] that only
/// witnesses, and a connection left in its backlog is a hard, checkable proof that argv was NOT
/// triaged before any I/O — the property every cheap test in this file rests on, and the thing that
/// makes a mistyped flag cost nothing.
///
/// ⚠ This was `a_refused_flag_never_opens_the_store`, proved by a store directory's ABSENCE: the
/// run path opened one locally until the owner closed the local READ door on 2026-09-25, and a
/// directory nothing can create any more proves nothing. The datahub is the one thing a run reaches
/// for now, so it is the thing witnessed.
#[test]
fn a_refused_flag_never_dials_the_datahub() {
    let (witness, addr) = silent_hub();
    refuses_via(&addr, &["no-such-profile.toml", "--optimizer", "bogus"], &["--optimizer"]);
    assert_never_dialled(&witness, "an argv refusal");
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
/// would make the assertion vacuous. The demo tape holds the slice this fixture's `[data]` names.
#[test]
fn the_classic_grid_metric_wire_is_unchanged_and_the_others_still_score() {
    let (_dir, profile, store) = scratch(PARAMSCAN_PROFILE);
    // The seed WRITES the store directly; the runs READ it through a datahub.
    local_datahub::seed_demo(Path::new(&store));
    let hub = local_datahub::serve(Path::new(&store));

    // The two sweeps only READ the seeded store, each from its own scratch project, so they run at
    // once — the datahub serves each connection on its own thread — and are judged below in the
    // order they always were. Each is a several-second debug-build sweep, the bulk of this test.
    let (classic, euler) = std::thread::scope(|scope| {
        let classic = scope.spawn(|| run_via(&hub, &[&profile, "--rank-by", "return", "--json"]));
        let euler = scope.spawn(|| {
            run_via(
                &hub,
                &[
                    &profile,
                    "--rank-by",
                    "return",
                    "--optimizer",
                    "euler",
                    "--euler-depth",
                    "1",
                    "--json",
                ],
            )
        });
        (classic.join().expect("the classic sweep's thread"), euler.join().expect("euler's thread"))
    });
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
         short name — this is the wire `vike-cli backtest run --local` prints verbatim; doc: {doc}"
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

// ---------------------------------------------------------------- the store verbs left this binary

/// ⚠ **SKEW, the new engine under an old `vike-cli`, and the old lines a script still types.** This
/// binary owns no store now, so `data seed-demo`, `data rm`, `data repair` and the five old flags stop
/// being read: each is refused at the argv with exit 2, and opens, creates and dials nothing. The
/// reverse half is `crates/vike-datahub/tests/datafusion_serve/store_cli/argv_pins.rs`'s
/// `an_old_clients_data_verb_is_refused_and_opens_nothing`.
#[test]
fn the_retired_data_verbs_are_refused_and_open_nothing() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let never = dir.path().join("never");
    let never_str = never.to_str().expect("a UTF-8 scratch path");
    let (witness, hub) = silent_hub();
    for args in [
        vec!["data", "seed-demo", "--store", never_str],
        vec!["data", "fetch-starter", "--store", never_str],
        vec!["data", "fetch", "binance:BTCUSDT:1h", "--days", "180", "--store", never_str],
        vec!["data", "rm", "--kind", "bar", "--venue", "b", "--yes", "--store", never_str],
        vec!["data", "repair", "--kind", "bar", "--venue", "b", "--yes", "--store", never_str],
        vec!["--seed-demo", "--store", never_str],
        vec!["--fetch-starter", "--store", never_str],
        vec!["--rm-series", "--kind", "bar", "--venue", "b", "--yes", "--store", never_str],
    ] {
        let out = run_via(&hub, &args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {:?}", String::from_utf8_lossy(&out.stdout));
        assert!(!never.exists(), "{args:?} created the store it was refused for");
    }
    assert_never_dialled(&witness, "a retired data verb");
}

// ---------------------------------------------------------------- `--archive`, deleted

/// **`--archive` is GONE** (the owner's ruling of 2026-11-06, decision 0084's amendment of that day),
/// so every spelling is the ordinary unknown-flag refusal: exit 2, the flag named, no history dialled
/// and no run saved. Its value is not read as a profile, and nothing runs over the datahub instead:
/// somebody typing it believes a local directory will be read.
#[test]
fn the_deleted_archive_flag_is_refused_and_nothing_runs() {
    let (_dir, profile, _store) = scratch(SINGLE_PROFILE);
    let project = engine::Scratch::project();
    let (witness, hub) = silent_hub();
    project.dial(&hub);
    for args in [
        vec!["--archive=some_dir", profile.as_str()],
        vec!["--archive", "some_dir", profile.as_str()],
        vec![profile.as_str(), "--archive", "some_dir"],
    ] {
        let out = project
            .engine()
            .args(&args)
            .output()
            .unwrap_or_else(|e| panic!("run backtest {args:?}: {e}"));
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "`backtest {args:?}`: {stderr:?}");
        assert!(stderr.contains("--archive"), "the flag is named: {stderr:?}");
        assert!(project.run_dirs().is_empty(), "saved {:?}: {stderr:?}", project.run_dirs());
        assert_never_dialled(&witness, "a command line with --archive");
    }
}
