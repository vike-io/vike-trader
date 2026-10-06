//! Script injection, ruling 7's route onto the three wire verbs, and the sub-verb spine.

use super::*;

#[test]
fn inject_sets_src_and_preserves_existing_params() {
    let profile =
        "[strategy]\nname = \"rhai\"\n[strategy.params]\nqty = 2.0\n\n[data]\nvenue = \"sim\"\n";
    let out = inject_script_src(profile, "fn on_bar() { buy(1.0); }").unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["strategy"]["params"]["src"].as_str(), Some("fn on_bar() { buy(1.0); }"));
    assert_eq!(v["strategy"]["params"]["qty"].as_float(), Some(2.0)); // knob preserved
    assert_eq!(v["strategy"]["name"].as_str(), Some("rhai")); // rest of the profile intact
    assert_eq!(v["data"]["venue"].as_str(), Some("sim"));
}

#[test]
fn inject_creates_strategy_params_when_absent() {
    let out = inject_script_src("[data]\nvenue = \"sim\"\n", "fn on_bar() {}").unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["strategy"]["params"]["src"].as_str(), Some("fn on_bar() {}"));
}

#[test]
fn inject_overwrites_a_prior_inline_src() {
    let profile = "[strategy]\nname = \"rhai\"\n[strategy.params]\nsrc = \"OLD\"\n";
    let out = inject_script_src(profile, "NEW").unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["strategy"]["params"]["src"].as_str(), Some("NEW"));
}

#[test]
fn inject_rejects_malformed_profile_toml() {
    assert!(inject_script_src("this is [not valid", "fn on_bar() {}").is_err());
}

// ---- the route: ruling 7's two axes, onto the three wire verbs -----------------------------

const BASE: &str = "name = \"t\"\n[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\n\
                        kind = \"bar\"\ninterval = \"1d\"\nfrom = \"0\"\nto = \"1\"\n\
                        [engine]\ncash = 1000.0\n[strategy]\nname = \"buy_hold\"\n";

/// [`BASE`] with `<key> = 3` at the TOP LEVEL — the WRONG-SHAPE case, which both presence
/// tests answer differently from an absent key.
///
/// ⚠ It PREPENDS, and that is load-bearing rather than style. `BASE` ends inside `[strategy]`,
/// so appending `sweep = 3` puts the key in THAT table and `doc.get("sweep")` answers `None` —
/// the ABSENT case, not the wrong-shaped one. Every such row would then pass while proving
/// nothing, which is what the first writing of these tests did.
fn with_top_level_scalar(key: &str) -> String {
    format!("{key} = 3\n{BASE}")
}

/// Every cell of ruling 7's two-axis table, plus the two shapes this side deliberately hands
/// to the far side's parser.
#[test]
fn the_two_axes_pick_the_wire_verb() {
    // Validated over ONE SLICE: the WHAT axis alone decides the carrier.
    assert_eq!(route_of(BASE, false), Route::Single);
    assert_eq!(route_of(&format!("{BASE}[sweep]\nsize = [1.0, 2.0]\n"), false), Route::Search);
    assert_eq!(
        route_of(&format!("{BASE}[sweep]\n"), false),
        Route::Single,
        "an EMPTY grid is no search"
    );

    // WALKED FORWARD: one carrier for both WHAT values, because `RunWalkforwardProfile` takes
    // the profile TOML whole and the far side's one parser reads the grid for itself.
    assert_eq!(
        route_of(&format!("{BASE}[walkforward]\nn_splits = 4\n"), false),
        Route::Walkforward
    );
    assert_eq!(
        route_of(
            &format!("{BASE}[sweep]\nsize = [1.0, 2.0]\n[walkforward]\nn_splits = 2\n"),
            false
        ),
        Route::Walkforward,
        "the two axes COMPOSE (ruling 5): the pair is not refused and neither section is \
             ignored"
    );

    // ⚠ An EMPTY [walkforward] still routes to the walk-forward verb, unlike an empty grid:
    // `WalkforwardCfg::n_splits` carries no `#[serde(default)]`, so the far side's parser
    // answers with the error naming the missing key — a better message than a guess here.
    assert_eq!(route_of(&format!("{BASE}[walkforward]\n"), false), Route::Walkforward);
    // The wrong SHAPE, and unparseable text: both go to the plain backtest, whose
    // `BacktestProfile::from_toml_str` produces the type error naming the field.
    assert_eq!(route_of(&with_top_level_scalar("walkforward"), false), Route::Single);
    assert_eq!(route_of("this is [not valid", false), Route::Single);
}

