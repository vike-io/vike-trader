//! Ruling 7's route onto the three wire verbs, and the sub-verb spine.

use super::*;

// ---- the route: ruling 7's two axes, onto the three wire verbs -----------------------------

const BASE: &str = "name = \"t\"\n[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\n\
                        kind = \"bar\"\ninterval = \"1d\"\nfrom = \"0\"\nto = \"1\"\n\
                        [engine]\ncash = 1000.0\n[strategy]\nfile = \"trend/buy_hold.rs\"\n";

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
    assert_eq!(route_of(&format!("{BASE}[paramscan]\nsize = [1.0, 2.0]\n"), false), Route::Search);
    assert_eq!(
        route_of(&format!("{BASE}[paramscan]\n"), false),
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
            &format!("{BASE}[paramscan]\nsize = [1.0, 2.0]\n[walkforward]\nn_splits = 2\n"),
            false
        ),
        Route::Walkforward,
        "the two axes COMPOSE (ruling 5): the pair is not refused and neither section is \
             ignored"
    );

    // ⚠ An EMPTY [walkforward] still routes to the walk-forward verb, unlike an empty grid:
    // `ProfileWalkforward::n_splits` carries no `#[serde(default)]`, so the far side's parser
    // answers with the error naming the missing key — a better message than a guess here.
    assert_eq!(route_of(&format!("{BASE}[walkforward]\n"), false), Route::Walkforward);
    // The wrong SHAPE, and unparseable text: both go to the plain backtest, whose
    // `ProfileBacktest::from_toml_str` produces the type error naming the field.
    assert_eq!(route_of(&with_top_level_scalar("walkforward"), false), Route::Single);
    assert_eq!(route_of("this is [not valid", false), Route::Single);
}

/// The two programs with an answer of their own: `[permutation]` (over `RunBacktest`) and the
/// combinatorial form of `[walkforward]` (over `RunWalkforwardProfile`). Each wins over the grid,
/// which permutation training and a searched split read for themselves; a half-declared cut still
/// routes to its answer, whose far-side load names the missing key.
#[test]
fn a_permutation_table_and_a_combinatorial_walk_pick_their_own_answers() {
    let grid = "[paramscan]\nsize = [1.0, 2.0]\n";
    assert_eq!(
        route_of(&format!("{BASE}[permutation]\npermutations = 10\n"), false),
        Route::Permutation
    );
    assert_eq!(
        route_of(&format!("{BASE}{grid}[permutation]\npermutations = 10\ntrain = true\n"), true),
        Route::Permutation
    );
    assert_eq!(
        route_of(&format!("{BASE}[walkforward]\ngroups = 4\ntest_groups = 2\n"), false),
        Route::CombinatorialWalkforward
    );
    assert_eq!(
        route_of(&format!("{BASE}{grid}[walkforward]\ngroups = 4\n"), false),
        Route::CombinatorialWalkforward
    );
    assert_eq!(route_of(&with_top_level_scalar("permutation"), false), Route::Single);
}

/// ⚠ The MIGRATED section name routes too, and that is this stage's whole share of ruling 2.
/// The rename gives `[sweep]` a named migration message rather than deleting it, so BOTH names
/// must still load — and a pre-parse that knew only `[paramscan]` would send a legacy profile
/// to `RunBacktest`, which refuses a grid profile rather than searching it. That is the
/// divergence [`declares_a_paramscan_grid`] exists to close, re-opened from the other side.
///
/// ⚠ `[sweep]` is still the spelling the ENGINE accepts (`ProfileBacktest` is
/// `deny_unknown_fields` and declares only that field), which is why every operator-facing
/// string in this crate still says `[sweep]` while this pre-parse answers to both.
#[test]
fn both_grid_section_names_route_to_the_search() {
    assert_eq!(route_of(&format!("{BASE}[paramscan]\nsize = [1.0, 2.0]\n"), false), Route::Search);
    assert_eq!(route_of(&format!("{BASE}[paramscan]\nsize = [1.0, 2.0]\n"), false), Route::Search);
    assert_eq!(
        route_of(&format!("{BASE}[paramscan]\n"), false),
        Route::Single,
        "an EMPTY migrated grid is no search either"
    );
    assert_eq!(
        declares_a_paramscan_grid(&format!("{BASE}[paramscan]\nsize = [1.0]\n")),
        Some(true)
    );
    assert_eq!(
        declares_a_paramscan_grid(&format!("{BASE}[paramscan]\nsize = [1.0]\n")),
        Some(true)
    );
    // Wrong SHAPE under EITHER name: this side declines to answer (`None`) and the far side's
    // parser produces the type error naming the field.
    assert_eq!(declares_a_paramscan_grid(&with_top_level_scalar("paramscan")), None);
    assert_eq!(declares_a_paramscan_grid(&with_top_level_scalar("sweep")), None);
}

