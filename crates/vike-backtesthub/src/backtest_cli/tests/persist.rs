use super::super::persist::{data_witness, search_witness};
use super::*;
use vike_model::OrderDrop;
use vike_model::StoreKindId;

/// A minimal profile that parses and validates. Kept close to the fixtures
/// `crates/vike-backtest/src/run/profile.rs` already carries.
const PROFILE_TOML: &str = r#"
name = "sma cross"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-01T00"
to = "2026-02-01T00"

[engine]
cash = 100000.0
# engine

[strategy]
file = "trend/sma_cross.rs"
"#;

fn a_result() -> vike_analytics::TradingRecord {
    vike_analytics::TradingRecord {
        trades: vec![vike_model::Trade {
            entry_price: 100.0,
            exit_price: 110.0,
            size: 1.0,
            pnl: 10.0,
            fees: 0.2,
            entry_timestamp: 1_756_000_000_000,
            exit_timestamp: 1_756_000_060_000,
            symbol: "BTCUSDT".to_string(),
            mae: -1.0,
            mfe: 12.0,
            is_long: true,
        }],
        equity_curve: vec![100_000.0, 100_010.0, 100_009.8],
        final_equity: 100_009.8,
        n_trades: 1,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), 10.0)],
        per_symbol_curves: vec![("BTCUSDT".to_string(), vec![0.0, 10.0, 9.8])],
        equity_timestamp: vec![1_756_000_000_000, 1_756_000_060_000, 1_756_000_120_000],
        funding_paid: -1.5,
        fees_paid: 0.2,
        settle_price: None,
        data_warnings: Vec::new(),
        closed_trades: None,
        forecasts: Vec::new(),
        sampling: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: vike_analytics::TradingRecordSource::Backtest(
            vike_analytics::TradingRecordSimulator {
                intrabar_both_hit: 2,
                stale_deferrals: 3,
                impact_unpriced: 4,
                session_deferrals: 5,
                dropped: vec![OrderDrop {
                    symbol: "BTCUSDT".to_string(),
                    reason: "volume_cap".to_string(),
                    size: 2.0,
                    weight: 1.0,
                }],
                dropped_at: vec![1_700_000_000_000],
                below_min_reversals: 6,
                warmup: 20,
                maker_fills: 0,
                taker_fills: 2,
            },
        ),
    }
}

fn a_report(
    profile: &ProfileBacktest,
    result: &vike_analytics::TradingRecord,
) -> vike_analytics::results::TradingResults {
    vike_analytics::results::TradingResults::from_record(
        profile.name.clone(),
        result,
        vike_backtest::run::report::periods_per_year(profile),
    )
}

/// The facts every persisting test starts from: the datahub provenance, no data fingerprint, no
/// input address, no build stamp, one fixed start and finish. A test that needs another value
/// for one field says so by struct update over this.
fn run_facts<'a>(profile: &'a ProfileBacktest, runs_root: &'a Path) -> BacktestRunInputs<'a> {
    BacktestRunInputs {
        profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root,
        data: None,
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    }
}

/// ⚠ **§13a, as a property.** Everything the run computed and the report could not hold comes
/// back off disk: the curve WITH its timestamps, the ledger, the per-symbol curves and every
/// diagnostic counter. Before this, `result` was dropped when `run` returned and nothing saved
/// could be re-tearsheeted, bootstrapped or compared curve-to-curve.
#[test]
fn a_persisted_run_keeps_the_curve_the_ledger_and_every_diagnostic_counter() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = run_facts(&profile, &runs_root);

    let dir = persist_run(&facts, &result, &report).unwrap();

    let series = vike_runs::read_series(&dir).unwrap();
    assert!(series.is_aligned(), "a persisted series must satisfy its own invariant");
    assert_eq!(series.equity, result.equity_curve);
    assert_eq!(series.equity_timestamp, result.equity_timestamp);
    assert_eq!(series.stride, 1, "a three-sample curve is under every cap");
    assert_eq!(series.source_len, 3);
    assert_eq!(series.per_symbol_equity, result.per_symbol_curves);
    assert_eq!(series.diagnostics.warmup, 20);
    assert_eq!(series.diagnostics.intrabar_both_hit, 2);
    assert_eq!(series.diagnostics.stale_deferrals, 3);
    assert_eq!(series.diagnostics.impact_unpriced, 4);
    assert_eq!(series.diagnostics.session_deferrals, 5);
    assert_eq!(series.diagnostics.below_min_reversals, 6);
    assert_eq!(series.diagnostics.dropped.len(), 1);
    assert_eq!(series.diagnostics.dropped[0].reason, "volume_cap");
    assert_eq!(series.diagnostics.dropped[0].symbol, "BTCUSDT");
    assert_eq!(series.diagnostics.dropped_at, vec![1_700_000_000_000], "when it was refused");

    let trades = vike_runs::read_trades(&dir).unwrap();
    assert_eq!(trades.source_len, 1);
    assert_eq!(trades.trades.len(), 1);
    assert_eq!(trades.trades[0].pnl, 10.0);
    assert_eq!(trades.trades[0].symbol, "BTCUSDT");
}

