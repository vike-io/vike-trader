//! End-to-end CLI gate for `vike-backend report`: a real `spawn_core` session journals two
//! round-trips (one win, one loss — so every metric stays finite, incl. `profit_factor`), then the
//! verb is invoked over that journal directory and its stdout is asserted for both
//! the human table and `--json` output. Also pins the missing-`--journal` usage error (exit 2).
//!
//! ⚠ Whole-file gated on the `tearsheet` feature, and the gate is load-bearing in two ways at once:
//! `crates/vike/src/main.rs`'s `TOOLS` carries the `report` row only under it, so a feature-off
//! binary answers the verb with its tool list and exit 2; and the `serde_json` this file parses with
//! is that feature's optional dependency. The crate's self dev-dependency turns `full`, and so
//! `tearsheet`, on in every test build, which is what runs it.
//!
//! This file moved here from `vike-report` on 2026-10-11 with the CLI it spawns.
#![cfg(feature = "tearsheet")]

use std::process::Command as OsCommand;

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::testing::TestExecutionClient;
use vike_execution::{Account, BalanceMode, Command, Engine, OrderIntent, RiskGate};
use vike_model::OrderRequest;
use vike_model::RiskLimits;

fn order(order_client_id: &str, symbol: &str, side: i32, px: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: symbol.into(),
        side,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(px),
        ..Default::default()
    }
}

/// Write a journal with two closed round-trips: +10 (buy 100 → sell 110) then -10 (buy 110 →
/// sell 100). One win + one loss keeps `gross_loss` non-zero so `profit_factor` (and thus `--json`)
/// is finite.
///
/// Returns the journal directory **and the guard that owns it**, and the guard must be BOUND by the
/// caller for as long as the directory is wanted. Returning the path alone would drop the `TempDir`
/// at the end of this function and delete the journal before [`report`] ever
/// opened it — the verb would then be handed an empty directory on every run.
///
/// ⚠ It used to return the path alone — a pid-keyed directory under the OS temp directory, with a
/// `remove_dir_all` at each end of the test. That cleans up only when the test
/// PASSES, and what it left behind on a failure was a `posix_fallocate`d journal segment
/// (`crates/vike-journal/src/segment.rs`'s `reserve_blocks`) — the shape that put 211 GB on
/// the CI box's `/tmp`. It was also invisible to `crates/vike-ops/tests/hygiene/journal_scratch_gate.rs`'s tree
/// rule, which matches create needles in the SAME file while the directory here is created by a
/// callee (`crates/vike-journal/src/writer.rs`'s `Journal::open`); that blind spot is
/// declared and driven there now, and this file is one of the two that measured it. `tempfile` was
/// already a declared, unused dev-dependency of this crate for exactly this pair of tests.
fn build_two_trade_journal() -> (tempfile::TempDir, std::path::PathBuf) {
    build_two_trade_journal_on("BTCUSDT")
}

/// [`build_two_trade_journal`] over the engine symbol `symbol`.
fn build_two_trade_journal_on(symbol: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("scratch dir");
    // NOT the tempdir root: `Journal::open` creating the directory and its segments is part
    // of what this harness exercises, so it must not exist yet.
    let dir = tmp.path().join("journal");

    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::default()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        symbol,
    );
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(vike_model::ClockTest::new(1_000)),
        order_client_id_session: Some(("deadbeef".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.clone(),
            file: vike_journal::JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 1_000,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    for (order_client_id, side, px) in
        [("b1", 1, 100.0), ("s1", -1, 110.0), ("b2", 1, 110.0), ("s2", -1, 100.0)]
    {
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(order(
            order_client_id,
            symbol,
            side,
            px,
        )))));
    }
    handle.shutdown_and_join();
    (tmp, dir)
}

/// The shipped verb, `vike-backend report`, with its arguments still to come.
fn report() -> OsCommand {
    let mut cmd = OsCommand::new(env!("CARGO_BIN_EXE_vike-backend"));
    cmd.arg("report");
    cmd
}

/// `--help` calls the program what the operator typed, and the old flag is not read.
#[test]
fn report_help_names_the_verb_and_seed_is_no_flag() {
    let out = report().arg("--help").output().expect("run vike-backend report --help");
    let help = String::from_utf8(out.stdout).unwrap().to_ascii_lowercase();
    assert!(help.contains("usage: report"), "{help}");
    assert!(help.contains("--cash"), "{help}");
    assert!(!help.contains("usage: tearsheet"), "{help}");
    let out = report().args(["--journal", "x", "--seed", "5000"]).output().expect("run");
    assert_eq!(out.status.code(), Some(2), "--seed is no longer a flag");
}

