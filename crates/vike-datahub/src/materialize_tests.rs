use super::*;
use crate::materialize::testkit::{
    accepted, canceled, fill, fill_ev, order_filled, order_partial, submit, submitted,
};
use std::sync::atomic::AtomicUsize;

use vike_data::{DataError, MemHistStore};
use vike_journal::JournalFileConfig;
use vike_model::OrderRequest;

/// Write a WAL with N fills, drain it once, assert Tier-2 holds exactly those fills; a second
/// drain (no new records) is a no-op; a re-drain of the SAME window never dups.
#[test]
fn materializes_wal_fills_into_tier2_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    // write a journal with 3 fills
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        j.append_cmd(1, &fill("t1", "c1", "BTCUSDT", 0.5, 100.0, 10)).unwrap();
        j.append_cmd(2, &fill("t2", "c1", "BTCUSDT", 0.5, 101.0, 20)).unwrap();
        j.append_cmd(3, &fill("t3", "c2", "ETHUSDT", 1.0, 50.0, 30)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut state = PassState::resume(dir.path());
    // cold start (no checkpoint) reads the whole journal, INCLUDING the seq-0 record.
    materialize_once(dir.path(), &store, &seq, &mut state).unwrap();

    let btc = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
    let eth = store.scan_exec_fills("binance", "ETHUSDT").unwrap();
    assert_eq!(btc.len(), 2, "two BTC fills materialized");
    assert_eq!(eth.len(), 1, "one ETH fill materialized");
    assert_eq!(btc[0].trade_id, "t1", "the seq-0 fill is NOT dropped on cold start");
    assert_eq!(eth[0].symbol, "ETHUSDT");
    assert_eq!(btc[0].liquidity_side, "taker", "LiquiditySide threaded through as its wire string");
    assert_eq!(btc[0].commission_asset, "", "empty Ustr threads through as an empty string");
    assert!(seq.load(Ordering::Relaxed) >= 2, "checkpoint advanced to the max seq read");

    // second drain: no new records past the checkpoint → no-op, no growth
    materialize_once(dir.path(), &store, &seq, &mut state).unwrap();
    assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(), 2, "no re-append");

    // crash-before-checkpoint: lose the checkpoint, re-drain cold over the SAME window → the
    // per-window commit_key makes the re-append a store no-op (at-least-once is safe).
    std::fs::remove_file(dir.path().join("materializer.ckpt")).unwrap();
    materialize_once(dir.path(), &store, &AtomicU64::new(0), &mut PassState::resume(dir.path()))
        .unwrap();
    assert_eq!(
        store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(),
        2,
        "re-drain after a lost checkpoint is idempotent (commit_key)"
    );
}

