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
/// class `crates/vike-ops/tests/path_key_gate.rs` exists for — a rotted row stops matching and
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

// ---- --preset ------------------------------------------------------------------------------

#[test]
fn preset_parses_beside_the_profile_and_the_script() {
    let args = parse_run_args(
        ["--profile", "p.toml", "--preset", "fast.toml", "--script", "s.rhai"]
            .map(String::from)
            .into_iter(),
    )
    .unwrap();
    assert_eq!(args.preset_path.as_deref(), Some("fast.toml"));
    assert_eq!(args.script_path.as_deref(), Some("s.rhai"));
}

/// THE merge: a preset's keys land in `[strategy.params]`, keeping their TOML types, without
/// disturbing anything else the profile said.
#[test]
fn merge_lands_every_knob_in_strategy_params_and_leaves_the_rest_alone() {
    let profile = "[strategy]\nname = \"buy_hold\"\n[strategy.params]\nqty = 1.0\n\n[data]\nvenue = \"sim\"\n";
    let out = merge_preset_params(profile, "size = 3\nsymbol = \"BTCUSDT\"\n").unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["strategy"]["params"]["size"].as_integer(), Some(3));
    assert_eq!(v["strategy"]["params"]["symbol"].as_str(), Some("BTCUSDT"));
    assert_eq!(v["strategy"]["params"]["qty"].as_float(), Some(1.0), "un-preset knob survives");
    assert_eq!(v["strategy"]["name"].as_str(), Some("buy_hold"));
    assert_eq!(v["data"]["venue"].as_str(), Some("sim"));
}

/// The preset WINS over a value the profile already set — it is the more specific instruction,
/// typed on the command line for this run.
#[test]
fn a_preset_key_overrides_the_profiles_own_value() {
    let out = merge_preset_params("[strategy.params]\nsize = 1\n", "size = 9\n").unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["strategy"]["params"]["size"].as_integer(), Some(9));
}

#[test]
fn merge_creates_strategy_params_when_absent() {
    let out = merge_preset_params("[data]\nvenue = \"sim\"\n", "size = 2\n").unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["strategy"]["params"]["size"].as_integer(), Some(2));
}

/// A nested knob table is a legitimate preset (`funding_carry` reads `[venues]` straight out of
/// `[strategy.params]`), so only the EXACT lone-`[params]` wrapper is refused.
#[test]
fn a_nested_knob_table_merges_intact() {
    let out = merge_preset_params(
        "[strategy]\nname = \"funding_carry\"\n",
        "cooldown_ms = 500\n[venues]\nBTCUSDT = \"binance\"\n",
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["strategy"]["params"]["venues"]["BTCUSDT"].as_str(), Some("binance"));
    assert_eq!(v["strategy"]["params"]["cooldown_ms"].as_integer(), Some(500));
}

/// ⚠ The `[params]`-wrapped shape is REFUSED with the fix in the message. Merged as-is it would
/// give the strategy one key nothing reads while every knob silently kept its default.
#[test]
fn a_params_wrapped_preset_is_refused_naming_the_fix() {
    let err = merge_preset_params("[strategy]\nname = \"buy_hold\"\n", "[params]\nsize = 3\n")
        .unwrap_err();
    assert!(err.contains("[params]"), "names the offending header: {err}");
    assert!(err.contains("top level"), "names the fix: {err}");
}

/// …and a preset that legitimately has ONE knob does not trip that rule just by being small.
#[test]
fn a_single_scalar_preset_is_not_mistaken_for_a_wrapper() {
    let out = merge_preset_params("[strategy.params]\n", "params = 3\n").unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(
        v["strategy"]["params"]["params"].as_integer(),
        Some(3),
        "only a lone `params` TABLE is the wrapper shape"
    );
}

/// A preset may not carry `src`: that would smuggle a whole script past `--script`.
#[test]
fn a_preset_that_defines_src_is_refused() {
    let err = merge_preset_params("[strategy.params]\n", "src = \"fn on_bar() {}\"\n").unwrap_err();
    assert!(err.contains("src"), "names the offending key: {err}");
    assert!(err.contains("--script"), "names what to use instead: {err}");
}

#[test]
fn merge_rejects_malformed_preset_and_profile_toml() {
    assert!(merge_preset_params("[strategy.params]\n", "size = = 3").is_err());
    assert!(merge_preset_params("this is [not valid", "size = 3").is_err());
}

/// ORDER: preset first, then `--script`, so a stale `src` in the profile cannot survive and the
/// script file always wins. (A preset carrying `src` is refused before either runs.)
#[test]
fn the_script_wins_over_whatever_the_preset_left_behind() {
    let profile = "[strategy]\nname = \"rhai\"\n[strategy.params]\nsrc = \"OLD\"\n";
    let merged = merge_preset_params(profile, "fast = 5\n").unwrap();
    let out = inject_script_src(&merged, "NEW").unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["strategy"]["params"]["src"].as_str(), Some("NEW"));
    assert_eq!(v["strategy"]["params"]["fast"].as_integer(), Some(5));
}

// ---- --set: value typing --------------------------------------------------------------------

/// `toml::Value` has NO scalar `FromStr` — `"2.5".parse::<toml::Value>()` is a DOCUMENT parse
/// and fails — so every one of these would be a STRING under the obvious implementation.
#[test]
fn a_scalar_value_keeps_its_toml_type() {
    assert_eq!(parse_scalar("2.5").as_float(), Some(2.5));
    assert_eq!(parse_scalar("250").as_integer(), Some(250));
    assert_eq!(parse_scalar("1e5").as_float(), Some(100_000.0));
    assert_eq!(parse_scalar("true").as_bool(), Some(true));
    assert_eq!(parse_scalar("-0.001").as_float(), Some(-0.001));
}

#[test]
fn a_non_toml_value_is_a_string_and_surrounding_space_is_trimmed() {
    assert_eq!(parse_scalar("BTCUSDT").as_str(), Some("BTCUSDT"));
    assert_eq!(parse_scalar("  binance  ").as_str(), Some("binance"));
    assert_eq!(parse_scalar("2026-01-01T00").as_str(), Some("2026-01-01T00"));
    assert_eq!(parse_scalar("").as_str(), Some(""));
}

#[test]
fn an_array_value_parses_as_an_array() {
    let v = parse_scalar(r#"["BTCUSDT", "ETHUSDT"]"#);
    let a = v.as_array().expect("an array");
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].as_str(), Some("BTCUSDT"));
}

// ---- --set: the dotted-key walk -------------------------------------------------------------

fn doc(text: &str) -> toml::Value {
    toml::from_str(text).expect("valid TOML fixture")
}

#[test]
fn a_dotted_key_creates_the_tables_it_needs() {
    let mut d = toml::Value::Table(Default::default());
    set_profile_key(&mut d, "engine.fee_rate", parse_scalar("0.001")).unwrap();
    assert_eq!(d["engine"]["fee_rate"].as_float(), Some(0.001));
}

#[test]
fn a_dotted_key_replaces_an_existing_value_and_leaves_its_siblings_alone() {
    let mut d = doc("[engine]\ncash = 1000.0\nfee_rate = 0.002\n");
    set_profile_key(&mut d, "engine.fee_rate", parse_scalar("0.001")).unwrap();
    assert_eq!(d["engine"]["fee_rate"].as_float(), Some(0.001));
    assert_eq!(d["engine"]["cash"].as_float(), Some(1000.0), "sibling untouched");
}

#[test]
fn a_single_segment_key_sets_a_top_level_scalar() {
    let mut d = doc("[data]\nvenue = \"binance\"\n");
    set_profile_key(&mut d, "name", parse_scalar("my-run")).unwrap();
    assert_eq!(d["name"].as_str(), Some("my-run"));
    assert_eq!(d["data"]["venue"].as_str(), Some("binance"));
}

#[test]
fn a_three_segment_key_reaches_a_nested_table() {
    let mut d = toml::Value::Table(Default::default());
    set_profile_key(&mut d, "engine.impact.model", parse_scalar("sqrt")).unwrap();
    assert_eq!(d["engine"]["impact"]["model"].as_str(), Some("sqrt"));
}

