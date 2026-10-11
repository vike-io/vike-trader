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
//! # Where it lives, and who runs it
//!
//! In `vike-datahub`, because it WRITES the store (`append_exec_fills`/`append_exec_orders`) and
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` gives the store one writer process.
//! It reads the WAL through `vike_journal`'s public reader half ([`CommandJournal::read_all`],
//! [`read_since`], [`record_seq`]) and resumes from [`MaterializeCheckpoint`], both of which stay in
//! vike-journal because the journal's own prune reads the checkpoint (below). The history: it was
//! `vike_ops::journal_mat` (the one crate that could name both the WAL and `vike-data`), then
//! `vike_journal::materialize` from 2026-09-25 (#2167), and this module since 2026-10-11. No
//! production root spawned it between 2026-09-22, when #2093 deleted the trade daemon's
//! `materialize` feature, and 2026-10-11, when the data daemon's
//! `crate::datahub_cli`'s `run` started calling [`maybe_spawn`] through its
//! `mount_journal_materializer` phase. That spawn is OFF by default: it needs the
//! `flags.journal_materializer` row on AND a `config.journal_dir` row, read from the settings
//! database the datahub boots from (on the CI box the one both daemons share). The store is injected as an
//! `Arc<dyn HistStore>`; the daemon hands it the same `DataFusionHist` handle it serves.
//!
//! # ⚠ The checkpoint is IN the journal directory, and that couples three things
//!
//! `<journal_dir>/materializer.ckpt` ([`MaterializeCheckpoint`]) is where this module resumes from,
//! and it is ALSO the retention floor of the journal's own prune: `vike_journal`'s
//! `CommandJournal::prune_before_latest_snap` keeps every segment holding a record past the
//! checkpoint for as long as the file exists, so a materializer that fell behind keeps its input.
//! It stays there on purpose — moving it would let the trade daemon prune segments this module has
//! not consumed. Three consequences follow, and none of them is enforced by code:
//!
//! 1. **The datahub process needs READ and WRITE access to the journal directory** — read for the
//!    segments, write for the checkpoint's temp-file-and-rename. The directory is the trade
//!    daemon's (under its `ReadWritePaths=`), so on a sandboxed box that is an ops step (a shared
//!    group or an ACL, plus the datahub unit's own `ReadWritePaths=`), never a code change. The
//!    mount checks it ONCE (`probe_checkpoint_write`) and refuses to spawn, with one warning,
//!    when the write is refused. A write that starts failing LATER is survived in-process (the
//!    cursor advances in memory, "What a failed pass leaves behind" below), but the file stays
//!    behind, so the lag below grows, and a RESTART in that state re-appends the window between
//!    the file and the in-memory cursor once: the store dedups by batch key, not by row.
//! 2. **A STOPPED materializer with a checkpoint present makes the journal keep segments without
//!    bound.** The prune cannot tell "behind" from "gone". Turning the materializer off for good
//!    means deleting `materializer.ckpt` as well; the datahub's startup line says so when it finds
//!    a checkpoint with the flag off.
//! 3. **A lag is therefore a disk-growth signal, not only a freshness one.** `LagWatch` warns —
//!    once on entering the state, then at most every [`LAG_REWARN_EVERY`], never per record — when
//!    the checkpoint trails the journal head by more than [`LAG_WARN_RECORDS`] records or has
//!    trailed it at all for longer than [`LAG_WARN_AFTER`]. The lag is measured against the
//!    checkpoint FILE (what the prune reads), not the in-memory cursor. Passes that keep FAILING
//!    count as behind even when no head was seen (a failed READ sees none), so a journal this
//!    process cannot read is warned about too. A pass that succeeds leaves the checkpoint AT the
//!    head, so in practice it fires when passes keep failing (a store refusing appends, a full
//!    disk, a checkpoint this process may not write). The per-pass failure line is bounded the same
//!    way: it is logged when the failing STAGE changes (read, append, checkpoint), otherwise at most
//!    every [`LAG_REWARN_EVERY`] with the count of failures it did not log, and the first pass that
//!    succeeds again says so once.
//!
//! # What a failed pass leaves behind
//!
//! The store dedups by BATCH key only (`vike_data::HistStore`'s "NEVER per-row value dedup"), and
//! the keys are `mat-fill-…-{after}-{max_seq}` / `mat-order-…-{after}-{max_seq}`: an EXACT replay
//! of a window is a no-op, a wider window is new rows. So a pass is made transactional in effect:
//!
//! - **It folds into a COPY of the [`OrderTracker`]** and commits the copy only after every append
//!   of the pass landed. A failed pass leaves the tracker as it was, so a retry does not fold the
//!   same fills twice into `filled_qty`/`avg_fill_px`.
//! - **A pass that failed after its read PINS its window** (`PassState`'s `pinned_upto`, in memory):
//!   every retry reads only records up to that pass's head, so it rebuilds the same groups under the
//!   same keys, the groups that already landed are store no-ops, and only the rest are written.
//!   Records that arrived meanwhile wait for the pass after.
//! - **Once every append landed, the cursor advances in memory even if the checkpoint WRITE
//!   fails**, so the next pass starts after it instead of re-reading it under a new key. The write
//!   is retried on every later pass, including one with nothing new.
//!
//! What it does NOT cover: a crash or restart between the last append and the checkpoint write
//! (or during a checkpoint outage) re-reads from the file's seq to a NEW head, a different key,
//! and re-appends that window once. Narrowing that needs a persisted pin beside the checkpoint.
//!
//! Contract (from the spec's Tier-2): a strict off-fold consumer of an already-enabled WAL. It
//! never touches the hot path. Durability is at-least-once via a persisted last-materialized seq
//! ([`MaterializeCheckpoint`]) + the store's `commit_key` dedup of an exact window. A `systemctl
//! stop` that kills the serve-only datahub mid-drain loses nothing: the checkpoint advances only
//! after the appends land (the window it re-reads may append once more; see above).
//!
//! SCOPE: FILLS (`Event::Fill` → `ExecFillRow`, self-contained) AND ORDER LIFECYCLE
//! (`Event::Order*` + Submit intents → `ExecOrderRow`, `kind=exec_order`). The order fold is
//! STATEFUL: an order spans many records (submit → accept → … → terminal), so a running
//! [`OrderTracker`] (`coid -> current snapshot`) is held on the writer thread and folded across
//! passes. It is rebuilt EMPTY on process restart — see the restart/staleness bound below.
//!
//! THE ORDER STATE IS THE REAL FSM, NOT A COPY OF IT. Each tracked order holds a
//! [`vike_model::ManagedOrder`] and every lifecycle event is folded through its `apply` — the SAME
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
//! event; the per-window `commit_key` makes an exact re-drain of a window a no-op (a wider one is
//! not: "What a failed pass leaves behind").
//! Folding through the real FSM does NOT tighten that bound into a correctness loss: a coid first
//! seen via a bare lifecycle event is ADOPTED into the status that event's own allowed-from set
//! requires (see [`adopt_order`]) — the same insert-only adoption
//! `ExecutionEngine::reregister_orders` performs for a venue-reported order — so the post-restart
//! tail still terminalizes correctly, and every SUBSEQUENT event for that coid goes through the
//! guarded `apply` like any other.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use vike_data::worker::Worker;
use vike_data::{ExecFillRow, ExecOrderRow, HistStore};
use vike_exec::Ingest;
use vike_journal::{CommandJournal, JournalRecord, MaterializeCheckpoint, read_since, record_seq};
use vike_model::events::Event;

