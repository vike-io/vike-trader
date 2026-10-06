//! `--preset` merging and `--set`: value typing, the dotted-key walk and the flag.

use super::*;

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
