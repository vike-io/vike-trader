use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use vike_account_window::history::page::ExecutionHistoryWindow;
use vike_data::HistoryStoreMemory;

const TEST_DEADLINE: Duration = Duration::from_secs(5);
const NOW: i64 = 1_790_000_000_000;

fn fill(venue: &str, symbol: &str, timestamp: i64, trade: &str) -> ExecFillRow {
    ExecFillRow {
        ts: timestamp,
        trade_id: trade.to_string(),
        order_client_id: "c1".to_string(),
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
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    }
}

fn order(timestamp: i64, order_client_id: &str, status: &str) -> ExecOrderRow {
    ExecOrderRow {
        ts: timestamp,
        order_client_id: order_client_id.to_string(),
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
        mount_id: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    }
}

fn query(window: ExecutionHistoryWindow) -> ExecutionHistoryQuery {
    ExecutionHistoryQuery {
        venue: None,
        symbol: None,
        window,
        now_ms: NOW,
        older_than: None,
        generation: 1,
    }
}

fn served() -> ExecutionHistoryServed {
    ExecutionHistoryServed { fills_range: true, orders: true, recording: true }
}

fn store_with_fills(rows: &[ExecFillRow]) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    store.append_exec_fills(&rows[0].venue, &rows[0].symbol, rows, None).unwrap();
    Arc::new(store)
}

/// [`load_execution_history`] over ONE handle, its progress unread.
fn load(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    served: ExecutionHistoryServed,
    query: &ExecutionHistoryQuery,
) -> Result<ExecutionHistoryPage, String> {
    load_execution_history(std::slice::from_ref(store), served, query, TEST_DEADLINE, &|_, _| {})
}

/// The walk goes BACKWARDS in windows: with a year of fills one per hour and a page far smaller
/// than a day's worth past the first window, the page is the NEWEST rows, newest first, and says
/// that older ones exist.
#[test]
fn walks_windows_backwards_until_a_page_is_full() {
    let hour = vike_model::MS_PER_DAY / 24;
    let rows: Vec<ExecFillRow> = (0..(24 * 400))
        .map(|k| fill("binance", "BTCUSDT", NOW - k * hour, &format!("t{k}")))
        .collect();
    let store = store_with_fills(&rows);
    let page = load(&store, served(), &query(ExecutionHistoryWindow::All)).expect("a read");
    assert_eq!(page.fills.len(), HISTORY_PAGE_ROWS, "one page");
    assert_eq!(page.fills[0].ts, NOW, "the newest row first");
    assert!(page.fills.windows(2).all(|w| w[0].ts > w[1].ts), "newest first, no repeat");
    assert_eq!(page.fills.last().map(|r| r.ts), Some(NOW - (HISTORY_PAGE_ROWS as i64 - 1) * hour));
    assert!(page.more, "a year of older rows is still there");
    assert_eq!(page.oldest_timestamp, page.fills.last().map(|r| r.ts));
    assert_eq!(
        page.venues.get("binance").map(|s| s.iter().cloned().collect::<Vec<_>>()),
        Some(vec!["BTCUSDT".to_string()]),
        "the filters' choices come from the catalog"
    );
}

/// "Older" continues strictly below the oldest loaded `ts`, and a window's floor is honoured.
#[test]
fn older_continues_from_the_oldest_loaded_timestamp() {
    let rows: Vec<ExecFillRow> =
        (0..10).map(|k| fill("binance", "BTCUSDT", NOW - k * 1_000, &format!("t{k}"))).collect();
    let store = store_with_fills(&rows);
    let first = load(&store, served(), &query(ExecutionHistoryWindow::Day)).unwrap();
    assert_eq!(first.fills.len(), 10);
    let older = ExecutionHistoryQuery {
        older_than: Some(NOW - 4_000),
        ..query(ExecutionHistoryWindow::Day)
    };
    let page = load(&store, served(), &older).unwrap();
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
    impl HistoryStore for Refusing {
        vike_data::hist_store_stubs!(inert: all);
    }
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(Refusing);
    let err = load(&store, served(), &query(ExecutionHistoryWindow::Week))
        .expect_err("an unreadable catalog is an error");
    assert!(err.contains("inventory"), "{err}");
}