mod tracker;
#[cfg(doc)]
use tracker::adopt_order;
use tracker::{OrderTracker, exec_order_row};

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
    /// The in-memory cursor: the last seq whose appends ALL landed — for observability. It equals
    /// `<dir>/materializer.ckpt` while the checkpoint writes succeed and runs AHEAD of the file while
    /// they fail (the module doc's "What a failed pass leaves behind").
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
        // What the writer thread carries across passes: the order tracker (rebuilt empty here on
        // restart), the cold-start flag, a failed pass's pinned window, the checkpoint file's seq.
        let mut state = PassState::resume(&journal_dir);
        let seq = Arc::new(AtomicU64::new(state.durable.unwrap_or(0)));
        let me = Arc::new(JournalMaterializer { materialized_seq: seq.clone() });

        let seq_t = seq.clone();
        let worker = Worker::spawn("dh-journal-mat", move |shared| {
            // The lag and failure signal — see the module doc's checkpoint section for why a lag
            // here is the journal directory GROWING, not only the exec log going stale.
            let mut lag_watch = LagWatch::default();
            loop {
                // One drain pass. A failure is retried next tick (at-least-once) and logged through
                // `lag_watch`'s bound, never once per pass.
                let outcome = materialize_once(&journal_dir, store.as_ref(), &seq_t, &mut state);
                let now = Instant::now();
                let (head, pass_event) = match &outcome {
                    Ok(head) => (*head, lag_watch.pass_succeeded(now)),
                    Err(failed) => (failed.head, lag_watch.pass_failed(failed.stage, now)),
                };
                if let Some(event) = pass_event {
                    log_event(&journal_dir, event, outcome.as_ref().err());
                }
                // Measured against the checkpoint FILE: it is what the trade daemon's prune reads.
                if let Some(event) = lag_watch.observe(head, state.durable.unwrap_or(0), now) {
                    log_event(&journal_dir, event, None);
                }
                // Interruptible sleep: shutdown cuts it short immediately (Condvar, not a poll).
                shared.sleep_interruptible(cfg.interval);
                if shared.is_stopped() {
                    // final drain so a clean shutdown loses nothing; one line if it cannot
                    if let Err(failed) =
                        materialize_once(&journal_dir, store.as_ref(), &seq_t, &mut state)
                    {
                        tracing::warn!(
                            dir = %journal_dir.display(),
                            stage = failed.stage.as_str(),
                            error = %failed.error,
                            "journal materializer's final drain at shutdown failed; the next start \
                             resumes from materializer.ckpt"
                        );
                    }
                    break;
                }
            }
        });

        (me, MaterializerHandle { worker })
    }
}