/// The manifest and the report keep doing exactly what they did — this stage ADDS documents and
/// changes neither of the two that were already there.
#[test]
fn persisting_still_writes_the_manifest_and_the_report_it_always_did() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = run_facts(&profile, &runs_root);

    let dir = persist_run(&facts, &result, &report).unwrap();
    let manifest = vike_runs::read_manifest(&dir).unwrap();

    assert_eq!(manifest.kind, vike_runs::BACKTEST_RUN_KIND);
    assert_eq!(manifest.produced_by, "backtesthub");
    // ⚠ RE-DERIVED, not adjusted to whatever came back. 1_756_000_000 unix seconds is
    // 2025-08-24T01:46:40Z: 1_735_689_600 is 2025-01-01T00:00:00Z, the difference is
    // 20_310_400 s = 235 days + 6_400 s, and day 235 of 2025 is 24 August. Python's
    // `datetime.fromtimestamp(1756000000, timezone.utc)` agrees, so `utc_rfc3339` is right and
    // the number this assertion first carried was not.
    assert_eq!(manifest.started_at, "2025-08-24T01:46:40Z");
    assert_eq!(manifest.config.path.as_deref(), Some("profiles/sma.toml"));
    assert_eq!(manifest.config.name.as_deref(), Some("sma cross"));
    assert_eq!(manifest.detail["strategy_file"], serde_json::json!("trend/sma_cross.rs"));
    assert!(dir.join(vike_runs::REPORT_FILE).is_file(), "the report is still written");
}

/// The retention cap, exercised through the real producer rather than through `decimate` alone:
/// a curve over the cap comes back thinned, says so, and still ENDS where the run ended.
#[test]
fn a_curve_over_the_cap_is_persisted_thinned_and_still_ends_where_the_run_ended() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let n = vike_runs::MAX_EQUITY_SAMPLES * 3 + 7;
    let mut result = a_result();
    result.equity_curve = (0..n).map(|i| 100_000.0 + i as f64).collect();
    result.equity_timestamp = (0..n).map(|i| 1_756_000_000_000 + i as i64 * 60_000).collect();
    result.per_symbol_curves = vec![("BTCUSDT".to_string(), result.equity_curve.clone())];
    result.final_equity = *result.equity_curve.last().unwrap();
    let report = a_report(&profile, &result);
    let facts = run_facts(&profile, &runs_root);

    let dir = persist_run(&facts, &result, &report).unwrap();
    let series = vike_runs::read_series(&dir).unwrap();

    assert!(series.stride > 1, "a curve three times the cap must be thinned");
    assert_eq!(series.source_len, n, "the record must say how long the real curve was");
    assert!(series.equity.len() <= vike_runs::MAX_EQUITY_SAMPLES + 1);
    assert_eq!(
        *series.equity.last().unwrap(),
        result.final_equity,
        "the last sample must be the run's outcome, or a reader deriving total_return from the \
             file disagrees with report.json"
    );
    assert!(series.is_aligned(), "thinning must thin BOTH vectors at the same stride");
    assert_eq!(
        series.per_symbol_equity[0].1.len(),
        series.equity.len(),
        "a per-symbol curve thinned at a different stride would no longer line up by index"
    );
}

/// Book W35: the forecasts a strategy published reach the run directory as published, and a run that
/// published none writes no forecasts file (its directory is what it always was).
#[test]
fn a_runs_forecasts_are_saved_and_a_run_without_any_saves_no_file() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let quiet = a_result();
    let facts = run_facts(&profile, &runs_root);
    let dir = persist_run(&facts, &quiet, &a_report(&profile, &quiet)).unwrap();
    assert!(!dir.join(vike_runs::FORECASTS_FILE).exists(), "no forecast, no file");

    let mut result = a_result();
    result.forecasts = (0..3)
        .map(|i| vike_model::ForecastRow {
            timestamp: 1_767_225_600_000 + i * 3_600_000,
            symbol: "BTCUSDT".to_string(),
            forecast: vike_model::Forecast::new(0.001 * i as f64, 3_600_000, None).unwrap(),
        })
        .collect();
    let facts = BacktestRunInputs { started_at: 1_756_000_001, ..run_facts(&profile, &runs_root) };
    let dir = persist_run(&facts, &result, &a_report(&profile, &result)).unwrap();
    let saved = vike_runs::read_forecasts(&dir).unwrap();
    assert_eq!(saved.forecasts, result.forecasts);
    assert_eq!((saved.schema, saved.source_len), (vike_runs::FORECASTS_SCHEMA, 3));
}

/// A ledger over the cap is a PREFIX and says so — never a sample, which would make win rate
/// and profit factor computed from the file fiction.
#[test]
fn a_ledger_over_the_cap_is_a_prefix_that_declares_its_true_length() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let mut result = a_result();
    let one = result.trades[0].clone();
    result.trades = (0..vike_runs::MAX_TRADES + 5)
        .map(|i| {
            let mut t = one.clone();
            t.pnl = i as f64;
            t
        })
        .collect();
    result.n_trades = result.trades.len();
    let report = a_report(&profile, &result);
    let facts = run_facts(&profile, &runs_root);

    let dir = persist_run(&facts, &result, &report).unwrap();
    let trades = vike_runs::read_trades(&dir).unwrap();

    assert_eq!(trades.trades.len(), vike_runs::MAX_TRADES);
    assert_eq!(trades.source_len, vike_runs::MAX_TRADES + 5);
    assert_eq!(trades.trades[0].pnl, 0.0, "a prefix keeps the FIRST trades, in order");
    assert_eq!(trades.trades[1].pnl, 1.0);
}

/// Persisting is ADDITIVE, never a new way to fail: a runs root that cannot be created comes
/// back as a message naming the path, so the caller prints the numbers it already computed.
#[test]
fn a_runs_root_that_cannot_be_created_is_reported_rather_than_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let blocked = tmp.path().join("blocked");
    std::fs::write(&blocked, b"a file where the runs directory would go").unwrap();
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = run_facts(&profile, &blocked);

    let why = persist_run(&facts, &result, &report).unwrap_err();

    assert!(why.contains("blocked"), "the message must name the path: {why}");
}

