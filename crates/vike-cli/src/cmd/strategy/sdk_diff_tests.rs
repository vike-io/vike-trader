use vike_analytics::TradingRecord;

use super::*;
use crate::cmd::strategy::{Sub, parse};

/// A canned report: a run whose equity went 1000 → 1050 → 1100, with `trades` as its closed-trade rows and
/// `sdk` as the SDK it records.
fn report(sdk: u32, trades: &[(f64, f64)]) -> Value {
    let record = TradingRecord {
        final_equity: 1_100.0,
        equity_curve: vec![1_000.0, 1_050.0, 1_100.0],
        ..TradingRecord::default()
    };
    let results = TradingResults::from_record(Some("lab".to_string()), &record, 252.0);
    let mut report = serde_json::to_value(results).expect("a report serializes");
    let rows: Vec<Value> = trades
        .iter()
        .map(|(entry, exit)| json!({ "symbol": "BTCUSDT", "entry_price": entry, "exit_price": exit }))
        .collect();
    report["closed_trades"] = json!({ "trades": rows });
    report[RECORDED_SDK] = json!(sdk);
    report
}

const TRADES: [(f64, f64); 3] = [(100.0, 110.0), (110.0, 105.0), (105.0, 120.0)];

fn side(sdk: u32, report: Value) -> Side {
    Side { sdk, report }
}

#[test]
fn two_identical_results_are_identical_and_exit_zero() {
    let c = compare(&side(3, report(3, &TRADES)), &side(4, report(4, &TRADES))).expect("compared");
    assert!(c.identical(), "only the recorded SDK differs, and it is left out");
    assert_eq!(c.trades, TradesDiff { from: Some(3), to: Some(3), first: None });
    assert!(c.metrics.rows().iter().all(|r| r.a == r.b), "every metric the same");
    let verdict = Verdict::Compared(Box::new(c));
    assert_eq!(rung([&verdict]), Exit::Ok);
    assert_eq!(one_line("trend/a.rs", &verdict), "identical  trend/a.rs");
    assert_eq!(file_document("trend/a.rs", 3, 4, &verdict)["verdict"], "identical");
}

#[test]
fn a_different_trade_is_found_and_breaches() {
    let mut moved = TRADES;
    moved[1].1 = 104.5;
    let c = compare(&side(3, report(3, &TRADES)), &side(4, report(4, &moved))).expect("compared");
    assert!(!c.identical());
    assert_eq!(c.trades, TradesDiff { from: Some(3), to: Some(3), first: Some(1) });
    assert!(c.fields.is_empty(), "the trade rows are summarized, not listed as fields");
    assert_eq!(c.trade_leaves, 1, "one leaf moved: trade [1]'s exit price");

    let mut one_more = TRADES.to_vec();
    one_more.push((120.0, 121.0));
    let c =
        compare(&side(3, report(3, &TRADES)), &side(4, report(4, &one_more))).expect("compared");
    assert_eq!(c.trades, TradesDiff { from: Some(3), to: Some(4), first: Some(3) });
    let verdict = Verdict::Compared(Box::new(c));
    assert_eq!(rung([&verdict]), Exit::Breach);
    assert!(one_line("trend/a.rs", &verdict).starts_with("DIFFERENT  trend/a.rs"));
    let doc = file_document("trend/a.rs", 3, 4, &verdict);
    assert_eq!(doc["verdict"], "different");
    assert_eq!(doc["trades"]["first_difference"], 3);
}

#[test]
fn a_different_metric_is_listed_side_by_side_and_breaches() {
    let mut moved = report(4, &TRADES);
    let sharpe = moved["sharpe"].as_f64().expect("a sharpe");
    moved["sharpe"] = json!(sharpe + 0.25);
    let c = compare(&side(3, report(3, &TRADES)), &side(4, moved)).expect("compared");
    assert!(!c.identical());
    assert_eq!(c.trades.first, None, "the trades are the same");
    let keys: Vec<&str> = c.fields.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["sharpe"]);
    let row = c.metrics.rows().into_iter().find(|r| r.spec.id == "sharpe").expect("a sharpe row");
    assert_eq!((row.a, row.b), (Some(sharpe), Some(sharpe + 0.25)));
    assert_eq!(c.metrics.a.name.as_deref(), Some("sdk 3"), "each side is named by its SDK");
    let doc = file_document("trend/a.rs", 3, 4, &Verdict::Compared(Box::new(c)));
    assert_eq!(doc["changed"][0]["key"], "sharpe");
    assert_eq!(doc["metrics"]["a"]["source"], "sdk 3");
}

