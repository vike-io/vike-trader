//! The `[engine]` knobs: cash gate, margin, volume limit, caps, multipliers, leverage, sessions.

use super::*;

#[test]
fn cash_gate_reaches_the_profile() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ncash_gate = true");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert!(p.engine.cash_gate);
}

/// `cash_gate`'s ONLY readers are `StrategyEngine::run`'s `granular` mask and its
/// `fill_step_gated` branch — both inside the BAR loop — and `fill_step_gated`'s single caller
/// is that branch. `run_ticks` never consults it, so a tick profile setting it would parse,
/// validate, reach `EngineParams` and change nothing: the defect this whole surface exists to
/// refuse, which is why it now joins the lane-asymmetric refusals.
#[test]
fn cash_gate_is_bar_mode_only() {
    let toml = TICK_TOML.replace("snap_to_properties = true", "cash_gate = true");
    refused_at_load(&toml, HarnessError::Validation, &["cash_gate", "bar-mode only"]);
}

/// The companion: an explicit `false` on a tick profile configures nothing and must keep
/// validating — the refusal is on the value that would mislead, not on the key's presence.
#[test]
fn cash_gate_false_on_a_tick_profile_is_accepted() {
    let toml = TICK_TOML.replace("snap_to_properties = true", "cash_gate = false");
    let p = BacktestProfile::from_toml_str(&toml).expect("an explicit false configures nothing");
    assert!(!p.engine.cash_gate);
}

#[test]
fn cash_gate_defaults_off() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert!(!p.engine.cash_gate, "the default must stay byte-identical to before this key");
}

#[test]
fn margin_keys_reach_the_profile() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nmaint_margin = 0.005\nliq_buffer = 0.2\n\
             venue_style_liquidation = true",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.maint_margin, 0.005);
    assert_eq!(p.engine.liq_buffer, 0.2);
    assert!(p.engine.venue_style_liquidation);
}

/// `liq_buffer`'s engine default is 0.10, NOT 0.0 — a serde `Default::default()` here would
/// silently change every profile that omits the key.
#[test]
fn liq_buffer_defaults_to_the_engine_value() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.liq_buffer, 0.10);
    assert_eq!(p.engine.maint_margin, 0.0);
    assert!(!p.engine.venue_style_liquidation);
}

#[test]
fn a_negative_maint_margin_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nmaint_margin = -0.01");
    refused_at_load(&toml, HarnessError::Validation, &["maint_margin"]);
}

#[test]
fn volume_limit_reaches_the_profile() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nvolume_limit = 0.05");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.volume_limit, Some(0.05));
}

#[test]
fn volume_limit_defaults_to_none() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.volume_limit, None);
}

/// A participation cap above 1.0 claims more than the whole bar traded.
#[test]
fn a_volume_limit_over_one_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nvolume_limit = 1.5");
    refused_at_load(&toml, HarnessError::Validation, &["volume_limit"]);
}

#[test]
fn position_caps_reach_the_profile() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nmax_open_positions = 10\nmax_open_long = 6\nmax_open_short = 4",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.max_open_positions, 10);
    assert_eq!(p.engine.max_open_long, 6);
    assert_eq!(p.engine.max_open_short, 4);
}

/// 0 is the engine's "unlimited", which is what every profile did before these keys.
#[test]
fn position_caps_default_to_unlimited() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.max_open_positions, 0);
    assert_eq!(p.engine.max_open_long, 0);
    assert_eq!(p.engine.max_open_short, 0);
}

#[test]
fn multipliers_reach_the_profile() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nmultiplier = 1.0\n\n[engine.multipliers]\n\
             BTCUSDT = 50.0\nETHUSDT = 20.0",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.multiplier, 1.0);
    assert_eq!(p.engine.multipliers.get("BTCUSDT"), Some(&50.0));
    assert_eq!(p.engine.multipliers.get("ETHUSDT"), Some(&20.0));
}

#[test]
fn multiplier_defaults_to_one_and_the_table_is_empty() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.multiplier, 1.0);
    assert!(p.engine.multipliers.is_empty());
}

#[test]
fn a_zero_multiplier_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nmultiplier = 0.0");
    refused_at_load(&toml, HarnessError::Validation, &["multiplier"]);
}

/// The per-symbol table gets its own negative-value test, distinct from the global
/// `multiplier` scalar above — `validate` is first-failure-wins, so this sets exactly one bad
/// per-symbol value and leaves the global scalar at its valid default. The key is a symbol
/// the slice ACTUALLY loads, so the value rule is what fails rather than the key rule below.
#[test]
fn a_zero_per_symbol_multiplier_is_rejected() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.multipliers]\nBTCUSDT = 0.0");
    refused_at_load(&toml, HarnessError::Validation, &["multipliers", "BTCUSDT", "> 0"]);
}

/// A `multipliers` key naming a symbol the run does not load is silently inert today:
/// `StrategyEngine::new` matches by EXACT name and otherwise falls back to the global
/// `multiplier`, so a typo'd row leaves every position sized at the global while the profile
/// reads as contract-sized. The slice is named back so the typo is visible in the message.
#[test]
fn a_multiplier_for_a_symbol_outside_the_slice_is_rejected() {
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.multipliers]\nES = 50.0");
    refused_at_load(
        &toml,
        HarnessError::Validation,
        &["engine.multipliers.ES", "BTCUSDT", "ETHUSDT"],
    );
}

