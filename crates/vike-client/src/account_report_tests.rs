use super::*;
use vike_data::HistoryStoreMemory;
use vike_data::exec_log::{OWNERSHIP_FOREIGN, OWNERSHIP_OURS};
use vike_model::AccountTier;

/// A stored fill of binance `symbol`: one unit at `px`, booked on `account`, with `ownership`.
fn row(
    trade_id: &str,
    symbol: &str,
    side: i32,
    px: f64,
    ts: i64,
    account: Option<&str>,
    ownership: Option<&str>,
) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: trade_id.to_string(),
        order_client_id: format!("c-{trade_id}"),
        venue: "binance".to_string(),
        symbol: symbol.to_string(),
        side,
        qty: 1.0,
        px,
        commission: 0.0,
        mark_price: None,
        liquidity_side: "taker".to_string(),
        commission_asset: "USDT".to_string(),
        venue_order_id: None,
        account: account.map(str::to_string),
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: None,
        received_ms: None,
        ownership: ownership.map(str::to_string),
        journal_sequence: None,
        journal_generation: None,
    }
}

/// The account `binance/MAIN`, over every instant.
fn main_account() -> LiveQuery {
    LiveQuery {
        account: account_ref("binance", AccountLabel::parse("MAIN").unwrap()).unwrap(),
        window: TimestampRange::all(),
        mount_id: None,
        title: None,
    }
}

/// Two series of binance: MAIN's own round trip on BTCUSDT (a1 at 100, a2 at 110), a FOREIGN round trip of
/// MAIN on ETHUSDT (f1, f2), ALT's fill, a row from before the stream that the stream restated (a1 again,
/// no account), and two rows from before the stream nobody restated (o1, o2: no account).
fn store() -> HistoryStoreMemory {
    let store = HistoryStoreMemory::new();
    let ours = Some(OWNERSHIP_OURS);
    let btc = [
        row("o1", "BTCUSDT", 1, 90.0, 5, None, None),
        row("a1", "BTCUSDT", 1, 100.0, 10, None, None),
        row("a1", "BTCUSDT", 1, 100.0, 10, Some("MAIN"), ours),
        row("x1", "BTCUSDT", 1, 100.0, 15, Some("ALT"), ours),
        row("a2", "BTCUSDT", -1, 110.0, 40, Some("MAIN"), ours),
        row("o2", "BTCUSDT", -1, 95.0, 45, None, None),
    ];
    let eth = [
        row("f1", "ETHUSDT", 1, 10.0, 20, Some("MAIN"), Some(OWNERSHIP_FOREIGN)),
        row("f2", "ETHUSDT", -1, 12.0, 30, Some("MAIN"), Some(OWNERSHIP_FOREIGN)),
    ];
    store.append_exec_fills("binance", "BTCUSDT", &btc, None).unwrap();
    store.append_exec_fills("binance", "ETHUSDT", &eth, None).unwrap();
    store
}

fn ids(read: &AccountFills) -> Vec<&str> {
    read.fills.iter().map(|f| f.trade_id.as_str()).collect()
}

/// **The account's fills are its own, in time order, across symbols**: another account's fill is not read,
/// a row the stream restated is read once (as the stream stated it), a FOREIGN fill counts and is named,
/// and a row from before the stream carries no account, so it is left out and counted.
#[test]
fn an_account_reads_its_own_fills_counts_the_foreign_and_leaves_out_the_accountless() {
    let read = account_fills(&store(), &main_account(), false, None).unwrap();
    assert_eq!(ids(&read), ["a1", "f1", "f2", "a2"], "time order across the two series");
    assert_eq!(read.foreign, ["f1", "f2"]);
    assert_eq!(read.without_account, 2, "o1 and o2; the restated a1 is not counted twice");

    let without = account_fills(&store(), &main_account(), true, None).unwrap();
    assert_eq!(ids(&without), ["a1", "a2"], "--without-foreign leaves them out");
    assert_eq!(without.foreign, ["f1", "f2"], "…and still names them");

    // The unlabelled account is `DEFAULT` in the column, and holds none of these.
    let default = LiveQuery {
        account: account_ref("binance", AccountLabel::default()).unwrap(),
        ..main_account()
    };
    assert!(account_fills(&store(), &default, false, None).unwrap().fills.is_empty());
}

