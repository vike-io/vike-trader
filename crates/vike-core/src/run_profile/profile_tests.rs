use super::samples::{BACKTEST_TOML, LIVE_TOML, PAPER_TOML};
use super::*;
use crate::scratch::Scratch;

// ---------------------------------------------------------------------------------------------
// Round-trip: parse each sample -> assert the expected structured values.
// ---------------------------------------------------------------------------------------------

#[test]
fn backtest_sample_round_trips() {
    let p = RunProfile::from_toml_str(BACKTEST_TOML).expect("backtest parses");
    assert_eq!(p.name.as_deref(), Some("btcusdt-1m-backtest"));
    assert_eq!(p.mode, Mode::Backtest);
    // sinks omitted -> all off (headless)
    assert!(!p.sinks.gui);
    assert!(!p.sinks.recorder);
    assert!(p.sinks.journal.is_none());
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
    let p = RunProfile::from_toml_str(PAPER_TOML).expect("paper parses");
    assert_eq!(p.mode, Mode::Paper);
    assert!(p.sinks.gui);
    assert!(p.sinks.recorder);
    // risk
    assert_eq!(p.risk.max_orders_per_window, Some(20));
    assert_eq!(p.risk.window_ms, 1000);
    // `max_leverage` is the only leverage key; 10x ⇒ the enforced 10% initial margin.
    assert_eq!(p.risk.max_leverage, Some(10.0));
    assert_eq!(p.risk.im_requirement(), Some(0.1));
    assert!(p.risk.block_reduce_only_overshoot);
    // `min_qty` absent from PAPER_TOML's [risk] block -> Option field defaults to None.
    assert_eq!(p.risk.min_qty, None);
    // guards -> Duration mapping
    assert_eq!(p.guards.submit_ack_timeout(), Some(Duration::from_millis(30_000)));
    // The grace is `Option` now — `None` would mean "the sample names none", not "5 s".
    assert_eq!(p.guards.submit_ack_confirm_grace(), Some(Duration::from_millis(15_000)));
    assert_eq!(p.guards.max_drawdown, Some(0.20));
    assert!(p.guards.conditionals_on_ticks);
    assert_eq!(p.guards.freshness(), Some(Duration::from_millis(5_000)));
    assert!(p.guards.margin_call_config().is_none());
}

#[test]
fn live_sample_round_trips() {
    let p = RunProfile::from_toml_str(LIVE_TOML).expect("live parses");
    assert_eq!(p.mode, Mode::Live);
    assert_eq!(p.sinks.equity_sample(), Some(Duration::from_millis(1000)));
    let j = p.sinks.journal.as_ref().expect("journal sink present");
    assert_eq!(j.dir, "data/journal");
    assert_eq!(j.segment_bytes, 67_108_864);
    assert_eq!(j.snapshot_every, 1024);
    assert_eq!(p.guards.initial_trading_state, ProfileTradingState::Active);
    assert_eq!(p.guards.max_drawdown, Some(0.15));
    assert!(p.guards.margin_call.is_some());
    // A `live` profile must never carry a venue-owned instrument field (see
    // `RunProfile::validate`'s unconditional live-mode check) — the sample carries none.
    assert_eq!(p.risk.min_qty, None);
    assert_eq!(p.risk.tick_size, None);
    assert_eq!(p.risk.lot_size, None);
    assert_eq!(p.risk.min_notional, None);
}

#[test]
fn all_samples_validate() {
    for (name, toml) in samples::ALL {
        RunProfile::from_toml_str(toml)
            .unwrap_or_else(|e| panic!("sample `{name}` must be valid: {e}"));
    }
}

// ---------------------------------------------------------------------------------------------
// Mapping to the real runtime types (the compile-checked converters).
// ---------------------------------------------------------------------------------------------

