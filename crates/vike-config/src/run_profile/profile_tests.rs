use super::*;
use std::assert_matches;

// ---------------------------------------------------------------------------------------------
// Round-trip: parse each sample -> assert the expected structured values.
// ---------------------------------------------------------------------------------------------

#[test]
fn backtest_sample_round_trips() {
    let p = samples::backtest().expect("backtest parses");
    assert_eq!(p.name.as_deref(), Some("btcusdt-1m-backtest"));
    // sinks omitted -> all off (headless)
    assert!(p.sinks.journal.is_none());
    assert!(p.sinks.equity_sample_ms.is_none());
    // risk
    assert_eq!(p.risk.tick_size, Some(0.1));
    // No `max_leverage`: a backtest sample must not arm the buying-power check. (#817 set
    // `max_leverage = 1.0` here when the knob was inert; now that it CONVERTS to
    // `im_requirement`, leaving it would silently arm a 1× margin gate on every backtest.)
    assert_eq!(p.risk.max_leverage, None);
    assert_eq!(p.risk.to_risk_limits().im_requirement, None);
    // guards
    assert_eq!(p.guards.max_drawdown, Some(0.25));
    assert_eq!(p.guards.initial_trading_state, ProfileTradingState::Active);
    assert_eq!(p.guards.submit_ack_timeout(), None);
}

#[test]
fn paper_sample_round_trips() {
    let p = samples::paper().expect("paper parses");
    // risk
    assert_eq!(p.risk.max_orders_per_window, Some(20));
    assert_eq!(p.risk.window_ms, 1000);
    // `max_leverage` is the only leverage key; 10x ⇒ the enforced 10% initial margin.
    assert_eq!(p.risk.max_leverage, Some(10.0));
    assert_eq!(p.risk.im_requirement(), Some(0.1));
    assert!(p.risk.block_reduce_only_overshoot);
    // `min_qty` absent from PAPER_ROWS's [risk] block -> Option field defaults to None.
    assert_eq!(p.risk.min_qty, None);
    // guards -> Duration mapping
    assert_eq!(p.guards.submit_ack_timeout(), Some(Duration::from_millis(30_000)));
    // The grace is `Option` now — `None` would mean "the sample names none", not "5 s".
    assert_eq!(p.guards.submit_ack_confirm_grace(), Some(Duration::from_millis(15_000)));
    assert_eq!(p.guards.max_drawdown, Some(0.20));
    assert!(p.guards.conditionals_on_ticks);
    assert!(p.guards.margin_call.is_none());
}

#[test]
fn live_sample_round_trips() {
    let p = samples::live().expect("live parses");
    assert_eq!(p.sinks.equity_sample(), Some(Duration::from_millis(1000)));
    let j = p.sinks.journal.as_ref().expect("the journal tuning table is present");
    assert!(j.dir.is_none(), "the sample names no directory: that is the row's");
    assert_eq!(j.segment_bytes, Some(67_108_864));
    assert_eq!(j.snapshot_every, Some(1024));
    assert_eq!(p.guards.initial_trading_state, ProfileTradingState::Active);
    assert_eq!(p.guards.max_drawdown, Some(0.15));
    assert!(p.guards.margin_call.is_some());
    // The live sample is for `demo`/`live` accounts, whose mount refuses a venue-owned
    // instrument field (`ProfileRisk::apply_to` under the venue-fetched grid) — it carries none.
    assert_eq!(p.risk.min_qty, None);
    assert_eq!(p.risk.tick_size, None);
    assert_eq!(p.risk.lot_size, None);
    assert_eq!(p.risk.min_notional, None);
}

#[test]
fn all_samples_validate() {
    for (name, load) in samples::ALL {
        load().unwrap_or_else(|e| panic!("sample `{name}` must be valid: {e}"));
    }
}

/// **No shipped sample carries a retired key.** The paper and live samples once set `sinks.gui`,
/// `sinks.recorder` and `guards.freshness_ms`, so a profile an operator started from one of them
/// warned at every boot; the keys are refused by name now, so a sample that still held one would
/// not load at all (`all_samples_validate`). This pins the cause rather than the symptom: the four
/// tombstones are empty on every sample, read off the parsed profile.
#[test]
fn no_shipped_sample_carries_a_retired_key() {
    for (name, load) in samples::ALL {
        let p = load().unwrap_or_else(|e| panic!("sample `{name}` must be valid: {e}"));
        assert!(p.sinks.gui.is_none(), "sample `{name}` sets sinks.gui");
        assert!(p.sinks.recorder.is_none(), "sample `{name}` sets sinks.recorder");
        assert!(p.sinks.raw_capture_dir.is_none(), "sample `{name}` sets sinks.raw_capture_dir");
        assert!(p.guards.freshness_ms.is_none(), "sample `{name}` sets guards.freshness_ms");
    }
}

