//! The profile builder, the selection sugar, the implied defaults, `--param` and `--decide`.

use super::*;

// ---- the builder ---------------------------------------------------------------------------

#[test]
fn the_builder_applies_overrides_onto_a_base_file() {
    let base = "[engine]\ncash = 1000.0\nvolume_limit = 0.002\n";
    let out = build_profile_toml(
        Some(base),
        &[Override {
            key: "engine.volume_limit".to_string(),
            value: parse_scalar("0.001"),
            origin: Origin::Set,
        }],
    )
    .unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["engine"]["volume_limit"].as_float(), Some(0.001));
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
        Some("[engine]\ncash = 9.5\n[data]\nkind = \"tick\"\n"),
        &[
            Override {
                key: "engine.cash".to_string(),
                value: parse_scalar("5.5"),
                origin: Origin::Set,
            },
            Override {
                key: "engine.cash".to_string(),
                value: parse_scalar("1000.5"),
                origin: Origin::Sugar("--cash"),
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
    assert_eq!(v["engine"]["cash"].as_float(), Some(1000.5), "sugar wins over --set");
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
        Some("[engine.margin_call]\ncover = 4.0\n"),
        &[Override {
            key: "engine.margin_call".to_string(),
            value: parse_scalar("1"),
            origin: Origin::Set,
        }],
    )
    .unwrap_err();
    assert!(err.contains("engine.margin_call"), "{err}");
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
            "trend/buy_hold.rs",
            "--sdk",
            "3",
            "--cash",
            "10000",
        ]
        .map(String::from)
        .into_iter(),
    )
    .expect("parses");
    let out = build_profile_toml(None, &a.overrides).unwrap();
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["data"]["venue"].as_str(), Some("binance"));
    let symbols = v["data"]["symbols"].as_array().expect("an array");
    assert_eq!(symbols.len(), 2);
    assert_eq!(symbols[1].as_str(), Some("ETHUSDT"));
    assert_eq!(v["data"]["interval"].as_str(), Some("1h"));
    assert_eq!(v["data"]["from"].as_str(), Some("2026-01-01T00"), "a DATE is a string");
    assert_eq!(v["data"]["kind"].as_str(), Some("bar"));
    assert_eq!(v["strategy"]["file"].as_str(), Some("trend/buy_hold.rs"));
    assert_eq!(v["strategy"]["sdk"].as_integer(), Some(3), "an SDK number is an integer");
    assert_eq!(v["engine"]["cash"].as_integer(), Some(10000), "a NUMBER keeps its type");
}

