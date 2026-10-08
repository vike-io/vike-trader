//! The `[engine.resolution]` settlement source and the opt-in `[risk]` section.

use super::*;

// --- G6: the settlement source ------------------------------------------------------------

#[test]
fn resolution_is_absent_unless_configured() {
    assert!(BacktestProfile::from_toml_str(BAR_TOML).unwrap().engine.resolution.is_none());
}

/// The source pays 1.0 to the winning outcome and 0.0 to the loser, and ONLY from the
/// window close onward — a spot symbol or another window's token is never touched.
#[test]
fn binary_outcome_source_pays_the_winner_from_the_close() {
    let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
    let symbols: Vec<String> = p.data.resolved_series().into_iter().map(|s| s.symbol).collect();
    let (src, end_ts) = p.engine.resolution.as_ref().unwrap().build(None, &symbols).unwrap();

    let res_ms = (1_775_001_600 + 300) * 1000;
    assert_eq!(end_ts, Some(res_ms), "the sweep probes at THIS window's close, not a sentinel");

    // still trading
    assert_eq!(src("btc-updown-5m-1775001600#0", res_ms - 1), None);
    // resolved: outcome 0 won
    assert_eq!(src("btc-updown-5m-1775001600#0", res_ms), Some(1.0));
    assert_eq!(src("btc-updown-5m-1775001600#1", res_ms), Some(0.0));
    // the reference series is never settled or latched (port backlog G5)
    assert_eq!(src("BTCUSDT", res_ms), None);
    // a window with no winner row is never invented
    assert_eq!(src("btc-updown-5m-1775001900#0", i64::MAX / 4), None);
}

/// A non-binary INLINE `winning_index` is an authoring mistake, not a data fact — reject it
/// rather than coerce it to "both sides lose".
#[test]
fn a_non_binary_inline_winning_index_is_rejected() {
    for bad in ["2", "-1", "4"] {
        let toml = CROSS_VENUE_TOML.replace(
            "\"btc-updown-5m-1775001600\" = 0",
            &format!("\"btc-updown-5m-1775001600\" = {bad}"),
        );
        refused_at_load(&toml, HarnessError::Validation, &["not a binary outcome index"]);
    }
}

/// A window token in the slice with no payout would end the run marked at its last traded
/// price. That must be loud.
#[test]
fn a_window_with_no_resolution_row_is_rejected_at_build() {
    let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
    let symbols = vec![
        "BTCUSDT".to_string(),
        "btc-updown-5m-1775001600#0".to_string(),
        "btc-updown-5m-1775001900#0".to_string(), // no winner row
    ];
    // `ResolutionSource` is a boxed closure and therefore not `Debug`, so match on the
    // Result rather than `unwrap_err`.
    match p.engine.resolution.as_ref().unwrap().build(None, &symbols) {
        Err(HarnessError::Validation(m)) => {
            assert!(m.contains("no binary winning_index"), "{m}");
            assert!(m.contains("btc-updown-5m-1775001900"), "{m}");
        }
        Err(other) => panic!("expected a validation error, got {other:?}"),
        Ok(_) => panic!("expected a validation error, got Ok"),
    }
}

/// The `slug,winning_index` CSV the ClickHouse export produces: header row, quoted slugs,
/// and non-binary rows that are DROPPED (a real on-chain fact) rather than coerced.
#[test]
fn resolution_reads_the_clickhouse_csv_and_drops_non_binary_rows() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("res.csv"),
        "\"slug\",\"winning_index\"\n\
             \"btc-updown-5m-1775001600\",1\n\
             \"btc-updown-5m-1775001900\",4\n",
    )
    .unwrap();

    let toml = CROSS_VENUE_TOML.replace(
        "[engine.resolution.winners]\n\"btc-updown-5m-1775001600\" = 0",
        "path = \"res.csv\"",
    );
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
    let (src, _) = p.engine.resolution.as_ref().unwrap().build(Some(dir.path()), &symbols).unwrap();
    // The slug is UNQUOTED before use — a quoted key matches no token symbol at all.
    assert_eq!(src("btc-updown-5m-1775001600#0", i64::MAX / 4), Some(0.0));
    assert_eq!(src("btc-updown-5m-1775001600#1", i64::MAX / 4), Some(1.0));
    // The `winning_index = 4` row was dropped, so that window has no payout...
    assert_eq!(src("btc-updown-5m-1775001900#0", i64::MAX / 4), None);
    // ...and asking to RUN it is an error, not a silent unsettled position.
    assert!(
        p.engine
            .resolution
            .as_ref()
            .unwrap()
            .build(Some(dir.path()), &["btc-updown-5m-1775001900#0".to_string()])
            .is_err()
    );
}