/// The window's END bounds the read, its START the counts: the account's fills before it are read (they give
/// the opening position), a fill after it is not, and the account-less rows before it are held apart.
#[test]
fn the_window_bounds_the_read() {
    let query = LiveQuery { window: TimestampRange::of(20, 35), ..main_account() };
    let read = account_fills(&store(), &query, false, None).unwrap();
    assert_eq!(ids(&read), ["a1", "f1", "f2"], "a1 opens the window's position; a2 is after it");
    assert_eq!((read.in_window, read.foreign.len()), (2, 2));
    assert_eq!(read.without_account, 0, "none in the window");
    assert_eq!(read.unattributed_before, [("BTCUSDT".to_string(), 1)], "o1, before it");
}

/// A forward run keeps the fills of its own MOUNT: never a foreign one, which has none.
#[test]
fn a_forward_run_keeps_its_mounts_fills_only() {
    let store = HistoryStoreMemory::new();
    let mounted = |id: &str, ts: i64| ExecFillRow {
        mount_id: Some("m-1".to_string()),
        ..row(id, "BTCUSDT", 1, 100.0, ts, Some("MAIN"), Some(OWNERSHIP_OURS))
    };
    let rows = [
        mounted("m1", 10),
        row("h1", "BTCUSDT", 1, 100.0, 11, Some("MAIN"), Some(OWNERSHIP_OURS)),
        row("f1", "BTCUSDT", 1, 100.0, 12, Some("MAIN"), Some(OWNERSHIP_FOREIGN)),
    ];
    store.append_exec_fills("binance", "BTCUSDT", &rows, None).unwrap();
    let query = LiveQuery { mount_id: Some("m-1".to_string()), ..main_account() };
    let read = account_fills(&store, &query, false, None).unwrap();
    assert_eq!(ids(&read), ["m1"]);
    assert!(read.foreign.is_empty());
}

/// **The fold**: a LIVE record of the account at the node's tier, over the asked window, on the REALIZED curve
/// from the cash; the notes say what was counted and left out, and they head the results.
#[test]
fn the_fold_is_a_live_record_of_the_account_at_its_tier() {
    let store = store();
    let query = LiveQuery { window: TimestampRange::of(0, 100), ..main_account() };
    let read = account_fills(&store, &query, false, None).unwrap();
    let side =
        fold_live(&query, &realized_from(1_000.0), read, &demo_facts(), &[], &store, 999).unwrap();
    match &side.results.source {
        Some(TradingRecordSource::Live { account, tier }) => {
            assert_eq!((account.venue, account.label.to_string()), ("binance", "MAIN".to_string()));
            assert_eq!(*tier, AccountTier::Demo);
        }
        other => panic!("not a live source: {other:?}"),
    }
    assert_eq!(side.results.period, Some(TradingPeriod { from_ms: 0, to_ms: 100 }));
    // Two round trips, +10 (BTC) and +2 (ETH, foreign, counted), on the realized curve from 1000.
    assert_eq!(side.results.n_trades, 2);
    assert_eq!(side.results.final_equity, 1_012.0);
    assert!(side.runtime.is_none(), "no bars are read without --mark-to-market");
    let name = side.sheet.name.clone().unwrap();
    assert!(name.starts_with("binance/MAIN (demo)"), "{name}");
    assert!(name.contains("2 FOREIGN fill(s) counted"), "{name}");
    assert!(name.contains("f1, f2"), "the foreign fills are named: {name}");
    assert!(name.contains("2 fill(s) of binance recorded before the execution stream"), "{name}");
    assert_eq!(side.notes.len(), 4, "{:?}", side.notes);
    assert_eq!(side.notes[0], "opening position at 1970-01-01T00:00:00Z: flat", "{:?}", side.notes);
    assert_eq!(side.notes[1], "starting capital 1000 (--cash)", "{:?}", side.notes);
    assert_eq!(side.results.name.as_deref(), Some(name.as_str()));
    // What a live record cannot know is not recorded, in the table and in its document.
    assert_eq!(side.sheet.metric("funding_paid"), None);
    assert!(serde_json::to_value(&side.sheet).unwrap()["funding_paid"].is_null());
    assert_eq!(side.sheet.metric("net_profit"), Some(12.0), "what it can know is there");
    let label = side.results.source_label().expect("a live source");
    assert_eq!(label, "binance/MAIN (demo) 1970-01-01 00:00 – 1970-01-01 00:00 UTC");
}

