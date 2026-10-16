//! The lease link: [`TradeLink`] (the builder), [`TradeSession`] (the handle) and the two threads under
//! them.
//!
//! # Threads
//!
//! * **`gw-trade-link`** (one per lease) owns the write half of the socket, the [`Ledger`] and the
//!   reconnect schedule. It is a state machine over its inbox: commands from the fold thread, requests
//!   from the boot and reconcile threads, and what its helpers report (an acknowledgement, a closed
//!   reader, a finished dial). It NEVER blocks on the network except in a `write_all` bounded by the
//!   socket's write timeout, and it never dials itself: a reconnect runs on a short-lived
//!   **`gw-trade-dial`** thread, so the inbox keeps being drained (an order submitted during a redial
//!   is rejected AT ONCE with the NEVER-SENT words, not after the dial's ten seconds).
//! * **`gw-trade-read`** (one per connection) reads [`TradeDown`] frames under the heartbeat deadline,
//!   relays events to the core with their ring cursor (`EventSender::blocking_send_gateway`) and routes
//!   answers to their waiters. A slow core back-pressures THIS reader through TCP into the gatewayhub's
//!   per-account ring; it never touches another account's lease.
//!
//! # What a reconnect does, in order
//!
//! `TradeOpen{resume}` (the in-memory cursor) is answered; the ledger resolves every Written frame
//! against the answer (`crate::trade::ledger`); the UNKNOWN submits of an epoch change go to
//! `ResolveOrders`; the held frames are flushed (after an EPOCH CHANGE: when the daemon answers the next
//! `AdapterStart`, since a restarted daemon has no adapter to hand them to); the link is announced `Live`
//! ([`LinkSignal::link_restored`]). A reconnect never takes the lease over: `LeaseHeld`, or a refusal that
//! repeats `REFUSED_REDIAL_ATTEMPTS` times (one may be momentary: the account table read), on
//! a redial closes the lease for good (the holder is another client; fighting it would be two writers).
//!
//! # A closed lease
//!
//! A lease closed for good (taken over, refused, the core gone, a stop) keeps its thread until
//! [`TradeSession::detach`] / drop: in that mode every frame is disposed with the NEVER-SENT words and
//! every request is failed, so no `send` ever finds a dropped receiver between a check and a drop and no
//! order can vanish in that window.

use std::io::{self, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vike_exec::{EventSender, GatewayCursor};
use vike_hub_proto::auth::Scope;
use vike_market_stream::frame::{is_timeout, read_capped};
use vike_model::events::Event;

use super::disclosure::{LinkSignal, NoSignal};
use super::heartbeat::{ping_due_in, read_deadline};
use super::ledger::{Fate, Ledger, Why, screen_finite, set_cseq};
use super::shared::{
    Flow, LinkState, Msg, Reply, Shared, TradeAccount, TradeError, TradeLinkStats, TradeTimings,
    lock,
};
use crate::client::{GatewayClient, GatewayLink, TradeOpenArgs, TradeOpenError, TradeStream};
use crate::proto::{
    AdapterReply, AdapterStart, DetachPhase, FEATURE_GW_ADAPTER, FEATURE_GW_TRADE_RESOLVE,
    FEATURE_GW_TRADE_WIRE_IDS, GW_DOWN_FRAME_CEILING_BYTES, GW_TRADE_UP_FRAME_CEILING_BYTES,
    OrderResolution, TradeBye, TradeDown, TradeOpened, TradeStatusReport, TradeUp,
};

/// How long a stopping link waits for the daemon's goodbye after its final `Detach`.
const GOODBYE_WAIT: Duration = Duration::from_millis(1_000);

/// How long a stopping link waits for the daemon to ACKNOWLEDGE the frames it wrote (the cancel sweep
/// among them) before it says its final `Detach`. Bounded: a daemon that never answers is not waited
/// for, and what is still unacknowledged then is named in a warning.
const ACK_DRAIN: Duration = Duration::from_millis(2_000);

/// **The most one lease's stop adds to a teardown**: the ack drain plus the goodbye wait. Every lease runs
/// them on its own thread from phase one on, so N leases cost this ONCE, not N times (the tradehub's
/// `graceful_stop` arithmetic reads it: `docs/ops/graceful-stop.md`).
pub const STOP_BOUND: Duration = ACK_DRAIN.saturating_add(GOODBYE_WAIT);

/// How many connections' epochs a lease remembers ([`Writer::answer_counts`]): a reply is read at most a few
/// connections after the one it was asked on.
const CONN_EPOCHS_KEPT: usize = 8;

/// How many consecutive `TradeRefused` answers a redial takes before the refusal closes the lease for good.
const REFUSED_REDIAL_ATTEMPTS: u32 = 3;

/// The builder of a lease. Everything has a sensible default except the three things a caller must
/// say: where the gatewayhub is (a [`GatewayLink`], which carries the Trade key as a parameter - this
/// crate reads no store), which account (a [`TradeAccount`]) and the core's lane for it.
pub struct TradeLink {
    link: GatewayLink,
    account: TradeAccount,
    events: EventSender,
    timings: TradeTimings,
    halt_path: Option<PathBuf>,
    trigger: Option<Sender<()>>,
    signal: Arc<dyn LinkSignal>,
    resume: Option<GatewayCursor>,
    takeover: bool,
}

impl TradeLink {
    /// A lease for `account` over `link`, delivering to `events`.
    ///
    /// `events` must be the core's lane for THIS account already scoped with `EventSender::routed`
    /// (the mount's job): the synthesized events and the relay both go through it, and its route-key
    /// stamp is what folds an account-wide payload into the right engine.
    pub fn new(link: GatewayLink, account: TradeAccount, events: EventSender) -> Self {
        Self {
            link,
            account,
            events,
            timings: TradeTimings::default(),
            halt_path: None,
            trigger: None,
            signal: Arc::new(NoSignal),
            resume: None,
            takeover: false,
        }
    }

    /// Other waits; see [`TradeTimings`] (raise-only in production).
    #[must_use]
    pub fn with_timings(mut self, timings: TradeTimings) -> Self {
        self.timings = timings;
        self
    }

    /// The HALT sentinel's path (`vike_boot::halt`'s resolution, handed in by the root: this crate
    /// resolves no process-wide path). `None` watches no file, which says so once at `error`
    /// (`vike_exec::halt::sentinel_engaged`).
    #[must_use]
    pub fn with_halt_path(mut self, path: PathBuf) -> Self {
        self.halt_path = Some(path);
        self
    }

    /// The reconcile trigger the daemon's `ReconcileNow` and `ReplayUnavailable` frames feed (the
    /// `recon_trigger: Sender<()>` a venue whose declaration `takes_recon_trigger` is handed).
    #[must_use]
    pub fn with_trigger(mut self, trigger: Sender<()>) -> Self {
        self.trigger = Some(trigger);
        self
    }

    /// What the link announces about itself; see [`LinkSignal`]. Default: nothing.
    #[must_use]
    pub fn with_signal(mut self, signal: Arc<dyn LinkSignal>) -> Self {
        self.signal = signal;
        self
    }

    /// Resume the event ring from `cursor` (the journal's, after a restart; the boot restore's
    /// `BootRestore::gateway_cursors()` for this account's route key). `None` opens fresh and the
    /// caller reconciles.
    #[must_use]
    pub fn with_resume(mut self, cursor: Option<GatewayCursor>) -> Self {
        self.resume = cursor;
        self
    }

    /// Take the lease over from its holder (explicit and fenced) instead of being refused with
    /// `LeaseHeld`. Only the FIRST open ever takes over; a redial never does.
    #[must_use]
    pub fn with_takeover(mut self, takeover: bool) -> Self {
        self.takeover = takeover;
        self
    }

    /// Dial, authenticate under `Scope::Trade`, open the lease and start the link. Blocks for the dial
    /// and the open reply (bounded), so the composition root learns `LeaseHeld`, a tier refusal or a
    /// dead gatewayhub HERE, before it builds an engine.
    pub fn open(self) -> Result<TradeSession, TradeOpenError> {
        let args = open_args(&self.account, self.resume, self.takeover);
        let stream = GatewayClient::connect(&self.link, Scope::Trade)?.open_trade(&args)?;
        let (shared, rx) = Shared::new(
            self.link,
            self.account,
            self.timings,
            self.halt_path,
            self.events,
            self.trigger,
            self.signal,
            self.resume,
        );
        let mut writer = Writer::new(Arc::clone(&shared), rx);
        writer.attach(stream, true);
        let join = thread::Builder::new()
            .name("gw-trade-link".to_string())
            .spawn(move || writer.run())
            .map_err(TradeOpenError::Link)?;
        Ok(TradeSession(Arc::new(Inner { shared, join: Mutex::new(Some(join)) })))
    }
}

fn open_args(
    account: &TradeAccount,
    resume: Option<GatewayCursor>,
    takeover: bool,
) -> TradeOpenArgs {
    TradeOpenArgs {
        client_id: account.client_id.clone(),
        venue: account.venue.clone(),
        account: account.label.clone(),
        tier: account.tier,
        resume: resume.map(Into::into),
        takeover,
    }
}

/// The owner of the link thread: when the last handle goes, the link flushes, says goodbye and ends.
pub(crate) struct Inner {
    pub(crate) shared: Arc<Shared>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl Inner {
    /// Ask the link thread to flush and exit, and wait up to `bound` for it. Idempotent.
    pub(crate) fn stop(&self, bound: Duration) {
        self.shared.mark_stopping();
        // A closed channel means the thread is already gone.
        let _ = self.shared.tx.send(Msg::Stop);
        let Some(handle) = lock(&self.join).take() else { return };
        let deadline = Instant::now() + bound;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                tracing::warn!(
                    "gatewayhub trade link did not finish within {bound:?}; leaving it to wind down"
                );
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        // A join error is a panic in an already-dead thread: nothing to recover at teardown.
        let _ = handle.join();
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop(self.shared.timings.detach_flush);
    }
}

/// An open lease. Cloning is cheap and shares it; the link ends when the last clone (and every client
/// built from it) is dropped, or at [`TradeSession::detach`].
#[derive(Clone)]
pub struct TradeSession(pub(crate) Arc<Inner>);

impl std::fmt::Debug for TradeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TradeSession").field("shared", &self.0.shared).finish()
    }
}