/// A datahub older than the order read answers fills only; the page carries the fact so the
/// screen can SAY it, and no order read is attempted.
#[test]
fn an_unserved_orders_read_degrades_to_fills_with_a_note() {
    let memory = HistoryStoreMemory::new();
    memory
        .append_exec_fills("binance", "BTCUSDT", &[fill("binance", "BTCUSDT", NOW, "t")], None)
        .unwrap();
    memory.append_exec_orders("binance", "BTCUSDT", &[order(NOW, "c1", "FILLED")], None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(memory);
    let old = ExecutionHistoryServed::from_features(&["scan_exec_fills".to_string()]);
    assert!(old.predates_history() && !old.not_recording(), "too old to say either way");
    let page = load(&store, old, &query(ExecutionHistoryWindow::Week)).unwrap();
    assert_eq!(page.fills.len(), 1);
    assert!(page.orders.is_empty(), "no order read against a datahub that does not serve one");

    let new = load(&store, served(), &query(ExecutionHistoryWindow::Week)).unwrap();
    assert_eq!(new.orders.len(), 1);
    let off = ExecutionHistoryServed { recording: false, ..served() };
    assert!(off.not_recording());
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
    let page = load(&store, served(), &query(ExecutionHistoryWindow::Day)).unwrap();
    assert!(page.more, "rows beyond the page are announced, not dropped in silence");
    let at_edge = page.fills.iter().filter(|r| r.ts == edge).count();
    assert_eq!(at_edge, 2, "the tied timestamp is kept whole");
    assert_eq!(page.oldest_timestamp, Some(edge));
}

/// The filters reach the catalog: another venue's series is listed for the picker but not read.
#[test]
fn a_venue_filter_reads_only_that_venue() {
    let memory = HistoryStoreMemory::new();
    memory
        .append_exec_fills("binance", "BTCUSDT", &[fill("binance", "BTCUSDT", NOW, "a")], None)
        .unwrap();
    memory
        .append_exec_fills("okx", "BTC-USDT", &[fill("okx", "BTC-USDT", NOW, "b")], None)
        .unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(memory);
    let q = ExecutionHistoryQuery {
        venue: Some("okx".to_string()),
        ..query(ExecutionHistoryWindow::Week)
    };
    let page = load(&store, served(), &q).unwrap();
    assert_eq!(page.fills.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), vec!["okx"]);
    assert_eq!(page.venues.len(), 2, "both venues stay pickable");
}

/// **The History load asks the datahub for the window's catalog, never for the whole inventory**, and a
/// datahub too old for that read is asked the inventory and folded here: the same page either way, the
/// out-of-window venue still pickable.
#[test]
fn the_load_reads_the_windows_catalog_and_an_older_datahub_its_inventory() {
    use vike_datahub_client::proto::{
        FEATURE_EXECUTION_SERIES, FEATURE_SCAN_EXECUTION_FILLS_RANGE, Request, Response,
        request_kind,
    };
    let memory = HistoryStoreMemory::new();
    let live = [fill("binance", "BTCUSDT", NOW - 5_000, "a")];
    memory.append_exec_fills("binance", "BTCUSDT", &live, None).unwrap();
    let month_old = [fill("okx", "BTC-USDT", NOW - 30 * vike_model::MS_PER_DAY, "b")];
    memory.append_exec_fills("okx", "BTC-USDT", &month_old, None).unwrap();
    let q = query(ExecutionHistoryWindow::Week);
    let (lower, upper) = q.bounds();
    let catalog = memory
        .execution_series(None, None, TimestampRange { start: lower, end: Some(upper) })
        .unwrap();
    let inventory = memory.inventory().unwrap();
    let load_from = |features: &[&str]| {
        let features = features.iter().map(|f| f.to_string()).collect();
        let (catalog, inventory, live) = (catalog.clone(), inventory.clone(), live.to_vec());
        let (address, asked, peer) =
            vike_datahub_client::test_support::spawn_fake_peer(features, move |r| match r {
                Request::ExecutionSeries { .. } => Some(Response::ExecutionSeries(catalog.clone())),
                Request::Inventory => Some(Response::Inventory(inventory.clone())),
                Request::ScanExecFills { .. } => Some(Response::ExecFills(live.clone())),
                _ => None,
            });
        let open = move || Some(vike_datahub_client::HistoryStoreDatahub::new(address.to_string()));
        let page = load_remote(&open, &q, TEST_DEADLINE, &|_, _| {}).expect("a load");
        let asked: Vec<&str> =
            asked.recv().expect("the peer reports").iter().map(request_kind).collect();
        peer.join().expect("the peer ends cleanly");
        (page, asked)
    };
    let (new, asked) = load_from(&[FEATURE_EXECUTION_SERIES, FEATURE_SCAN_EXECUTION_FILLS_RANGE]);
    assert_eq!(
        asked,
        ["ExecutionSeries", "ScanExecFills"],
        "the window's catalog, then its one live series"
    );
    assert_eq!(new.fills.iter().map(|r| r.trade_id.as_str()).collect::<Vec<_>>(), ["a"]);
    assert_eq!(
        new.venues.keys().collect::<Vec<_>>(),
        ["binance", "okx"],
        "both venues stay pickable"
    );
    let (old, asked) = load_from(&[FEATURE_SCAN_EXECUTION_FILLS_RANGE]);
    assert_eq!(asked, ["Inventory", "ScanExecFills"], "an older datahub is asked what it serves");
    assert_eq!(old, new, "the same page from either datahub");
}

