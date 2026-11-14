use super::*;

fn parse(args: &[&str]) -> Result<BootstrapArgs, args::Stop> {
    parse_bootstrap_daemon(args.iter().map(|s| (*s).to_string()))
}

#[test]
fn the_minimal_shape_is_a_name_a_venue_an_asset_class_a_symbol_and_a_strategy_file() {
    let a = parse(&[
        "boot",
        "--venue",
        "bybit",
        "--asset-class",
        "CryptoPerp",
        "--symbol",
        "BTCUSDT",
        "--strategy-file",
        "trend/momentum.rs",
    ])
    .unwrap();
    assert_eq!(a.name, "boot");
    assert_eq!(a.venue, "bybit");
    assert_eq!(a.asset_class, "CryptoPerp");
    assert_eq!(a.symbol.as_deref(), Some("BTCUSDT"));
    assert_eq!(a.token_id, None);
    assert_eq!(a.strategy_file, "trend/momentum.rs");
    assert!(!a.dry_run);
    assert!(!a.data_only);
}

/// **No default strategy exists, so a profile without its file is refused at the write**, naming the
/// flag, rather than stored for the daemon to refuse at its start.
#[test]
fn a_mount_without_a_strategy_file_is_refused_naming_the_flag() {
    let e = parse(&["boot", "--venue", "bybit", "--asset-class", "CryptoPerp", "--symbol", "x"])
        .unwrap_err();
    assert!(e.contains("--strategy-file"), "{e}");
    assert!(e.contains("REQUIRED"), "{e}");
    assert!(e.contains("strategies/"), "names the folder: {e}");
}

/// **The file reaches the stored row and the rendered `[strategy] file`**, the form the daemon's own
/// loader parses; the maker knobs land in that file's `[strategy.params]`.
#[test]
fn the_strategy_file_lands_in_the_stored_row_and_its_rendering() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    vike_secrets::create_empty_store_for_test(&db).unwrap();
    let a = parse(&[
        "boot",
        "--venue",
        "bybit",
        "--asset-class",
        "CryptoPerp",
        "--symbol",
        "BTCUSDT",
        "--strategy-file",
        "market_making/spread_maker.rs",
        "--qty",
        "0.001",
    ])
    .unwrap();
    bootstrap(&a, Some(dir.path()), 1).unwrap();
    let profiles = read_profiles(&db).unwrap();
    let stored = profiles.active(ProfileKind::Daemon).unwrap();
    assert_eq!(stored.mounts[0].strategy_file.as_deref(), Some("market_making/spread_maker.rs"));
    let toml = vike_secrets::profile_store::render_daemon_toml(stored).unwrap();
    assert!(toml.contains("file = \"market_making/spread_maker.rs\""), "{toml}");
    assert!(toml.contains("qty = 0.001"), "{toml}");
    assert!(!toml.contains("name = "), "no strategy is named: {toml}");
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
        "--strategy-file",
        "nothing.rhai",
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
        "--strategy-file",
        "market_making/spread_maker.rs",
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
        interval_ms: None,
        summary_ms: None,
        shutdown_deadline_ms: None,
        strategy_file: "nothing.rhai".into(),
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
    // A self-deleting handle for the WHOLE scope (`crates/vike-ops/tests/hygiene/journal_scratch_gate/tree_rule.rs`'s
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
        interval_ms: None,
        summary_ms: None,
        shutdown_deadline_ms: None,
        strategy_file: "nothing.rhai".into(),
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

/// The arguments every daemon-knob test below starts from: the OANDA recipe's mount, whose
/// `[daemon]` table is the reason these flags exist.
const OANDA_BASE: [&str; 11] = [
    "oanda-live",
    "--strategy-file",
    "nothing.rhai",
    "--venue",
    "oanda",
    "--asset-class",
    "Fx",
    "--symbol",
    "EURUSD",
    "--interval",
    "1m",
];

fn oanda_with(extra: &[&str]) -> Result<BootstrapArgs, args::Stop> {
    let mut argv: Vec<&str> = OANDA_BASE.to_vec();
    argv.extend_from_slice(extra);
    parse(&argv)
}

/// **The recipe's `[daemon]` table and `interval_ms` reach the stored rows.** `docs/ops`'s OANDA
/// recipe needs `shutdown_deadline_ms = 10000` (the quote reader observes its stop flag only
/// between the venue's ~5 s heartbeats), and until these flags existed the verb could not carry it,
/// so a row built by the one-line command silently kept the 5000 ms default. The assertions read
/// the rows back out of a real database and render them through the daemon-profile renderer, which
/// is the form the daemon's own loader parses.
#[test]
fn the_daemon_knobs_and_the_bar_width_land_in_the_stored_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    vike_secrets::create_empty_store_for_test(&db).unwrap();

    let a = oanda_with(&[
        "--interval-ms",
        "60000",
        "--summary-ms",
        "5000",
        "--shutdown-deadline-ms",
        "10000",
    ])
    .expect("the three knobs are accepted");
    bootstrap(&a, Some(dir.path()), 1).unwrap();

    let profiles = read_profiles(&db).unwrap();
    let stored = profiles.active(ProfileKind::Daemon).expect("the verb activates what it stores");
    assert_eq!(stored.mounts[0].interval_ms, Some(60_000));
    assert_eq!(stored.settings.get("daemon.summary_ms").map(String::as_str), Some("5000"));
    assert_eq!(
        stored.settings.get("daemon.shutdown_deadline_ms").map(String::as_str),
        Some("10000")
    );
    let toml = vike_secrets::profile_store::render_daemon_toml(stored).unwrap();
    assert!(toml.contains("interval_ms = 60000"), "{toml}");
    assert!(toml.contains("shutdown_deadline_ms = 10000"), "{toml}");
}

