//! A LEASE: one connection's hold on one account's session.
//!
//! The wire adapter (`crate::server::trade`) owns the socket and two threads; this type is everything
//! else. The reader thread feeds each client frame to [`TradeLeaseHold::handle`]; the writer thread pulls each
//! frame it must write from [`TradeLeaseHold::next`]. The lease holds the connection's outbound CONTROL queue
//! (replies, acks, pongs), its position in the account's event ring, the fence and the end state, and
//! nothing that touches a socket: the one way it can reach one is the `closer` closure the adapter hands
//! in, which the lease calls only to force a stuck connection shut.
//!
//! # Events are PULLED, not queued
//!
//! A lease keeps no event queue. [`TradeLeaseHold::next`] reads the account's ring by `seq` (`next_sequence`), so a
//! slow client costs the ring's bound and nothing else, a replay after a resume is the same code as the
//! live path, and a client whose next `seq` has fallen below the ring's floor is told so
//! (`ReplayUnavailable{floor}`) and skips to the floor: the ring never drops silently.
//!
//! # The fence
//!
//! A reconnect is FENCED. [`TradeLeaseHold::fence`] tells the old lease's reader to read its connection to the
//! end (everything the client had already sent is processed, and the watermark `received_through_command_sequence`
//! counts it) and then stop; `TradeOpen` waits for [`TradeLeaseHold::wait_finished`] before it answers the new
//! open. The reader notices the fence between its short waits, and "to the end" means until the
//! connection has been quiet for one slice: a client that is still writing keeps it reading, and
//! `TradeConfig::fence_drain` bounds the wait.
//!
//! # Every client-named value is bounded
//!
//! [`TradeLeaseHold::handle`] checks each frame before it touches any state: string lengths, element counts, the
//! shape of a batch. A frame over a bound is a protocol violation: `Bye(Protocol(..))`, the connection
//! ends. Nothing here indexes, unwraps or `expect`s on wire data.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{
    AdapterReply, AdapterStart, DetachPhase, GATEWAY_MAX_SYMBOL_BYTES, TradeBye, TradeDown,
    TradeStatusReport, TradeUp,
};
use vike_gatewayhub_client::trade::restored_orders;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderModifyRejected, OrderRejected};

use super::dispatch::ExecutionClientDispatchCommand;
use super::recon::ReconcileWorkerJob;
use super::session::{AccountSession, EventRingPull};

/// The longest order client id accepted.
pub const MAX_ORDER_CLIENT_ID_BYTES: usize = 256;

/// The most orders, ids or pairs one frame may carry. The frame ceiling bounds the bytes; this bounds
/// the work.
pub const MAX_FRAME_ITEMS: usize = 8192;

/// The most declared legs one adapter start may name.
pub const MAX_DECLARED_LEGS: usize = 64;

/// The most linked order ids one order may carry.
pub const MAX_LINKED_ORDER_IDS: usize = 64;

/// The most combo legs one order may carry.
pub const MAX_COMBO_LEGS: usize = 64;

/// The longest route key accepted: a venue id, `#` and a label.
pub const MAX_ROUTE_KEY_BYTES: usize = 64;

/// The longest venue or account text an order may carry.
pub const MAX_NAME_BYTES: usize = 64;

/// The reason of the reject an order gets when no adapter is running for its account.
pub const NO_ADAPTER_REASON: &str = "gatewayhub: no venue adapter is running for this account";

/// The reason of the reject an order gets when the adapter's dispatch thread is gone.
pub const ADAPTER_GONE_REASON: &str =
    "gatewayhub: the venue adapter's dispatch thread is unavailable";

/// The reason on the event of a command the adapter was handling when the bridge panicked. A submit may
/// have reached the venue before the failure: the terminal says the daemon cannot vouch for it.
pub const ADAPTER_FAILED_REASON: &str = "gatewayhub: the venue adapter failed while handling this command (an order it was submitting may have reached the venue; reconcile finds it)";

/// What the reader does after a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeLeaseHoldFlow {
    /// Read the next frame.
    Continue,
    /// The lease is over (a final detach, a protocol violation, a displaced holder): stop reading.
    End,
}

