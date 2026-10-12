use super::*;
use std::time::Duration;
use vike_data::MemHistStore;

const TEST_DEADLINE: Duration = Duration::from_secs(5);
const NOW: i64 = 1_790_000_000_000;

fn fill(venue: &str, symbol: &str, ts: i64, trade: &str) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: trade.to_string(),
        client_order_id: "c1".to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        px: 100.0,
        commission: 0.0,
        mark_price: None,
        liquidity_side: String::new(),
        commission_asset: String::new(),
        venue_order_id: None,
        account: None,
    }
}

fn order(ts: i64, coid: &str, status: &str) -> ExecOrderRow {
    ExecOrderRow {
        ts,
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        status: status.to_string(),
        price: Some(100.0),
        trigger_price: None,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        account: None,
    }
}

fn query(window: HistoryWindow) -> HistoryQuery {
    HistoryQuery { venue: None, symbol: None, window, now_ms: NOW, older_than: None, generation: 1 }
}

fn served() -> ExecServed {
    ExecServed { fills_range: true, orders: true, recording: true }
}

fn store_with_fills(rows: &[ExecFillRow]) -> Arc<dyn HistStore + Send + Sync> {
    let store = MemHistStore::new();
    store.append_exec_fills(&rows[0].venue, &rows[0].symbol, rows, None).unwrap();
    Arc::new(store)
}

/// The walk goes BACKWARDS in windows: with a year of fills one per hour and a page far smaller
/// than a day's worth past the first window, the page is the NEWEST rows, newest first, and says
/// that older ones exist.
#[test]
fn walks_windows_backwards_until_a_page_is_full() {
    let hour = DAY_MS / 24;
    let rows: Vec<ExecFillRow> = (0..(24 * 400))
        .map(|k| fill("binance", "BTCUSDT", NOW - k * hour, &format!("t{k}")))
        .collect();
    let store = store_with_fills(&rows);
    let page = load_exec_history(&store, served(), &query(HistoryWindow::All), TEST_DEADLINE)
        .expect("a read");
    assert_eq!(page.fills.len(), HISTORY_PAGE_ROWS, "one page");
    assert_eq!(page.fills[0].ts, NOW, "the newest row first");
    assert!(page.fills.windows(2).all(|w| w[0].ts > w[1].ts), "newest first, no repeat");
    assert_eq!(page.fills.last().map(|r| r.ts), Some(NOW - (HISTORY_PAGE_ROWS as i64 - 1) * hour));
    assert!(page.more, "a year of older rows is still there");
    assert_eq!(page.oldest_ts, page.fills.last().map(|r| r.ts));
    assert_eq!(
        page.venues.get("binance").map(|s| s.iter().cloned().collect::<Vec<_>>()),
        Some(vec!["BTCUSDT".to_string()]),
        "the filters' choices come from the catalog"
    );
}

/// "Older" continues strictly below the oldest loaded `ts`, and a window's floor is honoured.
#[test]
fn older_continues_from_the_oldest_loaded_ts() {
    let rows: Vec<ExecFillRow> =
        (0..10).map(|k| fill("binance", "BTCUSDT", NOW - k * 1_000, &format!("t{k}"))).collect();
    let store = store_with_fills(&rows);
    let first =
        load_exec_history(&store, served(), &query(HistoryWindow::Day), TEST_DEADLINE).unwrap();
    assert_eq!(first.fills.len(), 10);
    let older = HistoryQuery { older_than: Some(NOW - 4_000), ..query(HistoryWindow::Day) };
    let page = load_exec_history(&store, served(), &older, TEST_DEADLINE).unwrap();
    assert_eq!(
        page.fills.iter().map(|r| r.ts).collect::<Vec<_>>(),
        (5..10).map(|k| NOW - k * 1_000).collect::<Vec<_>>()
    );
    assert!(!page.more);

    let mut merged = first.clone();
    merged.extend_older(page);
    assert_eq!(merged.fills.len(), 15, "an older page is APPENDED below");
}

