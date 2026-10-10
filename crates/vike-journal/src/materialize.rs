//! `JournalMaterializer` — the off-path tail-follower that materializes the mmap command journal's
//! FILL records into the vike-data execution trade log (Tier-2, `kind=exec_fill`). It bridges the
//! durable WAL → the queryable parquet trade log, so reporting/recon read a columnar store instead
//! of re-scanning the WAL (unified-journaling #2).
//!
//! ⚠ This called itself "the execution-history sibling of
//! `vike_app_core::data::data_sink::CoreSinkAdapter` (which bridges live MARKET data onto the
//! store)" until 2026-09-28, and the pairing does not hold: that adapter forwards live market data
//! onto the CORE's ingest lanes and the GUI's book and tape stores, not onto a `HistStore`, and no
//! code has constructed one since the desktop lost its local core (#1610).
//!
//! It lives in `vike-journal`, beside the WAL it reads, and names `vike-data` for the `HistStore`
//! trait it writes through. ⚠ This said "It lives in `vike-ops`" — then the one crate that could
//! name both the WAL and `vike-data` — and that "the HEADLESS daemon (`vike-tradehub`) spawns it",
//! until 2026-09-28. The module moved here on 2026-09-25 (#2167), and no production root has
//! spawned it since 2026-09-22, when #2093 deleted the daemon's `materialize` feature (the GUI's
//! spawn had gone with its local core before that): the WAL is still written,
//! `tearsheet --journal DIR` reads it directly, and only this crate's own tests run the
//! materializer today. The store is injected as an `Arc<dyn HistStore>` so this crate needs no
//! DataFusion feature; a binary that spawns the materializer passes the concrete `DataFusionHist`.
//!
//! Contract (from the spec's Tier-2): a strict off-fold consumer of an already-enabled WAL. It
//! never touches the hot path. Durability is at-least-once via a persisted last-materialized seq
//! ([`crate::MaterializeCheckpoint`]) + the store's `commit_key` idempotency, so a
//! crash-and-resume re-processes only the un-materialized tail and never double-appends.
//!
//! SCOPE: FILLS (`Event::Fill` → `ExecFillRow`, self-contained) AND ORDER LIFECYCLE
//! (`Event::Order*` + Submit intents → `ExecOrderRow`, `kind=exec_order`). The order fold is
//! STATEFUL: an order spans many records (submit → accept → … → terminal), so a running
//! [`OrderTracker`] (`coid -> current snapshot`) is held on the writer thread and folded across
//! passes. It is rebuilt EMPTY on process restart — see the restart/staleness bound below.
//!
//! THE ORDER STATE IS THE REAL FSM, NOT A COPY OF IT. Each tracked order holds a
//! [`vike_exec::ManagedOrder`] and every lifecycle event is folded through its `apply` — the SAME
//! guarded mutator the live [`vike_exec::ExecutionEngine`] folds through, and the only one. This
//! module deliberately owns NO transition table, NO status strings of its own and NO fill
//! accumulator: `status` is `ManagedOrder::status.as_str()`, the resting terms are
//! `ManagedOrder::request`, and `filled_qty`/`avg_fill_px` are the FSM's own running VWAP.
//!
//! WHY THIS IS NOT A STYLE CHOICE. The WAL is WRITE-AHEAD — `spawn_core`'s fold appends the record
//! BEFORE `ExecutionEngine::on_event` folds it — so the journal necessarily contains events the
//! live FSM went on to REFUSE (`apply` -> `InvalidOrderTransition` -> `on_event` returns, event
//! dropped). A materializer that re-spells the fold WITHOUT the FSM's allowed-from guards therefore
//! records state changes that never happened. The concrete case this module used to get wrong: an
//! `OrderModified` arriving on an already-terminal order (a venue amend-ack racing the fill that
//! terminalized the order — the amend is a REST round trip, the fill is a one-hop WS push, so the
//! two land on the ingest lane in either order). `ManagedOrder::apply` refuses it
//! (`OrderStatus::MODIFIABLE` excludes every terminal), so the live order kept its terms; the old
//! copy applied it and appended an `exec_order` row carrying a qty/price the order never had.
//!
//! The ONE thing the FSM does not do for us is resolve the `exec_order` PARTITION KEY: a coid whose
//! row was created bare learns `(venue, symbol, side)` from the first fill that folds (see the
//! symbol-resolution note below). The live engine never needs that — it holds the `OrderRequest`
//! from `gate_and_register`.
//!
//! ORDER SYMBOL RESOLUTION: lifecycle events (`OrderAccepted`/`Canceled`/`Rejected`/…) carry ONLY
//! the client order id. An order's `(venue, symbol)` (the `exec_order` partition key) is learned
//! from ANY of three sources: an explicit non-empty coid on the Submit intent; the `FillEvent`
//! embedded in a fill event; OR — for a SERVER-MINTED (empty-coid) submit — the
//! [`JournalRecord::MintedSubmit`] record the runtime writes from inside `apply_intent` right after
//! it mints the coid (its write-ahead `Cmd` carried an empty coid, so this record is the durable
//! tie between the minted coid and the request's terms). That third source closes the former
//! minted-coid gap: a server-minted order that terminalizes WITHOUT ever filling is now persisted
//! too (it used to be dropped — its symbol was unlearnable). The only rows still skipped are truly
//! symbol-less coids first seen via a bare lifecycle event with neither a submit nor a
//! `MintedSubmit` nor a fill (`skipped_unresolved`, tracked for observability).
//!
//! RESTART/CRASH bound (staleness, NEVER corruption): the tracker rebuilds empty from the
//! post-checkpoint tail, so an order still open ACROSS a restart whose pre-restart submit/accept is
//! below the checkpoint may materialize a slightly stale snapshot until its next symbol-resolved
//! event; idempotency (the per-window `commit_key`) means a re-drain never double-appends.
//! Folding through the real FSM does NOT tighten that bound into a correctness loss: a coid first
//! seen via a bare lifecycle event is ADOPTED into the status that event's own allowed-from set
//! requires (see [`adopt_order`]) — the same insert-only adoption
//! `ExecutionEngine::reregister_orders` performs for a venue-reported order — so the post-restart
//! tail still terminalizes correctly, and every SUBSEQUENT event for that coid goes through the
//! guarded `apply` like any other.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::{JournalRecord, MaterializeCheckpoint, read_since, record_seq};
use indexmap::IndexMap;
use vike_data::worker::Worker;
use vike_data::{ExecFillRow, ExecOrderRow, HistStore};
use vike_exec::Ingest;
use vike_model::events::Event;