/// The bar lane resolves ONE id per symbol, sub-partitioned by the bar step — which is a real
/// path segment in this store, not a column.
#[test]
fn a_bar_profile_fingerprints_one_bar_series_per_symbol_at_its_interval() {
    let toml =
        PROFILE_TOML.replace(r#"symbols = ["BTCUSDT"]"#, r#"symbols = ["BTCUSDT", "ETHUSDT"]"#);
    let profile = ProfileBacktest::from_toml_str(&toml).unwrap();

    let ids = vike_backtest::run::data_plan::planned_series_ids(&profile, &[]);

    assert_eq!(ids.len(), 2, "one per symbol: {ids:?}");
    assert!(ids.iter().all(|i| i.kind == StoreKindId::Bar));
    assert!(ids.iter().all(|i| i.interval.as_deref() == Some("1h")));
    assert!(ids.iter().any(|i| i.symbol == "BTCUSDT"));
    assert!(ids.iter().any(|i| i.symbol == "ETHUSDT"));
}

/// A whole-lane tick series reads THREE kinds, because that is what the replay loader's own
/// `SeriesKind::Tick` admits — and a fingerprint that named only one would claim a run depended
/// on less data than it did.
#[test]
fn a_whole_lane_tick_profile_fingerprints_every_lane_the_loader_admits() {
    let toml = PROFILE_TOML
        .replace(r#"kind = "bar""#, r#"kind = "tick""#)
        .replace(r#"venue = "binance""#, r#"venue = "polymarket""#);
    let profile = ProfileBacktest::from_toml_str(&toml).unwrap();

    let ids = vike_backtest::run::data_plan::planned_series_ids(&profile, &[]);
    let kinds: Vec<&str> = ids.iter().map(|i| i.kind.word()).collect();

    assert_eq!(ids.len(), 3, "quote + trade + book: {ids:?}");
    for want in ["quote", "trade", "book"] {
        assert!(kinds.contains(&want), "the {want} lane is missing: {kinds:?}");
    }
    assert!(ids.iter().all(|i| i.interval.is_none()), "a tick series has no bar step");
}

/// The fingerprint reaches the manifest's kind-specific subtree, versioned, with the window and
/// the store the run actually read — so a reader holding the file alone can say which bytes
/// produced these numbers.
#[test]
fn the_data_fingerprint_lands_under_the_manifests_detail_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let data = run_fingerprint::DataFingerprint {
        schema: run_fingerprint::DATA_FINGERPRINT_SCHEMA,
        store: tmp.path().display().to_string(),
        from_ms: Some(1_756_000_000_000),
        to_ms: Some(1_756_999_000_000),
        series: vec![run_fingerprint::DataFingerprintSeries {
            id: vike_data::SeriesId::per_symbol(
                StoreKindId::Bar,
                "binance",
                "BTCUSDT",
                Some("1h".to_string()),
            ),
            coverage: Some(vike_data::SeriesCoverage {
                first_timestamp: 1_756_000_000_000,
                last_timestamp: 1_756_999_000_000,
                rows: 277,
                bytes: 4_096,
                parts: 3,
                dates: 2,
            }),
            commits: vec!["binance:BTCUSDT:1h:0-1".to_string()],
            commits_len: 1,
        }],
        inputs: Vec::new(),
        conversions: Vec::new(),
    };
    let facts = BacktestRunInputs { data: Some(&data), ..run_facts(&profile, &runs_root) };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let manifest = vike_runs::read_manifest(&dir).unwrap();

    let fp = &manifest.detail["data"]["fingerprint"];
    assert_eq!(fp["schema"], serde_json::json!(1));
    assert_eq!(fp["series"][0]["coverage"]["rows"], serde_json::json!(277));
    assert_eq!(fp["series"][0]["commits"][0], serde_json::json!("binance:BTCUSDT:1h:0-1"));
    assert_eq!(
        manifest.detail["data"]["interval"],
        serde_json::json!("1h"),
        "the fingerprint NESTS under `data` and does not displace what was already there"
    );
}

/// The run directory holds the config VERBATIM — comments, ordering and all — which is what a
/// later `promote` or a re-run needs, and what the input address was taken over.
#[test]
fn a_persisted_run_holds_the_config_text_that_drove_it_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = run_facts(&profile, &runs_root);

    let dir = persist_run(&facts, &result, &report).unwrap();

    assert_eq!(vike_runs::read_config_toml(&dir).unwrap(), PROFILE_TOML);
}

/// The address reaches BOTH places it has to: the manifest, which is the authority a `diff` or
/// a `gate` compares on, and the directory NAME, which is what a human scanning
/// `user_data/runs/` reads.
#[test]
fn a_run_with_an_address_carries_it_in_the_manifest_and_in_its_directory_name() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let addr = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    let facts = BacktestRunInputs { fingerprint: Some(addr), ..run_facts(&profile, &runs_root) };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let manifest = vike_runs::read_manifest(&dir).unwrap();

    assert_eq!(
        manifest.fingerprint.as_deref(),
        Some(addr),
        "the manifest carries the WHOLE digest — the name only carries a prefix"
    );
    let name = dir.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.starts_with("1756000000-9f86d081884c7d65-"), "{name}");
}

/// A producer that CAN name its build fills the common `git_sha` field and nests the rest,
/// because the sha is the one fact a listing renders a column from and the rest is detail.
#[test]
fn a_run_from_a_binary_that_knows_its_build_records_the_commit_and_the_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = BacktestRunInputs {
        build: Some(vike_runs::BuildStamp {
            git_sha: Some("62ccdd8e"),
            summary: Some(
                "git 62ccdd8e clean, built 2026-08-09T09:41:07Z, rustc 1.96.0, \
                     x86_64-unknown-linux-gnu",
            ),
        }),
        ..run_facts(&profile, &runs_root)
    };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let manifest = vike_runs::read_manifest(&dir).unwrap();

    assert_eq!(manifest.git_sha.as_deref(), Some("62ccdd8e"));
    assert!(
        manifest.detail["build"].as_str().unwrap().contains("rustc 1.96.0"),
        "the full stamp nests: {}",
        manifest.detail["build"]
    );
}