/// Spawn a materializer iff a journal directory is given — the one call the data daemon makes
/// (`crate::datahub_cli`'s `mount_journal_materializer` phase, which decides from its two rows
/// whether to pass a directory at all, and refuses one `probe_checkpoint_write` fails on). Returns
/// `None` for `None`. The caller holds the `MaterializerHandle` for the process lifetime (drop =
/// final drain + join).
pub fn maybe_spawn(
    journal_dir: Option<PathBuf>,
    store: Arc<dyn HistStore + Send + Sync>,
    cfg: MaterializerConfig,
) -> Option<MaterializerHandle> {
    journal_dir.map(|dir| JournalMaterializer::spawn(dir, store, cfg).1)
}

/// The file `probe_checkpoint_write` creates and removes when no checkpoint exists yet. Not a
/// `journal-*.vjl` name, so the journal's own reader never looks at it.
const PROBE_FILE: &str = "materializer.ckpt.probe";

/// Whether THIS process may write the journal directory `materializer.ckpt` lives in — asked once,
/// at mount, without changing what the directory means. The mount refuses to spawn on `Err`: a
/// materializer that can append but never checkpoint grows the journal directory and, after a
/// restart, re-appends what it had appended (the module doc's checkpoint section).
///
/// - A directory that is missing (or not a directory) is `Err`: it is the TRADE daemon's, and this
///   process does not create it.
/// - With a checkpoint present, the loaded value is stored BACK through the checkpoint's own write
///   (temp file, fsync, rename): the same seq, the same prune floor. A corrupt file loads as `0`
///   and is rewritten as `0`, which the next pass reads exactly as it would have read the corrupt
///   one (`read_since(dir, 0)`).
/// - With none, a throwaway file is created and removed instead: writing a `0` checkpoint would
///   change meaning (a PRESENT checkpoint makes the first pass skip the seq-0 record, and starts
///   pinning the trade daemon's prune before anything was materialized).
pub(crate) fn probe_checkpoint_write(dir: &Path) -> std::io::Result<()> {
    if !std::fs::metadata(dir)?.is_dir() {
        return Err(std::io::Error::other("the journal path is not a directory"));
    }
    if MaterializeCheckpoint::exists(dir) {
        return MaterializeCheckpoint::store(dir, MaterializeCheckpoint::load(dir));
    }
    let probe = dir.join(PROBE_FILE);
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe)?;
    std::fs::remove_file(&probe)
}