#[test]
fn an_empty_segment_is_refused_naming_the_key() {
    for key in ["engine..fee_rate", ".engine", "engine.", ""] {
        let mut d = toml::Value::Table(Default::default());
        let err = set_profile_key(&mut d, key, parse_scalar("1")).unwrap_err();
        assert!(err.contains("segment"), "says what is wrong: {err}");
    }
}

/// A path THROUGH a plain value cannot be created, and the refusal names the segment that
/// blocks it — the `set_setting` rule, on a profile document.
#[test]
fn a_path_through_a_plain_value_is_refused_naming_the_blocking_segment() {
    let mut d = doc("[engine]\ncash = 1000.0\n");
    let err = set_profile_key(&mut d, "engine.cash.deeper", parse_scalar("1")).unwrap_err();
    assert!(err.contains("engine.cash"), "names the blocking prefix: {err}");
    assert!(err.contains("engine.cash.deeper"), "…and the key asked for: {err}");
}

/// Replacing a whole TABLE with a scalar is refused: the remedy is to set its leaves.
#[test]
fn a_key_naming_a_whole_table_is_refused_naming_the_fix() {
    let mut d = doc("[engine.impact]\nmodel = \"sqrt\"\n");
    let err = set_profile_key(&mut d, "engine.impact", parse_scalar("0.5")).unwrap_err();
    assert!(err.contains("engine.impact"), "names the key: {err}");
    assert!(err.contains("leaves"), "points at the fix: {err}");
}

/// The document root must be a table — a profile that is a bare scalar is not one.
#[test]
fn a_non_table_root_is_refused() {
    let mut d = toml::Value::Integer(1);
    let err = set_profile_key(&mut d, "engine.cash", parse_scalar("1")).unwrap_err();
    assert!(err.contains("root"), "{err}");
}

// ---- --set: the flag ------------------------------------------------------------------------

fn set_args(argv: &[&str]) -> Args {
    let mut full = vec!["--profile".to_string(), "p.toml".to_string()];
    full.extend(argv.iter().map(|s| (*s).to_string()));
    parse_run_args(full.into_iter()).expect("parses")
}

fn explicit(a: &Args) -> Vec<&Override> {
    a.overrides.iter().filter(|o| !matches!(o.origin, Origin::Implied(_))).collect()
}

#[test]
fn set_accepts_both_spellings_and_keeps_argv_order() {
    let a = set_args(&["--set", "engine.cash=1000", "--set=engine.fee_rate=0.001"]);
    let ov = explicit(&a);
    assert_eq!(ov.len(), 2);
    assert_eq!(ov[0].key, "engine.cash");
    assert_eq!(ov[0].value.as_integer(), Some(1000));
    assert_eq!(ov[0].origin, Origin::Set);
    assert_eq!(ov[1].key, "engine.fee_rate");
    assert_eq!(ov[1].value.as_float(), Some(0.001));
}

/// ⚠ `Flags::next_flag` splits on the FIRST `=` only and `Flags::value` consumes the next argv
/// token RAW, so BOTH spellings hand this arm a `key=value` string whose value may contain its
/// own `=` — a Rhai param, a base64 blob. The split here must be `split_once` too.
#[test]
fn a_set_value_may_contain_its_own_equals_sign() {
    let a = set_args(&["--set", "strategy.params.expr=a=b"]);
    let ov = explicit(&a);
    assert_eq!(ov[0].key, "strategy.params.expr");
    assert_eq!(ov[0].value.as_str(), Some("a=b"));
}

#[test]
fn a_set_without_an_equals_sign_is_refused_naming_the_form() {
    let err = parse_run_args(
        ["--profile", "p.toml", "--set", "engine.cash"].map(String::from).into_iter(),
    )
    .unwrap_err();
    assert!(err.contains("--set"), "{err}");
    assert!(err.contains("key=value"), "names the form: {err}");
}

#[test]
fn a_set_naming_an_undeclared_top_level_table_is_refused_before_any_dial() {
    let err = parse_run_args(
        ["--profile", "p.toml", "--set", "egnine.fee_rate=0.1"].map(String::from).into_iter(),
    )
    .unwrap_err();
    assert!(err.contains("egnine"), "names the typo: {err}");
    for k in PROFILE_TOP_LEVEL_KEYS {
        assert!(err.contains(k), "the message lists the valid set, missing {k}: {err}");
    }
}

#[test]
fn every_declared_top_level_table_is_accepted() {
    for k in PROFILE_TOP_LEVEL_KEYS {
        let key = if k == "name" { "name".to_string() } else { format!("{k}.x") };
        let argv = vec![
            "--profile".to_string(),
            "p.toml".to_string(),
            "--set".to_string(),
            format!("{key}=1"),
        ];
        let a = parse_run_args(argv.into_iter())
            .unwrap_or_else(|e| panic!("{k} must be accepted: {e}"));
        assert_eq!(explicit(&a)[0].key, key);
    }
}

// ---- the builder ---------------------------------------------------------------------------

#[test]
fn the_builder_applies_overrides_onto_a_base_file() {
    let base = "[engine]\ncash = 1000.0\nfee_rate = 0.002\n";
    let out = build_profile_toml(
        Some(base),
        &[Override {
            key: "engine.fee_rate".to_string(),
            value: parse_scalar("0.001"),
            origin: Origin::Set,
        }],
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["engine"]["fee_rate"].as_float(), Some(0.001));
    assert_eq!(v["engine"]["cash"].as_float(), Some(1000.0));
}

#[test]
fn the_builder_starts_from_an_empty_document_when_there_is_no_base() {
    let out = build_profile_toml(
        None,
        &[Override {
            key: "engine.cash".to_string(),
            value: parse_scalar("1000"),
            origin: Origin::Set,
        }],
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["engine"]["cash"].as_integer(), Some(1000));
}

/// The passes, in spec §5.3's order: file, then every `Set`, then every `Sugar` — and
/// `Implied` last, applying ONLY where nothing else did.
#[test]
fn sugar_beats_set_and_implied_never_overwrites() {
    let out = build_profile_toml(
        Some("[engine]\nfee_rate = 0.9\n[data]\nkind = \"tick\"\n"),
        &[
            Override {
                key: "engine.fee_rate".to_string(),
                value: parse_scalar("0.5"),
                origin: Origin::Set,
            },
            Override {
                key: "engine.fee_rate".to_string(),
                value: parse_scalar("0.001"),
                origin: Origin::Sugar("--fee"),
            },
            Override {
                key: "data.kind".to_string(),
                value: toml::Value::String("bar".to_string()),
                origin: Origin::Implied("the flags-built default"),
            },
        ],
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["engine"]["fee_rate"].as_float(), Some(0.001), "sugar wins over --set");
    assert_eq!(v["data"]["kind"].as_str(), Some("tick"), "implied never overwrites");
}

#[test]
fn the_builder_refuses_an_unparseable_base_naming_it_as_the_profile() {
    let err = build_profile_toml(Some("this is [not valid"), &[]).unwrap_err();
    assert!(err.contains("profile"), "{err}");
}

/// An applier refusal surfaces THROUGH the builder rather than being swallowed.
#[test]
fn the_builder_propagates_an_applier_refusal() {
    let err = build_profile_toml(
        Some("[engine.impact]\nmodel = \"sqrt\"\n"),
        &[Override {
            key: "engine.impact".to_string(),
            value: parse_scalar("1"),
            origin: Origin::Set,
        }],
    )
    .unwrap_err();
    assert!(err.contains("engine.impact"), "{err}");
}

// ---- the selection sugar --------------------------------------------------------------------