#[test]
fn maps_to_real_risk_limits() {
    let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
    let got = p.risk.to_risk_limits();
    let want = vike_exec::RiskLimits {
        // The live sample carries no instrument-grid fields (a live profile may never set
        // them — see `RunProfile::validate`'s unconditional live-mode check).
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
        // DERIVED from `max_leverage` at the config edge — LIVE_TOML has no `im_requirement`
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
        // ACCOUNT-aggregate ceiling: `LIVE_TOML` is the fullest `[risk]` table in the tree, and
        // the converter still produces `None` here. That ceiling's authority is the
        // `policy.max_account_exposure` row, deliberately — see
        // `vike_exec::ProfileRisk::to_risk_limits`'s own line. If a `[risk]
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
fn maps_to_real_margin_call_and_trading_state() {
    let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
    let mc = p.guards.margin_call_config().expect("margin call present");
    assert_eq!(mc.mm_requirement, 0.05);
    assert_eq!(mc.warn_fraction, 0.05);
    assert_eq!(mc.buffer, 0.10);
    assert_eq!(p.guards.trading_state(), vike_exec::TradingState::Active);
}

#[test]
fn maps_to_real_journal_config() {
    let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
    let jc = p.sinks.journal_config().expect("journal config present");
    assert_eq!(jc.dir, std::path::PathBuf::from("data/journal"));
    assert_eq!(jc.file.segment_bytes, 67_108_864);
    assert_eq!(jc.file.flush_every, 256);
    assert_eq!(jc.snapshot_every, 1024);
}

#[test]
fn margin_call_defaults_apply() {
    // omit warn_fraction/buffer -> LEAN defaults
    let toml = r#"
mode = "live"
[guards.margin_call]
mm_requirement = 0.04
"#;
    let p = RunProfile::from_toml_str(toml).unwrap();
    let mc = p.guards.margin_call.unwrap();
    assert_eq!(mc.mm_requirement, 0.04);
    assert_eq!(mc.warn_fraction, 0.05);
    assert_eq!(mc.buffer, 0.10);
}

#[test]
fn sinks_parses_equity_sample_ms() {
    let toml = r#"
mode = "live"
[sinks]
equity_sample_ms = 1000
"#;
    let p = RunProfile::from_toml_str(toml).unwrap();
    assert_eq!(p.sinks.equity_sample(), Some(Duration::from_millis(1000)));

    // absent -> None
    let toml2 = r#"
mode = "live"
[sinks]
"#;
    let p2 = RunProfile::from_toml_str(toml2).unwrap();
    assert_eq!(p2.sinks.equity_sample(), None);
}

#[test]
fn halted_maps_to_kill_switch() {
    let toml = r#"
mode = "live"
[guards]
initial_trading_state = "halted"
"#;
    let p = RunProfile::from_toml_str(toml).unwrap();
    assert_eq!(p.guards.trading_state(), vike_exec::TradingState::Halted);
}

// ---------------------------------------------------------------------------------------------
// Unknown-field policy: DENY (top-level AND nested).
// ---------------------------------------------------------------------------------------------

#[test]
fn unknown_top_level_field_rejected() {
    let toml = r#"
mode = "backtest"
surprise = true
"#;
    let err = RunProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, ProfileError::Parse(_)), "got {err:?}");
    assert!(err.to_string().contains("surprise"), "message names the key: {err}");
}

#[test]
fn unknown_nested_field_rejected() {
    // a typo'd risk key must NOT be silently ignored
    let toml = r#"
mode = "backtest"
[risk]
max_levarage = 10.0
"#;
    let err = RunProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, ProfileError::Parse(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------------------------
// Malformed input -> a clear Parse error (not a panic).
// ---------------------------------------------------------------------------------------------

#[test]
fn malformed_toml_is_parse_error() {
    let err = RunProfile::from_toml_str("this is not = = toml [[[").unwrap_err();
    assert!(matches!(err, ProfileError::Parse(_)), "got {err:?}");
}

#[test]
fn bad_enum_value_is_parse_error() {
    let toml = r#"
mode = "hyperspeed"
"#;
    let err = RunProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, ProfileError::Parse(_)), "got {err:?}");
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
        let toml = format!("mode = \"live\"\n{body}");
        let err = RunProfile::from_toml_str(&toml).unwrap_err();
        let ProfileError::Validation(m) = err else {
            panic!("`[{table}]` must be a NAMED refusal, not a parse error: {err:?}");
        };
        assert!(m.contains(table), "the refusal must name the table: {m}");
        assert!(m.contains("no longer part of a run profile"), "{m}");
        assert!(m.contains("Delete the whole"), "…and say what to do: {m}");
    }
    for (table, body) in ["event_source", "broker"].into_iter().zip(malformed) {
        let toml = format!("mode = \"live\"\n{body}");
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
    let p = RunProfile::from_toml_str("mode = \"live\"\n").expect("neither table is required");
    assert_eq!(p.mode, Mode::Live);
    assert!(p.event_source.is_none() && p.broker.is_none());
}