/// **The retired cost flags are refused naming the params that replaced them** (the owner's ruling of
/// 2026-11-18: a run's commission and slippage are strategy params), never mapped onto a key.
#[test]
fn the_retired_cost_flags_are_refused_naming_the_params() {
    for (flag, param) in [("--fee", "--param commission="), ("--slippage", "--param slippage=")] {
        let err = parse_run_args([flag, "0.001"].map(String::from).into_iter()).unwrap_err();
        assert!(err.contains(&format!("{flag} is no longer read")), "{flag}: {err}");
        assert!(err.contains("strategy parameter now") && err.contains(param), "{flag}: {err}");
        assert!(err.contains("--set strategy.params."), "{flag}: {err}");
    }
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
    for (sugar, sets) in [
        (vec!["--cash", "10000"], vec!["engine.cash=10000"]),
        (vec!["--decide", "simultaneous"], vec!["engine.decide=simultaneous"]),
        (vec!["--sdk", "3"], vec!["strategy.sdk=3"]),
    ] {
        let a = parse_run_args(sugar.iter().map(|s| (*s).to_string())).unwrap();
        let b = parse_run_args(sets.iter().flat_map(|s| ["--set".to_string(), (*s).to_string()]))
            .unwrap();
        let lhs = build_profile_toml(None, &a.overrides).unwrap();
        let rhs = build_profile_toml(None, &b.overrides).unwrap();
        assert_eq!(lhs, rhs, "{sugar:?} must equal --set {sets:?}");
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
    let symbols: Vec<&str> = rows[0]
        .value
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect();
    assert_eq!(symbols, vec!["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
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
fn a_strategy_file_is_forwarded_unvalidated() {
    // The folder is the backtesthub's, which this side cannot see, so a path is never refused here.
    let a =
        parse_run_args(["--strategy", "other/not_a_real_file.rs"].map(String::from).into_iter())
            .unwrap();
    assert_eq!(explicit(&a)[0].key, "strategy.file");
    assert_eq!(explicit(&a)[0].value.as_str(), Some("other/not_a_real_file.rs"));
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
fn nothing_implies_a_strategy_file() {
    let a = parse_run_args(["--venue", "binance"].map(String::from).into_iter()).unwrap();
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    assert!(
        v.get("strategy").is_none(),
        "a strategy file nobody named would be a guess: the far side refuses the missing key"
    );
}

/// The flags a perp run is typed with on the CI box (2026-11-21), with no `--cash`.
const PERP_FLAGS: [&str; 12] = [
    "--venue",
    "binance",
    "--symbol",
    "BTCUSDT.P",
    "--interval",
    "1h",
    "--from",
    "2026-08-20",
    "--to",
    "2026-11-20",
    "--strategy",
    "trend/buy_hold.rs",
];

fn built_from(extra: &[&str]) -> String {
    let argv = PERP_FLAGS.iter().chain(extra).map(|s| (*s).to_string());
    build_profile_toml(None, &parse_run_args(argv).unwrap().overrides).unwrap()
}

/// A flags-only run (no `--profile`, no `--cash`) builds a profile the REAL loader accepts: it was
/// refused `missing field engine` before `engine.cash` was implied.
#[test]
fn a_flags_only_profile_loads() {
    let text = built_from(&[]);
    vike_backtest::run::profile::ProfileBacktest::from_toml_str(&text)
        .unwrap_or_else(|e| panic!("the flags-only profile must load: {e}\n{text}"));
}

/// **The `--fee` / `--slippage` sentences `cli.json` publishes are the ones the parser prints.** The
/// parser builds them from the model's unit words (`CommissionUnit::WORDS`, `SlippageUnit::WORDS`);
/// the published rows stay literals until the surface is rendered from the parser, so this holds
/// the two byte-equal.
#[test]
fn the_published_retired_cost_sentences_are_the_parsers() {
    use crate::cmd::backtest::run_args::{FEE_RETIRED, SLIPPAGE_RETIRED};
    for (flag, real) in
        [("--fee", FEE_RETIRED.to_string()), ("--slippage", SLIPPAGE_RETIRED.to_string())]
    {
        let row = crate::surface::FLAGS
            .iter()
            .find(|f| f.long == flag)
            .unwrap_or_else(|| panic!("{flag} has a published row"));
        assert_eq!(row.refusals[0].message(), real, "{flag}'s published sentence is stale");
    }
    // The two value rosters this side checks: each published refusal, its hole filled, is the
    // parser's sentence for a typo.
    for (flag, hole) in [("--decide", "{value:?}"), ("--kind", "{value:?}")] {
        let row = crate::surface::FLAGS
            .iter()
            .find(|f| f.long == flag)
            .unwrap_or_else(|| panic!("{flag} has a published row"));
        let real = parse_run_args([flag, "bogus"].map(String::from).into_iter())
            .expect_err("a typo is a local usage error");
        // The parser's error carries the program's `vike-cli backtest: ` head and the refusal's id; the row is the
        // sentence, its id beside it.
        let id = row.refusals[0].id().expect("the parser's own refusal has an id");
        let published = vike_utility::errors::render_refusal(
            id.word(),
            row.refusals[0].message().replace(hole, "\"bogus\""),
        );
        assert_eq!(
            real.trim_end().strip_prefix("vike-cli backtest: "),
            Some(published.as_str()),
            "{flag}'s published refusal is stale"
        );
    }
}

/// **Every profile key this crate spells by hand is one the engine loads**, the one weld for them:
/// the five `[data]` keys the data sugar writes (`crate::cmd::data`'s `KEY_*`), and the keys
/// `route::route_of` reads to pick a carrier (`permutation`, `walkforward` with `groups` and
/// `test_groups`, `paramscan`). The top-level names are `PROFILE_TOP_LEVEL_KEYS`' (held to the
/// loader both ways in `vike-backtest`), so what is checked here is that the loader KNOWS each name:
/// `unknown field` is the one refusal that would mean a misspelling.
#[test]
fn every_profile_key_this_crate_spells_is_one_the_engine_loads() {
    let unknown = |text: &str| {
        vike_backtest::run::profile::ProfileBacktest::from_toml_str(text)
            .err()
            .map(|e| e.to_string())
            .filter(|msg| msg.contains("unknown field"))
    };
    let data = built_from(&[
        "--explain-data",
        "--require-coverage",
        "--max-gap",
        "1d",
        "--on-gap",
        "warn",
        "--universe",
        "strict",
    ]);
    assert_eq!(unknown(&data), None, "a data sugar key the loader does not know:\n{data}");
    for table in ["permutation", "walkforward", "paramscan"] {
        assert!(
            vike_model::run_request::profile::PROFILE_TOP_LEVEL_KEYS.contains(&table),
            "route_of reads [{table}], which no profile declares"
        );
    }
    let walk = format!("{}\n[walkforward]\ngroups = 6\ntest_groups = 2\n", built_from(&[]));
    assert_eq!(unknown(&walk), None, "a [walkforward] key route_of reads is unknown:\n{walk}");
}

/// The owner's perp command: `--set engine.attach_funding=false` with no `--cash` lands in the ONE
/// `[engine]` table beside the implied cash, and the loaded profile has funding OFF (it was refused
/// `missing field cash` at `[engine]`).
#[test]
fn set_attach_funding_false_lands_in_the_one_engine_table() {
    let text = built_from(&["--set", "engine.attach_funding=false"]);
    assert_eq!(text.matches("[engine]").count(), 1, "{text}");
    let profile = vike_backtest::run::profile::ProfileBacktest::from_toml_str(&text)
        .unwrap_or_else(|e| panic!("must load: {e}\n{text}"));
    assert_eq!(profile.engine.attach_funding, Some(false), "{text}");
    assert_eq!(profile.engine.cash, vike_model::run_request::profile::DEFAULT_RUN_CASH, "{text}");
}

/// A `--set` on a table the flags already wrote merges into that table, one header each, every
/// key kept; a `--set` of the very key a flag wrote loses to the flag (spec §5.3).
#[test]
fn a_set_merges_into_every_table_the_flags_wrote() {
    let text = built_from(&[
        "--cash",
        "500",
        "--set",
        "engine.cash=7",
        "--set",
        "engine.attach_funding=false",
        "--set",
        "data.on_gap=warn",
        "--set",
        "strategy.params.size=2.0",
    ]);
    for table in ["[data]", "[engine]", "[strategy]"] {
        assert_eq!(text.matches(table).count(), 1, "{table} once: {text}");
    }
    let v: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(v["engine"]["cash"].as_integer(), Some(500), "the flag wins: {text}");
    assert_eq!(v["engine"]["attach_funding"].as_bool(), Some(false));
    assert_eq!(v["data"]["on_gap"].as_str(), Some("warn"));
    assert_eq!(v["data"]["venue"].as_str(), Some("binance"));
    assert_eq!(v["strategy"]["params"]["size"].as_float(), Some(2.0));
    assert_eq!(v["strategy"]["file"].as_str(), Some("trend/buy_hold.rs"));
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
        assert_eq!(
            *err,
            format!(
                "vike-cli backtest: error [cli.backtest.param.empty_key]: --param takes k=v with a \
                 non-empty key, got {bad:?} (e.g. --param size=1.5)\n"
            ),
            "it must name `--param`, never a flag nobody typed"
        );
    }
}

/// A `--param` key may not be dotted: `[strategy.params]` is a FLAT knob table, and a dotted
/// key there would create a nested table the strategy never reads.
#[test]
fn a_dotted_param_key_is_refused_pointing_at_set() {
    let err = parse_run_args(["--param", "a.b=1"].map(String::from).into_iter()).unwrap_err();
    assert!(err.contains("--set"), "{err}");
}

// ---- `--decide`, the cross-section mode ----------------------------------------------------

/// **`--decide` lands on `engine.decide` as a STRING, spelling-checked against the model's roster.**
///
/// The roster is `vike_model::run_request::decide::DecideMode`'s, the one the engine resolves, so a
/// typo is a local usage error naming both words; the match folds ASCII case as the engine's
/// `decide_mode` does, and an accepted value travels verbatim.
///
/// ⚠ The mutation this fails on, in PRODUCTION: change the `SUGAR` row's `shape` to
/// `Shape::Scalar`. `parse_scalar` then types the value, `engine.decide` lands as something
/// other than a TOML string for any value that parses as a number or a bool, and the first
/// `as_str` assertion reddens — which is the far-side type error
/// `crates/vike-cli/tests/cli_verbs/backtest_flags_schema.rs` exists to keep out of a round trip.
#[test]
fn decide_lands_on_its_engine_key_as_a_string_and_is_checked_against_the_model_roster() {
    let a = set_args(&["--decide", "simultaneous"]);
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap())
        .expect("the built profile parses");
    assert_eq!(v["engine"]["decide"].as_str(), Some("simultaneous"));

    // The `--set` spelling reaches the same key through the same applier.
    let b = set_args(&["--set", "engine.decide=simultaneous"]);
    let w: toml::Value = toml::from_str(&build_profile_toml(None, &b.overrides).unwrap())
        .expect("the built profile parses");
    assert_eq!(w["engine"]["decide"], v["engine"]["decide"], "sugar is not a second path");

    // A spelling the engine refuses is refused HERE, before any spawn or dial, naming the roster.
    let refused = parse_run_args(["--decide", "bogus"].map(String::from).into_iter())
        .expect_err("a typo is a local usage error");
    assert!(
        refused.contains("--decide must be sequential|simultaneous, got \"bogus\""),
        "{refused}"
    );
    // …in the engine's match rule (ASCII case folded), the value forwarded as typed.
    let c = parse_run_args(["--decide", "SIMULTANEOUS"].map(String::from).into_iter())
        .expect("the engine folds case, so this side does too");
    assert!(
        c.overrides.iter().any(|o| o.key == "engine.decide"
            && o.value.as_str() == Some("SIMULTANEOUS")
            && o.origin == Origin::Sugar("--decide")),
        "an accepted value travels verbatim"
    );

    // …and it is a RUN flag, refused on the discovery verb by name rather than dropped.
    let e = parse_read(&["params", "--decide", "sequential"]).unwrap_err();
    assert!(e.contains("--decide"), "{e}");
    assert!(e.contains("backtest run"), "…and names where it belongs: {e}");
}

/// A `--set` search axis reads through the Studio's own axis reader (`parse_axis`): the comma list
/// and the TOML array are one axis, and each value is typed by the knob it sweeps — a FLOAT knob
/// sweeps `5` as `5.0`, an INT knob sweeps `4.0` as `4`, and a knob the profile does not set keeps
/// the text's own kind (decision 0083's rule, the one the Studio applies).
#[test]
fn a_set_axis_reads_like_the_studio_grid_and_is_typed_by_its_knob() {
    let a = set_args(&[
        "--set",
        "paramscan.size=5, 10",
        "--set",
        "paramscan.rungs=[4.0, 4.5]",
        "--set",
        "paramscan.fresh=5, 7.5",
        "--param",
        "size=1.5",
        "--param",
        "rungs=2",
    ]);
    let v: toml::Value = toml::from_str(&build_profile_toml(None, &a.overrides).unwrap()).unwrap();
    let size = v["paramscan"]["size"].as_array().expect("an axis");
    assert_eq!((size[0].as_float(), size[1].as_float()), (Some(5.0), Some(10.0)));
    let rungs = v["paramscan"]["rungs"].as_array().expect("an axis");
    assert_eq!((rungs[0].as_integer(), rungs[1].as_float()), (Some(4), Some(4.5)));
    let fresh = v["paramscan"]["fresh"].as_array().expect("an axis");
    assert_eq!((fresh[0].as_integer(), fresh[1].as_float()), (Some(5), Some(7.5)));
}