/// ⚠ **PIN: the CLI's pre-parse and the ENGINE's `is_paramscan` answer identically**, for both
/// section spellings, the "present AND non-empty" rule and every shape the pre-parse declines.
///
/// This is the proof that lets the pre-parse stop being a hand-written MIRROR of the engine's
/// predicate and call the shared one (`vike_model::run_request::paramscan`): the answers of the
/// two sides are held equal here, over the real `ProfileBacktest` deserializer, BEFORE and after
/// that move. The contract the callers rely on is the `Option<bool>`: `Some(b)` is "this side can
/// tell", `None` is "the far side's parser must answer" — a wrong-shaped section (a scalar or an
/// array under either name) and text that is not TOML at all.
#[test]
fn the_route_pre_parse_agrees_with_the_engines_is_paramscan() {
    use vike_backtest::run::profile::ProfileBacktest;

    // (text, what the CLI pre-parse answers, whether the ENGINE loads it and what `is_paramscan` says)
    let cases: Vec<(String, Option<bool>, Option<bool>)> = vec![
        (BASE.to_string(), Some(false), Some(false)),
        (format!("{BASE}[paramscan]\nsize = [1.0, 2.0]\n"), Some(true), Some(true)),
        (format!("{BASE}[paramscan]\nsize = [1.0, 2.0]\n"), Some(true), Some(true)),
        (format!("{BASE}[paramscan]\n"), Some(false), Some(false)),
        (format!("{BASE}[paramscan]\n"), Some(false), Some(false)),
        // The wrong SHAPE under either name: the engine REFUSES to load it (a type error), and the
        // pre-parse declines to answer so that refusal is what the operator meets.
        (with_top_level_scalar("paramscan"), None, None),
        (with_top_level_scalar("sweep"), None, None),
        (format!("paramscan = [1, 2]\n{BASE}"), None, None),
        // Text that is not TOML: neither side can read it.
        ("this is [not valid".to_string(), None, None),
    ];
    for (text, cli, engine) in cases {
        assert_eq!(declares_a_paramscan_grid(&text), cli, "CLI pre-parse of {text:?}");
        let loaded = toml::from_str::<ProfileBacktest>(&text).ok().map(|p| p.is_paramscan());
        assert_eq!(loaded, engine, "engine is_paramscan of {text:?}");
        assert_eq!(cli, engine, "the two sides disagree about {text:?}");
    }
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
/// MEASURED before the fix: `crates/vike-backtest/src/search/paramscan/rank.rs`'s
/// `RankMetric::from_str_ci` lowercases and `crates/vike-backtest/src/search/select.rs`'s
/// `resolve` compares with `eq_ignore_ascii_case`, both pinned by their own tests, while this
/// parser asked `roster.contains(&value)` against a local array. So an operator who learned
/// `--rank-by` from `backtest --help` and then reached for `vike-cli` got a usage error for a
/// spelling the engine documents, accepts and tests. Both selectors now go through
/// `vike_model::run_request::flag_vocab`'s `accept_value`, which owns the roster and the rule.
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

/// The ranked table has no column for what the server found in the search's data, so it gets lines
/// of its own: the first row that carries any, once, and a failed point's `null` report is skipped.
#[test]
fn a_searchs_data_warnings_are_read_once_and_a_clean_search_has_none() {
    use super::super::render::search_data_warnings;

    let warned = serde_json::json!({"rows": [
        {"report": null, "error": "a point failed"},
        {"report": {"data_warnings": ["ETHUSDT: did not run"]}},
        {"report": {"data_warnings": ["ETHUSDT: did not run"]}},
    ]});
    assert_eq!(search_data_warnings(&warned), ["ETHUSDT: did not run"]);

    let clean = serde_json::json!({"rows": [{"report": {"final_equity": 1.0}}]});
    assert!(search_data_warnings(&clean).is_empty());
    assert!(search_data_warnings(&serde_json::json!({})).is_empty());
}

/// A search's strategy failures: a summarized search's own block as it is, else every row's report's
/// merged (a failed point's `null` report skipped); a clean search has none.
#[test]
fn a_searchs_strategy_failures_are_its_summarys_block_or_every_rows_together() {
    use super::super::render::search_strategy_failures;

    let rows = serde_json::json!({"rows": [
        {"report": null, "error": "a point failed"},
        {"report": {"strategy_failures": {"count": 2, "first_at": 9, "messages": ["a"], "disabled": false}}},
        {"report": {"strategy_failures": {"count": 3, "first_at": 4, "messages": ["b"], "disabled": true}}},
    ]});
    let merged = vike_model::StrategyFailures {
        count: 5,
        first_at: Some(4),
        messages: vec!["a".to_string(), "b".to_string()],
        disabled: true,
    };
    assert_eq!(search_strategy_failures(&rows), merged);

    let summary =
        serde_json::json!({"strategy_failures": {"count": 1, "disabled": false}, "rows": []});
    assert_eq!(search_strategy_failures(&summary).count, 1);
    let clean = serde_json::json!({"rows": [{"report": {"final_equity": 1.0}}]});
    assert!(search_strategy_failures(&clean).is_empty());
}

/// A permutation answer renders what was tested, the real winner when one was searched, and the
/// three numbers the test is read by; the per-history scores stay the `--json` reader's.
#[test]
fn a_permutation_answer_renders_its_protocol_and_its_p_value() {
    let trained = serde_json::json!({
        "trained": true, "seed": 7, "real_score": 1.25, "at_or_above": 3,
        "permuted_scores": [0.1, 2.0, 1.3, 1.25], "p_value": 0.8,
        "real_params": [["size", 2.0]], "data_warnings": ["BTCUSDT: one hole"],
    });
    assert_eq!(
        permutation_lines(&trained),
        [
            "permutation training: the real history against 4 permuted one(s), seed 7",
            "data warning: BTCUSDT: one hole",
            "real winner: size=2.0",
            "real_score = 1.2500   at_or_above = 3 of 4   p_value = 0.8000",
        ]
    );
    let plain = serde_json::json!({
        "trained": false, "seed": 0, "real_score": 0.5, "at_or_above": 0,
        "permuted_scores": [0.1], "p_value": 0.5, "real_params": null, "data_warnings": [],
    });
    let lines = permutation_lines(&plain);
    assert_eq!(lines[0], "permutation test: the real history against 1 permuted one(s), seed 0");
    assert_eq!(lines.len(), 2, "no winner line when nothing was searched: {lines:?}");
}

/// ⚠ **Every metric [`print_paramscan`] prints must have a catalog row.** [`cell`]'s `None`
/// arm renders through `Ratio` — no scaling, no suffix — so an id that fell out of
/// `vike_analytics::metric_catalog`'s `METRICS` would quietly go back to publishing `0.0310`
/// for a three-percent drawdown under a `max_drawdown` header, which is the defect that function's
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
    .expect("a TradingResults's compact scalars");
    assert_eq!(cell(&r, "max_drawdown"), "3.1000%", "a drawdown is a PERCENT, never 0.0310");
    assert_eq!(cell(&r, "total_return"), "5.0000%");
    assert_eq!(cell(&r, "sharpe"), "1.2500", "a Ratio takes four decimals and no suffix");
    assert_eq!(cell(&r, "final_equity"), "10500.00", "Money takes two");
    assert_eq!(
        cell(&r, "n_trades"),
        "412",
        "a Count carries none — 412.0000 trades is not a thing"
    );
    // A `null` or absent metric still RENDERS rather than breaking the table — [`number`]'s rule,
    // unchanged by the migration, and the reason a degenerate search still prints its rows.
    let degenerate: Value = serde_json::from_str(r#"{"sharpe": null}"#).unwrap();
    assert_eq!(cell(&degenerate, "sharpe"), "0.0000");
    assert_eq!(cell(&degenerate, "max_drawdown"), "0.0000%");
}

/// ⚠ **The ranked table's money column names the sweep's ledger currency when it is not the dollar**,
/// because `final_equity` below it is an amount of that currency and a yen table headed
/// `final_equity` reads as dollars. The server sends `account_currency` only off the dollar, so a dollar
/// document, and every document older than the label, keeps the bare header.
#[test]
fn the_ranked_tables_money_header_names_a_non_dollar_ledger() {
    let dollar: Value = serde_json::from_str(r#"{"rows": []}"#).unwrap();
    assert_eq!(ledgered_head("final_equity", &dollar), "final_equity");
    let yen: Value = serde_json::from_str(r#"{"rows": [], "account_currency": "JPY"}"#).unwrap();
    assert_eq!(ledgered_head("final_equity", &yen), "final_equity JPY");
    let junk: Value = serde_json::from_str(r#"{"rows": [], "account_currency": 7}"#).unwrap();
    assert_eq!(ledgered_head("final_equity", &junk), "final_equity", "a non-string names nothing");
}

// ---- the sub-verb spine (decision 11) ------------------------------------------------------

fn run_args_of(args: &[&str]) -> Result<Args, crate::cmd::args::Stop> {
    parse_run_args(args.iter().map(|s| (*s).to_string()))
}

/// The router alone — what [`run`] does before it hands the tail to a sub-verb's own rules.
/// Returns the [`Sub`] `clap` claimed, so a test can assert the NAME without also supplying every
/// flag that sub-verb requires.
fn parse_sub(args: &[&str]) -> Result<Sub, crate::cmd::args::Stop> {
    parse(args.iter().map(|s| (*s).to_string()))
}

/// The plane's visible sub-verbs in the order `--help` lists them — DERIVED from the `clap` command,
/// so a refusal or a test that names the roster can never name the set short.
fn plane_subcommands() -> Vec<String> {
    command()
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .map(|s| s.get_name().to_string())
        .collect()
}

/// The word a parsed [`Sub`] stands for.
fn word_of(sub: Sub) -> String {
    match sub.into_read() {
        None => "run".to_string(),
        Some((read, _)) => read.as_str().to_string(),
    }
}

/// Every sub-verb is reachable by the name it advertises, in BOTH directions: each name in the
/// command parses to the sub-verb of that name, and nothing is parsed that the command does not
/// list. `crate::cmd::data`'s `all_subcommands_are_reachable_by_the_name_they_advertise` is the
/// shape.
///
/// ⚠ The ROUTER is all this drives, so the bare NAME is enough for the reading verbs: each verb's
/// own required flags are asserted by `every_reading_subcommand_is_reachable_by_the_name_it_advertises`,
/// which drives [`parse_read`] with the minimum line each one accepts.
#[test]
fn every_subcommand_is_reachable_by_the_name_it_advertises() {
    let names = plane_subcommands();
    assert!(names.len() > 1, "the command lists its sub-verbs: {names:?}");
    for name in &names {
        // Parsed with the flags each one REQUIRES, so a refusal can only be about the NAME.
        let argv: Vec<&str> =
            if name == "run" { vec!["run", "--profile", "p.toml"] } else { vec![name.as_str()] };
        let parsed = parse_sub(&argv).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(word_of(parsed), *name, "{name} parsed as a different subcommand");
    }
}

/// ⚠ A missing sub-verb names EVERY sub-verb that exists, DERIVED from the command. The
/// hand-written copy this shape replaced in `crate::cmd::data` omitted a subcommand that had
/// shipped months earlier — on the verb that DELETES.
#[test]
fn a_missing_subcommand_names_every_subcommand_that_exists() {
    let err = parse_sub(&[]).unwrap_err();
    assert!(!err.is_help(), "a bare `backtest` is a usage error: {err}");
    for name in plane_subcommands() {
        assert!(err.contains(&name), "must name {name}: {err}");
    }
}

/// ⚠ Decision 11: there is no bare form. A flag where a sub-verb belongs is refused, naming the
/// flag — it must not read as a SUBCOMMAND called `--profile`.
#[test]
fn a_flag_where_a_subcommand_belongs_is_refused() {
    let err = parse_sub(&["--profile", "p.toml"]).unwrap_err();
    assert!(!err.is_help());
    assert!(err.contains("--profile"), "names what was typed: {err}");
}

/// ⚠ The one spelling that had shipped for months gets a sentence rather than a shrug:
/// `--list-params` is now `backtest params`.
///
/// ⚠ Two DIFFERENT code paths answer it and both are asserted here. The bare
/// `vike-cli backtest --list-params` is the PLANE's answer ([`parse`]); a
/// `vike-cli backtest run --list-params` gets past it — `run` is a valid sub-verb — and is
/// answered by [`run_args::RunCommand`]'s own rule. A single-path test would have left whichever half
/// it did not drive answering with `clap`'s unknown argument.
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
        assert!(err.contains("vike-backend datahub --store DIR"), "{argv:?}: {err}");
        assert!(
            row.refusals.iter().any(|r| err.contains(&r.message())),
            "the published row carries the parser's sentence verbatim: {err}"
        );
    }
}

/// Every sub-verb is NAMED in the help. `crate::cmd::data`'s `Sub` doc states the contract as three
/// places — an arm in the router, an arm in [`run`], a row in the help — and this is the third one,
/// machine-checked; with `clap` the help's roster is the router's own, so it can only fail if a
/// sub-verb is hidden.
///
/// ⚠ It exists because the gate that looks closest to it is weaker than it appears:
/// `crate::surface`'s `the_mcp_instructions_name_only_real_commands` asserts the help contains each
/// word after `vike-cli backtest`, and `run` is already a substring of `<run.toml>` — so that gate
/// would pass a help that never advertises the sub-verb at all.
#[test]
fn every_subcommand_is_named_in_usage() {
    let help = crate::cmd::args::help_tree(&command());
    for name in plane_subcommands() {
        let spelled = format!("vike-cli backtest {name}");
        assert!(help.contains(&spelled), "the help must offer `{spelled}`:\n{help}");
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
        // refused by `decide_mode`'s combination checks in `ProfileBacktest::refusals`.
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
/// third mode inside one parser. `crate::cmd::backtest::params::run_params` reads one file, or the
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
/// `crate::cmd::backtest::read::show`'s `the_refusal_roster_no_longer_names_a_renderer_that_ships`
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

/// ⚠ The plane's roster is the `clap` command's own, so this is the completeness test the old hand
/// copy (`SUBCOMMANDS`, spelled out because a `const` could not map over a slice) owed: every
/// [`READ_SUBCOMMANDS`] entry is a sub-verb of the command, and the command holds nothing but those
/// plus `run`.
///
/// ⚠ **`run` is still the ONE non-reading sub-verb, and the authoring three did not change
/// that.** `templates`, `script-api` and `script-check` all joined [`READ_SUBCOMMANDS`] as well as
/// the command — they compute no backtest, dial nothing and share [`parse_read`]'s flag set — so
/// the `+ 1` below is unchanged and still means exactly `run`. A verb that computed would have to
/// change this number, and the right response then is to re-argue it here rather than to bump it.
#[test]
fn the_roster_carries_every_reading_subverb() {
    let names = plane_subcommands();
    for r in READ_SUBCOMMANDS {
        assert!(
            names.iter().any(|n| n == r.as_str()),
            "`{}` is a reading sub-verb that the command does not name — every refusal that \
             derives its roster from the command would name the set short",
            r.as_str()
        );
    }
    assert_eq!(
        names.len(),
        READ_SUBCOMMANDS.len() + 1,
        "the command is `run` plus the reading roster and nothing else"
    );
}

/// `crate::surface::sub_verbs` is read off the command, in the command's order, and it is the INNER
/// LOOP BOUND of [`the_surface_and_the_parsers_agree_about_every_flag_and_sub_verb`], the
/// `sub_verbs` array of the PUBLISHED `cli.json` and its `all_subcommands` roster (a literal once,
/// and three sub-verbs short of the command while `cli.json` advertised the short set).
///
/// This test lives HERE rather than in `tests/` because the command and [`Sub`] are private to this
/// module.
#[test]
fn the_surface_names_exactly_the_sub_verbs_this_plane_dispatches() {
    let real = plane_subcommands();
    assert_eq!(crate::surface::sub_verbs(), real, "the published `sub_verbs` array");
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

/// **The published `params` refusals are the parser's**: the `params_refused` roster renders
/// [`PARAMS_REFUSED`] and `refuse_a_run_flag_on_params` itself (it was six flags short and spelled
/// "strategy roster" for "strategy file" while it was a copy), and every flag row's `params` refusal,
/// still a copy, is the sentence that function prints.
#[test]
fn the_params_roster_publishes_the_parsers_refusals() {
    use crate::cmd::backtest::run_args::refuse_a_run_flag_on_params;
    for f in crate::surface::FLAGS.iter() {
        for r in f.refusals.iter().filter(|r| r.on == "params") {
            assert_eq!(r.message(), refuse_a_run_flag_on_params(f.long).to_string(), "{}", f.long);
        }
    }
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

/// …and so is a `--preset` with no file — it is a profile-building input.
#[test]
fn a_preset_alone_is_also_a_complete_command_line() {
    let argv = ["--preset", "fast.toml"];
    parse_run_args(argv.iter().map(|s| (*s).to_string()))
        .unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
}

/// `--script` on `run` is no longer supported: the run's own reading refuses it (alone too), on the
/// REFUSAL rung, never read as a source to inject — and only after every usage rule, so a malformed
/// command line is still answered as one.
#[test]
fn script_on_run_is_refused_on_the_refusal_rung() {
    let read = |argv: &[&str]| {
        let argv = std::iter::once("run").chain(argv.iter().copied()).map(str::to_string);
        match crate::cmd::backtest::parse(argv) {
            Ok(crate::cmd::backtest::Sub::Run(cmd)) => cmd.into_args(),
            _ => panic!("reads as `run`"),
        }
    };
    for argv in [&["--profile", "p.toml", "--script", "s.rhai"][..], &["--script", "s.rhai"]] {
        let refused = read(argv).expect_err("a script is refused");
        assert_eq!(refused.exit, crate::exit::Exit::Refused, "{argv:?}");
        assert_eq!(
            refused.message,
            "error [backtest.script_unsupported]: --script is no longer supported: put the strategy \
             in a file under strategies/ and name it with `[strategy] file`"
        );
    }
    let usage = read(&["--profile", "p.toml", "--fee", "1", "--script", "s.rhai"]).unwrap_err();
    assert_eq!(
        usage.exit,
        crate::exit::Exit::Usage,
        "a usage rule answers first: {}",
        usage.message
    );
    assert!(read(&["--profile", "p.toml"]).is_ok(), "no script, no refusal");
}