#[test]
fn negative_tick_size_rejected() {
    let toml = r#"
mode = "backtest"
[risk]
tick_size = -0.1
"#;
    assert_validation(toml, "`risk.tick_size`");
}

#[test]
fn leverage_below_one_rejected() {
    let toml = r#"
mode = "backtest"
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
mode = "backtest"
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
mode = "backtest"
[risk]
max_orders_per_window = 10
"#;
    assert_validation(toml, "`risk.window_ms` must be > 0");
}

// ---------------------------------------------------------------------------------------------
// Structural load-time gate: `mode = "live"` may never set a venue-owned `[risk]` field — no
// escape hatch (the corrected design: `GridSource` is derived from `mode`, not a second flag).
// ---------------------------------------------------------------------------------------------

/// One live-mode profile template per venue-owned field name, so each can be proven
/// independently to fail at load (each check narrowed to a single field must still fail this
/// test if a regression drops one field from the gate).
fn live_toml_with_risk_field(field_line: &str) -> String {
    format!(
        r#"
mode = "live"
[risk]
{field_line}
"#
    )
}

#[test]
fn live_mode_with_min_qty_rejected_at_load() {
    assert_validation(&live_toml_with_risk_field("min_qty = 0.5"), "min_qty");
}

#[test]
fn live_mode_with_tick_size_rejected_at_load() {
    assert_validation(&live_toml_with_risk_field("tick_size = 0.1"), "tick_size");
}

#[test]
fn live_mode_with_lot_size_rejected_at_load() {
    assert_validation(&live_toml_with_risk_field("lot_size = 0.01"), "lot_size");
}

#[test]
fn live_mode_with_min_notional_rejected_at_load() {
    assert_validation(&live_toml_with_risk_field("min_notional = 5.0"), "min_notional");
}

#[test]
fn live_mode_names_every_offending_field_when_several_are_set() {
    let toml = r#"
mode = "live"
[risk]
tick_size = 0.1
lot_size = 0.01
"#;
    let err = RunProfile::from_toml_str(toml).unwrap_err();
    let ProfileError::Validation(m) = err else { panic!("expected Validation error: {err:?}") };
    assert!(m.contains("risk.tick_size"), "message: {m}");
    assert!(m.contains("risk.lot_size"), "message: {m}");
}

#[test]
fn live_mode_with_no_instrument_fields_is_allowed() {
    // A `live` profile confined to operator-budget fields is unaffected by the gate — the
    // common case must stay exactly as easy as before.
    let toml = r#"
mode = "live"
[risk]
max_notional_per_order = 1000.0
max_total_exposure = 5000.0
"#;
    RunProfile::from_toml_str(toml).expect("operator-only fields are always allowed under live");
}

#[test]
fn backtest_mode_with_instrument_fields_is_allowed() {
    // backtest never fetches a real venue grid, so the profile is free to set every
    // instrument-grid field with no opt-in of any kind.
    let toml = r#"
mode = "backtest"
[risk]
tick_size = 0.1
lot_size = 0.01
min_qty = 0.5
min_notional = 5.0
"#;
    let p = RunProfile::from_toml_str(toml).expect("backtest may set instrument fields freely");
    assert_eq!(p.risk.tick_size, Some(0.1));
    assert_eq!(p.risk.lot_size, Some(0.01));
    assert_eq!(p.risk.min_qty, Some(0.5));
    assert_eq!(p.risk.min_notional, Some(5.0));
}

#[test]
fn paper_mode_with_instrument_fields_is_allowed() {
    // paper never fetches a real venue grid either — same freedom as backtest.
    let toml = r#"
mode = "paper"
[risk]
tick_size = 0.1
lot_size = 0.01
min_qty = 0.5
min_notional = 5.0
"#;
    let p = RunProfile::from_toml_str(toml).expect("paper may set instrument fields freely");
    assert_eq!(p.risk.tick_size, Some(0.1));
    assert_eq!(p.risk.lot_size, Some(0.01));
    assert_eq!(p.risk.min_qty, Some(0.5));
    assert_eq!(p.risk.min_notional, Some(5.0));
}

