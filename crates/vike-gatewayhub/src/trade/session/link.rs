//! **A session's VENUE LINK**: what the daemon does when the connection between its adapter and the venue
//! drops by itself (decision 0164 verdict 7(b), wave 2 W14 and W14b; today IBKR's SOCKET gateway, the only
//! venue whose bridge latches dead for good).
//!
//! A child of `session` because it works the session's private state: the adapter, the ring, the lease.
//! The POLICY (probe, backoff, ask the rows before every attempt) is `crate::session::reconnect`'s and the
//! venue's facts are the lane's `LinkWatch`; this file is the session half the loop drives.
//!
//! # The three states
//!
//! ```text
//!  Up ---- two probes in a row fail (or a bridge reports its link gone) ----> Down ---- the rows say stop ----> GaveUp
//!   ^                                                             |
//!   '------------------- restore succeeds ------------------------'
//! ```
//!
//! * **Down.** At the loss, in this order: the state goes Down and the adapter is marked dead (every command
//!   still queued is ANSWERED from then on, never dispatched to a bridge that is going away); a bounded moment
//!   is given for a bridge that REPORTS its link gone (`ExecLink{up:false}`, which the IBKR bridge does at
//!   its stream's death: a loss it reported needs no wait; one a probe found first waits out the grace) to
//!   name the orders it had in flight;
//!   the dead adapter is torn down (its client must not linger: the socket library the bridge wraps
//!   reconnects by itself, and a zombie client that came back to the returning gateway would hold
//!   the client ids the rebuilt one needs); the `AdapterStart` is parked; and **the attached lease is ended
//!   with `Bye(ServerStopping)`, after the ring has been written to it**, so the CLIENT's link goes down NOW
//!   (its `ExecLink` down, the strategies' `Disconnected`, the stuck-order watchdog deferred) instead of
//!   staying "up" for the ~85 s the gateway is away. While down, a client's `TradeOpen` is refused with
//!   words (a plain error it retries on its own schedule, which keeps its link reporting down: `Live` is
//!   announced only when the venue session is back) and the status report says the adapter is not up. Nothing
//!   is sent to the venue and no order vanishes.
//! * **Restore** (one attempt, only while the rows still say go and the port answers): the adapter is
//!   first the ring gets a NEW EPOCH (under the open lock; a lease still attached, none can be, is ended), BEFORE
//!   the adapter exists, so whatever a bridge pushes the moment it connects belongs to the new ring and is
//!   delivered to the first lease (it used to be swapped after the start and the replay was lost with the old
//!   ring). The rebuilt adapter's coid map is empty, which is the situation `gw_epoch` exists to name: the
//!   client answers a foreign epoch with a `ReplayUnavailable` reconcile and its `epoch_changed` re-teaching.
//!   Then the adapter is started again over the parked `AdapterStart` with NO lock held for the venue's login
//!   (it can take long: the daemon's stop and the reaper must not wait for it; `start_adapter_locked` takes
//!   the adapter lock itself, after the login, to install, and tears the adapter down if the session was
//!   retired meanwhile). An attempt the venue refuses changes nothing else. Once it is up, under the open
//!   lock: the session is marked up (and HELD, below) and a `ReconcileNow` is owed to the next lease. The
//!   client's next redial is the one that announces `Live`.
//! * **GaveUp.** The rows said stop (the account is no longer active at its tier, or the venue-scoped two-tier
//!   conflict of decision 0140 arrived). The words are logged and carried by every later command's reject.
//!   A client's own `AdapterStart` clears the state and starts afresh, which the lane then judges by the
//!   rows itself. (A rows READ that failed is not a stop: the loop waits and asks again.)
//!
//! # Held: a rebuilt adapter that cannot manage the orders of the one before it
//!
//! No resting order is cancelled by any of this (owner ruling D6): they sit at the venue. A rebuilt IBKR
//! bridge starts BLIND to them: its coid-to-order-id map is empty, so until it has asked the venue it can
//! neither cancel nor modify them (the cancel is refused, nothing is sent) and it misses their fills. An
//! engine that believed otherwise would hold a cancel it thinks is pending, or a position short of what the
//! venue holds. So the daemon keeps a LEDGER ([`Resting`]) of the orders it sent and has not seen finish, and
//! **when an adapter REPLACES another (a restore, or a client's different `AdapterStart`) over a non-empty
//! ledger the session is HELD**: every order verb (submit, cancel, modify) is answered with a terminal reject
//! (a submit) or an advisory (a cancel or modify) carrying `held_words`, and is NOT sent. Queries, the
//! reconcile and the events of the venue go on.
//!
//! **The hold is released by the bridge itself.** The rebuilt IBKR bridge asks the venue for its open orders
//! the moment it connects, rebinds the ones THIS client placed (the others it ignores), replays the day's
//! executions, and only then announces `ExecLink{up:true}` (`vike-ibkr`'s `exec.rs`, `announce_link`). The
//! forwarder takes that as "this bridge is no longer blind": [`AccountSession::bridge_learned`] clears the
//! hold, and `hold_if_blind` does not set one for a bridge that announced before it was installed. A coid
//! the venue no longer lists is finished (filled or cancelled while the connection was down): the bridge
//! does not claim to know which, the reconcile the restore owes reports it. **If the bridge's learning
//! FAILS** (a request errors or does not end in time) it announces nothing and the hold STAYS: the operator
//! cancels or confirms the resting orders at the venue, then ends the session (a client's final `Detach`,
//! the orphan timeout) or restarts the daemon. A replacement over an EMPTY ledger is not held. The reconcile
//! the restore owes still reports (under the default `quarantine` policy it reports and folds nothing).
//!
//! **The handover (W14f).** A new adapter's client is taught `(coid, symbol)` for every order the ledger holds
//! BEFORE its bridge may replay the day's executions (`start_adapter_locked` calls `adopt_restored` as soon as the
//! lane returns the client; the IBKR bridge holds its replay until then), because a replayed fill is stamped with
//! IB's own spelling of the instrument and the engine drops one it does not mount. That books the fills of orders
//! THIS daemon sent; the fills of orders only the tradehub knew are not recovered.
//!
//! **A bridge that never learns is held too.** [`AccountSession::watch_learning`] gives a supervised venue's bridge
//! [`LEARN_BUDGET`] to announce `ExecLink{up:true}`; past it the session is held with [`unlearned_words`], on a first
//! start as on a rebuild, and a later announcement releases it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_config::{ArmingBlock, VenueArming};
use vike_gatewayhub_client::proto::{AdapterReply, TradeBye};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::parse_wire_account;
use vike_model::events::Event;