/// ⚠ The MIGRATED section name routes too, and that is this stage's whole share of ruling 2.
/// The rename gives `[sweep]` a named migration message rather than deleting it, so BOTH names
/// must still load — and a pre-parse that knew only `[paramscan]` would send a legacy profile
/// to `RunBacktest`, which reports ONE point and never mentions the grid. That is exactly the
/// silent divergence [`declares_a_paramscan_grid`] exists to close, re-opened from the other side.
///
/// ⚠ `[sweep]` is still the spelling the ENGINE accepts (`BacktestProfile` is
/// `deny_unknown_fields` and declares only that field), which is why every operator-facing
/// string in this crate still says `[sweep]` while this pre-parse answers to both.
#[test]
fn both_grid_section_names_route_to_the_search() {
    assert_eq!(route_of(&format!("{BASE}[paramscan]\nsize = [1.0, 2.0]\n"), false), Route::Search);
    assert_eq!(route_of(&format!("{BASE}[sweep]\nsize = [1.0, 2.0]\n"), false), Route::Search);
    assert_eq!(
        route_of(&format!("{BASE}[paramscan]\n"), false),
        Route::Single,
        "an EMPTY migrated grid is no search either"
    );
    assert_eq!(
        declares_a_paramscan_grid(&format!("{BASE}[paramscan]\nsize = [1.0]\n")),
        Some(true)
    );
    assert_eq!(declares_a_paramscan_grid(&format!("{BASE}[sweep]\nsize = [1.0]\n")), Some(true));
    // Wrong SHAPE under EITHER name: this side declines to answer (`None`) and the far side's
    // parser produces the type error naming the field.
    assert_eq!(declares_a_paramscan_grid(&with_top_level_scalar("paramscan")), None);
    assert_eq!(declares_a_paramscan_grid(&with_top_level_scalar("sweep")), None);
}

/// The two flags the walk-forward carrier cannot carry are refused BY NAME, and the message
/// names the PROFILE key that does the job instead.
#[test]
fn the_walkforward_route_refuses_a_ranking_flag_by_name() {
    for argv in [
        vec!["--profile", "p.toml", "--rank-by", "sharpe"],
        vec!["--profile", "p.toml", "--optimizer", "grid"],
    ] {
        let args = run_args_of(&argv).unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
        let err = refuse_a_walkforward_flag(&args)
            .expect_err("a ranking flag must be refused on this route");
        assert!(err.contains(argv[2]), "{argv:?} names the flag: {err}");
        assert!(err.contains("[walkforward].rank_by"), "…and where it belongs: {err}");
    }
    // …and a run carrying neither is not refused, which is what stops the guard from being a
    // blanket refusal of the whole route.
    let plain = run_args_of(&["--profile", "p.toml"]).unwrap();
    assert!(refuse_a_walkforward_flag(&plain).is_ok());
}

