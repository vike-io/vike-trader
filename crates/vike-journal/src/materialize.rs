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
use vike_exec::lanes::{Command, Ingest, OrderIntent};
use vike_exec::{ManagedOrder, OrderStatus};
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent};

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
/// journal (`journal_config_from_env`). The caller holds the `MaterializerHandle` for the process
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

/// One tracked order: the REAL FSM aggregate plus the one column it does not carry.
///
/// [`ManagedOrder`] is the whole order state — status, resting terms, venue order id, the running
/// fill VWAP. The only thing beside it is `ts`, the materializer-owned monotonic row timestamp
/// (`ExecOrderRow.ts`, what `vike_data::exec_index::recent_order_statuses` collapses on): the FSM
/// is timeless, and an out-of-order event must never rewind the durable row's clock.
struct TrackedOrder {
    order: ManagedOrder,
    ts: i64,
}

/// Running order-lifecycle state the materializer folds the WAL into (`coid -> current snapshot`).
/// Held by the writer thread across drain passes; rebuilt empty on process restart (see the module
/// note's restart/staleness bound).
#[derive(Default)]
struct OrderTracker {
    rows: IndexMap<String, TrackedOrder>,
    /// touched-but-symbol-unresolved coids skipped this process run (observability; see module note).
    skipped_unresolved: u64,
    /// Lifecycle events `ManagedOrder::apply` REFUSED this process run — the events the live engine
    /// also dropped (`ExecutionEngine::on_event`'s `apply_result.is_err() -> return`). Counted, not
    /// applied: before the FSM fold this module applied them silently, writing history that never
    /// happened. Observability only; a nonzero count is normal (idempotent/out-of-order WS replays).
    dropped_invalid: u64,
}

impl OrderTracker {
    /// Fold ONE journal record into the tracker, recording every coid whose row changed in `touched`.
    fn fold_record(&mut self, rec: &JournalRecord, touched: &mut HashSet<String>) {
        match rec {
            JournalRecord::Cmd { msg: Ingest::Command(Command::Order(intent)), now_ms, .. } => {
                self.seed_from_intent(intent, *now_ms, touched);
            }
            JournalRecord::StrategySubmit { intent, now_ms, .. } => {
                self.seed_from_intent(intent, *now_ms, touched);
            }
            // Server-minted submit: the resolved request, journaled AFTER its coid was minted (the
            // write-ahead `Cmd` for the SAME order carried an empty coid we skip). This is what lets
            // a server-minted order that terminalizes without ever filling be seeded — folded exactly
            // like a non-empty-coid Submit intent (its coid is now the minted one).
            JournalRecord::MintedSubmit { req, now_ms, .. } => {
                self.seed_from_intent(
                    &OrderIntent::Submit(Box::new(req.clone())),
                    *now_ms,
                    touched,
                );
            }
            JournalRecord::Cmd { msg: Ingest::Event(ev), .. } => {
                self.fold_event(ev, touched);
            }

            // ── EXHAUSTIVE by design: NO `_` arm. A new `JournalRecord` variant must fail to
            // compile HERE until someone decides whether it feeds the exec log — see the "Adding a
            // variant" contract on [`crate::JournalRecord`]. ⚠ This said the materializer lived in a
            // DIFFERENT crate from the one owning the enum, "which is precisely how a variant used to
            // slip past unnoticed (PR #915)". Since 2026-09-25 it is the SAME crate, so a new variant
            // and this match are one compile apart rather than two — the exhaustiveness below still
            // does the work, but the distance that made #915 possible is gone. Everything below is
            // deliberately ignored:
            //
            // A `Cmd` carrying any OTHER `Ingest` — a non-`Order` command (a Cancel/Modify/… is
            // materialized from the venue's own authoritative `Order*` EVENT, folded by the arm
            // above, never from the local intent), an `Ingest::Watchdog`, or a market-data message
            // (never journaled at all). The refined `Cmd` arms above take the two that matter; this
            // is their complement and is what keeps the match exhaustive over `Cmd`.
            JournalRecord::Cmd { .. } => {}
            // A full-state checkpoint. Its `engines` payload is a restore base for `vike-core`, not
            // an exec-log event stream: every order it describes already materialized from the
            // records that built it, so folding it would re-append stale snapshots.
            JournalRecord::Snap { .. } => {}
            // A periodic portfolio OBSERVATION (equity/balance/positions) — no order, no fill.
            JournalRecord::PortfolioSnap { .. } => {}
            // An emulated conditional's ARM / DISARM. Nothing reached a venue, so there is no
            // `exec_order` row: an armed stop is local runtime state until it FIRES.
            JournalRecord::ConditionalArmed { .. } | JournalRecord::ConditionalDisarmed { .. } => {}
            // A conditional's FIRE and a margin-call LIQUIDATION both RELEASE an order — but with
            // an empty coid, which `seed_from_intent` cannot key on. Both materialize through the
            // `MintedSubmit` (+ later `Fill`) records `apply_intent` writes for them immediately
            // after minting, so folding them here as well would double-seed the same order.
            JournalRecord::ConditionalFire { .. } | JournalRecord::MarginCallLiquidate { .. } => {}
            // A managed GTD/Day expiry DECISION. The cancel it decided reaches the exec log through
            // the venue's own authoritative `OrderCanceled` event; a row here would double-count it
            // (and would land BEFORE the venue confirmed anything).
            JournalRecord::GtdExpire { .. } => {}
            // A wall-clock schedule FIRE decision. The orders `on_schedule` produced arrive as
            // their own `StrategySubmit`/`MintedSubmit`/`Fill` records, folded by the arms above.
            JournalRecord::ScheduleFire { .. } => {}
        }
    }