// ---------------------------------------------------------------------------------------------
// RunProfile::grid_source — derived straight from `mode`.
// ---------------------------------------------------------------------------------------------

#[test]
fn grid_source_from_live_profile_is_venue_fetched() {
    let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
    assert_eq!(p.grid_source(), GridSource::VenueFetched);
}

#[test]
fn grid_source_from_backtest_profile_is_no_grid_fetched() {
    let p = RunProfile::from_toml_str(BACKTEST_TOML).unwrap();
    assert_eq!(p.grid_source(), GridSource::NoGridFetched);
}

#[test]
fn grid_source_from_paper_profile_is_no_grid_fetched() {
    let p = RunProfile::from_toml_str(PAPER_TOML).unwrap();
    assert_eq!(p.grid_source(), GridSource::NoGridFetched);
}

#[test]
fn apply_risk_uses_the_mode_derived_grid_source() {
    // `RunProfile::apply_risk` must route through the SAME derivation as `grid_source` — a
    // backtest/paper profile's own instrument fields take effect (NoGridFetched), while a
    // fresh `RiskLimits::new()` base for a live profile stays untouched (VenueFetched, and a
    // live profile can never carry instrument fields anyway per `validate`).
    let backtest = RunProfile::from_toml_str(BACKTEST_TOML).unwrap();
    let got = backtest.apply_risk(vike_exec::RiskLimits::new()).expect("backtest is always Ok");
    assert_eq!(got.tick_size, backtest.risk.tick_size);

    let live = RunProfile::from_toml_str(LIVE_TOML).unwrap();
    let got = live
        .apply_risk(vike_exec::RiskLimits::new())
        .expect("live with no instrument fields is Ok");
    assert_eq!(got.tick_size, None, "live has no instrument fields to begin with");
}

// ---------------------------------------------------------------------------------------------
// RunProfile::risk_for_live_venue_mount — the BLOCKING-2(a) guard: a non-`live`-mode profile
// must never reach a real 12-venue live mount, which cannot see `mode` at all.
// ---------------------------------------------------------------------------------------------

#[test]
fn risk_for_live_venue_mount_ok_for_a_live_profile() {
    let live = RunProfile::from_toml_str(LIVE_TOML).unwrap();
    let risk = live.risk_for_live_venue_mount().expect("mode = live must be accepted");
    assert_eq!(risk, &live.risk);
}

#[test]
fn risk_for_live_venue_mount_rejects_a_backtest_profile() {
    let backtest = RunProfile::from_toml_str(BACKTEST_TOML).unwrap();
    let err = backtest
        .risk_for_live_venue_mount()
        .expect_err("a backtest-mode profile must never arm a live 12-venue mount");
    let ProfileError::Validation(m) = err else { panic!("expected Validation error") };
    assert!(m.contains("live"), "error must mention the mode requirement: {m}");
}

#[test]
fn risk_for_live_venue_mount_rejects_a_paper_profile() {
    let paper = RunProfile::from_toml_str(PAPER_TOML).unwrap();
    let err = paper
        .risk_for_live_venue_mount()
        .expect_err("a paper-mode profile must never arm a live 12-venue mount");
    assert!(matches!(err, ProfileError::Validation(_)));
}

#[test]
fn bad_max_drawdown_rejected() {
    let toml = r#"
mode = "backtest"
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
mode = "live"
[sinks.journal]
dir = "data/journal"
snapshot_every = 0
"#;
    assert_validation(toml, "`sinks.journal.snapshot_every` must be >= 1");
}

// ---------------------------------------------------------------------------------------------
// from_path.
// ---------------------------------------------------------------------------------------------

#[test]
fn from_path_reads_and_validates() {
    // Through `write_temp_profile` (below) rather than a path minted inline under the system temp
    // directory: that path was removed by a trailing `remove_file` a failing case never reached.
    let profile = write_temp_profile("from-path", PAPER_TOML);
    let p = RunProfile::from_path(&profile).expect("from_path parses");
    assert_eq!(p.mode, Mode::Paper);
}