/// **Two runs that traded nothing compared nothing**: identical flat reports are NOT "unchanged across SDKs".
/// They warn and exit 7, alone and in a batch beside identical files; a difference without a trade still
/// breaches; a lot held open to the end is a trade.
#[test]
fn identical_runs_with_no_trade_on_either_sdk_warn_and_exit_seven() {
    let idle = compare(&side(3, report(3, &[])), &side(4, report(4, &[]))).expect("compared");
    assert!(idle.identical() && !idle.traded() && idle.compared_nothing());
    let idle = Verdict::Compared(Box::new(idle));
    assert_eq!(rung([&idle]), Exit::Empty);
    assert_eq!(Exit::Empty as u8, 7, "the number a script branches on");
    assert_eq!(one_line("trend/idle.rs", &idle), format!("NO TRADES  trend/idle.rs  {NO_TRADES}"));
    let doc = file_document("trend/idle.rs", 3, 4, &idle);
    assert_eq!(
        (doc["verdict"].as_str(), doc["warning"].as_str()),
        (Some("no_trades"), Some(NO_TRADES))
    );

    let traded = || {
        Verdict::Compared(Box::new(
            compare(&side(3, report(3, &TRADES)), &side(4, report(4, &TRADES))).expect("compared"),
        ))
    };
    assert!(
        file_document("trend/a.rs", 3, 4, &traded())["warning"].is_null(),
        "no warning when it traded"
    );
    assert_eq!(
        rung([&traded(), &idle]),
        Exit::Empty,
        "a batch with an idle file did not compare it"
    );
    assert!(
        counts_line(&[("a".into(), traded()), ("b".into(), idle)]).contains("1 with no trades")
    );

    let mut moved = report(4, &[]);
    moved["final_equity"] = json!(1_099.0);
    let moved = compare(&side(3, report(3, &[])), &side(4, moved)).expect("compared");
    assert!(!moved.compared_nothing(), "a difference is a finding, trades or not");
    assert_eq!(rung([&Verdict::Compared(Box::new(moved))]), Exit::Breach);

    let held = || {
        let mut held = report(3, &[]);
        held["closed_trades"]["open_trades"] = json!([{ "symbol": "BTCUSDT", "qty": 1.0 }]);
        held
    };
    let mut held_four = held();
    held_four[RECORDED_SDK] = json!(4);
    let held = compare(&side(3, held()), &side(4, held_four)).expect("compared");
    assert!(held.traded(), "a lot held open to the end is the strategy's logic");
    assert_eq!(rung([&Verdict::Compared(Box::new(held))]), Exit::Ok);
}

#[test]
fn a_result_that_does_not_record_its_sdk_fails_the_file() {
    let mut unstamped = report(4, &TRADES);
    unstamped.as_object_mut().expect("an object").remove(RECORDED_SDK);
    let err = compare(&side(3, report(3, &TRADES)), &side(4, unstamped)).err().expect("refused");
    assert!(err.contains("records no SDK"), "{err}");
    let err =
        compare(&side(3, report(3, &TRADES)), &side(4, report(3, &TRADES))).err().expect("refused");
    assert!(err.contains("asked for SDK 4, and the result records SDK 3"), "{err}");
}

#[test]
fn the_rung_is_failed_over_different_over_identical_and_empty_when_nothing_ran() {
    let same = || {
        Verdict::Compared(Box::new(
            compare(&side(3, report(3, &TRADES)), &side(4, report(4, &TRADES))).expect("compared"),
        ))
    };
    let moved = Verdict::Compared(Box::new(
        compare(&side(3, report(3, &TRADES)), &side(4, report(4, &TRADES[..2]))).expect("compared"),
    ));
    let failed = Verdict::Failed("sdk 4: error[E0425]: cannot find value `x`".to_string());
    assert_eq!(rung([] as [&Verdict; 0]), Exit::Empty);
    assert_eq!(rung([&same(), &same()]), Exit::Ok);
    assert_eq!(rung([&same(), &moved, &same()]), Exit::Breach);
    assert_eq!(rung([&moved, &failed, &same()]), Exit::Failed);
    assert_eq!(Exit::Breach as u8, 6, "the number a script branches on");
    assert!(one_line("trend/b.rs", &failed).starts_with("FAILED     trend/b.rs  sdk 4:"));
}

