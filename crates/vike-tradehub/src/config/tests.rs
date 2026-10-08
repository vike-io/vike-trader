use super::*;

#[cfg(test)]
mod data_only;
#[cfg(test)]
mod paper_risk;
#[cfg(test)]
mod rhai;
#[cfg(test)]
mod strategy;
#[cfg(test)]
mod strategy_params;
#[cfg(test)]
mod wired_set_sync;

#[test]
fn minimal_profile_uses_polymarket_defaults() {
    let p = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("minimal profile parses");
    assert_eq!(p.venue(), "polymarket");
    assert_eq!(p.mount_symbol(), "TOK");
    assert!(p.strategy.is_none(), "no [strategy] table ⇒ the historical A-S maker");
    assert_eq!(p.resolution_ts_ms, None);
    assert_eq!(p.daemon.summary_ms, 5_000);
    assert_eq!(p.daemon.shutdown_deadline_ms, 5_000);

    // The MakerMountConfig::outcome_token recommended defaults flow through untouched.
    let cfg = p.to_mount_config();
    assert_eq!(cfg.venue, "polymarket");
    assert_eq!(cfg.token_id, "TOK");
    assert_eq!(cfg.interval, "1m");
    assert_eq!(cfg.interval_ms, 60_000);
    assert_eq!(cfg.qty.to_bits(), 20.0_f64.to_bits());
    assert_eq!(cfg.tick_size.to_bits(), 0.01_f64.to_bits());
    assert_eq!(cfg.seed_cash.to_bits(), 1_000.0_f64.to_bits());
    assert_eq!(cfg.as_params.resolution_ts, None);
}

#[test]
fn overrides_apply_to_the_mount_config() {
    let toml = r#"
venue = "polymarket"
token_id = "OUTCOME"
resolution_ts_ms = 1793491200000
interval = "5m"
interval_ms = 300000
qty = 50.0
half_spread = 0.02
tick_size = 0.01
seed_cash = 250.0

[daemon]
summary_ms = 2000
shutdown_deadline_ms = 3000
"#;
    let p = DaemonProfile::from_toml_str(toml).expect("full profile parses");
    assert_eq!(p.daemon.summary_ms, 2_000);
    assert_eq!(p.summary_interval(), Duration::from_millis(2_000));
    assert_eq!(p.shutdown_deadline(), Duration::from_millis(3_000));

    let cfg = p.to_mount_config();
    assert_eq!(cfg.token_id, "OUTCOME");
    assert_eq!(cfg.as_params.resolution_ts, Some(1_793_491_200_000));
    assert_eq!(cfg.interval, "5m");
    assert_eq!(cfg.interval_ms, 300_000);
    assert_eq!(cfg.qty.to_bits(), 50.0_f64.to_bits());
    assert_eq!(cfg.half_spread.to_bits(), 0.02_f64.to_bits());
    assert_eq!(cfg.seed_cash.to_bits(), 250.0_f64.to_bits());
}

#[test]
fn empty_symbol_is_rejected_under_either_spelling() {
    for toml in ["token_id = \"\"", "symbol = \"\""] {
        let err = DaemonProfile::from_toml_str(toml).unwrap_err();
        assert!(err.contains("symbol"), "error must name the symbol: {err}");
    }
}

#[test]
fn a_profile_with_no_symbol_at_all_is_rejected() {
    let err = DaemonProfile::from_toml_str("venue = \"polymarket\"").unwrap_err();
    assert!(err.contains("symbol"), "names what is missing: {err}");
}

#[test]
fn setting_both_symbol_spellings_is_rejected() {
    // Two spellings of one field with different values has no defensible winner, and silently
    // picking one is how a live mount ends up on the instrument nobody typed.
    let err = DaemonProfile::from_toml_str("symbol = \"BTC\"\ntoken_id = \"TOK\"").unwrap_err();
    assert!(err.contains("not both"), "names the conflict: {err}");
}