    /// Seed/enrich rows from a Submit-carrying intent. Empty-coid (server-minted-later) requests are
    /// skipped — they cannot be keyed here (the mint happens in the fold, after this record).
    fn seed_from_intent(
        &mut self,
        intent: &OrderIntent,
        now_ms: i64,
        touched: &mut HashSet<String>,
    ) {
        let reqs: &[OrderRequest] = match intent {
            OrderIntent::Submit(r) => std::slice::from_ref(r.as_ref()),
            OrderIntent::SubmitBatch(rs) => rs.as_slice(),
            _ => &[],
        };
        for r in reqs {
            if r.client_order_id.is_empty() {
                continue;
            }
            let tracked = self.rows.entry(r.client_order_id.clone()).or_insert_with(|| {
                // EXACTLY `ExecutionEngine::gate_and_register`'s registration: a freshly-submitted
                // order enters the FSM at INITIALIZED. The adapter's own `OrderSubmitted` — emitted
                // BEFORE the venue round trip ("Submitted → REST → Accepted|Rejected", the
                // emitter-split contract `bridge_conformance.rs` machine-checks) and journaled on
                // the ingest lane like every other venue event — is what advances it to SUBMITTED.
                TrackedOrder { order: ManagedOrder::new(r.clone()), ts: now_ms }
            });
            // Identity/terms come from the submit; fill them if unset (do not clobber a later
            // fill's accumulated qty/px or a modify's rewritten resting terms).
            let req = &mut tracked.order.request;
            if req.symbol.is_empty() {
                req.venue = r.venue.clone();
                req.symbol = r.symbol.clone();
                req.side = r.side;
                req.qty = r.qty;
                req.order_type = r.order_type.clone();
                req.price = r.price;
                req.trigger_price = r.trigger_price;
            }
            tracked.ts = tracked.ts.max(now_ms);
            touched.insert(r.client_order_id.clone());
        }
    }