#[test]
fn report_prints_table_and_json() {
    // ⚠ `_scratch` is BOUND, not discarded. Binding it to a bare `_` drops the guard immediately
    // and deletes the journal before the binary below is spawned.
    let (_scratch, dir) = build_two_trade_journal();

    // --- default human table ---
    let out = report()
        .args(["--journal", dir.to_str().unwrap(), "--name", "cli-test"])
        .output()
        .expect("run vike-backend report");
    assert!(out.status.success(), "report exited non-zero: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Live Tearsheet: cli-test"), "labeled header missing:\n{stdout}");
    // ⚠ The row label is the CATALOG ID now, and so is its column width. This used to read
    // `"trades:              2"` — a label and a padding the table hand-typed. Both changed when
    // `Tearsheet` stopped keeping its own roster: the id is `n_trades` (the string an operator
    // types at `--metrics` and the key the JSON carries) and the column is as wide as the widest
    // catalog id. So the assertion names the id and the value and not the whitespace between them,
    // which is presentation and would re-break on the next catalog addition.
    let n_trades_row = stdout
        .lines()
        .find(|l| l.starts_with("n_trades:"))
        .unwrap_or_else(|| panic!("no n_trades row:\n{stdout}"));
    assert!(n_trades_row.ends_with(" 2"), "expected 2 trades, got {n_trades_row:?}");
    assert!(stdout.contains("net_profit:"), "net_profit row missing:\n{stdout}");
    assert!(stdout.contains("sharpe:"), "sharpe row missing:\n{stdout}");
    // The thirteen metrics the old hand-typed field list discarded are on the table now — `omega`
    // stands for the set, because it was computed by every run and printed by nothing.
    assert!(stdout.contains("omega:"), "the widened roster renders:\n{stdout}");
    // ...and the document declares its version where a human can see it.
    assert!(stdout.contains("(schema 2)"), "the header carries the schema:\n{stdout}");

    // --- --json ---
    let out = report()
        .args(["--journal", dir.to_str().unwrap(), "--json"])
        .output()
        .expect("run vike-backend report --json");
    assert!(out.status.success(), "--json exited non-zero: {:?}", out.status);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    // ⚠ `2`, not `2.0`. `n_trades` was a `usize` field and is a catalog `MetricUnit::Count` slot
    // now; `serde_json::Value`'s `PartialEq` does not consider the two equal, so this assertion is
    // the wire half of `TearsheetMetricValues`' integer-for-Count serialization and fails if that is lost.
    assert_eq!(json["n_trades"], 2);
    // net profit = +10 - 10 = 0 over the two round-trips
    assert_eq!(json["net_profit"].as_f64().unwrap(), 0.0);
    assert!(json["profit_factor"].as_f64().unwrap().is_finite(), "profit_factor must be finite");
    // The document is FLAT and NAMES ITSELF — the two properties a machine consumer branches on.
    assert_eq!(json["schema"], 2, "the live tearsheet declares its shape version");
    assert!(json.get("omega").is_some(), "a widened-roster key is on the wire");
    assert!(
        json.as_object().expect("an object").values().all(|v| !v.is_object()),
        "the flat shape is the contract: {json}"
    );

    // …and no `remove_dir_all` here: dropping `_scratch` removes the tree, on the panic path too.
}

/// **A journal holding fills on a symbol the name says is quoted in another fiat currency is not folded**
/// (decision 0183, item 13): the fold adds every fill's money as one currency and holds no rate, so
/// yen would print as dollars. The tool exits non-zero, names the symbol and its currency on stderr and
/// prints no sheet; a dollar-quoted journal is the test above, unchanged.
#[test]
fn report_refuses_a_journal_of_fills_in_another_fiat_currency() {
    let (_scratch, dir) = build_two_trade_journal_on("USDJPY");
    let out = report()
        .args(["--journal", dir.to_str().unwrap()])
        .output()
        .expect("run vike-backend report");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a yen-quoted journal must be refused; it printed:\n{stdout}");
    assert!(stderr.contains("USDJPY (JPY)"), "names the symbol and its currency:\n{stderr}");
    assert!(stdout.is_empty(), "and prints no sheet:\n{stdout}");
}