#[test]
fn from_path_missing_file_is_io_error() {
    let err = RunProfile::from_path("does/not/exist/vike-nope.toml").unwrap_err();
    assert!(matches!(err, ProfileError::Io(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------------------------
// choose_journal — the pure resolver behind journal_config_from_env (env-free, so tested here).
// ---------------------------------------------------------------------------------------------

#[test]
fn choose_journal_neither_is_none() {
    assert!(super::choose_journal(None, None, None).is_none());
}

#[test]
fn choose_journal_dir_fallback_uses_default_cadence() {
    let jc = super::choose_journal(None, Some("data/journal".into()), None)
        .expect("a dir enables the journal");
    assert_eq!(jc.dir, std::path::PathBuf::from("data/journal"));
    // JournalConfig::at defaults (mirror the [sinks.journal] defaults)
    assert_eq!(jc.file.segment_bytes, 64 * 1024 * 1024);
    assert_eq!(jc.file.flush_every, 256);
    assert_eq!(jc.snapshot_every, 1024);
}

#[test]
fn choose_journal_dir_snapshot_override_applies() {
    let jc = super::choose_journal(None, Some("d".into()), Some(64)).unwrap();
    assert_eq!(jc.snapshot_every, 64);
    // a 0 override is ignored (0 would snapshot on every record — a p99 cliff the CoreThread asserts against)
    let jc0 = super::choose_journal(None, Some("d".into()), Some(0)).unwrap();
    assert_eq!(jc0.snapshot_every, 1024);
}

#[test]
fn choose_journal_profile_is_authoritative() {
    // a LIVE profile carries a [sinks.journal] → that config wins, dir fallback ignored.
    let p = RunProfile::from_toml_str(LIVE_TOML).unwrap();
    let jc = super::choose_journal(Some(&p), Some("ignored/dir".into()), Some(7))
        .expect("profile journal sink present");
    assert_eq!(jc.dir, std::path::PathBuf::from("data/journal"));
    assert_eq!(jc.snapshot_every, 1024); // from the profile, NOT the ignored override
}

#[test]
fn choose_journal_profile_without_journal_sink_is_off_even_with_dir() {
    // a profile present but with no journal sink → OFF; the dir fallback must NOT resurrect it
    // (a present profile is authoritative, so it can never be silently overridden).
    let p = RunProfile::from_toml_str(PAPER_TOML).unwrap();
    assert!(p.sinks.journal.is_none());
    assert!(super::choose_journal(Some(&p), Some("data/journal".into()), None).is_none());
}

// ---------------------------------------------------------------------------------------------
// resolve_profile — the INJECTED-vars resolver (never reads std::env::var itself).
// ---------------------------------------------------------------------------------------------

/// A temp profile that DELETES ITSELF, the file and the directory holding it, when the handle
/// drops: on a passing case and on a failing assertion's unwind alike. It derefs to the profile's
/// `Path`, so every call site reads as it did when this was a bare `PathBuf`.
///
/// The directory is this crate's own `crate::scratch::Scratch`, the guard every vike-core test
/// allocates through (`crates/vike-ops/tests/hygiene/journal_scratch_gate.rs`'s strict rule), so the
/// handle adds only the one FILE inside it.
struct TempProfile {
    /// Owns the directory the profile sits in; its `Drop` IS the cleanup. Never read: it exists to
    /// be dropped, and binding it here rather than to `_` is what keeps it alive for the case.
    _dir: Scratch,
    path: PathBuf,
}

impl std::ops::Deref for TempProfile {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

/// For `RunProfile::from_path`'s generic `impl AsRef<Path>`, which gets no deref coercion.
impl AsRef<Path> for TempProfile {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// The PATH's `Debug`, so a message that formats the profile with `{:?}` prints exactly what it
/// printed when this was a `PathBuf`.
impl std::fmt::Debug for TempProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.path, f)
    }
}

/// Write `toml` to a profile inside a fresh temp directory owned by the returned handle, so
/// parallel cases never collide and nothing outlives the case.
///
/// ⚠ This returned a bare `PathBuf` under the system temp directory ("caller cleans up"), and the
/// cleanup was a trailing `remove_file` that only a PASSING case reached: every failing case leaked
/// its `vike_resolve_profile_*.toml` wherever `TMPDIR` points at a directory nothing sweeps (the
/// CI runners and the verification lanes). The shape is copied from
/// `crates/vike-cli/tests/search_walkforward_cli.rs`'s `write_temp_profile`, which already returned
/// a self-deleting handle; this one owns a `Scratch` where that one owns a `tempfile::TempDir`.
fn write_temp_profile(tag: &str, toml: &str) -> TempProfile {
    let dir = Scratch::created(&format!("run-profile-{tag}"));
    let path = dir.join("profile.toml");
    std::fs::write(&path, toml).expect("write temp profile");
    TempProfile { _dir: dir, path }
}

/// The helper's own contract, pinned because the `resolve_profile` cases below lean on it: the
/// profile is a real file while its handle lives, and once the handle drops neither the file nor
/// the directory holding it is left behind. A helper returning a bare path fails here, because
/// dropping a `PathBuf` removes nothing.
#[test]
fn a_temp_profile_is_removed_when_its_handle_drops() {
    let handle = write_temp_profile("dropcheck", PAPER_TOML);
    let path = handle.to_path_buf();
    let dir = path.parent().expect("a profile path has a parent").to_path_buf();
    assert!(path.is_file(), "the profile must exist while its handle lives: {}", path.display());

    drop(handle);
    assert!(!path.exists(), "the profile outlived its handle: {}", path.display());
    assert!(
        !dir.exists(),
        "the profile's directory outlived its handle. The profile must live in a directory the \
         handle OWNS, never loose in the shared temp root: {}",
        dir.display()
    );
}

/// ...and the half a trailing `remove_file` never had: a case that PANICS part-way leaves nothing
/// behind either, because the unwind drops the handle.
#[test]
fn a_temp_profile_is_removed_when_the_case_panics() {
    let mut seen: Option<PathBuf> = None;
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let handle = write_temp_profile("unwindcheck", PAPER_TOML);
        seen = Some(handle.to_path_buf());
        panic!("a failing assertion, part-way through a case");
    }));
    assert!(unwound.is_err(), "the closure must have panicked");
    let path = seen.expect("the closure wrote the profile before it panicked");
    assert!(!path.exists(), "a panicking case leaked its profile: {}", path.display());
    let dir = path.parent().expect("a profile path has a parent");
    assert!(!dir.exists(), "a panicking case leaked its profile's directory: {}", dir.display());
}