/// Order lifecycle materializes into `exec_order` snapshots: an explicit-coid order folds
/// submit → OrderSubmitted → accept → partial → fill (symbol from the submit, qty accumulated);
/// a server-minted (empty-coid) order resolves its symbol from the fill; and a
/// symbol-unresolvable order (empty coid, canceled without ever filling) is skipped, not
/// persisted.
///
/// The adapter's own `OrderSubmitted` records are in the WAL here because they are in the real
/// WAL (a venue emits `OrderSubmitted` BEFORE the REST round trip), and folding through the FSM
/// makes them load-bearing: `OrderAccepted` is legal only from SUBMITTED. Every status string
/// asserted below is `OrderStatus::as_str()`, unchanged from the hand-written ones it replaced.
#[test]
fn materializes_order_lifecycle_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        // explicit-coid order c1: submit(limit BTCUSDT 1.0) → OrderSubmitted → accept →
        // partial 0.3@100 → fill 0.7@110
        j.append_cmd(1, &submit("c1", "binance", "BTCUSDT", 1, 1.0)).unwrap();
        j.append_cmd(2, &submitted("c1", 10)).unwrap();
        j.append_cmd(2, &accepted("c1", "v1", 11)).unwrap();
        j.append_cmd(3, &order_partial("c1", fill_ev("c1", "BTCUSDT", 0.3, 100.0, 12), 12))
            .unwrap();
        j.append_cmd(4, &order_filled("c1", fill_ev("c1", "BTCUSDT", 0.7, 110.0, 13), 13)).unwrap();
        // server-minted order cm: only an OrderFilled arrives (symbol resolved from the fill)
        j.append_cmd(5, &order_filled("cm", fill_ev("cm", "ETHUSDT", 2.0, 50.0, 20), 20)).unwrap();
        // server-minted order m1 that terminalizes with NO fill (the minted-coid gap case):
        // the empty-coid Submit write-ahead (skipped) → a MintedSubmit carrying the RESOLVED
        // request (minted coid m1, SOLUSDT limit) → accept → cancel. It must now be PERSISTED
        // with its symbol/terms from the MintedSubmit and its terminal status CANCELED.
        j.append_cmd(6, &submit("", "binance", "SOLUSDT", 1, 1.0)).unwrap();
        j.append_minted_submit(
            7,
            &OrderRequest {
                client_order_id: "m1".to_string(),
                venue: "binance".to_string(),
                symbol: "SOLUSDT".to_string(),
                side: 1,
                qty: 1.0,
                order_type: vike_model::OrderType::Limit,
                price: Some(100.0),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        j.append_cmd(8, &submitted("m1", 30)).unwrap();
        j.append_cmd(8, &accepted("m1", "vm1", 31)).unwrap();
        j.append_cmd(9, &canceled("m1", 32)).unwrap();
        // truly symbol-unresolvable order: a canceled event on a coid with no submit, no
        // MintedSubmit and no fill — still unlearnable, still skipped.
        j.append_cmd(10, &canceled("cx", 40)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut state = PassState::resume(dir.path());
    materialize_once(dir.path(), &store, &seq, &mut state).unwrap();

    // c1: latest snapshot is FILLED, symbol/side/type from the submit, qty accumulated 0.3+0.7,
    // avg_fill_px qty-weighted (0.3*100 + 0.7*110)/1.0 = 107, venue_order_id from the accept.
    let btc = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
    let last = btc.last().expect("c1 order materialized");
    assert_eq!(last.client_order_id, "c1");
    assert_eq!(last.status, "FILLED");
    assert_eq!(last.order_type, "limit");
    assert!((last.filled_qty - 1.0).abs() < 1e-9, "0.3 + 0.7 accumulated");
    assert!((last.avg_fill_px - 107.0).abs() < 1e-9, "qty-weighted avg");
    assert_eq!(last.venue_order_id.as_deref(), Some("v1"));

    // cm: symbol resolved from the fill, status FILLED
    let eth = store.scan_exec_orders("binance", "ETHUSDT").unwrap();
    assert_eq!(eth.last().unwrap().status, "FILLED");
    assert_eq!(eth.last().unwrap().client_order_id, "cm");

    // server-minted, never-filled order m1: NOW persisted under SOLUSDT (minted-coid gap fix) —
    // symbol/side/type from the MintedSubmit, terminal status CANCELED, venue_order_id from the
    // accept. This is exactly the case the module doc used to say was dropped.
    let sol = store.scan_exec_orders("binance", "SOLUSDT").unwrap();
    let m1 = sol.last().expect("server-minted no-fill order m1 is persisted");
    assert_eq!(m1.client_order_id, "m1");
    assert_eq!(m1.status, "CANCELED");
    assert_eq!(m1.symbol, "SOLUSDT");
    assert_eq!(m1.order_type, "limit");
    assert_eq!(m1.side, 1);
    assert!((m1.qty - 1.0).abs() < 1e-9);
    assert_eq!(m1.price, Some(100.0));
    assert_eq!(m1.venue_order_id.as_deref(), Some("vm1"));
    assert!((m1.filled_qty - 0.0).abs() < 1e-9, "m1 never filled");

    // truly symbol-unresolvable order cx: still never persisted (counted skipped)
    assert!(state.orders.skipped_unresolved >= 1, "the symbol-less canceled coid was skipped");

    // the recon read helper collapses to latest-status-per-coid
    let statuses =
        vike_data::exec_index::recent_order_statuses(&store, "binance", "BTCUSDT", 0).unwrap();
    assert_eq!(statuses.get("c1").map(String::as_str), Some("FILLED"));

    // and the recon read path now sees the server-minted, never-filled order m1 too — the
    // JournalView.orders visibility the minted-coid gap used to deny.
    let sol_statuses =
        vike_data::exec_index::recent_order_statuses(&store, "binance", "SOLUSDT", 0).unwrap();
    assert_eq!(sol_statuses.get("m1").map(String::as_str), Some("CANCELED"));

    // idempotent re-drain over the same window (lost checkpoint) → no duplicate order rows
    let btc_before = store.scan_exec_orders("binance", "BTCUSDT").unwrap().len();
    std::fs::remove_file(dir.path().join("materializer.ckpt")).unwrap();
    materialize_once(dir.path(), &store, &AtomicU64::new(0), &mut PassState::resume(dir.path()))
        .unwrap();
    assert_eq!(
        store.scan_exec_orders("binance", "BTCUSDT").unwrap().len(),
        btc_before,
        "re-drain is idempotent for orders too (per-window commit_key)"
    );
}

/// The Bracket-arm counterpart of `materializes_order_lifecycle_snapshots`: entry/SL/TP coids
/// are ALL minted inline in `apply_intent`'s `OrderIntent::Bracket` arm, never through
/// `Submit`/`SubmitBatch` — so before that arm gained its own `MintedSubmit` append, none of
/// the three legs had a symbol-resolving record other than a fill, and a leg that terminalized
/// WITHOUT ever filling (exactly the OCO-sibling-fills-so-I-get-canceled case a bracket exists
/// for) was silently dropped, same as the original Submit-arm gap. Here the SL leg is accepted
/// then canceled with no fill ever arriving; it must still be persisted, terms/symbol resolved
/// from its `MintedSubmit`.
#[test]
fn bracket_leg_canceled_without_filling_is_materialized() {
    let dir = tempfile::tempdir().unwrap();
    let spec = vike_model::BracketSpec {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    };
    let [entry, sl, tp] = vike_model::build_bracket(&spec, "e1", "sl1", "tp1");
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        // the three inline-minted legs, journaled exactly as apply_intent's Bracket arm now does
        j.append_minted_submit(1, &entry, None).unwrap();
        j.append_minted_submit(1, &sl, None).unwrap();
        j.append_minted_submit(1, &tp, None).unwrap();
        // SL is released to the venue (adapter's OrderSubmitted), accepted, then canceled (its
        // OCO sibling TP filled instead) — no fill event for SL ever arrives.
        j.append_cmd(2, &submitted("sl1", 10)).unwrap();
        j.append_cmd(2, &accepted("sl1", "vsl1", 11)).unwrap();
        j.append_cmd(3, &canceled("sl1", 12)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut state = PassState::resume(dir.path());
    materialize_once(dir.path(), &store, &seq, &mut state).unwrap();

    let rows = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
    let sl_row = rows
        .iter()
        .find(|r| r.client_order_id == "sl1")
        .expect("SL leg persisted from its MintedSubmit despite never filling");
    assert_eq!(sl_row.status, "CANCELED");
    assert_eq!(sl_row.symbol, "BTCUSDT");
    assert_eq!(sl_row.order_type, "stop");
    assert_eq!(sl_row.side, -1, "SL is the opposite side of the long entry");
    assert!((sl_row.qty - 2.0).abs() < 1e-9);
    assert_eq!(sl_row.trigger_price, Some(95.0));
    assert_eq!(sl_row.venue_order_id.as_deref(), Some("vsl1"));
    assert!((sl_row.filled_qty - 0.0).abs() < 1e-9, "SL never filled");

    // the entry and TP legs are ALSO persisted purely from their own MintedSubmit — the fix
    // closes the gap for every minted leg, not just the one under test. They sit at
    // INITIALIZED, not SUBMITTED: a leg with a registration record and no venue event yet is
    // EXACTLY what `gate_and_register` holds in the live registry (the adapter's own
    // `OrderSubmitted` is what advances it), and a held bracket leg has not been sent anywhere.
    let e1 = rows.iter().find(|r| r.client_order_id == "e1").expect("entry leg persisted too");
    let tp = rows.iter().find(|r| r.client_order_id == "tp1").expect("TP leg persisted too");
    assert_eq!(e1.status, "INITIALIZED");
    assert_eq!(tp.status, "INITIALIZED");
}

/// The skip guard is `symbol.is_empty() || venue.is_empty()`, and its `||` was pinned by
/// nothing: the existing `cx` case in `materializes_order_lifecycle_snapshots` leaves BOTH
/// fields empty, which `&&` skips just as happily.
///
/// The one-empty shape is not contrived — production MAKES it. `vike_model::orders::order`'s
/// `build_combo` sets `venue` from the spec and leaves `symbol` for the adapter to resolve, so
/// a combo leg that terminalizes before its symbol is known arrives here with venue set and
/// symbol empty. Under `&&` that row is not skipped: it is written into a partition keyed
/// `symbol=`, which `scan_exec_orders` can never address again. A durable row nothing can read
/// is worse than no row — recon asks this store what it knows.
#[test]
fn a_venue_without_a_symbol_is_skipped_not_written_to_an_unaddressable_partition() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        // The `build_combo` shape: venue known, symbol left to the adapter, then terminal.
        j.append_cmd(1, &submit("cb1", "deribit", "", 1, 1.0)).unwrap();
        j.append_cmd(2, &submitted("cb1", 10)).unwrap();
        j.append_cmd(3, &accepted("cb1", "vcb1", 11)).unwrap();
        j.append_cmd(4, &canceled("cb1", 12)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut state = PassState::resume(dir.path());
    materialize_once(dir.path(), &store, &seq, &mut state).unwrap();

    assert!(
        store.scan_exec_orders("deribit", "").unwrap().is_empty(),
        "a symbol-less row must not be written; that partition is unaddressable"
    );
    assert!(
        state.orders.skipped_unresolved >= 1,
        "and the skip must be COUNTED — that counter is the only observability this has"
    );
}

/// The FILL twin of the test above, and the one with teeth.
///
/// `append_exec_fills` refuses an empty symbol (#1311 — such a row is unattributable, and its
/// trade_ids would be missing from the reconcile seen-fill set, which turns a booked fill into
/// a `MissingFill` that `hybrid` auto-applies). That append is reached through `?`. So without
/// a skip in the fill loop, ONE unattributable fill fails the whole pass — every good fill and
/// order in the batch with it — and because the checkpoint never advances, the next pass reads
/// the same records and fails again. A permanently stuck materializer persists nothing.
///
/// The good rows in the SAME batch are the point of this test: it is not enough that the bad
/// row is dropped, the rest must still land.
#[test]
fn a_symbol_less_fill_is_skipped_without_failing_the_pass_for_the_good_rows() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        // One unattributable fill, and one perfectly good one in the same window.
        j.append_cmd(1, &fill("t-bad", "cx", "", 1.0, 100.0, 10)).unwrap();
        j.append_cmd(2, &fill("t-good", "c1", "BTCUSDT", 2.0, 200.0, 11)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut state = PassState::resume(dir.path());

    // The pass must SUCCEED. Before the skip existed this was `Err` and nothing was persisted.
    materialize_once(dir.path(), &store, &seq, &mut state)
        .expect("one unattributable fill must not fail the pass");

    let good = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
    assert_eq!(good.len(), 1, "the attributable fill still lands: {good:?}");
    assert_eq!(good[0].trade_id, "t-good");
    assert!(
        store.scan_exec_fills("binance", "").unwrap().is_empty(),
        "and the unattributable one is not written to an unaddressable partition"
    );
    assert!(state.orders.skipped_unresolved >= 1, "the skip is counted, not silent");
}

/// The lag signal's rules, on a clock the test holds: silent while caught up or briefly behind,
/// one warning when a bound is crossed, no repeat inside [`LAG_REWARN_EVERY`], and one line when it
/// catches up. A failed READ (which reports the checkpoint as its head) must not read as caught up.
#[test]
fn the_lag_watch_warns_past_a_bound_once_per_window_and_says_when_it_caught_up() {
    let t0 = Instant::now();
    let secs = Duration::from_secs;
    let mut w = LagWatch::default();
    assert_eq!(w.observe(10, 10, t0), None, "caught up is silent");
    assert_eq!(w.observe(20, 10, t0), None, "a small, fresh lag is silent");

    let late = t0 + LAG_WARN_AFTER + secs(1);
    assert_eq!(
        w.observe(20, 10, late),
        Some(LagEvent::Behind { lag: 10, behind_for: LAG_WARN_AFTER + secs(1), failing: false }),
        "the same small lag past the TIME bound is warned"
    );
    assert_eq!(w.observe(25, 10, late + secs(60)), None, "and not again a minute later");
    assert!(
        matches!(
            w.observe(25, 10, late + LAG_REWARN_EVERY),
            Some(LagEvent::Behind { lag: 15, .. })
        ),
        "a lag that persists is warned again once the window has passed"
    );
    assert_ne!(
        w.observe(10, 10, late + LAG_REWARN_EVERY + secs(1)),
        Some(LagEvent::CaughtUp),
        "a failed read reports the checkpoint as its head; the highest head seen still counts"
    );
    let up = late + LAG_REWARN_EVERY + secs(2);
    assert_eq!(w.observe(25, 25, up), Some(LagEvent::CaughtUp), "catching up is said once");
    assert_eq!(w.observe(25, 25, up + secs(1)), None, "...and only once");

    let mut w = LagWatch::default();
    assert!(
        matches!(w.observe(LAG_WARN_RECORDS + 1, 0, t0), Some(LagEvent::Behind { .. })),
        "a lag past the RECORD bound is warned at once, whatever its age"
    );
}

/// Append `msgs` to the journal in `dir` (opened, resumed, flushed and closed again), so a test can
/// grow the journal between two passes the way the trade daemon does.
fn append_journal(dir: &Path, msgs: &[Ingest]) {
    let mut j = CommandJournal::open(dir, JournalFileConfig::default()).unwrap();
    for (i, msg) in msgs.iter().enumerate() {
        j.append_cmd(i as i64, msg).unwrap();
    }
    j.flush().unwrap();
}

/// The trade ids of `(binance, symbol)`'s stored fills, in store order.
fn fill_ids(store: &dyn HistStore, symbol: &str) -> Vec<String> {
    store.scan_exec_fills("binance", symbol).unwrap().into_iter().map(|r| r.trade_id).collect()
}

/// A `MemHistStore` whose exec appends refuse from a chosen attempt on — the store half of a pass
/// that fails part-way. Reads always pass through.
struct FlakyStore {
    inner: MemHistStore,
    /// Exec appends attempted so far, refused ones included.
    attempts: AtomicUsize,
    /// The attempt from which appends refuse; `usize::MAX` = never.
    refuse_from: AtomicUsize,
}

impl FlakyStore {
    fn new() -> Self {
        FlakyStore {
            inner: MemHistStore::new(),
            attempts: AtomicUsize::new(0),
            refuse_from: AtomicUsize::new(usize::MAX),
        }
    }

    /// Let `n` more appends land, then refuse every one after.
    fn refuse_after(&self, n: usize) {
        let from = self.attempts.load(Ordering::SeqCst) + n;
        self.refuse_from.store(from, Ordering::SeqCst);
    }

    fn heal(&self) {
        self.refuse_from.store(usize::MAX, Ordering::SeqCst);
    }

    fn gate(&self, verb: &str) -> Result<(), DataError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt >= self.refuse_from.load(Ordering::SeqCst) {
            return Err(DataError::Io(format!("planted: {verb} refused")));
        }
        Ok(())
    }
}

impl HistStore for FlakyStore {
    fn append_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecFillRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.gate("append_exec_fills")?;
        self.inner.append_exec_fills(venue, symbol, rows, commit_key)
    }

    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.gate("append_exec_orders")?;
        self.inner.append_exec_orders(venue, symbol, rows, commit_key)
    }

    fn scan_exec_fills(&self, venue: &str, symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        self.inner.scan_exec_fills(venue, symbol)
    }

    fn scan_exec_orders(&self, venue: &str, symbol: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        self.inner.scan_exec_orders(venue, symbol)
    }

    vike_data::hist_store_stubs!(inert: load_bars, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, append_bars, append_quotes, append_trades,
        append_book_updates, append_symbol_properties, append_equity, resample_quotes_to_bars,
        resample_trades_to_bars);
}