/// What the writer thread carries from one drain pass to the next, beside the shared cursor
/// (`materialize_once`'s `seq_atomic`). The module doc's "What a failed pass leaves behind" is the
/// contract these fields keep.
struct PassState {
    /// The order-lifecycle fold, as of the last pass whose appends ALL landed. A pass folds into a
    /// clone and replaces this only on success.
    orders: OrderTracker,
    /// No checkpoint existed at start and no pass has landed since: the next read is the whole
    /// journal, seq 0 included (`read_since` is strict `>`). Decided in memory, NOT from the file,
    /// so a checkpoint this process cannot write does not make every pass re-read everything.
    cold: bool,
    /// The head of a pass that read records and then failed before all its appends landed. Every
    /// retry reads only records up to it, so it rebuilds the SAME batches under the SAME keys and
    /// the batches that already landed are store no-ops. Cleared when the pass lands.
    pinned_upto: Option<u64>,
    /// What `<dir>/materializer.ckpt` holds, as far as this process knows: `None` when absent.
    durable: Option<u64>,
}

impl PassState {
    /// The state a (re)started materializer begins from: an empty tracker and the checkpoint file.
    fn resume(dir: &Path) -> Self {
        let exists = MaterializeCheckpoint::exists(dir);
        PassState {
            orders: OrderTracker::default(),
            cold: !exists,
            pinned_upto: None,
            durable: exists.then(|| MaterializeCheckpoint::load(dir)),
        }
    }

    /// Write the checkpoint when it is OWED: a pass has landed (never while cold — a `0` written on
    /// an empty journal would make the first real pass skip seq 0) and the file does not already
    /// hold `seq`. Retried by every later pass until it succeeds.
    fn store_owed_checkpoint(&mut self, dir: &Path, seq: u64) -> Result<(), PassFailure> {
        if self.cold || self.durable == Some(seq) {
            return Ok(());
        }
        // ⚠ This is the WRITE the datahub process needs on the journal directory (the module doc's
        // checkpoint section).
        MaterializeCheckpoint::store(dir, seq).map_err(|e| PassFailure {
            stage: PassStage::Checkpoint,
            head: seq,
            error: vike_data::DataError::Io(e.to_string()),
        })?;
        self.durable = Some(seq);
        Ok(())
    }
}

