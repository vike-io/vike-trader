//! `JournalMaterializer` — the off-path tail-follower that materializes the mmap command journal's
//! FILL records into the vike-data execution trade log (Tier-2, `kind=exec_fill`). It is the
//! execution-history sibling of `vike_app_core::data_sink::CoreSinkAdapter` (which bridges live
//! MARKET data onto the store): this one bridges the durable WAL → the queryable parquet trade log,
//! so reporting/recon read a columnar store instead of re-scanning the WAL (unified-journaling #2).
//!
//! It lives in `vike-ops` — which depends on BOTH `vike-core` (the WAL, `vike_core::journal`) and
//! `vike-data` (the `HistStore` trait) — because neither of those may depend on the other, and
//! because the HEADLESS daemon (`vike-tradehub`) spawns it and must not link an egui crate to do
//! so. The store is injected as an `Arc<dyn HistStore>` so this crate needs no DataFusion feature;
//! the binary that spawns the materializer passes the concrete `DataFusionHist`.
//!
//! Contract (from the spec's Tier-2): a strict off-fold consumer of an already-enabled WAL. It
//! never touches the hot path. Durability is at-least-once via a persisted last-materialized seq
//! ([`vike_core::journal::MaterializeCheckpoint`]) + the store's `commit_key` idempotency, so a
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

use indexmap::IndexMap;
use vike_core::journal::{JournalRecord, MaterializeCheckpoint, read_since};
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
        vike_core::journal::CommandJournal::read_all(dir)
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
            // variant" contract on [`vike_core::journal::JournalRecord`]. This crate is a DIFFERENT
            // crate from the one that owns the enum, which is precisely how a variant used to slip
            // past unnoticed (PR #915). Everything below is deliberately ignored:
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