/// **A pass whose checkpoint WRITE fails is not re-appended by the next pass.** The store dedups
/// by batch key only, and the key names the window, so a cursor left behind would re-read the
/// landed records next pass under a WIDER window — a new key — and append them again, every
/// interval. The cursor advances in memory once every append landed; the owed write is retried,
/// by a pass with nothing new too, and lands once the directory takes it.
#[test]
fn a_pass_whose_checkpoint_write_fails_is_not_re_appended_by_the_next_pass() {
    let dir = tempfile::tempdir().unwrap();
    append_journal(dir.path(), &[fill("t1", "c1", "BTCUSDT", 0.5, 100.0, 10)]);
    // `MaterializeCheckpoint::store` cannot create its temp file while a DIRECTORY holds the name.
    let blocker = dir.path().join("materializer.ckpt.tmp");
    std::fs::create_dir(&blocker).unwrap();
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut state = PassState::resume(dir.path());

    let failed = materialize_once(dir.path(), &store, &seq, &mut state)
        .expect_err("the checkpoint write is refused");
    assert_eq!(failed.stage, PassStage::Checkpoint);
    assert_eq!(fill_ids(&store, "BTCUSDT"), ["t1"], "the rows landed before the write failed");
    assert!(!MaterializeCheckpoint::exists(dir.path()), "...and no checkpoint was written");
    let landed = seq.load(Ordering::Relaxed);
    assert_eq!(failed.head, landed, "the cursor advanced to the landed head in memory");

    append_journal(dir.path(), &[fill("t2", "c1", "BTCUSDT", 0.5, 101.0, 20)]);
    let failed = materialize_once(dir.path(), &store, &seq, &mut state)
        .expect_err("the checkpoint write is still refused");
    assert_eq!(failed.stage, PassStage::Checkpoint);
    assert_eq!(
        fill_ids(&store, "BTCUSDT"),
        ["t1", "t2"],
        "the second pass starts AFTER the landed window: t1 is not appended again"
    );
    assert!(seq.load(Ordering::Relaxed) > landed);

    std::fs::remove_dir(&blocker).unwrap();
    let head = materialize_once(dir.path(), &store, &seq, &mut state)
        .expect("nothing new, and the owed checkpoint is written");
    assert_eq!(head, seq.load(Ordering::Relaxed));
    assert_eq!(MaterializeCheckpoint::load(dir.path()), head, "the file caught up with the cursor");
    assert_eq!(fill_ids(&store, "BTCUSDT"), ["t1", "t2"], "and still no duplicate");
}