// ---------------------------------------------------------------------------------------------
// Mapping to the real runtime types (the compile-checked converters).
// ---------------------------------------------------------------------------------------------

#[test]
fn maps_to_real_risk_limits() {
    let p = samples::live().unwrap();
    let got = p.risk.to_risk_limits();
    let want = vike_model::RiskLimits {
        // The live sample carries no instrument-grid fields (a `demo`/`live` account's mount
        // refuses them — the venue owns its grid).
        tick_size: None,
        lot_size: None,
        min_notional: None,
        min_qty: None,
        max_notional_per_order: Some(50_000.0),
        max_total_exposure: Some(200_000.0),
        max_orders_per_window: Some(20),
        window_ms: 1000,
        max_leverage: Some(5.0),
        block_reduce_only_overshoot: true,
        // DERIVED from `max_leverage` at the config edge — LIVE_ROWS has no `im_requirement`
        // key at all (issue #822: one operator-facing name, one enforced storage field).
        im_requirement: Some(0.2),
        im_by_symbol: Default::default(),
        required_free_bp_pct: 0.05,
        max_slippage_bps: None,
        require_fillable: false,
        price_collar: None,
        collar_by_symbol: Default::default(),
        grid_by_symbol: Default::default(),
        // ⚠ `None`, and this assertion is what PINS that a run profile cannot arm the
        // ACCOUNT-aggregate ceiling: `LIVE_ROWS` is the fullest `[risk]` table in the tree, and
        // the converter still produces `None` here. That ceiling's authority is the
        // `policy.max_account_exposure` row, deliberately — see
        // `vike_model::ProfileRisk::to_risk_limits`'s own line. If a `[risk]
        // max_account_exposure` key is ever accepted, this line is where the change becomes
        // visible.
        max_account_exposure: None,
        // ⚠ `None`, and the same pin one axis over: the fullest `[risk]` table in the tree
        // still cannot arm the SIZING-EQUITY ceiling. Its authority is the
        // `policy.max_sizing_equity` row; if a `[risk] max_sizing_equity` key is ever
        // accepted, this line is where the change becomes visible.
        max_sizing_equity: None,
    };
    assert_eq!(got, want);
}

#[test]
fn trading_state_maps_to_the_real_one() {
    let p = samples::live().unwrap();
    assert_eq!(p.guards.trading_state(), vike_model::TradingState::Active);
}

/// A margin-call table that leaves `warn_fraction` / `buffer` off takes `MarginCallConfig::default`'s
/// values at parse (the table IS that type; `vike_tradehub_core`'s glue tests pin the numbers).
#[test]
fn margin_call_keys_left_off_take_the_default() {
    let toml = r#"
[guards.margin_call]
mm_requirement = 0.04
"#;
    let p = RunProfile::from_toml_str(toml).unwrap();
    let mc = p.guards.margin_call.unwrap();
    assert_eq!(
        mc,
        vike_model::MarginCallConfig {
            mm_requirement: 0.04,
            ..vike_model::MarginCallConfig::default()
        }
    );
}

/// `mm_requirement` stays REQUIRED and the table stays closed: a missing key or a stray one is a
/// parse error, exactly as when this table was a separate mirror type.
#[test]
fn margin_call_requires_mm_requirement_and_denies_unknown_keys() {
    let missing = "[guards.margin_call]\nbuffer = 0.1\n";
    assert!(RunProfile::from_toml_str(missing).is_err());
    let stray = "[guards.margin_call]\nmm_requirement = 0.04\nwarn_fraction_pct = 5\n";
    assert!(RunProfile::from_toml_str(stray).is_err());
}

#[test]
fn sinks_parses_equity_sample_ms() {
    let toml = r#"
[sinks]
equity_sample_ms = 1000
"#;
    let p = RunProfile::from_toml_str(toml).unwrap();
    assert_eq!(p.sinks.equity_sample(), Some(Duration::from_millis(1000)));

    // absent -> None
    let toml2 = r#"
[sinks]
"#;
    let p2 = RunProfile::from_toml_str(toml2).unwrap();
    assert_eq!(p2.sinks.equity_sample(), None);
}