/// ...and the companion, so the rule above cannot be satisfied by refusing everything: a row
/// naming a symbol the slice DOES load still validates.
#[test]
fn a_multiplier_for_a_symbol_in_the_slice_is_accepted() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.multipliers]\nETHUSDT = 50.0");
    let p = BacktestProfile::from_toml_str(&toml).expect("the symbol is in the slice");
    assert_eq!(p.engine.multipliers.get("ETHUSDT"), Some(&50.0));
}

#[test]
fn leverage_keys_reach_the_profile() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\nleverage = 5.0\nclamp_to_leverage = true");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.leverage, Some(5.0));
    assert!(p.engine.clamp_to_leverage);
}

#[test]
fn leverage_defaults_to_none() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.leverage, None);
    assert!(!p.engine.clamp_to_leverage);
}

#[test]
fn a_non_positive_leverage_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nleverage = 0.0");
    refused_at_load(&toml, HarnessError::Validation, &["leverage"]);
}

/// `clamp_to_leverage` with no `leverage` set has nothing to clamp to — the brief's step 4
/// adds this rule but its own step 1 test list omits a case for it, so this closes that gap.
#[test]
fn clamp_to_leverage_without_leverage_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nclamp_to_leverage = true");
    refused_at_load(&toml, HarnessError::Validation, &["clamp_to_leverage"]);
}

/// The end state the two older rules left UNSTATED: `clamp_to_leverage` is unavailable to any
/// profile carrying `[risk]`. Before the reorder, this exact profile was told to add
/// `engine.leverage` — and adding it was then refused by the next rule, a dead end with the
/// governing constraint named nowhere.
#[test]
fn clamp_to_leverage_with_a_risk_table_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nclamp_to_leverage = true\n\n[risk]\nmax_notional_per_order = 1000.0",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("clamp_to_leverage"), "{m}");
            assert!(m.contains("[risk]"), "{m}");
            // The misdirection this replaces: never send the operator to add a key the next
            // rule refuses.
            assert!(
                !m.contains("needs engine.leverage"),
                "the old rule-18 misdirection must not fire first: {m}"
            );
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// ...and with `engine.leverage` supplied as well, the SAME refusal must answer — the clamp
/// rule is what governs, whichever way the operator arrived at the combination.
#[test]
fn clamp_to_leverage_with_a_risk_table_and_leverage_is_rejected_the_same_way() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nleverage = 5.0\nclamp_to_leverage = true\n\n[risk]\n\
             max_notional_per_order = 1000.0",
    );
    refused_at_load(&toml, HarnessError::Validation, &["clamp_to_leverage"]);
}

/// `SimBroker::build_risk_gate` matches `(&p.risk_limits, p.leverage)`; with the clamp OFF its
/// `(Some(l), _)` arm takes `risk_limits` UNCONDITIONALLY whenever `[risk]` is present — even
/// one that never sets `max_leverage` — and silently drops `engine.leverage`. Declaring both
/// must be a named refusal, not a silent precedence pick. (The clamp-ON case has the opposite
/// precedence and its own refusal, two tests above.) The check once read `"[risk]"` OR `"risk"`;
/// the `"[risk]"` alternative was dead (a message holding it holds `"risk"`), so it is now just
/// `"risk"`.
#[test]
fn engine_leverage_with_a_risk_table_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nleverage = 5.0\n\n[risk]\nmax_notional_per_order = 1000.0",
    );
    refused_at_load(&toml, HarnessError::Validation, &["engine.leverage", "risk"]);
}

/// The companion to the rejection above: `engine.leverage` with NO `[risk]` table is exactly
/// the shape `SimBroker::build_risk_gate`'s `(None, Some(lev))` arm consumes, and must keep
/// working.
#[test]
fn engine_leverage_alone_with_no_risk_table_still_works() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nleverage = 5.0");
    let p = BacktestProfile::from_toml_str(&toml).expect("leverage alone, no [risk] table");
    assert_eq!(p.engine.leverage, Some(5.0));
    assert!(p.risk.is_none());
}

#[test]
fn settlement_period_reaches_the_profile() {
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsettlement_period_ms = 86400000");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.settlement_period_ms, Some(86_400_000));
}

#[test]
fn settlement_period_defaults_to_none() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.settlement_period_ms, None);
}

#[test]
fn a_non_positive_settlement_period_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsettlement_period_ms = 0");
    refused_at_load(&toml, HarnessError::Validation, &["settlement_period_ms"]);
}

#[test]
fn session_gate_reaches_the_profile() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsession_gate = true");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert!(p.engine.session_gate);
}

#[test]
fn session_gate_defaults_off() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert!(!p.engine.session_gate);
}

/// RULED (see `EngineCfg::session_gate`'s doc): step 1 found no by-name `SessionCalendar`
/// lookup, so this task's deliverable is `session_gate` alone — no `[engine.sessions]` table.
/// The brief's own third test was written as a decision point that passed either way; this
/// replaces it with the real assertion for the scope that was actually chosen: the gate parses
/// and validates cleanly with no calendar configured.
///
/// ⚠ This comment used to add "fail-permissive always-open at runtime", and that was FALSE as
/// a general claim — `crate::hist_replay::replay_ticks` sets `default_venue` unconditionally
/// and `bar_engine_params` sets it under `snap_to_properties`, so each symbol resolves through
/// `vike_model::session_for`, which hands back a real `FX_WEEK` calendar for the FX/CFD
/// venues. Arming the gate with no calendar table is a MODELLING choice, not a no-op. What
/// this test asserts is only what its name says: no calendar table is needed to LOAD it.
#[test]
fn session_gate_without_a_calendar_parses_and_validates_cleanly() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsession_gate = true");
    let p =
        BacktestProfile::from_toml_str(&toml).expect("no calendar table needed to arm the gate");
    assert!(p.engine.session_gate);
}