/// **The local door leaves out and names a contract the sheet cannot value, as the node's does** (decision 0243,
/// item 6): 1,000 USD of inverse `BTC-PERPETUAL` from 60,000 to 66,000 would read +6,000,000 where the account booked
/// +100. stderr and the sheet's `name` say what went, and the sheet holds no trade.
#[test]
fn report_leaves_out_and_names_an_inverse_contract() {
    use vike_execution::testing::RecordingClient;
    use vike_model::ContractValue;
    use vike_model::events::{Event, FillEvent, PositionSide, TradeId};

    let symbol = "BTC-PERPETUAL";
    let grid = [(symbol.to_string(), ContractValue::inverse(1.0))].into_iter().collect();
    let engine = Engine::new(
        Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        symbol,
    )
    .snapshot_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = vike_journal::JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut journal = vike_journal::Journal::open(dir.path(), config).expect("open");
    let books = vike_journal::JournalSnapshotParts::default();
    journal.append_snap(900, &[engine], "sess", 0, 0, books, 1).expect("append a snapshot");
    for (n, (side, px)) in [(1, 60_000.0), (-1, 66_000.0)].into_iter().enumerate() {
        let fill = FillEvent {
            trade_id: TradeId::new(format!("t{n}")).expect("trade id"),
            order_client_id: format!("c{n}"),
            venue: "deribit".into(),
            symbol: symbol.into(),
            side,
            last_qty: 1_000.0,
            last_price: px,
            commission: 0.0,
            commission_asset: "".into(),
            liquidity_side: "taker".into(),
            timestamp: n as i64 + 1,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        let ingest = vike_execution::Ingest::Event(Event::Fill(fill));
        journal.append_command(1_000 + n as i64, &ingest).expect("append a fill");
    }
    drop(journal);

    let out = report()
        .args(["--journal", dir.path().to_str().unwrap(), "--json"])
        .output()
        .expect("run vike-backend report");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "a sheet that left fills out is an answer:\n{stderr}");
    let named = "2 fills of deribit BTC-PERPETUAL (inverse, 1 a unit)";
    assert!(stderr.contains(named), "{stderr}");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(doc["n_trades"], serde_json::json!(0), "{doc}");
    assert!(doc["name"].as_str().expect("the sheet is named").contains(named), "{doc}");
}

#[test]
fn report_requires_journal_flag() {
    // No --journal → usage error, exit code 2. No variable stands in for it (decision 0111).
    let out = report().output().expect("run vike-backend report with no args");
    assert_eq!(out.status.code(), Some(2), "missing --journal must exit 2");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("--journal"), "usage message must name --journal:\n{stderr}");
}

/// ⚠ **The POSITIVE argv pins of decision 0155 for the `report` tool**, written against the
/// hand-rolled readers and run unchanged against `clap`'s: every spelling the old reader accepted
/// still produces the same answer. Spaced and inline, each flag the tool takes.
#[test]
fn report_accepts_every_flag_spelling_it_accepted() {
    let (_scratch, dir) = build_two_trade_journal();
    let journal = dir.to_str().unwrap();
    let html_dir = tempfile::tempdir().expect("scratch dir for the html");
    let html = html_dir.path().join("sheet.html");
    let html = html.to_str().unwrap();

    let inline_journal = format!("--journal={journal}");
    let inline_html = format!("--html={html}");
    let spaced: Vec<&str> = vec![
        "--journal",
        journal,
        "--cash",
        "5000",
        "--name",
        "pins",
        "--periods-per-year",
        "365",
        "--json",
        "--html",
        html,
        "--mtm",
    ];
    let inline: Vec<&str> = vec![
        inline_journal.as_str(),
        "--cash=5000",
        "--name=pins",
        "--periods-per-year=365",
        "--json",
        inline_html.as_str(),
    ];
    for args in [spaced, inline] {
        let _ = std::fs::remove_file(html);
        let out = report().args(&args).output().expect("run vike-backend report");
        assert!(
            out.status.success(),
            "{args:?} exited {:?}; stderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
        assert_eq!(json["n_trades"], 2, "{args:?}");
        assert!(std::path::Path::new(html).exists(), "{args:?} must write the --html file");
    }

    // `--store` is refused BY NAME, in both spellings.
    for store in [vec!["--store", "/srv/hist"], vec!["--store=/srv/hist"]] {
        let out = report()
            .args(["--journal", journal])
            .args(&store)
            .output()
            .expect("run vike-backend report");
        assert_eq!(out.status.code(), Some(2), "{store:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.contains("no longer reads history locally"), "{store:?}: {stderr}");
    }
}