/// ⚠ **THE CASE DISAGREEMENT, CLOSED ON THIS SIDE — and this is the test that could not exist
/// while the roster did.**
///
/// MEASURED before the fix: `crates/vike-backtest/src/harness/sweep.rs`'s
/// `RankMetric::from_str_ci` lowercases and `crates/vike-backtest/src/harness/search_select.rs`'s
/// `resolve` compares with `eq_ignore_ascii_case`, both pinned by their own tests, while this
/// parser asked `roster.contains(&value)` against a local array. So an operator who learned
/// `--rank-by` from `backtest --help` and then reached for `vike-cli` got a usage error for a
/// spelling the engine documents, accepts and tests. Both selectors now go through
/// `vike_datahub_client::flag_vocab`'s `accept_value`, which owns the roster and the rule.
///
/// The CANONICALISATION is asserted, not just the acceptance: what this side forwards is the
/// member's own spelling, so the wire, a spawned engine and the run artifact all name one
/// thing — that argument is on `accept_value` and on [`Args::optimizer`].
#[test]
fn an_upper_case_selector_is_accepted_and_canonicalised_by_the_shared_vocabulary() {
    let a = run_args_of(&["--profile", "p.toml", "--rank-by", "SHARPE", "--optimizer", "TPE"])
        .expect("the engine accepts both spellings, so this side must too");
    assert_eq!(a.rank_by.as_deref(), Some("sharpe"), "forwarded CANONICAL, not as typed");
    assert_eq!(a.optimizer.as_deref(), Some("tpe"), "forwarded CANONICAL, not as typed");
    // Mixed case, on the member whose spelling carries an underscore.
    let b = run_args_of(&["--profile", "p.toml", "--rank-by", "Max_Dd"]).unwrap();
    assert_eq!(b.rank_by.as_deref(), Some("max_dd"));
    // ⚠ Case-insensitivity widens the SPELLINGS of a member and never the membership: a
    // near-miss is still a refusal, and the message RENDERS the one roster rather than
    // restating it, so a sixth name cannot be accepted by the parser and missing from the
    // sentence.
    let e = run_args_of(&["--profile", "p.toml", "--rank-by", "sharp"]).unwrap_err();
    assert!(e.contains("--rank-by") && e.contains("sharp"), "names flag and value: {e}");
    for member in flag_vocab::RANK_METRICS {
        assert!(e.contains(member), "the refusal must offer {member}: {e}");
    }
    // ⚠ `--kind` is NOT widened with them, and that is the point of leaving `one_of` alone:
    // its value is written verbatim into the profile TOML, so accepting `BAR` here would hand
    // the far side a `kind = "BAR"` its own deserializer refuses — a local usage error traded
    // for a remote one.
    assert!(
        run_args_of(&["--profile", "p.toml", "--kind", "BAR"]).is_err(),
        "--kind stays EXACT: see `one_of`'s doc"
    );
}

/// ⚠ **Every metric [`print_paramscan`] prints must have a catalog row.** [`cell`]'s `None`
/// arm renders through `Ratio` — no scaling, no suffix — so an id that fell out of
/// `vike_analytics::metric_catalog`'s `METRICS` would quietly go back to publishing `0.0310`
/// for a three-percent drawdown under a `max_dd` header, which is the defect that function's
/// doc records. This is the assertion that keeps the fallback unreachable.
#[test]
fn every_metric_the_ranked_table_prints_has_a_catalog_row() {
    for id in ["final_equity", "total_return", "sharpe", "max_drawdown", "n_trades"] {
        assert!(
            vike_analytics::metric_catalog::spec_for(id).is_some(),
            "`{id}` is a column of the ranked table and the catalog holds no row for it — \
                 `cell` would fall back to Ratio and print a fraction under a percent header"
        );
    }
}

/// ⚠ **The two percent columns of the ranked table read as PERCENTS, and the bare fraction
/// this table used to print fails here.**
///
/// Read the literals as the convention rather than as arithmetic: a `max_drawdown` of `0.031`
/// is `3.1000%`, not `0.0310` — the number this table published for months, beside a `return`
/// column that was scaled and suffixed. The two conventions in one row are what
/// `vike_analytics::metric_catalog::MetricUnit::render` exists to make impossible, and nothing
/// pinned these cells until now: the integration test over this path asserts the banner, the
/// rank name and one override, all of which survive any number being wrong.
#[test]
fn the_ranked_table_renders_both_percent_columns_scaled_and_suffixed() {
    let r: Value = serde_json::from_str(
        r#"{"final_equity": 10500.0, "total_return": 0.05, "sharpe": 1.25,
                "max_drawdown": 0.031, "n_trades": 412}"#,
    )
    .expect("a BacktestReport's compact scalars");
    assert_eq!(cell(&r, "max_drawdown"), "3.1000%", "a drawdown is a PERCENT, never 0.0310");
    assert_eq!(cell(&r, "total_return"), "5.0000%");
    assert_eq!(cell(&r, "sharpe"), "1.2500", "a Ratio takes four decimals and no suffix");
    assert_eq!(cell(&r, "final_equity"), "10500.00", "Money takes two");
    assert_eq!(
        cell(&r, "n_trades"),
        "412",
        "a Count carries none — 412.0000 trades is not a thing"
    );
    // A `null` or absent metric still RENDERS rather than breaking the table — [`num`]'s rule,
    // unchanged by the migration, and the reason a degenerate search still prints its rows.
    let degenerate: Value = serde_json::from_str(r#"{"sharpe": null}"#).unwrap();
    assert_eq!(cell(&degenerate, "sharpe"), "0.0000");
    assert_eq!(cell(&degenerate, "max_drawdown"), "0.0000%");
}