use super::{AccountSession, EventRing, Inner, OpenRefusal, lock, mint_epoch, word};
use crate::session::reconnect::{self, Link, Pause, Verdict};
use crate::trade::dispatch::ExecCmd;
use crate::trade::lane::{LinkWatch, TradeTarget};
use crate::trade::lease::Lease;

/// The state of a session's supervised venue link. See the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LinkState {
    /// The link is up, or the venue has none the daemon supervises.
    Up,
    /// The link dropped; the daemon is bringing it back.
    Down {
        /// Why the link was called lost.
        why: String,
    },
    /// The rows said stop; the daemon is not bringing it back.
    GaveUp {
        /// What the rows said.
        why: String,
    },
}

impl LinkState {
    pub(super) fn is_up(&self) -> bool {
        matches!(self, LinkState::Up)
    }

    /// The words a command rejected by this state carries (`None` while up).
    pub(super) fn reason(&self, venue: &str) -> Option<String> {
        match self {
            LinkState::Up => None,
            LinkState::Down { why } => Some(format!(
                "gatewayhub: the {venue} venue session is down ({why}); the daemon is re-establishing it and this command was NOT sent"
            )),
            LinkState::GaveUp { why } => Some(format!(
                "gatewayhub: the {venue} venue session was lost and the daemon is not re-establishing it ({why}); this command was NOT sent"
            )),
        }
    }
}

/// How long the lease being replaced is given to be read to its end, after the fence, once it is forced shut.
const FORCED_CLOSE_WAIT: Duration = Duration::from_secs(2);

/// The most a loss found by a PROBE waits for the bridge to say so itself before the adapter is torn down
/// (so the bridge can name the orders it had in flight in its own stream-death log; the teardown used to
/// pre-empt it). A loss the bridge reported needs no wait. Also capped at five probe periods, so a test's
/// millisecond schedule does not wait for a production number.
const BRIDGE_REPORT_GRACE: Duration = Duration::from_millis(1_500);

/// The slice a pause sleeps in, so a stop request or a bridge's report is heard promptly.
const PAUSE_SLICE: Duration = Duration::from_millis(50);

impl AccountSession {
    /// A client's `AdapterStart` while the daemon is bringing the link back: refused with words, and not
    /// acted on (the supervisor holds the adapter lock for the whole of an attempt). A `GaveUp` session
    /// clears its state here and lets the start through. `None` = go on.
    ///
    /// (Defence in depth since W14b: a client holds no lease while the link is down, because
    /// [`Self::link_refuses_open`] refuses it, so the start can only come from a lease that attached before
    /// the loss and is already being ended.)
    pub(super) fn link_refuses_start(&self) -> Option<AdapterReply> {
        let mut inner = lock(&self.inner);
        match &inner.link {
            LinkState::Up => None,
            LinkState::Down { .. } => {
                let text = inner.link.reason(&self.key.0).unwrap_or_default();
                Some(AdapterReply::NotRunning {
                    block_word: word(ArmingBlock::AdapterFailed),
                    refusal_text: text,
                })
            }
            LinkState::GaveUp { .. } => {
                inner.link = LinkState::Up;
                inner.parked = None;
                None
            }
        }
    }

    /// A `TradeOpen` while the daemon is bringing the link back: refused with words, so the client's own
    /// link stays down until the venue session is back. A plain error, NOT a `TradeRefused` (the client
    /// closes its lease for good after three refusals, and this one is momentary by design). `GaveUp` lets
    /// the open through: the account's own state is judged by the lane's `account_tier` (an inactive account
    /// is refused there) and a client must be able to start the account afresh once it runs again.
    pub(super) fn link_refuses_open(&self) -> Option<OpenRefusal> {
        match &lock(&self.inner).link {
            LinkState::Down { why } => Some(OpenRefusal::Error(format!(
                "gatewayhub: the {} venue session is down ({why}); the daemon is re-establishing it, open again when it is back",
                self.key.0
            ))),
            LinkState::Up | LinkState::GaveUp { .. } => None,
        }
    }

    /// What the running adapter's bridge said about its own venue link, if it said it is gone. `None` while
    /// the session is down (there is no adapter) or the bridge has said nothing.
    pub(super) fn reported_down(&self) -> Option<String> {
        let inner = lock(&self.inner);
        if !inner.link.is_up() {
            return None;
        }
        let adapter = inner.adapter.as_ref()?;
        lock(&adapter.reported).clone()
    }