#[test]
fn halted_maps_to_kill_switch() {
    let toml = r#"
[guards]
initial_trading_state = "halted"
"#;
    let p = RunProfile::from_toml_str(toml).unwrap();
    assert_eq!(p.guards.trading_state(), vike_model::TradingState::Halted);
}

// ---------------------------------------------------------------------------------------------
// Unknown-field policy: DENY (top-level AND nested).
// ---------------------------------------------------------------------------------------------

#[test]
fn unknown_top_level_field_rejected() {
    let toml = r#"
surprise = true
"#;
    let err = RunProfile::from_toml_str(toml).unwrap_err();
    assert_matches!(err, ProfileError::Parse(_), "got {err:?}");
    assert!(err.to_string().contains("surprise"), "message names the key: {err}");
}

#[test]
fn unknown_nested_field_rejected() {
    // a typo'd risk key must NOT be silently ignored
    let toml = r#"
[risk]
max_levarage = 10.0
"#;
    let err = RunProfile::from_toml_str(toml).unwrap_err();
    assert_matches!(err, ProfileError::Parse(_), "got {err:?}");
}

// ---------------------------------------------------------------------------------------------
// Malformed input -> a clear Parse error (not a panic).
// ---------------------------------------------------------------------------------------------

#[test]
fn malformed_toml_is_parse_error() {
    let err = RunProfile::from_toml_str("this is not = = toml [[[").unwrap_err();
    assert_matches!(err, ProfileError::Parse(_), "got {err:?}");
}

#[test]
fn bad_enum_value_is_parse_error() {
    let toml = r#"
[guards]
initial_trading_state = "hyperspeed"
"#;
    let err = RunProfile::from_toml_str(toml).unwrap_err();
    assert_matches!(err, ProfileError::Parse(_), "got {err:?}");
}

// ---------------------------------------------------------------------------------------------
// Semantic validation -> clear Validation errors.
// ---------------------------------------------------------------------------------------------

fn assert_validation(toml: &str, needle: &str) {
    let err = RunProfile::from_toml_str(toml).unwrap_err();
    match err {
        ProfileError::Validation(m) => {
            assert!(m.contains(needle), "expected `{needle}` in validation message, got: {m}")
        }
        other => panic!("expected Validation error, got {other:?}"),
    }
}

// ⚠ SEVEN tests stood here and are DELETED with the two tables they were about
// (`live_with_paper_broker_rejected`, `live_with_hist_source_rejected`,
// `backtest_with_venue_broker_rejected`, `backtest_with_live_source_rejected`,
// `paper_broker_zero_seed_cash_rejected`, `venue_broker_missing_venue_rejected`,
// `empty_symbol_rejected`). Every one of them asserted a cross-check between `mode` and a
// field NO RUNNER read, so none of them was protecting behaviour — which is the finding that
// deleted the tables. The two tests below replace all seven and assert the one thing that IS
// behaviour now: the tables are refused BY NAME.

/// **Each deleted table is refused, by name, whatever it contains** — the refusal
/// `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md` asks for in place
/// of a migration, and the reason the fields survive as tombstones rather than simply going.
///
/// ⚠ **Shape, not content.** Both tombstones are `serde::de::IgnoredAny`, so the refusal must
/// fire on a table the old schema would have called PERFECT and on one it would have called
/// malformed alike — an operator whose profile was correct is exactly the one who must be told
/// what happened rather than shown a parser complaining about a word.
///
/// ⚠ **Mutation proof (production code, not the harness):** delete the `for` loop at the top
/// of `RunProfile::validate` and this goes red on the `Validation` arm — `from_toml_str` then
/// ACCEPTS both tables in silence, which is the state the tombstones exist to prevent. Make
/// the fields plain `#[serde(skip)]` instead and it goes red on the message, with serde's
/// bare *"unknown field"*, which is the state the tombstones exist to improve on.
#[test]
fn each_deleted_table_is_refused_by_name_whatever_it_holds() {
    let well_formed = [
        "[event_source]\nkind = \"live_venue\"\nvenue = \"binance\"\nsymbol = \"BTCUSDT\"\n",
        "[broker]\nkind = \"venue\"\nvenue = \"binance\"\nseed_cash = 25000.0\n",
    ];
    // The same two tables holding something the old schema would have refused outright.
    let malformed = ["[event_source]\nkind = \"nonsense\"\n", "[broker]\nwhat = 1\n"];

    for (table, body) in ["event_source", "broker"].into_iter().zip(well_formed) {
        let toml = body.to_string();
        let err = RunProfile::from_toml_str(&toml).unwrap_err();
        let ProfileError::Validation(m) = err else {
            panic!("`[{table}]` must be a NAMED refusal, not a parse error: {err:?}");
        };
        assert!(m.contains(table), "the refusal must name the table: {m}");
        assert!(m.contains("no longer part of a run profile"), "{m}");
        assert!(m.contains("Delete the whole"), "…and say what to do: {m}");
    }
    for (table, body) in ["event_source", "broker"].into_iter().zip(malformed) {
        let toml = body.to_string();
        let err = RunProfile::from_toml_str(&toml).unwrap_err();
        let ProfileError::Validation(m) = err else {
            panic!("`[{table}]`'s CONTENT must not decide the refusal: {err:?}");
        };
        assert!(m.contains(table), "{m}");
    }
}

