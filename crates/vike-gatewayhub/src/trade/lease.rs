//! A LEASE: one connection's hold on one account's session.
//!
//! The wire adapter (`crate::server::trade`) owns the socket and two threads; this type is everything
//! else. The reader thread feeds each client frame to [`Lease::handle`]; the writer thread pulls each
//! frame it must write from [`Lease::next`]. The lease holds the connection's outbound CONTROL queue
//! (replies, acks, pongs), its position in the account's event ring, the fence and the end state, and
//! nothing that touches a socket: the one way it can reach one is the `closer` closure the adapter hands
//! in, which the lease calls only to force a stuck connection shut.
//!
//! # Events are PULLED, not queued
//!
//! A lease keeps no event queue. [`Lease::next`] reads the account's ring by `seq` (`next_seq`), so a
//! slow client costs the ring's bound and nothing else, a replay after a resume is the same code as the
//! live path, and a client whose next `seq` has fallen below the ring's floor is told so
//! (`ReplayUnavailable{floor}`) and skips to the floor: the ring never drops silently.
//!
//! # The fence
//!
//! A reconnect is FENCED. [`Lease::fence`] tells the old lease's reader to read its connection to the
//! end (everything the client had already sent is processed, and the watermark `received_through_cseq`
//! counts it) and then stop; `TradeOpen` waits for [`Lease::wait_finished`] before it answers the new
//! open. The reader notices the fence between its short waits, and "to the end" means until the
//! connection has been quiet for one slice: a client that is still writing keeps it reading, and
//! `TradeConfig::fence_drain` bounds the wait.
//!
//! # Every client-named value is bounded
//!
//! [`Lease::handle`] checks each frame before it touches any state: string lengths, element counts, the
//! shape of a batch. A frame over a bound is a protocol violation: `Bye(Protocol(..))`, the connection
//! ends. Nothing here indexes, unwraps or `expect`s on wire data.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_exec::CancelIntent;
use vike_gatewayhub_client::proto::{
    AdapterReply, AdapterStart, DetachPhase, GW_MAX_SYMBOL_BYTES, TradeBye, TradeDown,
    TradeStatusReport, TradeUp,
};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderModifyRejected, OrderRejected};

use super::dispatch::ExecCmd;
use super::recon::ReconJob;
use super::session::{AccountSession, Pulled};

/// The longest client order id accepted.
pub const MAX_COID_BYTES: usize = 256;

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
pub enum Flow {
    /// Read the next frame.
    Continue,
    /// The lease is over (a final detach, a protocol violation, a displaced holder): stop reading.
    End,
}

/// What the writer does next.
#[derive(Debug)]
pub enum Next {
    /// Write this frame.
    Frame(TradeDown),
    /// Write this last frame, then close.
    Bye(TradeBye),
    /// Nothing for the heartbeat period: write a `Heartbeat`.
    Idle,
    /// The lease is closed and everything queued was written: end the writer.
    Closed,
}