/// The memory store, counting the exec reads that would each be a round trip over the wire, and
/// how many of them were in flight at once (each read holds `pause`, so overlapping reads overlap).
struct CountingStore {
    inner: HistoryStoreMemory,
    reads: AtomicUsize,
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    pause: Duration,
}

impl CountingStore {
    fn new(inner: HistoryStoreMemory, pause: Duration) -> Arc<Self> {
        let zero = || AtomicUsize::new(0);
        Arc::new(Self { inner, reads: zero(), in_flight: zero(), peak: zero(), pause })
    }

    fn read<T>(&self, scan: impl FnOnce() -> T) -> T {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(self.pause);
        let out = scan();
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        out
    }
}

impl HistoryStore for CountingStore {
    vike_data::hist_store_stubs!(inert: all);
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, vike_data::HistoryStoreError> {
        self.inner.inventory()
    }
    fn scan_execution_fills_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, vike_data::HistoryStoreError> {
        self.read(|| self.inner.scan_execution_fills_head(venue, symbol, range, n))
    }
    fn scan_execution_orders_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        n: usize,
    ) -> Result<Vec<ExecOrderRow>, vike_data::HistoryStoreError> {
        self.read(|| self.inner.scan_execution_orders_head(venue, symbol, range, n))
    }
}

/// Forty series that last traded a month ago and two that traded today.
fn forty_old_two_live() -> HistoryStoreMemory {
    let memory = HistoryStoreMemory::new();
    let month = 30 * vike_model::MS_PER_DAY;
    for k in 0..40 {
        let symbol = format!("OLD{k}");
        let rows = [fill("binance", &symbol, NOW - month - k, &format!("o{k}"))];
        memory.append_exec_fills("binance", &symbol, &rows, None).unwrap();
    }
    for (symbol, timestamp) in [("BTCUSDT", NOW - 5_000), ("ETHUSDT", NOW - 9_000)] {
        let rows =
            [fill("bybit", symbol, timestamp, symbol), fill("bybit", symbol, timestamp - 1, "x")];
        memory.append_exec_fills("bybit", symbol, &rows, None).unwrap();
    }
    memory
}