/// …and the converse, which is the half that says the deletion actually happened: a profile
/// with NEITHER table loads. Without it the test above would pass just as well against a
/// schema that still required them.
#[test]
fn a_profile_carrying_neither_deleted_table_loads() {
    let p = RunProfile::from_toml_str("").expect("neither table is required");
    assert!(p.event_source.is_none() && p.broker.is_none());
}

#[test]
fn negative_tick_size_rejected() {
    let toml = r#"
[risk]
tick_size = -0.1
"#;
    assert_validation(toml, "`risk.tick_size`");
}

#[test]
fn leverage_below_one_rejected() {
    let toml = r#"
[risk]
max_leverage = 0.5
"#;
    assert_validation(toml, "`risk.max_leverage` must be finite and >= 1.0");
}

/// Issue #822: `[risk] im_requirement` is RETIRED from the operator surface — `max_leverage`
/// is the only leverage key, and the config edge derives `im_requirement` from it. Because
/// `ProfileRisk` is `deny_unknown_fields`, a profile still setting the old key fails LOUDLY
/// naming it, rather than parsing fine and silently arming nothing (a silently-ignored risk
/// key is exactly the live-money footgun the DENY policy exists for).
#[test]
fn retired_im_requirement_key_is_rejected_by_name() {
    let toml = r#"
[risk]
im_requirement = 0.1
"#;
    let err = RunProfile::from_toml_str(toml).expect_err("the retired key must not parse");
    let msg = err.to_string();
    assert!(msg.contains("im_requirement"), "the error must name the retired key: {msg}");
}

#[test]
fn max_orders_without_window_rejected() {
    let toml = r#"
[risk]
max_orders_per_window = 10
"#;
    assert_validation(toml, "`risk.window_ms` must be > 0");
}

// ---------------------------------------------------------------------------------------------
// The profile states no tier (decision 0156 item 7): a stored `mode` is refused like any unknown
// key, and the venue-owned `[risk]` fields are judged at the MOUNT, by the account's tier.
// ---------------------------------------------------------------------------------------------

/// **`mode` is gone, and a body still carrying it does not load.** No ignore rule and no
/// tombstone: the deny policy refuses it like any key the type does not have, naming it, so the
/// daemon's startup error names the stored row (`vike_tradehub::profile_rows::rows_to_run_profile`
/// adds the command that re-writes the body without it).
#[test]
fn a_stored_mode_key_is_refused_as_unknown() {
    for word in ["backtest", "paper", "live"] {
        let err = RunProfile::from_toml_str(&format!("mode = \"{word}\"\n")).unwrap_err();
        assert_matches!(err, ProfileError::Parse(_), "got {err:?}");
        assert!(err.to_string().contains("unknown field `mode`"), "names the key: {err}");
    }
}

/// The venue-owned instrument fields LOAD: whether they may apply depends on the tier of the
/// account a mount runs on, which a profile does not state. The refusal a `mode = "live"` profile
/// used to meet here is made at the mount of each `demo`/`live` account instead
/// (`a_demo_or_live_account_refuses_the_profiles_instrument_fields` below, and
/// `crates/vike-tradehub-core/src/budget_tests.rs`'s merge-site twin).
#[test]
fn instrument_fields_load_because_the_profile_states_no_tier() {
    let toml = r#"
[risk]
tick_size = 0.1
lot_size = 0.01
min_qty = 0.5
min_notional = 5.0
"#;
    let p = RunProfile::from_toml_str(toml).expect("a profile may carry the instrument grid");
    assert_eq!(p.risk.tick_size, Some(0.1));
    assert_eq!(p.risk.lot_size, Some(0.01));
    assert_eq!(p.risk.min_qty, Some(0.5));
    assert_eq!(p.risk.min_notional, Some(5.0));
}