/// What the writer does next.
// large_enum_variant: `Frame` IS the dominant variant (every event, ack and reply the writer sends), and the
// facts reply grew by the conversion fields (decision 0183). Boxing the frame would cost an allocation per
// frame on the writer's path to spare the three tiny variants beside it a few hundred bytes.
#[expect(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum TradeLeaseHoldNext {
    /// Write this frame.
    Frame(TradeDown),
    /// Write this last frame, then close.
    Bye(TradeBye),
    /// Nothing for the heartbeat period: write a `Heartbeat`.
    Idle,
    /// The lease is closed and everything queued was written: end the writer.
    Closed,
}

struct Control {
    control: VecDeque<TradeDown>,
    bye: Option<TradeBye>,
    /// Set by anything that may have made `next` productive; consumed before every ring look.
    hint: bool,
    next_sequence: u64,
    live_from: u64,
    cap: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One connection's hold on one account's session. See the module doc.
pub struct TradeLeaseHold {
    /// This lease's generation (`TradeOpened.lease_epoch`).
    pub id: u64,
    /// The caller's id for the lease, stable across its reconnects.
    pub client_id: String,
    /// The non-secret fingerprint of the key that authenticated.
    pub key_id: String,
    /// When the lease was granted, epoch ms.
    pub since_ms: i64,
    session: Arc<AccountSession>,
    control: Mutex<Control>,
    wake: Condvar,
    fenced: AtomicBool,
    ended: AtomicBool,
    final_detach: AtomicBool,
    finished: Mutex<bool>,
    finished_cv: Condvar,
    closer: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for TradeLeaseHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TradeLeaseHold")
            .field("id", &self.id)
            .field("client_id", &self.client_id)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// How a lease is made (the session does it at `TradeOpen`).
pub struct TradeLeaseHoldSeed {
    /// The generation.
    pub id: u64,
    /// The caller's id.
    pub client_id: String,
    /// The key's fingerprint.
    pub key_id: String,
    /// Now, epoch ms.
    pub since_ms: i64,
    /// The first ring `seq` this lease delivers.
    pub next_sequence: u64,
    /// Ring entries below this `seq` are replays.
    pub live_from: u64,
    /// The control queue bound.
    pub control_cap: usize,
    /// Forces the connection shut.
    pub closer: Arc<dyn Fn() + Send + Sync>,
}

impl TradeLeaseHold {
    pub(crate) fn new(session: Arc<AccountSession>, seed: TradeLeaseHoldSeed) -> Arc<Self> {
        Arc::new(Self {
            id: seed.id,
            client_id: seed.client_id,
            key_id: seed.key_id,
            since_ms: seed.since_ms,
            session,
            control: Mutex::new(Control {
                control: VecDeque::new(),
                bye: None,
                hint: true,
                next_sequence: seed.next_sequence,
                live_from: seed.live_from,
                cap: seed.control_cap.max(1),
            }),
            wake: Condvar::new(),
            fenced: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            final_detach: AtomicBool::new(false),
            finished: Mutex::new(false),
            finished_cv: Condvar::new(),
            closer: seed.closer,
        })
    }

    // ---- the outbound side ------------------------------------------------------------------------

    /// Queue a control frame for the writer. `false` = the lease is over, or its queue was full (which
    /// ends it: `Bye(TooSlow)`).
    pub fn send(&self, frame: TradeDown) -> bool {
        if self.ended.load(Ordering::SeqCst) {
            return false;
        }
        let mut c = lock(&self.control);
        if c.control.len() >= c.cap {
            if c.bye.is_none() {
                c.bye = Some(TradeBye::TooSlow);
            }
            c.hint = true;
            drop(c);
            self.wake.notify_all();
            return false;
        }
        c.control.push_back(frame);
        c.hint = true;
        drop(c);
        self.wake.notify_all();
        true
    }

    /// The next ring `seq` this lease's writer will hand out: every event below it has been taken by the
    /// writer already, so a `Bye` raised once this passes the ring's last `seq` is written after them.
    pub(crate) fn next_sequence(&self) -> u64 {
        lock(&self.control).next_sequence
    }

    /// Say a ring append may have made [`Self::next`] productive.
    pub(crate) fn wake_events(&self) {
        lock(&self.control).hint = true;
        self.wake.notify_all();
    }