/// **A series the catalog says holds nothing in the window costs no read** — the owner's stuck
/// Tearsheet (2026-11-16): 180 exec series, two empty reads each, ~4 minutes over his tunnel. A
/// 7-day load over [`forty_old_two_live`] reads the two, ONCE each, shows exactly their rows, and
/// reports "2 of 2" once the catalog said how many.
#[test]
fn series_outside_the_window_cost_no_read() {
    let counting = CountingStore::new(forty_old_two_live(), Duration::ZERO);
    let store: Arc<dyn HistoryStore + Send + Sync> = counting.clone();
    let reported = Mutex::new(Vec::new());
    let report = |read: usize, total: usize| reported.lock().unwrap().push((read, total));
    let page = load_execution_history(
        std::slice::from_ref(&store),
        served(),
        &query(ExecutionHistoryWindow::Week),
        TEST_DEADLINE,
        &report,
    )
    .unwrap();
    assert_eq!(counting.reads.load(Ordering::SeqCst), 2, "one read per live series");
    assert_eq!(
        page.fills.iter().map(|r| r.ts).collect::<Vec<_>>(),
        vec![NOW - 5_000, NOW - 5_001, NOW - 9_000, NOW - 9_001]
    );
    assert!(!page.more);
    assert_eq!(page.venues.get("binance").map(|s| s.len()), Some(40), "still pickable");
    assert_eq!(*reported.lock().unwrap(), vec![(0, 2), (1, 2), (2, 2)], "0, 1 then 2 of 2");
}

/// **Reads run at most `lanes` at a time and answer exactly what one lane answers.** Eight live
/// series over three handles: the reads overlap (the owner's tunnel pays one round trip per read),
/// never more than three at once, and the page is byte-for-byte the one-handle page.
#[test]
fn reads_run_on_at_most_the_lanes_given_and_answer_as_one_lane_does() {
    let live = || {
        let memory = HistoryStoreMemory::new();
        for k in 0..8 {
            let symbol = format!("S{k}");
            let rows: Vec<ExecFillRow> = (0..3)
                .map(|j| fill("bybit", &symbol, NOW - k * 100 - j, &format!("{k}-{j}")))
                .collect();
            memory.append_exec_fills("bybit", &symbol, &rows, None).unwrap();
        }
        memory
    };
    let pause = Duration::from_millis(30);
    let one = CountingStore::new(live(), pause);
    let one_store: Arc<dyn HistoryStore + Send + Sync> = one.clone();
    let alone = load(&one_store, served(), &query(ExecutionHistoryWindow::Week)).unwrap();
    assert_eq!(one.peak.load(Ordering::SeqCst), 1, "one handle reads one series at a time");

    let three = CountingStore::new(live(), pause);
    let store: Arc<dyn HistoryStore + Send + Sync> = three.clone();
    let lanes = vec![Arc::clone(&store), Arc::clone(&store), store];
    let together = load_execution_history(
        &lanes,
        served(),
        &query(ExecutionHistoryWindow::Week),
        TEST_DEADLINE,
        &|_, _| {},
    )
    .unwrap();
    let peak = three.peak.load(Ordering::SeqCst);
    assert!((2..=3).contains(&peak), "reads overlap, at most three at once: {peak}");
    assert_eq!(three.reads.load(Ordering::SeqCst), 8, "one read per live series");
    assert_eq!(together, alone, "the same page whichever lanes read it");
}

/// Once the page is full, a series whose newest row is older than the page's edge is not read, and
/// the page says older rows exist so "Older" reaches it.
#[test]
fn a_full_page_skips_a_series_older_than_its_edge() {
    let memory = HistoryStoreMemory::new();
    let busy: Vec<ExecFillRow> = (0..HISTORY_PAGE_ROWS as i64 + 20)
        .map(|k| fill("binance", "BTCUSDT", NOW - k, &format!("b{k}")))
        .collect();
    memory.append_exec_fills("binance", "BTCUSDT", &busy, None).unwrap();
    let old = [fill("okx", "BTC-USDT", NOW - vike_model::MS_PER_DAY / 2, "old")];
    memory.append_exec_fills("okx", "BTC-USDT", &old, None).unwrap();
    let counting = CountingStore::new(memory, Duration::ZERO);
    let store: Arc<dyn HistoryStore + Send + Sync> = counting.clone();
    let page = load(&store, served(), &query(ExecutionHistoryWindow::All)).unwrap();
    assert_eq!(page.fills.len(), HISTORY_PAGE_ROWS);
    assert!(page.fills.iter().all(|r| r.venue == "binance"), "the newest rows");
    assert!(page.more, "the skipped series is announced");
    let reads = counting.reads.load(Ordering::SeqCst);
    let older = ExecutionHistoryQuery {
        older_than: page.oldest_timestamp,
        ..query(ExecutionHistoryWindow::All)
    };
    let next = load(&store, served(), &older).unwrap();
    assert!(next.fills.iter().any(|r| r.venue == "okx"), "\"Older\" reaches it");
    assert!(reads < counting.reads.load(Ordering::SeqCst));
}