/// **A fee in another asset is no quote money**: 0.0123 BNB on a USDT fill is left out of `total_fees` and of the
/// realized curve, and said per asset, unconverted; the USDT fee beside it counts.
#[test]
fn a_fee_in_another_asset_is_left_out_of_the_fees_and_the_curve_and_said() {
    let store = HistoryStoreMemory::new();
    let ours = Some(OWNERSHIP_OURS);
    let paid = |id: &str, side: i32, px: f64, ts: i64, fee: f64, asset: &str| ExecFillRow {
        commission: fee,
        commission_asset: asset.to_string(),
        ..row(id, "BTCUSDT", side, px, ts, Some("MAIN"), ours)
    };
    let rows = [paid("b1", 1, 100.0, 10, 0.0123, "BNB"), paid("s1", -1, 110.0, 20, 0.5, "USDT")];
    store.append_exec_fills("binance", "BTCUSDT", &rows, None).unwrap();
    let read = account_fills(&store, &main_account(), false, None).unwrap();
    let fold = realized_from(1_000.0);
    let side = fold_live(&main_account(), &fold, read, &demo_facts(), &[], &store, 999).unwrap();
    assert_eq!(side.sheet.metric("total_fees"), Some(0.5), "the USDT fee alone");
    assert_eq!(
        side.sheet.metric("final_equity"),
        Some(1_009.5),
        "the curve leaves the BNB out too"
    );
    let said = "fees in other assets: 0.0123 BNB (not converted; left out of total_fees and the realized \
                curve)";
    assert!(side.notes.iter().any(|n| n == said), "{:?}", side.notes);
    assert!(
        side.sheet.name.as_deref().unwrap_or_default().contains(said),
        "the document says it too"
    );
}

/// An open window's period runs from the first fill to now; a forward run is headed by its strategy.
#[test]
fn an_open_window_runs_from_the_first_fill_to_now() {
    let store = store();
    let query = LiveQuery { title: Some("forward trend.rhai".to_string()), ..main_account() };
    let read = account_fills(&store, &query, true, None).unwrap();
    let facts = NodeFacts { tier: AccountTier::Live, ledgers: Vec::new(), contracts: Vec::new() };
    let fold = LiveFold { periods_per_year: None, without_foreign: true, ..realized_from(1_000.0) };
    let side = fold_live(&query, &fold, read, &facts, &[], &store, 999).unwrap();
    assert_eq!(side.results.period, Some(TradingPeriod { from_ms: 10, to_ms: 999 }));
    let name = side.sheet.name.unwrap();
    assert!(name.starts_with("forward trend.rhai on binance/MAIN (live)"), "{name}");
    assert!(name.contains("left out (--without-foreign)"), "{name}");
}

/// The reviewer's scenario: a long bought at 100 BEFORE the window and sold at 110 inside it. Folded from flat,
/// the exit read as a phantom short and later trades booked phantom P&L; folded from the position it opens with,
/// the window holds ONE trade of +10, and the output says what it opened with.
fn opened_before_the_window() -> HistoryStoreMemory {
    let store = HistoryStoreMemory::new();
    let ours = Some(OWNERSHIP_OURS);
    let rows = [
        row("b1", "BTCUSDT", 1, 100.0, 10, Some("MAIN"), ours),
        row("s1", "BTCUSDT", -1, 110.0, 40, Some("MAIN"), ours),
    ];
    store.append_exec_fills("binance", "BTCUSDT", &rows, None).unwrap();
    store
}