    /// End the lease with `bye` as its last frame. The first one wins.
    pub fn bye(&self, bye: TradeBye) {
        let mut c = lock(&self.control);
        if c.bye.is_none() {
            c.bye = Some(bye);
        }
        c.hint = true;
        drop(c);
        self.wake.notify_all();
    }

    /// The connection is going away (its reader left): the writer writes what is queued, then ends. Idempotent.
    pub fn close(&self) {
        self.ended.store(true, Ordering::SeqCst);
        lock(&self.control).hint = true;
        self.wake.notify_all();
    }

    /// The daemon is stopping: `Bye(ServerStopping)`.
    pub fn stop(&self) {
        self.bye(TradeBye::ServerStopping);
    }

    /// The next frame to write, waiting up to `idle` for one: control frames first, then the closing `Bye`,
    /// then the ring in `seq` order, then [`TradeLeaseHoldNext::Idle`]. ⚠ A `Bye` overtakes what the ring still holds: a caller
    /// that owes the client those events drains first (`session/link.rs`'s `drain_ring_to`), and a client treats a
    /// submit that was acknowledged and never answered as UNKNOWN when its lease ends (`Shared::with_unanswered`).
    pub fn next(&self, idle: Duration) -> TradeLeaseHoldNext {
        let deadline = Instant::now() + idle;
        loop {
            let next_sequence = {
                let mut c = lock(&self.control);
                if let Some(frame) = c.control.pop_front() {
                    return TradeLeaseHoldNext::Frame(frame);
                }
                if let Some(bye) = c.bye.take() {
                    self.ended.store(true, Ordering::SeqCst);
                    return TradeLeaseHoldNext::Bye(bye);
                }
                if self.ended.load(Ordering::SeqCst) {
                    return TradeLeaseHoldNext::Closed;
                }
                c.hint = false;
                c.next_sequence
            };
            match self.session.pull(next_sequence) {
                EventRingPull::Event { entry, account, gateway_epoch } => {
                    let mut c = lock(&self.control);
                    c.next_sequence = entry.sequence + 1;
                    let replayed = entry.sequence < c.live_from;
                    return TradeLeaseHoldNext::Frame(TradeDown::Event {
                        account: account.to_string(),
                        gateway_epoch,
                        sequence: entry.sequence,
                        timestamp_ms: entry.timestamp_ms,
                        replayed,
                        event: entry.event,
                    });
                }
                EventRingPull::Gap { floor } => {
                    // The ring evicted what this lease had not read yet: say so, never skip silently.
                    lock(&self.control).next_sequence = floor;
                    tracing::warn!(
                        client = %self.client_id,
                        floor,
                        "TRADE-LINK: a slow lease fell behind the event ring; the client is told to reconcile"
                    );
                    return TradeLeaseHoldNext::Frame(TradeDown::ReplayUnavailable { floor });
                }
                EventRingPull::Nothing => {}
            }
            let c = lock(&self.control);
            if c.hint || !c.control.is_empty() || c.bye.is_some() {
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return TradeLeaseHoldNext::Idle;
            }
            let (c, _) =
                self.wake.wait_timeout(c, deadline - now).unwrap_or_else(PoisonError::into_inner);
            drop(c);
        }
    }

    // ---- the lifetime ------------------------------------------------------------------------------

    /// Tell the reader to read its connection to the end and stop (a reconnect is being answered).
    pub fn fence(&self) {
        self.fenced.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    /// Whether a fence was raised.
    pub fn is_fenced(&self) -> bool {
        self.fenced.load(Ordering::SeqCst)
    }

    /// Whether the connection is going away (a `Bye` was taken, or [`Self::finish`] ran).
    pub fn is_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// Force the connection shut (the adapter's `closer`).
    pub fn force_close(&self) {
        (self.closer)();
    }

    /// Wait up to `within` for [`Self::finish`]; whether it ran.
    pub fn wait_finished(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut done = lock(&self.finished);
        while !*done {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, _) = self
                .finished_cv
                .wait_timeout(done, deadline - now)
                .unwrap_or_else(PoisonError::into_inner);
            done = guard;
        }
        true
    }

