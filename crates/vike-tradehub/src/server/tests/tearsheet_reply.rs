use super::settings::SettingsShowSource;
use super::tearsheet::{TEARSHEET_DEFAULT_SEED, tearsheet_reply};
use vike_tradehub_client::proto::Response;

/// A source carrying exactly the journal the daemon resolved (`None` = journaling off) and which
/// resolves no project — the reply reads ONLY `journal`, so the other fields are inert here.
fn source(journal: Option<&std::path::Path>) -> SettingsShowSource {
    SettingsShowSource {
        settings_dir: None,
        hot: None,
        journal: journal.map(|d| vike_core::JournalConfig::at(d.to_path_buf())),
    }
}

fn error_text(r: Response) -> String {
    match r {
        Response::Error(m) => m,
        other => panic!("expected Response::Error, got {other:?}"),
    }
}

/// A server built through `serve(.., None)` cannot resolve its own journal, and says that
/// rather than reporting an empty tearsheet — the identity-less `StrategyStatus` rule.
#[test]
fn a_source_less_server_refuses_by_name() {
    let msg = error_text(tearsheet_reply(None, None, None));
    assert!(msg.contains("without a settings source"), "{msg}");
}

/// A node running with no journal says so, and names the ONE way to turn one on: the
/// `config.journal_dir` row (decision 0111, amended 2026-10-11) — never a run profile.
#[test]
fn no_resolved_journal_names_the_one_way_to_turn_one_on() {
    let msg = error_text(tearsheet_reply(Some(&source(None)), None, None));
    assert!(msg.contains("no command journal"), "{msg}");
    assert!(msg.contains("vike-cli config set config.journal_dir"), "the settings row: {msg}");
    assert!(!msg.contains("sinks.journal"), "a run profile no longer turns it on: {msg}");
}

/// A directory that is not there is an UNREADABLE journal, not an absent one: the operator
/// configured a path and the path is wrong, which is a different fact from "journaling is off"
/// and gets a different sentence (and names the path, so it can be fixed).
#[test]
fn a_missing_journal_directory_reports_the_read_failure_and_the_path() {
    let dir = std::env::temp_dir().join("vike-tradehub-tearsheet-absent-983471");
    let msg = error_text(tearsheet_reply(Some(&source(Some(&dir))), None, None));
    assert!(msg.contains("could not be read"), "{msg}");
    assert!(msg.contains("983471"), "…naming the directory it tried: {msg}");
}

/// An EMPTY journal directory is a journal with no fills in it — a zero-trade tearsheet, which
/// is an answer — and the reply is the pretty JSON spelling `tearsheet --json` prints, because
/// `remote_handle::tearsheet` hands this text to its caller verbatim.
#[test]
fn an_empty_journal_folds_to_a_zero_trade_tearsheet_in_pretty_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = source(Some(dir.path()));
    let Response::Tearsheet(json) = tearsheet_reply(Some(&src), None, None) else {
        panic!("an empty journal is an answer, not a refusal");
    };
    assert!(json.contains('\n'), "the payload is to_string_pretty, not compact: {json}");
    let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(doc["n_trades"], serde_json::json!(0));
    // Seed `None` ⇒ the LOCAL door's default, so the remote and local reports of one journal
    // cannot scale `total_return`/`cagr`/`sharpe` off different equity bases.
    assert_eq!(doc["final_equity"], serde_json::json!(TEARSHEET_DEFAULT_SEED));
}

/// Journal `fills` (symbol, side, price) as bare fill records of 100,000 units, the way a daemon writes
/// them, and close the journal so a reader can open it.
fn journal_of(dir: &std::path::Path, fills: &[(&str, i32, f64)]) {
    use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
    let cfg = vike_journal::JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut journal = vike_journal::CommandJournal::open(dir, cfg).expect("open the journal");
    for (i, (symbol, side, px)) in fills.iter().enumerate() {
        let n = i as i64;
        let fill = FillEvent {
            trade_id: TradeId::new(format!("t{n}")).expect("trade id"),
            client_order_id: format!("c{n}"),
            venue: "oanda".into(),
            symbol: (*symbol).into(),
            side: *side,
            last_qty: 100_000.0,
            last_px: *px,
            commission: 0.0,
            commission_asset: "USD".into(),
            liquidity_side: "taker".into(),
            ts: n + 1,
            mark_price: None,
            position_side: PositionSide::Both,
        };
        journal
            .append_cmd(1_000 + n, &vike_exec::Ingest::Event(Event::Fill(fill)))
            .expect("append a fill");
    }
}