/// A read that fails is an `Err` with the reason — never an empty page, which would read as
/// "nothing traded".
#[test]
fn a_failed_read_is_an_error_not_an_empty_history() {
    // The catalog read is the trait DEFAULT, which refuses: a store that cannot enumerate.
    struct Refusing;
    impl HistStore for Refusing {
        vike_data::hist_store_stubs!(inert: all);
    }
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(Refusing);
    let err = load_exec_history(&store, served(), &query(HistoryWindow::Week), TEST_DEADLINE)
        .expect_err("an unreadable catalog is an error");
    assert!(err.contains("inventory"), "{err}");
}

/// A datahub older than the order read answers fills only; the page carries the fact so the
/// screen can SAY it, and no order read is attempted.
#[test]
fn an_unserved_orders_read_degrades_to_fills_with_a_note() {
    let mem = MemHistStore::new();
    mem.append_exec_fills("binance", "BTCUSDT", &[fill("binance", "BTCUSDT", NOW, "t")], None)
        .unwrap();
    mem.append_exec_orders("binance", "BTCUSDT", &[order(NOW, "c1", "FILLED")], None).unwrap();
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(mem);
    let old = ExecServed::from_features(&["scan_exec_fills".to_string()]);
    assert!(old.predates_history() && !old.materializer_off(), "too old to say either way");
    let page = load_exec_history(&store, old, &query(HistoryWindow::Week), TEST_DEADLINE).unwrap();
    assert_eq!(page.fills.len(), 1);
    assert!(page.orders.is_empty(), "no order read against a datahub that does not serve one");

    let new =
        load_exec_history(&store, served(), &query(HistoryWindow::Week), TEST_DEADLINE).unwrap();
    assert_eq!(new.orders.len(), 1);
    let off = ExecServed { recording: false, ..served() };
    assert!(off.materializer_off());
}

/// The page cut keeps a tied timestamp WHOLE, so "Older" (strictly below the oldest `ts`) loses
/// no row of it.
#[test]
fn the_row_ceiling_is_said_not_silent() {
    let mut rows: Vec<ExecFillRow> = (0..HISTORY_PAGE_ROWS as i64 + 10)
        .map(|k| fill("binance", "BTCUSDT", NOW - k, &format!("t{k}")))
        .collect();
    // a tie straddling the page edge
    let edge = NOW - (HISTORY_PAGE_ROWS as i64 - 1);
    rows.push(fill("binance", "BTCUSDT", edge, "tie"));
    let store = store_with_fills(&rows);
    let page =
        load_exec_history(&store, served(), &query(HistoryWindow::Day), TEST_DEADLINE).unwrap();
    assert!(page.more, "rows beyond the page are announced, not dropped in silence");
    let at_edge = page.fills.iter().filter(|r| r.ts == edge).count();
    assert_eq!(at_edge, 2, "the tied timestamp is kept whole");
    assert_eq!(page.oldest_ts, Some(edge));
}

/// The filters reach the catalog: another venue's series is listed for the picker but not read.
#[test]
fn a_venue_filter_reads_only_that_venue() {
    let mem = MemHistStore::new();
    mem.append_exec_fills("binance", "BTCUSDT", &[fill("binance", "BTCUSDT", NOW, "a")], None)
        .unwrap();
    mem.append_exec_fills("okx", "BTC-USDT", &[fill("okx", "BTC-USDT", NOW, "b")], None).unwrap();
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(mem);
    let q = HistoryQuery { venue: Some("okx".to_string()), ..query(HistoryWindow::Week) };
    let page = load_exec_history(&store, served(), &q, TEST_DEADLINE).unwrap();
    assert_eq!(page.fills.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), vec!["okx"]);
    assert_eq!(page.venues.len(), 2, "both venues stay pickable");
}

/// A load with no datahub answers `NoDatahub` through the slot without dialling anything.
#[test]
fn no_datahub_is_its_own_answer() {
    let slot = ExecHistorySlot::default();
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_load(query(HistoryWindow::Week), slot.clone(), || None, move || tx.send(()).unwrap());
    rx.recv_timeout(TEST_DEADLINE).expect("the load finishes");
    let load = slot.take().expect("the load landed");
    assert_eq!(load.outcome, Err(HistoryFailure::NoDatahub));
    assert_eq!(load.query.generation, 1);
}