/// The globally-monotonic seq carried by every journal record variant.
fn record_seq(rec: &JournalRecord) -> u64 {
    match rec {
        JournalRecord::Cmd { seq, .. }
        | JournalRecord::Snap { seq, .. }
        | JournalRecord::StrategySubmit { seq, .. }
        | JournalRecord::MintedSubmit { seq, .. }
        // Core-ergonomics: the periodic portfolio OBSERVATION record carries a seq like every
        // other record (so checkpointing stays monotonic), but the materializer's fold above
        // ignores its content — it is not an exec-log event.
        | JournalRecord::PortfolioSnap { seq, .. }
        // Emulator-journal PR-1/PR-2: the ARM's resolved terms, the conditional FIRE, and the
        // DISARM. All carry a seq (checkpointing stays monotonic); the fold above ignores their
        // content — the FIRE's released order still materializes through the `MintedSubmit`/
        // `Fill` records `apply_intent` writes for it, and an armed/disarmed conditional is not
        // an exec-log event (nothing reached a venue), so there is no exec-log row to add here.
        | JournalRecord::ConditionalArmed { seq, .. }
        | JournalRecord::ConditionalFire { seq, .. }
        | JournalRecord::ConditionalDisarmed { seq, .. }
        // Emulator PR-4: the margin-call auto-liquidation's released order carries a seq like every
        // other record; the fold above ignores its content — the released reduce-only MARKET still
        // materializes through the `MintedSubmit`/`Fill` records `apply_intent` writes for it (the
        // same treatment as a `ConditionalFire`), so there is no exec-log row to add here.
        | JournalRecord::MarginCallLiquidate { seq, .. }
        // Emulator PR-5: the managed GTD/Day expiry DECISION carries a seq like every other record;
        // the fold above ignores its content — the cancel it decided reaches the exec log through
        // the venue's own authoritative `OrderCanceled` event record, so there is no exec-log row
        // to add here (adding one would double-count the cancel).
        | JournalRecord::GtdExpire { seq, .. }
        // steal/core-live-scheduler: the wall-clock schedule FIRE DECISION carries a seq like every
        // other record; the fold above ignores its content — the orders the fire's `on_schedule`
        // produced reach the exec log through the `StrategySubmit`/`MintedSubmit`/`Fill` records
        // `drain_broker`/`apply_intent` write for them, so there is no exec-log row to add here.
        | JournalRecord::ScheduleFire { seq, .. } => *seq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::MemHistStore;
    use vike_exec::lanes::Ingest;
    use vike_model::events::{Event, FillEvent, TradeId};

    fn fill(
        trade_id: &'static str,
        coid: &str,
        symbol: &str,
        qty: f64,
        px: f64,
        ts: i64,
    ) -> Ingest {
        Ingest::Event(Event::Fill(FillEvent {
            trade_id: trade_id.into(),
            client_order_id: coid.to_string(),
            venue: "binance".into(),
            symbol: symbol.into(),
            side: 1,
            last_qty: qty,
            last_px: px,
            commission: 0.1,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts,
            mark_price: None,
            position_side: "BOTH".into(),
        }))
    }

    /// Write a WAL with N fills, drain it once, assert Tier-2 holds exactly those fills; a second
    /// drain (no new records) is a no-op; a re-drain of the SAME window never dups.
    #[test]
    fn materializes_wal_fills_into_tier2_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        // write a journal with 3 fills
        {
            let mut j = vike_core::journal::CommandJournal::open(
                dir.path(),
                vike_core::journal::JournalFileConfig::default(),
            )
            .unwrap();
            j.append_cmd(1, &fill("t1", "c1", "BTCUSDT", 0.5, 100.0, 10)).unwrap();
            j.append_cmd(2, &fill("t2", "c1", "BTCUSDT", 0.5, 101.0, 20)).unwrap();
            j.append_cmd(3, &fill("t3", "c2", "ETHUSDT", 1.0, 50.0, 30)).unwrap();
            j.flush().unwrap();
        }
        let store = MemHistStore::new();
        let seq = AtomicU64::new(0);
        let mut orders = OrderTracker::default();
        // cold start (no checkpoint) reads the whole journal, INCLUDING the seq-0 record.
        materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();

        let btc = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
        let eth = store.scan_exec_fills("binance", "ETHUSDT").unwrap();
        assert_eq!(btc.len(), 2, "two BTC fills materialized");
        assert_eq!(eth.len(), 1, "one ETH fill materialized");
        assert_eq!(btc[0].trade_id, "t1", "the seq-0 fill is NOT dropped on cold start");
        assert_eq!(eth[0].symbol, "ETHUSDT");
        assert_eq!(
            btc[0].liquidity_side, "taker",
            "LiquiditySide threaded through as its wire string"
        );
        assert_eq!(btc[0].commission_asset, "", "empty Ustr threads through as an empty string");
        assert!(seq.load(Ordering::Relaxed) >= 2, "checkpoint advanced to the max seq read");

        // second drain: no new records past the checkpoint → no-op, no growth
        materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(), 2, "no re-append");

        // crash-before-checkpoint: lose the checkpoint, re-drain cold over the SAME window → the
        // per-window commit_key makes the re-append a store no-op (at-least-once is safe).
        std::fs::remove_file(dir.path().join("materializer.ckpt")).unwrap();
        materialize_once(dir.path(), &store, &AtomicU64::new(0), &mut OrderTracker::default())
            .unwrap();
        assert_eq!(
            store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(),
            2,
            "re-drain after a lost checkpoint is idempotent (commit_key)"
        );
    }

    use vike_model::events::{OrderModified, OrderSubmitted};

    fn submitted_ev(coid: &str, ts: i64) -> Event {
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts })
    }

    fn accepted_ev(coid: &str, voi: Option<&str>, ts: i64) -> Event {
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: voi.map(Into::into),
            ts,
        })
    }

    fn modified_ev(coid: &str, qty: Option<f64>, px: Option<f64>, ts: i64) -> Event {
        Event::OrderModified(OrderModified {
            client_order_id: coid.to_string(),
            venue_order_id: None,
            new_qty: qty,
            new_price: px,
            ts,
        })
    }

    /// Drive one order from a Submit intent through the adapter's own `OrderSubmitted` to ACCEPTED
    /// — the real WAL prefix for every live order ("Submitted → REST → Accepted|Rejected"). Every
    /// test below that wants a MODIFIABLE order has to walk it, because the FSM's guards are now
    /// the materializer's guards.
    fn seed_accepted(tracker: &mut OrderTracker, req: OrderRequest, touched: &mut HashSet<String>) {
        let coid = req.client_order_id.clone();
        tracker.seed_from_intent(&OrderIntent::Submit(Box::new(req)), 10, touched);
        tracker.fold_event(&submitted_ev(&coid, 11), touched);
        tracker.fold_event(&accepted_ev(&coid, None, 12), touched);
    }

    fn stop_req(coid: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            order_type: "stop".to_string(),
            price: None,
            trigger_price: Some(90.0),
            ..Default::default()
        }
    }

    fn limit_req(coid: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            order_type: "limit".to_string(),
            price: Some(100.0),
            trigger_price: None,
            ..Default::default()
        }
    }

    /// Regression: an `OrderModified` on a STOP order must write its new price to the durable
    /// `trigger_price` column (what the order rests on), not `price`. This used to be a hand-copied
    /// `modified_price_is_trigger` branch in this module; it is now simply what the FSM did.
    #[test]
    fn stop_order_modify_routes_new_price_to_trigger_column() {
        let mut tracker = OrderTracker::default();
        let mut touched = HashSet::new();

        // STOP order resting on trigger 90.0, no limit price.
        seed_accepted(&mut tracker, stop_req("s1"), &mut touched);
        tracker.fold_event(&modified_ev("s1", None, Some(95.0), 20), &mut touched);
        let stop_row = exec_order_row(&tracker.rows["s1"]);
        assert_eq!(stop_row.trigger_price, Some(95.0), "stop modify updates the trigger column");
        assert_eq!(stop_row.price, None, "stop modify must NOT write the limit-price column");

        // Control: a LIMIT order's modify routes to the limit price, leaving trigger untouched.
        seed_accepted(&mut tracker, limit_req("l1"), &mut touched);
        tracker.fold_event(&modified_ev("l1", None, Some(105.0), 20), &mut touched);
        let limit_row = exec_order_row(&tracker.rows["l1"]);
        assert_eq!(limit_row.price, Some(105.0), "limit modify updates the limit-price column");
        assert_eq!(limit_row.trigger_price, None, "limit modify must NOT write the trigger column");
    }

    /// THE DIVERGENCE THIS FOLD EXISTS TO CLOSE.
    ///
    /// The WAL is WRITE-AHEAD, so it carries events the live FSM went on to REFUSE. The reachable
    /// case: a venue amend-ack lands AFTER the fill that terminalized the order (the amend is a
    /// REST round trip — e.g. `BinancePerpRest::modify_order`'s `PUT /fapi/v1/order`, whose `Ok`
    /// arm emits `OrderModified` — while the fill is a one-hop user-WS push; both are pushed onto
    /// the same ingest lane and journaled in arrival order).
    ///
    /// `ManagedOrder::apply` refuses it — `OrderStatus::MODIFIABLE` is {ACCEPTED, TRIGGERED,
    /// PARTIALLY_FILLED} and the order is FILLED — so `ExecutionEngine::on_event` returns and the
    /// live order kept qty 1.0 @ 100.0. The old unguarded copy applied it and appended an
    /// `exec_order` row saying qty 5.0 @ 123.0. Now the materializer gives the FSM's answer.
    #[test]
    fn modify_after_terminal_is_refused_exactly_as_the_live_fsm_refuses_it() {
        let mut tracker = OrderTracker::default();
        let mut touched = HashSet::new();

        seed_accepted(&mut tracker, limit_req("c1"), &mut touched);
        tracker.fold_event(
            &Event::OrderFilled(OrderFilled {
                client_order_id: "c1".to_string(),
                fill: fill_ev("c1", "BTCUSDT", 1.0, 100.0, 13),
                ts: 13,
            }),
            &mut touched,
        );
        let filled = exec_order_row(&tracker.rows["c1"]);
        assert_eq!(filled.status, "FILLED");

        // The late amend-ack: qty 1.0 -> 5.0, price 100.0 -> 123.0. The live FSM dropped it.
        touched.clear();
        tracker.fold_event(&modified_ev("c1", Some(5.0), Some(123.0), 20), &mut touched);

        let after = exec_order_row(&tracker.rows["c1"]);
        assert_eq!(after.qty, 1.0, "a terminal order's qty is NOT rewritten (old copy wrote 5.0)");
        assert_eq!(
            after.price,
            Some(100.0),
            "a terminal order's price is NOT rewritten (old copy wrote 123.0)"
        );
        assert_eq!(after.status, "FILLED", "status untouched");
        assert_eq!(after.ts, filled.ts, "a refused event must not advance the durable row clock");
        assert!(
            touched.is_empty(),
            "a refused event marks nothing touched — no exec_order row is appended for it"
        );
        assert_eq!(tracker.dropped_invalid, 1, "the refusal is counted, not silently absorbed");
    }

    /// The same guard, on the other axis: a fill wrap arriving on an already-CANCELED order (the
    /// cancel-vs-fill race the engine warns about as `stranded_terminal_drops`). The FSM refuses it
    /// — `OrderFilled` is legal only from {ACCEPTED, TRIGGERED, PARTIALLY_FILLED} — so the durable
    /// row must NOT flip to FILLED nor accumulate the qty. The bare `Event::Fill` for the same
    /// execution still materializes into `exec_fill` on its own lane; only the ORDER snapshot is
    /// held to what the engine actually folded.
    #[test]
    fn fill_wrap_after_terminal_neither_flips_status_nor_accumulates_qty() {
        let mut tracker = OrderTracker::default();
        let mut touched = HashSet::new();

        seed_accepted(&mut tracker, limit_req("c2"), &mut touched);
        tracker.fold_event(
            &Event::OrderCanceled(OrderCanceled {
                client_order_id: "c2".to_string(),
                reason: String::new().into(),
                ts: 20,
            }),
            &mut touched,
        );
        touched.clear();
        tracker.fold_event(
            &Event::OrderFilled(OrderFilled {
                client_order_id: "c2".to_string(),
                fill: fill_ev("c2", "BTCUSDT", 1.0, 100.0, 21),
                ts: 21,
            }),
            &mut touched,
        );

        let row = exec_order_row(&tracker.rows["c2"]);
        assert_eq!(row.status, "CANCELED", "the canceled order does NOT become FILLED");
        assert_eq!(row.filled_qty, 0.0, "no qty accumulated onto a terminal order");
        assert!(touched.is_empty(), "nothing touched, so no fabricated snapshot row");
        assert_eq!(tracker.dropped_invalid, 1);
    }

    /// A coid first seen via a bare lifecycle event — the post-restart tail whose submit/accept sits
    /// below the checkpoint — is ADOPTED into the state that event's allowed-from set requires, so
    /// the tail still terminalizes instead of stalling at INITIALIZED. The adoption is one-shot:
    /// the NEXT event is guarded like any other.
    #[test]
    fn bare_first_sighting_is_adopted_then_guarded_like_any_other_order() {
        let mut tracker = OrderTracker::default();
        let mut touched = HashSet::new();

        // No submit, no MintedSubmit: the first record for this coid is a fill wrap.
        tracker.fold_event(
            &Event::OrderFilled(OrderFilled {
                client_order_id: "orphan".to_string(),
                fill: fill_ev("orphan", "ETHUSDT", 2.0, 50.0, 30),
                ts: 30,
            }),
            &mut touched,
        );
        let row = exec_order_row(&tracker.rows["orphan"]);
        assert_eq!(row.status, "FILLED", "adopted at ACCEPTED, so the fill wrap applies");
        assert_eq!(row.symbol, "ETHUSDT", "partition key learned from the fill");
        assert_eq!(row.filled_qty, 2.0);
        assert_eq!(tracker.dropped_invalid, 0, "the adopting event is never a refusal");

        // …and the adoption does not disable the guards: a modify on the now-terminal order is
        // refused exactly as it is for a fully-seeded order.
        touched.clear();
        tracker.fold_event(&modified_ev("orphan", Some(9.0), None, 31), &mut touched);
        assert_eq!(exec_order_row(&tracker.rows["orphan"]).qty, 0.0, "terms not rewritten");
        assert_eq!(tracker.dropped_invalid, 1);
    }

    use vike_model::events::{OrderAccepted, OrderCanceled, OrderFilled, OrderPartiallyFilled};

    fn submit(coid: &str, venue: &str, symbol: &str, side: i32, qty: f64) -> Ingest {
        Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: coid.to_string(),
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            side,
            qty,
            order_type: "limit".to_string(),
            price: Some(100.0),
            ..Default::default()
        }))))
    }

    fn fill_ev(coid: &str, symbol: &str, qty: f64, px: f64, ts: i64) -> FillEvent {
        FillEvent {
            // minted by this helper — same `tr-<ts>` bytes as the `format!` it replaced
            trade_id: TradeId::prefixed("tr-", ts),
            client_order_id: coid.to_string(),
            venue: "binance".into(),
            symbol: symbol.into(),
            side: 1,
            last_qty: qty,
            last_px: px,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts,
            mark_price: None,
            position_side: "BOTH".into(),
        }
    }

    fn accepted(coid: &str, voi: &str, ts: i64) -> Ingest {
        Ingest::Event(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(voi.into()),
            ts,
        }))
    }

    /// The adapter's own `OrderSubmitted`, emitted BEFORE the venue round trip and journaled on the
    /// ingest lane like every other venue event ("Submitted → REST → Accepted|Rejected" — the
    /// emitter-split contract `bridge_conformance.rs` machine-checks for every covered venue).
    ///
    /// The WAL-writing tests below carry it because the REAL WAL carries it, and folding through
    /// the FSM means it is now load-bearing: `OrderAccepted` is legal only from SUBMITTED. A venue
    /// that skipped it would have its accept dropped by the LIVE engine too — which is exactly the
    /// agreement this fold buys.
    fn submitted(coid: &str, ts: i64) -> Ingest {
        Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: coid.to_string(),
            ts,
        }))
    }

    fn order_filled(coid: &str, f: FillEvent, ts: i64) -> Ingest {
        Ingest::Event(Event::OrderFilled(OrderFilled {
            client_order_id: coid.to_string(),
            fill: f,
            ts,
        }))
    }

    fn order_partial(coid: &str, f: FillEvent, ts: i64) -> Ingest {
        Ingest::Event(Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid.to_string(),
            fill: f,
            ts,
        }))
    }

    fn canceled(coid: &str, ts: i64) -> Ingest {
        Ingest::Event(Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.to_string(),
            reason: String::new().into(),
            ts,
        }))
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
            let mut j = vike_core::journal::CommandJournal::open(
                dir.path(),
                vike_core::journal::JournalFileConfig::default(),
            )
            .unwrap();
            // explicit-coid order c1: submit(limit BTCUSDT 1.0) → OrderSubmitted → accept →
            // partial 0.3@100 → fill 0.7@110
            j.append_cmd(1, &submit("c1", "binance", "BTCUSDT", 1, 1.0)).unwrap();
            j.append_cmd(2, &submitted("c1", 10)).unwrap();
            j.append_cmd(2, &accepted("c1", "v1", 11)).unwrap();
            j.append_cmd(3, &order_partial("c1", fill_ev("c1", "BTCUSDT", 0.3, 100.0, 12), 12))
                .unwrap();
            j.append_cmd(4, &order_filled("c1", fill_ev("c1", "BTCUSDT", 0.7, 110.0, 13), 13))
                .unwrap();
            // server-minted order cm: only an OrderFilled arrives (symbol resolved from the fill)
            j.append_cmd(5, &order_filled("cm", fill_ev("cm", "ETHUSDT", 2.0, 50.0, 20), 20))
                .unwrap();
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
                    order_type: "limit".to_string(),
                    price: Some(100.0),
                    ..Default::default()
                },
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
        let mut orders = OrderTracker::default();
        materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();

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
        assert!(orders.skipped_unresolved >= 1, "the symbol-less canceled coid was skipped");

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
        materialize_once(dir.path(), &store, &AtomicU64::new(0), &mut OrderTracker::default())
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
            let mut j = vike_core::journal::CommandJournal::open(
                dir.path(),
                vike_core::journal::JournalFileConfig::default(),
            )
            .unwrap();
            // the three inline-minted legs, journaled exactly as apply_intent's Bracket arm now does
            j.append_minted_submit(1, &entry).unwrap();
            j.append_minted_submit(1, &sl).unwrap();
            j.append_minted_submit(1, &tp).unwrap();
            // SL is released to the venue (adapter's OrderSubmitted), accepted, then canceled (its
            // OCO sibling TP filled instead) — no fill event for SL ever arrives.
            j.append_cmd(2, &submitted("sl1", 10)).unwrap();
            j.append_cmd(2, &accepted("sl1", "vsl1", 11)).unwrap();
            j.append_cmd(3, &canceled("sl1", 12)).unwrap();
            j.flush().unwrap();
        }
        let store = MemHistStore::new();
        let seq = AtomicU64::new(0);
        let mut orders = OrderTracker::default();
        materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();

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

    use vike_model::events::{
        OrderDenied, OrderExpired, OrderLiquidated, OrderRejected, OrderTriggered,
    };

    /// The skip guard is `symbol.is_empty() || venue.is_empty()`, and its `||` was pinned by
    /// nothing: the existing `cx` case in `materializes_order_lifecycle_snapshots` leaves BOTH
    /// fields empty, which `&&` skips just as happily.
    ///
    /// The one-empty shape is not contrived — production MAKES it. `vike_model::order`'s
    /// `build_combo` sets `venue` from the spec and leaves `symbol` for the adapter to resolve, so
    /// a combo leg that terminalizes before its symbol is known arrives here with venue set and
    /// symbol empty. Under `&&` that row is not skipped: it is written into a partition keyed
    /// `symbol=`, which `scan_exec_orders` can never address again. A durable row nothing can read
    /// is worse than no row — recon asks this store what it knows.
    #[test]
    fn a_venue_without_a_symbol_is_skipped_not_written_to_an_unaddressable_partition() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut j = vike_core::journal::CommandJournal::open(
                dir.path(),
                vike_core::journal::JournalFileConfig::default(),
            )
            .unwrap();
            // The `build_combo` shape: venue known, symbol left to the adapter, then terminal.
            j.append_cmd(1, &submit("cb1", "deribit", "", 1, 1.0)).unwrap();
            j.append_cmd(2, &submitted("cb1", 10)).unwrap();
            j.append_cmd(3, &accepted("cb1", "vcb1", 11)).unwrap();
            j.append_cmd(4, &canceled("cb1", 12)).unwrap();
            j.flush().unwrap();
        }
        let store = MemHistStore::new();
        let seq = AtomicU64::new(0);
        let mut orders = OrderTracker::default();
        materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();

        assert!(
            store.scan_exec_orders("deribit", "").unwrap().is_empty(),
            "a symbol-less row must not be written; that partition is unaddressable"
        );
        assert!(
            orders.skipped_unresolved >= 1,
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
            let mut j = vike_core::journal::CommandJournal::open(
                dir.path(),
                vike_core::journal::JournalFileConfig::default(),
            )
            .unwrap();
            // One unattributable fill, and one perfectly good one in the same window.
            j.append_cmd(1, &fill("t-bad", "cx", "", 1.0, 100.0, 10)).unwrap();
            j.append_cmd(2, &fill("t-good", "c1", "BTCUSDT", 2.0, 200.0, 11)).unwrap();
            j.flush().unwrap();
        }
        let store = MemHistStore::new();
        let seq = AtomicU64::new(0);
        let mut orders = OrderTracker::default();

        // The pass must SUCCEED. Before the skip existed this was `Err` and nothing was persisted.
        materialize_once(dir.path(), &store, &seq, &mut orders)
            .expect("one unattributable fill must not fail the pass");

        let good = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
        assert_eq!(good.len(), 1, "the attributable fill still lands: {good:?}");
        assert_eq!(good[0].trade_id, "t-good");
        assert!(
            store.scan_exec_fills("binance", "").unwrap().is_empty(),
            "and the unattributable one is not written to an unaddressable partition"
        );
        assert!(orders.skipped_unresolved >= 1, "the skip is counted, not silent");
    }

    /// `fold_event`'s match ends in `_ => return`, so DELETING one of its arms compiles clean and
    /// silently stops folding that event — the durable row simply keeps whatever status it had.
    /// A mutation sweep found five arms no test pinned; this pins the two that matter, and the
    /// other three ride along because a table costs nothing to widen.
    ///
    /// Why these two are worth a test and the rest are not:
    ///
    /// - `OrderRejected` is half of every submit's outcome ("Submitted → REST → Accepted|Rejected",
    ///   the WAL prefix `seed_accepted` walks). Unfolded, the durable row rests at SUBMITTED
    ///   forever for an order the venue refused outright.
    /// - `OrderExpired` leaves the row at ACCEPTED permanently, and that one is not merely stale:
    ///   `recon/diff.rs` asks the journal whether an order is live, so an expired order that still
    ///   reads ACCEPTED manufactures a `JournalDivergence` — the one divergence kind held for
    ///   operator confirm before the policy is even consulted. A silent fold gap becomes a
    ///   quarantine an operator has to clear by hand.
    ///
    /// `OrderTriggered`/`OrderLiquidated`/`OrderDenied` are asserted here only because they share
    /// the loop. Do not read this test as a claim that their inputs are reachable in this seam.
    #[test]
    fn every_lifecycle_event_folds_into_the_durable_status() {
        // Each arm gets its OWN baseline, because the FSM's entry states differ per event and a
        // single shared prefix would silently test nothing: `transition_for` admits
        // `OrderRejected` only from {Initialized, Submitted} and `OrderDenied` only from
        // {Initialized}, so seeding everything to ACCEPTED would have `apply` REFUSE those two,
        // leave the row at ACCEPTED, and pass just as happily with the fold arm deleted.
        //
        // (coid, seed depth, event, expected durable status).
        let cases: &[FoldCase] = &[
            (
                "rejected",
                Seed::Submitted,
                |c, ts| {
                    Event::OrderRejected(OrderRejected {
                        client_order_id: c.to_string(),
                        reason: "insufficient margin".into(),
                        ts,
                    })
                },
                "REJECTED",
            ),
            (
                "expired",
                Seed::Accepted,
                |c, ts| Event::OrderExpired(OrderExpired { client_order_id: c.to_string(), ts }),
                "EXPIRED",
            ),
            (
                "triggered",
                Seed::AcceptedStop,
                |c, ts| {
                    Event::OrderTriggered(OrderTriggered { client_order_id: c.to_string(), ts })
                },
                "TRIGGERED",
            ),
            (
                "liquidated",
                Seed::Accepted,
                |c, ts| {
                    Event::OrderLiquidated(OrderLiquidated {
                        client_order_id: c.to_string(),
                        liq_price: 88.0,
                        ts,
                    })
                },
                "LIQUIDATED",
            ),
            (
                "denied",
                Seed::Initialized,
                |c, ts| {
                    Event::OrderDenied(OrderDenied {
                        client_order_id: c.to_string(),
                        reason: "risk gate".into(),
                        ts,
                    })
                },
                "DENIED",
            ),
        ];

        for (coid, seed, make, expected) in cases {
            let mut tracker = OrderTracker::default();
            let mut touched = HashSet::new();

            let req =
                if matches!(seed, Seed::AcceptedStop) { stop_req(coid) } else { limit_req(coid) };
            tracker.seed_from_intent(&OrderIntent::Submit(Box::new(req)), 10, &mut touched);
            if !matches!(seed, Seed::Initialized) {
                tracker.fold_event(&submitted_ev(coid, 11), &mut touched);
            }
            if matches!(seed, Seed::Accepted | Seed::AcceptedStop) {
                tracker.fold_event(&accepted_ev(coid, None, 12), &mut touched);
            }
            let baseline = exec_order_row(&tracker.rows[*coid]).status;
            assert_eq!(
                baseline,
                seed.status(),
                "{coid}: the baseline itself must be the state this event is admitted FROM — \
                 otherwise `apply` refuses and the assertion below proves nothing"
            );

            touched.clear();
            tracker.fold_event(&make(coid, 30), &mut touched);

            assert_eq!(
                exec_order_row(&tracker.rows[*coid]).status,
                *expected,
                "{coid}: the durable row must carry the FSM's status after the fold — a deleted \
                 `fold_event` arm leaves it at {baseline} and says nothing"
            );
            assert!(
                touched.contains(*coid),
                "{coid}: a folded event must mark the row dirty, or it never reaches the store"
            );
        }
    }

    /// One row of the fold table: the order's coid, how far its baseline is driven, the event to
    /// fold, and the durable status that must come out.
    type FoldCase = (&'static str, Seed, fn(&str, i64) -> Event, &'static str);

    /// How far down the WAL prefix a case's baseline order is driven before the event under test.
    #[derive(Clone, Copy)]
    enum Seed {
        Initialized,
        Submitted,
        Accepted,
        /// ACCEPTED, but seeded from a STOP request — the only shape `OrderTriggered` applies to.
        AcceptedStop,
    }

    impl Seed {
        fn status(self) -> &'static str {
            match self {
                Seed::Initialized => "INITIALIZED",
                Seed::Submitted => "SUBMITTED",
                Seed::Accepted | Seed::AcceptedStop => "ACCEPTED",
            }
        }
    }
}
