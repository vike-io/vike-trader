use super::*;

/// A minimal profile that parses and validates. Kept close to the fixtures
/// `crates/vike-backtest/src/harness/profile.rs` already carries.
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
fee_rate = 0.001

[strategy]
name = "sma_cross"
"#;

fn a_result() -> vike_analytics::BacktestResult {
    vike_analytics::BacktestResult {
        trades: vec![vike_model::Trade {
            entry_price: 100.0,
            exit_price: 110.0,
            size: 1.0,
            pnl: 10.0,
            fees: 0.2,
            entry_ts: 1_756_000_000_000,
            exit_ts: 1_756_000_060_000,
            symbol: "BTCUSDT".to_string(),
            mae: -1.0,
            mfe: 12.0,
            is_long: true,
        }],
        equity_curve: vec![100_000.0, 100_010.0, 100_009.8],
        final_equity: 100_009.8,
        n_trades: 1,
        intrabar_both_hit: 2,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), 10.0)],
        per_symbol_curves: vec![("BTCUSDT".to_string(), vec![0.0, 10.0, 9.8])],
        equity_ts: vec![1_756_000_000_000, 1_756_000_060_000, 1_756_000_120_000],
        stale_deferrals: 3,
        impact_unpriced: 4,
        session_deferrals: 5,
        dropped: vec![("BTCUSDT".to_string(), "volume_cap".to_string(), 2.0, 1.0)],
        below_min_reversals: 6,
        warmup: 20,
        funding_paid: -1.5,
        maker_fills: 0,
        taker_fills: 2,
        fees_paid: 0.2,
    }
}

fn a_report(
    profile: &BacktestProfile,
    result: &vike_analytics::BacktestResult,
) -> vike_analytics::report::BacktestReport {
    vike_analytics::report::BacktestReport::from_result(
        profile.name.clone(),
        result,
        harness::report::periods_per_year(profile),
    )
}

/// ⚠ **§13a, as a property.** Everything the run computed and the report could not hold comes
/// back off disk: the curve WITH its timestamps, the ledger, the per-symbol curves and every
/// diagnostic counter. Before this, `result` was dropped when `run` returned and nothing saved
/// could be re-tearsheeted, bootstrapped or compared curve-to-curve.
#[test]
fn a_persisted_run_keeps_the_curve_the_ledger_and_every_diagnostic_counter() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: None,
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();

    let series = runs::read_series(&dir).unwrap();
    assert!(series.is_aligned(), "a persisted series must satisfy its own invariant");
    assert_eq!(series.equity, result.equity_curve);
    assert_eq!(series.equity_ts, result.equity_ts);
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

    let trades = runs::read_trades(&dir).unwrap();
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
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: None,
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let manifest = runs::read_manifest(&dir).unwrap();

    assert_eq!(manifest.kind, runs::BACKTEST_RUN_KIND);
    assert_eq!(manifest.produced_by, "backtest");
    // ⚠ RE-DERIVED, not adjusted to whatever came back. 1_756_000_000 unix seconds is
    // 2025-08-24T01:46:40Z: 1_735_689_600 is 2025-01-01T00:00:00Z, the difference is
    // 20_310_400 s = 235 days + 6_400 s, and day 235 of 2025 is 24 August. Python's
    // `datetime.fromtimestamp(1756000000, timezone.utc)` agrees, so `utc_rfc3339` is right and
    // the number this assertion first carried was not.
    assert_eq!(manifest.started_at, "2025-08-24T01:46:40Z");
    assert_eq!(manifest.config.path.as_deref(), Some("profiles/sma.toml"));
    assert_eq!(manifest.config.name.as_deref(), Some("sma cross"));
    assert_eq!(manifest.detail["strategy"], serde_json::json!("sma_cross"));
    assert!(dir.join(runs::REPORT_FILE).is_file(), "the report is still written");
}

/// The retention cap, exercised through the real producer rather than through `decimate` alone:
/// a curve over the cap comes back thinned, says so, and still ENDS where the run ended.
#[test]
fn a_curve_over_the_cap_is_persisted_thinned_and_still_ends_where_the_run_ended() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let n = runs::MAX_EQUITY_SAMPLES * 3 + 7;
    let mut result = a_result();
    result.equity_curve = (0..n).map(|i| 100_000.0 + i as f64).collect();
    result.equity_ts = (0..n).map(|i| 1_756_000_000_000 + i as i64 * 60_000).collect();
    result.per_symbol_curves = vec![("BTCUSDT".to_string(), result.equity_curve.clone())];
    result.final_equity = *result.equity_curve.last().unwrap();
    let report = a_report(&profile, &result);
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: None,
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let series = runs::read_series(&dir).unwrap();

    assert!(series.stride > 1, "a curve three times the cap must be thinned");
    assert_eq!(series.source_len, n, "the record must say how long the real curve was");
    assert!(series.equity.len() <= runs::MAX_EQUITY_SAMPLES + 1);
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