/// BACK-COMPAT, the property the CI box's running paper daemon depends on: the SHIPPED profile shape
/// — `token_id` with no `symbol` key and no `[strategy]` table — still parses and still lowers
/// to the identical `MakerMountConfig`. Both example profiles in this repo are that shape.
#[test]
fn the_shipped_token_id_profile_shape_is_unchanged() {
    let shipped = r#"
venue = "polymarket"
token_id = "71321045679252212594626385532706912750332728571942532289631379312455583992563"
interval = "1m"
interval_ms = 60000
qty = 20.0
half_spread = 0.01
tick_size = 0.01
seed_cash = 1000.0

[daemon]
summary_ms = 5000
shutdown_deadline_ms = 5000
"#;
    let p = DaemonProfile::from_toml_str(shipped).expect("the shipped profile shape parses");
    assert!(p.strategy.is_none(), "no [strategy] ⇒ the A-S maker, as before");
    let cfg = p.to_mount_config();
    assert_eq!(cfg.venue, "polymarket");
    assert_eq!(
        cfg.token_id,
        "71321045679252212594626385532706912750332728571942532289631379312455583992563"
    );
    assert_eq!(cfg.qty.to_bits(), 20.0_f64.to_bits());
    assert_eq!(cfg.seed_cash.to_bits(), 1_000.0_f64.to_bits());
    // ...and it is still live-armable IN A BUILD THAT CAN MOUNT IT, which is the half a
    // venue-based gate could have broken.
    //
    // ⚠ The feature condition is a deliberate TIGHTENING, not a regression. `validate_for_live`
    // used to be feature-free and accepted a polymarket profile even in a build with no
    // polymarket `live_mount` arm; the daemon then hard-errored a few frames later, INSIDE the
    // mount. Refusing it here means the same outcome (a loud startup failure, never a silent
    // paper fallback) reported before anything is built, by the gate whose job it is.
    assert_eq!(
        p.validate_for_live().is_ok(),
        cfg!(feature = "polymarket"),
        "the shipped live profile shape stays armable wherever it can actually be mounted"
    );
}

/// The tightening above, stated as its own claim so it cannot be read as an accident: in a build
/// that CANNOT mount polymarket, the refusal names the venue rather than the token shape.
#[cfg(not(feature = "polymarket"))]
#[test]
fn a_default_build_refuses_polymarket_by_venue_not_by_token_shape() {
    let p = DaemonProfile::from_toml_str(
            "venue = \"polymarket\"\ntoken_id = \"71321045679252212594626385532706912750332728571942532289631379312455583992563\"",
        )
        .expect("parses");
    let err = p.validate_for_live().unwrap_err();
    assert!(err.contains("not live-wired in this build"), "names the real reason: {err}");
}

#[test]
fn symbol_is_the_general_spelling_of_token_id() {
    let by_symbol =
        DaemonProfile::from_toml_str("venue = \"hyperliquid\"\nsymbol = \"BTC\"").unwrap();
    let by_token =
        DaemonProfile::from_toml_str("venue = \"hyperliquid\"\ntoken_id = \"BTC\"").unwrap();
    assert_eq!(by_symbol.mount_symbol(), by_token.mount_symbol());
    assert_eq!(by_symbol.to_mount_config().token_id, by_token.to_mount_config().token_id);
}

#[test]
fn unknown_field_is_rejected() {
    // deny_unknown_fields catches a typo'd key rather than silently ignoring it.
    let err = DaemonProfile::from_toml_str("token_id = \"TOK\"\nbogus = 1").unwrap_err();
    assert!(!err.is_empty());
}

#[test]
fn partial_daemon_table_keeps_the_other_default() {
    // Only one of the two daemon knobs set — the other must keep its default.
    let p = DaemonProfile::from_toml_str("token_id = \"TOK\"\n[daemon]\nsummary_ms = 1000")
        .expect("partial daemon table parses");
    assert_eq!(p.daemon.summary_ms, 1_000);
    assert_eq!(p.daemon.shutdown_deadline_ms, 5_000);
}

fn strategy_profile(name: &str) -> Result<DaemonProfile, String> {
    DaemonProfile::from_toml_str(&format!(
        "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n"
    ))
}