/// A producer that CANNOT name its build still writes both keys as `null`, for the reason
/// `RunManifest::git_sha`'s own doc gives: "does not know" and "wrote no such field" are
/// different answers, and the standalone `backtesthub` binary is the common path on Linux.
#[test]
fn a_run_from_a_binary_that_cannot_name_its_build_writes_null_rather_than_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = run_facts(&profile, &runs_root);

    let dir = persist_run(&facts, &result, &report).unwrap();
    let text = std::fs::read_to_string(dir.join(vike_runs::MANIFEST_FILE)).unwrap();
    let raw: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert_eq!(raw["git_sha"], serde_json::Value::Null);
    assert_eq!(raw["detail"]["build"], serde_json::Value::Null);
}

/// ⚠ **Stride `0` means KEEP NOTHING, and the companion vectors must agree with `decimate`
/// about it.** `decimate(_, 0)` returns `(vec![], 0)`; `keep_at_stride` once spelled its fast
/// path `stride <= 1` and so returned the WHOLE vector for the same stride. The result is a
/// document THIS BUILD wrote that fails its own `is_aligned` invariant — and a
/// `--keep-series none`-shaped flag, whose whole purpose is to suppress the series, writing a
/// LARGER file than keeping it. Reverting either clause below turns this red.
#[test]
fn a_stride_of_zero_keeps_nothing_in_the_companion_vectors_too() {
    let curve = vec![10_000.0, 10_100.0, 9_950.0];
    let ts = vec![1_756_000_000_000i64, 1_756_000_060_000, 1_756_000_120_000];

    let (equity, stride) = vike_runs::decimate(&curve, 0);
    assert!(equity.is_empty(), "decimate's own contract");
    assert_eq!(stride, 0, "...and the stride it reports for it");

    assert!(
        keep_at_stride(&ts, stride).is_empty(),
        "a timestamp vector kept at stride 0 while the equity was dropped is a MISALIGNED \
             document, written by this build"
    );
    assert!(keep_at_stride(&curve, stride).is_empty(), "and the same for a per-symbol curve");

    // The property those two add up to, asserted through the type that declares it.
    let series = vike_runs::RunSeries {
        schema: vike_runs::SERIES_SCHEMA,
        equity,
        equity_timestamp: keep_at_stride(&ts, stride),
        per_symbol_equity: vec![("BTCUSDT".to_string(), keep_at_stride(&curve, stride))],
        stride,
        source_len: curve.len(),
        diagnostics: vike_runs::RunDiagnostics::default(),
    };
    assert!(series.is_aligned(), "a suppressed series must still satisfy its own invariant");
    assert_eq!(series.source_len, 3, "...and must still say what the run actually produced");
}

/// The other end of the same clause, so the fix cannot be "return empty always": stride 1 is
/// the EXACTNESS claim and must keep every sample.
#[test]
fn a_stride_of_one_keeps_every_companion_sample() {
    let ts = vec![1i64, 2, 3];

    assert_eq!(keep_at_stride(&ts, 1), ts);
    assert_eq!(keep_at_stride::<i64>(&[], 1), Vec::<i64>::new());
    assert_eq!(keep_at_stride::<i64>(&[], 0), Vec::<i64>::new(), "empty is empty either way");
}

/// ⚠ **ABSENT and EMPTY are different answers, and until this the only producer in the tree
/// could never say ABSENT.** `series_facts` folds a manifest, and `read_manifest` maps
/// `NotFound` to an EMPTY manifest — so a series the store has never held came back as
/// `Ok(SeriesCoverage::default())`, `Some(all-zeros)`, rendering `coverage 0 0 0 0`. The
/// `coverage: None` arm and the `coverage missing` rendering were both unreachable, and the
/// test that "proved" the distinction built BOTH states by hand. Driven here through the real
/// collector against a real (empty) store, which is the thing that was never true.
/// ⚠ **A wire-routed run must not record a DIRECTORY, and the failure mode is silent.** The
/// local ladder still resolves a root on that arm — the fallback needs one — so the old
/// `store_root.display()` would have written this box's default path into the one document a
/// later reader uses to reproduce the run, naming a directory nothing opened. Nothing would
/// have failed; the record would simply have been false.
///
/// Driven through the REAL collector with the REAL label renderer, not by asserting the two
/// strings match: composing them here is the whole claim.
#[test]
fn a_wire_run_records_the_datahub_rather_than_a_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    let store = HistoryStoreDataFusion::open(&root).unwrap();
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();

    let label = vike_datahub_client::route::history_route(Some("<host>:7878")).label();
    let fp = collect_data_fingerprint(&store, &profile, &label).unwrap();

    assert_eq!(fp.store, "the datahub at <host>:7878");
    assert!(
        !fp.store.contains(&root.display().to_string()),
        "the record named a local directory the run never opened: {}",
        fp.store
    );
}

#[test]
fn a_series_the_store_does_not_hold_is_recorded_as_missing_rather_than_as_all_zeros() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    let store = HistoryStoreDataFusion::open(&root).unwrap();
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();

    let fp = collect_data_fingerprint(&store, &profile, &root.display().to_string()).unwrap();

    assert_eq!(fp.series.len(), 1, "one bar series: {:?}", fp.series);
    assert!(
        fp.series[0].coverage.is_none(),
        "an empty store HOLDS no series — `Some(all-zeros)` would be the old silent answer: \
             {:?}",
        fp.series[0]
    );
    assert!(
        fp.canonical().contains("coverage missing"),
        "...and the address must SAY so rather than rendering a zero row: {}",
        fp.canonical()
    );
    assert_ne!(
        fp.canonical(),
        {
            let mut empty = fp.clone();
            empty.series[0].coverage = Some(vike_data::SeriesCoverage::default());
            empty.canonical()
        },
        "absent and empty must not address the same"
    );
}