mod tracker;
#[cfg(doc)]
use tracker::adopt_order;
use tracker::{OrderTracker, exec_order_row};

#[cfg(test)]
mod testkit;

/// Materializer knobs. `interval` is how often the tail is drained (off-path, so seconds, not µs).
#[derive(Debug, Clone)]
pub struct MaterializerConfig {
    pub interval: Duration,
}

impl Default for MaterializerConfig {
    fn default() -> Self {
        MaterializerConfig { interval: Duration::from_secs(5) }
    }
}

/// Handle to the running materializer thread — hold it to keep materializing; `shutdown` (or drop)
/// does one final drain then joins.
///
/// The stop flag / interruptible sleep / join-on-drop machinery is the shared
/// [`vike_data::worker`] harness, so shutdown is now signalled through a `Condvar` rather than
/// polled: the thread leaves its inter-pass sleep the instant stop is raised instead of waiting out
/// a poll tick. Behavior is otherwise unchanged — `shutdown` (and the [`Drop`] that mirrors it)
/// still stops, lets the thread do its final drain, and joins.
pub struct MaterializerHandle {
    worker: Worker,
}

impl MaterializerHandle {
    /// Signal stop and join: the thread wakes immediately, does one final drain, then exits. Drop
    /// does the same, so a caller that just drops the handle loses nothing either.
    pub fn shutdown(mut self) {
        self.worker.stop();
    }
}

/// The materializer. Spawn it once per enabled WAL dir; it owns a writer thread that drains the WAL
/// tail on `cfg.interval` and appends fills to `store`.
pub struct JournalMaterializer {
    /// last-materialized seq (also mirrored durably in `<dir>/materializer.ckpt`) — for observability.
    materialized_seq: Arc<AtomicU64>,
}