    /// Run the supervisor over `watch` (the lane's link to watch, asked for once per start) beside the adapter
    /// that just came up. Replaces the supervisor of an earlier start.
    pub(super) fn supervise(self: &Arc<Self>, target: &TradeTarget, watch: Arc<dyn LinkWatch>) {
        self.stop_supervisor();
        let schedule = watch.schedule();
        if !schedule.is_sane() {
            tracing::error!(
                venue = %self.key.0,
                "TRADE-LINK: the venue link's schedule is not usable; it is not supervised"
            );
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        lock(&self.inner).supervised = Some(Arc::clone(&stop));
        let link = SessionLink {
            session: Arc::clone(self),
            watch,
            tier: target.tier,
            stop: Arc::clone(&stop),
            grace: schedule.probe_every.saturating_mul(5).min(BRIDGE_REPORT_GRACE),
        };
        let pause = LinkPause { stop: Arc::clone(&stop), session: Arc::clone(self) };
        let spawned = std::thread::Builder::new().name("gwh-trade-link".into()).spawn(move || {
            reconnect::supervise(&link, &schedule, &pause);
            link.session.supervisor_ended(&link.stop);
        });
        match spawned {
            Ok(_) => tracing::info!(
                venue = %self.key.0,
                probe_s = schedule.probe_every.as_secs_f64(),
                "TRADE-LINK: the venue link is supervised (a loss is reconnected while the account rows say it runs)"
            ),
            Err(e) => {
                self.supervisor_ended(&stop);
                tracing::error!(
                    venue = %self.key.0,
                    error = %e,
                    "TRADE-LINK: could not start the link supervisor; the venue link is not supervised"
                );
            }
        }
    }

    /// Ask the running supervisor, if any, to end. It is not joined: it ends at its next pause, and one
    /// that is inside an attempt finds the stop flag raised before it touches the session again.
    pub(super) fn stop_supervisor(&self) {
        if let Some(stop) = lock(&self.inner).supervised.take() {
            stop.store(true, Ordering::SeqCst);
        }
    }

    fn supervisor_ended(&self, stop: &Arc<AtomicBool>) {
        let mut inner = lock(&self.inner);
        if inner.supervised.as_ref().is_some_and(|s| Arc::ptr_eq(s, stop)) {
            inner.supervised = None;
        }
    }

    fn link_stopped(&self, stop: &AtomicBool) -> bool {
        stop.load(Ordering::SeqCst) || self.retired.load(Ordering::SeqCst) || self.hub_stopping()
    }

    /// The adapter is lost (the bridge said so, or two probes failed): mark the link down, answer what is
    /// queued, let the bridge name its orders, tear the dead adapter down and end the lease. See the module
    /// doc for the order, which matters.
    fn link_lost(&self, stop: &AtomicBool, why: &str, grace: Duration) {
        // The documented order: open, then adapter. Taking the open lock keeps an open that is mid-attach from
        // slipping a lease in between the state change and the lease's end; one that comes after finds the
        // session down and is refused.
        let _open = lock(&self.open_lock);
        let _adapter = lock(&self.adapter_lock);
        if self.link_stopped(stop) {
            return;
        }
        let reported = {
            let mut inner = lock(&self.inner);
            let Some(adapter) = inner.adapter.as_ref() else { return };
            let parked = adapter.start.clone();
            // From here the dispatch thread answers every command it reads (`dispatch::run`); the one in the
            // bridge's hands, if any, is the bridge's to name.
            adapter.dead.store(true, Ordering::SeqCst);
            let reported = lock(&adapter.reported).is_some();
            inner.parked = Some(parked);
            inner.link = LinkState::Down { why: why.to_string() };
            reported
        };
        tracing::warn!(
            venue = %self.key.0,
            why,
            "TRADE-LINK: the venue session was lost: queued commands are answered, the adapter is torn down, \
             the client's lease is ended and the daemon re-establishes the session while the account rows say it runs"
        );
        if !reported {
            self.await_bridge_report(stop, grace);
        }
        self.stop_adapter_locked();
        self.end_lease();
    }

    /// Give the bridge a bounded moment to say its link is gone itself (a probe found the loss first): its
    /// stream-death log names the orders it had in flight, which the teardown would otherwise pre-empt.
    fn await_bridge_report(&self, stop: &AtomicBool, grace: Duration) {
        let deadline = Instant::now() + grace;
        loop {
            let said = {
                let inner = lock(&self.inner);
                inner.adapter.as_ref().is_some_and(|a| lock(&a.reported).is_some())
            };
            if said || stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// End the attached lease, if any: what the ring holds (the rejects of the commands that were queued at
    /// the loss included) is written to it first, then `Bye(ServerStopping)`, then the connection is read to
    /// its end. The client discloses its own link down and redials; the redial is refused while the session
    /// is down.
    fn end_lease(&self) {
        let lease = lock(&self.inner).lease.clone();
        let Some(old) = lease else { return };
        if !old.is_ended() {
            self.drain_ring_to(&old);
            old.bye(TradeBye::ServerStopping);
            old.fence();
        }
        if !old.wait_finished(self.shared.cfg.fence_drain) {
            old.force_close();
            old.wait_finished(FORCED_CLOSE_WAIT);
        }
    }

    /// Wait (bounded by the fence drain) until `lease` has taken every event the ring holds now. A `Bye`
    /// raised before that is written BEFORE the events behind it, which the client would never see.
    fn drain_ring_to(&self, lease: &Lease) {
        let last = lock(&self.inner).ring.last_seq();
        let deadline = Instant::now() + self.shared.cfg.fence_drain;
        while lease.next_seq() <= last && !lease.is_ended() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// One attempt to bring the adapter back. See the module doc for the order of the steps: the adapter is
    /// started FIRST, so a refused attempt (the gateway is up and not logged in yet) touches nothing and a
    /// client is not bounced once per attempt; only a start that came up changes the epoch.
    fn link_restore(self: &Arc<Self>, stop: &AtomicBool) -> Result<(), String> {
        if self.link_stopped(stop) {
            return Err("the session is stopping".to_string());
        }
        let Some(start) = lock(&self.inner).parked.clone() else {
            return Err("no adapter start is parked for this session".to_string());
        };
        // A NEW EPOCH, minted BEFORE the adapter exists: the rebuilt adapter knows none of the orders the lost
        // one did, and everything it pushes (a bridge may replay events the moment it connects) must belong
        // to the new ring. Swapping after the start dropped exactly those first events with the old ring.
        // No lease can be attached (an open is refused while the link is down), but one that raced the loss
        // is ended the same way.
        {
            let _open = lock(&self.open_lock);
            if self.link_stopped(stop) {
                return Err("the session is stopping".to_string());
            }
            self.end_lease();
            let mut inner = lock(&self.inner);
            inner.ring = EventRing::new(mint_epoch(), self.shared.cfg.ring_events);
            inner.once.clear();
            inner.watermarks.clear();
        }
        // The venue's login can take long and holds NO lock (`start_adapter_locked` takes the adapter lock
        // itself, to install): the open lock is what an open, the reaper and the daemon's stop wait on, and
        // the adapter lock is what the stop's teardown waits on. The link stays DOWN through the start: a
        // command that arrives meanwhile is still answered with the reason, never handed to an adapter whose
        // epoch the client does not know yet.
        if let AdapterReply::NotRunning { block_word, refusal_text } =
            self.start_adapter_locked(start, true)
        {
            return Err(format!("{block_word}: {refusal_text}"));
        }
        let _open = lock(&self.open_lock);
        if self.link_stopped(stop) {
            // A retire or the daemon's stop tears the adapter just started down itself.
            return Err("the session is stopping".to_string());
        }
        {
            let mut inner = lock(&self.inner);
            inner.link = LinkState::Up;
            inner.parked = None;
            inner.carry = true;
        }
        tracing::info!(
            venue = %self.key.0,
            "TRADE-LINK: the venue session was re-established: a new epoch, a fresh adapter and a reconcile owed"
        );
        self.reconcile_now("the venue session was re-established after a loss");
        Ok(())
    }

    fn link_gave_up(&self, why: &str) {
        {
            let mut inner = lock(&self.inner);
            inner.link = LinkState::GaveUp { why: why.to_string() };
            inner.parked = None;
        }
        tracing::warn!(
            venue = %self.key.0,
            why,
            "TRADE-LINK: the venue session is NOT being re-established: the account rows no longer say it runs"
        );
    }
}

/// The most orders the ledger names; past it the ledger only remembers THAT it overflowed (and is then
/// never empty), which is the safe direction.
const MAX_RESTING: usize = 4096;

/// The most fill ids the ledger remembers per order (the dedup of replayed fills). Past it the order is never
/// retired by its fills (the safe direction: it stays in the ledger until a terminal event names it).
const MAX_FILLS_PER_ORDER: usize = 256;

/// One order the ledger holds.
#[derive(Debug, Default)]
struct Sent {
    /// The quantity the order asked for (0 = unknown: the order is retired only by a terminal event).
    qty: f64,
    /// The quantity of its fills seen so far, each execution counted once.
    filled: f64,
    /// The trade ids counted in `filled`.
    seen: HashSet<String>,
    /// More fills than `MAX_FILLS_PER_ORDER`: `filled` is no longer trusted.
    untrusted: bool,
    /// The symbol the order was sent under (the unified one the engine registered it with): what a rebuilt
    /// bridge is TAUGHT about the order before it replays the day's executions (see `symbols`). Empty = unknown.
    symbol: String,
}

/// The orders a SUPERVISED session sent toward its venue and has not seen finish, by client order id: the
/// only thing the daemon knows about what may rest at the venue (or be in flight to it) across a rebuild of
/// the adapter. It is filled when a `Submit` is queued for the adapter and emptied, per order, by a terminal
/// event (`finished`) **or by its fills adding up to the quantity it asked for**: a bridge that learns an
/// order finished while it was down reports the fills it missed as PARTIAL fills (it cannot know the order's
/// total), so without the sum such an order would stay here for ever and hold every later rebuild. An order
/// whose fate the adapter never reported stays in it: that is the point.
#[derive(Debug, Default)]
pub(super) struct Resting {
    coids: HashMap<String, Sent>,
    overflowed: bool,
}

impl Resting {
    /// A `Submit` for `coid` (asking for `qty`, on `symbol`) was queued for the adapter.
    pub(super) fn sent(&mut self, coid: &str, qty: f64, symbol: &str) {
        if self.coids.len() >= MAX_RESTING && !self.coids.contains_key(coid) {
            self.overflowed = true;
            return;
        }
        let qty = if qty.is_finite() && qty > 0.0 { qty } else { 0.0 };
        let sent = self.coids.entry(coid.to_string()).or_default();
        sent.qty = qty;
        sent.symbol = symbol.to_string();
    }

    /// The `(coid, symbol)` pairs of the orders the ledger holds, sorted: what a NEW bridge is taught before it
    /// replays the day's executions, so the fills it replays of these orders carry the symbol the engine knows
    /// them by (the execution's own instrument spelling never equals it: `AAPL.IBEOS.USD` for `AAPL.SMART.USD`).
    /// An order sent with no symbol is not in it.
    pub(super) fn symbols(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = self
            .coids
            .iter()
            .filter(|(_, sent)| !sent.symbol.is_empty())
            .map(|(coid, sent)| (coid.clone(), sent.symbol.clone()))
            .collect();
        pairs.sort_unstable();
        pairs
    }

    /// An event passed through the ring: an order that finished leaves the ledger.
    pub(super) fn finished(&mut self, event: &Event) {
        let coid = match event {
            Event::OrderRejected(e) => &e.client_order_id,
            Event::OrderDenied(e) => &e.client_order_id,
            Event::OrderFilled(e) => &e.client_order_id,
            Event::OrderCanceled(e) => &e.client_order_id,
            Event::OrderExpired(e) => &e.client_order_id,
            Event::OrderPartiallyFilled(e) => {
                self.filled(&e.client_order_id, e.fill.trade_id.as_str(), e.fill.last_qty);
                return;
            }
            _ => return,
        };
        self.coids.remove(coid);
    }

    /// One fill of `coid` (a partial) passed: when the fills add up to the order's quantity the order is done.
    fn filled(&mut self, coid: &str, trade_id: &str, qty: f64) {
        const EPS: f64 = 1e-9;
        let Some(sent) = self.coids.get_mut(coid) else { return };
        if sent.untrusted || !qty.is_finite() || qty <= 0.0 {
            return;
        }
        if !sent.seen.insert(trade_id.to_string()) {
            return; // a replay of a fill already counted
        }
        if sent.seen.len() > MAX_FILLS_PER_ORDER {
            sent.untrusted = true;
            return;
        }
        sent.filled += qty;
        if sent.qty > 0.0 && sent.filled + EPS >= sent.qty {
            self.coids.remove(coid);
        }
    }

    /// Nothing may rest: no order is open and the ledger never overflowed.
    pub(super) fn is_empty(&self) -> bool {
        self.coids.is_empty() && !self.overflowed
    }

    /// The coids the ledger holds now.
    pub(super) fn snapshot(&self) -> HashSet<String> {
        self.coids.keys().cloned().collect()
    }

    /// Whether the ledger lost track (more orders than it holds).
    pub(super) fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// An order the daemon has decided is finished leaves the ledger.
    pub(super) fn forget(&mut self, coid: &str) {
        self.coids.remove(coid);
    }
}

/// What the BRIDGE of one adapter said about its learning of the venue, as the forwarder saw it: whether it
/// announced `ExecLink{up:true}`, and every coid it announced since it started (an `OrderAccepted` for each
/// order it rebound, the fills and terminals it reported). Per adapter, created before the adapter starts, so
/// what the bridge pushed the moment it connected is counted whatever the install order.
#[derive(Debug, Default)]
pub(super) struct BridgeLearning {
    learned: AtomicBool,
    announced: Mutex<HashSet<String>>,
}

impl BridgeLearning {
    /// An event from the bridge passed the forwarder: note the order it is about.
    pub(super) fn note(&self, event: &Event) {
        let coid = match event {
            Event::OrderAccepted(e) => &e.client_order_id,
            Event::OrderPartiallyFilled(e) => &e.client_order_id,
            Event::OrderFilled(e) => &e.client_order_id,
            Event::OrderCanceled(e) => &e.client_order_id,
            Event::OrderRejected(e) => &e.client_order_id,
            Event::OrderDenied(e) => &e.client_order_id,
            Event::OrderExpired(e) => &e.client_order_id,
            _ => return,
        };
        let mut announced = lock(&self.announced);
        if announced.len() < MAX_RESTING {
            announced.insert(coid.clone());
        }
    }

    /// The bridge announced `ExecLink{up:true}`.
    pub(super) fn mark_learned(&self) {
        self.learned.store(true, Ordering::SeqCst);
    }

    fn is_learned(&self) -> bool {
        self.learned.load(Ordering::SeqCst)
    }

    fn announced(&self) -> HashSet<String> {
        lock(&self.announced).clone()
    }
}

/// The ledger coids the bridge never announced: the ones it did not find at the venue (they finished while
/// the connection was down) or could not see. Sorted.
pub(super) fn unaccounted(held: &HashSet<String>, announced: &HashSet<String>) -> Vec<String> {
    let mut missing: Vec<String> = held.difference(announced).cloned().collect();
    missing.sort_unstable();
    missing
}

/// What a bridge's announcement that it learned the venue does to a hold, given the ledger coids it did not
/// account for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    /// Every ledger coid accounted for: lift.
    Lift,
    /// On a DEMO tier: lift, and say loudly which coids the bridge never mentioned; they are forgotten.
    LiftNaming(Vec<String>),
    /// On a LIVE tier: stay held over these coids (a live session rebuilds unattended; the hold is its only
    /// barrier, and an order nobody can vouch for is exactly what it exists for).
    Hold(Vec<String>),
}

pub(super) fn decide(tier: AccountTier, missing: Vec<String>, overflowed: bool) -> Decision {
    if missing.is_empty() && !overflowed {
        Decision::Lift
    } else if tier == AccountTier::Live {
        Decision::Hold(missing)
    } else {
        Decision::LiftNaming(missing)
    }
}

/// Whether `cmd` is a verb about an order (a submit, a cancel or a modify): the ones a held session refuses.
pub(super) fn carries_order(cmd: &ExecCmd) -> bool {
    matches!(
        cmd,
        ExecCmd::Submit(_)
            | ExecCmd::SubmitBatch(_)
            | ExecCmd::Cancel { .. }
            | ExecCmd::CancelBatch { .. }
            | ExecCmd::Modify { .. }
    )
}

/// Called under `inner` when an adapter is installed (a start, or the supervisor's restore). An adapter that
/// REPLACES one (`restore`, or a start after a first one) is a new bridge that is blind to every order the ledger
/// still holds, so the session is HELD; returns the words, for the log line. `None` = nothing held.
///
/// `learning`: the bridge may already have announced that it learned the venue (`ExecLink{up:true}`, which can
/// beat this install): it is then judged at once, exactly as `bridge_learned` would judge it.
pub(super) fn hold_if_blind(
    inner: &mut Inner,
    venue: &str,
    restore: bool,
    learning: &BridgeLearning,
) -> Option<String> {
    let replaces = restore || inner.had_adapter;
    inner.had_adapter = true;
    if !replaces || inner.resting.is_empty() {
        if learning.is_learned() {
            inner.held = None;
            inner.held_coids.clear();
        }
        return None;
    }
    inner.held_coids = inner.resting.snapshot();
    inner.held_overflowed = inner.resting.overflowed();
    if learning.is_learned() {
        return settle(inner, venue, learning);
    }
    let words = held_words(venue, &inner.held_coids.len().to_string(), &sample(&inner.held_coids));
    inner.held = Some(words.clone());
    Some(words)
}

/// Judge a bridge that says it learned the venue against the ledger coids the hold is waiting on: lifts the
/// hold, or leaves it (naming the coids nobody vouched for); returns the words while it stays held.
fn settle(inner: &mut Inner, venue: &str, learning: &BridgeLearning) -> Option<String> {
    let missing = unaccounted(&inner.held_coids, &learning.announced());
    match decide(inner.tier, missing, inner.held_overflowed) {
        Decision::Lift => {
            inner.held = None;
            inner.held_coids.clear();
            None
        }
        Decision::LiftNaming(named) => {
            tracing::error!(
                venue,
                unaccounted = ?named,
                overflowed = inner.held_overflowed,
                "TRADE-LINK: the rebuilt bridge learned the venue but never mentioned these orders the \
                 session had sent: they finished while the connection was down, or the bridge cannot see \
                 them. The session is RELEASED (demo tier) and forgets them; a fill among them is the \
                 reconcile to find"
            );
            for coid in &named {
                inner.resting.forget(coid);
            }
            inner.held = None;
            inner.held_coids.clear();
            None
        }
        Decision::Hold(named) => {
            let count = if inner.held_overflowed {
                format!("more than {MAX_RESTING}")
            } else {
                named.len().to_string()
            };
            let words = held_words(venue, &count, &named.join(", "));
            tracing::error!(
                venue,
                unaccounted = ?named,
                "TRADE-LINK: the rebuilt bridge learned the venue but never mentioned these orders the \
                 session had sent. This is a LIVE session: it stays HELD"
            );
            inner.held = Some(words.clone());
            Some(words)
        }
    }
}

/// Up to ten of the coids, sorted, for a log line or a reject.
fn sample(coids: &HashSet<String>) -> String {
    let mut sorted: Vec<&str> = coids.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.truncate(10);
    sorted.join(", ")
}

impl AccountSession {
    /// The bridge of the CURRENT adapter (`learning` is that adapter's cell) announced `ExecLink{up:true}`:
    /// its connection has learned the venue's open orders (rebound the ones it still lists, replayed the day's
    /// executions). The hold is JUDGED against what the bridge announced ([`settle`]): lifted when every ledger
    /// coid is accounted for, and otherwise by tier (a demo session lifts with the rest named and forgotten, a
    /// live session stays held). A bridge whose adapter has been replaced since is not the current one and
    /// releases nothing. If the learning FAILED the bridge never announces, and the hold stays: the fallback.
    pub(super) fn bridge_learned(&self, learning: &Arc<BridgeLearning>) {
        let mut inner = lock(&self.inner);
        let current = inner.adapter.as_ref().is_some_and(|a| Arc::ptr_eq(&a.learning, learning));
        if !current || inner.held.is_none() {
            return;
        }
        let still = settle(&mut inner, &self.key.0, learning);
        drop(inner);
        if still.is_none() {
            tracing::warn!(
                venue = %self.key.0,
                "TRADE-LINK: RELEASED: the rebuilt bridge reported it has learned the venue's open orders; the \
                 session takes order verbs again"
            );
        }
    }
}

/// The words a HELD session refuses every order verb with. The first sentence names the state, the second
/// is the sentence the operator acts on.
fn held_words(venue: &str, count: &str, coids: &str) -> String {
    format!(
        "gatewayhub: the {venue} venue session was rebuilt after a loss while {} order(s) were resting or \
         in flight ({}); the rebuilt adapter cannot cancel, modify or follow them, so it takes no order \
         verb: {} session rebuilt; resting orders from before the outage are not manageable until the \
         bridge learns open orders; reconcile/operator action needed. This command was NOT sent",
        count,
        coids,
        venue.to_uppercase()
    )
}

/// How long a SUPERVISED venue's bridge has, after its adapter is installed, to announce that it learned the
/// venue's open orders before the session is HELD (finding 2 of the third IBKR review: a learning that failed
/// on a first start left no hold, no line and a session that looked Live). Past the bridge's own attempts
/// (three, each bounded at 30 s, with waits of 2 s and 5 s between them), so a bridge that is merely slow is
/// not held. A bridge that announces late releases the hold when it does.
pub(super) const LEARN_BUDGET: Duration = Duration::from_secs(120);

/// The slice the learning watch sleeps in, so a retire or a stop is heard promptly.
const WATCH_SLICE: Duration = Duration::from_millis(50);

/// The words a session held because its bridge never learned the venue refuses every order verb with.
fn unlearned_words(venue: &str, secs: f64) -> String {
    format!(
        "gatewayhub: the {venue} bridge has not reported that it learned the venue's open orders {secs:.0} s \
         after it started (its learning failed or is stuck: the {venue} error lines in the journal say why), so \
         the session takes no order verb: {} session not learned; operator action needed (clear foreign or \
         stale orders in TWS, then restart the gatewayhub to rebuild the session). This command was NOT sent",
        venue.to_uppercase()
    )
}

impl AccountSession {
    /// Watch the bridge of the adapter just installed (`learning` is its cell): when it has not announced that it
    /// learned the venue within the hub's learning budget the session is HELD with [`unlearned_words`], loudly,
    /// so a failed learning is a state the tradehub sees (every order verb answered with those words) and not
    /// only an error line. The bridge announcing later lifts the hold (`bridge_learned`). The thread ends as
    /// soon as the bridge announces, the adapter is replaced or torn down, or the session or daemon stops.
    pub(super) fn watch_learning(self: &Arc<Self>, learning: Arc<BridgeLearning>) {
        let budget = Duration::from_millis(self.shared.learn_budget_ms.load(Ordering::SeqCst));
        let session = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("gwh-trade-learn".into()).spawn(move || {
            let started = Instant::now();
            loop {
                if learning.is_learned()
                    || session.retired.load(Ordering::SeqCst)
                    || session.hub_stopping()
                {
                    return;
                }
                let current = lock(&session.inner)
                    .adapter
                    .as_ref()
                    .is_some_and(|a| Arc::ptr_eq(&a.learning, &learning));
                if !current {
                    return;
                }
                let waited = started.elapsed();
                if waited >= budget {
                    break;
                }
                std::thread::sleep((budget - waited).min(WATCH_SLICE));
            }
            let mut inner = lock(&session.inner);
            let current =
                inner.adapter.as_ref().is_some_and(|a| Arc::ptr_eq(&a.learning, &learning));
            if !current || learning.is_learned() {
                return;
            }
            let words = unlearned_words(&session.key.0, started.elapsed().as_secs_f64());
            if inner.held.is_none() {
                inner.held_coids.clear();
                inner.held_overflowed = false;
                inner.held = Some(words.clone());
            }
            drop(inner);
            tracing::error!(venue = %session.key.0, "TRADE-LINK: HELD: {words}");
        });
        if let Err(e) = spawned {
            tracing::error!(
                venue = %self.key.0,
                error = %e,
                "TRADE-LINK: could not start the learning watch; a bridge that never learns the venue is not surfaced"
            );
        }
    }
}

/// What the loop drives for one session.
struct SessionLink {
    session: Arc<AccountSession>,
    watch: Arc<dyn LinkWatch>,
    /// The tier the adapter ran at; the rows must still state exactly it.
    tier: AccountTier,
    stop: Arc<AtomicBool>,
    /// How long a loss found by a probe waits for the bridge to say so itself.
    grace: Duration,
}

impl Link for SessionLink {
    fn verdict(&self) -> Verdict {
        match self.watch.tier_now() {
            Ok(tier) if tier == self.tier => Verdict::Go,
            Ok(tier) => Verdict::Stop(format!(
                "the account rows now state tier {} and the session ran at {}",
                tier.as_str(),
                self.tier.as_str()
            )),
            // `account_tier` answers this for ONE thing: the account table could not be READ (a locked or
            // unopenable store). That is neither "deactivated" nor "not there": it waits.
            Err(block @ ArmingBlock::AccountNotInStore) => Verdict::Retry(block_words(
                &self.session.key.0,
                self.session.key.1.as_deref(),
                self.tier,
                block,
            )),
            Err(block) => Verdict::Stop(block_words(
                &self.session.key.0,
                self.session.key.1.as_deref(),
                self.tier,
                block,
            )),
        }
    }

    fn reachable(&self) -> Result<(), String> {
        self.watch.reachable()
    }

    fn reported_lost(&self) -> Option<String> {
        self.session.reported_down()
    }

    fn lost(&self, why: &str) {
        self.session.link_lost(&self.stop, why, self.grace);
    }

    fn restore(&self) -> Result<(), String> {
        if self.tier == AccountTier::Live {
            // A LIVE session is rebuilt unattended, so the rows are asked once more right before the
            // start (the pass asked them at its top, and a login is slow): the account must still be active
            // at live, and the rebuild is said at warn so it is never a silent event in the journal.
            match self.verdict() {
                Verdict::Go => {}
                Verdict::Stop(why) => return Err(format!("LIVE rebuild not started: {why}")),
                Verdict::Retry(why) => {
                    return Err(format!(
                        "LIVE rebuild not started: the rows cannot be read ({why})"
                    ));
                }
            }
            tracing::warn!(
                venue = %self.session.key.0,
                account = self.session.key.1.as_deref().unwrap_or("default"),
                "TRADE-LINK: LIVE rebuild: the live venue session is being rebuilt unattended, the account \
                 is still active at live"
            );
        }
        self.session.link_restore(&self.stop)
    }

    fn gave_up(&self, why: &str) {
        self.session.link_gave_up(why);
    }
}

/// The operator's sentence for a block (`VenueArming::why`, the one `vike-backend venues` prints), or the
/// block's word for a venue outside the roster.
fn block_words(
    venue: &str,
    account: Option<&str>,
    tier: AccountTier,
    block: ArmingBlock,
) -> String {
    let label = account.and_then(|a| parse_wire_account(a).ok()).unwrap_or_default();
    vike_model::VENUES.iter().copied().find(|v| *v == venue).map_or_else(
        || block.as_str().to_string(),
        |venue| {
            VenueArming {
                venue,
                label,
                tier: Some(tier),
                effective: None,
                block,
                account_ids: Vec::new(),
            }
            .why()
        },
    )
}

/// Sleeps in slices and gives up as soon as the stop flag is up. Wakes EARLY (`true`, the wait is over)
/// when the running adapter's bridge has reported its link gone, so a loss is acted on at once instead of
/// at the next probe.
struct LinkPause {
    stop: Arc<AtomicBool>,
    session: Arc<AccountSession>,
}

impl Pause for LinkPause {
    fn pause(&self, for_: Duration) -> bool {
        let until = Instant::now() + for_;
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return false;
            }
            if self.session.reported_down().is_some() {
                return true;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(PAUSE_SLICE));
        }
    }
}

#[cfg(test)]
mod tests {
    use vike_model::events::{OrderAccepted, OrderCanceled, OrderRejected};