/// **A pass that failed PART-WAY is retried without duplicating the batches that landed.** The BTC
/// batch lands, the ETH batch is refused; a record arriving before the retry would widen the window
/// and give BTC's batch a NEW key, so the retry is pinned to the failed pass's head: BTC repeats
/// its key (a store no-op), ETH lands, and the new record waits for the pass after.
#[test]
fn a_pass_that_failed_part_way_is_retried_without_duplicating_the_batches_that_landed() {
    let dir = tempfile::tempdir().unwrap();
    append_journal(
        dir.path(),
        &[fill("t1", "c1", "BTCUSDT", 0.5, 100.0, 10), fill("t2", "c2", "ETHUSDT", 1.0, 50.0, 11)],
    );
    let store = FlakyStore::new();
    store.refuse_after(1);
    let seq = AtomicU64::new(0);
    let mut state = PassState::resume(dir.path());

    let failed = materialize_once(dir.path(), &store, &seq, &mut state)
        .expect_err("the second batch is refused");
    assert_eq!(failed.stage, PassStage::Append);
    assert_eq!(fill_ids(&store, "BTCUSDT"), ["t1"], "the first batch landed");
    assert!(fill_ids(&store, "ETHUSDT").is_empty(), "the second did not");
    assert_eq!(seq.load(Ordering::Relaxed), 0, "the cursor did not move");
    assert!(!MaterializeCheckpoint::exists(dir.path()), "nor did the checkpoint");

    append_journal(dir.path(), &[fill("t3", "c1", "BTCUSDT", 0.5, 102.0, 20)]);
    store.heal();
    let retried = materialize_once(dir.path(), &store, &seq, &mut state).expect("the retry lands");
    assert_eq!(retried, failed.head, "the retry covers exactly the failed pass's window");
    assert_eq!(fill_ids(&store, "BTCUSDT"), ["t1"], "the landed batch is NOT appended again");
    assert_eq!(fill_ids(&store, "ETHUSDT"), ["t2"], "the refused one lands");

    let next = materialize_once(dir.path(), &store, &seq, &mut state).expect("the next pass");
    assert!(next > retried);
    assert_eq!(fill_ids(&store, "BTCUSDT"), ["t1", "t3"], "the waiting record lands once");
    assert_eq!(MaterializeCheckpoint::load(dir.path()), next);
}