/// One drain pass: read WAL records past the cursor, append their fills and touched orders to
/// Tier-2, advance the cursor to the max seq READ (fills or not, so non-fill records are never
/// re-scanned), then write the checkpoint.
///
/// Transactional in effect (the module doc's "What a failed pass leaves behind"): the order fold
/// runs on a clone of `state.orders` committed only once every append landed; a pass that fails
/// after its read pins its window, so a retry repeats the same `commit_key`s and the batches that
/// already landed are no-ops; and once every append landed the cursor advances even if the
/// checkpoint write then fails.
///
/// `Ok` carries the journal HEAD the pass read up to (the cursor itself when nothing was new); a
/// [`PassFailure`] carries the head the pass had read before it failed. Either way the head is what
/// `LagWatch` measures the checkpoint against.
fn materialize_once(
    dir: &Path,
    store: &(dyn HistStore + Send + Sync),
    seq_atomic: &AtomicU64,
    state: &mut PassState,
) -> Result<u64, PassFailure> {
    let after = seq_atomic.load(Ordering::Relaxed);
    // Cold start must INCLUDE seq 0 — `read_since` is strict `> after`, so a fresh materializer
    // reads the whole journal once; every pass after the first that lands uses `read_since`.
    let records = if state.cold { CommandJournal::read_all(dir) } else { read_since(dir, after) };
    // A read that failed saw no head: report the cursor, so the lag is measured from what an
    // earlier pass saw (`LagWatch` keeps the highest head it was ever shown, and counts the failing
    // passes themselves as behind).
    let mut records = records.map_err(|e| PassFailure {
        stage: PassStage::Read,
        head: after,
        error: vike_data::DataError::Io(e.to_string()),
    })?;
    if let Some(upto) = state.pinned_upto {
        // A RETRY of a failed pass: the same window, so the same keys (records past it wait).
        records.retain(|rec| record_seq(rec) <= upto);
    } else if records.is_empty() {
        // Nothing new; a checkpoint an earlier pass could not write is still owed.
        state.store_owed_checkpoint(dir, after)?;
        return Ok(after);
    }
    let max_seq =
        records.iter().map(record_seq).fold(after, u64::max).max(state.pinned_upto.unwrap_or(0));
    // Pinned BEFORE the first append, so any failure below leaves the window for the retry.
    state.pinned_upto = Some(max_seq);

    // The order fold runs on a COPY, committed below only once every append landed.
    let mut orders = state.orders.clone();
    let mut fills: IndexMap<(String, String), Vec<ExecFillRow>> = IndexMap::new();
    // coids whose order state changed THIS pass — their current snapshot is appended below.
    let mut touched: HashSet<String> = HashSet::new();
    for rec in &records {
        // Order lifecycle fold (submit intents + Order* events) into the pass's copy — FIRST, so a
        // fill row below can read the venue order id and the account its own record taught it.
        orders.fold_record(rec, &mut touched);
        if let JournalRecord::Cmd { msg: Ingest::Event(Event::Fill(f)), .. } = rec {
            // The two ids a `FillEvent` does not carry, read off the order it belongs to: the
            // VENUE's order id (learned from its ack) and the ACCOUNT the submit named. `None` when
            // the tracker never saw the order (a fill whose submit is below the checkpoint), which
            // is the column's own "not known" — never a guess.
            let order = orders.rows.get(&f.client_order_id).map(|t| &t.order);
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
                    venue_order_id: order.and_then(|o| o.venue_order_id.clone()),
                    account: order.and_then(|o| o.request.account.as_ref()).map(|a| a.to_string()),
                },
            );
        }
    }

    // Append fills per (venue, symbol). The seq window in the commit key names each pass's batch,
    // and a re-run of the SAME window (a pinned retry, or a cold re-drain) is a store no-op.
    for ((venue, symbol), rows) in &fills {
        // SKIP-AND-COUNT, exactly as the order loop below does — and this arm is now load-bearing
        // rather than defensive. `append_exec_fills` REFUSES an empty symbol (that guard exists
        // because such a row is unattributable: no scan can address it, and its trade_ids would be
        // missing from the reconcile seen-fill set, which turns a booked fill into a `MissingFill`
        // that `hybrid` auto-applies). The append is reached through `?`, so without this skip one
        // unattributable fill would fail the WHOLE pass — taking every good fill and order in the
        // batch with it, leaving the cursor un-advanced, and making the next pass re-read the same
        // records and fail again. A permanently stuck materializer persists nothing at all, which
        // is far worse than dropping the one row that cannot be addressed.
        //
        // The order loop has always skipped this shape. The fill loop did not, and until the store
        // began refusing it the omission was invisible: the row simply landed in an unreadable
        // `symbol=` partition.
        if symbol.is_empty() || venue.is_empty() {
            orders.skipped_unresolved = orders.skipped_unresolved.saturating_add(1);
            continue;
        }
        let key = format!("mat-fill-{venue}-{symbol}-{after}-{max_seq}");
        store.append_exec_fills(venue, symbol, rows, Some(&key)).map_err(|error| PassFailure {
            stage: PassStage::Append,
            head: max_seq,
            error,
        })?;
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
        store.append_exec_orders(venue, symbol, rows, Some(&key)).map_err(|error| PassFailure {
            stage: PassStage::Append,
            head: max_seq,
            error,
        })?;
    }

    // Every append landed: commit the fold, release the pin, advance the cursor — in memory first,
    // so a checkpoint write that fails next does not make the following pass re-read this window
    // under a new key.
    state.orders = orders;
    state.pinned_upto = None;
    state.cold = false;
    seq_atomic.store(max_seq, Ordering::Relaxed);
    state.store_owed_checkpoint(dir, max_seq)?;
    Ok(max_seq)
}