    use super::*;

    fn rejected(coid: &str) -> Event {
        Event::OrderRejected(OrderRejected {
            cause: None,
            client_order_id: coid.to_string(),
            reason: "no".into(),
            ts: 0,
        })
    }

    #[test]
    fn an_order_stays_in_the_ledger_until_a_terminal_event_names_it() {
        let mut resting = Resting::default();
        assert!(resting.is_empty());
        resting.sent("a", 1.0, "");
        resting.sent("b", 1.0, "");
        resting.finished(&Event::OrderAccepted(OrderAccepted {
            client_order_id: "a".into(),
            venue_order_id: None,
            ts: 0,
        }));
        assert!(!resting.is_empty(), "an accept does not finish an order");
        resting.finished(&rejected("a"));
        assert_eq!(resting.coids.len(), 1);
        resting.finished(&Event::OrderCanceled(OrderCanceled {
            client_order_id: "b".into(),
            reason: "".into(),
            ts: 0,
        }));
        assert!(resting.is_empty());
    }

    fn partial(coid: &str, trade_id: &str, qty: f64) -> Event {
        Event::OrderPartiallyFilled(vike_model::events::OrderPartiallyFilled {
            client_order_id: coid.to_string(),
            fill: vike_model::events::FillEvent {
                trade_id: vike_model::events::TradeId::new(trade_id).expect("a trade id"),
                client_order_id: coid.to_string(),
                venue: "ibkr".into(),
                symbol: "AAPL.SMART.USD".into(),
                side: 1,
                last_qty: qty,
                last_px: 190.0,
                commission: 0.0,
                commission_asset: "USD".into(),
                liquidity_side: Default::default(),
                ts: 0,
                mark_price: None,
                position_side: Default::default(),
            },
            ts: 0,
        })
    }