/// **A failed pass leaves the order tracker as it was**, so its retries fold the same records ONCE:
/// before the pass folded into a copy, a refused append left the partial fill folded into the
/// tracker and every retry folded it again, inflating `filled_qty` (and the VWAP) in each
/// `exec_order` row it then wrote.
#[test]
fn a_failed_pass_leaves_the_order_tracker_as_it_was_so_its_retry_folds_once() {
    let dir = tempfile::tempdir().unwrap();
    append_journal(
        dir.path(),
        &[
            submit("c1", "binance", "BTCUSDT", 1, 1.0),
            submitted("c1", 10),
            accepted("c1", "v1", 11),
        ],
    );
    let store = FlakyStore::new();
    let seq = AtomicU64::new(0);
    let mut state = PassState::resume(dir.path());
    materialize_once(dir.path(), &store, &seq, &mut state).expect("the accepted order lands");
    let before = state.orders.rows.get("c1").map(|t| t.order.clone()).expect("c1 tracked");
    let skipped_before = state.orders.skipped_unresolved;

    append_journal(
        dir.path(),
        &[order_partial("c1", fill_ev("c1", "BTCUSDT", 0.3, 100.0, 12), 12), canceled("cx", 13)],
    );
    store.refuse_after(0);
    for attempt in 0..2 {
        let failed = materialize_once(dir.path(), &store, &seq, &mut state)
            .expect_err("every append is refused");
        assert_eq!(failed.stage, PassStage::Append);
        assert_eq!(
            state.orders.rows.get("c1").map(|t| &t.order),
            Some(&before),
            "failed pass {attempt} folded nothing into the tracker"
        );
        assert!(!state.orders.rows.contains_key("cx"), "...not even a new coid");
        assert_eq!(state.orders.skipped_unresolved, skipped_before, "...nor a skip count");
    }

    store.heal();
    materialize_once(dir.path(), &store, &seq, &mut state).expect("the retry lands");
    let c1 = &state.orders.rows.get("c1").expect("c1 tracked").order;
    assert!((c1.filled_qty - 0.3).abs() < 1e-9, "the partial folded ONCE: {}", c1.filled_qty);
    let rows = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
    let last = rows.last().expect("c1's snapshot landed");
    assert!((last.filled_qty - 0.3).abs() < 1e-9, "the stored row too: {}", last.filled_qty);
    assert!((last.avg_fill_px - 100.0).abs() < 1e-9, "and its VWAP: {}", last.avg_fill_px);
}