#[test]
fn every_sugar_flag_lands_on_its_key_with_the_right_toml_type() {
    let a = parse_run_args(
        [
            "--venue",
            "binance",
            "--symbol",
            "BTCUSDT,ETHUSDT",
            "--interval",
            "1h",
            "--from",
            "2026-01-01T00",
            "--to",
            "2026-02-01T00",
            "--kind",
            "bar",
            "--strategy",
            "buy_hold",
            "--cash",
            "10000",
            "--fee",
            "0.001",
            "--slippage",
            "0.0005",
        ]
        .map(String::from)
        .into_iter(),
    )
    .expect("parses");
    let out = build_profile_toml(None, &a.overrides).unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["data"]["venue"].as_str(), Some("binance"));
    let syms = v["data"]["symbols"].as_array().expect("an array");
    assert_eq!(syms.len(), 2);
    assert_eq!(syms[1].as_str(), Some("ETHUSDT"));
    assert_eq!(v["data"]["interval"].as_str(), Some("1h"));
    assert_eq!(v["data"]["from"].as_str(), Some("2026-01-01T00"), "a DATE is a string");
    assert_eq!(v["data"]["kind"].as_str(), Some("bar"));
    assert_eq!(v["strategy"]["name"].as_str(), Some("buy_hold"));
    assert_eq!(v["engine"]["cash"].as_integer(), Some(10000), "a NUMBER keeps its type");
    assert_eq!(v["engine"]["fee_rate"].as_float(), Some(0.001));
    assert_eq!(v["engine"]["slippage"].as_float(), Some(0.0005));
}

/// ⚠ `data.from` is a `String` in the schema, so a bare epoch-ms MUST stay a string. The
/// `--set` spelling types it as an integer and is refused far-side — a real divergence, and
/// the reason `--from` exists as its own flag.
#[test]
fn a_bare_epoch_ms_date_stays_a_string_through_the_sugar_flag() {
    let a =
        parse_run_args(["--from", "0", "--to", "100000"].map(String::from).into_iter()).unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    assert_eq!(v["data"]["from"].as_str(), Some("0"));
    assert_eq!(v["data"]["to"].as_str(), Some("100000"));
}

/// §15.2: a sugar flag and its `--set` spelling produce the SAME document, for the numeric
/// rows. (The `Str`-shaped flags deliberately differ on a value that is itself a TOML scalar;
/// see `a_bare_epoch_ms_date_stays_a_string_through_the_sugar_flag`.)
#[test]
fn sugar_and_set_produce_the_same_document() {
    for (sugar, key, value) in [
        (vec!["--fee", "0.001"], "engine.fee_rate", "0.001"),
        (vec!["--slippage", "0.0005"], "engine.slippage", "0.0005"),
        (vec!["--cash", "10000"], "engine.cash", "10000"),
    ] {
        let a = parse_run_args(sugar.iter().map(|s| (*s).to_string())).unwrap();
        let b =
            parse_run_args(["--set".to_string(), format!("{key}={value}")].into_iter()).unwrap();
        let lhs = build_profile_toml(None, &a.overrides).unwrap();
        let rhs = build_profile_toml(None, &b.overrides).unwrap();
        assert_eq!(lhs, rhs, "{sugar:?} must equal --set {key}={value}");
    }
}

#[test]
fn symbol_is_repeatable_and_comma_splitting_and_yields_one_override() {
    let a = parse_run_args(
        ["--symbol", "BTCUSDT,ETHUSDT", "--symbol", "SOLUSDT"].map(String::from).into_iter(),
    )
    .unwrap();
    let rows: Vec<&Override> = a.overrides.iter().filter(|o| o.key == "data.symbols").collect();
    assert_eq!(rows.len(), 1, "one accumulated override, not one per occurrence");
    let syms: Vec<&str> = rows[0]
        .value
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect();
    assert_eq!(syms, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
}

#[test]
fn an_empty_symbol_element_is_refused() {
    for bad in ["BTCUSDT,", ",BTCUSDT", "BTCUSDT,,ETHUSDT", ""] {
        let err = parse_run_args(["--symbol", bad].map(String::from).into_iter()).unwrap_err();
        assert!(err.contains("--symbol"), "{bad:?}: {err}");
    }
}

#[test]
fn a_bad_interval_is_refused_locally_naming_the_units() {
    let err = parse_run_args(["--interval", "1hh"].map(String::from).into_iter()).unwrap_err();
    assert!(err.contains("1hh"), "names the value: {err}");
    assert!(err.contains('s') && err.contains('d'), "names the units: {err}");
}

#[test]
fn a_bad_kind_is_refused_locally_naming_the_roster() {
    let err = parse_run_args(["--kind", "ticks"].map(String::from).into_iter()).unwrap_err();
    assert!(err.contains("bar") && err.contains("tick"), "{err}");
}

#[test]
fn a_strategy_name_is_forwarded_unvalidated() {
    // The roster lives in vike-backtest and includes USER strategies this side cannot see, so
    // a name is never refused here.
    let a = parse_run_args(["--strategy", "not_a_real_strategy"].map(String::from).into_iter())
        .unwrap();
    assert_eq!(explicit(&a)[0].value.as_str(), Some("not_a_real_strategy"));
}

// ---- the implied defaults ------------------------------------------------------------------

#[test]
fn a_flags_built_profile_gets_kind_bar_when_nothing_said_otherwise() {
    let a = parse_run_args(["--venue", "binance"].map(String::from).into_iter()).unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    assert_eq!(v["data"]["kind"].as_str(), Some("bar"));
}

#[test]
fn an_explicit_kind_and_a_files_kind_both_beat_the_implied_default() {
    let a = parse_run_args(["--kind", "tick"].map(String::from).into_iter()).unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    assert_eq!(v["data"]["kind"].as_str(), Some("tick"));

    let b = parse_run_args(["--venue", "polymarket"].map(String::from).into_iter()).unwrap();
    let v: toml::Value = toml::from_str(
        &build_profile_toml(Some("[data]\nkind = \"tick\"\n"), &b.overrides).unwrap(),
    )
    .unwrap();
    assert_eq!(v["data"]["kind"].as_str(), Some("tick"), "the file wins");
}

#[test]
fn script_implies_the_rhai_strategy_name_but_never_overrides_one() {
    let a = parse_run_args(["--script", "s.rhai"].map(String::from).into_iter()).unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    assert_eq!(v["strategy"]["name"].as_str(), Some("rhai"));

    let b = parse_run_args(
        ["--script", "s.rhai", "--strategy", "buy_hold"].map(String::from).into_iter(),
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &b.overrides).unwrap()).unwrap();
    assert_eq!(v["strategy"]["name"].as_str(), Some("buy_hold"), "explicit wins");
}

#[test]
fn no_script_implies_no_strategy_name_and_never_a_cash_default() {
    let a = parse_run_args(["--venue", "binance"].map(String::from).into_iter()).unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    assert!(
        v.get("strategy").and_then(|s| s.get("name")).is_none(),
        "a strategy name with no --script and no --strategy would be a guess"
    );
    assert!(
        v.get("engine").and_then(|e| e.get("cash")).is_none(),
        "engine.cash has no defensible default — the far side names it as a missing field"
    );
}

// ---- --param ---------------------------------------------------------------------------------

