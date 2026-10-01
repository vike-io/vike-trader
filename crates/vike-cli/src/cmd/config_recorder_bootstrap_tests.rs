use super::*;

fn parse(args: &[&str]) -> Result<BootstrapArgs, String> {
    parse_bootstrap_recorder(args.iter().map(|s| (*s).to_string()))
}

#[test]
fn the_minimal_shape_is_a_name_a_store_a_venue_and_a_family() {
    let a =
        parse(&["boot", "--store", "/data/hist", "--venue", "binance", "--family", "*USDT-PERP"])
            .unwrap();
    assert_eq!(a.name, "boot");
    assert_eq!(a.store, "/data/hist");
    assert_eq!(a.venue, "binance");
    assert_eq!(a.family.as_deref(), Some("*USDT-PERP"));
    assert!(a.symbols.is_empty());
    assert!(!a.dry_run);
}

#[test]
fn symbols_are_comma_split_and_mutually_exclusive_with_family() {
    let a = parse(&[
        "boot",
        "--store",
        "/data/hist",
        "--venue",
        "polymarket",
        "--symbols",
        "BTC-5m, ETH-5m",
    ])
    .unwrap();
    assert_eq!(a.symbols, vec!["BTC-5m".to_string(), "ETH-5m".to_string()]);
    assert_eq!(a.family, None);

    let e =
        parse(&["boot", "--store", "s", "--venue", "binance", "--family", "f", "--symbols", "x"])
            .unwrap_err();
    assert!(e.contains("not both"), "{e}");

    let e = parse(&["boot", "--store", "s", "--venue", "binance"]).unwrap_err();
    assert!(e.contains("--family"), "{e}");
}

#[test]
fn store_and_venue_are_required_by_name() {
    let e = parse(&["boot", "--venue", "binance", "--family", "f"]).unwrap_err();
    assert!(e.contains("--store"), "{e}");
    let e = parse(&["boot", "--store", "s", "--family", "f"]).unwrap_err();
    assert!(e.contains("--venue"), "{e}");
}

#[test]
fn an_unknown_backfill_word_is_refused_by_name() {
    let e = parse(&[
        "boot",
        "--store",
        "s",
        "--venue",
        "binance",
        "--family",
        "f",
        "--backfill",
        "sideways",
    ])
    .unwrap_err();
    assert!(e.contains("sideways"), "{e}");
    assert!(e.contains("archive"), "{e}");
}

#[test]
fn the_optional_knobs_all_parse() {
    let a = parse(&[
        "boot",
        "--store",
        "s",
        "--venue",
        "binance",
        "--family",
        "f",
        "--backfill",
        "archive",
        "--interval-secs",
        "300",
        "--min-parts",
        "4",
        "--target-mb",
        "384",
        "--max-merge-rows",
        "1000000",
        "--retention-days",
        "30",
        "--webhooks",
        "telegram, pager",
        "--alert-repeat-secs",
        "600",
        "--alert-series-prefix",
        "prod",
        "--note",
        "bootstrap",
    ])
    .unwrap();
    assert_eq!(a.backfill.as_deref(), Some("archive"));
    assert_eq!(a.interval_secs, Some(300));
    assert_eq!(a.min_parts, Some(4));
    assert_eq!(a.target_mb, Some(384));
    assert_eq!(a.max_merge_rows, Some(1_000_000));
    assert_eq!(a.retention_days, Some(30));
    assert_eq!(a.webhooks, vec!["telegram".to_string(), "pager".to_string()]);
    assert_eq!(a.alert_repeat_secs, Some(600));
    assert_eq!(a.alert_series_prefix.as_deref(), Some("prod"));
    assert_eq!(a.note.as_deref(), Some("bootstrap"));
}

#[test]
fn a_bad_number_is_a_named_refusal() {
    let e = parse(&[
        "boot",
        "--store",
        "s",
        "--venue",
        "binance",
        "--family",
        "f",
        "--interval-secs",
        "not-a-number",
    ])
    .unwrap_err();
    assert!(e.contains("--interval-secs"), "{e}");
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
        store: "s".into(),
        venue: "binance".into(),
        family: Some("f".into()),
        symbols: Vec::new(),
        backfill: None,
        interval_secs: None,
        min_parts: None,
        target_mb: None,
        max_merge_rows: None,
        retention_days: None,
        webhooks: Vec::new(),
        alert_repeat_secs: None,
        alert_series_prefix: None,
        note: None,
        dry_run: true,
    };
    let e = bootstrap(&a, None, 0).unwrap_err();
    assert!(e.contains("VIKE_SETTINGS_DIR"), "{e}");
}

#[test]
fn a_dry_run_against_a_fresh_store_reports_the_crossing_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path())).unwrap();
    let a = BootstrapArgs {
        name: "default".into(),
        store: "/data/hist".into(),
        venue: "binance".into(),
        family: Some("*USDT-PERP".into()),
        symbols: Vec::new(),
        backfill: None,
        interval_secs: None,
        min_parts: None,
        target_mb: None,
        max_merge_rows: None,
        retention_days: None,
        webhooks: Vec::new(),
        alert_repeat_secs: None,
        alert_series_prefix: None,
        note: None,
        dry_run: true,
    };
    let report = bootstrap(&a, Some(dir.path()), 0).unwrap();
    assert!(report.contains("NOTHING WAS WRITTEN"), "{report}");
    assert!(report.contains("CROSSING"), "{report}");
    assert!(already_active_recorder(&vike_secrets::db_path_in(dir.path())).is_none());
}

#[test]
fn a_real_run_stores_and_activates_and_a_second_run_reports_the_repoint() {
    let dir = tempfile::tempdir().unwrap();
    vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path())).unwrap();
    let db = vike_secrets::db_path_in(dir.path());

    let mut a = BootstrapArgs {
        name: "default".into(),
        store: "/data/hist".into(),
        venue: "binance".into(),
        family: Some("*USDT-PERP".into()),
        symbols: Vec::new(),
        backfill: None,
        interval_secs: None,
        min_parts: None,
        target_mb: None,
        max_merge_rows: None,
        retention_days: None,
        webhooks: Vec::new(),
        alert_repeat_secs: None,
        alert_series_prefix: None,
        note: None,
        dry_run: false,
    };
    let report = bootstrap(&a, Some(dir.path()), 1).unwrap();
    assert!(report.contains("ACTIVATED"), "{report}");
    assert_eq!(already_active_recorder(&db).as_deref(), Some("default"));

    a.name = "second".into();
    let report = bootstrap(&a, Some(dir.path()), 2).unwrap();
    assert!(report.contains("REPOINTED"), "{report}");
    assert_eq!(already_active_recorder(&db).as_deref(), Some("second"));
}
