//! The profile builder, the selection sugar, the implied defaults, `--param` and `--decide`.

use super::*;

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