#[test]
fn param_sets_one_strategy_knob() {
    let a = parse_run_args(
        ["--param", "size=1.5", "--param", "sym=BTCUSDT"].map(String::from).into_iter(),
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    assert_eq!(v["strategy"]["params"]["size"].as_float(), Some(1.5));
    assert_eq!(v["strategy"]["params"]["sym"].as_str(), Some("BTCUSDT"));
}

#[test]
fn param_and_its_set_spelling_agree() {
    let a = parse_run_args(["--param", "size=1.5"].map(String::from).into_iter()).unwrap();
    let b = parse_run_args(["--set", "strategy.params.size=1.5"].map(String::from).into_iter())
        .unwrap();
    assert_eq!(
        build_profile_toml(None, &a.overrides).unwrap(),
        build_profile_toml(None, &b.overrides).unwrap()
    );
}

/// ⚠ A RANGE declares a search AXIS, which reroutes the whole request to `RunParamscanProfile` and
/// changes the response document. That is §5.4 work; taking `"60:100:10"` as a STRING param
/// would let an operator believe they declared a search that never happened.
///
/// ⚠ The SECTION NAME in the `contains` below is the third of the three re-key sites owner
/// ruling R2 (`[sweep]` → `[paramscan]`) touches in this plan, and it must name whatever the
/// loader accepts — a refusal that told an operator to write a section the loader refuses is
/// worse than no refusal. `RunParamscanProfile` above keeps its name: R2 renames the TOML section
/// and no Rust or wire identifier.
#[test]
fn a_param_range_is_refused_by_name_rather_than_taken_as_a_string() {
    for bad in ["threshold=60:100:10", "regime=[trend,chop]", "x=0.5:2.0:0.5"] {
        let err = parse_run_args(["--param", bad].map(String::from).into_iter()).unwrap_err();
        assert!(err.contains("--param"), "{bad}: {err}");
        assert!(err.contains("[paramscan]"), "points at where a search is declared: {err}");
        assert!(err.contains("--set"), "…and at the escape hatch: {err}");
    }
}

/// A colon that is not a three-part numeric range is an ordinary value.
#[test]
fn a_colon_that_is_not_a_range_is_an_ordinary_param_value() {
    let a =
        parse_run_args(["--param", "sym=binance:BTCUSDT"].map(String::from).into_iter()).unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    assert_eq!(v["strategy"]["params"]["sym"].as_str(), Some("binance:BTCUSDT"));
}

#[test]
fn a_param_without_an_equals_sign_is_refused() {
    let err = parse_run_args(["--param", "size"].map(String::from).into_iter()).unwrap_err();
    assert!(err.contains("k=v"), "{err}");
}

/// ⚠ …and an EMPTY key is refused naming `--param`, not `--set`. It builds the dotted key
/// `strategy.params.`, whose trailing empty segment [`set_profile_key`] refuses in the `--set`
/// GRAMMAR's own words — so before this guard the operator was told about a flag they never
/// typed. The rung was always right; the flag name was not.
#[test]
fn an_empty_param_key_is_refused_naming_param_rather_than_set() {
    for bad in ["=1", " =1"] {
        let err = parse_run_args(["--param", bad].map(String::from).into_iter()).unwrap_err();
        assert!(err.contains("--param"), "{bad:?}: {err}");
        assert!(!err.contains("--set"), "it must not name a flag nobody typed: {err}");
    }
}

/// A `--param` key may not be dotted: `[strategy.params]` is a FLAT knob table, and a dotted
/// key there would create a nested table the strategy never reads.
#[test]
fn a_dotted_param_key_is_refused_pointing_at_set() {
    let err = parse_run_args(["--param", "a.b=1"].map(String::from).into_iter()).unwrap_err();
    assert!(err.contains("--set"), "{err}");
}

// ---- --write-profile / --show-effective ------------------------------------------------------

#[test]
fn render_effective_is_valid_toml_with_the_overrides_as_comments() {
    let ov = vec![
        Override {
            key: "engine.fee_rate".to_string(),
            value: parse_scalar("0.001"),
            origin: Origin::Sugar("--fee"),
        },
        Override {
            key: "engine.cash".to_string(),
            value: parse_scalar("1000"),
            origin: Origin::Set,
        },
        Override {
            key: "data.kind".to_string(),
            value: toml::Value::String("bar".to_string()),
            origin: Origin::Implied("no --kind"),
        },
    ];
    let body = build_profile_toml(None, &ov).unwrap();
    let out = render_effective(&body, &ov);
    // The whole thing still parses: a redirect produces a usable profile.
    let v: toml::Value = toml::from_str(&out).expect("--show-effective output must be TOML");
    assert_eq!(v["engine"]["fee_rate"].as_float(), Some(0.001));
    assert!(out.contains("--fee"), "names the sugar flag that set it: {out}");
    assert!(out.contains("--set"), "…and the --set origin: {out}");
    assert!(out.contains("implied"), "…and the implied one: {out}");
    assert!(
        out.lines().take_while(|l| l.starts_with('#') || l.trim().is_empty()).count() >= 4,
        "the origin block is a comment header: {out}"
    );
}

/// ⚠ A TABLE-valued override must not put a raw NEWLINE inside the `#` comment line.
/// `--set engine.impact={model="sqrt"}` is spellable today — nothing refuses it, because
/// [`set_profile_key`]'s table check fires only when the LEAF is already a table — and a
/// `toml::Value::Table` serializes as a `[v]` SECTION with its keys on following lines. Printed
/// verbatim that breaks the one property this whole flag rests on: that stdout is valid TOML,
/// so `--show-effective > run.toml` writes a usable profile.
#[test]
fn a_table_valued_override_does_not_break_the_comment_header() {
    let ov = vec![Override {
        key: "engine.impact".to_string(),
        value: parse_scalar(r#"{ model = "sqrt", coef = 0.5 }"#),
        origin: Origin::Set,
    }];
    assert!(ov[0].value.is_table(), "the fixture must really be a table value");
    // Built WITHOUT the applier (which refuses a key naming a whole table): the renderer is
    // what is under test, and it must survive any value that can reach it.
    let body = "[engine.impact]\nmodel = \"sqrt\"\ncoef = 0.5\n";
    let out = render_effective(body, &ov);
    let header: Vec<&str> = out.lines().take_while(|l| l.starts_with('#')).collect();
    assert!(header.iter().any(|l| l.contains("engine.impact")), "the row is still rendered: {out}");
    // THE PROPERTY: every header line is a comment, and the whole document still parses.
    let v: toml::Value = toml::from_str(&out).unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert_eq!(v["engine"]["impact"]["model"].as_str(), Some("sqrt"));
}

/// ⚠ An `Origin::Implied` row that `build_profile_toml` SKIPPED must not be reported as though
/// it applied. `--profile tick.toml --show-effective` would otherwise print
/// `data.kind "bar" implied` three lines above a document reading `kind = "tick"` — a
/// provenance answer that is positively wrong, on every `--show-effective` run that also
/// carries a `--profile` naming one of the two implied keys.
#[test]
fn an_implied_row_the_profile_overrode_is_not_reported_as_applied() {
    let ov =
        parse_run_args(["--venue", "binance"].map(String::from).into_iter()).unwrap().overrides;
    let body = build_profile_toml(Some("[data]\nkind = \"tick\"\n"), &ov).unwrap();
    let out = render_effective(&body, &ov);

    let kind_row = out
        .lines()
        .find(|l| l.starts_with('#') && l.contains("data.kind"))
        .unwrap_or_else(|| panic!("the implied row is still rendered: {out}"));
    assert!(
        kind_row.contains("NOT APPLIED"),
        "the row must say it did not apply: {kind_row}\nfull output:\n{out}"
    );
    // …and the document itself is the file's value, which is what makes the old row a lie.
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["data"]["kind"].as_str(), Some("tick"));
}

/// …and the other direction: a row that genuinely DID apply is not annotated away.
#[test]
fn an_implied_row_that_applied_is_reported_plainly() {
    let ov =
        parse_run_args(["--venue", "binance"].map(String::from).into_iter()).unwrap().overrides;
    let body = build_profile_toml(None, &ov).unwrap();
    let out = render_effective(&body, &ov);
    let kind_row = out
        .lines()
        .find(|l| l.starts_with('#') && l.contains("data.kind"))
        .unwrap_or_else(|| panic!("the implied row is rendered: {out}"));
    assert!(kind_row.contains("implied"), "{kind_row}");
    assert!(!kind_row.contains("NOT APPLIED"), "it DID apply: {kind_row}");
}

/// An explicit flag naming the same key beats the implied row, and the row says so.
///
/// ⚠ **Read what this case can and cannot see.** `--kind tick` makes the built document `"tick"`
/// while the implied row carries `"bar"`, so [`implied_row_applied`]'s VALUE comparison already
/// answers — the sibling-override early return above it is never the reason. An earlier
/// write-up advertised this test as gating that clause; it does not, and deleting the clause
/// leaves this test green. `an_implied_row_a_sibling_carrying_the_same_value_is_not_reported_as_applied`
/// is the case that gates it. (This is the same class of claim the round-1 FIX 5 correction was
/// raised for — a test asserted to prove something it cannot — so it is written at the test
/// rather than in a report somebody has to find.)
#[test]
fn an_implied_row_an_explicit_flag_overrode_is_not_reported_as_applied() {
    let ov = parse_run_args(["--kind", "tick"].map(String::from).into_iter()).unwrap().overrides;
    let body = build_profile_toml(None, &ov).unwrap();
    let out = render_effective(&body, &ov);
    let rows: Vec<&str> =
        out.lines().filter(|l| l.starts_with('#') && l.contains("data.kind")).collect();
    assert_eq!(rows.len(), 2, "the sugar row AND the implied row are both listed: {out}");
    assert!(
        rows.iter().any(|l| l.contains("--kind") && !l.contains("NOT APPLIED")),
        "the sugar row applied: {rows:?}"
    );
    assert!(rows.iter().any(|l| l.contains("NOT APPLIED")), "the implied row did not: {rows:?}");
}

/// ⚠ **THE ONLY CASE THAT GATES [`implied_row_applied`]'s SIBLING-OVERRIDE CLAUSE**, and it
/// exists because nothing else did.
///
/// That clause returns early when another override on the same command line names the key. Its
/// two neighbours cannot see it: both make the built document hold a value the implied row does
/// NOT carry (`tick` vs `bar`, or the file's own), so the VALUE comparison on the next line
/// answers first and the clause is dead weight as far as they are concerned. Delete the clause
/// and they stay green.
///
/// It is observable only when a sibling sets the key to the SAME value the implied row would
/// have: the sugar (or `--set`) pass writes `bar`, the implied pass then SKIPS because the key
/// is already set, and a renderer without the clause compares `bar == bar` and reports a row
/// that never applied as applied. Both spellings are covered because they take different passes
/// in `build_profile_toml` and only the clause makes them agree.
#[test]
fn an_implied_row_a_sibling_carrying_the_same_value_is_not_reported_as_applied() {
    for argv in [
        vec!["--venue", "binance", "--kind", "bar"],
        vec!["--venue", "binance", "--set", "data.kind=bar"],
    ] {
        let ov = parse_run_args(argv.iter().map(|s| (*s).to_string())).unwrap().overrides;
        let body = build_profile_toml(None, &ov).unwrap();
        let out = render_effective(&body, &ov);
        // The IMPLIED row specifically — the sibling's own row carries its flag as the origin,
        // so filtering on `implied` isolates the one under test.
        let implied_row = out
            .lines()
            .find(|l| l.starts_with('#') && l.contains("data.kind") && l.contains("implied"))
            .unwrap_or_else(|| panic!("{argv:?}: the implied row is rendered: {out}"));
        assert!(
            implied_row.contains("NOT APPLIED"),
            "{argv:?}: a sibling set data.kind, so the implied row did NOT apply — the value \
                 being identical is exactly what makes this invisible without the sibling \
                 clause: {implied_row}\nfull output:\n{out}"
        );
        // …and the document is still right, so nobody is misled about the RUN either way.
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["data"]["kind"].as_str(), Some("bar"));
    }
}