// ---- the sub-verb spine (decision 11) ------------------------------------------------------

fn run_args_of(args: &[&str]) -> Result<Args, String> {
    parse_run_args(args.iter().map(|s| (*s).to_string()))
}

/// The first-token router alone — what [`run`] does before it hands the tail to a sub-verb
/// parser. Returns the [`Sub`] it claimed, so a test can assert the NAME without also supplying
/// every flag that sub-verb requires.
fn parse_sub(args: &[&str]) -> Result<Sub, String> {
    claim_subcommand(&mut args.iter().map(|s| (*s).to_string()))
}

/// Every sub-verb is reachable by the name it advertises, and [`claim_subcommand`]'s match and
/// [`SUBCOMMANDS`] agree in BOTH directions. `crate::cmd::data`'s
/// `all_subcommands_are_reachable_by_the_name_they_advertise` is the shape.
///
/// ⚠ The ROUTER is all this drives, so the bare NAME is enough for the reading verbs:
/// [`claim_subcommand`] claims a token and stops, and each verb's own required flags are
/// asserted by `every_reading_subcommand_is_reachable_by_the_name_it_advertises`, which drives
/// [`parse_read`] with the minimum line each one accepts.
#[test]
fn every_subcommand_is_reachable_by_the_name_it_advertises() {
    for sub in SUBCOMMANDS {
        // Parsed with the flags each one REQUIRES, so a refusal can only be about the NAME.
        let argv: Vec<&str> = match sub {
            Sub::Run => vec!["run", "--profile", "p.toml"],
            Sub::Read(r) => vec![r.as_str()],
        };
        let parsed = parse_sub(&argv).unwrap_or_else(|e| panic!("{}: {e}", sub.as_str()));
        assert_eq!(parsed, *sub, "{} parsed as a different subcommand", sub.as_str());
    }
}

/// ⚠ A missing sub-verb names EVERY sub-verb that exists, DERIVED from [`SUBCOMMANDS`]. The
/// hand-written copy this shape replaced in `crate::cmd::data` omitted a subcommand that had
/// shipped months earlier — on the verb that DELETES.
#[test]
fn a_missing_subcommand_names_every_subcommand_that_exists() {
    let err = parse_sub(&[]).unwrap_err();
    for sub in SUBCOMMANDS {
        assert!(err.contains(sub.as_str()), "must name {}: {err}", sub.as_str());
    }
}

/// ⚠ Decision 11: there is no bare form. A flag where a sub-verb belongs must not read as an
/// unknown SUBCOMMAND called `--profile` — it is a flag, and the message says so and names
/// the roster.
#[test]
fn a_flag_where_a_subcommand_belongs_says_so() {
    let err = parse_sub(&["--profile", "p.toml"]).unwrap_err();
    assert!(err.contains("--profile"), "names what was typed: {err}");
    assert!(err.contains("subcommand"), "…and says what was expected: {err}");
    assert!(err.contains("run"), "…and names the roster: {err}");
}

/// ⚠ The one spelling that had shipped for months gets a sentence rather than a shrug:
/// `--list-params` is now `backtest params`.
///
/// ⚠ Two DIFFERENT code paths answer it and both are asserted here. The bare
/// `vike-cli backtest --list-params` is the ROUTER's arm ([`claim_subcommand`]); a
/// `vike-cli backtest run --list-params` gets past the router — `run` is a valid sub-verb —
/// and is answered by [`parse_run_args`]'s own arm. A single-path test would have left
/// whichever half it did not drive answering "unknown argument".
#[test]
fn the_retired_list_params_flag_names_its_replacement() {
    let router = parse_sub(&["--list-params", "--script", "s.rhai"]).unwrap_err();
    assert!(router.contains("--list-params"), "{router}");
    assert!(router.contains("backtest params"), "names the replacement: {router}");

    let inside_run = run_args_of(&["--list-params", "--script", "s.rhai"]).unwrap_err();
    assert!(inside_run.contains("--list-params"), "{inside_run}");
    assert!(inside_run.contains("backtest params"), "names the replacement: {inside_run}");
}