impl TradeSession {
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.0.shared
    }

    /// The execution client the core drives: HALT-checked, non-blocking, NEVER-SENT-aware.
    pub fn exec_client(&self) -> super::exec::RemoteExecClient {
        super::exec::RemoteExecClient::new(self.clone())
    }

    /// The reconcile client the `ReconManager` drives: blocking, per-request idle bound, never an empty
    /// answer for a fetch that did not happen.
    pub fn recon_client(&self) -> super::recon::RemoteReconClient {
        super::recon::RemoteReconClient::new(self.clone())
    }

    /// Whether the daemon of this lease advertised [`crate::proto::FEATURE_GW_TRADE_CONTRACT_VALUE`]: it
    /// sends `AdapterReply::Live::contract_value`. Without it that field reads `None` whatever the
    /// bridge knew, so a venue whose contract is not `Linear` 1.0 must not be run on this daemon
    /// (decision 0170, item 6).
    pub fn reports_contract_value(&self) -> bool {
        self.0.shared.has_feature(crate::proto::FEATURE_GW_TRADE_CONTRACT_VALUE)
    }

    /// The open reply of the CURRENT connection.
    pub fn opened(&self) -> Option<TradeOpened> {
        self.0.shared.opened()
    }

    /// Whether the link is up, down (redialling) or closed for good.
    pub fn link_state(&self) -> LinkState {
        self.0.shared.link_state()
    }

    /// Whether a teardown of this lease has begun (`begin_detach`, `detach`, the last handle dropped): a
    /// caller that would start something new on it (the tradehub's re-teaching worker) must not.
    pub fn is_stopping(&self) -> bool {
        self.0.shared.is_stopping()
    }

    /// Why the lease is closed for good, if it is.
    pub fn closed_reason(&self) -> Option<String> {
        self.0.shared.closed_reason()
    }

    /// The last ring position delivered to the core (what a redial resumes from).
    pub fn cursor(&self) -> Option<GatewayCursor> {
        self.0.shared.cursor()
    }

    /// What the readers did.
    pub fn stats(&self) -> TradeLinkStats {
        self.0.shared.stats()
    }

    /// The submits that stay UNKNOWN: Written across an epoch change, not yet spoken about by the venue.
    pub fn unknown_orders(&self) -> Vec<String> {
        self.0.shared.unknown_orders()
    }

    /// What the daemon said about an UNKNOWN order (`ResolveOrders`' answer).
    pub fn resolution_of(&self, coid: &str) -> Option<crate::proto::ResolveState> {
        self.0.shared.resolution_of(coid)
    }

    /// The account this lease is for.
    pub fn account(&self) -> &TradeAccount {
        &self.0.shared.account
    }

    /// One request, one answer frame.
    fn single(
        &self,
        feature: Option<&'static str>,
        build: impl FnOnce(u64) -> TradeUp,
    ) -> Result<TradeDown, TradeError> {
        let shared = &self.0.shared;
        if let Some(feature) = feature {
            shared.require(feature)?;
        }
        let rid = shared.next_rid();
        let rx = shared.ask(rid, build(rid))?;
        let bound = shared.timings.request_reply;
        match rx.recv_timeout(bound) {
            Ok(Reply::Frame(frame)) => Ok(*frame),
            Ok(Reply::Down) => Err(TradeError::LinkDown(shared.unavailable_text())),
            Ok(Reply::Lost(why)) => Err(TradeError::LinkLost(format!(
                "gatewayhub {}: the trade link was lost before the answer ({why})",
                shared.link.addr()
            ))),
            Err(RecvTimeoutError::Timeout) => {
                shared.forget(rid);
                Err(TradeError::Timeout(format!(
                    "gatewayhub {}: no answer within {bound:?}",
                    shared.link.addr()
                )))
            }
            Err(RecvTimeoutError::Disconnected) => Err(TradeError::LinkLost(format!(
                "gatewayhub {}: the trade link ended before the answer",
                shared.link.addr()
            ))),
        }
    }

    fn unexpected<T>(&self, wanted: &str, got: &TradeDown) -> Result<T, TradeError> {
        Err(TradeError::Protocol(format!(
            "gatewayhub {}: expected {wanted}, got {got:?}",
            self.0.shared.link.addr()
        )))
    }

    /// Start the venue adapter of this lease. `start.rid` is assigned here and `start.route_key` is the
    /// account's (one spelling). The answer is the bridge's own: `Live` with its grids, or `NotRunning`
    /// with the refusal text byte for byte.
    pub fn start_adapter(&self, mut start: AdapterStart) -> Result<AdapterReply, TradeError> {
        start.route_key.clone_from(&self.0.shared.account.route_key);
        let answer = self.single(Some(FEATURE_GW_ADAPTER), |rid| {
            start.rid = rid;
            TradeUp::AdapterStart(start)
        })?;
        match answer {
            TradeDown::AdapterReply { reply, .. } => Ok(*reply),
            other => self.unexpected("AdapterReply", &other),
        }
    }

    /// **`start_adapter` for a caller that re-teaches the restarted adapter itself** (the tradehub's
    /// gatewayhub-restart worker). After an epoch change the risk-off cancels held across the restart are
    /// NOT released by the answer, only the held teaching is: the caller queues the teaching of every order
    /// minted since boot (`ExecutionClient::adopt_restored`) and then calls [`TradeSession::release_held`]
    /// (or [`TradeSession::release_held_anyway`] on a way out that never gets that far), so the cancels
    /// reach the daemon BEHIND the teaching they need (hyperliquid, polymarket, ig and ctrader fail a cancel of
    /// an order the restart made them forget with "no venue order id"). ⚠ A caller that uses this and
    /// never releases strands the cancels until the lease closes; one that has nothing to teach calls
    /// [`TradeSession::start_adapter`].
    pub fn restart_adapter(&self, start: AdapterStart) -> Result<AdapterReply, TradeError> {
        self.0.shared.hold_next_answer();
        self.start_adapter(start)
    }

    /// **The boot prefetch behind `wire_id_for`.** Call it on the boot thread, BEFORE the core is
    /// spawned, with every restored coid. Until it has succeeded a reconcile is not attempted (the
    /// reconcile client answers "not ready"). A failure leaves the prefetch owed: the reconcile client
    /// retries it, and the caller may call this again.
    pub fn prefetch_wire_ids(&self, coids: &[String]) -> Result<(), TradeError> {
        want_wire_ids(&self.0.shared, coids);
        fill_wire_ids(self)
    }

    /// Ask what the daemon knows of `coids` after an epoch change (query-only: a Written frame is never
    /// re-sent). The answer is recorded in [`TradeSession::resolution_of`] as well.
    pub fn resolve_orders(&self, coids: &[String]) -> Result<Vec<OrderResolution>, TradeError> {
        let answer = self.single(Some(FEATURE_GW_TRADE_RESOLVE), |rid| TradeUp::ResolveOrders {
            rid,
            coids: coids.to_vec(),
        })?;
        match answer {
            TradeDown::OrdersResolved { results, .. } => {
                self.0.shared.record_resolutions(results.clone());
                Ok(results)
            }
            other => self.unexpected("OrdersResolved", &other),
        }
    }

    /// Record the venue-confirmed account identity at the daemon (bookkeeping; the caller logs a
    /// failure as a warning, it never stops a start).
    pub fn record_identity(&self) -> Result<(), TradeError> {
        let answer = self.single(None, |rid| TradeUp::IdentityRecord { rid })?;
        match answer {
            TradeDown::IdentityRecorded { ok: true, .. } => Ok(()),
            TradeDown::IdentityRecorded { ok: false, note, .. } => {
                Err(TradeError::Protocol(format!(
                    "gatewayhub {}: the identity was not recorded: {note}",
                    self.0.shared.link.addr()
                )))
            }
            other => self.unexpected("IdentityRecorded", &other),
        }
    }

    /// The daemon's own account of the lease.
    pub fn status(&self) -> Result<TradeStatusReport, TradeError> {
        let answer = self.single(None, |rid| TradeUp::TradeStatus { rid })?;
        match answer {
            TradeDown::TradeStatusIs { status, .. } => Ok(status),
            other => self.unexpected("TradeStatusIs", &other),
        }
    }

    /// **After a gatewayhub restart: the caller has taught the new adapter what it means to teach.** The
    /// link released the boot teaching when the daemon answered the `AdapterStart` that [`TradeSession::restart_adapter`]
    /// sent; the risk-off cancels held across the restart wait for THIS call, so they go out behind the teaching the caller queued
    /// before it (the orders minted since boot) and not ahead of it. Call it on every way out of the
    /// re-start (a refusal the daemon ANSWERED included). A no-op when nothing is held, and nothing
    /// while a newer restart's adapter has not answered.
    pub fn release_held(&self) {
        let _ = self.0.shared.tx.send(Msg::ReleaseHeld { anyway: false });
    }

    /// [`TradeSession::release_held`] for a caller that GIVES UP on the adapter ever answering (a refusal
    /// asking again cannot change, or it has not started in the time a held cancel may wait): the held
    /// cancels go out whatever the adapter's state and are answered, an advisory each, rather than left
    /// waiting for ever.
    pub fn release_held_anyway(&self) {
        let _ = self.0.shared.tx.send(Msg::ReleaseHeld { anyway: true });
    }

    /// Phase one of a teardown: raise the stop and return at once (the twin of
    /// `ExecutionClient::begin_detach`; idempotent with [`TradeSession::detach`]).
    pub fn begin_detach(&self) {
        self.0.shared.mark_stopping();
        let _ = self.0.shared.tx.send(Msg::Detach(DetachPhase::Begin));
    }

    /// Phase two: flush what is queued, say goodbye, end the link thread, wait up to
    /// [`TradeTimings::detach_flush`]. Idempotent.
    pub fn detach(&self) {
        self.0.stop(self.0.shared.timings.detach_flush);
    }
}