impl JournalMaterializer {
    pub fn materialized_seq(&self) -> u64 {
        self.materialized_seq.load(Ordering::Relaxed)
    }

    /// Spawn the tail-follower. Resumes from the durable checkpoint (0 on a fresh dir).
    pub fn spawn(
        journal_dir: PathBuf,
        store: Arc<dyn HistStore + Send + Sync>,
        cfg: MaterializerConfig,
    ) -> (Arc<Self>, MaterializerHandle) {
        let seq = Arc::new(AtomicU64::new(MaterializeCheckpoint::load(&journal_dir)));
        let me = Arc::new(JournalMaterializer { materialized_seq: seq.clone() });

        let seq_t = seq.clone();
        let worker = Worker::spawn("vike-journal-mat", move |shared| {
            // Running order-lifecycle state, folded across passes (rebuilt empty here on restart).
            let mut orders = OrderTracker::default();
            loop {
                // one drain pass (errors are logged and retried next tick — at-least-once)
                if let Err(e) = materialize_once(&journal_dir, store.as_ref(), &seq_t, &mut orders)
                {
                    tracing::warn!(error = %e, "journal materialize pass failed; will retry");
                }
                // Interruptible sleep: shutdown cuts it short immediately (Condvar, not a poll).
                shared.sleep_interruptible(cfg.interval);
                if shared.is_stopped() {
                    // final drain so a clean shutdown loses nothing
                    let _ = materialize_once(&journal_dir, store.as_ref(), &seq_t, &mut orders);
                    break;
                }
            }
        });

        (me, MaterializerHandle { worker })
    }
}

/// Spawn a materializer iff the WAL is enabled — the one call a live binary makes. Returns `None`
/// when `journal_dir` is `None` (WAL off), so materialization enables/disables exactly with the
/// journal (the daemon's resolved `CoreConfig::journal`). The caller holds the `MaterializerHandle` for the process
/// lifetime (drop = final drain + join).
pub fn maybe_spawn(
    journal_dir: Option<PathBuf>,
    store: Arc<dyn HistStore + Send + Sync>,
    cfg: MaterializerConfig,
) -> Option<MaterializerHandle> {
    journal_dir.map(|dir| JournalMaterializer::spawn(dir, store, cfg).1)
}