    /// The connection is over (the reader left and the writer was joined): release the session's hold,
    /// let a fenced open proceed and, after a final detach, retire the session.
    pub fn finish(self: &Arc<Self>) {
        self.ended.store(true, Ordering::SeqCst);
        self.session.lease_ended(self);
        *lock(&self.finished) = true;
        self.finished_cv.notify_all();
        if self.final_detach.load(Ordering::SeqCst) {
            self.session.retire_if_idle("the client detached");
        }
    }

    // ---- the inbound side --------------------------------------------------------------------------

    fn live(&self) -> bool {
        !self.ended.load(Ordering::SeqCst) && !self.session.hub_stopping()
    }

    fn protocol(&self, words: String) -> TradeLeaseHoldFlow {
        tracing::warn!(client = %self.client_id, "TRADE-LINK: protocol violation: {words}");
        self.bye(TradeBye::Protocol(words));
        TradeLeaseHoldFlow::End
    }

    /// Queue `cmd` for the adapter, or, when it cannot reach one, put the event the contract owes in the
    /// ring: a submit is a terminal reject, a cancel or a modify a non-terminal advisory.
    fn dispatch(&self, cmd: ExecutionClientDispatchCommand) {
        if let Err(cmd) = self.session.enqueue(cmd, || self.live()) {
            self.session.synthesize(cmd);
        }
    }

    /// The terminal reject of a submit this lease refuses (`AccountSession::submit_refusal`: another venue or account,
    /// or a symbol the control door writes another way): it never reaches the adapter, and the lease goes on.
    fn refuse_submit(&self, order: &OrderRequest, reason: &str) {
        tracing::warn!(
            client = %self.client_id,
            %reason,
            "TRADE-LINK: an order was refused before its venue"
        );
        self.session.emit_event(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order.order_client_id.clone(),
            reason: reason.into(),
            timestamp: order.timestamp,
        }));
    }