fn want_wire_ids(shared: &Shared, coids: &[String]) {
    shared.wire_ids.write().unwrap_or_else(std::sync::PoisonError::into_inner).want(coids);
}

/// How many coids one `WireIds` frame carries: half the daemon's `MAX_FRAME_ITEMS` (8192), and about
/// 150 KiB of the lease's 256 KiB up-frame ceiling at 32 characters a coid.
const WIRE_IDS_PER_REQUEST: usize = 4096;

/// Pay the owed prefetch: `WireIds` requests (paged) for the wanted coids, then fill the cache. Called by
/// the boot thread and, if that failed, by the reconcile client before its first fetch.
pub(crate) fn fill_wire_ids(session: &TradeSession) -> Result<(), TradeError> {
    let shared = session.shared();
    let wanted: Vec<String> =
        shared.wire_ids.read().unwrap_or_else(std::sync::PoisonError::into_inner).wanted().to_vec();
    // PAGED: a frame carries at most `WIRE_IDS_PER_REQUEST` coids (the daemon bounds a frame, and a
    // restored set is the ownership file's cap plus the journal's resting orders). Nothing is cut: the
    // cache is filled only when EVERY page was answered, so one failed page leaves the whole prefetch
    // owed and the reconcile NOT READY, never a half-translated map.
    let mut pairs = Vec::new();
    for page in wanted.chunks(WIRE_IDS_PER_REQUEST) {
        let answer = session.single(Some(FEATURE_GW_TRADE_WIRE_IDS), |rid| TradeUp::WireIds {
            rid,
            coids: page.to_vec(),
        })?;
        match answer {
            TradeDown::WireIdsReply { pairs: page_pairs, .. } => pairs.extend(page_pairs),
            other => return session.unexpected("WireIdsReply", &other),
        }
    }
    shared.wire_ids.write().unwrap_or_else(std::sync::PoisonError::into_inner).filled(pairs);
    Ok(())
}

// ---- the link thread ------------------------------------------------------------------------------

/// The write half of one connection and what the thread needs to keep it alive.
struct Conn {
    id: u64,
    stream: TcpStream,
    heartbeat: Duration,
    last_write: Instant,
}

/// Why a frame was not written.
enum WriteError {
    /// The frame exceeds the up ceiling: nothing was written.
    TooLarge,
    /// The frame could not be serialized: nothing was written.
    Encode(String),
    /// The socket failed: at most part of the frame left, which the daemon cannot decode.
    Io(io::Error),
}

struct Writer {
    shared: Arc<Shared>,
    rx: mpsc::Receiver<Msg>,
    ledger: Ledger,
    conn: Option<Conn>,
    next_conn_id: u64,
    prev_epoch: Option<u64>,
    /// The `gw_epoch` each recent connection was opened in, newest last (bounded by [`CONN_EPOCHS_KEPT`]): a
    /// reply read off a connection since lost still belongs to its daemon process (see [`Writer::answer_counts`]).
    conn_epochs: Vec<(u64, u64)>,
    backoff: Duration,
    next_dial: Option<Instant>,
    dialing: bool,
    dial_failures: u64,
    /// Consecutive `TradeRefused` answers to a redial (a refusal can be momentary).
    refused_redials: u32,
    /// Closed for good: dispose everything, dial nothing, exit only on `Stop`.
    closed: bool,
    done: bool,
}

impl Writer {
    fn new(shared: Arc<Shared>, rx: mpsc::Receiver<Msg>) -> Self {
        let backoff = shared.link.timings().reconnect_base;
        Self {
            shared,
            rx,
            ledger: Ledger::default(),
            conn: None,
            next_conn_id: 0,
            prev_epoch: None,
            conn_epochs: Vec::new(),
            backoff,
            next_dial: None,
            dialing: false,
            dial_failures: 0,
            refused_redials: 0,
            closed: false,
            done: false,
        }
    }