fn demo_facts() -> NodeFacts {
    NodeFacts { tier: AccountTier::Demo, ledgers: Vec::new(), contracts: Vec::new() }
}

/// The realized curve from `cash`, as `--cash` set it.
fn realized_from(cash: f64) -> LiveFold {
    LiveFold {
        cash,
        cash_from: "--cash".to_string(),
        periods_per_year: Some(365.0),
        mark_to_market: None,
        without_foreign: false,
    }
}

/// **The seed is said, with where it came from**, in the text's notes and the document's `name`: the return
/// ratios are of that capital, and a reader who cannot see it cannot read them.
#[test]
fn the_starting_capital_and_its_source_are_said() {
    let store = opened_before_the_window();
    let fold = LiveFold {
        cash: 9_000.0,
        cash_from: "run:a-1-0's stored curve's first equity".to_string(),
        ..realized_from(0.0)
    };
    let read = account_fills(&store, &main_account(), false, None).unwrap();
    let side = fold_live(&main_account(), &fold, read, &demo_facts(), &[], &store, 999).unwrap();
    let said = "starting capital 9000 (run:a-1-0's stored curve's first equity)";
    assert!(side.notes.iter().any(|n| n == said), "{:?}", side.notes);
    assert!(side.sheet.name.as_deref().unwrap_or_default().contains(said), "the document says it");
    assert_eq!(side.sheet.metric("final_equity"), Some(9_010.0), "the curve starts there");
}

#[test]
fn a_window_measures_its_exits_against_the_position_it_opens_with() {
    let store = opened_before_the_window();
    let query = LiveQuery { window: TimestampRange::of(20, 100), ..main_account() };
    let read = account_fills(&store, &query, false, None).unwrap();
    assert_eq!(read.in_window, 1, "the sell alone is in the window");
    let side =
        fold_live(&query, &realized_from(1_000.0), read, &demo_facts(), &[], &store, 999).unwrap();
    assert_eq!(side.results.n_trades, 1, "one round trip, no phantom short");
    assert_eq!(side.sheet.metric("net_profit"), Some(10.0));
    assert_eq!(side.sheet.metric("final_equity"), Some(1_010.0));
    let opened = "opening position at 1970-01-01T00:00:00Z: long 1 BTCUSDT at 100 (folded from the account's \
                  fills before it)";
    assert!(side.notes.iter().any(|n| n == opened), "{:?}", side.notes);
}

/// A fill before the window that names no account could be the account's own opening position: the window is
/// refused in words, never folded from flat.
#[test]
fn a_window_whose_opening_cannot_be_read_is_refused() {
    let store = opened_before_the_window();
    let old = [row("o1", "BTCUSDT", 1, 95.0, 5, None, None)];
    store.append_exec_fills("binance", "BTCUSDT", &old, None).unwrap();
    let query = LiveQuery { window: TimestampRange::of(20, 100), ..main_account() };
    let read = account_fills(&store, &query, false, None).unwrap();
    let Err(e) = fold_live(&query, &realized_from(1_000.0), read, &demo_facts(), &[], &store, 999)
    else {
        panic!("an unreadable opening position is refused")
    };
    assert!(e.message.contains("the opening position of binance/MAIN"), "{}", e.message);
    assert!(e.message.contains("1 of BTCUSDT"), "{}", e.message);

    // Without a window start the whole recorded history folds, starting flat, and says so.
    let read = account_fills(&store, &main_account(), false, None).unwrap();
    let side =
        fold_live(&main_account(), &realized_from(1_000.0), read, &demo_facts(), &[], &store, 999)
            .unwrap();
    assert!(side.notes[0].starts_with("the fold starts flat at the account's first recorded fill"));
}