/// One drain pass: read WAL records past the checkpoint, append their fills to Tier-2, advance the
/// checkpoint to the max seq READ (fills or not, so non-fill records are never re-scanned).
/// Idempotent: the per-`(venue,symbol)` `commit_key` embeds the seq window, and the checkpoint only
/// advances, so re-running a pass (or resuming after a crash) never double-appends.
fn materialize_once(
    dir: &std::path::Path,
    store: &(dyn HistStore + Send + Sync),
    seq_atomic: &AtomicU64,
    orders: &mut OrderTracker,
) -> Result<(), vike_data::DataError> {
    let after = seq_atomic.load(Ordering::Relaxed);
    // Cold start (no checkpoint yet) must INCLUDE seq 0 — `read_since` is strict `> after`, so a
    // fresh materializer reads the whole journal once; every pass after the first checkpoint uses
    // the incremental `read_since` (which also avoids the double-append hazard).
    let records = if MaterializeCheckpoint::exists(dir) {
        read_since(dir, after)
    } else {
        crate::CommandJournal::read_all(dir)
    }
    .map_err(|e| vike_data::DataError::Io(e.to_string()))?;
    if records.is_empty() {
        return Ok(());
    }

    let mut fills: IndexMap<(String, String), Vec<ExecFillRow>> = IndexMap::new();
    // coids whose order state changed THIS pass — their current snapshot is appended below.
    let mut touched: HashSet<String> = HashSet::new();
    let mut max_seq = after;
    for rec in &records {
        max_seq = max_seq.max(record_seq(rec));
        if let JournalRecord::Cmd { msg: Ingest::Event(Event::Fill(f)), .. } = rec {
            fills.entry((f.venue.to_string(), f.symbol.to_string())).or_default().push(
                ExecFillRow {
                    ts: f.ts,
                    trade_id: f.trade_id.to_string(),
                    client_order_id: f.client_order_id.clone(),
                    venue: f.venue.to_string(),
                    symbol: f.symbol.to_string(),
                    side: f.side,
                    qty: f.last_qty,
                    px: f.last_px,
                    commission: f.commission,
                    mark_price: f.mark_price,
                    liquidity_side: f.liquidity_side.to_string(),
                    commission_asset: f.commission_asset.to_string(),
                },
            );
        }
        // Order lifecycle fold (submit intents + Order* events) into the running tracker.
        orders.fold_record(rec, &mut touched);
    }

    // Append fills per (venue, symbol). The seq window in the commit key makes each pass's batch
    // unique and idempotent (a re-run of the SAME window is a store no-op).
    for ((venue, symbol), rows) in &fills {
        // SKIP-AND-COUNT, exactly as the order loop below does — and this arm is now load-bearing
        // rather than defensive. `append_exec_fills` REFUSES an empty symbol (that guard exists
        // because such a row is unattributable: no scan can address it, and its trade_ids would be
        // missing from the reconcile seen-fill set, which turns a booked fill into a `MissingFill`
        // that `hybrid` auto-applies). The append is reached through `?`, so without this skip one
        // unattributable fill would fail the WHOLE pass — taking every good fill and order in the
        // batch with it, leaving the checkpoint un-advanced, and making the next pass re-read the
        // same records and fail again. A permanently stuck materializer persists nothing at all,
        // which is far worse than dropping the one row that cannot be addressed.
        //
        // The order loop has always skipped this shape. The fill loop did not, and until the store
        // began refusing it the omission was invisible: the row simply landed in an unreadable
        // `symbol=` partition.
        if symbol.is_empty() || venue.is_empty() {
            orders.skipped_unresolved = orders.skipped_unresolved.saturating_add(1);
            continue;
        }
        let key = format!("mat-fill-{venue}-{symbol}-{after}-{max_seq}");
        store.append_exec_fills(venue, symbol, rows, Some(&key))?;
    }

    // Append the current snapshot of every order TOUCHED this pass whose symbol is resolved, grouped
    // by (venue, symbol). Symbol-unresolved touched coids (see the module note) are counted, skipped.
    let mut order_rows: IndexMap<(String, String), Vec<ExecOrderRow>> = IndexMap::new();
    for coid in &touched {
        let Some(tracked) = orders.rows.get(coid) else { continue };
        let req = &tracked.order.request;
        if req.symbol.is_empty() || req.venue.is_empty() {
            orders.skipped_unresolved = orders.skipped_unresolved.saturating_add(1);
            continue;
        }
        let key = (req.venue.clone(), req.symbol.clone());
        order_rows.entry(key).or_default().push(exec_order_row(tracked));
    }
    for ((venue, symbol), rows) in &order_rows {
        let key = format!("mat-order-{venue}-{symbol}-{after}-{max_seq}");
        store.append_exec_orders(venue, symbol, rows, Some(&key))?;
    }

    // Advance the checkpoint LAST (after successful appends) so a crash mid-pass re-drains this
    // window next time — at-least-once, made safe by the commit_key idempotency above.
    MaterializeCheckpoint::store(dir, max_seq)
        .map_err(|e| vike_data::DataError::Io(e.to_string()))?;
    seq_atomic.store(max_seq, Ordering::Relaxed);
    Ok(())
}

// ⚠ A private `fn record_seq(rec: &JournalRecord) -> u64` stood here and is DELETED as a
// DUPLICATE. `crate::record_seq` has always been the same match over the same eleven variants,
// `pub` and re-exported from this crate's root — but this module lived in `vike-ops` until
// 2026-09-25, a crate away, and `crates/vike-journal/src/record.rs`'s own decision table listed
// the two as separate sites ("the same, in the materializer's crate"). Moving the module in is
// what turned a cross-crate copy into a same-crate duplicate, which is the point of the move.
// ⚠ Its per-variant comments went with it, and nothing was lost: every one of them said "the fold
// above ignores this content", and `OrderTracker::fold_record` already states the same thing per
// variant and states it BETTER — it names the exec-log consequence (which record the row arrives
// through instead, and what double-counting would follow from adding one here) rather than only
// that the variant is skipped. The reasoning has one home now, and it is the one that folds.

#[path = "materialize_tests.rs"]
#[cfg(test)]
mod materialize_tests;