/// A relative sidecar path resolves next to the PROFILE, not against the CWD.
#[test]
fn a_relative_resolution_path_resolves_against_the_profile_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("res.csv"), "btc-updown-5m-1775001600,0\n").unwrap();
    let toml = CROSS_VENUE_TOML.replace(
        "[engine.resolution.winners]\n\"btc-updown-5m-1775001600\" = 0",
        "path = \"res.csv\"",
    );
    let profile_path = dir.path().join("run.toml");
    std::fs::write(&profile_path, &toml).unwrap();

    let p = BacktestProfile::from_path(&profile_path).unwrap();
    assert_eq!(p.base_dir.as_deref(), Some(dir.path()));
    let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
    assert!(p.engine.resolution.as_ref().unwrap().build(p.base_dir.as_deref(), &symbols).is_ok());
}

#[test]
fn an_unknown_resolution_kind_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("\"binary_outcome\"", "\"binary\"");
    refused_at_load(&toml, HarnessError::Validation, &["unknown engine.resolution.kind"]);
}

/// An explicit `end_ts` overrides the derived "latest window close in the slice".
#[test]
fn an_explicit_resolution_end_ts_overrides_the_derived_one() {
    let toml = CROSS_VENUE_TOML
        .replace("kind = \"binary_outcome\"", "kind = \"binary_outcome\"\nend_ts = \"12345\"");
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
    let (_, end) = p.engine.resolution.as_ref().unwrap().build(None, &symbols).unwrap();
    assert_eq!(end, Some(12_345));
}

// --- the opt-in `[risk]` section (runprofile-wiring-step2) --------------------------------

/// Absent `[risk]` must parse to `None` — the byte-identical-default claim: nothing in
/// `BAR_TOML`/`TICK_TOML` sets it, so every profile written before this field existed keeps
/// parsing exactly as before.
#[test]
fn risk_is_absent_unless_configured() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
    assert!(p.risk.is_none());
    let p = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
    assert!(p.risk.is_none());
}

/// A `[risk]` section parses into the SAME `vike_exec::ProfileRisk` fields paper/live use, and
/// `ProfileRisk::to_risk_limits` maps them onto the real `RiskLimits` the engine reads.
#[test]
fn risk_section_parses_into_profile_risk() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[risk]\nmax_notional_per_order = 250000.0\nmax_leverage = 2.0\n\
             min_qty = 0.001",
    );
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    let risk = p.risk.as_ref().expect("`[risk]` configured");
    assert_eq!(risk.max_notional_per_order, Some(250_000.0));
    assert_eq!(risk.max_leverage, Some(2.0));
    assert_eq!(risk.min_qty, Some(0.001));

    let limits = risk.to_risk_limits();
    assert_eq!(limits.max_notional_per_order, Some(250_000.0));
    assert_eq!(limits.max_leverage, Some(2.0));
    // A backtest `[risk] max_leverage` arms the SAME buying-power check paper/live arm
    // (issue #822): 2x ⇒ 50% initial margin. `SimBroker::build_risk_gate` already maps its own
    // `EngineParams::leverage` this way, so the two edges now agree on what "2x" means.
    assert_eq!(limits.im_requirement, Some(0.5));
    assert_eq!(limits.min_qty, Some(0.001));
}

/// A typo'd `[risk]` key must fail the profile — `ProfileRisk` carries its own
/// `deny_unknown_fields`, so the nested-denial policy applies inside `[risk]` too, exactly as
/// it does for `vike-core`'s `RunProfile`.
#[test]
fn unknown_risk_key_is_rejected() {
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[risk]\nmax_levarage = 10.0");
    refused_at_load(&toml, HarnessError::Parse, &[]);
}

/// `risk.max_orders_per_window` is a wall-clock throttle the sim gate always disarms
/// (`SimBroker::build_risk_gate`) — REJECTED at load rather than silently ignored, the
/// documented divergence this wiring step must surface loudly instead of papering over.
#[test]
fn risk_max_orders_per_window_is_rejected_at_load() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[risk]\nmax_orders_per_window = 5\nwindow_ms = 1000",
    );
    refused_at_load(&toml, HarnessError::Validation, &["max_orders_per_window", "wall-clock"]);
}

/// The rejection fires from the TICK lane too — the throttle is meaningless in sim time
/// regardless of bar/tick mode.
#[test]
fn risk_max_orders_per_window_is_rejected_on_the_tick_lane_too() {
    let toml = TICK_TOML.replace(
        "snap_to_properties = true",
        "snap_to_properties = true\n\n[risk]\nmax_orders_per_window = 1\nwindow_ms = 1000",
    );
    refused_at_load(&toml, HarnessError::Validation, &[]);
}

/// Every OTHER `risk.*` limit is unaffected by the `max_orders_per_window` gate — a profile
/// setting only operator-budget fields (no throttle) parses and validates cleanly.
#[test]
fn risk_without_max_orders_per_window_is_accepted() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[risk]\nmax_notional_per_order = 1000.0\nmax_total_exposure = 5000.0",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("no throttle set -> accepted");
    assert_eq!(p.risk.unwrap().max_notional_per_order, Some(1000.0));
}