/// ⚠ **THE GROUPED-STORE HOLE.** All three tick kinds are `grouped: true` and the readers union
/// every `group=` directory under `kind=…/venue=…`, so on a grouped store every row the run
/// replays lives in the group and the per-symbol path does not exist. Naming only the
/// per-symbol id made the data half of the address a CONSTANT — a backfill could add a month
/// and the address would not move.
#[test]
fn a_tick_profile_fingerprints_the_grouped_series_the_reader_actually_unions() {
    let toml = PROFILE_TOML
        .replace(r#"kind = "bar""#, r#"kind = "tick""#)
        .replace(r#"venue = "binance""#, r#"venue = "polymarket""#);
    let profile = ProfileBacktest::from_toml_str(&toml).unwrap();
    let held = vec![
        vike_data::SeriesId::grouped(StoreKindId::Book, "polymarket", "btc-5m"),
        vike_data::SeriesId::grouped(StoreKindId::Quote, "polymarket", "btc-5m"),
        // A different VENUE — the reader lists groups under `kind=…/venue=…`, so this one is
        // not in the union and must not be in the address.
        vike_data::SeriesId::grouped(StoreKindId::Trade, "bybit", "x"),
        // `bar` is `grouped: false` in `store_kind.rs` and `load_bars` reads the per-symbol
        // directory alone, so a `group=` leaf of that kind is not the bar lane's business.
        vike_data::SeriesId::grouped(StoreKindId::Bar, "polymarket", "never"),
    ];

    let ids = vike_backtest::run::data_plan::planned_series_ids(&profile, &held);

    assert!(ids.contains(&vike_data::SeriesId::grouped(StoreKindId::Book, "polymarket", "btc-5m")));
    assert!(ids.contains(&vike_data::SeriesId::grouped(
        StoreKindId::Quote,
        "polymarket",
        "btc-5m"
    )));
    assert!(!ids.iter().any(|i| i.venue == "bybit"), "another venue's group: {ids:?}");
    assert!(
        !ids.iter().any(|i| i.kind == StoreKindId::Bar),
        "a bar lane is never grouped: {ids:?}"
    );
    assert_eq!(
        ids.iter().filter(|i| i.group.is_none()).count(),
        3,
        "the three per-symbol lanes are still named: {ids:?}"
    );
}

/// Two symbols of one venue pull the SAME grouped directories, and the reader reads each group
/// once — so the record must name it once too, or a group's rows would be counted twice by
/// anyone folding the fingerprint.
#[test]
fn one_grouped_series_is_named_once_however_many_symbols_pull_it() {
    let toml = PROFILE_TOML
        .replace(r#"kind = "bar""#, r#"kind = "tick""#)
        .replace(r#"symbols = ["BTCUSDT"]"#, r#"symbols = ["A", "B", "C"]"#);
    let profile = ProfileBacktest::from_toml_str(&toml).unwrap();
    let held = vec![vike_data::SeriesId::grouped(StoreKindId::Quote, "binance", "g1")];

    let ids = vike_backtest::run::data_plan::planned_series_ids(&profile, &held);

    assert_eq!(
        ids.iter().filter(|i| i.group.as_deref() == Some("g1")).count(),
        1,
        "three symbols, one group, one row: {ids:?}"
    );
}

/// A `source=` lane is no part of a run's address: no read of the run reads it, and the recorder's
/// flush every thirty seconds must not change the address of a run over the series beside it.
#[test]
fn a_lane_beside_a_planned_series_is_no_part_of_the_run_address() {
    let toml = PROFILE_TOML.replace(r#"kind = "bar""#, r#"kind = "tick""#);
    let profile = ProfileBacktest::from_toml_str(&toml).unwrap();
    let held = vec![
        vike_data::SeriesId::per_symbol(StoreKindId::Quote, "binance", "BTCUSDT", None)
            .with_source("recorder"),
    ];

    let ids = vike_backtest::run::data_plan::planned_series_ids(&profile, &held);

    assert!(!ids.is_empty(), "the tick profile plans its own series");
    assert!(ids.iter().all(|id| id.source.is_none()), "{ids:?}");
}

/// A bar profile takes no grouped series even when the store holds one of that kind — the
/// symmetry that keeps the tick fix from widening the bar lane's address.
#[test]
fn a_bar_profile_takes_no_grouped_series_even_when_the_store_holds_one() {
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let held = vec![vike_data::SeriesId::grouped(StoreKindId::Bar, "binance", "g1")];

    let ids = vike_backtest::run::data_plan::planned_series_ids(&profile, &held);

    assert_eq!(ids.len(), 1, "{ids:?}");
    assert!(ids[0].group.is_none());
}

/// The collector-level FLOOR for the commit bound: whatever the store held, every series it
/// produces obeys [`run_fingerprint::MAX_COMMIT_KEYS`] and its true count is never below the
/// prefix it kept.
///
/// ⚠ **This does NOT prove the truncation, and saying so is the point.** The bound binds only
/// on a log of hundreds of keys, which needs a store a unit test cannot cheaply build — so this
/// passed with the truncation REMOVED (measured: the whole 819-test suite did). The truncation
/// itself is proved on the pure primitive, `run_fingerprint`'s
/// `a_commit_log_over_the_bound_becomes_a_prefix_that_declares_its_true_length`, which is why
/// that primitive is a function rather than two lines here.
#[test]
fn every_series_the_collector_produces_obeys_the_commit_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("store");
    let store = HistoryStoreDataFusion::open(&root).unwrap();
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();

    let fp = collect_data_fingerprint(&store, &profile, &root.display().to_string()).unwrap();

    assert!(!fp.series.is_empty(), "the floor: a harvest that produced nothing proves nothing");
    for s in &fp.series {
        assert!(
            s.commits.len() <= run_fingerprint::MAX_COMMIT_KEYS,
            "{:?} carries {} keys, over the bound",
            s.id,
            s.commits.len()
        );
        assert!(s.commits_len >= s.commits.len(), "the true count is never below the prefix");
    }
}

// ---- what `--resume` compares ------------------------------------------------------------------

/// Answers the two reads the witness makes from what a test planted, each series with its commits.
#[derive(Default)]
struct Planted(Vec<(vike_data::SeriesId, Vec<String>)>);

impl vike_data::HistoryStore for Planted {
    fn list_series(&self) -> Result<Vec<vike_data::SeriesId>, vike_data::HistoryStoreError> {
        Ok(self.0.iter().map(|(id, _)| id.clone()).collect())
    }
    fn series_facts(
        &self,
        id: &vike_data::SeriesId,
    ) -> Result<(vike_data::SeriesCoverage, Vec<String>), vike_data::HistoryStoreError> {
        let commits = self.0.iter().find(|(held, _)| held == id).map(|(_, c)| c.clone());
        Ok((vike_data::SeriesCoverage::default(), commits.expect("the test planted this series")))
    }
    vike_data::hist_store_stubs!(inert: all);
}

/// A store with no answer to any read the witness could make: the trait's defaults refuse both.
struct AnswersNothing;

impl vike_data::HistoryStore for AnswersNothing {
    vike_data::hist_store_stubs!(inert: all);
}

/// Two price lanes as the collector records them, one held and one not.
fn two_price_lanes() -> run_fingerprint::DataFingerprint {
    let lane = |symbol: &str| {
        vike_data::SeriesId::per_symbol(StoreKindId::Bar, "binance", symbol, Some("1h".to_string()))
    };
    run_fingerprint::DataFingerprint {
        schema: run_fingerprint::DATA_FINGERPRINT_SCHEMA,
        store: "the datahub at 127.0.0.1:7878".to_string(),
        from_ms: None,
        to_ms: None,
        series: vec![
            run_fingerprint::DataFingerprintSeries {
                id: lane("ETHUSDT"),
                coverage: None,
                commits: Vec::new(),
                commits_len: 0,
            },
            run_fingerprint::DataFingerprintSeries {
                id: lane("BTCUSDT"),
                coverage: Some(vike_data::SeriesCoverage::default()),
                commits: vec!["k1".to_string(), "k2".to_string()],
                commits_len: 2,
            },
        ],
        inputs: Vec::new(),
        conversions: Vec::new(),
    }
}

/// ⚠ **`store_data` is compared as a string, so these bytes are a promise to every search already
/// on someone's disk**: a profile that set none of the opt-in keys must witness exactly what it
/// always did, and reach the store for nothing to do it. The literal is what an older binary wrote.
#[test]
fn a_profile_with_none_of_the_opt_in_keys_keeps_the_witness_byte_equal_and_reads_nothing() {
    let profile = ProfileBacktest::from_toml_str(PROFILE_TOML).unwrap();
    let witness = search_witness(&two_price_lanes(), &profile, &AnswersNothing)
        .expect("nothing to read, so nothing to fail");
    assert_eq!(
        witness,
        "series bar binance BTCUSDT - 1h commits 2 k1 k2\n\
         series bar binance ETHUSDT - 1h absent\n"
    );
}

/// A strategy that declares the names it holds as its inputs, and does nothing.
struct DeclaresInputs(Vec<String>);

impl vike_model::Strategy<vike_simulator::OrderBrokerSimulator> for DeclaresInputs {
    fn inputs(&self) -> Vec<String> {
        self.0.clone()
    }
}

/// [`PROFILE_TOML`] with `extra` tables after it, whose strategy declares `names`.
fn declaring(extra: &str, names: &[&str]) -> ProfileBacktest {
    let mut p = ProfileBacktest::from_toml_str(&format!("{PROFILE_TOML}\n{extra}")).unwrap();
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    p.strategy.as_mut().unwrap().builder =
        Some(vike_backtest::run::strategy_builder::StrategyBuilder::new(move |_| {
            Ok(Box::new(DeclaresInputs(names.clone())))
        }));
    p
}

/// The collector records each input the strategy declares by its name and the series it resolves
/// to, through `[inputs]` and the one-part form, with what the store held of that series; the search
/// witness names the series it read (the lane included) and its commit keys. A strategy that declares
/// none records no input, and its document carries no `inputs` key.
#[test]
fn the_collector_records_each_input_by_name_and_series_and_the_witness_names_its_series() {
    let four = vike_data::SeriesId::per_symbol(
        StoreKindId::Bar,
        "binance",
        "BTCUSDT",
        Some("4h".to_string()),
    );
    let store = Planted(vec![(four, vec!["k4".to_string()])]);
    let remap = "[inputs]\n\"binance:ETHUSDT:1d\" = \"user-mine:binance:ETHUSDT:1d\"\n";
    let profile = declaring(remap, &["4h", "binance:ETHUSDT:1d"]);
    let fp = collect_data_fingerprint(&store, &profile, "test store").unwrap();

    let read: Vec<(&str, &str, bool)> = fp
        .inputs
        .iter()
        .map(|i| (i.name.as_str(), i.read.as_str(), i.series.coverage.is_some()))
        .collect();
    assert_eq!(
        read,
        vec![
            ("4h", "binance:BTCUSDT:4h", true),
            ("binance:ETHUSDT:1d", "user-mine:binance:ETHUSDT:1d", false),
        ]
    );
    let readings: Vec<String> =
        fp.inputs.iter().map(run_fingerprint::DataFingerprintInput::reading).collect();
    assert_eq!(readings[0], r#""4h" = "binance:BTCUSDT:4h""#);
    let witness = data_witness(&fp);
    assert!(witness.contains("input \"binance:BTCUSDT:4h\" commits 1 k4\n"), "{witness}");
    assert!(witness.contains("input \"user-mine:binance:ETHUSDT:1d\" absent\n"), "{witness}");

    let none = collect_data_fingerprint(&store, &declaring("", &[]), "test store").unwrap();
    assert!(none.inputs.is_empty());
    assert!(!serde_json::to_string(&none).unwrap().contains("inputs"));
    assert!(!data_witness(&none).contains("input "), "{}", data_witness(&none));
}

/// The collector records each conversion pair the profile names (decision 0243) with what the store
/// held of its series, the witness names it with its commit keys, and a write under the pair (a
/// backfill or a re-import) moves the witness, so `--resume` REFUSES rather than ranking scores booked
/// at two rates. A profile that names none records none and witnesses the bytes it always did.
#[test]
fn a_conversion_pair_is_witnessed_and_a_write_under_it_refuses_a_resume() {
    let pair_id = vike_data::SeriesId::per_symbol(
        StoreKindId::Bar,
        "coinbase",
        "USDT-USD",
        Some("1d".to_string()),
    );
    let profile = declaring("[engine.conversion]\nUSDT = \"coinbase:USDT-USD:1d\"\n", &[]);
    let witness_over = |commits: &[&str]| {
        let store =
            Planted(vec![(pair_id.clone(), commits.iter().map(|c| c.to_string()).collect())]);
        let fp = collect_data_fingerprint(&store, &profile, "test store").unwrap();
        (search_witness(&fp, &profile, &store).unwrap(), fp)
    };
    let (before, fp) = witness_over(&["c1"]);
    let recorded: Vec<(&str, &str, bool)> = fp
        .conversions
        .iter()
        .map(|c| (c.quote_currency.as_str(), c.read.as_str(), c.series.coverage.is_some()))
        .collect();
    assert_eq!(recorded, vec![("USDT", "coinbase:USDT-USD:1d", true)]);
    assert!(
        before.contains("conversion \"USDT\" \"coinbase:USDT-USD:1d\" commits 1 c1\n"),
        "{before}"
    );

    let identity = |store_data: String| trial_ledger::SearchIdentity {
        profile_fnv1a64: "f".to_string(),
        profile_name: None,
        store: "the datahub".to_string(),
        store_data,
        build: None,
        method: "grid".to_string(),
        rank_by: "sharpe".to_string(),
        seed: None,
        budget: None,
        inputs: Vec::new(),
        strategy_sha256: String::new(),
    };
    let (same, _) = witness_over(&["c1"]);
    assert!(identity(before.clone()).differences(&identity(same)).is_empty(), "nothing moved");
    let (backfilled, _) = witness_over(&["c1", "c2"]);
    let refused = identity(before).differences(&identity(backfilled));
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].starts_with("data: the store's contents MOVED")
            && refused[0].contains("a conversion pair's series"),
        "{refused:?}"
    );

    let none = collect_data_fingerprint(&Planted::default(), &declaring("", &[]), "test store");
    let none = none.unwrap();
    assert!(none.conversions.is_empty());
    assert!(!serde_json::to_string(&none).unwrap().contains("conversions"));
    assert!(!data_witness(&none).contains("conversion"), "{}", data_witness(&none));
}

/// Each store series a key makes the run read joins the witness when the key is set, and a write
/// under it moves the witness.
#[test]
fn each_opt_in_series_joins_the_witness_and_a_write_under_it_moves_it() {
    let id = |kind, interval: Option<&str>| {
        vike_data::SeriesId::per_symbol(kind, "binance", "BTCUSDT", interval.map(str::to_string))
    };
    let engine = |key: &str| PROFILE_TOML.replace("# engine", &format!("# engine\n{key} = true"));
    let cases = [
        (engine("attach_funding"), id(StoreKindId::Bar, Some("funding"))),
        (engine("snap_to_properties"), id(StoreKindId::Properties, None)),
        (
            PROFILE_TOML
                .replace(r#"interval = "1h""#, "interval = \"1h\"\ndetail_interval = \"5m\""),
            id(StoreKindId::Bar, Some("5m")),
        ),
    ];
    let data = two_price_lanes();
    let bare = data_witness(&data);
    for (toml, series) in cases {
        assert_ne!(toml, PROFILE_TOML, "the fixture's text moved");
        let profile = ProfileBacktest::from_toml_str(&toml).unwrap();
        let witness = |planted: Planted| search_witness(&data, &profile, &planted).unwrap();

        let absent = witness(Planted::default());
        assert!(absent.starts_with(&bare), "{absent}");
        assert!(absent.ends_with(" absent\n"), "a series not held yet is named: {absent}");
        assert_ne!(absent, bare, "{series:?} is not in the witness");

        let one = witness(Planted(vec![(series.clone(), vec!["k1".to_string()])]));
        let two =
            witness(Planted(vec![(series.clone(), vec!["k1".to_string(), "k2".to_string()])]));
        assert_ne!(absent, one, "a first write under {series:?} moves it");
        assert_ne!(one, two, "and so does a backfill");
        assert_eq!(one, witness(Planted(vec![(series.clone(), vec!["k1".to_string()])])));
    }
}

/// The recorder adds a grid row every UTC day and a fill reads the row as of its own time, so a row past
/// the window changes no score. It used to move the witness all the same, and `--resume` then refused
/// the search the next day as data moved "inside this profile's own window".
#[test]
fn a_grid_row_past_the_window_leaves_the_witness_where_it_was() {
    let grid = vike_data::SeriesId::per_symbol(StoreKindId::Properties, "binance", "BTCUSDT", None);
    let toml = PROFILE_TOML.replace("# engine", "# engine\nsnap_to_properties = true");
    let profile = ProfileBacktest::from_toml_str(&toml).unwrap();
    let data = two_price_lanes();
    let planted = |keys: Vec<String>| {
        search_witness(&data, &profile, &Planted(vec![(grid.clone(), keys)])).unwrap()
    };
    let witness =
        |days: &[&str]| planted(days.iter().map(|day| format!("binance:BTCUSDT:{day}")).collect());

    let in_window = witness(&["2026-01-15", "2026-01-16"]);
    let later = ["2026-01-15", "2026-01-16", "2026-02-02", "2026-02-03"];
    assert_eq!(witness(&later), in_window, "the window ends 2026-02-01");

    let inside = ["2026-01-15", "2026-01-16", "2026-01-20"];
    assert_ne!(witness(&inside), in_window, "a row inside the window moves it");
    let last_day = ["2026-01-15", "2026-01-16", "2026-02-01"];
    assert_ne!(witness(&last_day), in_window, "and so does one on its last day");

    let absent = search_witness(&data, &profile, &Planted::default()).unwrap();
    assert_eq!(witness(&["2026-02-02"]), absent, "a tape that begins past the window");

    let unstated = planted(vec!["k1".to_string(), "k2".to_string()]);
    assert_ne!(unstated, planted(vec!["k1".to_string()]), "a key naming no day stays");
}

/// ...and a store fault there is an `Err`, which the search records as the sentinel that never
/// compares equal, rather than a witness that leaves the series out.
#[test]
fn an_opt_in_series_the_store_will_not_list_is_an_error() {
    let toml = PROFILE_TOML.replace("# engine", "# engine\nattach_funding = true");
    let profile = ProfileBacktest::from_toml_str(&toml).unwrap();
    assert!(search_witness(&two_price_lanes(), &profile, &AnswersNothing).is_err());
}

/// The winners file sits beside the profile and outside the store. Its bytes join the witness when
/// the key names it, resolved the way the run resolves it, and a corrected file moves the witness.
#[test]
fn a_winners_file_joins_the_witness_and_a_correction_moves_it() {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "{PROFILE_TOML}\n[engine.resolution]\nkind = \"binary_outcome\"\npath = \"winners.csv\"\n"
    );
    let mut profile = ProfileBacktest::from_toml_str(&toml).unwrap();
    profile.base_dir = Some(dir.path().to_path_buf());
    let data = two_price_lanes();
    let witness = || search_witness(&data, &profile, &AnswersNothing);

    assert!(witness().is_err(), "a file that will not read is not a witness");

    std::fs::write(dir.path().join("winners.csv"), "slug,winning_index\nw-1,0\n").unwrap();
    let first = witness().unwrap();
    assert!(first.starts_with(&data_witness(&data)), "{first}");
    assert!(first.ends_with('\n') && first.contains("winners "), "{first}");
    assert_eq!(first, witness().unwrap(), "the same bytes are the same witness");

    std::fs::write(dir.path().join("winners.csv"), "slug,winning_index\nw-1,1\n").unwrap();
    assert_ne!(first, witness().unwrap(), "a corrected file moves it");
}

/// A resume prints the ledger and never the sweep report, so what the data lacked was printed nowhere.
/// A trial's metrics are its report, with the key only when it has something to say.
#[test]
fn a_trials_table_prints_what_the_ledger_says_the_data_lacked() {
    let record = |n, metrics| trial_ledger::TrialRecord {
        n,
        overrides: Vec::new(),
        score: 1.0,
        metrics,
        error: None,
    };
    let doc = trial_ledger::TrialLedgerDocument {
        schema: trial_ledger::TRIAL_LEDGER_SCHEMA,
        run_id: "1756000000-1-0".to_string(),
        keep_trials: "scalars".to_string(),
        identity: trial_ledger::SearchIdentity {
            profile_fnv1a64: "f".to_string(),
            profile_name: None,
            store: "the datahub".to_string(),
            store_data: String::new(),
            build: None,
            method: "grid".to_string(),
            rank_by: "sharpe".to_string(),
            seed: None,
            budget: None,
            inputs: Vec::new(),
            strategy_sha256: String::new(),
        },
        evaluated: 2,
        reused: 2,
        failed: 0,
        unreadable: 0,
        superseded: 0,
        trials: Vec::new(),
        overfit: None,
    };
    let warned = [
        record(0, None),
        record(1, Some(serde_json::json!({ "n_trades": 1 }))),
        record(2, Some(serde_json::json!({ "data_warnings": ["ETHUSDT: did not run"] }))),
        record(3, Some(serde_json::json!({ "data_warnings": ["ETHUSDT: did not run"] }))),
    ];

    let table = render_trials(&doc, &warned);
    assert!(table.contains("data_warnings:\n  ETHUSDT: did not run\n"), "{table}");
    assert_eq!(table.matches("ETHUSDT").count(), 1, "said once, not per row: {table}");

    let clean = render_trials(&doc, &[record(1, Some(serde_json::json!({ "n_trades": 1 })))]);
    assert!(!clean.contains("data_warnings"), "{clean}");
}