    /// Fold one `Order*` lifecycle event into the coid's order — through
    /// [`ManagedOrder::apply`], the live engine's ONE mutator, guards included. A coid seen here
    /// for the first time is adopted first (see [`adopt_order`]).
    ///
    /// An event `apply` REFUSES changes nothing: the order keeps its state, the row's `ts` does not
    /// advance, and the coid is NOT marked touched — so no `exec_order` row is appended for it.
    /// That is exactly what the live engine did with the same record (`on_event` returns on
    /// `apply_result.is_err()`), which is the entire point of folding through the FSM.
    fn fold_event(&mut self, ev: &Event, touched: &mut HashSet<String>) {
        // ONE match over the lifecycle events, for the three things the FSM does NOT give us: the
        // row key, the durable row's monotonic ts, and — for the two fill wraps — the embedded
        // `FillEvent` a bare row still needs for its `(venue, symbol)` partition key. Everything
        // else about this fold is `apply`'s answer.
        //
        // `Event::OrderCancelRejected`/`OrderModifyRejected` are deliberately NOT here: the FSM
        // treats both as advisories that change neither status nor terms, so folding them could
        // only manufacture a row for a coid nothing else in this window mentions.
        let (coid, ts, fill): (&str, i64, Option<&FillEvent>) = match ev {
            Event::OrderSubmitted(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderAccepted(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderTriggered(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderPartiallyFilled(e) => (e.client_order_id.as_str(), e.ts, Some(&e.fill)),
            Event::OrderFilled(e) => (e.client_order_id.as_str(), e.ts, Some(&e.fill)),
            Event::OrderCanceled(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderRejected(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderExpired(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderLiquidated(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderDenied(e) => (e.client_order_id.as_str(), e.ts, None),
            Event::OrderModified(e) => (e.client_order_id.as_str(), e.ts, None),
            _ => return,
        };
        let tracked = self.rows.entry(coid.to_string()).or_insert_with(|| adopt_order(coid, ev));
        // THE fold. No transition table here, no status strings, no fill accumulator — `apply` is
        // the live FSM and is the only mutator of order state on either side of this seam.
        let outcome = tracked.order.apply(ev);
        if outcome.is_ok() {
            // MATERIALIZER-ONLY, not an FSM concern: the `exec_order` partition key. A row created
            // bare — no Submit intent and no `MintedSubmit` in this drain window (module note's
            // restart bound) — learns `(venue, symbol, side)` from the first fill that folds. The
            // live engine never needs this; it holds the request from `gate_and_register`.
            if let Some(f) = fill {
                let req = &mut tracked.order.request;
                if req.symbol.is_empty() {
                    req.venue = f.venue.to_string();
                    req.symbol = f.symbol.to_string();
                    req.side = f.side;
                }
            }
            tracked.ts = tracked.ts.max(ts);
        }
        // (`tracked`'s borrow of `self.rows` ends here.)
        if let Err(e) = outcome {
            self.dropped_invalid = self.dropped_invalid.saturating_add(1);
            tracing::debug!(
                coid = %coid,
                refusal = %e,
                dropped_invalid = self.dropped_invalid,
                "lifecycle event refused by the order FSM; not materialized (the live engine dropped it too)"
            );
            return;
        }
        touched.insert(coid.to_string());
    }
}

/// The [`ManagedOrder`] a coid FIRST seen via a bare lifecycle event is adopted into — the
/// insert-only, materializer-side twin of `ExecutionEngine::reregister_orders` (which adopts a
/// venue-reported order at the status the venue reports).
///
/// The request is a shell: `(venue, symbol, side, qty, …)` stay empty/zero until a Submit intent,
/// a `MintedSubmit` or a fill resolves them — the module's symbol-resolution note.
///
/// The seeded STATUS answers one question — "what state must an order already be in for THIS event
/// to be legal?" — which is that event's allowed-from set in `vike_exec`'s transition table, read
/// ONCE, at adoption. It is not a second copy of the table: three seeds cover all eleven events,
/// because `ACCEPTED` is in the allowed-from set of every event except the four pre-acceptance
/// ones. Adopting is what keeps the module's restart bound at STALENESS rather than turning it into
/// a loss — a post-restart tail whose submit/accept sits below the checkpoint still terminalizes —
/// and every event AFTER the adopting one goes through the guarded `apply` like any other.
fn adopt_order(coid: &str, ev: &Event) -> TrackedOrder {
    let mut order =
        ManagedOrder::new(OrderRequest { client_order_id: coid.to_string(), ..Default::default() });
    order.status = match ev {
        // allowed-from {INITIALIZED}
        Event::OrderSubmitted(_) | Event::OrderDenied(_) => OrderStatus::Initialized,
        // allowed-from {SUBMITTED} (`OrderRejected` also accepts INITIALIZED; either seed works)
        Event::OrderAccepted(_) | Event::OrderRejected(_) => OrderStatus::Submitted,
        // Triggered / the two fill wraps / Canceled / Expired / Liquidated / Modified: ACCEPTED is
        // in all of their allowed-from sets (`CAN_RECEIVE_CANCEL` and `MODIFIABLE` included).
        _ => OrderStatus::Accepted,
    };
    TrackedOrder { order, ts: 0 }
}

/// Render the durable `exec_order` snapshot from the FSM aggregate. Every column is READ off
/// [`ManagedOrder`] (status via `OrderStatus::as_str`, the same strings `OrderStatus::parse` reads
/// back on the recon side) — there is no second copy of the order's state that could disagree.
fn exec_order_row(tracked: &TrackedOrder) -> ExecOrderRow {
    let o = &tracked.order;
    let r = &o.request;
    ExecOrderRow {
        ts: tracked.ts,
        client_order_id: r.client_order_id.clone(),
        venue: r.venue.clone(),
        symbol: r.symbol.clone(),
        side: r.side,
        qty: r.qty,
        order_type: r.order_type.clone(),
        status: o.status.as_str().to_string(),
        price: r.price,
        trigger_price: r.trigger_price,
        venue_order_id: o.venue_order_id.clone(),
        filled_qty: o.filled_qty,
        avg_fill_px: o.avg_fill_px,
    }
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