    fn run(mut self) {
        while !self.done {
            let wait = self.next_wait();
            match self.rx.recv_timeout(wait) {
                Ok(msg) => self.on_msg(msg),
                Err(RecvTimeoutError::Timeout) => self.on_tick(),
                // Every sender is gone: nobody can ask anything of this lease any more.
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.finish();
    }

    fn next_wait(&self) -> Duration {
        let now = Instant::now();
        let wait = match (&self.conn, self.closed) {
            (_, true) => Duration::from_secs(1),
            (Some(conn), false) => ping_due_in(conn.last_write, conn.heartbeat, now),
            (None, false) if self.dialing => Duration::from_millis(250),
            (None, false) => self
                .next_dial
                .map_or(Duration::from_millis(50), |t| t.saturating_duration_since(now)),
        };
        wait.max(Duration::from_millis(1))
    }

    fn on_tick(&mut self) {
        if self.closed {
            return;
        }
        let now = Instant::now();
        if let Some(conn) = &self.conn {
            if ping_due_in(conn.last_write, conn.heartbeat, now).is_zero() {
                self.send_control(&TradeUp::Ping);
            }
        } else if !self.dialing && self.next_dial.is_none_or(|t| now >= t) {
            self.start_dial();
        }
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Frame(frame) => self.on_frame(*frame),
            Msg::Ask(frame) => self.on_ask(*frame),
            Msg::Acked { conn, cseq } => {
                if self.conn.as_ref().is_some_and(|c| c.id == conn) {
                    self.ledger.acked(cseq);
                }
            }
            Msg::AdapterAnswered { conn, hold } => {
                if self.answer_counts(conn) {
                    for frame in self.ledger.adapter_answered(hold) {
                        self.on_frame(frame);
                    }
                }
            }
            Msg::ReleaseHeld { anyway } => {
                // With the link down the frames go straight back into the ledger (`on_frame` holds
                // them), to be flushed at the reconnect: the gate is lifted either way.
                let released = if anyway {
                    self.ledger.release_held_anyway()
                } else {
                    self.ledger.release_held()
                };
                for frame in released {
                    self.on_frame(frame);
                }
            }
            Msg::ReaderClosed { conn, why } => {
                if self.conn.as_ref().is_some_and(|c| c.id == conn) {
                    if self.shared.core_gone() {
                        self.close_for_good("the core has exited", Fate::Unknown);
                    } else {
                        self.lost(&why);
                    }
                }
            }
            Msg::Bye { conn, bye } => {
                if self.conn.as_ref().is_some_and(|c| c.id == conn) {
                    self.on_bye(bye);
                }
            }
            Msg::Dialed(result) => self.on_dialed(result),
            Msg::Detach(phase) => {
                if !self.closed {
                    self.send_control(&TradeUp::Detach { phase });
                }
                // Phase one of a teardown finishes the lease HERE, on this lease's own thread, and
                // returns to the caller at once: the core raises phase one on every engine before it
                // joins any, so N leases wind down in PARALLEL (N goodbyes inside one wait, not one
                // after another) and the later `detach` finds this link already over.
                if phase == DetachPhase::Begin {
                    self.finish_stop();
                }
            }
            Msg::Stop => {
                self.finish_stop();
                self.done = true;
            }
        }
    }

    // ---- frames ------------------------------------------------------------------------------------

    /// A command frame from the fold thread (or the boot thread's teaching).
    fn on_frame(&mut self, frame: TradeUp) {
        // A number that cannot cross is refused HERE, before any state, whatever the link is doing.
        let (frame, refused) = screen_finite(frame);
        self.emit_all(refused);
        let Some(frame) = frame else { return };
        if self.conn.is_none() {
            let why = if self.closed { Why::Closed } else { Why::NeverSent };
            let events = self.ledger.unsent(frame, why);
            self.emit_all(events);
            return;
        }
        // After an epoch change a cancel that may not be shed queues behind the teaching.
        let Some(mut frame) = self.ledger.defer(frame) else { return };
        let n = self.ledger.next_cseq();
        set_cseq(&mut frame, n);
        // Remembered BEFORE the write (the answer can be read the instant the daemon has the frame), and
        // forgotten again below if the frame did not leave: see `Shared::note_cancel_written`.
        let noted = self.shared.note_cancel_written(&frame);
        match self.write(&frame) {
            Ok(()) => self.ledger.wrote(frame),
            Err(WriteError::TooLarge) => {
                self.shared.forget_cancel_written(&noted);
                self.too_large(frame);
            }
            Err(WriteError::Encode(why)) => {
                self.shared.forget_cancel_written(&noted);
                tracing::error!(
                    why,
                    "gatewayhub trade: a frame could not be serialized and was not sent"
                );
                let events = self.ledger.unsent(frame, Why::NeverSent);
                self.emit_all(events);
            }
            Err(WriteError::Io(e)) => {
                self.shared.forget_cancel_written(&noted);
                // At most a PART of the frame left: the daemon cannot decode it, so it is NEVER-SENT.
                let events = self.ledger.unsent(frame, Why::NeverSent);
                self.emit_all(events);
                self.lost(&format!("write failed: {e}"));
            }
        }
    }

    /// A frame over the up ceiling: halve a batch, reject a single frame that can never fit.
    fn too_large(&mut self, mut frame: TradeUp) {
        match split_off_tail(&mut frame) {
            Some(tail) => {
                self.on_frame(frame);
                self.on_frame(tail);
            }
            None => {
                tracing::error!(
                    ceiling = GW_TRADE_UP_FRAME_CEILING_BYTES,
                    "gatewayhub trade: a single frame exceeds the lease's up ceiling and was not sent"
                );
                let events = self.ledger.unsent(frame, Why::TooLarge);
                self.emit_all(events);
            }
        }
    }

    /// A request frame (it has a `rid` and a waiter, no `cseq`, no order to lose).
    fn on_ask(&mut self, frame: TradeUp) {
        let Some(rid) = rid_of(&frame) else {
            tracing::error!("gatewayhub trade: a frame with no request id was asked; dropped");
            return;
        };
        if self.conn.is_none() {
            self.shared.fail_unsent(rid);
            return;
        }
        match self.write(&frame) {
            Ok(()) => {}
            Err(WriteError::TooLarge) => self.shared.fail_unsent(rid),
            Err(WriteError::Encode(why)) => {
                tracing::error!(why, "gatewayhub trade: a request could not be serialized");
                self.shared.fail_unsent(rid);
            }
            Err(WriteError::Io(e)) => {
                let why = format!("write failed: {e}");
                self.shared.fail_lost(rid, &why);
                self.lost(&why);
            }
        }
    }

    /// A ping or a detach: no waiter, no ledger. A failed write is a lost link.
    fn send_control(&mut self, frame: &TradeUp) {
        if let Err(e) = self.write(frame) {
            match e {
                WriteError::Io(e) => self.lost(&format!("write failed: {e}")),
                WriteError::TooLarge | WriteError::Encode(_) => {
                    tracing::error!("gatewayhub trade: a control frame could not be encoded");
                }
            }
        }
    }

    /// ONE `write_all` of the whole frame. When it returns `Ok` the frame is Written (UNKNOWN until
    /// acknowledged); when it does not, the frame is NEVER-SENT.
    fn write(&mut self, frame: &TradeUp) -> Result<(), WriteError> {
        let Some(conn) = self.conn.as_mut() else {
            return Err(WriteError::Io(io::Error::from(io::ErrorKind::NotConnected)));
        };
        let body = serde_json::to_vec(frame).map_err(|e| WriteError::Encode(e.to_string()))?;
        if body.len() > GW_TRADE_UP_FRAME_CEILING_BYTES as usize {
            return Err(WriteError::TooLarge);
        }
        let prefix = u32::try_from(body.len())
            .map_err(|_| WriteError::Encode("frame length exceeds u32".to_string()))?;
        let mut buf = Vec::with_capacity(4 + body.len());
        buf.extend_from_slice(&prefix.to_be_bytes());
        buf.extend_from_slice(&body);
        conn.stream.write_all(&buf).and_then(|()| conn.stream.flush()).map_err(WriteError::Io)?;
        conn.last_write = Instant::now();
        Ok(())
    }

    fn emit_all(&self, events: Vec<Event>) {
        for event in events {
            // `CoreGone` = the core has exited = shutting down; there is nobody left to tell.
            let _ = self.shared.events.blocking_send(event);
        }
    }

    // ---- the connection ----------------------------------------------------------------------------

    /// Take over a freshly opened lease: start its reader, resolve what the previous connection left
    /// in flight, flush the held frames, announce the link.
    fn attach(&mut self, opened: TradeStream, first: bool) {
        let TradeStream { stream, opened, heartbeat, features } = opened;
        let reader_stream = match stream.try_clone() {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "gatewayhub trade: cannot clone the lease socket for its reader");
                self.schedule_dial();
                return;
            }
        };
        let id = self.next_conn_id;
        self.next_conn_id += 1;
        self.conn_epochs.push((id, opened.gw_epoch));
        if self.conn_epochs.len() > CONN_EPOCHS_KEPT {
            self.conn_epochs.remove(0);
        }
        let deadline = read_deadline(self.shared.timings.read_floor, heartbeat);
        // The reader is spawned CLOSED (W2G-5): it reads nothing until `start_reader` below, after the
        // coids this connection leaves UNKNOWN are marked. A daemon that rebuilt the session itself has the
        // answer in the ring already and replays it right behind `TradeOpened`; a reader that ran first
        // delivered it before the coid was UNKNOWN (no prefix) and the mark then outlived its resolution.
        let (start_reader, start_gate) = mpsc::channel::<()>();
        if let Err(e) =
            spawn_reader(Arc::clone(&self.shared), id, reader_stream, deadline, start_gate)
        {
            tracing::error!(error = %e, "gatewayhub trade: cannot start the lease reader");
            self.schedule_dial();
            return;
        }
        self.shared.set_opened(opened.clone(), features);
        self.conn = Some(Conn { id, stream, heartbeat, last_write: Instant::now() });
        self.backoff = self.shared.link.timings().reconnect_base;
        self.dial_failures = 0;
        self.refused_redials = 0;
        self.dialing = false;
        self.next_dial = None;