// ⚠ A private `fn record_seq(rec: &JournalRecord) -> u64` stood here and was DELETED as a
// DUPLICATE on 2026-09-25: `vike_journal::record_seq` is the same match over the same variants,
// `pub` at that crate's root, and `crates/vike-journal/src/record.rs`'s decision table had listed
// the two as separate sites. It stays deleted now that this module is a crate away again — the
// import above names the one spelling. Its per-variant comments went with it, and nothing was
// lost: `OrderTracker::fold_record` states the same thing per variant and names the exec-log
// consequence. The reasoning has one home, and it is the one that folds.

/// Where a drain pass stopped — what the bounded failure line compares, rather than the error
/// TEXT (an error that embeds a changing number would otherwise be "new" every pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassStage {
    /// Reading the journal segments.
    Read,
    /// Appending a fill or order batch to the store.
    Append,
    /// Writing `materializer.ckpt` (every append of the pass had landed).
    Checkpoint,
}

impl PassStage {
    fn as_str(self) -> &'static str {
        match self {
            PassStage::Read => "read",
            PassStage::Append => "append",
            PassStage::Checkpoint => "checkpoint",
        }
    }
}

/// A drain pass that did not finish: where, the error, and the journal HEAD it had read up to
/// before it failed (the cursor's own value when the READ failed), which `LagWatch` measures
/// against.
#[derive(Debug)]
struct PassFailure {
    stage: PassStage,
    head: u64,
    error: vike_data::DataError,
}

/// The lag bound in journal RECORDS: the checkpoint trailing the journal head by more than this is
/// a lag at once, whatever its age. 100 000 records is days of a quiet box's traffic and minutes of
/// a busy one's, and every one of them is a record the trade daemon's prune must keep on disk.
pub const LAG_WARN_RECORDS: u64 = 100_000;

/// The lag bound in TIME: a checkpoint that has trailed the journal head at all — or passes that
/// have kept failing — for longer than this is a lag, however small the gap. Five minutes is sixty
/// passes at the default [`MaterializerConfig::interval`] of five seconds — no transient failure
/// lasts that long.
pub const LAG_WARN_AFTER: Duration = Duration::from_secs(5 * 60);

/// How often a lag, or a pass failure at the same stage, that PERSISTS is logged again. The first
/// line fires on entering the state; after that one line per fifteen minutes, never one per pass
/// and never one per record.
pub const LAG_REWARN_EVERY: Duration = Duration::from_secs(15 * 60);

/// What one `LagWatch` call has to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LagEvent {
    /// The checkpoint trails the head past a bound (`lag` records, for `behind_for`), or passes have
    /// kept failing for longer than [`LAG_WARN_AFTER`] (`failing`).
    Behind { lag: u64, behind_for: Duration, failing: bool },
    /// It was warned about and has caught up — said once, so the warning has an end in the log.
    CaughtUp,
    /// The bounded per-pass failure line: a pass failed at `stage`, and `unlogged` failures since
    /// the previous line were counted, not logged.
    PassFailed { stage: PassStage, unlogged: u64 },
    /// The first pass that succeeded after `failed_passes` failing ones over `failing_for`.
    Recovered { failed_passes: u64, failing_for: Duration },
}

/// The lag and failure signal's state, held on the writer thread beside the tracker. Pure over its
/// inputs (the clock is a parameter), so its rules are unit-tested without a thread or a sleep.
#[derive(Debug, Default)]
struct LagWatch {
    /// The highest journal head any pass has reported. A failed READ reports the cursor, and
    /// keeping the maximum stops that from reading as "caught up".
    head: u64,
    /// When the checkpoint first trailed the head in the current episode; `None` while caught up.
    behind_since: Option<Instant>,
    /// When this episode was last warned about; `None` until it is.
    last_warn: Option<Instant>,
    /// When the current run of FAILED passes began; `None` while passes succeed. A failing pass
    /// counts as behind whatever the lag says, since a failed read sees no head at all.
    failing_since: Option<Instant>,
    /// Failed passes in the current run, for the recovery line.
    failed_passes: u64,
    /// The stage and time of the last failure line logged; `None` until one is.
    last_failure_line: Option<(PassStage, Instant)>,
    /// Failed passes since that line that were counted and not logged.
    unlogged: u64,
}

