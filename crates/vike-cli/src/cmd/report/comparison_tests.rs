use super::*;
use crate::cmd::backtest::read::selector::test_run_at;
use crate::cmd::report::comparison_schema::{ROW_KEYS, SIDE_KEYS, TOP_LEVEL_KEYS};
use vike_analytics::TradingRecord;

/// The results of a run whose equity went `equity`, named `name`.
fn results(name: &str, equity: Vec<f64>) -> TradingResults {
    let record = TradingRecord {
        final_equity: equity.last().copied().unwrap_or(0.0),
        equity_curve: equity,
        ..TradingRecord::default()
    };
    TradingResults::from_record(Some(name.to_string()), &record, 252.0)
}

/// The keys of a JSON object, sorted.
fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().expect("an object").keys().cloned().collect();
    k.sort();
    k
}

/// A key list, sorted.
fn sorted(k: &[&str]) -> Vec<String> {
    let mut k: Vec<String> = k.iter().map(|s| s.to_string()).collect();
    k.sort();
    k
}

/// **The document is the shape `report --schema --compare` publishes**: every top-level, side and row key, the
/// sides as the line named them, and each row's `b − a`.
#[test]
fn the_document_carries_exactly_the_keys_the_schema_publishes() {
    let comparison = TradingResultsComparison::of(
        results("a", vec![1_000.0, 1_100.0]),
        results("b", vec![1_000.0, 1_050.0]),
    );
    let note = "fees in other assets: 0.0123 BNB (not converted; left out of total_fees and the realized curve)";
    let named = [
        ComparisonSide { source: "run:x-1-0".to_string(), notes: Vec::new() },
        ComparisonSide {
            source: "account:binance/MAIN".to_string(),
            notes: vec![note.to_string()],
        },
    ];
    let doc = document(&named, &comparison);
    assert_eq!(keys(&doc), sorted(&TOP_LEVEL_KEYS));
    assert_eq!((doc["a"]["notes"].clone(), doc["b"]["notes"].clone()), (json!([]), json!([note])));
    assert_eq!(doc["document"], json!("vike-cli-report-comparison"));
    for side in ["a", "b"] {
        assert_eq!(keys(&doc[side]), sorted(&SIDE_KEYS), "{side}");
    }
    assert_eq!(
        (doc["a"]["source"].as_str(), doc["b"]["source"].as_str()),
        (Some("run:x-1-0"), Some("account:binance/MAIN"))
    );
    assert_eq!(doc["a"]["label"], json!("backtest a"), "the side's source_label()");
    let rows = doc["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), comparison.rows().len());
    for row in rows {
        assert_eq!(keys(row), sorted(&ROW_KEYS));
    }
    let final_equity = rows.iter().find(|r| r["metric"] == "final_equity").expect("a row");
    assert_eq!(final_equity["difference"], json!(-50.0), "b − a");
}

/// A live side's funding is not recorded, so its cell in the document is `null`, and so is the difference.
#[test]
fn a_live_sides_funding_is_null_in_the_document() {
    let live = TradingRecord {
        source: TradingRecordSource::Live {
            account: account_report::account_ref("binance", Default::default()).unwrap(),
            tier: vike_model::AccountTier::Demo,
        },
        final_equity: 1_050.0,
        equity_curve: vec![1_000.0, 1_050.0],
        ..TradingRecord::default()
    };
    let live = TradingResults::from_record(None, &live, 365.0);
    let comparison = TradingResultsComparison::of(results("a", vec![1_000.0, 1_100.0]), live);
    let side = |source: &str| ComparisonSide { source: source.to_string(), notes: Vec::new() };
    let doc = document(&[side("run:a"), side("account:binance/DEFAULT")], &comparison);
    let rows = doc["rows"].as_array().expect("rows");
    let funding = rows.iter().find(|r| r["metric"] == "funding_paid").expect("the row");
    assert!(funding["a"].is_number(), "{funding}");
    assert!(funding["b"].is_null() && funding["difference"].is_null(), "{funding}");
}

/// **A stored backtest is its own report**, typed, with the period of its stored curve and that curve's first
/// sample as the capital a live side aligns to, labelled as a backtest. A report that is not one result (a
/// walk-forward run's steps) is refused by name, never read as zeros.
#[test]
fn a_stored_backtest_reads_its_report_its_period_and_its_capital() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a-1-0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("report.json"),
        r#"{"name":"demo","final_equity":10500.0,"total_return":0.05,"n_trades":3,
            "win_rate":0.66,"sharpe":1.25,"max_drawdown":0.031,"profit_factor":2.0}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("series.json"),
        r#"{"schema":1,"equity":[10000.0,10500.0],"equity_timestamp":[1000,5000],"stride":1,"source_len":2}"#,
    )
    .unwrap();
    let mut run = test_run_at("a-1-0", "backtest");
    run.dir = dir.clone();
    run.report = Some(dir.join("report.json"));

    let Ok(Side::Stored { run_id, results, first_equity, notes, .. }) = stored_side(&run) else {
        panic!("a stored backtest is a stored side")
    };
    assert_eq!(run_id, "a-1-0");
    // No stored profile: the bar width is unknown, so the period ends at the last OPEN, and says so.
    assert_eq!(results.period, Some(TradingPeriod { from_ms: 1000, to_ms: 5000 }));
    assert!(notes[0].contains("ends at its last bar's OPEN"), "{notes:?}");
    assert_eq!(notes[1], "starting capital 10000 (its stored curve's first equity)");
    assert_eq!(first_equity, Some(10_000.0));
    let label = results.source_label().expect("a backtest source");
    assert_eq!(label, "backtest demo 1970-01-01 00:00 – 1970-01-01 00:00 UTC");

    // The reviewer's scenario: 1h bars opening at 1000 and 5000. The last bar CLOSES an hour after it
    // opens, so the period (and an account aligned to it) runs to 5000 + 1h − 1 ms, and the label says so.
    let profile =
        "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\ninterval = \"1h\"\n";
    std::fs::write(dir.join("config.toml"), profile).unwrap();
    let Ok(Side::Stored { results, notes, .. }) = stored_side(&run) else {
        panic!("a stored side")
    };
    assert_eq!(results.period, Some(TradingPeriod { from_ms: 1000, to_ms: 3_605_000 - 1 }));
    assert_eq!(notes.len(), 1, "the close is known: {notes:?}");
    let label = results.source_label().expect("a backtest source");
    assert_eq!(label, "backtest demo 1970-01-01 00:00 – 1970-01-01 01:00 UTC");
    // A tick run's last sample is its last event.
    std::fs::write(dir.join("config.toml"), profile.replace("\"bar\"", "\"tick\"")).unwrap();
    let Ok(Side::Stored { results, .. }) = stored_side(&run) else { panic!("a stored side") };
    assert_eq!(results.period, Some(TradingPeriod { from_ms: 1000, to_ms: 5000 }));
    std::fs::remove_file(dir.join("config.toml")).unwrap();

    // The reviewer's scenario: no series.json. The side says exactly what it lacks, and keeps no period
    // and no capital rather than inventing one.
    std::fs::remove_file(dir.join("series.json")).unwrap();
    let Ok(Side::Stored { results, first_equity, gap, notes, .. }) = stored_side(&run) else {
        panic!("a stored side")
    };
    let lacks = "it stored no series.json, so its period and starting capital are unknown";
    assert_eq!((results.period, first_equity, gap.as_deref()), (None, None, Some(lacks)));
    assert_eq!(notes, [lacks]);

    std::fs::write(dir.join("report.json"), r#"{"steps":[],"overall":null}"#).unwrap();
    let Err(e) = stored_side(&run) else { panic!("a walk-forward report is no one result") };
    assert!(e.message.contains("a-1-0 cannot be compared"), "{}", e.message);

    run.report = None;
    let Err(e) = stored_side(&run) else { panic!("no report, nothing to compare") };
    assert!(e.message.contains("has no report.json"), "{}", e.message);
}