    /// F4 (re-review of Part B). A bridge that finds out an order finished while it was down reports the fills it
    /// missed as PARTIAL fills (it cannot know the total), so the ledger used to keep such an order for ever and
    /// hold every later rebuild of a live session. The fills that add up to the order's quantity retire it, each
    /// execution counted once, so a replay of a fill does not.
    #[test]
    fn fills_that_add_up_to_the_quantity_retire_an_order_and_a_replayed_fill_counts_once() {
        let mut resting = Resting::default();
        resting.sent("c-a", 10.0, "");
        resting.finished(&partial("c-a", "t1", 4.0));
        assert!(!resting.is_empty(), "4 of 10");
        resting.finished(&partial("c-a", "t1", 4.0));
        resting.finished(&partial("c-a", "t1", 4.0));
        assert!(!resting.is_empty(), "the replay of t1 is not another 4");
        resting.finished(&partial("c-a", "t2", 6.0));
        assert!(resting.is_empty(), "4 + 6 = 10: the order is done");
    }

    #[test]
    fn an_order_of_unknown_quantity_is_retired_only_by_a_terminal_event() {
        let mut resting = Resting::default();
        resting.sent("c-a", 0.0, "");
        resting.finished(&partial("c-a", "t1", 4.0));
        resting.finished(&partial("c-a", "t2", 400.0));
        assert!(!resting.is_empty());
        resting.finished(&rejected("c-a"));
        assert!(resting.is_empty());
    }