/// `row` as the execution stream stored it: journaled at `sequence` of the datahub's first journal.
fn streamed(row: ExecFillRow, sequence: i64) -> ExecFillRow {
    ExecFillRow { journal_generation: Some(1), journal_sequence: Some(sequence), ..row }
}

/// `account`'s position snapshot of binance at `as_of_ms`, journaled at `sequence`: `held` as `(symbol, size, average
/// price)`, or `unknown`'s words.
fn snapshot(
    account: &str,
    sequence: u64,
    as_of_ms: i64,
    held: &[(&str, f64, f64)],
    unknown: Option<&str>,
) -> vike_datahub_client::execution_stream::ExecutionAccountPositionsWire {
    vike_datahub_client::execution_stream::ExecutionAccountPositionsWire {
        generation: 1,
        sequence,
        as_of_ms,
        venue: "binance".into(),
        account: account.into(),
        positions: held
            .iter()
            .map(|(symbol, size, average_price)| vike_model::AccountPosition {
                symbol: (*symbol).into(),
                position_side: vike_model::events::PositionSide::Both,
                size: *size,
                average_price: *average_price,
            })
            .collect(),
        unknown: unknown.map(str::to_string),
    }
}

/// **A position opened before the deploy and closed after it is ONE trade of the right P&L.** The buy at 100 was
/// recorded before the execution stream and names no account (with a second pre-stream fill beside it), so the
/// window [20, 100] cannot be read from the fills alone and is refused. The boot's position snapshot at 15 holds
/// MAIN long 1 at 100: the window opens from it, a stream row journaled before it (in its book) is not folded
/// again, and the sell at 110 journaled after it closes the position for +10.
#[test]
fn a_position_opened_before_the_deploy_and_closed_after_it_is_one_trade() {
    let store = HistoryStoreMemory::new();
    let ours = Some(OWNERSHIP_OURS);
    let rows = [
        row("o1", "BTCUSDT", 1, 100.0, 5, None, None),
        streamed(row("b0", "BTCUSDT", 1, 100.0, 6, Some("MAIN"), ours), 8),
        row("o2", "BTCUSDT", -1, 99.0, 7, None, None),
        streamed(row("s1", "BTCUSDT", -1, 110.0, 40, Some("MAIN"), ours), 12),
    ];
    store.append_exec_fills("binance", "BTCUSDT", &rows, None).unwrap();
    let query = LiveQuery { window: TimestampRange::of(20, 100), ..main_account() };
    let fold = realized_from(1_000.0);

    let blind = account_fills(&store, &query, false, None).unwrap();
    let Err(e) = fold_live(&query, &fold, blind, &demo_facts(), &[], &store, 999) else {
        panic!("before the first snapshot the pre-stream fills make the opening unreadable")
    };
    assert!(e.message.contains("no position snapshot of the tradehub's"), "{}", e.message);

    let boot = snapshot("MAIN", 10, 15, &[("BTCUSDT", 1.0, 100.0)], None);
    let read = account_fills(&store, &query, false, Some(&boot)).unwrap();
    assert_eq!(ids(&read), ["s1"], "only the fills journaled after the snapshot");
    let side = fold_live(&query, &fold, read, &demo_facts(), &[], &store, 999).unwrap();
    assert_eq!(side.results.n_trades, 1, "one round trip, no phantom short");
    assert_eq!(side.sheet.metric("net_profit"), Some(10.0));
    assert_eq!(side.sheet.metric("final_equity"), Some(1_010.0));
    let opened = "opening position at 1970-01-01T00:00:00Z: long 1 BTCUSDT at 100 (the tradehub's position \
                  snapshot of 1970-01-01T00:00:00Z and the account's fills after it)";
    assert!(side.notes.iter().any(|n| n == opened), "{:?}", side.notes);
}