/// **A journal holding a fill on a symbol the name says is quoted in another fiat currency is not folded
/// into a tearsheet** (decision 0183, item 13): the fold adds every fill's money as one currency and
/// holds no rate, so 100,000 USDJPY closed 0.10 higher would show as +10,000 where the truth is
/// 10,000 / 150.10 = 66.62 USD. The reply is a refusal that names the symbol and its currency.
#[test]
fn a_journal_with_a_foreign_quoted_fill_is_refused_in_words() {
    let dir = tempfile::tempdir().expect("tempdir");
    journal_of(dir.path(), &[("USD_JPY", 1, 150.0), ("USD_JPY", -1, 150.1)]);
    let src = source(Some(dir.path()));
    let msg = error_text(tearsheet_reply(Some(&src), None, None));
    assert!(msg.contains("USD_JPY (JPY)"), "names the symbol and its currency: {msg}");
    assert!(msg.contains("tearsheet"), "says what is refused: {msg}");
}

/// The same journal shape on dollar-quoted and crypto symbols folds as it always did: one round trip,
/// +1,000,000 on 100,000 units at a 10 move.
#[test]
fn a_dollar_quoted_journal_still_folds_to_a_tearsheet() {
    for symbol in ["EUR_USD", "BTCUSDT"] {
        let dir = tempfile::tempdir().expect("tempdir");
        journal_of(dir.path(), &[(symbol, 1, 100.0), (symbol, -1, 110.0)]);
        let src = source(Some(dir.path()));
        let Response::Tearsheet(json) = tearsheet_reply(Some(&src), None, None) else {
            panic!("{symbol}: a dollar-quoted journal is an answer, not a refusal");
        };
        let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(doc["n_trades"], serde_json::json!(1), "{symbol}");
    }
}

/// **The fills of a venue whose engine books off the dollar are left out of the sheet, and the sheet says so**
/// (decision 0183, item 13): a journal of a node with a dollar account beside a EUR one (the `Snap` records
/// the ledgers) holds EURUSD fills of both, and a euro account's price PnL is not dollars. The dollar venue's
/// round trip is the sheet, the EUR venue's is named in the sheet's `name`, and a journal that records no
/// off-dollar ledger folds every fill as before.
#[test]
fn the_fills_of_a_euro_ledger_venue_are_left_out_and_named() {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
    use vike_model::RiskLimits;
    use vike_model::events::{Event, FillEvent, PositionSide, TradeId};

    let engine = |venue: &str, ccy: &str| {
        let mut e = ExecutionEngine::new(
            Account::new(1.0, venue, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            venue,
            "EUR_USD",
        );
        e.set_account_ccy(ccy).expect("a currency code");
        e.snapshot_state()
    };
    let fill = |n: i64, venue: &str, side: i32, px: f64| FillEvent {
        trade_id: TradeId::new(format!("{venue}-{n}")).expect("trade id"),
        client_order_id: format!("c{venue}{n}"),
        venue: venue.into(),
        symbol: "EUR_USD".into(),
        side,
        last_qty: 100_000.0,
        last_px: px,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: "taker".into(),
        ts: n + 1,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = vike_journal::JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut journal = vike_journal::CommandJournal::open(dir.path(), cfg).expect("open");
    journal
        .append_snap(
            900,
            &[engine("oanda", "USD"), engine("fxcm", "EUR")],
            "sess",
            0,
            0,
            vike_journal::SnapBooks::default(),
            1,
        )
        .expect("append a snapshot");
    for (n, (venue, side, px)) in
        [("oanda", 1, 100.0), ("fxcm", 1, 1.10), ("oanda", -1, 110.0), ("fxcm", -1, 1.20)]
            .into_iter()
            .enumerate()
    {
        let ingest = vike_exec::Ingest::Event(Event::Fill(fill(n as i64, venue, side, px)));
        journal.append_cmd(1_000 + n as i64, &ingest).expect("append a fill");
    }
    drop(journal);

    let Response::Tearsheet(json) = tearsheet_reply(Some(&source(Some(dir.path()))), None, None)
    else {
        panic!("a mixed journal is an answer");
    };
    let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(doc["n_trades"], serde_json::json!(1), "the dollar venue's round trip alone");
    let name = doc["name"].as_str().expect("the sheet is named");
    assert!(name.contains("2 EUR-ledger fills of fxcm"), "names what it left out: {name}");
}

/// …and a caller WITH an opinion gets it: the wire's optionals are honoured, not clamped to the
/// defaults above.
#[test]
fn a_supplied_seed_reaches_the_fold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = source(Some(dir.path()));
    let Response::Tearsheet(json) = tearsheet_reply(Some(&src), Some(25_000.0), Some(365.0)) else {
        panic!("an empty journal is an answer, not a refusal");
    };
    let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(doc["final_equity"], serde_json::json!(25_000.0));
}