struct Ctl {
    control: VecDeque<TradeDown>,
    bye: Option<TradeBye>,
    /// Set by anything that may have made `next` productive; consumed before every ring look.
    hint: bool,
    next_seq: u64,
    live_from: u64,
    cap: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One connection's hold on one account's session. See the module doc.
pub struct Lease {
    /// This lease's generation (`TradeOpened.lease_epoch`).
    pub id: u64,
    /// The caller's id for the lease, stable across its reconnects.
    pub client_id: String,
    /// The non-secret fingerprint of the key that authenticated.
    pub key_id: String,
    /// When the lease was granted, epoch ms.
    pub since_ms: i64,
    session: Arc<AccountSession>,
    ctl: Mutex<Ctl>,
    wake: Condvar,
    fenced: AtomicBool,
    ended: AtomicBool,
    final_detach: AtomicBool,
    finished: Mutex<bool>,
    finished_cv: Condvar,
    closer: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("id", &self.id)
            .field("client_id", &self.client_id)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// How a lease is made (the session does it at `TradeOpen`).
pub struct LeaseSeed {
    /// The generation.
    pub id: u64,
    /// The caller's id.
    pub client_id: String,
    /// The key's fingerprint.
    pub key_id: String,
    /// Now, epoch ms.
    pub since_ms: i64,
    /// The first ring `seq` this lease delivers.
    pub next_seq: u64,
    /// Ring entries below this `seq` are replays.
    pub live_from: u64,
    /// The control queue bound.
    pub control_cap: usize,
    /// Forces the connection shut.
    pub closer: Arc<dyn Fn() + Send + Sync>,
}

impl Lease {
    pub(crate) fn new(session: Arc<AccountSession>, seed: LeaseSeed) -> Arc<Self> {
        Arc::new(Self {
            id: seed.id,
            client_id: seed.client_id,
            key_id: seed.key_id,
            since_ms: seed.since_ms,
            session,
            ctl: Mutex::new(Ctl {
                control: VecDeque::new(),
                bye: None,
                hint: true,
                next_seq: seed.next_seq,
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
        let mut c = lock(&self.ctl);
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
    pub(crate) fn next_seq(&self) -> u64 {
        lock(&self.ctl).next_seq
    }

    /// Say a ring append may have made [`Self::next`] productive.
    pub(crate) fn wake_events(&self) {
        lock(&self.ctl).hint = true;
        self.wake.notify_all();
    }

    /// End the lease with `bye` as its last frame. The first one wins.
    pub fn bye(&self, bye: TradeBye) {
        let mut c = lock(&self.ctl);
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
        lock(&self.ctl).hint = true;
        self.wake.notify_all();
    }

    /// The daemon is stopping: `Bye(ServerStopping)`.
    pub fn stop(&self) {
        self.bye(TradeBye::ServerStopping);
    }

    /// The next frame to write, waiting up to `idle` for one: control frames first, then the ring in
    /// `seq` order, then the closing `Bye`, then [`Next::Idle`].
    pub fn next(&self, idle: Duration) -> Next {
        let deadline = Instant::now() + idle;
        loop {
            let next_seq = {
                let mut c = lock(&self.ctl);
                if let Some(frame) = c.control.pop_front() {
                    return Next::Frame(frame);
                }
                if let Some(bye) = c.bye.take() {
                    self.ended.store(true, Ordering::SeqCst);
                    return Next::Bye(bye);
                }
                if self.ended.load(Ordering::SeqCst) {
                    return Next::Closed;
                }
                c.hint = false;
                c.next_seq
            };
            match self.session.pull(next_seq) {
                Pulled::Event { entry, account, gw_epoch } => {
                    let mut c = lock(&self.ctl);
                    c.next_seq = entry.seq + 1;
                    let replayed = entry.seq < c.live_from;
                    return Next::Frame(TradeDown::Event {
                        account: account.to_string(),
                        gw_epoch,
                        seq: entry.seq,
                        ts_ms: entry.ts_ms,
                        replayed,
                        event: entry.event,
                    });
                }
                Pulled::Gap { floor } => {
                    // The ring evicted what this lease had not read yet: say so, never skip silently.
                    lock(&self.ctl).next_seq = floor;
                    tracing::warn!(
                        client = %self.client_id,
                        floor,
                        "TRADE-LINK: a slow lease fell behind the event ring; the client is told to reconcile"
                    );
                    return Next::Frame(TradeDown::ReplayUnavailable { floor });
                }
                Pulled::Nothing => {}
            }
            let c = lock(&self.ctl);
            if c.hint || !c.control.is_empty() || c.bye.is_some() {
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return Next::Idle;
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

    fn protocol(&self, words: String) -> Flow {
        tracing::warn!(client = %self.client_id, "TRADE-LINK: protocol violation: {words}");
        self.bye(TradeBye::Protocol(words));
        Flow::End
    }

    /// Queue `cmd` for the adapter, or, when it cannot reach one, put the event the contract owes in the
    /// ring: a submit is a terminal reject, a cancel or a modify a non-terminal advisory.
    fn dispatch(&self, cmd: ExecCmd) {
        if let Err(cmd) = self.session.enqueue(cmd, || self.live()) {
            self.session.synthesize(cmd);
        }
    }

    /// The terminal reject of a submit that names another venue or account than this lease: it never
    /// reaches the adapter, and the lease goes on.
    fn refuse_submit(&self, order: &OrderRequest, reason: &str) {
        tracing::warn!(
            client = %self.client_id,
            "TRADE-LINK: an order names another venue or account than its lease and was refused"
        );
        self.session.emit_event(Event::OrderRejected(OrderRejected {
            client_order_id: order.client_order_id.clone(),
            reason: reason.into(),
            ts: order.ts,
        }));
    }

    /// Handle one decoded client frame. See the module doc for the bounds.
    pub fn handle(self: &Arc<Self>, frame: TradeUp) -> Flow {
        if !self.session.lease_is_current(self.id) {
            // Displaced (a takeover whose fence timed out): a late frame from an old connection.
            return Flow::End;
        }
        match frame {
            TradeUp::Ping => {
                self.send(TradeDown::Pong);
                Flow::Continue
            }
            TradeUp::Detach { phase: DetachPhase::Begin } => Flow::Continue,
            TradeUp::Detach { phase: DetachPhase::Final } => {
                self.final_detach.store(true, Ordering::SeqCst);
                self.bye(TradeBye::ClientClosed);
                Flow::End
            }
            TradeUp::AdapterStart(start) => {
                if let Err(why) = check_start(&start) {
                    return self.protocol(why);
                }
                let rid = start.rid;
                let reply: AdapterReply = self.session.start_adapter(start);
                self.send(TradeDown::AdapterReply { rid, reply: Box::new(reply) });
                Flow::Continue
            }
            TradeUp::Submit { cseq, order } => {
                if let Err(why) = check_order(&order) {
                    return self.protocol(why);
                }
                self.session.note_cseq(&self.client_id, cseq);
                match self.session.identity_conflict(&order) {
                    Some(reason) => self.refuse_submit(&order, &reason),
                    None => self.dispatch(ExecCmd::Submit(Box::new(order))),
                }
                self.send(TradeDown::CmdAck { cseq });
                Flow::Continue
            }
            TradeUp::SubmitBatch { cseq, orders } => {
                if orders.len() > MAX_FRAME_ITEMS {
                    return self.protocol(format!("a batch of {} orders", orders.len()));
                }
                if let Some(why) = orders.iter().find_map(|o| check_order(o).err()) {
                    return self.protocol(why);
                }
                self.session.note_cseq(&self.client_id, cseq);
                let mut kept = Vec::with_capacity(orders.len());
                for order in orders {
                    match self.session.identity_conflict(&order) {
                        Some(reason) => self.refuse_submit(&order, &reason),
                        None => kept.push(order),
                    }
                }
                if !kept.is_empty() {
                    self.dispatch(ExecCmd::SubmitBatch(kept));
                }
                self.send(TradeDown::CmdAck { cseq });
                Flow::Continue
            }
            TradeUp::Cancel { cseq, coid, intent } => {
                if let Err(why) = check_coid(&coid) {
                    return self.protocol(why);
                }
                self.session.note_cseq(&self.client_id, cseq);
                self.dispatch(ExecCmd::Cancel { coid, intent: CancelIntent::from(intent) });
                self.send(TradeDown::CmdAck { cseq });
                Flow::Continue
            }
            TradeUp::CancelBatch { cseq, coids, intent } => {
                if let Err(why) = check_coids(&coids) {
                    return self.protocol(why);
                }
                self.session.note_cseq(&self.client_id, cseq);
                self.dispatch(ExecCmd::CancelBatch { coids, intent: CancelIntent::from(intent) });
                self.send(TradeDown::CmdAck { cseq });
                Flow::Continue
            }
            TradeUp::Modify { cseq, order, new_qty, new_price } => {
                if let Err(why) = check_order(&order) {
                    return self.protocol(why);
                }
                self.session.note_cseq(&self.client_id, cseq);
                match self.session.identity_conflict(&order) {
                    Some(reason) => {
                        self.session.emit_event(Event::OrderModifyRejected(OrderModifyRejected {
                            client_order_id: order.client_order_id.clone(),
                            reason: reason.into(),
                            ts: order.ts,
                        }));
                    }
                    None => {
                        self.dispatch(ExecCmd::Modify {
                            order: Box::new(order),
                            new_qty,
                            new_price,
                        });
                    }
                }
                self.send(TradeDown::CmdAck { cseq });
                Flow::Continue
            }
            TradeUp::Confirm { cseq, coid } => {
                if let Err(why) = check_coid(&coid) {
                    return self.protocol(why);
                }
                self.session.note_cseq(&self.client_id, cseq);
                self.dispatch(ExecCmd::Confirm(coid));
                self.send(TradeDown::CmdAck { cseq });
                Flow::Continue
            }
            TradeUp::AdoptRestored { cseq, pairs } => {
                if pairs.len() > MAX_FRAME_ITEMS {
                    return self.protocol(format!("{} restored pairs in one frame", pairs.len()));
                }
                if let Some(why) = pairs.iter().find_map(|(coid, symbol)| {
                    check_coid(coid).err().or_else(|| check_symbol(symbol).err())
                }) {
                    return self.protocol(why);
                }
                self.session.note_cseq(&self.client_id, cseq);
                self.dispatch(ExecCmd::Adopt(pairs));
                self.send(TradeDown::CmdAck { cseq });
                Flow::Continue
            }
            TradeUp::WireIds { rid, coids } => {
                if let Err(why) = check_coids(&coids) {
                    return self.protocol(why);
                }
                let cmd = ExecCmd::WireIds { rid, coids, to: Arc::clone(self) };
                if let Err(ExecCmd::WireIds { rid, .. }) = self.session.enqueue(cmd, || self.live())
                {
                    // No adapter: no coid has a wire id that differs. An answer, not a hang.
                    self.send(TradeDown::WireIdsReply { rid, pairs: Vec::new() });
                }
                Flow::Continue
            }
            TradeUp::ResolveOrders { rid, coids } => {
                if let Err(why) = check_coids(&coids) {
                    return self.protocol(why);
                }
                let cmd = ExecCmd::Resolve { rid, coids, to: Arc::clone(self) };
                if let Err(ExecCmd::Resolve { rid, coids, .. }) =
                    self.session.enqueue(cmd, || self.live())
                {
                    // No adapter: nothing is tracked and nothing can be re-queried.
                    let results = super::resolve::resolve_rows(&coids, |_| false, false, |_| {});
                    self.send(TradeDown::OrdersResolved { rid, results });
                }
                Flow::Continue
            }
            TradeUp::IdentityRecord { rid } => {
                self.session.identity_record(rid, self);
                Flow::Continue
            }
            TradeUp::Recon { rid, kind } => {
                // R31: a venue whose price feed is mid-gap is not reconciled against (the old health
                // gate, now the daemon's evidence): NOT READY, never an empty answer and never a fetch.
                if let Some(reason) = self.session.recon_degraded() {
                    self.send(TradeDown::ReconNotReady { rid, reason });
                    return Flow::Continue;
                }
                match self.session.recon_tx() {
                    Some(tx) => {
                        if tx.send(ReconJob::Fetch { rid, kind, to: Arc::clone(self) }).is_err() {
                            self.send(TradeDown::ReconNotReady {
                                rid,
                                reason: "the reconcile worker has stopped".to_string(),
                            });
                        }
                    }
                    None => {
                        self.send(TradeDown::ReconNotReady {
                            rid,
                            reason: self.session.recon_unavailable_reason(),
                        });
                    }
                }
                Flow::Continue
            }
            TradeUp::SymbolFacts { rid, symbol, probe } => {
                if let Err(why) = check_symbol(&symbol) {
                    return self.protocol(why);
                }
                self.session.symbol_facts(rid, symbol, probe, self);
                Flow::Continue
            }
            TradeUp::TradeStatus { rid } => {
                let status: TradeStatusReport = self.session.status();
                self.send(TradeDown::TradeStatusIs { rid, status });
                Flow::Continue
            }
        }
    }
}

// ---- bounds ---------------------------------------------------------------------------------------

fn check_coid(coid: &str) -> Result<(), String> {
    if coid.is_empty() || coid.len() > MAX_COID_BYTES {
        return Err(format!("a client order id of {} bytes", coid.len()));
    }
    Ok(())
}

fn check_coids(coids: &[String]) -> Result<(), String> {
    if coids.len() > MAX_FRAME_ITEMS {
        return Err(format!("{} client order ids in one frame", coids.len()));
    }
    coids.iter().try_for_each(|c| check_coid(c))
}

fn check_symbol(symbol: &str) -> Result<(), String> {
    if symbol.len() > GW_MAX_SYMBOL_BYTES {
        return Err(format!("a symbol of {} bytes", symbol.len()));
    }
    Ok(())
}

fn check_order(order: &OrderRequest) -> Result<(), String> {
    check_coid(&order.client_order_id)?;
    check_symbol(&order.symbol)?;
    if order.venue.len() > MAX_NAME_BYTES {
        return Err(format!("an order venue of {} bytes", order.venue.len()));
    }
    // The contingency and combo fields are client-named too: length-checked only (an empty id is the
    // core's business), so every order the core sends today still passes.
    for (what, id) in [
        ("a parent order id", order.parent_order_id.as_deref()),
        ("an order list id", order.order_list_id.as_deref()),
    ] {
        if let Some(id) = id.filter(|id| id.len() > MAX_COID_BYTES) {
            return Err(format!("{what} of {} bytes", id.len()));
        }
    }
    if let Some(kind) = order.contingency_type.as_deref().filter(|k| k.len() > MAX_NAME_BYTES) {
        return Err(format!("a contingency type of {} bytes", kind.len()));
    }
    if order.linked_order_ids.len() > MAX_LINKED_ORDER_IDS {
        return Err(format!("{} linked order ids", order.linked_order_ids.len()));
    }
    if let Some(id) = order.linked_order_ids.iter().find(|id| id.len() > MAX_COID_BYTES) {
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

    fn order(coid: &str) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn the_bounds_refuse_an_empty_a_long_and_a_crowded_value() {
        assert!(check_coid("c1").is_ok());
        assert!(check_coid("").is_err());
        assert!(check_coid(&"x".repeat(MAX_COID_BYTES + 1)).is_err());
        assert!(check_coid(&"x".repeat(MAX_COID_BYTES)).is_ok());
        assert!(check_coids(&vec!["a".to_string(); MAX_FRAME_ITEMS + 1]).is_err());
        assert!(check_order(&order("c1")).is_ok());
        let mut long_symbol = order("c1");
        long_symbol.symbol = "S".repeat(GW_MAX_SYMBOL_BYTES + 1);
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
            recon_enabled: true,
            takes_trigger: false,
            risk: None,
            market_slippage: None,
            halt_admit: Default::default(),
            withhold: None,
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
        symbol.symbol = "S".repeat(GW_MAX_SYMBOL_BYTES + 1);
        assert!(check_start(&symbol).is_err());
    }
}