/// **A snapshot that could not know an account's positions refuses that account only**, in its words: its other
/// account, whose positions the same boot knew, folds.
#[test]
fn an_unknown_snapshot_refuses_only_its_account() {
    let store = HistoryStoreMemory::new();
    let ours = Some(OWNERSHIP_OURS);
    let rows = [
        streamed(row("m1", "BTCUSDT", -1, 110.0, 40, Some("MAIN"), ours), 12),
        streamed(row("a1", "BTCUSDT", -1, 110.0, 41, Some("ALT"), ours), 13),
    ];
    store.append_exec_fills("binance", "BTCUSDT", &rows, None).unwrap();
    let fold = realized_from(1_000.0);
    let main = LiveQuery { window: TimestampRange::of(20, 100), ..main_account() };
    let unknown = snapshot("MAIN", 10, 15, &[], Some("the journal holds no earlier book"));
    let read = account_fills(&store, &main, false, Some(&unknown)).unwrap();
    let Err(e) = fold_live(&main, &fold, read, &demo_facts(), &[], &store, 999) else {
        panic!("an unknown opening is refused")
    };
    assert!(e.message.contains("the opening position of binance/MAIN"), "{}", e.message);
    assert!(e.message.contains("(the journal holds no earlier book)"), "{}", e.message);

    let alt = LiveQuery {
        account: account_ref("binance", AccountLabel::parse("ALT").unwrap()).unwrap(),
        ..main.clone()
    };
    let known = snapshot("ALT", 10, 15, &[("BTCUSDT", 1.0, 100.0)], None);
    let read = account_fills(&store, &alt, false, Some(&known)).unwrap();
    let side = fold_live(&alt, &fold, read, &demo_facts(), &[], &store, 999).unwrap();
    assert_eq!(side.sheet.metric("net_profit"), Some(10.0), "ALT's boot knew its book");
}

/// The reviewer's scenario: a window after the account's last fill. Nothing was measured, so it says "no
/// fills in this window" and its ratios read "not recorded" (`null` in the document), never a 0.00 Sharpe;
/// its money and counts are facts.
#[test]
fn an_empty_window_says_so_and_records_no_ratio() {
    let store = opened_before_the_window();
    let query = LiveQuery { window: TimestampRange::of(50, 100), ..main_account() };
    let read = account_fills(&store, &query, false, None).unwrap();
    assert_eq!(read.in_window, 0);
    let side =
        fold_live(&query, &realized_from(1_000.0), read, &demo_facts(), &[], &store, 999).unwrap();
    let said = "no fills in this window: its returns and ratios are not recorded";
    assert!(side.notes.iter().any(|n| n == said), "{:?}", side.notes);
    assert!(side.sheet.name.as_deref().unwrap_or_default().contains(said));
    for id in ["sharpe", "win_rate", "total_return", "max_drawdown"] {
        assert_eq!(side.sheet.metric(id), None, "{id}");
        assert!(serde_json::to_value(&side.sheet).unwrap()[id].is_null(), "{id}");
    }
    assert_eq!(side.sheet.metric("final_equity"), Some(1_000.0));
    assert_eq!(side.sheet.metric("n_trades"), Some(0.0));
    let text = side.sheet.to_string();
    let sharpe = text.lines().find(|l| l.starts_with("sharpe:")).expect("the sharpe row");
    assert!(sharpe.ends_with("not recorded"), "{text}");
}

/// `--without-foreign` cannot leave out foreign fills that share a position with the install's own: refused.
#[test]
fn without_foreign_refuses_a_position_it_would_break() {
    let store = opened_before_the_window();
    let foreign = [row("f9", "BTCUSDT", -1, 105.0, 30, Some("MAIN"), Some(OWNERSHIP_FOREIGN))];
    store.append_exec_fills("binance", "BTCUSDT", &foreign, None).unwrap();
    let read = account_fills(&store, &main_account(), true, None).unwrap();
    let fold = LiveFold { without_foreign: true, ..realized_from(1_000.0) };
    let Err(e) = fold_live(&main_account(), &fold, read, &demo_facts(), &[], &store, 999) else {
        panic!("a shared position is refused")
    };
    assert!(e.message.contains("they trade BTCUSDT beside this install's own"), "{}", e.message);
}