impl LagWatch {
    /// A pass failed at `stage`: the failure line is due on the first failure, when the stage
    /// changes, and otherwise once per [`LAG_REWARN_EVERY`].
    fn pass_failed(&mut self, stage: PassStage, now: Instant) -> Option<LagEvent> {
        self.failing_since.get_or_insert(now);
        self.failed_passes = self.failed_passes.saturating_add(1);
        let due = match self.last_failure_line {
            None => true,
            Some((last, at)) => {
                last != stage || now.saturating_duration_since(at) >= LAG_REWARN_EVERY
            }
        };
        if !due {
            self.unlogged = self.unlogged.saturating_add(1);
            return None;
        }
        self.last_failure_line = Some((stage, now));
        Some(LagEvent::PassFailed { stage, unlogged: std::mem::take(&mut self.unlogged) })
    }

    /// A pass succeeded: the recovery line, once, if passes had been failing.
    fn pass_succeeded(&mut self, now: Instant) -> Option<LagEvent> {
        let since = self.failing_since.take()?;
        self.last_failure_line = None;
        self.unlogged = 0;
        Some(LagEvent::Recovered {
            failed_passes: std::mem::take(&mut self.failed_passes),
            failing_for: now.saturating_duration_since(since),
        })
    }

    /// Fold one pass's `head` and the checkpoint after it (`materialized`) at `now`. Call it after
    /// `pass_failed`/`pass_succeeded` for the same pass.
    fn observe(&mut self, head: u64, materialized: u64, now: Instant) -> Option<LagEvent> {
        self.head = self.head.max(head);
        let lag = self.head.saturating_sub(materialized);
        let failing = self.failing_since.is_some();
        if lag == 0 && !failing {
            self.behind_since = None;
            return self.last_warn.take().map(|_| LagEvent::CaughtUp);
        }
        let behind_since = *self.behind_since.get_or_insert(now);
        let since = self.failing_since.map_or(behind_since, |f| f.min(behind_since));
        let behind_for = now.saturating_duration_since(since);
        if lag <= LAG_WARN_RECORDS && behind_for <= LAG_WARN_AFTER {
            return None;
        }
        let due = match self.last_warn {
            None => true,
            Some(t) => now.saturating_duration_since(t) >= LAG_REWARN_EVERY,
        };
        if !due {
            return None;
        }
        self.last_warn = Some(now);
        Some(LagEvent::Behind { lag, behind_for, failing })
    }
}

/// The one place a `LagEvent` becomes a log line. `failure` is the pass's error, for `PassFailed`.
fn log_event(dir: &Path, event: LagEvent, failure: Option<&PassFailure>) {
    match event {
        LagEvent::PassFailed { stage, unlogged } => tracing::warn!(
            dir = %dir.display(),
            stage = stage.as_str(),
            error = %failure.map_or_else(String::new, |f| f.error.to_string()),
            unlogged_failures = unlogged,
            "journal materialize pass failed; will retry (repeated only when the failing stage \
             changes or once per LAG_REWARN_EVERY; unlogged_failures counts the passes between)"
        ),
        LagEvent::Recovered { failed_passes, failing_for } => tracing::info!(
            dir = %dir.display(),
            failed_passes,
            failing_secs = failing_for.as_secs(),
            "journal materialize passes succeed again"
        ),
        LagEvent::Behind { lag, behind_for, failing } => tracing::warn!(
            dir = %dir.display(),
            lag_records = lag,
            behind_secs = behind_for.as_secs(),
            passes_failing = failing,
            "journal materializer is BEHIND the journal head. While materializer.ckpt trails it, \
             the trade daemon's prune keeps every segment past it, so the journal directory GROWS: \
             read the pass-failure lines (the store, the disk, this process's access to that \
             directory)"
        ),
        LagEvent::CaughtUp => tracing::info!(
            dir = %dir.display(),
            "journal materializer caught up with the journal head"
        ),
    }
}

#[cfg(test)]
mod testkit;

#[path = "materialize_tests.rs"]
#[cfg(test)]
mod materialize_tests;