/// `--store` is REFUSED on BOTH arms since 2026-09-25 (decision 0084: every history read goes
/// through a datahub), with the one command that replaces it named — and the sentence is the one
/// the published surface carries, so the table cannot drift from the parser. All three spellings
/// are driven because the arm fires before `--local` is consulted and before any value is read.
#[test]
fn the_store_flag_is_refused_on_both_arms_with_its_replacement() {
    let row = crate::surface::FLAGS.iter().find(|f| f.long == "--store").expect("the row");
    assert_eq!(row.status, crate::surface::Status::Retired);
    for argv in [
        &["--profile", "p.toml", "--store", "/data"][..],
        &["--local", "--profile", "p.toml", "--store", "/data"][..],
        &["--local", "--profile", "p.toml", "--store=/data"][..],
    ] {
        let err = run_args_of(argv).unwrap_err();
        assert!(err.contains("VIKE_DATAHUB_STORE=DIR vike-backend datahub"), "{argv:?}: {err}");
        assert!(
            row.refusals.iter().any(|r| r.message == err),
            "the published row carries the parser's sentence verbatim: {err}"
        );
    }
}

/// Every sub-verb is NAMED in the usage text. `crate::cmd::data`'s `Sub` doc states the
/// contract as three places — an arm in the router, an arm in [`run`], a row in [`USAGE`] —
/// and this is the third one, machine-checked.
///
/// ⚠ It exists because the gate that looks closest to it is weaker than it appears:
/// `crate::cmd::mcp`'s `the_instructions_name_only_real_commands` asserts
/// `USAGE.contains(word)` for every token after `vike-cli backtest`, and `run` is already a
/// substring of `<run.toml>` — so that gate would pass a USAGE that never advertises the
/// sub-verb at all.
#[test]
fn every_subcommand_is_named_in_usage() {
    for sub in SUBCOMMANDS {
        let spelled = format!("vike-cli backtest {}", sub.as_str());
        assert!(USAGE.contains(&spelled), "USAGE must offer `{spelled}`:\n{USAGE}");
    }
}

/// A sample value for a flag that takes one, or `None` for a bare boolean. The ONE place this
/// test knows a flag's shape; the ROSTER itself is [`PARAMS_REFUSED`], which the parser
/// reads too, so the two cannot drift.
fn sample_value_for(flag: &str) -> Option<&'static str> {
    match flag {
        "--local" => None,
        "--profile" => Some("p.toml"),
        "--preset" => Some("fast.toml"),
        "--set" => Some("engine.cash=1000"),
        "--engine" => Some("/opt/backtest"),
        "--rank-by" => Some("sharpe"),
        "--optimizer" => Some(DEFAULT_OPTIMIZER),
        "--euler-depth" => Some("2"),
        "--trials" => Some("8"),
        "--seed" => Some("1"),
        "--venue" => Some("binance"),
        "--symbol" => Some("BTCUSDT"),
        "--interval" => Some("1d"),
        "--from" => Some("0"),
        "--to" => Some("1"),
        "--kind" => Some("bar"),
        "--cash" => Some("1000"),
        "--fee" => Some("0.001"),
        "--slippage" => Some("0.0"),
        // `sequential` is the ABSENT-key default, so it is the one value that can never be
        // refused by `decide_mode`'s combination checks in `BacktestProfile::refusals`.
        "--decide" => Some("sequential"),
        "--param" => Some("size=1.5"),
        "--write-profile" => Some("out.toml"),
        "--show-effective" => None,
        "--explain-data" => None,
        "--require-coverage" => None,
        "--max-gap" => Some("1d"),
        "--on-gap" => Some("warn"),
        "--universe" => Some("strict"),
        other => panic!(
            "{other} joined PARAMS_REFUSED without a sample value here — add its arm, do \
                 not delete the check"
        ),
    }
}