    /// Handle one decoded client frame. See the module doc for the bounds.
    pub fn handle(self: &Arc<Self>, frame: TradeUp) -> TradeLeaseHoldFlow {
        if !self.session.lease_is_current(self.id) {
            // Displaced (a takeover whose fence timed out): a late frame from an old connection.
            return TradeLeaseHoldFlow::End;
        }
        match frame {
            TradeUp::Ping => {
                self.send(TradeDown::Pong);
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::Detach { phase: DetachPhase::Begin } => TradeLeaseHoldFlow::Continue,
            TradeUp::Detach { phase: DetachPhase::Final } => {
                self.final_detach.store(true, Ordering::SeqCst);
                self.bye(TradeBye::ClientClosed);
                TradeLeaseHoldFlow::End
            }
            TradeUp::AdapterStart(start) => {
                if let Err(why) = check_start(&start) {
                    return self.protocol(why);
                }
                let rid = start.rid;
                let reply: AdapterReply = self.session.start_adapter(start);
                self.send(TradeDown::AdapterReply { rid, reply: Box::new(reply) });
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::Submit { command_sequence: cseq, order } => {
                if let Err(why) = check_order(&order) {
                    return self.protocol(why);
                }
                self.session.note_command_sequence(&self.client_id, cseq);
                match self.session.submit_refusal(&order) {
                    Some(reason) => self.refuse_submit(&order, &reason),
                    None => self.dispatch(ExecutionClientDispatchCommand::Submit(Box::new(order))),
                }
                self.send(TradeDown::CommandAck { command_sequence: cseq });
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::SubmitBatch { command_sequence: cseq, orders } => {
                if orders.len() > MAX_FRAME_ITEMS {
                    return self.protocol(format!("a batch of {} orders", orders.len()));
                }
                if let Some(why) = orders.iter().find_map(|o| check_order(o).err()) {
                    return self.protocol(why);
                }
                self.session.note_command_sequence(&self.client_id, cseq);
                let mut kept = Vec::with_capacity(orders.len());
                for order in orders {
                    match self.session.submit_refusal(&order) {
                        Some(reason) => self.refuse_submit(&order, &reason),
                        None => kept.push(order),
                    }
                }
                if !kept.is_empty() {
                    self.dispatch(ExecutionClientDispatchCommand::SubmitBatch(kept));
                }
                self.send(TradeDown::CommandAck { command_sequence: cseq });
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::Cancel { command_sequence: cseq, order_client_id, intent } => {
                if let Err(why) = check_order_client_id(&order_client_id) {
                    return self.protocol(why);
                }
                self.session.note_command_sequence(&self.client_id, cseq);
                self.dispatch(ExecutionClientDispatchCommand::Cancel { order_client_id, intent });
                self.send(TradeDown::CommandAck { command_sequence: cseq });
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::CancelBatch { command_sequence: cseq, order_client_ids, intent } => {
                if let Err(why) = check_order_client_ids(&order_client_ids) {
                    return self.protocol(why);
                }
                self.session.note_command_sequence(&self.client_id, cseq);
                self.dispatch(ExecutionClientDispatchCommand::CancelBatch {
                    order_client_ids,
                    intent,
                });
                self.send(TradeDown::CommandAck { command_sequence: cseq });
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::Modify {
                command_sequence: cseq,
                order,
                new_qty,
                new_price,
                new_trigger_price,
            } => {
                if let Err(why) = check_order(&order) {
                    return self.protocol(why);
                }
                self.session.note_command_sequence(&self.client_id, cseq);
                match self.session.identity_conflict(&order) {
                    Some(reason) => {
                        self.session.emit_event(Event::OrderModifyRejected(OrderModifyRejected {
                            order_client_id: order.order_client_id.clone(),
                            reason: reason.into(),
                            // stamped when made: `vike_bridge_core::rest::amend_answer_time`'s rule
                            timestamp: vike_model::now_ms(),
                        }));
                    }
                    None => {
                        self.dispatch(ExecutionClientDispatchCommand::Modify {
                            order: Box::new(order),
                            new_qty,
                            new_price,
                            new_trigger_price,
                        });
                    }
                }
                self.send(TradeDown::CommandAck { command_sequence: cseq });
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::Confirm { command_sequence: cseq, order_client_id } => {
                if let Err(why) = check_order_client_id(&order_client_id) {
                    return self.protocol(why);
                }
                self.session.note_command_sequence(&self.client_id, cseq);
                self.dispatch(ExecutionClientDispatchCommand::Confirm(order_client_id));
                self.send(TradeDown::CommandAck { command_sequence: cseq });
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::AdoptRestored { command_sequence: cseq, pairs, venue_ids } => {
                if pairs.len() > MAX_FRAME_ITEMS || venue_ids.len() > MAX_FRAME_ITEMS {
                    return self.protocol(format!(
                        "{} restored pairs and {} venue ids in one frame",
                        pairs.len(),
                        venue_ids.len()
                    ));
                }
                if let Some(why) = pairs
                    .iter()
                    .find_map(|(order_client_id, symbol)| {
                        check_order_client_id(order_client_id)
                            .err()
                            .or_else(|| check_symbol(symbol).err())
                    })
                    .or_else(|| {
                        venue_ids.iter().find_map(|(order_client_id, id)| {
                            check_order_client_id(order_client_id)
                                .err()
                                .or_else(|| check_venue_order_id(id).err())
                        })
                    })
                {
                    return self.protocol(why);
                }
                self.session.note_command_sequence(&self.client_id, cseq);
                self.dispatch(ExecutionClientDispatchCommand::Adopt(restored_orders(
                    pairs, venue_ids,
                )));
                self.send(TradeDown::CommandAck { command_sequence: cseq });
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::WireIds { rid, order_client_ids } => {
                if let Err(why) = check_order_client_ids(&order_client_ids) {
                    return self.protocol(why);
                }
                let cmd = ExecutionClientDispatchCommand::WireIds {
                    rid,
                    order_client_ids,
                    to: Arc::clone(self),
                };
                if let Err(ExecutionClientDispatchCommand::WireIds { rid, .. }) =
                    self.session.enqueue(cmd, || self.live())
                {
                    // No adapter: no order client id has a wire id that differs. An answer, not a hang.
                    self.send(TradeDown::WireIdsReply { rid, pairs: Vec::new() });
                }
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::ResolveOrders { rid, order_client_ids } => {
                if let Err(why) = check_order_client_ids(&order_client_ids) {
                    return self.protocol(why);
                }
                let cmd = ExecutionClientDispatchCommand::Resolve {
                    rid,
                    order_client_ids,
                    to: Arc::clone(self),
                };
                if let Err(ExecutionClientDispatchCommand::Resolve {
                    rid, order_client_ids, ..
                }) = self.session.enqueue(cmd, || self.live())
                {
                    // No adapter: nothing is tracked and nothing can be re-queried.
                    let results =
                        super::resolve::resolve_rows(&order_client_ids, |_| false, false, |_| {});
                    self.send(TradeDown::OrdersResolved { rid, results });
                }
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::IdentityRecord { rid } => {
                self.session.identity_record(rid, self);
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::Reconcile { rid, kind } => {
                // R31: a venue whose price feed is mid-gap is not reconciled against (the old health
                // gate, now the daemon's evidence): NOT READY, never an empty answer and never a fetch.
                if let Some(reason) = self.session.reconcile_degraded() {
                    self.send(TradeDown::ReconcileNotReady { rid, reason });
                    return TradeLeaseHoldFlow::Continue;
                }
                match self.session.reconcile_tx() {
                    Some(tx) => {
                        if tx
                            .send(ReconcileWorkerJob::Fetch { rid, kind, to: Arc::clone(self) })
                            .is_err()
                        {
                            self.send(TradeDown::ReconcileNotReady {
                                rid,
                                reason: "the reconcile worker has stopped".to_string(),
                            });
                        }
                    }
                    None => {
                        self.send(TradeDown::ReconcileNotReady {
                            rid,
                            reason: self.session.reconcile_unavailable_reason(),
                        });
                    }
                }
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::SymbolProperties { rid, symbol, probe, quoted, forget, modes } => {
                if let Err(why) = check_symbol(&symbol) {
                    return self.protocol(why);
                }
                if forget {
                    // The node's engine gave the symbol back (decision 0205): no lookup, only the cue.
                    self.session.forget_symbol(rid, symbol, self);
                } else {
                    self.session.symbol_facts(rid, symbol, probe, quoted, modes, self);
                }
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::VenueRead { rid, kind } => {
                // Read-only and bounded in the session (`session/venue_read.rs`): not a command, so no
                // cseq, no dedup, no HALT gate and no ring entry.
                self.session.venue_read(rid, kind, self);
                TradeLeaseHoldFlow::Continue
            }
            TradeUp::TradeStatus { rid } => {
                let status: TradeStatusReport = self.session.status();
                self.send(TradeDown::TradeStatusIs { rid, status });
                TradeLeaseHoldFlow::Continue
            }
        }
    }
}

// ---- bounds ---------------------------------------------------------------------------------------

fn check_order_client_id(order_client_id: &str) -> Result<(), String> {
    if order_client_id.is_empty() || order_client_id.len() > MAX_ORDER_CLIENT_ID_BYTES {
        return Err(format!("an order client id of {} bytes", order_client_id.len()));
    }
    Ok(())
}

fn check_order_client_ids(order_client_ids: &[String]) -> Result<(), String> {
    if order_client_ids.len() > MAX_FRAME_ITEMS {
        return Err(format!("{} order client ids in one frame", order_client_ids.len()));
    }
    order_client_ids.iter().try_for_each(|c| check_order_client_id(c))
}

/// A restored order's venue order id, as the client sent it: bounded like an order client id, and never empty (the
/// client leaves an order with no id out of the list).
fn check_venue_order_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_ORDER_CLIENT_ID_BYTES {
        return Err(format!("a venue order id of {} bytes", id.len()));
    }
    Ok(())
}

fn check_symbol(symbol: &str) -> Result<(), String> {
    if symbol.len() > GATEWAY_MAX_SYMBOL_BYTES {
        return Err(format!("a symbol of {} bytes", symbol.len()));
    }
    Ok(())
}

fn check_order(order: &OrderRequest) -> Result<(), String> {
    check_order_client_id(&order.order_client_id)?;
    check_symbol(&order.symbol)?;
    if order.venue.len() > MAX_NAME_BYTES {
        return Err(format!("an order venue of {} bytes", order.venue.len()));
    }
    // The contingency and combo fields are client-named too: length-checked only (an empty id is the
    // core's business), so every order the core sends today still passes. The contingency type is one
    // of three words (`vike_model::ContingencyType`), bounded by its decode.
    for (what, id) in [
        ("a parent order id", order.parent_order_id.as_deref()),
        ("an order list id", order.order_list_id.as_deref()),
    ] {
        if let Some(id) = id.filter(|id| id.len() > MAX_ORDER_CLIENT_ID_BYTES) {
            return Err(format!("{what} of {} bytes", id.len()));
        }
    }
    if order.linked_order_ids.len() > MAX_LINKED_ORDER_IDS {
        return Err(format!("{} linked order ids", order.linked_order_ids.len()));
    }
    if let Some(id) = order.linked_order_ids.iter().find(|id| id.len() > MAX_ORDER_CLIENT_ID_BYTES)
    {
        return Err(format!("a linked order id of {} bytes", id.len()));
    }
    if order.combo_legs.len() > MAX_COMBO_LEGS {
        return Err(format!("{} combo legs", order.combo_legs.len()));
    }
    order.combo_legs.iter().try_for_each(|leg| check_symbol(&leg.symbol))
}

/// The bounds of an adapter start (shared with the tests).
pub fn check_start(start: &AdapterStart) -> Result<(), String> {
    check_symbol(&start.symbol)?;
    if start.declared_legs.len() > MAX_DECLARED_LEGS {
        return Err(format!("{} declared legs", start.declared_legs.len()));
    }
    start.declared_legs.iter().try_for_each(|s| check_symbol(s))?;
    if start.route_key.is_empty() || start.route_key.len() > MAX_ROUTE_KEY_BYTES {
        return Err(format!("a route key of {} bytes", start.route_key.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(order_client_id: &str) -> OrderRequest {
        OrderRequest {
            order_client_id: order_client_id.to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn the_bounds_refuse_an_empty_a_long_and_a_crowded_value() {
        assert!(check_order_client_id("c1").is_ok());
        assert!(check_order_client_id("").is_err());
        assert!(check_order_client_id(&"x".repeat(MAX_ORDER_CLIENT_ID_BYTES + 1)).is_err());
        assert!(check_order_client_id(&"x".repeat(MAX_ORDER_CLIENT_ID_BYTES)).is_ok());
        assert!(check_order_client_ids(&vec!["a".to_string(); MAX_FRAME_ITEMS + 1]).is_err());
        assert!(check_order(&order("c1")).is_ok());
        let mut long_symbol = order("c1");
        long_symbol.symbol = "S".repeat(GATEWAY_MAX_SYMBOL_BYTES + 1);
        assert!(check_order(&long_symbol).is_err());
        let mut long_venue = order("c1");
        long_venue.venue = "v".repeat(MAX_NAME_BYTES + 1);
        assert!(check_order(&long_venue).is_err());
    }

    #[test]
    fn an_adapter_start_is_bounded_in_its_symbol_its_legs_and_its_route_key() {
        let start = AdapterStart {
            rid: 1,
            symbol: "BTCUSDT".to_string(),
            declared_legs: vec!["ETHUSDT".to_string()],
            reconcile_enabled: true,
            takes_trigger: false,
            risk: None,
            market_slippage: None,
            halt_admit: Default::default(),
            withhold: None,
            account_leverage: None,
            read_account_currency: false,
            route_key: "binance#ALT".to_string(),
        };
        assert!(check_start(&start).is_ok());
        let mut many = start.clone();
        many.declared_legs = vec!["X".to_string(); MAX_DECLARED_LEGS + 1];
        assert!(check_start(&many).is_err());
        let mut key = start.clone();
        key.route_key = String::new();
        assert!(check_start(&key).is_err());
        key.route_key = "k".repeat(MAX_ROUTE_KEY_BYTES + 1);
        assert!(check_start(&key).is_err());
        let mut symbol = start;
        symbol.symbol = "S".repeat(GATEWAY_MAX_SYMBOL_BYTES + 1);
        assert!(check_start(&symbol).is_err());
    }
}