/// **The mount's write probe changes nothing it finds**, and refuses what the materializer could
/// not checkpoint into. An absent checkpoint must STAY absent: a present `0` would make the first
/// pass skip the seq-0 record and start pinning the trade daemon's prune.
#[test]
fn the_write_probe_changes_nothing_it_finds_and_refuses_an_unwritable_directory() {
    let dir = tempfile::tempdir().unwrap();
    probe_checkpoint_write(dir.path()).expect("a writable directory passes");
    assert!(!MaterializeCheckpoint::exists(dir.path()), "an absent checkpoint stays absent");
    assert!(!dir.path().join(PROBE_FILE).exists(), "the throwaway file is removed");

    MaterializeCheckpoint::store(dir.path(), 42).unwrap();
    probe_checkpoint_write(dir.path()).expect("a writable directory with a checkpoint passes");
    assert_eq!(MaterializeCheckpoint::load(dir.path()), 42, "a present checkpoint keeps its value");

    std::fs::create_dir(dir.path().join("materializer.ckpt.tmp")).unwrap();
    assert!(probe_checkpoint_write(dir.path()).is_err(), "a refused checkpoint write is a refusal");
    assert_eq!(MaterializeCheckpoint::load(dir.path()), 42, "...that changed nothing");

    let missing = dir.path().join("absent");
    assert!(probe_checkpoint_write(&missing).is_err(), "a missing directory is a refusal");
    assert!(!missing.exists(), "...and is not created: it is the trade daemon's");
}