/// A venue this build does not know names no account.
#[test]
fn an_unknown_venue_is_refused_by_name() {
    let e = account_ref("nowhere", AccountLabel::default()).unwrap_err();
    assert!(e.message.contains("`nowhere` is not a venue this build knows"), "{}", e.message);
}

/// **A datahub that records no live executions is refused** in ONE line naming the feature and the fix,
/// whatever it holds; one that records them passes.
#[test]
fn a_datahub_that_records_no_live_executions_is_refused() {
    let recorded = vike_datahub_client::proto::FEATURE_EXECUTIONS_RECORDED.to_string();
    // Only formatted, never dialed: a port no hub binds.
    let e = refuse_a_datahub_that_records_nothing("127.0.0.1:1", &["scan_exec_fills".into()])
        .expect_err("refused");
    assert_eq!(e.rung, Rung::Refused, "{}", e.message);
    assert_eq!(
        e.message,
        format!(
            "the datahub at 127.0.0.1:1 records no live executions (it does not advertise \
             `{recorded}`): update it, the datahub and the tradehub deploy together"
        ),
        "one line"
    );
    assert!(refuse_a_datahub_that_records_nothing("a:1", &["x".into(), recorded]).is_ok());
}

/// One round trip of `symbol` on `venue`, account MAIN: bought at 100, sold at 110.
fn round_trip_on(store: &HistoryStoreMemory, venue: &str, symbol: &str) {
    let on = |id: &str, side: i32, px: f64, ts: i64| ExecFillRow {
        venue: venue.to_string(),
        commission_asset: String::new(),
        ..row(id, symbol, side, px, ts, Some("MAIN"), Some(OWNERSHIP_OURS))
    };
    let rows = [on("b1", 1, 100.0, 10), on("b2", -1, 110.0, 20)];
    store.append_exec_fills(venue, symbol, &rows, None).unwrap();
}

/// `venue`'s kept row of `symbol`, quoted in `quote`, with its stored user name (decision 0290).
fn kept_row(venue: &str, symbol: &str, quote: &str) -> vike_model::Instrument {
    let share = vike_model::Contract::Equity(vike_model::ContractEquity::default());
    let mut row = vike_model::Instrument::new(venue, symbol, symbol, quote, share);
    row.naming = Some(vike_model::InstrumentNaming::User(symbol.to_string()));
    row
}

/// **A fill's quote is its KEPT row's, never its name's alone** (the review of #3509): an IBKR share whose row quotes
/// it in euros is refused on the dollar ledger, though `SAP` states no currency; the same share quoted in dollars
/// folds; and a broker's CFD whose name states no currency and that no row names is refused as "unknown quote
/// currency", never booked at par.
#[test]
fn a_fill_quoted_off_the_dollar_by_its_kept_row_is_refused() {
    let fold = |venue: &str, symbol: &str, kept: &[vike_model::Instrument]| {
        let store = HistoryStoreMemory::new();
        round_trip_on(&store, venue, symbol);
        let query = LiveQuery {
            account: account_ref(venue, AccountLabel::parse("MAIN").unwrap()).unwrap(),
            ..main_account()
        };
        let read = account_fills(&store, &query, false, None).unwrap();
        fold_live(&query, &realized_from(1_000.0), read, &demo_facts(), kept, &store, 999)
    };
    let refused = vike_analytics::trading_results_table_foreign_quote_refusal;

    let Err(e) = fold("ibkr", "SAP", &[kept_row("ibkr", "SAP", "EUR")]) else {
        panic!("a euro share folded as dollars");
    };
    assert_eq!((e.rung, e.message), (Rung::Failed, refused("SAP (EUR)")));

    let side =
        fold("ibkr", "SAP", &[kept_row("ibkr", "SAP", "USD")]).expect("a dollar share folds");
    assert_eq!(side.results.n_trades, 1);

    let Err(e) = fold("ig", "IX.D.DAX.IFD.IP", &[]) else {
        panic!("an index of no known currency folded at par");
    };
    assert_eq!(
        (e.rung, e.message),
        (Rung::Failed, refused("IX.D.DAX.IFD.IP (unknown quote currency)"))
    );
}