// ---------------------------------------------------------------------------------------------
// RunProfile::apply_risk — the grid source is the mounted account's TIER.
// ---------------------------------------------------------------------------------------------

/// A `paper` account fetches no grid, so the profile's own instrument fields take effect.
#[test]
fn a_paper_account_takes_the_profiles_instrument_fields() {
    let p = samples::backtest().unwrap();
    let got = p.apply_risk(vike_model::RiskLimits::new(), AccountTier::Paper).expect("paper is Ok");
    assert_eq!(got.tick_size, p.risk.tick_size);
    assert_eq!(got.lot_size, p.risk.lot_size);
    assert_eq!(got.min_notional, p.risk.min_notional);
}

/// A `demo` or `live` account mounts the venue's grid, so a profile that sets any of the four
/// venue-owned fields is refused, every offending key named — each field on its own, so a gate that
/// dropped one field would fail here.
#[test]
fn a_demo_or_live_account_refuses_the_profiles_instrument_fields() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        for (line, key) in [
            ("tick_size = 0.1", "risk.tick_size"),
            ("lot_size = 0.01", "risk.lot_size"),
            ("min_qty = 0.5", "risk.min_qty"),
            ("min_notional = 5.0", "risk.min_notional"),
        ] {
            let p = RunProfile::from_toml_str(&format!("[risk]\n{line}\n")).expect("loads");
            let err = p
                .apply_risk(vike_model::RiskLimits::new(), tier)
                .expect_err("a demo/live account owns no profile grid");
            let ProfileError::Validation(m) = err else { panic!("expected Validation: {err:?}") };
            assert!(m.contains(key), "{tier}: names `{key}`: {m}");
        }
    }
}

/// A profile confined to the operator budget merges on every tier — the common case stays exactly
/// as easy as before.
#[test]
fn an_operator_only_profile_applies_on_every_tier() {
    let live = samples::live().unwrap();
    for tier in AccountTier::ALL {
        let got = live
            .apply_risk(vike_model::RiskLimits::new(), tier)
            .unwrap_or_else(|e| panic!("{tier}: an operator-only profile merges: {e}"));
        assert_eq!(got.tick_size, None, "{tier}: the live sample carries no instrument fields");
        assert_eq!(got.max_notional_per_order, live.risk.max_notional_per_order);
    }
}

// ⚠ `grid_source_from_{live,backtest,paper}_profile_*`, `apply_risk_uses_the_mode_derived_grid_source`
// and the three `risk_for_live_venue_mount_*` tests stood here and are DELETED with `mode`, the
// derivation they pinned and the guard they exercised (decision 0156 item 7). The tier-keyed tests
// above replace them.

#[test]
fn bad_max_drawdown_rejected() {
    let toml = r#"
[guards]
max_drawdown = 1.5
"#;
    assert_validation(toml, "`guards.max_drawdown` must be in (0.0, 1.0]");
}

// ⚠ THREE more tests stood here and went with `[broker]`/`[event_source]`:
// `max_drawdown_on_a_venue_broker_without_a_positive_seed_cash_rejected`, its converse
// `a_venue_broker_without_a_drawdown_guard_still_loads_with_no_seed_cash`, and
// `recorder_without_live_source_rejected`. The first two asserted a load-time refusal over
// `broker.seed_cash` — a number no runner ever carried into `CoreConfig::seed_cash`, so the
// drawdown latch they described was never fed by it. The surviving check is
// `vike_tradehub::config::MountCfg`'s, at the row the daemon's seed is actually read from, and
// its own tests cover it. The third compared two keys that are BOTH unwired.

#[test]
fn journal_snapshot_every_zero_rejected() {
    let toml = r#"
[sinks.journal]
snapshot_every = 0
"#;
    assert_validation(toml, "`sinks.journal.snapshot_every` must be >= 1");
}