/// **A persistent failure is warned once, then rate-limited, and its recovery said once.** A
/// failing READ reports the cursor as its head, so the lag alone stays 0 and never warned; the
/// failing passes now count as behind. The per-pass failure line is logged on the first failure
/// and on a change of stage, otherwise once per [`LAG_REWARN_EVERY`] carrying the count it skipped.
#[test]
fn a_persistent_failure_is_warned_once_then_rate_limited_and_its_recovery_is_said_once() {
    let t0 = Instant::now();
    let secs = Duration::from_secs;
    let mut w = LagWatch::default();
    let read_failed = |unlogged| Some(LagEvent::PassFailed { stage: PassStage::Read, unlogged });

    assert_eq!(w.pass_failed(PassStage::Read, t0), read_failed(0), "the first failure is logged");
    assert_eq!(w.observe(0, 0, t0), None, "a fresh failure is not yet a lag");
    let mut t = t0;
    for _ in 0..10 {
        t += secs(5);
        assert_eq!(w.pass_failed(PassStage::Read, t), None, "the same stage again is counted");
        assert_eq!(w.observe(0, 0, t), None, "...and is no lag inside the time bound");
    }

    let late = t0 + LAG_WARN_AFTER + secs(1);
    assert_eq!(w.pass_failed(PassStage::Read, late), None);
    assert_eq!(
        w.observe(0, 0, late),
        Some(LagEvent::Behind { lag: 0, behind_for: LAG_WARN_AFTER + secs(1), failing: true }),
        "failing past the time bound is warned, though no head was ever seen"
    );
    assert_eq!(w.pass_failed(PassStage::Read, late + secs(5)), None);
    assert_eq!(w.observe(0, 0, late + secs(5)), None, "...and not again inside the window");

    assert_eq!(
        w.pass_failed(PassStage::Read, t0 + LAG_REWARN_EVERY),
        read_failed(12),
        "the failure line repeats once per window, with the passes it did not log"
    );
    assert_eq!(
        w.pass_failed(PassStage::Append, t0 + LAG_REWARN_EVERY + secs(5)),
        Some(LagEvent::PassFailed { stage: PassStage::Append, unlogged: 0 }),
        "a change of stage is logged at once"
    );

    let up = t0 + LAG_REWARN_EVERY + secs(10);
    assert_eq!(
        w.pass_succeeded(up),
        Some(LagEvent::Recovered { failed_passes: 15, failing_for: LAG_REWARN_EVERY + secs(10) }),
        "the first pass that succeeds says so"
    );
    assert_eq!(w.pass_succeeded(up + secs(5)), None, "...once");
    assert_eq!(w.observe(0, 0, up + secs(5)), Some(LagEvent::CaughtUp), "the lag warning ends");
    assert_eq!(w.observe(0, 0, up + secs(10)), None, "...once too");
    assert_eq!(
        w.pass_failed(PassStage::Read, up + secs(15)),
        read_failed(0),
        "a NEW failure after the recovery is logged at once"
    );
}