#[test]
fn resolve_profile_no_explicit_no_env_is_ok_none() {
    // Neither an explicit path nor VIKE_RUN_PROFILE in the injected vars → Ok(None), and
    // critically NOT an Err — a run with no profile configured must proceed untouched.
    let vars: HashMap<String, String> = HashMap::new();
    let got = resolve_profile(None, &vars).expect("no profile configured is not an error");
    assert!(got.is_none(), "expected no profile resolved, got {got:?}");
}

#[test]
fn resolve_profile_explicit_wins_over_env() {
    // The env var points at a profile that FAILS validation, while the explicit path points at
    // a good one. If explicit did NOT win, this would return Err, not the backtest sample — so
    // this test fails loudly under either a swapped precedence OR an ignored explicit path.
    //
    // ⚠ The bad profile used to be `backtest` + a `venue` broker, and that pair is no longer a
    // failure of any kind: `[broker]` is deleted. It is now a `[risk]` refusal, which is a
    // table this profile genuinely consumes — a stronger fixture than the one it replaces,
    // because a future deletion cannot make it quietly VALID again without something reading
    // the change.
    let good = write_temp_profile("explicit-good", BACKTEST_TOML);
    let bad_toml = r#"
mode = "backtest"
[risk]
tick_size = -0.1
"#;
    let bad = write_temp_profile("env-bad", bad_toml);

    let mut vars = HashMap::new();
    vars.insert("VIKE_RUN_PROFILE".to_string(), bad.display().to_string());

    let got = resolve_profile(Some(&good), &vars)
        .expect("explicit path must be used, not the broken env path")
        .expect("a profile must resolve");
    assert_eq!(got.mode, Mode::Backtest);
    assert_eq!(got.name.as_deref(), Some("btcusdt-1m-backtest"));
}