/// **The fills before the page are read WITH it, once per load** (the Summary's opening position): from the
/// first row of each series on the page that the catalog says starts before the page's oldest fill, and of no
/// other. The page itself, which the Fills list shows, is the window's as before; an "Older" page's earlier
/// fills join, and the ones read first stay.
#[test]
fn the_fills_before_the_page_are_read_with_it_once_per_series_that_has_some() {
    let memory = HistoryStoreMemory::new();
    let day = vike_model::MS_PER_DAY;
    let btc = [
        fill("binance", "BTCUSDT", NOW - 10 * day, "old"),
        fill("binance", "BTCUSDT", NOW - 1_000, "new"),
    ];
    memory.append_exec_fills("binance", "BTCUSDT", &btc, None).unwrap();
    let eth = [fill("binance", "ETHUSDT", NOW - 2_000, "e")];
    memory.append_exec_fills("binance", "ETHUSDT", &eth, None).unwrap();
    let old = [fill("binance", "SOLUSDT", NOW - 20 * day, "s")];
    memory.append_exec_fills("binance", "SOLUSDT", &old, None).unwrap();
    let counting = CountingStore::new(memory, Duration::ZERO);
    let store: Arc<dyn HistoryStore + Send + Sync> = counting.clone();
    let page = load(&store, served(), &query(ExecutionHistoryWindow::Week)).unwrap();
    let timestamps = |rows: &[ExecFillRow]| rows.iter().map(|r| r.ts).collect::<Vec<_>>();
    assert_eq!(timestamps(&page.fills), [NOW - 1_000, NOW - 2_000], "the page is the window's");
    let key = |symbol: &str| ("binance".to_string(), symbol.to_string());
    assert_eq!(page.before.get(&key("BTCUSDT")).map(|r| timestamps(r)), Some(vec![NOW - 10 * day]));
    assert!(!page.before.contains_key(&key("ETHUSDT")), "it starts inside the page: no read");
    assert!(!page.before.contains_key(&key("SOLUSDT")), "no fill of it is on the page");
    assert_eq!(counting.reads.load(Ordering::SeqCst), 3, "two series read, one of them before too");
    assert_eq!(page.before_unread, None);

    let mut merged = page.clone();
    let older = ExecutionHistoryPage {
        before: [(key("BTCUSDT"), Vec::new()), (key("XRPUSDT"), vec![btc[0].clone()])].into(),
        ..ExecutionHistoryPage::default()
    };
    merged.extend_older(older);
    assert_eq!(merged.before.get(&key("BTCUSDT")), page.before.get(&key("BTCUSDT")), "kept");
    assert!(merged.before.contains_key(&key("XRPUSDT")), "joined");
}

/// The slot's progress answers only for the generation that asked, and only once the catalog said
/// how many series there are to read.
#[test]
fn progress_answers_for_its_own_generation_once_counted() {
    let slot = ExecutionHistorySlot::default();
    assert_eq!(slot.progress(1), None, "nothing asked yet");
    slot.report(1, 0, 0);
    assert_eq!(slot.progress(1), None, "the catalog has not counted yet");
    slot.report(1, 5, 21);
    assert_eq!(slot.progress(1), Some((5, 21)));
    assert_eq!(slot.progress(2), None, "another generation's load says nothing for this one");
}

/// A load with no datahub answers `NoDatahub` through the slot without dialling anything.
#[test]
fn no_datahub_is_its_own_answer() {
    let slot = ExecutionHistorySlot::default();
    let (tx, rx) = std::sync::mpsc::channel();
    spawn_load(
        query(ExecutionHistoryWindow::Week),
        slot.clone(),
        TEST_DEADLINE,
        || None,
        move || tx.send(()).unwrap(),
    );
    rx.recv_timeout(TEST_DEADLINE).expect("the load finishes");
    let load = slot.take().expect("the load landed");
    assert_eq!(load.outcome, Err(ExecutionHistoryFailure::NoDatahub));
    assert_eq!(load.query.generation, 1);
}