/// A fill row carries the two ids a `FillEvent` does not: the VENUE's order id (from the order's
/// ack) and the ACCOUNT its submit named — and the order row carries the account too. A fill whose
/// order the tracker never saw reads `None` for both, the columns' own "not known", never a guess.
#[test]
fn fill_and_order_rows_carry_the_venue_order_id_and_the_account() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j = CommandJournal::open(dir.path(), JournalFileConfig::default()).unwrap();
        let alt = vike_model::accounts::account_keys::AccountLabel::parse("ALT").unwrap();
        j.append_cmd(
            1,
            &Ingest::Command(vike_exec::Command::Order(vike_exec::OrderIntent::Submit(Box::new(
                OrderRequest {
                    client_order_id: "c1".to_string(),
                    venue: "binance".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    side: 1,
                    qty: 1.0,
                    order_type: vike_model::OrderType::Limit,
                    price: Some(100.0),
                    account: Some(alt),
                    ..Default::default()
                },
            )))),
        )
        .unwrap();
        j.append_cmd(2, &submitted("c1", 10)).unwrap();
        j.append_cmd(3, &accepted("c1", "v1", 11)).unwrap();
        j.append_cmd(4, &fill("t1", "c1", "BTCUSDT", 1.0, 100.0, 12)).unwrap();
        // a fill for an order this tracker never saw (its submit is below any checkpoint)
        j.append_cmd(5, &fill("t2", "unknown", "BTCUSDT", 1.0, 100.0, 13)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    materialize_once(dir.path(), &store, &AtomicU64::new(0), &mut PassState::resume(dir.path()))
        .unwrap();

    let fills = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
    let known = fills.iter().find(|f| f.trade_id == "t1").expect("t1 materialized");
    assert_eq!(known.venue_order_id.as_deref(), Some("v1"), "learned from the order's ack");
    assert_eq!(known.account.as_deref(), Some("ALT"), "the account the submit named");
    let unknown = fills.iter().find(|f| f.trade_id == "t2").expect("t2 materialized");
    assert_eq!((unknown.venue_order_id.as_deref(), unknown.account.as_deref()), (None, None));

    let orders = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
    assert!(orders.iter().all(|o| o.account.as_deref() == Some("ALT")), "{orders:?}");
}