/// A ledger over the cap is a PREFIX and says so — never a sample, which would make win rate
/// and profit factor computed from the file fiction.
#[test]
fn a_ledger_over_the_cap_is_a_prefix_that_declares_its_true_length() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let mut result = a_result();
    let one = result.trades[0].clone();
    result.trades = (0..runs::MAX_TRADES + 5)
        .map(|i| {
            let mut t = one.clone();
            t.pnl = i as f64;
            t
        })
        .collect();
    result.n_trades = result.trades.len();
    let report = a_report(&profile, &result);
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: None,
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let trades = runs::read_trades(&dir).unwrap();

    assert_eq!(trades.trades.len(), runs::MAX_TRADES);
    assert_eq!(trades.source_len, runs::MAX_TRADES + 5);
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
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &blocked,
        data: None,
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let why = persist_run(&facts, &result, &report).unwrap_err();

    assert!(why.contains("blocked"), "the message must name the path: {why}");
}

/// The bar lane resolves ONE id per symbol, sub-partitioned by the bar step — which is a real
/// path segment in this store, not a column.
#[test]
fn a_bar_profile_fingerprints_one_bar_series_per_symbol_at_its_interval() {
    let toml =
        PROFILE_TOML.replace(r#"symbols = ["BTCUSDT"]"#, r#"symbols = ["BTCUSDT", "ETHUSDT"]"#);
    let profile = BacktestProfile::from_toml_str(&toml).unwrap();

    let ids = run_fingerprint::planned_series_ids(&profile, &[]);

    assert_eq!(ids.len(), 2, "one per symbol: {ids:?}");
    assert!(ids.iter().all(|i| i.kind == "bar"));
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
    let profile = BacktestProfile::from_toml_str(&toml).unwrap();

    let ids = run_fingerprint::planned_series_ids(&profile, &[]);
    let kinds: Vec<&str> = ids.iter().map(|i| i.kind.as_str()).collect();

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
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let data = run_fingerprint::DataFingerprint {
        schema: run_fingerprint::DATA_FINGERPRINT_SCHEMA,
        store: tmp.path().display().to_string(),
        from_ms: Some(1_756_000_000_000),
        to_ms: Some(1_756_999_000_000),
        series: vec![run_fingerprint::SeriesFingerprint {
            id: vike_data::SeriesId::per_symbol(
                "bar",
                "binance",
                "BTCUSDT",
                Some("1h".to_string()),
            ),
            coverage: Some(vike_data::SeriesCoverage {
                first_ts: 1_756_000_000_000,
                last_ts: 1_756_999_000_000,
                rows: 277,
                bytes: 4_096,
                parts: 3,
                dates: 2,
            }),
            commits: vec!["binance:BTCUSDT:1h:0-1".to_string()],
            commits_len: 1,
        }],
    };
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: Some(&data),
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let manifest = runs::read_manifest(&dir).unwrap();

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
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: None,
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();

    assert_eq!(runs::read_config_toml(&dir).unwrap(), PROFILE_TOML);
}

/// The address reaches BOTH places it has to: the manifest, which is the authority a `diff` or
/// a `gate` compares on, and the directory NAME, which is what a human scanning
/// `user_data/runs/` reads.
#[test]
fn a_run_with_an_address_carries_it_in_the_manifest_and_in_its_directory_name() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let addr = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: None,
        fingerprint: Some(addr),
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let manifest = runs::read_manifest(&dir).unwrap();

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
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: None,
        fingerprint: None,
        build: Some(runs::BuildStamp {
            git_sha: Some("62ccdd8e"),
            summary: Some(
                "git 62ccdd8e clean, built 2026-08-09T09:41:07Z, rustc 1.96.0, \
                     x86_64-unknown-linux-gnu",
            ),
        }),
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let manifest = runs::read_manifest(&dir).unwrap();

    assert_eq!(manifest.git_sha.as_deref(), Some("62ccdd8e"));
    assert!(
        manifest.detail["build"].as_str().unwrap().contains("rustc 1.96.0"),
        "the full stamp nests: {}",
        manifest.detail["build"]
    );
}