#[test]
fn write_profile_and_show_effective_parse_and_join_the_refusal_roster() {
    let a = parse_run_args(
        ["--venue", "binance", "--write-profile", "out.toml", "--show-effective"]
            .map(String::from)
            .into_iter(),
    )
    .unwrap();
    assert_eq!(a.write_profile.as_deref(), Some("out.toml"));
    assert!(a.show_effective);
    assert!(PARAMS_REFUSED.contains(&"--write-profile"));
    assert!(PARAMS_REFUSED.contains(&"--show-effective"));
}

// ---- the address ladder ----------------------------------------------------------------------

/// The address ladder, moved here from `study` — ⚠ **not** because that verb disappears into
/// this one. Owner ruling R1 makes it `vike-cli research study`, a sub-verb of a FOURTH plane,
/// so it survives beside `backtest` rather than inside it. The move is forced by something
/// else: both verbs dial the SAME compute daemon on the SAME `config.backtest_addr` setting,
/// and two copies could answer differently about a blank rung — aiming a verb at `7878` (the
/// data server) or `7879` (the daemon that signs orders) instead of `7880`.
///
/// ⚠ `backtest` did NOT read `config.backtest_addr` before this: it applied a compiled-in const
/// inside `parse_run_args`, and the dispatcher handed the setting to `study` alone. Spec §5.6
/// describes the ladder as though it already existed on this verb.
#[test]
fn the_address_ladder_prefers_the_flag_then_the_setting_then_the_compiled_default() {
    assert_eq!(resolve_addr(Some("<host>:1"), Some("<host>:2")), "<host>:1");
    assert_eq!(resolve_addr(None, Some("<host>:2")), "<host>:2");
    assert_eq!(resolve_addr(None, None), vike_config::DEFAULT_BACKTEST_ADDR);
}

/// A BLANK rung is an ABSENT rung — an `Environment=` line or a settings key set to `""` must
/// not dial the empty string.
#[test]
fn a_blank_address_rung_falls_through_and_a_padded_one_is_trimmed() {
    assert_eq!(resolve_addr(Some("   "), Some("<host>:2")), "<host>:2");
    assert_eq!(resolve_addr(Some(""), None), vike_config::DEFAULT_BACKTEST_ADDR);
    assert_eq!(resolve_addr(None, Some(" ")), vike_config::DEFAULT_BACKTEST_ADDR);
    assert_eq!(resolve_addr(Some(" <host>:1 "), None), "<host>:1");
}

/// `--addr` is no longer collapsed in the parser: the setting is not visible there.
#[test]
fn the_parser_leaves_the_address_unresolved() {
    let a = parse_run_args(["--profile", "p.toml"].map(String::from).into_iter()).unwrap();
    assert!(a.addr.is_none(), "the parser cannot see config.backtest_addr");
    let b = parse_run_args(
        ["--profile", "p.toml", "--addr", "1.2.3.4:9"].map(String::from).into_iter(),
    )
    .unwrap();
    assert_eq!(b.addr.as_deref(), Some("1.2.3.4:9"));
}

// ─── the READING sub-verbs (spec §6.1/§6.2/§6.5, §8.3, §8.5) ────────────────────────────────

/// Every reading sub-verb is reachable by the name the usage advertises. Copied from
/// `crate::cmd::data`'s `all_subcommands_are_reachable_by_the_name_they_advertise`, and it earns
/// its place the same way that one did: the hand-written roster it replaced omitted `rm`, a
/// subcommand that had shipped months earlier.
#[test]
fn every_reading_subcommand_is_reachable_by_the_name_it_advertises() {
    for sub in READ_SUBCOMMANDS {
        let argv = minimum_read_line(sub);
        let parsed = parse_read(&argv).unwrap_or_else(|e| panic!("{}: {e}", sub.as_str()));
        assert_eq!(parsed.sub, *sub, "{} parsed as a different subcommand", sub.as_str());
    }
}

/// The MINIMUM command line each reading sub-verb accepts.
///
/// ⚠ The minimum line, not a bare name: three of them REFUSE a command line that would do
/// nothing, so a bare token would prove only that the refusal fires. ONE copy, because two
/// tests drive it — the reachability check above, and
/// [`the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb`] below, which needs a
/// line that already parses before it can ask whether one more flag is accepted on top of it.
fn minimum_read_line(sub: &ReadSub) -> Vec<&'static str> {
    match sub {
        ReadSub::Show | ReadSub::Path => vec![sub.as_str(), "@last"],
        ReadSub::Tag => vec![sub.as_str(), "@last", "--add", "ci"],
        ReadSub::Diff => vec![sub.as_str(), "@last", "1756000000-1-0"],
        ReadSub::Gate => {
            vec![sub.as_str(), "@last", "--against", "@baseline/m", "--fail-if", "sharpe:-5%"]
        }
        other => vec![other.as_str()],
    }
}