#[test]
fn the_command_line_parses() {
    let argv = |args: &[&str]| parse(args.iter().map(|s| (*s).to_string()));
    let Ok(Sub::SdkDiff(cmd)) =
        argv(&["sdk-diff", "trend/a.rs", "--from", "3", "--to", "4", "--profile", "lab.toml"])
    else {
        panic!("parses")
    };
    assert_eq!(
        (cmd.target.as_deref(), cmd.from, cmd.to, cmd.profile.as_str(), cmd.all, cmd.json),
        (Some("trend/a.rs"), 3, 4, "lab.toml", false, false)
    );
    let Ok(Sub::SdkDiff(cmd)) =
        argv(&["sdk-diff", "--all", "--from", "3", "--to", "4", "--profile", "p", "--json"])
    else {
        panic!("--all needs no FILE")
    };
    assert!(cmd.all && cmd.json && cmd.target.is_none());
    let refused: [&[&str]; 5] = [
        &["sdk-diff", "--from", "3", "--to", "4", "--profile", "p"],
        &["sdk-diff", "a.rs", "--to", "4", "--profile", "p"],
        &["sdk-diff", "a.rs", "--from", "3", "--to", "4"],
        &["sdk-diff", "a.rs", "--from", "three", "--to", "4", "--profile", "p"],
        &[],
    ];
    for refused in refused {
        assert!(argv(refused).is_err(), "{refused:?} parsed");
    }
}

/// The refusals before the dial, each on its own rung: nothing reaches a backtesthub.
#[test]
fn a_command_line_that_cannot_run_is_refused_on_its_rung() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let single = dir.path().join("single.toml");
    std::fs::write(&single, "[data]\nvenue = \"binance\"\n[engine]\ncash = 1000.0\n")
        .expect("write");
    let grid = dir.path().join("grid.toml");
    std::fs::write(&grid, "[engine]\ncash = 1000.0\n[paramscan]\nfast = [1, 2]\n").expect("write");
    let exit = |profile: &std::path::Path, from: &str, to: &str, addr: &str| {
        let profile = profile.to_str().expect("utf-8").to_string();
        let args = [
            "sdk-diff",
            "a.rs",
            "--from",
            from,
            "--to",
            to,
            "--profile",
            profile.as_str(),
            "--addr",
            addr,
        ];
        let Ok(Sub::SdkDiff(cmd)) = parse(args.iter().map(|s| (*s).to_string())) else {
            panic!("parses")
        };
        execute(&cmd, None, None).expect_err("refused").exit
    };
    assert_eq!(exit(&single, "3", "3", "127.0.0.1:1"), Exit::Usage, "one SDK twice");
    assert_eq!(exit(&dir.path().join("absent.toml"), "3", "4", "127.0.0.1:1"), Exit::Failed);
    assert_eq!(exit(&grid, "3", "4", "127.0.0.1:1"), Exit::Usage, "a grid is no one backtest");
    assert_eq!(exit(&single, "3", "4", "127.0.0.1:1"), Exit::Connect, "no backtesthub there");
}

#[test]
fn each_side_runs_the_profile_with_its_file_and_its_sdk() {
    let base: toml::Value =
        toml::from_str("[engine]\ncash = 1000.0\n[strategy]\nfile = \"x.rs\"\nsdk = 9\n")
            .expect("toml");
    let text = profile_for(&base, "trend/a.rs", 4).expect("built");
    let built: toml::Value = toml::from_str(&text).expect("toml");
    assert_eq!(built["strategy"]["file"].as_str(), Some("trend/a.rs"));
    assert_eq!(built["strategy"]["sdk"].as_integer(), Some(4));
    assert_eq!(built["engine"]["cash"].as_float(), Some(1000.0), "the rest is the profile's");
}

#[test]
fn all_takes_the_rust_files_under_the_folder() {
    let roster: Vec<String> =
        ["trend/a.rs", "trend/b.rhai", "trend/deep/c.rs", "trendy/d.rs", "e.rs"]
            .map(String::from)
            .into();
    assert_eq!(rust_files_under(&roster, Some("trend/")), ["trend/a.rs", "trend/deep/c.rs"]);
    assert_eq!(rust_files_under(&roster, Some(".")).len(), 4);
    assert_eq!(rust_files_under(&roster, None).len(), 4);
    assert!(rust_files_under(&roster, Some("none")).is_empty());
}
