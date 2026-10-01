use super::*;

fn parse(args: &[&str]) -> Result<BootstrapArgs, String> {
    parse_bootstrap_daemon(args.iter().map(|s| (*s).to_string()))
}

#[test]
fn the_minimal_shape_is_a_name_a_venue_an_asset_class_and_a_symbol() {
    let a =
        parse(&["boot", "--venue", "bybit", "--asset-class", "CryptoPerp", "--symbol", "BTCUSDT"])
            .unwrap();
    assert_eq!(a.name, "boot");
    assert_eq!(a.venue, "bybit");
    assert_eq!(a.asset_class, "CryptoPerp");
    assert_eq!(a.symbol.as_deref(), Some("BTCUSDT"));
    assert_eq!(a.token_id, None);
    assert!(!a.dry_run);
    assert!(!a.data_only);
}

#[test]
fn token_id_is_the_polymarket_spelling_and_is_mutually_exclusive_with_symbol() {
    let a = parse(&[
        "boot",
        "--venue",
        "polymarket",
        "--asset-class",
        "PredictionMarket",
        "--token-id",
        "12345",
    ])
    .unwrap();
    assert_eq!(a.token_id.as_deref(), Some("12345"));
    assert_eq!(a.symbol, None);

    let e = parse(&[
        "boot",
        "--venue",
        "polymarket",
        "--asset-class",
        "PredictionMarket",
        "--symbol",
        "x",
        "--token-id",
        "12345",
    ])
    .unwrap_err();
    assert!(e.contains("not both"), "{e}");

    let e = parse(&["boot", "--venue", "bybit", "--asset-class", "CryptoPerp"]).unwrap_err();
    assert!(e.contains("mount symbol is required"), "{e}");
}

#[test]
fn venue_and_asset_class_are_required_by_name() {
    let e = parse(&["boot", "--asset-class", "CryptoPerp", "--symbol", "x"]).unwrap_err();
    assert!(e.contains("--venue"), "{e}");
    let e = parse(&["boot", "--venue", "bybit", "--symbol", "x"]).unwrap_err();
    assert!(e.contains("--asset-class"), "{e}");
}

#[test]
fn the_optional_mount_knobs_all_parse() {
    let a = parse(&[
        "boot",
        "--venue",
        "bybit",
        "--asset-class",
        "CryptoPerp",
        "--symbol",
        "BTCUSDT",
        "--interval",
        "1m",
        "--qty",
        "0.001",
        "--half-spread",
        "0.0005",
        "--tick-size",
        "0.1",
        "--seed-cash",
        "1000",
        "--account",
        "sub1",
        "--data-only",
    ])
    .unwrap();
    assert_eq!(a.interval.as_deref(), Some("1m"));
    assert_eq!(a.qty, Some(0.001));
    assert_eq!(a.half_spread, Some(0.0005));
    assert_eq!(a.tick_size, Some(0.1));
    assert_eq!(a.seed_cash, Some(1000.0));
    assert_eq!(a.account.as_deref(), Some("sub1"));
    assert!(a.data_only);
}

#[test]
fn a_bad_number_is_a_named_refusal() {
    let e = parse(&[
        "boot",
        "--venue",
        "bybit",
        "--asset-class",
        "CryptoPerp",
        "--symbol",
        "x",
        "--qty",
        "not-a-number",
    ])
    .unwrap_err();
    assert!(e.contains("--qty"), "{e}");
    assert!(e.contains("not-a-number"), "{e}");
}

#[test]
fn help_travels_back_as_the_shared_sentinel() {
    assert!(parse(&["--help"]).is_err());
}

#[test]
fn a_run_with_no_settings_directory_refuses_rather_than_guessing_one() {
    let a = BootstrapArgs {
        name: "x".into(),
        venue: "bybit".into(),
        asset_class: "CryptoPerp".into(),
        symbol: Some("BTCUSDT".into()),
        token_id: None,
        interval: None,
        qty: None,
        half_spread: None,
        tick_size: None,
        seed_cash: None,
        account: None,
        data_only: false,
        dry_run: true,
    };
    let e = bootstrap(&a, None, 0).unwrap_err();
    assert!(e.contains("VIKE_SETTINGS_DIR"), "{e}");
}

#[test]
fn an_unknown_asset_class_is_refused_by_name() {
    // A self-deleting handle for the WHOLE scope (`crates/vike-ops/tests/journal_scratch_gate.rs`'s
    // `no_new_unguarded_temp_paths_outside_vike_core`): a bare `remove_dir_all` at the tail does
    // not run when an assertion above it panics, so the directory this test creates must be owned
    // by a `Drop` impl instead — `tempfile::TempDir` is exactly that.
    let dir = tempfile::tempdir().unwrap();
    let a = BootstrapArgs {
        name: "x".into(),
        venue: "bybit".into(),
        asset_class: "NotAWord".into(),
        symbol: Some("BTCUSDT".into()),
        token_id: None,
        interval: None,
        qty: None,
        half_spread: None,
        tick_size: None,
        seed_cash: None,
        account: None,
        data_only: false,
        dry_run: true,
    };
    let e = bootstrap(&a, Some(dir.path()), 0).unwrap_err();
    assert!(e.contains("NotAWord"), "{e}");
    assert!(e.contains("CryptoPerp"), "{e}");
}