        let old_epoch = self.prev_epoch;
        let epoch_changed = old_epoch.is_some_and(|e| e != opened.gw_epoch);
        if !first {
            self.shared.count_reconnect(epoch_changed);
        }
        let resumed = self.ledger.resumed(old_epoch, &opened);
        self.prev_epoch = Some(opened.gw_epoch);
        self.emit_all(resumed.events);
        if !resumed.resolve.is_empty() {
            // Only the coids still open: one whose terminal event this lease delivered has nothing to resolve.
            let marked = self.shared.mark_unknown(resumed.resolve);
            if !marked.is_empty() {
                self.ask_resolution(marked);
            }
        }
        // A dropped sender opens the gate too: the reader then reads a socket this thread has shut.
        let _ = start_reader.send(());
        for frame in resumed.flush {
            self.on_frame(frame);
        }
        if self.conn.is_none() {
            return; // the link fell again while flushing: `lost` has scheduled the next dial.
        }
        self.shared.set_up();
        if !first {
            tracing::info!(
                gw_epoch = opened.gw_epoch,
                epoch_changed,
                "TRADE-LINK: the gatewayhub trade link is back"
            );
            self.shared.signal.link_restored();
        }
        if let Some(old) = old_epoch.filter(|e| *e != opened.gw_epoch) {
            self.shared.signal.epoch_changed(old, opened.gw_epoch);
        }
    }

    /// **Whether an `AdapterReply` read off connection `conn` still counts.** It does on the current connection;
    /// it ALSO does on one lost since, as long as the daemon process is the same (the connection's epoch is the
    /// last attached one's): the adapter answered, and a write failure at the instant of the reply (a ping or a
    /// submit calls `lost`) must not strand the held teaching and cancels behind `awaiting_adapter` with nothing
    /// left to ask again (W13c; the redial on the same epoch neither re-arms the flag nor re-fires
    /// `epoch_changed`). A reply from an OLDER epoch is the previous process's: the new epoch's own adapter has
    /// not answered, and the flag is armed for it.
    fn answer_counts(&self, conn: u64) -> bool {
        if self.conn.as_ref().is_some_and(|c| c.id == conn) {
            return true;
        }
        let epoch_of = |id: u64| self.conn_epochs.iter().find(|(i, _)| *i == id).map(|(_, e)| *e);
        epoch_of(conn).is_some() && epoch_of(conn) == self.prev_epoch
    }

    /// After an epoch change: ask what the daemon knows of the UNKNOWN submits (fire and record).
    fn ask_resolution(&self, coids: Vec<String>) {
        if !self.shared.has_feature(FEATURE_GW_TRADE_RESOLVE) {
            tracing::warn!(
                count = coids.len(),
                "gatewayhub trade: orders Written across a gatewayhub restart stay UNKNOWN and the \
                 daemon cannot resolve them ({FEATURE_GW_TRADE_RESOLVE:?} not advertised); reconcile \
                 and the watchdog are the backstop"
            );
            return;
        }
        let rid = self.shared.next_rid();
        self.shared.ask_resolve(rid, TradeUp::ResolveOrders { rid, coids });
    }

    /// The link was lost: tear the connection down, fail the waiters, tell the core, schedule a redial.
    fn lost(&mut self, why: &str) {
        let Some(conn) = self.conn.take() else { return };
        let _ = conn.stream.shutdown(Shutdown::Both);
        self.shared.set_down();
        self.shared.fail_pending(why);
        tracing::warn!(why, "TRADE-LINK: the gatewayhub trade link was lost");
        self.shared.signal.link_lost(why);
        if !self.closed {
            self.schedule_dial();
        }
    }

    /// The lease is over for good. Held cancels get their last word, and so does every frame that was
    /// Written and never acknowledged (`fate` says whether the daemon provably never read it, or may have
    /// executed it); the thread stays to dispose of whatever still arrives, until `Stop`.
    fn close_for_good(&mut self, why: &str, fate: Fate) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.shared.set_closed(why);
        if why != "stopped" {
            tracing::error!(why, "TRADE-LINK: the gatewayhub trade lease is closed for good");
        }
        self.shared.fail_pending(why);
        if let Some(conn) = self.conn.take() {
            let _ = conn.stream.shutdown(Shutdown::Both);
            if why != "stopped" {
                // The dead-man's trade-link latch must open and STAY open: no more `Live` is coming.
                self.shared.signal.link_lost(why);
            }
        }
        let closed = self.ledger.closed(fate);
        self.emit_all(closed.events);
        if !closed.unknown_submits.is_empty() {
            // Nothing can ask the daemon about them any more (the lease is gone), and a synthesized
            // terminal could be false: they stay UNKNOWN, named, until reconcile or a restart settles
            // them. `TradeSession::unknown_orders` lists them.
            tracing::error!(
                why,
                coids = ?closed.unknown_submits,
                "TRADE-LINK: submits were Written and never acknowledged when the lease closed; the \
                 gatewayhub may have executed them (UNKNOWN: reconcile, or a restart's boot reconcile, \
                 finds them)"
            );
            self.shared.mark_unknown(closed.unknown_submits);
        }
        if !closed.unknown_cancels.is_empty() {
            tracing::error!(
                why,
                coids = ?closed.unknown_cancels,
                "TRADE-LINK: cancels were Written and never acknowledged when the lease closed; the \
                 orders may still be resting (reconcile finds them)"
            );
        }
    }

    fn on_bye(&mut self, bye: TradeBye) {
        match bye {
            TradeBye::Takeover { by_key_id } => {
                self.close_for_good(&format!("taken over by {by_key_id}"), Fate::Unknown);
            }
            TradeBye::Protocol(words) => {
                // The daemon stopped reading at the violating frame, after acknowledging every frame
                // before it: what is still unacknowledged was never read.
                self.close_for_good(
                    &format!("the gatewayhub refused this client's frames: {words}"),
                    Fate::Unprocessed,
                );
            }
            TradeBye::ClientClosed => {
                if self.conn.is_some() {
                    self.lost("the gatewayhub closed the lease at the client's request");
                }
            }
            TradeBye::ServerStopping => self.lost("the gatewayhub is stopping"),
            TradeBye::TooSlow => {
                self.lost("the gatewayhub dropped this client for reading too slowly")
            }
        }
    }

    /// End the lease cleanly: wait (bounded) for the daemon to acknowledge what was written, say the final
    /// `Detach`, wait (bounded) for its goodbye, and close for good. Idempotent (a closed lease is left).
    fn finish_stop(&mut self) {
        if self.conn.is_some() && !self.closed {
            self.drain_acks();
            self.send_control(&TradeUp::Detach { phase: DetachPhase::Final });
            self.await_goodbye();
        }
        self.close_for_good("stopped", Fate::Unknown);
    }

    /// Wait up to [`ACK_DRAIN`] for every Written frame to be acknowledged (`CmdAck`: the daemon READ it
    /// and queued it - the cancel sweep of a shutdown is what this protects). The wait is a `recv` on the
    /// inbox, so the acks (and anything else that arrives) are handled as usual; what is still
    /// unacknowledged at the bound is named, and the final `Detach` follows regardless: it is ordered
    /// behind those frames on the one TCP stream, so the daemon reads them first.
    fn drain_acks(&mut self) {
        let deadline = Instant::now() + ACK_DRAIN;
        while self.conn.is_some() && self.ledger.in_flight() > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                tracing::warn!(
                    unacknowledged = self.ledger.in_flight(),
                    "TRADE-LINK: stopping with frames the gatewayhub has not acknowledged yet; the \
                     final detach follows them on the same stream, so it reads them first"
                );
                return;
            }
            // The heartbeat keeps beating while this waits: a silent client is a ghost the daemon releases.
            let wait = self
                .conn
                .as_ref()
                .map_or(left, |c| ping_due_in(c.last_write, c.heartbeat, Instant::now()).min(left));
            match self.rx.recv_timeout(wait.max(Duration::from_millis(1))) {
                Err(RecvTimeoutError::Timeout) => self.on_tick(),
                // Already finishing: a second detach changes nothing, and a stop only means the thread
                // ends when this does.
                Ok(Msg::Stop) => self.done = true,
                Ok(Msg::Detach(_)) => {}
                Ok(msg) => self.on_msg(msg),
                Err(_) => return,
            }
        }
    }

    /// After the final `Detach`, wait (bounded) for the daemon's goodbye or its close BEFORE tearing the
    /// socket down. Closing at once would be an abortive close whenever the daemon's answer is still on
    /// its way: this end would answer that data with a reset, and a reset makes the daemon DISCARD the
    /// frames it had received but not yet read - including the cancel sweep sent just before.
    fn await_goodbye(&mut self) {
        let Some(id) = self.conn.as_ref().map(|c| c.id) else { return };
        let deadline = Instant::now() + GOODBYE_WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match self.rx.recv_timeout(left) {
                Ok(Msg::Bye { conn, .. } | Msg::ReaderClosed { conn, .. }) if conn == id => return,
                Ok(Msg::Stop) => self.done = true,
                Ok(msg) => self.on_msg(msg),
                Err(_) => return,
            }
        }
    }

    // ---- redial ------------------------------------------------------------------------------------

    fn schedule_dial(&mut self) {
        let timings = self.shared.link.timings();
        self.next_dial = Some(Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(timings.reconnect_max);
        self.dialing = false;
    }

    fn start_dial(&mut self) {
        self.dialing = true;
        let link = self.shared.link.clone();
        let args = open_args(&self.shared.account, self.shared.resume_cursor(), false);
        let tx = self.shared.tx.clone();
        let spawned = thread::Builder::new().name("gw-trade-dial".to_string()).spawn(move || {
            let result = GatewayClient::connect(&link, Scope::Trade)
                .map_err(TradeOpenError::from)
                .and_then(|client| client.open_trade(&args));
            // A closed inbox means the lease ended while dialing; the stream drops with it.
            let _ = tx.send(Msg::Dialed(result));
        });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "gatewayhub trade: cannot start the redial thread");
            self.schedule_dial();
        }
    }

    fn on_dialed(&mut self, result: Result<TradeStream, TradeOpenError>) {
        self.dialing = false;
        if self.closed {
            return; // a dial that finished after the lease closed: the stream drops here.
        }
        match result {
            Ok(stream) => self.attach(stream, false),
            Err(TradeOpenError::LeaseHeld { holder_key_id, .. }) => {
                self.close_for_good(
                    &format!("the lease is held by {holder_key_id}"),
                    Fate::Unknown,
                );
            }
            Err(TradeOpenError::Refused { block_word, text }) => {
                // A refusal can be momentary (the account table read failed, a row was being edited), and
                // closing for good strands the link until the tradehub restarts: retry a bounded number
                // of times on the dial schedule before the refusal is taken as the account's own state.
                self.refused_redials += 1;
                if self.refused_redials >= REFUSED_REDIAL_ATTEMPTS {
                    self.close_for_good(
                        &format!("the gatewayhub refused the lease ({block_word}): {text}"),
                        Fate::Unknown,
                    );
                } else {
                    tracing::warn!(
                        attempt = self.refused_redials,
                        of = REFUSED_REDIAL_ATTEMPTS,
                        block_word,
                        text,
                        "TRADE-LINK: the gatewayhub refused the lease at a redial; trying again"
                    );
                    self.schedule_dial();
                }
            }
            Err(e @ (TradeOpenError::Link(_) | TradeOpenError::Daemon(_))) => {
                self.dial_failures += 1;
                // The first failure and every dozenth are worth a line; the rest would be a flood.
                if self.dial_failures == 1 || self.dial_failures.is_multiple_of(12) {
                    tracing::warn!(
                        attempts = self.dial_failures,
                        error = %e,
                        "TRADE-LINK: the gatewayhub trade link is down and redialling"
                    );
                }
                self.schedule_dial();
            }
        }
    }

    // ---- the end -----------------------------------------------------------------------------------

    /// The loop is over: whatever is still in the inbox gets its disposition, never silence.
    fn finish(&mut self) {
        self.close_for_good("stopped", Fate::Unknown);
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Frame(frame) => {
                    let events = self.ledger.unsent(*frame, Why::Closed);
                    self.emit_all(events);
                }
                Msg::Ask(frame) => {
                    if let Some(rid) = rid_of(&frame) {
                        self.shared.fail_unsent(rid);
                    }
                }
                Msg::Acked { .. }
                | Msg::AdapterAnswered { .. }
                | Msg::ReleaseHeld { .. }
                | Msg::ReaderClosed { .. }
                | Msg::Bye { .. }
                | Msg::Dialed(_)
                | Msg::Detach(_)
                | Msg::Stop => {}
            }
        }
        self.shared.fail_pending("the lease ended");
    }
}