/// ⚠ **THE EXPORTED TABLE AND THE REAL PARSERS MUST AGREE ABOUT EVERY (flag, sub-verb) PAIR.**
///
/// [`crate::surface::FLAGS`] is hand-maintained, and until this test existed nothing held it
/// against the code it describes: every other gate over that table checks it for internal
/// consistency (a sample matches an arity, a roster resolves, a conditional default names a
/// real flag), and none of those can see a row that is simply WRONG about the parser.
///
/// MEASURED, and the reason this exists: `--json` shipped with `applies_to: &["run"]` and was
/// wrong about the EIGHT reading sub-verbs that also accept it. This plane has TWO parsers —
/// [`parse_run_args`] for `run`, [`parse_read`] for the reading family — and the row had only
/// ever looked at one. The published reference then told readers that a `gate --json` pipeline
/// named a flag that does not exist, and the documentation repository's own recipe gate, which
/// reads `applies_to`, would have REFUSED the correct line. A table that is wrong is worse than
/// no table at all, because everything downstream trusts it.
///
/// So acceptance is measured by DRIVING the parser rather than by reading it: the minimum line
/// for the sub-verb, plus the flag under test (with its own `sample` when it takes a value),
/// and whether the parse returns `Ok` is the answer. `--help` is excluded because it
/// short-circuits through the Err channel by design on every verb, and a
/// [`crate::surface::Status::Unbuilt`] flag is excluded because it accepts nowhere — where its
/// refusal REACHES is a separate field with its own gate.
///
/// ⚠ This is the shape to copy when the `data` and `trade` planes export their surfaces: a
/// per-plane table is only worth what a test that drives the parser it describes is worth.
#[test]
fn the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb() {
    // A flag the parser accepts only ALONGSIDE another one. The probe must supply the
    // companion, or the refusal it gets back is about the missing companion rather than about
    // the flag under test — which would read as "the table is wrong" when the table is right.
    //
    // ⚠ The row is the `--local` form: `run --local --profile P [--engine PATH]` is what the
    // usage advertises, and it names a local engine binary, which means nothing when the work
    // is shipped to a daemon. (`--store DIR` was a second row until it was refused on both arms
    // on 2026-09-25; a retired row is not probed here.) Declared here rather than smoothed
    // over, because the CONDITIONALITY is itself a fact about the surface — and a row added to
    // silence a failure, rather than because a companion is genuinely required, would hide
    // exactly what this gate is for.
    const COMBINATION_GATED: &[(&str, &str)] = &[("--engine", "--local")];

    let accepts = |sub: &str, flag: &crate::surface::FlagRow| -> bool {
        let mut argv: Vec<&str> = if sub == "run" {
            let mut base = vec!["--profile", "p.toml"];
            if let Some((_, companion)) = COMBINATION_GATED.iter().find(|(f, _)| *f == flag.long) {
                base.push(companion);
            }
            base
        } else {
            let Some(read) = READ_SUBCOMMANDS.iter().find(|r| r.as_str() == sub) else {
                panic!("the surface names the sub-verb {sub:?}, which this plane does not have")
            };
            minimum_read_line(read)
        };
        argv.push(flag.long);
        if flag.value == crate::surface::Value::Required {
            argv.push(flag.sample.unwrap_or_else(|| {
                panic!("{} takes a value and the table gives no sample to drive", flag.long)
            }));
        }
        if sub == "run" {
            parse_run_args(argv.iter().map(|s| (*s).to_string())).is_ok()
        } else {
            parse_read(&argv).is_ok()
        }
    };

    // Every disagreement, collected before anything is asserted: a test that stops at the
    // first one turns a table-wide audit into one round trip per row.
    let mut wrong: Vec<String> = Vec::new();
    for flag in crate::surface::FLAGS {
        if flag.long == "--help" || flag.status != crate::surface::Status::Ships {
            continue;
        }
        for sub in crate::surface::SUB_VERBS {
            let declared = flag.applies_to.contains(sub);
            let real = accepts(sub, &flag);
            if declared != real {
                wrong.push(format!(
                    "{} on `{sub}`: the table says {}, the parser {}",
                    flag.long,
                    if declared { "it applies" } else { "it does NOT apply" },
                    if real { "ACCEPTS it" } else { "REFUSES it" },
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "crate::surface::FLAGS disagrees with the parsers about {} (flag, sub-verb) pair(s). \
             The PARSER is the authority — fix `applies_to`, or fix the parser if the table \
             describes the intended behaviour:\n  {}",
        wrong.len(),
        wrong.join("\n  "),
    );
}

/// `tag`'s three writes parse, and `--add` is REPEATABLE — the only repeatable flag in this
/// loop, because two labels are two labels rather than the second replacing the first.
#[test]
fn tag_parses_its_three_writes_and_add_repeats() {
    let a = parse_read(&[
        "tag",
        "@last",
        "--add",
        "ci",
        "--add",
        "fee-fix",
        "--note",
        "n",
        "--as",
        "baseline/m",
    ])
    .unwrap();
    assert_eq!(a.add, vec!["ci".to_string(), "fee-fix".to_string()]);
    assert_eq!(a.note.as_deref(), Some("n"));
    assert_eq!(a.mark_as.as_deref(), Some("baseline/m"));
}

/// ⚠ `diff` takes TWO runs and neither is optional. A one-operand `diff` that silently used
/// `@last` for the other side would compare against whatever happened to run most recently,
/// which is the silent precedence this grammar refuses everywhere else.
#[test]
fn diff_requires_both_operands_and_names_them() {
    let a = parse_read(&["diff", "1756000000-1-0", "@last"]).unwrap();
    assert_eq!(a.selector.as_deref(), Some("1756000000-1-0"));
    assert_eq!(a.file.as_deref(), Some("@last"), "the second positional is the RIGHT operand");
    let e = parse_read(&["diff", "@last"]).unwrap_err();
    assert!(e.contains("TWO runs") || e.contains("two runs"), "{e}");
    assert!(e.contains("<a> <b>"), "…and shows the shape: {e}");
}

/// `templates` takes an OPTIONAL starter id — absent is the roster — and at most one, because
/// a second positional is a shell-quoting accident worth naming rather than ignoring.
#[test]
fn templates_takes_an_optional_starter_id_and_at_most_one() {
    assert_eq!(parse_read(&["templates"]).unwrap().selector, None, "absent is the roster");
    assert_eq!(
        parse_read(&["templates", "sma-cross"]).unwrap().selector.as_deref(),
        Some("sma-cross")
    );
    let e = parse_read(&["templates", "sma-cross", "extra"]).unwrap_err();
    assert!(e.contains("extra"), "{e}");
}

/// ⚠ `--script` is the SECOND flag TWO sub-verbs own — `params` lists what a script declares
/// and `script-check` compiles it — so it is refused on the rest by a named rule whose sentence
/// mentions BOTH owners. Refusing it on `script-check` with the old `params`-only sentence
/// would have sent an operator to the wrong verb, which is the cost a named refusal exists to
/// avoid.
#[test]
fn script_belongs_to_params_and_script_check_and_to_nothing_else() {
    assert_eq!(
        parse_read(&["script-check", "--script", "s.rhai"]).unwrap().script.as_deref(),
        Some("s.rhai")
    );
    assert_eq!(
        parse_read(&["params", "--script", "s.rhai"]).unwrap().script.as_deref(),
        Some("s.rhai")
    );
    let e = parse_read(&["templates", "--script", "s.rhai"]).unwrap_err();
    assert!(e.contains("--script"), "{e}");
    assert!(e.contains("params"), "the refusal names the first owner: {e}");
    assert!(e.contains("script-check"), "…and the second: {e}");
}

/// ⚠ `--write-strategy` is `templates`' alone, and it is deliberately NOT `--out`: that flag
/// OVERWRITES on `ls`/`show` while this one refuses an existing path, and one flag may not
/// carry two clobber policies. So `--out` stays refused on `templates` even though `templates`
/// is the one reading verb that writes a file.
#[test]
fn write_strategy_belongs_to_templates_and_out_still_does_not() {
    assert_eq!(
        parse_read(&["templates", "x", "--write-strategy", "s.rhai"])
            .unwrap()
            .write_strategy
            .as_deref(),
        Some("s.rhai")
    );
    let e = parse_read(&["ls", "--write-strategy", "s.rhai"]).unwrap_err();
    assert!(e.contains("--write-strategy") && e.contains("templates"), "{e}");
    let e = parse_read(&["templates", "--out", "s.rhai"]).unwrap_err();
    assert!(e.contains("--out"), "{e}");
}

/// The authoring three refuse a `run` flag the ordinary way — "unknown option" — because none
/// of them shares `params`' named roster. Asserted so the audit's expectation is written down
/// somewhere a reader will find it.
#[test]
fn the_authoring_subverbs_refuse_a_run_flag_as_unknown() {
    for sub in ["templates", "script-api", "script-check"] {
        let e = parse_read(&[sub, "--cash", "1000"]).unwrap_err();
        assert!(e.contains("--cash"), "{sub}: {e}");
    }
}

/// ⚠ `--trades` is the FIRST flag TWO sub-verbs own — `show` renders the ledger and `diff`
/// compares two of them — so it is refused on every other reading verb rather than living in
/// either roster. (`--script` is the second, and has its own test above.)
#[test]
fn trades_belongs_to_show_and_diff_and_to_nothing_else() {
    assert!(parse_read(&["show", "@last", "--trades"]).unwrap().trades);
    assert!(parse_read(&["diff", "@last", "1756000000-1-0", "--trades"]).unwrap().trades);
    let e = parse_read(&["ls", "--trades"]).unwrap_err();
    assert!(e.contains("--trades"), "{e}");
    assert!(e.contains("show") && e.contains("diff"), "it names both owners: {e}");
}

/// A flag belonging to one of the three JUDGING verbs is refused by name on the others, with
/// the reason — never dropped, and never "unknown option".
#[test]
fn a_judging_flag_on_the_wrong_subverb_is_refused_by_name() {
    let e = parse_read(&["ls", "--as", "baseline/m"]).unwrap_err();
    assert!(e.contains("--as") && e.contains("tag"), "{e}");
    let e = parse_read(&["show", "@last", "--fail-if", "sharpe:-5%"]).unwrap_err();
    assert!(e.contains("--fail-if") && e.contains("gate"), "{e}");
    let e = parse_read(&["show", "@last", "--md"]).unwrap_err();
    assert!(e.contains("--md") && e.contains("diff"), "{e}");
}

/// The roster the usage advertises is DERIVED from the enum, never re-typed —
/// `crate::cmd::data`'s `SUBCOMMANDS` doc carries what the hand copy cost.
///
/// ⚠ It asserts the usage LINE (`backtest ls `), not the bare verb. `"ls"` is a substring of
/// `--cols` and `"params"` of `--list-params`, both of which [`USAGE`] carries for other
/// reasons — so a `contains("ls")` stays green with the whole `backtest ls …` line deleted, and
/// pins nothing while reading as coverage.
#[test]
fn the_usage_names_every_reading_subcommand() {
    for sub in READ_SUBCOMMANDS {
        let line = format!("backtest {} ", sub.as_str());
        assert!(USAGE.contains(&line), "USAGE has no `{line}…` line");
    }
}

/// `path` takes a selector and at most one FILE. A second positional is a shell-quoting accident
/// worth naming, not something to ignore.
#[test]
fn path_takes_a_selector_and_at_most_one_file() {
    assert_eq!(parse_read(&["path", "@last"]).unwrap().selector.as_deref(), Some("@last"));
    assert_eq!(parse_read(&["path", "@last", "report"]).unwrap().file.as_deref(), Some("report"));
    let e = parse_read(&["path", "@last", "report", "extra"]).unwrap_err();
    assert!(e.contains("extra"), "{e}");
}

/// A missing selector is a USAGE error naming the grammar, never a silent `@last`.
#[test]
fn path_without_a_selector_is_a_usage_error() {
    let e = parse_read(&["path"]).unwrap_err();
    assert!(e.contains("path"), "{e}");
}

/// A flag that belongs to a SIBLING sub-verb is refused BY NAME with the reason — never dropped,
/// and never reported as "unknown option", which would say the flag does not exist.
#[test]
fn a_sibling_subverbs_flag_is_refused_by_name() {
    let e = parse_read(&["show", "@last", "--sort", "sharpe"]).unwrap_err();
    assert!(e.contains("--sort"), "{e}");
    assert!(e.contains("listing"), "{e}");

    let e = parse_read(&["ls", "--metrics"]).unwrap_err();
    assert!(e.contains("--metrics"), "{e}");

    let e = parse_read(&["path", "@last", "--out", "x.json"]).unwrap_err();
    assert!(e.contains("--out"), "{e}");
}

/// ⚠ The §6.2 renderers this stage cannot build are refused with what they would NEED, not with
/// "unknown option". `--trades` is deliberately absent from that set: the run record grew
/// `trades.json`, so the flag ships.
#[test]
fn an_unbuilt_renderer_is_refused_with_what_it_would_need() {
    for flag in crate::cmd::runs::show::UNBUILT_RENDERERS {
        let e = parse_read(&["show", "@last", flag]).unwrap_err();
        assert!(e.contains(flag), "{flag}: {e}");
        assert!(!e.contains("unknown option"), "{flag}: {e}");
    }
    assert!(parse_read(&["show", "@last", "--trades"]).unwrap().trades);
}

/// The reading roster claims READING tokens and nothing else — which is what lets
/// [`claim_subcommand`] hand `run` to [`parse_run_args`] and a flag to the refusal that names
/// it as a flag. ⚠ It was a PEEK ahead of a flag-form fall-through when this was written;
/// stage 3 deleted that fall-through, so `--profile` here is no longer "not a sub-verb, carry
/// on" but "not a sub-verb, and a sub-verb is required".
#[test]
fn a_line_that_is_not_a_reading_subverb_is_not_claimed_by_the_read_parser() {
    assert!(ReadSub::from_token("--profile").is_none());
    assert!(ReadSub::from_token("--local").is_none());
    assert!(ReadSub::from_token("run").is_none(), "`run` is the computing sub-verb, not a read");
    assert_eq!(ReadSub::from_token("ls"), Some(ReadSub::Ls));
}

/// The reading sub-verbs are named in the verb's own summary, because that summary is what
/// `scripts/gen_skills.sh` renders into every SKILL.md verb table and what an agent reads before
/// it decides which command to name. A sub-verb that ships and is never advertised is a surface
/// only the source tree knows about.
#[test]
fn the_dispatcher_summary_names_the_reading_subverbs() {
    let summary = crate::COMMANDS
        .iter()
        .find(|(name, _)| *name == "backtest")
        .map(|(_, s)| *s)
        .expect("backtest is a registered command");
    // ⚠ Each sub-verb is asserted in the SPELLING this summary uses — the `ls|show|path`
    // shorthand, and the backticked `params`/`strategies` — rather than as a bare word. A bare
    // `contains("ls")` is the shape that goes green on an unrelated substring (`--cols`,
    // `--list-params`) and stops pinning anything the day the summary is reworded.
    for spelling in ["backtest ls|show|path", "`params`", "`strategies`"] {
        assert!(summary.contains(spelling), "the `backtest` summary omits {spelling}");
    }
    // ...and the ROSTER half, so a SIXTH sub-verb cannot ship unadvertised. The two together
    // are what the spelling list alone could not do: one pins the words, the other pins the set.
    for sub in READ_SUBCOMMANDS {
        assert!(summary.contains(sub.as_str()), "the `backtest` summary omits `{}`", sub.as_str());
    }
    // ⚠ `scripts/gen_skills.sh` renders this into a markdown TABLE CELL, so a literal `|` would
    // break the row. The one spelling allowed is the `ls|show|path` shorthand this summary uses.
    assert!(
        !summary.contains('|') || summary.contains("ls|show|path"),
        "a bare pipe breaks the rendered table: {summary}"
    );
}

/// ⚠ The MCP instructions name `vike-cli backtest run --local --profile run.toml`, and
/// `crate::cmd::mcp`'s `the_instructions_name_only_real_commands` holds every backticked word to
/// this USAGE as a SUBSTRING. Growing USAGE must not drop either flag.
///
/// ⚠ The `run` in that line is a SUB-VERB now, not decoration — decision 11 — and
/// `crate::cmd::mcp`'s `initialize_carries_instructions_that_name_the_surface_beyond_this_one`
/// carries the needle with the sub-verb in it, because a needle stopping at `vike-cli backtest`
/// would have kept passing over an invocation that is now an exit-2.
#[test]
fn the_usage_keeps_the_flags_the_mcp_instructions_name() {
    for word in ["--local", "--profile", "--script", "--json", "--addr"] {
        assert!(USAGE.contains(word), "USAGE dropped `{word}`, which the MCP instructions name");
    }
}

// ---- the metric catalog LISTING ------------------------------------------------------------

/// **`show --metrics-list` needs NO selector, and that is the property the flag exists for.**
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the `ReadSub::Show if a.metrics_list`
/// arm from [`parse_read`]. The combined `Show | Tag | Gate` arm below it then calls
/// `required_selector`, the first assertion reddens, and the listing becomes reachable only by
/// naming a run it does not read — on a box with no runs directory at all,
/// `crate::cmd::runs::show::run_show` would refuse before it ever printed the catalog.
#[test]
fn the_metric_listing_takes_no_selector_and_keeps_one_if_given() {
    let a = parse_read(&["show", "--metrics-list"]).expect("a listing needs no run");
    assert!(a.metrics_list);
    assert_eq!(a.selector, None, "no selector was given and none is required");

    // A selector given anyway is KEPT and unused — the listing is the answer either way, and
    // refusing it would be a refusal about a token rather than about a mistake.
    let a = parse_read(&["show", "@last", "--metrics-list"]).expect("a selector is allowed");
    assert!(a.metrics_list);
    assert_eq!(a.selector.as_deref(), Some("@last"));

    // …and the ordinary `show` still REQUIRES one, so the exemption is scoped to this flag.
    let e = parse_read(&["show"]).expect_err("a bare `show` names no run");
    assert!(e.contains("run selector"), "{e}");
}

/// ⚠ **The listing is refused beside every flag that renders the RUN, BY NAME, and beside
/// `--json` with its own sentence** — because [`crate::cmd::runs::show::run_show`] returns the
/// catalog from the top, so a combination would silently make the other flag do nothing.
///
/// It iterates the PRODUCTION array
/// (`crate::cmd::runs::show::run_rendering_flags_given`) rather than a literal list of its own,
/// and then asserts the two agree in LENGTH — so a fifth section flag added there without a
/// sample here stops this test with the reason instead of going unrefused.
///
/// ⚠ The mutation this fails on, in PRODUCTION: delete the
/// `crate::cmd::runs::show::refuse_a_listing_beside_a_run_rendering` call from [`parse_read`].
/// Every line below then parses `Ok` and `show <run> --metrics-list --trades` would print the
/// catalog while the operator waited for a ledger.
#[test]
fn the_listing_refuses_every_run_rendering_flag_by_name() {
    let empty = ReadArgs::empty(ReadSub::Show);
    let rendering = crate::cmd::runs::show::run_rendering_flags_given(&empty);
    let samples: Vec<(&str, Vec<&str>)> = vec![
        ("--metrics", vec!["--metrics"]),
        ("--trades", vec!["--trades"]),
        ("--config", vec!["--config"]),
        ("--export", vec!["--export", "trades"]),
        ("--html", vec!["--html"]),
    ];
    assert_eq!(
        samples.len(),
        rendering.len(),
        "a run-rendering flag was added to `run_rendering_flags_given` without a sample here — \
             add its row, do not delete the check"
    );
    for (flag, _) in rendering {
        let (_, argv) = samples
            .iter()
            .find(|(name, _)| *name == flag)
            .unwrap_or_else(|| panic!("{flag} has no sample argv in this test"));
        let mut line = vec!["show", "@last", "--metrics-list"];
        line.extend(argv.iter().copied());
        let e = parse_read(&line).unwrap_err();
        assert!(e.contains("--metrics-list"), "{flag}: the refusal names the listing: {e}");
        assert!(e.contains(flag), "{flag}: …and the flag it collided with: {e}");
        assert!(
            e.contains("two different documents"),
            "{flag}: …and says WHY neither wins rather than picking one: {e}"
        );
    }

    // `--json` is its own case with its own sentence: the listing has no JSON rendering at all,
    // so the message names what DOES answer the machine-readable question — the convention
    // `crate::cmd::runs::show::refuse_an_unbuilt_renderer`'s `--drawdowns` arm follows.
    let e = parse_read(&["show", "--metrics-list", "--json"]).unwrap_err();
    assert!(e.contains("--metrics-list") && e.contains("--json"), "{e}");
    assert!(e.contains("cli.json"), "…and names the asset that does carry the ids: {e}");

    // ⚠ `--out` COMPOSES — it names a file, not a document — so this must NOT be refused.
    assert!(parse_read(&["show", "--metrics-list", "--out", "catalog.txt"]).is_ok());

    // …and the listing is refused on every SIBLING reading verb by name, through `show_only`.
    // Driven off [`READ_SUBCOMMANDS`] and [`minimum_read_line`], so an ELEVENTH reading verb is
    // covered by existing — and each line already parses before the flag is added, or the
    // refusal that came back would be about the missing operand instead.
    for sub in READ_SUBCOMMANDS.iter().filter(|s| **s != ReadSub::Show) {
        let mut line = minimum_read_line(sub);
        line.push("--metrics-list");
        let e = parse_read(&line).unwrap_err();
        assert!(e.contains("--metrics-list"), "on `{}`: {e}", sub.as_str());
    }
}

// ---- `--decide`, the cross-section mode ----------------------------------------------------

/// **`--decide` lands on `engine.decide` as a STRING and is forwarded unvalidated.**
///
/// ⚠ The unvalidated half is the deliberate one, and it is why this test asserts a value the
/// engine would REFUSE is accepted here: `sequential | simultaneous` has no `NAMES`-shaped home
/// on the far side (`vike_backtest::harness::profile`'s `decide_mode` matches the two strings
/// and types them again into its own refusal), so a local roster would be a THIRD copy. The
/// engine owns the sentence, exactly as it owns `--trials`' range and `--max-gap`'s grammar.
///
/// ⚠ The mutation this fails on, in PRODUCTION: change the `SUGAR` row's `shape` to
/// `Shape::Scalar`. `parse_scalar` then types the value, `engine.decide` lands as something
/// other than a TOML string for any value that parses as a number or a bool, and the first
/// `as_str` assertion reddens — which is the far-side type error
/// `crates/vike-cli/tests/backtest_flags_schema.rs` exists to keep out of a round trip.
#[test]
fn decide_lands_on_its_engine_key_as_a_string_and_is_not_spelling_checked() {
    let a = set_args(&["--decide", "simultaneous"]);
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap())
        .expect("the built profile parses");
    assert_eq!(v["engine"]["decide"].as_str(), Some("simultaneous"));

    // The `--set` spelling reaches the same key through the same applier.
    let b = set_args(&["--set", "engine.decide=simultaneous"]);
    let w: toml::Value = toml::from_str(&build_profile_toml(None, &b.overrides).unwrap())
        .expect("the built profile parses");
    assert_eq!(w["engine"]["decide"], v["engine"]["decide"], "sugar is not a second path");

    // ⚠ FORWARDED UNVALIDATED: a spelling the engine refuses is accepted HERE, on purpose.
    let c = parse_run_args(["--decide", "bogus"].map(String::from).into_iter())
        .expect("this side owns no roster for engine.decide");
    assert!(
        c.overrides.iter().any(|o| o.key == "engine.decide"
            && o.value.as_str() == Some("bogus")
            && o.origin == Origin::Sugar("--decide")),
        "the typo is forwarded so the ENGINE's own sentence is the one an operator reads"
    );

    // …and it is a RUN flag, refused on the discovery verb by name rather than dropped.
    let e = parse_read(&["params", "--decide", "sequential"]).unwrap_err();
    assert!(e.contains("--decide"), "{e}");
    assert!(e.contains("backtest run"), "…and names where it belongs: {e}");
}