/// `params` refuses what it cannot honour, BY NAME — the same debt `--list-params` paid as a
/// third mode inside one parser. `crate::cmd::params::run_params` reads one file, or the
/// built-in roster, and lists the knobs it declares — opening no socket, locating no engine and
/// touching no store — so accepting one of these would leave an operator believing their
/// discovery consulted something it never touched.
///
/// ⚠ It iterates [`PARAMS_REFUSED`] — the SAME roster [`parse_read`] drives its refusal from
/// through [`refuse_a_run_flag_on_params`] — rather than a second literal list, so a flag added
/// to one is added to both.
///
/// ⚠ It drives [`parse_read`] rather than a params-only parser, because stage 3's
/// `parse_params_args` did not survive the merge with the reading plane: `params` is one of the
/// reading sub-verbs sharing ONE flag loop now. What survived is the roster and the sentence.
#[test]
fn params_refuses_every_flag_on_the_roster() {
    for flag in PARAMS_REFUSED {
        let mut argv = vec!["params".to_string(), "--script".to_string(), "s.rhai".to_string()];
        argv.push((*flag).to_string());
        if let Some(v) = sample_value_for(flag) {
            argv.push(v.to_string());
        }
        let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
        let Err(err) = parse_read(&borrowed) else {
            panic!("{flag} must be refused under `params`, not silently dropped");
        };
        assert!(err.contains(flag), "the message names the flag: {err}");
        assert!(err.contains("params"), "…and the subcommand that refused it: {err}");
    }
}

/// ⚠ The two rows that LEFT [`PARAMS_REFUSED`] when the stages merged are accepted here, and
/// this is the assertion that would catch them being put back. Stage 3's `params` took
/// `--script` alone; stage 4's takes `--script | --strategy` and renders a `--json` document,
/// and stage 4's is the one that shipped. A roster row for either would refuse a flag this
/// sub-verb implements — the same defect
/// `crate::cmd::runs::show`'s `the_refusal_roster_no_longer_names_a_renderer_that_ships`
/// guards one shape over.
#[test]
fn params_accepts_the_two_sources_and_the_json_document() {
    for flag in ["--strategy", "--json"] {
        assert!(
            !PARAMS_REFUSED.contains(&flag),
            "{flag} ships on `params` — a roster row would refuse it"
        );
    }
    let a = parse_read(&["params", "--strategy", "rhai", "--json"]).unwrap();
    assert_eq!(a.strategy.as_deref(), Some("rhai"));
    assert!(a.json);
    let b = parse_read(&["params", "--script", "s.rhai"]).unwrap();
    assert_eq!(b.script.as_deref(), Some("s.rhai"));
}

/// ⚠ [`SUBCOMMANDS`] spells the reading rows out because a `const` cannot map over a slice, so
/// this is the completeness test that hand copy owes: every [`READ_SUBCOMMANDS`] entry is in
/// the outer roster, and the outer roster holds nothing but those plus `run`.
///
/// ⚠ **`run` is still the ONE non-reading sub-verb, and the authoring three did not change
/// that.** `templates`, `script-api` and `script-check` all joined [`READ_SUBCOMMANDS`] as well
/// as [`SUBCOMMANDS`] — they compute no backtest, dial nothing and share [`parse_read`]'s flag
/// loop — so the `+ 1` below is unchanged and still means exactly `run`. A verb that computed
/// would have to change this number, and the right response then is to re-argue it here rather
/// than to bump it.
#[test]
fn the_roster_carries_every_reading_subverb() {
    for r in READ_SUBCOMMANDS {
        assert!(
            SUBCOMMANDS.contains(&Sub::Read(*r)),
            "`{}` is a reading sub-verb that SUBCOMMANDS does not name — every refusal that \
                 derives its roster from SUBCOMMANDS would name the set short",
            r.as_str()
        );
    }
    assert_eq!(
        SUBCOMMANDS.len(),
        READ_SUBCOMMANDS.len() + 1,
        "SUBCOMMANDS is `run` plus the reading roster and nothing else"
    );
}