/// The `rid` of a request frame.
fn rid_of(frame: &TradeUp) -> Option<u64> {
    match frame {
        TradeUp::AdapterStart(start) => Some(start.rid),
        TradeUp::WireIds { rid, .. }
        | TradeUp::ResolveOrders { rid, .. }
        | TradeUp::IdentityRecord { rid }
        | TradeUp::Recon { rid, .. }
        | TradeUp::TradeStatus { rid } => Some(*rid),
        TradeUp::Submit { .. }
        | TradeUp::SubmitBatch { .. }
        | TradeUp::Cancel { .. }
        | TradeUp::CancelBatch { .. }
        | TradeUp::Modify { .. }
        | TradeUp::Confirm { .. }
        | TradeUp::AdoptRestored { .. }
        | TradeUp::Ping
        | TradeUp::Detach { .. } => None,
    }
}

/// Halve a batch frame IN PLACE: `frame` keeps the head and the tail is returned. `None` = the frame is
/// not a batch, or holds one element (nothing to split).
fn split_off_tail(frame: &mut TradeUp) -> Option<TradeUp> {
    match frame {
        TradeUp::SubmitBatch { cseq, orders } if orders.len() > 1 => {
            let tail = orders.split_off(orders.len() / 2);
            Some(TradeUp::SubmitBatch { cseq: *cseq, orders: tail })
        }
        TradeUp::CancelBatch { cseq, coids, intent } if coids.len() > 1 => {
            let tail = coids.split_off(coids.len() / 2);
            Some(TradeUp::CancelBatch { cseq: *cseq, coids: tail, intent: *intent })
        }
        TradeUp::AdoptRestored { cseq, pairs } if pairs.len() > 1 => {
            let tail = pairs.split_off(pairs.len() / 2);
            Some(TradeUp::AdoptRestored { cseq: *cseq, pairs: tail })
        }
        _ => None,
    }
}