    #[test]
    fn fills_of_an_order_the_ledger_never_sent_change_nothing() {
        let mut resting = Resting::default();
        resting.finished(&partial("c-x", "t1", 4.0));
        assert!(resting.is_empty());
    }

    #[test]
    fn a_ledger_that_overflowed_is_never_empty() {
        let mut resting = Resting::default();
        for n in 0..=MAX_RESTING {
            resting.sent(&format!("c-{n}"), 1.0, "");
        }
        for n in 0..=MAX_RESTING {
            resting.finished(&rejected(&format!("c-{n}")));
        }
        assert!(!resting.is_empty(), "past the bound the ledger only remembers that it overflowed");
        assert!(resting.overflowed());
    }

    fn set(coids: &[&str]) -> HashSet<String> {
        coids.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn the_unaccounted_are_the_ledger_coids_the_bridge_never_announced_sorted() {
        let held = set(&["c-b", "c-a", "c-c"]);
        assert_eq!(unaccounted(&held, &set(&["c-b", "x"])), ["c-a", "c-c"]);
        assert!(unaccounted(&held, &set(&["c-a", "c-b", "c-c"])).is_empty());
        assert_eq!(unaccounted(&held, &set(&[])), ["c-a", "c-b", "c-c"]);
    }

    #[test]
    fn a_bridge_that_accounted_for_everything_lifts_on_any_tier() {
        for tier in [AccountTier::Demo, AccountTier::Live] {
            assert_eq!(decide(tier, vec![], false), Decision::Lift);
        }
    }

    /// R1: the tier decides what an unaccounted order means. A live session rebuilds unattended and the hold is
    /// its only barrier.
    #[test]
    fn an_unaccounted_order_lifts_a_demo_session_with_the_coids_named_and_holds_a_live_one() {
        let missing = vec!["c-gone".to_string()];
        assert_eq!(
            decide(AccountTier::Demo, missing.clone(), false),
            Decision::LiftNaming(missing.clone())
        );
        assert_eq!(decide(AccountTier::Live, missing.clone(), false), Decision::Hold(missing));
    }

    /// A ledger that overflowed cannot be accounted for, whatever the bridge announced.
    #[test]
    fn an_overflowed_ledger_is_never_accounted_for() {
        assert_eq!(decide(AccountTier::Demo, vec![], true), Decision::LiftNaming(vec![]));
        assert_eq!(decide(AccountTier::Live, vec![], true), Decision::Hold(vec![]));
    }

    #[test]
    fn what_the_bridge_announces_is_noted_per_order() {
        let learning = BridgeLearning::default();
        learning.note(&Event::OrderAccepted(OrderAccepted {
            client_order_id: "c-a".into(),
            venue_order_id: None,
            ts: 0,
        }));
        learning.note(&rejected("c-b"));
        learning.note(&Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: "c-ignored".into(),
            ts: 0,
        }));
        assert_eq!(learning.announced(), set(&["c-a", "c-b"]));
        assert!(!learning.is_learned());
        learning.mark_learned();
        assert!(learning.is_learned());
    }