/// **A NOT RUNNING account is refused, its ledger and contracts unknown** (the review of #3509): the node's row of a
/// disarmed bybit account states neither its ledger's currency nor its contracts' values, so a fold of its inverse
/// fills would read them as linear dollars. An account the node names nowhere is refused for its tier.
#[test]
fn a_not_running_account_is_refused_and_an_unknown_one_too() {
    let mut wire = vike_tradehub_client::wire::CoreSnapshotWire::empty();
    wire.not_running.push(
        serde_json::from_value(serde_json::json!({
            "venue": "bybit", "route_key": "bybit", "tier": "live", "reason": "no credentials"
        }))
        .unwrap(),
    );
    let bybit = account_ref("bybit", AccountLabel::default()).unwrap();
    let Err(e) = facts_of("n:1", &wire, &bybit) else {
        panic!("a not running account was folded");
    };
    assert_eq!(e.rung, Rung::Refused);
    assert_eq!(
        e.message,
        "bybit/DEFAULT is NOT RUNNING on the tradehub at n:1, so its ledger's currency and its contracts' \
         values are unknown (only a running engine states them): a live report would read its fills as \
         dollars of a linear contract. Report it while it runs."
    );

    let okx = account_ref("okx", AccountLabel::default()).unwrap();
    let Err(e) = facts_of("n:1", &wire, &okx) else {
        panic!("an account the node names nowhere was folded");
    };
    assert_eq!(e.rung, Rung::Refused);
    assert!(
        e.message.starts_with("the tradehub at n:1 names no account okx/DEFAULT"),
        "{}",
        e.message
    );
}

/// **A silent datahub ends the read inside the store's reply bound** (the review of #3509): the MCP tool answers on
/// its one thread, so the caller's bound is what stands between a half-open link and the whole server waiting out the
/// client's five minutes. A hub that says what it records and then nothing more: the kept-rows read gives up and
/// says so as an unreachable datahub, before the node is ever asked.
#[test]
fn a_silent_datahub_gives_up_inside_the_stores_bound() {
    use std::io::Read as _;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let welcome = vike_hub_protocol::handshake::HandshakeReply::Welcome {
                    proto_version: vike_datahub_client::PROTO_VERSION,
                    features: vec![
                        vike_datahub_client::proto::FEATURE_EXECUTIONS_RECORDED.to_string(),
                        vike_datahub_client::proto::FEATURE_INSTRUMENT_LISTS.to_string(),
                    ],
                    nonce: None,
                };
                vike_hub_protocol::frame::write_frame(&mut stream, &welcome).unwrap();
                let mut sink = [0u8; 256];
                while matches!(stream.read(&mut sink), Ok(n) if n > 0) {}
            });
        }
    });
    let bound = std::time::Duration::from_millis(300);
    let store = vike_datahub_client::HistoryStoreDatahub::new(&addr).with_reply_timeout(bound);
    let keys = HubKeyring::default();
    let fold = realized_from(1_000.0);
    let started = std::time::Instant::now();
    let Err(e) = read_live_side("127.0.0.1:1", &keys, &store, &addr, &main_account(), &fold, 999)
    else {
        panic!("a silent datahub answered");
    };
    assert_eq!(e.rung, Rung::Connect, "{}", e.message);
    let said = format!("the datahub at {addr} could not be read: ");
    assert!(e.message.starts_with(&said), "{}", e.message);
    // The bound, and the one redial a dead pooled connection takes: seconds, never the client's five minutes.
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
}