/// One reader per connection. It waits on `start` before it reads a byte (W2G-5): the link thread opens the
/// gate once the connection's UNKNOWN coids are marked. A closed gate (the sender dropped) opens it too.
fn spawn_reader(
    shared: Arc<Shared>,
    conn: u64,
    stream: TcpStream,
    deadline: Duration,
    start: mpsc::Receiver<()>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name("gw-trade-read".to_string()).spawn(move || {
        let _ = start.recv();
        let tx = shared.tx.clone();
        let mut stream = stream;
        let _ = stream.set_read_timeout(Some(deadline));
        let why = loop {
            match read_capped::<_, TradeDown>(&mut stream, GW_DOWN_FRAME_CEILING_BYTES) {
                Ok(frame) => {
                    if let Flow::Stop(why) = shared.on_down(conn, frame) {
                        break why;
                    }
                }
                Err(e) if is_timeout(&e) => {
                    break format!(
                        "no frame for {deadline:?} (the heartbeat deadline): the link is dead or \
                         the gatewayhub is wedged"
                    );
                }
                Err(e) => break format!("read failed: {e}"),
            }
        };
        // A closed inbox means the lease already ended.
        let _ = tx.send(Msg::ReaderClosed { conn, why });
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::WireCancelIntent;
    use vike_model::OrderRequest;

    fn order(coid: &str) -> OrderRequest {
        OrderRequest { client_order_id: coid.into(), ..Default::default() }
    }

    #[test]
    fn a_batch_is_halved_until_a_single_frame_and_a_single_frame_is_not() {
        let mut head = TradeUp::SubmitBatch {
            cseq: 9,
            orders: (0..5).map(|i| order(&format!("c{i}"))).collect(),
        };
        let Some(tail) = split_off_tail(&mut head) else { panic!("a five-order batch splits") };
        let (
            TradeUp::SubmitBatch { cseq: a, orders: h },
            TradeUp::SubmitBatch { cseq: b, orders: t },
        ) = (head, tail)
        else {
            panic!("two batches")
        };
        assert_eq!((a, b, h.len(), t.len()), (9, 9, 2, 3));
        let mut one = TradeUp::SubmitBatch { cseq: 1, orders: vec![order("only")] };
        assert!(split_off_tail(&mut one).is_none());
        let mut cancel =
            TradeUp::Cancel { cseq: 1, coid: "k".into(), intent: WireCancelIntent::RiskOff };
        assert!(split_off_tail(&mut cancel).is_none(), "a single cancel never splits");
    }

    #[test]
    fn every_request_frame_names_its_rid_and_no_command_does() {
        assert_eq!(rid_of(&TradeUp::WireIds { rid: 4, coids: vec![] }), Some(4));
        assert_eq!(rid_of(&TradeUp::TradeStatus { rid: 5 }), Some(5));
        assert_eq!(rid_of(&TradeUp::Ping), None);
        assert_eq!(rid_of(&TradeUp::Confirm { cseq: 1, coid: "c".into() }), None);
    }

    /// The write boundary, against a socket whose peer is GONE: a frame whose `write_all` fails is
    /// NEVER-SENT (terminal reject, `cseq` not spent), and one whose write returned is Written. Every
    /// frame is exactly one of the two, and the link is declared lost by the failure.
    #[test]
    fn a_write_that_fails_is_never_sent_and_a_write_that_returned_is_written() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = TcpStream::connect(addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        // The peer is gone: the first segment may land in the kernel, and the reset it provokes fails a
        // later write.
        drop(server);
        drop(listener);

        let (events, mut lane) = vike_exec::event_channel(4_096);
        let account = TradeAccount {
            client_id: "c".into(),
            venue: "binance".into(),
            label: None,
            tier: vike_model::AccountTier::Demo,
            route_key: "binance".into(),
        };
        let keys = vike_hub_proto::auth::HubKeys::new(b"o".to_vec(), b"c".to_vec())
            .with_trade(b"t".to_vec());
        let (shared, inbox) = Shared::new(
            GatewayLink::new(addr.to_string(), keys),
            account,
            TradeTimings::default(),
            None,
            events,
            None,
            Arc::new(NoSignal),
            None,
        );
        let mut writer = Writer::new(Arc::clone(&shared), inbox);
        let opened = TradeOpened {
            gw_epoch: 1,
            lease_epoch: 1,
            received_through_cseq: 0,
            project_token: String::new(),
            heartbeat_ms: 2_000,
            event_ring_floor: Some(0),
        };
        writer.attach(
            TradeStream {
                stream: client,
                opened,
                heartbeat: Duration::from_secs(2),
                features: vec![],
            },
            true,
        );
        assert!(writer.conn.is_some());

        let big = "x".repeat(60_000);
        let mut submitted = Vec::new();
        for i in 0..400 {
            let coid = format!("w{i}");
            let mut o = order(&coid);
            o.symbol.clone_from(&big);
            submitted.push(coid);
            writer.on_frame(TradeUp::Submit { cseq: 0, order: o });
            if writer.conn.is_none() {
                break;
            }
        }
        assert!(writer.conn.is_none(), "a write to a vanished peer must fail and lose the link");

        let mut rejected = std::collections::BTreeSet::new();
        while let Ok(ingest) = lane.try_recv() {
            if let vike_exec::Ingest::Event(vike_model::events::Event::OrderRejected(r)) = ingest {
                assert_eq!(r.reason.as_str(), crate::trade::NEVER_SENT_SUBMIT_REASON);
                rejected.insert(r.client_order_id);
            }
        }
        assert_eq!(rejected.len(), 1, "exactly the frame whose write failed: {rejected:?}");
        assert_eq!(
            rejected.len() + writer.ledger.in_flight(),
            submitted.len(),
            "every frame is Written or rejected: none vanished, none is both"
        );
        assert!(
            !writer.ledger.written_cseqs().contains(&writer.ledger.next_cseq()),
            "the failed frame's number was not spent"
        );
    }

    /// A connected socket pair on loopback: `(client end, server end)`.
    fn socket_pair(listener: &std::net::TcpListener) -> (TcpStream, TcpStream) {
        let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        (client, server)
    }

    fn opened_in(epoch: u64) -> TradeOpened {
        TradeOpened {
            gw_epoch: epoch,
            lease_epoch: 1,
            received_through_cseq: 0,
            project_token: String::new(),
            heartbeat_ms: 2_000,
            event_ring_floor: Some(0),
        }
    }

    fn stream_of(client: TcpStream, epoch: u64) -> TradeStream {
        TradeStream {
            stream: client,
            opened: opened_in(epoch),
            heartbeat: Duration::from_secs(2),
            features: vec![],
        }
    }

    /// The next frame the daemon end reads, or `None` after 300 ms of silence.
    fn read_up(server: &mut TcpStream) -> Option<TradeUp> {
        use std::io::Read;
        server.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
        let mut len = [0u8; 4];
        server.read_exact(&mut len).ok()?;
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        server.read_exact(&mut body).ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// A writer over `addr` delivering to `events` (the caller holds the lane's other end).
    fn writer_over(addr: std::net::SocketAddr, events: EventSender) -> Writer {
        let account = TradeAccount {
            client_id: "c".into(),
            venue: "binance".into(),
            label: None,
            tier: vike_model::AccountTier::Demo,
            route_key: "binance".into(),
        };
        let keys = vike_hub_proto::auth::HubKeys::new(b"o".to_vec(), b"c".to_vec())
            .with_trade(b"t".to_vec());
        let (shared, inbox) = Shared::new(
            GatewayLink::new(addr.to_string(), keys),
            account,
            TradeTimings::default(),
            None,
            events,
            None,
            Arc::new(NoSignal),
            None,
        );
        Writer::new(shared, inbox)
    }

    /// **W13c finding 2.** The daemon restarted (a new epoch), the held risk-off cancel waits for the
    /// adapter's answer, and the link is lost at the instant the answer was read: a failed write of a ping
    /// or a submit calls `lost`, so the link thread sees the connection gone BEFORE it handles
    /// `AdapterAnswered`. Dropped for the stale connection id, `awaiting_adapter` stayed set and nothing
    /// re-fired the epoch change: the held cancel waited until the lease closed. The answer belongs to the
    /// daemon process, not to the socket, so it counts while the epoch is the same.
    #[test]
    fn an_adapter_answer_read_off_a_connection_lost_since_still_releases_the_held_frames() {
        use crate::proto::WireCancelIntent;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let (events, _lane) = vike_exec::event_channel(4_096);
        let mut writer = writer_over(listener.local_addr().expect("addr"), events);

        // epoch 1: a risk-off cancel is written and never acknowledged; the link is lost.
        let (a, _a_server) = socket_pair(&listener);
        writer.attach(stream_of(a, 1), true);
        writer.on_frame(TradeUp::Cancel {
            cseq: 0,
            coid: "k".into(),
            intent: WireCancelIntent::RiskOff,
        });
        writer.lost("the link was cut");

        // epoch 2 (the daemon restarted): the cancel is held for the adapter's answer.
        let (b, _b_server) = socket_pair(&listener);
        writer.attach(stream_of(b, 2), false);
        let b_id = writer.conn.as_ref().expect("attached").id;
        assert!(writer.ledger.gated(), "held for the new adapter");

        // The answer is read on `b`, and `b` is lost before the link thread handles it.
        writer.lost("write failed at the instant of the reply");
        writer.on_msg(Msg::AdapterAnswered { conn: b_id, hold: true });
        writer.on_msg(Msg::ReleaseHeld { anyway: false });

        // The redial lands on the SAME daemon process: the held cancel goes out.
        let (c, mut c_server) = socket_pair(&listener);
        writer.attach(stream_of(c, 2), false);
        assert!(!writer.ledger.gated(), "the answer was not stranded behind awaiting_adapter");
        assert!(
            matches!(read_up(&mut c_server), Some(TradeUp::Cancel { coid, .. }) if coid == "k"),
            "the held risk-off cancel reached the daemon"
        );
    }

    /// The other side of the same rule: a reply read off a connection of an OLDER epoch is the previous
    /// process's, not the new adapter's, and must not release what waits for the new one.
    #[test]
    fn an_adapter_answer_from_an_older_epoch_releases_nothing() {
        use crate::proto::WireCancelIntent;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let (events, _lane) = vike_exec::event_channel(4_096);
        let mut writer = writer_over(listener.local_addr().expect("addr"), events);
        let (a, _a_server) = socket_pair(&listener);
        writer.attach(stream_of(a, 1), true);
        let a_id = writer.conn.as_ref().expect("attached").id;
        writer.on_frame(TradeUp::Cancel {
            cseq: 0,
            coid: "k".into(),
            intent: WireCancelIntent::RiskOff,
        });
        writer.lost("the link was cut");
        let (b, _b_server) = socket_pair(&listener);
        writer.attach(stream_of(b, 2), false);
        assert!(writer.ledger.gated());

        writer.on_msg(Msg::AdapterAnswered { conn: a_id, hold: false });

        assert!(writer.ledger.gated(), "epoch 1's adapter is not epoch 2's");
    }

    /// **W2G-5: a reader hears nothing until the link thread opens its gate.** The gate is what lets
    /// `attach` mark the connection's UNKNOWN coids before any event can be delivered: a frame already
    /// waiting in the socket (a restored session's ring, replayed right behind `TradeOpened`) is not read,
    /// let alone delivered, while the gate is shut, and is delivered once it opens.
    #[test]
    fn a_reader_reads_nothing_until_its_start_gate_opens() {
        use crate::proto::{TradeDown, write_frame};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let (events, mut lane) = vike_exec::event_channel(64);
        let writer = writer_over(listener.local_addr().expect("addr"), events);
        let (client, mut server) = socket_pair(&listener);
        write_frame(
            &mut server,
            &TradeDown::Event {
                account: "binance".into(),
                gw_epoch: 1,
                seq: 1,
                ts_ms: 1,
                replayed: true,
                event: Event::OrderAccepted(vike_model::events::OrderAccepted {
                    client_order_id: "k".into(),
                    venue_order_id: Some("v".into()),
                    ts: 1,
                }),
            },
        )
        .expect("the frame is waiting in the socket");

        let (open, gate) = mpsc::channel::<()>();
        let reader =
            spawn_reader(Arc::clone(&writer.shared), 1, client, Duration::from_secs(2), gate)
                .expect("the reader spawns");
        thread::sleep(Duration::from_millis(250));
        assert!(lane.try_recv().is_err(), "a closed gate delivers nothing");

        open.send(()).expect("the reader is waiting");
        let deadline = Instant::now() + Duration::from_secs(5);
        while lane.try_recv().is_err() {
            assert!(Instant::now() < deadline, "an open gate delivers the waiting frame");
            thread::sleep(Duration::from_millis(5));
        }
        drop(server); // the reader ends on the closed socket
        reader.join().expect("the reader ends");
    }

    /// **W2G-5, at `attach`: the coids are marked BEFORE the reader hears its first event.** The daemon that
    /// rebuilt the session itself has the answer waiting in the socket. The test parks `attach` inside
    /// `mark_unknown` (it holds the UNKNOWN set's lock) and watches the core's lane for a while: nothing may
    /// arrive. Released, the accept arrives, clears the mark that is already there, and the coid is no longer
    /// UNKNOWN. (An accept is the event that needs no UNKNOWN lookup before it is delivered, so a reader
    /// that ran ahead would deliver it while `attach` is parked.)
    #[test]
    fn attach_marks_the_unknown_coids_before_the_reader_hears_a_waiting_event() {
        use crate::proto::{TradeDown, write_frame};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let (events, lane) = vike_exec::event_channel(4_096);
        let mut writer = writer_over(listener.local_addr().expect("addr"), events);

        // Epoch 1: a submit is Written and never acknowledged; the daemon dies.
        let (a, _a_server) = socket_pair(&listener);
        writer.attach(stream_of(a, 1), true);
        writer.on_frame(TradeUp::Submit { cseq: 0, order: order("k") });
        writer.lost("the daemon died");

        // Epoch 2: the new daemon already holds the venue's answer, waiting in the socket.
        let (b, mut b_server) = socket_pair(&listener);
        write_frame(
            &mut b_server,
            &TradeDown::Event {
                account: "binance".into(),
                gw_epoch: 2,
                seq: 1,
                ts_ms: 1,
                replayed: true,
                event: Event::OrderAccepted(vike_model::events::OrderAccepted {
                    client_order_id: "k".into(),
                    venue_order_id: Some("v".into()),
                    ts: 1,
                }),
            },
        )
        .expect("the answer is waiting in the socket");

        let shared = Arc::clone(&writer.shared);
        let watcher = thread::spawn(move || {
            let mut lane = lane;
            let parked = shared.hold_unknown_for_test();
            thread::sleep(Duration::from_millis(500));
            let heard = lane.try_recv().is_ok();
            drop(parked);
            (heard, lane)
        });
        thread::sleep(Duration::from_millis(100)); // the watcher holds the lock before `attach` runs
        writer.attach(stream_of(b, 2), false);
        let (heard, lane) = watcher.join().expect("the watcher ends");
        assert!(!heard, "the reader delivered an event while the coid was still unmarked");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut lane = lane;
        while lane.try_recv().is_err() {
            assert!(Instant::now() < deadline, "the waiting answer arrives once the gate opens");
            thread::sleep(Duration::from_millis(5));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !writer.shared.unknown_orders().is_empty() {
            assert!(
                Instant::now() < deadline,
                "the answer clears the mark: {:?}",
                writer.shared.unknown_orders()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