/// An account aligns to a stored run's period, else to a forward run's window; an account names none.
#[test]
fn the_reference_period_is_a_runs() {
    let forward = Side::Forward {
        run_id: "f-1-0".to_string(),
        query: LiveQuery {
            account: account_report::account_ref("binance", Default::default()).unwrap(),
            window: TimestampRange::of(10, 20),
            mount_id: Some("m".to_string()),
            title: None,
        },
    };
    assert_eq!(forward.reference_period(), Some(TradingPeriod { from_ms: 10, to_ms: 20 }));
    let mut stored = results("demo", vec![1.0]);
    stored.period = Some(TradingPeriod { from_ms: 1, to_ms: 2 });
    let stored = Side::Stored {
        run_id: "a".to_string(),
        results: Box::new(stored),
        first_equity: None,
        gap: None,
        notes: Vec::new(),
    };
    assert_eq!(stored.reference_period(), Some(TradingPeriod { from_ms: 1, to_ms: 2 }));
    let account = Side::Account(LiveQuery {
        account: account_report::account_ref("binance", Default::default()).unwrap(),
        window: TimestampRange::all(),
        mount_id: None,
        title: None,
    });
    assert_eq!(account.reference_period(), None);
}

/// **The live sides' capital, and where it came from**: `--cash`, else the stored side's first equity (named by
/// its run), else the default; each side says it.
#[test]
fn the_live_sides_capital_is_said_with_its_source() {
    let line = |extra: &[&str]| {
        let argv = ["--compare", "run:a-1-0", "account:binance"].iter().chain(extra);
        super::super::parse(argv.map(|s| s.to_string())).unwrap()
    };
    let stored = |first_equity| Side::Stored {
        run_id: "a-1-0".to_string(),
        results: Box::new(results("demo", vec![1.0])),
        first_equity,
        gap: None,
        notes: Vec::new(),
    };
    let fold = aligned_fold(&line(&[]), &[stored(Some(9_000.0))]);
    assert_eq!(
        (fold.cash, fold.cash_from.as_str()),
        (9_000.0, "run:a-1-0's stored curve's first equity")
    );
    let fold = aligned_fold(&line(&["--cash", "5000"]), &[stored(Some(9_000.0))]);
    assert_eq!((fold.cash, fold.cash_from.as_str()), (5_000.0, "--cash"));
    let fold = aligned_fold(&line(&[]), &[stored(None)]);
    assert_eq!(
        (fold.cash, fold.cash_from.as_str()),
        (vike_analytics::results::DEFAULT_REPORT_SEED, "the default; --cash sets it")
    );
}