/// A producer that CANNOT name its build still writes both keys as `null`, for the reason
/// `RunManifest::git_sha`'s own doc gives: "does not know" and "wrote no such field" are
/// different answers, and the standalone `backtest` binary is the common path on Linux.
#[test]
fn a_run_from_a_binary_that_cannot_name_its_build_writes_null_rather_than_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let runs_root = tmp.path().join("runs");
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let result = a_result();
    let report = a_report(&profile, &result);
    let facts = BacktestRunFacts {
        profile: &profile,
        profile_path: "profiles/sma.toml",
        profile_toml: PROFILE_TOML,
        store: "the datahub at 127.0.0.1:7878",
        runs_root: &runs_root,
        data: None,
        fingerprint: None,
        build: None,
        started_at: 1_756_000_000,
        finished_at: 1_756_000_012,
    };

    let dir = persist_run(&facts, &result, &report).unwrap();
    let text = std::fs::read_to_string(dir.join(runs::MANIFEST_FILE)).unwrap();
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

    let (equity, stride) = runs::decimate(&curve, 0);
    assert!(equity.is_empty(), "decimate's own contract");
    assert_eq!(stride, 0, "...and the stride it reports for it");

    assert!(
        keep_at_stride(&ts, stride).is_empty(),
        "a timestamp vector kept at stride 0 while the equity was dropped is a MISALIGNED \
             document, written by this build"
    );
    assert!(keep_at_stride(&curve, stride).is_empty(), "and the same for a per-symbol curve");

    // The property those two add up to, asserted through the type that declares it.
    let series = runs::RunSeries {
        schema: runs::SERIES_SCHEMA,
        equity,
        equity_ts: keep_at_stride(&ts, stride),
        per_symbol_equity: vec![("BTCUSDT".to_string(), keep_at_stride(&curve, stride))],
        stride,
        source_len: curve.len(),
        diagnostics: runs::RunDiagnostics::default(),
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
    let store = DataFusionHist::open(&root).unwrap();
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();

    let label = vike_datahub_client::route::history_route(Some("<host>:7878")).label();
    let fp = collect_data_fingerprint(Some(&store as &dyn HistStore), &profile, &label).unwrap();

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
    let store = DataFusionHist::open(&root).unwrap();
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();

    let fp = collect_data_fingerprint(
        Some(&store as &dyn HistStore),
        &profile,
        &root.display().to_string(),
    )
    .unwrap();

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
    let profile = BacktestProfile::from_toml_str(&toml).unwrap();
    let held = vec![
        vike_data::SeriesId::grouped("book", "polymarket", "btc-5m"),
        vike_data::SeriesId::grouped("quote", "polymarket", "btc-5m"),
        // A different VENUE — the reader lists groups under `kind=…/venue=…`, so this one is
        // not in the union and must not be in the address.
        vike_data::SeriesId::grouped("trade", "bybit", "x"),
        // `bar` is `grouped: false` in `store_kind.rs` and `load_bars` reads the per-symbol
        // directory alone, so a `group=` leaf of that kind is not the bar lane's business.
        vike_data::SeriesId::grouped("bar", "polymarket", "never"),
    ];

    let ids = run_fingerprint::planned_series_ids(&profile, &held);

    assert!(ids.contains(&vike_data::SeriesId::grouped("book", "polymarket", "btc-5m")));
    assert!(ids.contains(&vike_data::SeriesId::grouped("quote", "polymarket", "btc-5m")));
    assert!(!ids.iter().any(|i| i.venue == "bybit"), "another venue's group: {ids:?}");
    assert!(!ids.iter().any(|i| i.kind == "bar"), "a bar lane is never grouped: {ids:?}");
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
    let profile = BacktestProfile::from_toml_str(&toml).unwrap();
    let held = vec![vike_data::SeriesId::grouped("quote", "binance", "g1")];

    let ids = run_fingerprint::planned_series_ids(&profile, &held);

    assert_eq!(
        ids.iter().filter(|i| i.group.as_deref() == Some("g1")).count(),
        1,
        "three symbols, one group, one row: {ids:?}"
    );
}

/// A bar profile takes no grouped series even when the store holds one of that kind — the
/// symmetry that keeps the tick fix from widening the bar lane's address.
#[test]
fn a_bar_profile_takes_no_grouped_series_even_when_the_store_holds_one() {
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
    let held = vec![vike_data::SeriesId::grouped("bar", "binance", "g1")];

    let ids = run_fingerprint::planned_series_ids(&profile, &held);

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
    let store = DataFusionHist::open(&root).unwrap();
    let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();

    let fp = collect_data_fingerprint(
        Some(&store as &dyn HistStore),
        &profile,
        &root.display().to_string(),
    )
    .unwrap();

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