/// ⚠ **`crate::surface::SUB_VERBS` is a HAND COPY of [`SUBCOMMANDS`], and nothing held the two
/// equal.** It is not decoration: it is the INNER LOOP BOUND of
/// [`the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb`], the containment filter
/// `surface.rs` applies to a flag's `applies_to`, and the `sub_verbs` array of the PUBLISHED
/// `cli.json`.
///
/// So the failure was SUBTRACT-ONLY and silent. Over-inclusion was already caught — the audit's
/// `accepts` closure panics on a sub-verb this plane does not have. UNDER-inclusion was caught by
/// nothing: a sub-verb missing from the copy simply removed a whole COLUMN from the flag audit,
/// which then ran shorter, stayed green, and published a roster naming the set short. That is the
/// class `crates/vike-ops/tests/hygiene/path_key_gate.rs` exists for — a rotted row stops matching and
/// nothing can ever go red on it.
///
/// This test lives HERE rather than in `tests/` because [`SUBCOMMANDS`] and [`Sub::as_str`] are
/// private to this module: an integration test would need a new `pub(crate)` accessor first, and
/// the audit loop this guards already reaches both consts from exactly here.
#[test]
fn the_surface_names_exactly_the_sub_verbs_this_plane_dispatches() {
    let real: Vec<&str> = SUBCOMMANDS.iter().map(|s| s.as_str()).collect();

    for sub in &real {
        assert!(
            crate::surface::SUB_VERBS.contains(sub),
            "`{sub}` is a sub-verb this plane dispatches and `surface::SUB_VERBS` does not name \
                 it — the flag audit runs one column short and `cli.json` publishes a short roster"
        );
    }
    for named in crate::surface::SUB_VERBS {
        assert!(
            real.contains(named),
            "`surface::SUB_VERBS` names `{named}`, which this plane does not dispatch — the \
                 audit's `accepts` closure panics on it"
        );
    }
    // Order too, not just membership: this slice IS the published `sub_verbs` array.
    assert_eq!(
        crate::surface::SUB_VERBS,
        real.as_slice(),
        "the surface's hand copy and SUBCOMMANDS must agree in ORDER as well as membership"
    );

    // ⚠ **The SECOND hand copy, and it is the one that gets PUBLISHED.** The
    // `all_subcommands` roster row claims `derived_from: SUBCOMMANDS` in its own evidence and
    // was held equal to it by nothing — it was three sub-verbs short the moment this plane grew
    // `templates`, `script-api` and `script-check`, while `cli.json` advertised the short set to
    // every reader of the published asset. Same class as the copy above, caught the same way.
    let roster = crate::surface::ROSTERS
        .iter()
        .find(|r| r.id == "all_subcommands")
        .expect("the `all_subcommands` roster row exists");
    assert_eq!(
        roster.members, real,
        "the `all_subcommands` roster and SUBCOMMANDS must agree — that row says it is DERIVED \
             from SUBCOMMANDS and it is published in cli.json"
    );
}

/// The roster is not empty and now DOES carry `--profile`.
///
/// ⚠ This assertion is the INVERSE of the one it replaces, and the inversion is the point.
/// Under `--list-params`, `--profile` was excused as unused-and-optional so that a run and a
/// discovery over the SAME command line stayed spellable. Under a sub-verb the two are no
/// longer the same command line, so the excuse is spent and refusing it is the correct
/// tightening — see [`PARAMS_REFUSED`].
#[test]
fn the_params_roster_is_populated_and_now_refuses_the_profile() {
    assert!(!PARAMS_REFUSED.is_empty());
    assert!(
        PARAMS_REFUSED.contains(&"--profile"),
        "a discovery builds no profile, and under a sub-verb there is no shared command line \
             left to keep spellable"
    );
}

/// ⚠ TRANSITIONAL. Stage 2 makes `--profile` optional, so the refusal is no longer "you forgot
/// the flag" but "you gave me nothing to run".
#[test]
fn a_command_line_with_no_profile_and_no_building_flag_is_refused_naming_both_routes() {
    let err = parse_run_args(["--json".to_string()].into_iter()).unwrap_err();
    assert!(err.contains("--profile"), "names the file route: {err}");
    assert!(err.contains("--set"), "…and the flag route: {err}");
}

/// THE INVERSION, as a parser-level pin: flags alone are a complete command line.
#[test]
fn flags_alone_are_a_complete_command_line() {
    let a = parse_run_args(
        ["--set", "engine.cash=1000", "--set", "data.kind=bar"].map(String::from).into_iter(),
    )
    .expect("a flags-only invocation must parse");
    assert!(a.profile_path.is_none());
    assert_eq!(explicit(&a).len(), 2);
}

/// …and so is a `--preset` or a `--script` with no file — each is a profile-building input.
#[test]
fn a_preset_or_a_script_alone_is_also_a_complete_command_line() {
    for argv in [vec!["--preset", "fast.toml"], vec!["--script", "s.rhai"]] {
        parse_run_args(argv.iter().map(|s| (*s).to_string()))
            .unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
    }
}

#[test]
fn script_and_profile_parse_together() {
    let args =
        parse_run_args(["--profile", "p.toml", "--script", "s.rhai"].map(String::from).into_iter())
            .unwrap();
    assert_eq!(args.profile_path.as_deref(), Some("p.toml"));
    assert_eq!(args.script_path.as_deref(), Some("s.rhai"));
}