    /// Finding 1: the ledger names the symbol each order was sent under: what a rebuilt bridge is taught before
    /// it replays the day's executions.
    #[test]
    fn the_ledger_names_the_symbol_each_order_was_sent_under() {
        let pair = |c: &str, s: &str| (c.to_string(), s.to_string());
        let mut resting = Resting::default();
        resting.sent("c-b", 1.0, "EUR.USD.IDEALPRO");
        resting.sent("c-a", 1.0, "AAPL.SMART.USD");
        resting.sent("c-none", 1.0, "");
        assert_eq!(
            resting.symbols(),
            [pair("c-a", "AAPL.SMART.USD"), pair("c-b", "EUR.USD.IDEALPRO")],
            "sorted, and an order sent with no symbol is not taught"
        );
        resting.finished(&rejected("c-a"));
        assert_eq!(
            resting.symbols(),
            [pair("c-b", "EUR.USD.IDEALPRO")],
            "a finished order leaves with its symbol"
        );
    }

    #[test]
    fn the_unlearned_words_are_one_line_and_say_what_to_do() {
        let words = unlearned_words("ibkr", 120.4);
        assert!(!words.contains('\n') && !words.contains("  ") && !words.contains('\\'), "{words}");
        assert!(words.contains("120 s") && words.contains("IBKR session not learned"), "{words}");
        assert!(words.contains("restart the gatewayhub") && words.contains("NOT sent"), "{words}");
    }
}