#[test]
fn resolve_profile_env_used_when_no_explicit_path() {
    // No explicit path → the injected VIKE_RUN_PROFILE entry is used.
    let path = write_temp_profile("env-only", PAPER_TOML);
    let mut vars = HashMap::new();
    vars.insert("VIKE_RUN_PROFILE".to_string(), path.display().to_string());

    let got = resolve_profile(None, &vars)
        .expect("env-resolved profile must load")
        .expect("a profile must resolve");
    assert_eq!(got.mode, Mode::Paper);
}

#[test]
fn resolve_profile_missing_explicit_file_is_err_not_none() {
    // A typo'd/deleted profile path must be a loud Err, never a silent Ok(None) — the dangerous
    // failure mode this function exists to close off.
    let vars: HashMap<String, String> = HashMap::new();
    let bogus = Path::new("does/not/exist/vike-resolve-profile-nope.toml");
    let err = resolve_profile(Some(bogus), &vars)
        .expect_err("a missing explicit profile path must be an Err");
    assert!(matches!(err, ProfileError::Io(_)), "expected Io error, got {err:?}");
    assert!(
        err.to_string().contains("vike-resolve-profile-nope.toml"),
        "error should name the missing file: {err}"
    );
}

#[test]
fn resolve_profile_missing_env_file_is_err_not_none() {
    // Same dangerous-failure-mode guard, but reached via the env-var path rather than explicit.
    let mut vars = HashMap::new();
    vars.insert(
        "VIKE_RUN_PROFILE".to_string(),
        "does/not/exist/vike-resolve-profile-env-nope.toml".to_string(),
    );
    let err = resolve_profile(None, &vars)
        .expect_err("a missing env-resolved profile path must be an Err");
    assert!(matches!(err, ProfileError::Io(_)), "expected Io error, got {err:?}");
}

#[test]
fn resolve_profile_malformed_toml_is_err_naming_the_file() {
    // A malformed TOML file must fail with a message that names the offending file, so an
    // operator staring at a startup failure knows exactly which file to fix.
    let path = write_temp_profile("malformed", "this is not = = toml [[[");
    let mut vars = HashMap::new();
    vars.insert("VIKE_RUN_PROFILE".to_string(), path.display().to_string());

    let err = resolve_profile(None, &vars).expect_err("malformed TOML must be an Err");
    assert!(matches!(err, ProfileError::Parse(_)), "expected Parse error, got {err:?}");
    let msg = err.to_string();
    assert!(
        msg.contains(&path.display().to_string()),
        "error message must name the offending file {path:?}: {msg}"
    );
}

#[test]
fn resolve_profile_path_precedence_is_pure() {
    // The precedence helper itself: explicit > env > None, with no filesystem access — proven
    // directly so a future edit to resolve_profile can't silently invert the rule undetected.
    let mut vars = HashMap::new();
    vars.insert("VIKE_RUN_PROFILE".to_string(), "from/env.toml".to_string());

    assert_eq!(
        super::resolve_profile_path(Some(Path::new("from/explicit.toml")), &vars),
        Some(PathBuf::from("from/explicit.toml"))
    );
    assert_eq!(super::resolve_profile_path(None, &vars), Some(PathBuf::from("from/env.toml")));
    assert_eq!(super::resolve_profile_path(None, &HashMap::new()), None);
}

// NOTE: the direct `ProfileRisk::apply_to` unit tests (the venue-grid / operator-budget split)
// MOVED with the type to `vike_exec::risk_profile`'s own `#[cfg(test)]` module — see that
// module for `apply_to_operator_only_leaves_venue_fields_byte_identical`,
// `apply_to_each_venue_owned_field_is_config_error_when_grid_fetched`,
// `apply_to_names_every_offending_field_in_one_error`,
// `apply_to_no_grid_fetched_profile_supplies_instrument_fields`,
// `apply_to_no_grid_fetched_ignores_bases_venue_fields`,
// `apply_to_matches_to_risk_limits_when_no_grid_fetched`, and
// `apply_to_preserves_fields_neither_side_owns`. This module keeps only the RunProfile-level
// integration coverage (`apply_risk_uses_the_mode_derived_grid_source`, the live-mode
// structural gate tests, `grid_source_from_*`), which exercises the re-export end-to-end.