/// **The retired `sinks.journal.dir` is refused BY NAME, and the refusal names the fix** — the
/// `config.journal_dir` row and a `bootstrap-run` without the key (decision 0111, amended
/// 2026-10-11). A profile written before the ruling must not load as if it still pointed the
/// journal somewhere: the directory is the row's alone.
///
/// ⚠ **Mutation proof (production code):** delete the `sinks.journal.dir` block at the top of
/// `RunProfile::validate` and this goes red on the `Validation` arm (the profile then LOADS, and
/// its directory silently names nothing); turn the tombstone into a plain absence and it goes red
/// the same way, with serde's bare *"unknown field"* parse error.
#[test]
fn the_retired_journal_dir_is_refused_by_name_with_the_fix() {
    for body in ["dir = \"data/journal\"\n", "dir = \"data/journal\"\nsnapshot_every = 64\n"] {
        let toml = format!("[sinks.journal]\n{body}");
        let err = RunProfile::from_toml_str(&toml).unwrap_err();
        let ProfileError::Validation(m) = err else {
            panic!("`sinks.journal.dir` must be a NAMED refusal, not a parse error: {err:?}");
        };
        assert!(m.contains("`sinks.journal.dir` is no longer part of a run profile"), "{m}");
        assert!(m.contains("vike-cli config set config.journal_dir <dir>"), "the fix: {m}");
        assert!(m.contains("bootstrap-run"), "…and the re-run without the key: {m}");
    }
    // …and the converse: a tuning-only table loads.
    let toml = "[sinks.journal]\nsnapshot_every = 64\n";
    let p = RunProfile::from_toml_str(toml).expect("a tuning-only [sinks.journal] loads");
    assert_eq!(p.sinks.journal.and_then(|j| j.snapshot_every), Some(64));
}

/// **The four retired keys no mount ever applied are refused BY NAME, whatever value they hold, and
/// the refusal names the fix** — the `sinks.journal.dir` shape, one key at a time. A stored body
/// written before the retirement must not load as if the key still did something, and (under
/// `deny_unknown_fields` alone) must not fail with a bare *"unknown field"* that reads like a typo.
///
/// ⚠ **Mutation proof (production code):** delete the retired-keys loop in `RunProfile::validate`
/// and every case below goes red on the `Validation` arm (the profile then LOADS, the key silently
/// naming nothing); turn a tombstone into a plain absence and it goes red the same way, with
/// serde's *"unknown field"* parse error.
#[test]
fn each_retired_key_is_refused_by_name_whatever_it_holds() {
    for (table, key, values) in [
        ("guards", "freshness_ms", ["5000", "0"]),
        ("sinks", "gui", ["true", "false"]),
        ("sinks", "recorder", ["true", "false"]),
        ("sinks", "raw_capture_dir", ["\"/srv/caps\"", "\"\""]),
    ] {
        for value in values {
            let toml = format!("[{table}]\n{key} = {value}\n");
            let err = RunProfile::from_toml_str(&toml).unwrap_err();
            let ProfileError::Validation(m) = err else {
                panic!("`{table}.{key}` must be a NAMED refusal, not a parse error: {err:?}");
            };
            let dotted = format!("{table}.{key}");
            assert!(m.contains(&format!("`{dotted}` is no longer part of a run profile")), "{m}");
            assert!(m.contains(&format!("WITHOUT `--{dotted}`")), "the fix names the flag: {m}");
            assert!(m.contains("bootstrap-run"), "…and the verb that drops it: {m}");
        }
    }
    // …and the converse: a body of the keys that ARE applied loads, beside a neighbour of each
    // retired one (`sinks.equity_sample_ms`, `guards.max_drawdown`).
    let toml = "[sinks]\nequity_sample_ms = 1000\n[guards]\nmax_drawdown = 0.25\n";
    let p = RunProfile::from_toml_str(toml).expect("the applied sinks and guards load");
    assert!(p.sinks.gui.is_none() && p.guards.freshness_ms.is_none());
}

// NOTE: the direct `ProfileRisk::apply_to` unit tests (the venue-grid / operator-budget split)
// MOVED with the type to `crates/vike-model/src/risk/profile_tests.rs` — see that
// module for `apply_to_operator_only_leaves_venue_fields_byte_identical`,
// `apply_to_each_venue_owned_field_is_config_error_when_grid_fetched`,
// `apply_to_names_every_offending_field_in_one_error`,
// `apply_to_no_grid_fetched_profile_supplies_instrument_fields`,
// `apply_to_no_grid_fetched_ignores_bases_venue_fields`,
// `apply_to_matches_to_risk_limits_when_no_grid_fetched`, and
// `apply_to_preserves_fields_neither_side_owns` and
// `the_grid_source_is_the_tier_of_the_mounted_account`. This module keeps only the
// RunProfile-level integration coverage (the tier-keyed `apply_risk` tests above), which
// exercises the path end-to-end.