/// **A knob left off stores nothing for it.** The daemon's own defaults (5000 ms each) must stay
/// the daemon's — a stored default would freeze today's number onto the row, the same hazard
/// `data_only` is written as `Some(true)` or absent to avoid.
#[test]
fn a_knob_left_off_stores_no_row_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    vike_secrets::create_empty_store_for_test(&db).unwrap();

    let a = oanda_with(&["--shutdown-deadline-ms", "10000"]).expect("one knob alone is accepted");
    bootstrap(&a, Some(dir.path()), 1).unwrap();

    let profiles = read_profiles(&db).unwrap();
    let stored = profiles.active(ProfileKind::Daemon).unwrap();
    assert_eq!(stored.mounts[0].interval_ms, None);
    assert_eq!(stored.settings.get("daemon.summary_ms"), None);
    assert_eq!(stored.settings.len(), 1, "{:?}", stored.settings);
}

/// **A bad number is a named refusal, and nothing is written.** The daemon's loader takes any
/// `u64`, so these are the verb's own sanity checks: a zero deadline would time out a teardown that
/// never got to run, a zero cadence is not a cadence, and a bar width of zero or below is no bar.
#[test]
fn a_zero_negative_fractional_or_non_numeric_knob_is_refused_by_name() {
    for flag in ["--interval-ms", "--summary-ms", "--shutdown-deadline-ms"] {
        for bad in ["0", "-5", "1.5", "soon"] {
            let e = oanda_with(&[flag, bad]).unwrap_err();
            assert!(e.contains(flag), "{flag} {bad}: {e}");
            assert!(e.contains(&format!("'{bad}'")), "{flag} {bad}: {e}");
            assert!(e.contains("whole number of milliseconds"), "{flag} {bad}: {e}");
        }
    }
}

/// **`--interval-ms` must agree with `--interval` whenever the interval is a spelling vike can
/// measure.** The recipes say "`interval_ms` must agree with `interval`", and the mount takes the
/// two independently, so a disagreement is a bar window that is not the series it is labelled as.
/// An interval outside `vike_marketdata::interval::interval_ms`'s vocabulary cannot be checked and is not
/// refused.
#[test]
fn the_bar_width_must_agree_with_an_interval_vike_can_measure() {
    let swap = |interval: &str, ms: &str| {
        let mut argv: Vec<&str> = OANDA_BASE[..10].to_vec();
        argv.extend_from_slice(&[interval, "--interval-ms", ms]);
        parse(&argv)
    };
    let e = swap("5m", "60000").unwrap_err();
    assert!(e.contains("--interval-ms 60000"), "{e}");
    assert!(e.contains("5m"), "{e}");
    assert!(e.contains("300000"), "{e}");
    assert!(swap("5m", "300000").is_ok(), "a width that agrees is accepted");
    assert!(swap("1w", "604800000").is_ok(), "an interval outside the vocabulary is not judged");
}

/// **`--symbol` is stored in the control door's spelling, or refused in its words** (the C-D4 follow-up, as
/// `secrets account set-symbol` stores an account's): `ethusdt.s` on bybit used to be stored as typed, a perpetual
/// named `ethusdt.s` to bybit's exec side, and is stored `ETHUSDT.S`; a name the grammar refuses writes nothing.
#[test]
fn the_symbol_is_stored_in_the_doors_spelling_or_refused() {
    let dir = tempfile::tempdir().unwrap();
    let db = vike_secrets::db_path_in(dir.path());
    vike_secrets::create_empty_store_for_test(&db).unwrap();
    let typed = |symbol: &str| {
        parse(&[
            "boot",
            "--venue",
            "bybit",
            "--asset-class",
            "CryptoSpot",
            "--symbol",
            symbol,
            "--strategy-file",
            "nothing.rhai",
        ])
        .unwrap()
    };
    bootstrap(&typed("ethusdt.s"), Some(dir.path()), 1).unwrap();
    let profiles = read_profiles(&db).unwrap();
    let stored = profiles.active(ProfileKind::Daemon).unwrap();
    assert_eq!(stored.mounts[0].symbol.as_deref(), Some("ETHUSDT.S"));
    let why = bootstrap(&typed("BTC-31OCT26-75500-C-USDT"), Some(dir.path()), 2).unwrap_err();
    assert!(why.contains("is a bybit option"), "{why}");
}