/// **A live side is never aligned to what a stored run cannot give**: with no series.json the run has no
/// period and no starting equity, so an account side with no `--from`/`--to` is refused (never read over all
/// of history), and a live side with no `--cash` too (never folded from the default); each names what the run
/// lacks. With both named, the side aligns to them.
#[test]
fn a_side_the_stored_run_cannot_align_is_refused_in_words() {
    let line = |extra: &[&str]| {
        let argv = ["--compare", "run:a-1-0", "account:binance"].iter().chain(extra);
        super::super::parse(argv.map(|s| s.to_string())).unwrap()
    };
    let lacks = "it stored no series.json, so its period and starting capital are unknown";
    let sides = [
        Side::Stored {
            run_id: "a-1-0".to_string(),
            results: Box::new(results("demo", vec![1.0])),
            first_equity: None,
            gap: Some(lacks.to_string()),
            notes: vec![lacks.to_string()],
        },
        Side::Account(LiveQuery {
            account: account_report::account_ref("binance", Default::default()).unwrap(),
            window: TimestampRange::all(),
            mount_id: None,
            title: None,
        }),
    ];
    let e = refuse_an_unalignable_side(&line(&[]), &sides).expect_err("no period to align to");
    assert!(e.message.contains("cannot be aligned to run:a-1-0"), "{}", e.message);
    assert!(e.message.contains(lacks) && e.message.contains("--from/--to"), "{}", e.message);
    let e = refuse_an_unalignable_side(&line(&["--from", "1", "--to", "2"]), &sides)
        .expect_err("no capital to align to");
    let capital = e.message.contains("starting capital") && e.message.contains("--cash");
    assert!(capital, "{}", e.message);
    let named = line(&["--from", "1", "--to", "2", "--cash", "5000"]);
    assert!(refuse_an_unalignable_side(&named, &sides).is_ok());
}
