//! The dedicated-thread bridge + `ExecutionClient` impl. See the module docs.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use super::close::{self, AccountMode, CloseDesk, CloseTrack, Route, Shape};
use super::commission::OpenCommissions;
use super::event_mapper::{
    amend_needs_reread, closes_routing, ends_cancelability, is_market, map_amend,
    map_drained_event, map_placement, plan_amend, preflight_request, resolve_unknown_amend,
    venue_rate_differs,
};
use super::exit;
use super::sys::{FxcmSession, PendingLookup, PlacedOrder, Side};
use vike_bridge_core::exec_actor::{
    CancelOutcome, ConfirmFn, ExecActor, ExecCommand, cancel_batch_undeclared, cancel_event,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{Event, OrderModifyRejected, OrderSubmitted};
use vike_model::{OrderRequest, OrderType};

use super::config::FxcmConfig;
use super::redial::{CREDENTIALS_REFUSED, one_retry_at_a_time, pause_after, refuses_credentials};

/// Cadence for polling the shim's async order-event queue between commands (audit A3 fill lane).
const EVENT_POLL: Duration = Duration::from_millis(100);

/// Resting distance (pips) for a limit that names NO price: the shim computes the rate from the live
/// quote (`fc_place_entry`). A limit that names a price never uses it — it rests at that rate
/// (`fc_place_entry_at`).
const RESTING_PIPS: i32 = 50;

/// How many venue-order-id → client-order-id routing rows the session keeps.
///
/// The map exists so a LATER async fill can be routed back to the order that placed it, and until
/// this bound existed nothing ever removed a row that had filled — one row per accepted order, for
/// the life of the session. The cap is a MEMORY bound, not a correctness one: evicting the oldest
/// row makes a very old order's late fill unroutable, which is the same declared hole a restart
/// already opens (see [`map_drained_event`]) and which reconcile is what recovers from. Sized so a
/// session placing an order a second still keeps a day of them.
const MAX_ROUTES: usize = 100_000;

/// How many placements left UNKNOWN (reply lost) the session remembers for its `confirm`.
/// An entry leaves [`UnknownTable`] when `confirm` resolves it (found -> `OrderAccepted`); one that
/// never resolves stays until this cap evicts the OLDEST. Unknown outcomes are rare and a hole here
/// only costs the active confirm (reconcile and the watchdog still stand).
const MAX_UNKNOWN: usize = 1024;

/// The pause between the two read-backs that settle a lost amend reply (bounded on purpose: the
/// exec thread is blocked for it).
const AMEND_REREAD_PAUSE: Duration = Duration::from_millis(1500);

/// How long a `confirm` worker waits for the exec thread's ACK. The ack only bounds the worker (it
/// is joined at `detach`); the events themselves are sent by the exec thread, so an ack that comes
/// late loses nothing. It must still outlast the worst case below, or the worker gives up on a
/// thread that is merely busy.
const CONFIRM_REPLY_WAIT: Duration = Duration::from_secs(45);

/// The most time the exec thread spends in ONE command before it reaches the confirm lane, from the
/// shim's own bounds (`fcshim.cpp`): a placement <= 20 s, plus the subscription of an instrument the session meets for
/// the first time ([`crate::subscription::EXEC_BUDGET_MS`]); an amend = 10 s edit + 5 s read-back +
/// [`AMEND_REREAD_PAUSE`] + 5 s read-back; both followed by the 5 s rate echo.
const COMMAND_WORST_MS: u128 =
    PLACEMENT_WORST_MS + crate::subscription::EXEC_BUDGET_MS as u128 + 5_000;
/// One placement alone, from `fcshim.cpp`'s bounds (`ORDER_WAIT_MS` + the two pre-send refreshes + `LOOKUP_REFRESH_MS`).
const PLACEMENT_WORST_MS: u128 = 20_000;
/// The default of `guards.submit_ack_timeout_ms` (30 s): the stuck-order watchdog's confirm falls this long after a submit, and a
/// FIRST order on an instrument may spend the subscription budget before its placement. That sum must stay under it, and
/// it does by 2 s: raise `EXEC_BUDGET_MS` and this fails the build, not the watchdog.
const SUBMIT_ACK_TIMEOUT_MS: u128 = 30_000;
const _: () = assert!(
    PLACEMENT_WORST_MS + (crate::subscription::EXEC_BUDGET_MS as u128) < SUBMIT_ACK_TIMEOUT_MS,
    "a first order's subscription plus its placement must fit under the default ack timeout"
);
/// One closing leg alone, from `fcshim.cpp`'s bounds (`fc_close_trade`: `PRESEND_REFRESH_MS` + `ORDER_WAIT_MS`).
const CLOSE_LEG_WORST_MS: u128 = 12_500;
/// A close sends one order per trade it closes and starts another only while the elapsed time is under this, so the
/// whole command (the open-trades read of 2.5 s counted in the elapsed time, the last leg at its worst) ends inside the
/// default ack timeout, and the legs left over are named in the order's terminal ([`close::Closed::stopped`]).
const CLOSE_LEG_START_MS: u128 = 15_000;
const _: () = assert!(
    CLOSE_LEG_START_MS + CLOSE_LEG_WORST_MS < SUBMIT_ACK_TIMEOUT_MS,
    "a close's last leg must end under the default ack timeout"
);
/// One trade-bound exit alone, from `fcshim.cpp`'s bounds: the open-trades read and the exits read (`PRESEND_REFRESH_MS`
/// each) in front of `fc_place_exit` (its own trades refresh, `ORDER_WAIT_MS`, and the one `LOOKUP_REFRESH_MS` lookup of a
/// reply that was lost). Nothing is subscribed first (an exit is reduce-only), and the rate echo that follows is the 5 s
/// `ECHO_REFRESH_MS`, so the whole command fits [`COMMAND_WORST_MS`] and ends inside the default ack timeout.
const EXIT_WORST_MS: u128 = 2_500 + 2_500 + 2_500 + 10_000 + 5_000;
const _: () = assert!(
    EXIT_WORST_MS + 5_000 <= COMMAND_WORST_MS && EXIT_WORST_MS < SUBMIT_ACK_TIMEOUT_MS,
    "an exit's reads, placement and lookup must end under the default ack timeout and inside the command bound"
);
/// How many closes in progress the session remembers (see [`insert_close`]).
const MAX_CLOSES: usize = 1024;
const AMEND_WORST_MS: u128 = 10_000 + 5_000 + AMEND_REREAD_PAUSE.as_millis() + 5_000 + 5_000;
/// One `fc_find_order` (the shim's `LOOKUP_REFRESH_MS`).
const LOOKUP_WORST_MS: u128 = 5_000;
const _: () = assert!(
    COMMAND_WORST_MS + LOOKUP_WORST_MS < CONFIRM_REPLY_WAIT.as_millis()
        && AMEND_WORST_MS + LOOKUP_WORST_MS < CONFIRM_REPLY_WAIT.as_millis(),
    "CONFIRM_REPLY_WAIT must outlast the slowest command plus one lookup"
);

/// The total time one pass over the confirm lane may spend, so a burst of confirms (each a bounded
/// `fc_find_order`) cannot starve the commands and the event drain. Past it the rest stay queued for
/// the next pass of the loop, never dropped.
const CONFIRM_DRAIN_BUDGET: Duration = Duration::from_secs(10);

/// A placement whose reply was lost: its terms for `fc_find_order`, and the request's own timestamp
/// for the `OrderAccepted` a successful `confirm` emits.
struct UnknownPlacement {
    lookup: PendingLookup,
    ts: i64,
}

/// The placements left UNKNOWN, coid -> terms, bounded FIFO (see [`MAX_UNKNOWN`]): resolved rows are
/// removed, and past the cap the OLDEST goes (age of the loss is the only signal; an old unknown is
/// the least likely to still be findable).
#[derive(Default)]
struct UnknownTable {
    map: HashMap<String, UnknownPlacement>,
    inserted: VecDeque<String>,
}

impl UnknownTable {
    fn insert(&mut self, coid: String, placement: UnknownPlacement) {
        if self.map.insert(coid.clone(), placement).is_none() {
            self.inserted.push_back(coid);
        }
        while self.inserted.len() > MAX_UNKNOWN {
            if let Some(oldest) = self.inserted.pop_front() {
                self.map.remove(&oldest);
            }
        }
    }

    /// Drop the row from BOTH halves, so they always hold the same set (as [`RouteTable::remove`]).
    fn remove(&mut self, coid: &str) -> Option<UnknownPlacement> {
        let placement = self.map.remove(coid)?;
        if let Some(pos) = self.inserted.iter().position(|c| c == coid) {
            self.inserted.remove(pos);
        }
        Some(placement)
    }
}

/// One `confirm` for the exec thread to answer: the coid, and where to ACK. The events are sent by
/// the exec thread itself (never through this reply): a worker that already gave up on the wait must
/// not take an `OrderAccepted` down with it.
struct ConfirmRequest {
    coid: String,
    reply: Sender<()>,
}

/// The `confirm` worker's side: hand the coid to the exec thread (the one owner of the session) and
/// wait, bounded, for its ack. A gone exec thread, or no ack, returns at once or at the bound; either
/// way the exec thread emits whatever it found, so there is never a `Vec` to carry back.
fn ask_exec_thread(tx: &Sender<ConfirmRequest>, coid: &str) {
    let (reply, ack) = channel();
    if tx.send(ConfirmRequest { coid: coid.to_string(), reply }).is_ok() {
        let _ = ack.recv_timeout(CONFIRM_REPLY_WAIT);
    }
}

/// Venue order id → client order id, bounded FIFO (see [`MAX_ROUTES`]).
///
/// Insertion-ordered eviction, not LRU: the question a route answers is "did THIS process place
/// this order", and recency of USE says nothing about that — an order that has been resting
/// untouched for hours is exactly as live as one that just filled. Age of PLACEMENT is the only
/// signal available, so it is the one used.
#[derive(Default)]
struct RouteTable {
    map: HashMap<String, String>,
    inserted: VecDeque<String>,
}

impl RouteTable {
    fn insert(&mut self, venue_order_id: String, coid: String) {
        if self.map.insert(venue_order_id.clone(), coid).is_none() {
            self.inserted.push_back(venue_order_id);
        }
        while self.inserted.len() > MAX_ROUTES {
            if let Some(oldest) = self.inserted.pop_front() {
                self.map.remove(&oldest);
            }
        }
    }

    /// Drop the row for `venue_order_id` from BOTH halves, so the two always hold the same set.
    ///
    /// The queue scan is O(n) and deliberate: leaving a dangling id behind would let a later
    /// eviction pop it, count it as having freed a row, and evict a LIVE row early. This runs on
    /// the venue's own session thread once per confirmed cancel — never on the vike-core hot fold.
    fn remove(&mut self, venue_order_id: &str) {
        if self.map.remove(venue_order_id).is_some()
            && let Some(pos) = self.inserted.iter().position(|id| id == venue_order_id)
        {
            self.inserted.remove(pos);
        }
    }

    fn routes(&self) -> &HashMap<String, String> {
        &self.map
    }
}

/// Live FXCM exec client. `submit`/`cancel` enqueue onto the session thread (non-blocking);
/// every venue event returns through the core ingest. Dropping it stops the thread (logout).
pub struct FxcmExecutionClient(ExecActor);

impl FxcmExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path))
    }

    /// Spawn the session thread: it logs in with `config` and pumps venue events into `events`.
    ///
    /// ⚠ **When the login FAILS the thread stays alive and REFUSES every command with the reason**
    /// ([`refuse_every_command`]) — it does not exit — **and logs in again** after a growing pause
    /// (30 s, 1 min, 2 min, 5 min, then every 10 min: `crate::redial::pause_after`), for the life of
    /// the mount (`open_session`). That is the half of "a login failure says why" that an operator
    /// actually meets: this doc used to read "the thread exits immediately and later submits are
    /// no-ops", and those no-ops were a silent vanish the venue-adapter contract forbids. No session
    /// still means no orders reach FXCM; the intent comes back REJECTED, naming the failure, until a
    /// retry opens the session, and then the mount works as after a normal start.
    ///
    /// ⚠ **`confirm` is wired** (the stuck-order watchdog's active prod at `submit_ack_timeout`): it
    /// asks the exec thread - the session's one owner, no second login - to look a placement whose
    /// reply was lost up again in a fresh Orders table by its RequestID. Found -> `OrderAccepted`;
    /// otherwise nothing, and the order waits for reconcile and the watchdog's last resort. A MARKET
    /// order whose reply is lost fills at once and leaves the Orders table, so this cannot find it:
    /// its fill arrives on the Trades lane for an order id this process never learned (dropped with a
    /// `warn!`, see [`map_drained_event`]) and reconcile books it.
    pub fn spawn(config: FxcmConfig, events: EventSender) -> Self {
        let (confirm_tx, confirm_rx) = channel::<ConfirmRequest>();
        // The exec thread sends the events itself; this closure only waits for its ack.
        let confirm: ConfirmFn = Arc::new(move |coid: &str| {
            ask_exec_thread(&confirm_tx, coid);
            Vec::new()
        });
        Self(
            ExecActor::spawn("fxcm-session", events.clone(), move |rx| {
                run(config, events, rx, confirm_rx)
            })
            .with_confirm(confirm),
        )
    }
}

impl ExecutionClient for FxcmExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.0.submit(request)
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.0.cancel(client_order_id)
    }
    /// Amend a resting order's PRICE (`fc_edit_order`). ⚠ It MUST be delegated like every other method
    /// on this wrapper: a newtype that omits it inherits the trait's no-op, and the amend then never
    /// reaches the session thread at all (no event, resting order untouched).
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.0.modify(order, new_qty, new_price)
    }
    /// The stuck-order watchdog's active prod. ⚠ It MUST be delegated like every other method on this
    /// wrapper: the trait default is an EMPTY `confirm`, so a newtype that omits it makes the whole
    /// lookup wired in [`Self::spawn`] unreachable while `COID_LOOKUP` still says `true`
    /// (`crates/vike-ops/tests/venues/confirm_forwarded_gate.rs` holds it for every bridge that
    /// wires `with_confirm`).
    fn confirm(&mut self, client_order_id: &str) {
        self.0.confirm(client_order_id)
    }
    /// Phase-one teardown seam — raise the stop flags, join nothing. See
    /// `crates/vike-exec/src/execution_engine/client.rs`'s `ExecutionClient::begin_detach`.
    /// ⚠ It MUST be delegated like every other method on this wrapper: a newtype that omits it
    /// inherits the trait's no-op, and the core's raise-all phase then skips this venue entirely
    /// while `detach` below still pays its full wind-down.
    fn begin_detach(&mut self) {
        self.0.begin_detach()
    }
    fn detach(&mut self) {
        self.0.detach()
    }
}

/// Map a canonical symbol to FXCM's slashed, upper-cased instrument form (`"eurusd"` -> `"EUR/USD"`).
/// `pub(crate)`: [`crate::home_conversion::facts_from_offers`] names the Offers row the same way.
pub(crate) fn to_fxcm_instrument(symbol: &str) -> String {
    let up = symbol.to_uppercase();
    if up.contains('/') {
        return up;
    }
    if up.len() == 6 && up.chars().all(|c| c.is_ascii_alphabetic()) {
        format!("{}/{}", &up[..3], &up[3..])
    } else {
        up
    }
}

/// **Whether and how an order makes its instrument tradable first**, with whether the shim was ASKED (so the caller
/// logs a failure once, not at every remembered refusal). A CLOSE (`reduce_only`) never subscribes and never pays a
/// round trip; a session that cannot subscribe at all (`cannot_subscribe`: an older shim, a connection that is not
/// `Demo`) answers a `transient` note without asking; otherwise `ask` runs once per instrument per session
/// ([`crate::subscription::ensure`]).
fn ensure_for_order(
    reduce_only: bool,
    cannot_subscribe: Option<&str>,
    instrument: &str,
    subscribed: &mut crate::subscription::Attempts,
    ask: impl FnOnce() -> Result<(), crate::subscription::Failure>,
) -> (Option<crate::subscription::Failure>, bool) {
    if reduce_only {
        return (None, false);
    }
    if let Some(gap) = cannot_subscribe {
        let why =
            format!("fxcm: the bridge cannot subscribe `{instrument}` for trading itself: {gap}");
        return (Some(crate::subscription::Failure { why, transient: true }), false);
    }
    let mut asked = false;
    let result = crate::subscription::ensure(subscribed, instrument, Instant::now(), || {
        asked = true;
        ask()
    });
    (result.err(), asked)
}

/// What one placement came to: the order, or `(sent with an UNKNOWN outcome, the reason, what a later lookup needs)`.
type Placement = Result<PlacedOrder, (bool, String, Option<PendingLookup>)>;

/// **The reason an instrument could not be subscribed rides on the refusal of an order the venue then refuses.** An
/// unknown outcome of the subscription (`note.transient`: no answer, an unreadable table, a shim too old to ask, a
/// connection that is not `Demo`) lets the order go to the venue, whose own lookup judges the row; when it is refused
/// (a DEFINITE refusal, `false`) the words say why the bridge did not subscribe it either. An accepted order, an order
/// whose own outcome is unknown (nothing was refused) and a definite subscription failure (it ended the order
/// before any placement, in its own words) are left exactly as they are.
fn with_subscription_note(
    note: Option<&crate::subscription::Failure>,
    outcome: Placement,
) -> Placement {
    match (note, outcome) {
        (Some(note), Err((false, reason, lookup))) if note.transient => {
            Err((false, format!("{reason}; {}", note.why), lookup))
        }
        (_, outcome) => outcome,
    }
}

/// Answer ONE command with `reason`, for a session that is not open. `false` = it was `Shutdown`:
/// the caller leaves.
///
/// ⚠ **The first version of this arm was `Err(_) => return`, and the two things it dropped were the
/// error text AND the orders.** Returning closed the command channel, so a submit that lost the race
/// with the thread's exit was simply discarded — no `OrderSubmitted`, no terminal, nothing — which is
/// the silent vanish the venue-adapter contract forbids, and the 45 seconds of nothing
/// `crates/bridges/fxcm/tests/fxcm_live_smoke.rs` reported as one mute timeout. (The other half of
/// the race reached `vike_bridge_core::exec_actor`'s dead-channel backstop, which synthesizes a
/// terminal reason of its own — "venue exec thread unavailable" — that says nothing about FXCM.)
///
/// The shape is the emitter split the contract requires: `OrderSubmitted` synchronously, then the
/// SAME [`map_placement`] refusal the preflight's own rejections take, so a dead session's reject
/// is byte-identical in shape to every other one this adapter emits.
fn refuse(events: &EventSender, cmd: ExecCommand, reason: &str) -> bool {
    match cmd {
        ExecCommand::Submit(req) => {
            let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: req.client_order_id.clone(),
                ts: req.ts,
            }));
            for ev in map_placement(&req.client_order_id, req.ts, Err(reason)) {
                let _ = events.blocking_send(ev);
            }
        }
        // NON-terminal, like every other unknown-order cancel here: there is no resting order,
        // and a cancel must never vanish either.
        ExecCommand::Cancel { client_order_id, .. } => {
            let _ = events.blocking_send(cancel_event(
                &client_order_id,
                CancelOutcome::Rejected(reason.to_string()),
            ));
        }
        ExecCommand::CancelBatch { client_order_ids, .. } => {
            cancel_batch_undeclared(events, &client_order_ids)
        }
        // NON-terminal, like a cancel: the resting order (there is none) keeps its terms, and the
        // amend must not vanish either.
        ExecCommand::Modify { order, .. } => {
            let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: order.client_order_id.clone(),
                reason: reason.into(),
                ts: order.ts,
            }));
        }
        ExecCommand::Shutdown => return false,
    }
    true
}

/// How long one spell of refusing lasts when the login will NEVER be tried again (a refused credential):
/// the spell is only the unit the loop is restarted in, so its length decides nothing.
const TERMINAL_SPELL: Duration = Duration::from_secs(3600);

/// How a spell of refusing ended.
#[derive(Debug, PartialEq, Eq)]
enum Spell {
    /// The pause is over: log in again.
    Elapsed,
    /// `Shutdown`, or every sender is gone: leave.
    Stop,
}

/// Refuse every command with `reason` until `until`, calling `tick` every [`EVENT_POLL`] (it answers
/// the `confirm` lane). A session that is not open answers each command at once — it never leaves
/// one waiting — and still stops on `Shutdown`, so `ExecActor::detach` joins this thread.
fn refuse_every_command(
    events: &EventSender,
    rx: &Receiver<ExecCommand>,
    reason: &str,
    until: Instant,
    mut tick: impl FnMut(),
) -> Spell {
    loop {
        tick();
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Spell::Elapsed;
        }
        match rx.recv_timeout(left.min(EVENT_POLL)) {
            Ok(cmd) => {
                if !refuse(events, cmd, reason) {
                    return Spell::Stop;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Spell::Stop,
        }
    }
}

/// Answer every `confirm` the watchdog has queued with the EMPTY answer a lane with no session to
/// look anything up in gives (it used to drop the lane so a `confirm` failed fast; the lane must
/// outlive a failed login now, because a later one may open).
fn ack_confirms(confirms: &Receiver<ConfirmRequest>) {
    while let Ok(req) = confirms.try_recv() {
        let _ = req.reply.send(());
    }
}

/// Log in; while that fails, REFUSE every command with the latest reason and try again after each
/// pause of `pause_for(failed attempts so far)` — for the life of the mount, because a login fails
/// for reasons that heal (FXCM's host discovery is down for a weekend close) and the account used to
/// stay dead until a process restart. `None` = the client was shut down before a session opened.
///
/// ⚠ **Refusing stays loud while the session is down**: each reason carries the
/// `crate::SESSION_UNAVAILABLE` MARKER so a consumer can tell a dead SESSION from a venue reject.
/// `crates/bridges/fxcm/tests/fxcm_live_smoke.rs`'s `OrderRejected` arm treats a venue reject as
/// acceptable ("market closed") and passes — so without the marker a dead demo account would land in
/// that arm and read GREEN.
///
/// ⚠ **A REFUSED credential ends the retrying** ([`refuses_credentials`]): after one such failure the
/// thread logs in no more and refuses every command until shut down, one `error!` line saying so.
/// Any other failure (a host that is down) keeps the schedule.
///
/// ⚠ **A command that queued DURING the login that finally succeeds is refused, not placed**: the
/// login blocks this thread for up to the shim's whole wait, the stuck-order watchdog has by then
/// timed the order out and told the book it was rejected, and placing it on a late session would
/// leave a live order the book does not know. (The FIRST login is different and unchanged: nothing
/// has refused or timed out anything yet, so what queued behind it is served.)
fn open_session<S>(
    mut login: impl FnMut() -> Result<S, String>,
    events: &EventSender,
    rx: &Receiver<ExecCommand>,
    confirms: &Receiver<ConfirmRequest>,
    pause_for: impl Fn(u32) -> Duration,
) -> Option<S> {
    let mut failed = 0u32;
    let mut reason = String::new();
    loop {
        // A RETRY takes its turn behind the other session's retry (the first login does not: unchanged).
        let turn = (failed > 0).then(one_retry_at_a_time);
        let attempt = login();
        drop(turn);
        match attempt {
            Ok(session) if failed == 0 => return Some(session),
            Ok(session) => {
                tracing::info!(
                    target: "vike_fxcm",
                    venue = "fxcm",
                    failed_attempts = failed,
                    "fxcm: the ForexConnect session opened after {failed} failed attempt(s); this \
                     mount accepts orders again"
                );
                let late = format!(
                    "{reason}; the session opened only after this command was sent, so it was not \
                     placed: send it again"
                );
                while let Ok(cmd) = rx.try_recv() {
                    if !refuse(events, cmd, &late) {
                        return None;
                    }
                }
                return Some(session);
            }
            Err(e) => {
                failed += 1;
                reason = format!("{}: {e}", crate::SESSION_UNAVAILABLE);
                if refuses_credentials(&e) {
                    // TERMINAL: a rotated or mistyped password does not heal, and every further failed
                    // login may count towards a lockout. Keep refusing, as a dead session always did,
                    // but never log in again; only `Shutdown` ends this.
                    tracing::error!(
                        target: "vike_fxcm",
                        venue = "fxcm",
                        reason = %reason,
                        "fxcm: the ForexConnect session did not open — {CREDENTIALS_REFUSED}; every \
                         command this mount receives will be REJECTED with this reason"
                    );
                    while refuse_every_command(
                        events,
                        rx,
                        &reason,
                        Instant::now() + TERMINAL_SPELL,
                        || ack_confirms(confirms),
                    ) == Spell::Elapsed
                    {}
                    return None;
                }
                let pause = pause_for(failed);
                if failed == 1 {
                    tracing::error!(
                        target: "vike_fxcm",
                        venue = "fxcm",
                        reason = %reason,
                        "fxcm: the ForexConnect session did not open — every command this mount \
                         receives will be REJECTED with this reason until a login succeeds; the \
                         login is retried, first in {} s and then at growing pauses",
                        pause.as_secs()
                    );
                } else {
                    tracing::warn!(
                        target: "vike_fxcm",
                        venue = "fxcm",
                        reason = %reason,
                        failed_attempts = failed,
                        "fxcm: the ForexConnect session still did not open; every command is still \
                         REJECTED; the next attempt is in {} s",
                        pause.as_secs()
                    );
                }
                let spell =
                    refuse_every_command(events, rx, &reason, Instant::now() + pause, || {
                        ack_confirms(confirms)
                    });
                if spell == Spell::Stop {
                    return None;
                }
            }
        }
    }
}

fn run(
    config: FxcmConfig,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    confirms: Receiver<ConfirmRequest>,
) {
    let Some(session) = open_session(
        || {
            FxcmSession::login(&config.user, &config.password, &config.url, &config.connection)
                .map_err(|e| e.to_string())
        },
        &events,
        &rx,
        &confirms,
        pause_after,
    ) else {
        return;
    };
    // The cancel map: coid -> the ids `delete_order` needs. Pruned on every terminal, since a
    // terminal order has no resting order to cancel (`ends_cancelability`).
    let mut placed: HashMap<String, PlacedOrder> = HashMap::new();
    // The fill-routing map, bounded — venue order_id -> coid. Pruned only when nothing further can
    // arrive for that order (`closes_routing`); a FILL deliberately keeps its row.
    let mut routes = RouteTable::default();
    // FXCM instrument -> its base unit size on THIS session's account: the number `qty` is divided
    // by to get the lot count the shim places. Cached for the life of the session because that is
    // exactly the scope over which it cannot change — it is per-instrument AND per-ACCOUNT, and a
    // session has one account (`firstAccount`). A cache miss costs one FFI round trip on the first
    // order for an instrument and nothing after; without it every submit pays one.
    let mut base_units: HashMap<String, f64> = HashMap::new();
    // What THIS session already asked FXCM to subscribe for trading, and how it went (`crate::subscription`).
    let mut subscribed = crate::subscription::Attempts::default();
    // Why this session cannot make an instrument tradable itself (an older shim, a `Real` connection), asked once.
    let cannot_subscribe = session.cannot_subscribe();
    if let Some(gap) = &cannot_subscribe {
        tracing::warn!(
            target: "vike_fxcm",
            venue = "fxcm",
            "fxcm: this session cannot subscribe an instrument for trading itself: {gap}. An order on an instrument \
             that is already tradable is placed as before; one that is not is refused with this reason"
        );
    }
    // The placements whose reply was LOST (coid -> what the shim sent), for `confirm`. In memory like
    // `routes`: after a restart the question falls to reconcile.
    let mut unknown = UnknownTable::default();
    // THE ACCOUNT'S MODE, read once (a login-time row): on a HEDGING account an opposite market order opens a second
    // trade, so a reduce-only market order closes the trades facing it BY ID instead (`crate::close`). `Unknown` (a shim
    // without `fc_account_traits`, a failed read) is not assumed to net.
    let traits = session.account_traits_json();
    let account_mode = AccountMode::from_traits(traits.as_ref().ok().and_then(|t| t.as_deref()));
    tracing::info!(
        target: "vike_fxcm",
        venue = "fxcm",
        mode = account_mode.describe(),
        traits = ?traits,
        "fxcm: the account's traits (`maintenance_type` \"0\" is a netting account; a reduce-only market order closes \
         trades by id on any other, and on an unreadable one)"
    );
    // Why this shim cannot close a trade by its id, if it cannot: asked once, like `cannot_subscribe`.
    let close_gap = session.close_gap();
    if let Some(gap) = &close_gap {
        tracing::warn!(
            target: "vike_fxcm",
            venue = "fxcm",
            "fxcm: this session cannot close a trade by its id: {gap}. A reduce-only market order is refused with this \
             reason unless the account is known to net"
        );
    }
    // Why this shim cannot bind a stop or a take-profit to a trade, if it cannot: asked once, like `close_gap`.
    let exit_gap = session.exit_gap();
    if let Some(gap) = &exit_gap {
        tracing::warn!(
            target: "vike_fxcm",
            venue = "fxcm",
            "fxcm: this session cannot bind a stop or a take-profit to a trade: {gap}. A reduce-only stop or limit is \
             refused with this reason unless the account is known to net"
        );
    }
    // The closes in progress (coid -> what their fills still owe), so the several fills of one close read as ONE order.
    let mut closes: HashMap<String, CloseTrack> = HashMap::new();
    // What each opening fill carried in commission, so a closing fill books only what is still owed ([`crate::commission`]).
    let mut opens = OpenCommissions::default();
    // The routing of a submit that may be a close: the account's mode, the shim's gap and the instruments with a close of
    // UNKNOWN outcome (further reduce-only closes of them are held for a minute).
    let mut close_desk = CloseDesk::new(account_mode, close_gap, exit_gap);

    // recv_timeout (not recv): between commands we poll the shim's async event queue so a delayed
    // fill / cancel / reject — and, after a ForexConnect reconnect, the re-surfaced trades (audit
    // A3) — reach the core. An empty queue makes poll_event return None, so this idles. (It said
    // "In the stub build poll_event returns None" until 2026-09-28; #1720 deleted the stub build —
    // a box with no shim never opens a session, and `refuse_every_command` answers there instead.)
    loop {
        match rx.recv_timeout(EVENT_POLL) {
            Ok(ExecCommand::Submit(req)) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                let instrument = to_fxcm_instrument(&req.symbol);
                let side = if req.side >= 0 { Side::Buy } else { Side::Sell };
                // THE SUBSCRIPTION. FXCM holds most of its Offers table as view-only or disabled, and only a `T` row
                // trades; the bridge makes the instrument tradable ITSELF the first time this session is asked to
                // trade it (`crate::subscription`; the owner's "any ticker of the venue"), bounded and never
                // repeated inside the memory of `Attempts`. A DEFINITE refusal ends the order here, in FXCM's
                // words. An UNKNOWN outcome (or a shim too old to ask) places the order anyway: the shim's own
                // offer lookup judges the row, so an instrument that is `T` trades exactly as it always did, and
                // the reason rides on the refusal if the row is not.
                //
                // ⚠ A CLOSE NEVER SUBSCRIBES (`reduce_only`): it must not pay a venue round trip, nor a refusal, on
                // the way out of a position (a flatten after a restart is the first order of its session).
                // A session that cannot subscribe at all (an older shim, a `Real` connection) is said ONCE, at
                // its start, and then neither asks nor repeats itself at every order.
                let (not_ensured, asked) = ensure_for_order(
                    req.reduce_only || req.order_type == OrderType::Stop,
                    cannot_subscribe.as_deref(),
                    &instrument,
                    &mut subscribed,
                    || session.subscribe_tradable(&instrument, crate::subscription::EXEC_BUDGET_MS),
                );
                if asked
                    && let Some(failure) = &not_ensured
                    && failure.transient
                {
                    tracing::warn!(
                        target: "vike_fxcm",
                        venue = "fxcm",
                        instrument = %instrument,
                        "{}; the order is placed anyway and the venue judges the instrument",
                        failure.why
                    );
                }
                // THE SIZING CONVERSION. `req.qty` is BASE UNITS (as it is at every other venue,
                // and as a fill's `last_qty` comes back); the shim places LOTS. So the instrument's
                // base unit size has to be read from the live session before the request can be
                // sized at all — it is per-instrument, per-account, and nothing static knows it.
                //
                // A session that cannot answer REFUSES the order. It must: the alternative is
                // assuming a multiplier, and this venue's whole sizing history is one silent
                // substitution after another. The refusal rides the same `Err` arm the preflight's
                // own refusals do, so it lands as a terminal `OrderRejected` like everything else.
                let refused_subscription =
                    not_ensured.as_ref().filter(|f| !f.transient).map(|f| f.why.clone());
                let base_unit = match (refused_subscription, base_units.get(&instrument).copied()) {
                    (Some(why), _) => Err(why),
                    (None, Some(b)) => Ok(b),
                    (None, None) => match session.base_unit_size(&instrument) {
                        Ok(b) => {
                            let b = f64::from(b);
                            base_units.insert(instrument.clone(), b);
                            Ok(b)
                        }
                        Err(e) => Err(format!(
                            "fxcm could not read the base unit size for {instrument} ({e}), so \
                             `qty` cannot be converted to a lot count. Refusing rather than \
                             assuming a multiplier"
                        )),
                    },
                };
                // REFUSE, before the shim sees anything, every request shape this adapter would
                // otherwise silently substitute — a size that is not a whole number of lots, and a
                // limit price that cannot be a rate. `preflight_request` owns both
                // arguments; a refusal becomes the same terminal `OrderRejected` a dead venue path
                // would synthesize, so the intent is refused VISIBLY rather than filled at a size
                // or price nobody chose.
                let sized = base_unit
                    .clone()
                    .and_then(|b| preflight_request(&req.order_type, req.qty, req.price, b));
                // The legs of a close, past the first: their order ids (routes) and what their fills owe (`closes`).
                let mut close_reg: Option<(Vec<String>, CloseTrack)> = None;
                let outcome = match sized {
                    Err(reason) => Err((false, reason, None)),
                    // The ONE place `order_type` is read for ROUTING. A market request goes to the
                    // shim's true-market placement (executes now); a limit that NAMES a price rests
                    // AT that rate (`fc_place_entry_at`); a limit with no price keeps the fixed
                    // resting entry at RESTING_PIPS, which is what the caps row declares for it.
                    // (`preflight_request` has already refused every other kind.) Before the split
                    // every submit rested at RESTING_PIPS regardless of the requested kind — the
                    // silent-substitution defect the `fxcm` caps row denied.
                    Ok(lots) => {
                        // Base units, the unit the venue's `Amount` is in: what a later lookup of a
                        // lost placement checks a candidate row against.
                        let amount =
                            lots.saturating_mul(base_unit.as_ref().map_or(0, |b| *b as i32));
                        // ⚠ A REDUCE-ONLY order on an account that may HEDGE does not go out as an opposite order: that
                        // opens a second trade (`crate::close`, which owns the rules). A market one closes the trades
                        // facing it by id; a priced limit and a stop are bound to the trade they protect
                        // (`crate::exit`); an unpriced limit, and a stop that is not reduce-only, are refused by name;
                        // a plain order and a netting account are exactly what they were.
                        let route = match Shape::of(&req.order_type, req.price) {
                            Some(shape) => close_desk.route(
                                &instrument,
                                shape,
                                req.reduce_only,
                                Instant::now(),
                            ),
                            None => {
                                Route::Refuse(format!("fxcm places no `{}` order", req.order_type))
                            }
                        };
                        match route {
                            Route::Refuse(why) => Err((false, why, None)),
                            Route::BindExit(kind) => {
                                match exit::exit_shape(
                                    &req.order_type,
                                    req.price,
                                    req.trigger_price,
                                    req.trigger_by,
                                ) {
                                    Err(why) => Err((false, why, None)),
                                    Ok((_, rate)) => exit::bind_exit(
                                        &instrument,
                                        side == Side::Buy,
                                        kind,
                                        i64::from(amount),
                                        || session.open_trades_json(),
                                        || session.open_exits_json(),
                                        |trade_id, units| {
                                            session.place_exit(trade_id, side, kind.code(), rate, units)
                                        },
                                    )
                                    .map(|bound| {
                                        if bound.units != i64::from(amount) {
                                            tracing::warn!(
                                                venue = "fxcm",
                                                coid = %req.client_order_id,
                                                trade = %bound.trade_id,
                                                asked = amount,
                                                covers = bound.units,
                                                "fxcm exit: the {} covers the one free trade there is, which holds \
                                                 less than the order asked; the engine goes on counting the size it \
                                                 asked for",
                                                kind.word()
                                            );
                                        }
                                        tracing::info!(
                                            venue = "fxcm",
                                            coid = %req.client_order_id,
                                            trade = %bound.trade_id,
                                            units = bound.units,
                                            rate,
                                            "fxcm exit: the {} is bound to the trade and rests at the venue",
                                            kind.word()
                                        );
                                        bound.placed
                                    }),
                                }
                            }
                            Route::CloseByTrade => {
                                let started = Instant::now();
                                let result = close::close_by_trade(
                                    &instrument,
                                    side == Side::Buy,
                                    i64::from(amount),
                                    || session.open_trades_json(),
                                    |trade_id, units| session.close_trade(trade_id, units),
                                    || started.elapsed().as_millis() < CLOSE_LEG_START_MS,
                                );
                                // A close whose reply was lost may have happened with nothing to route its fill: hold
                                // further reduce-only closes of this instrument (`close::UNKNOWN_CLOSE_HOLD`).
                                if close_desk.settle(&instrument, &result, Instant::now()) {
                                    tracing::warn!(
                                        target: "vike_fxcm",
                                        venue = "fxcm",
                                        instrument = %instrument,
                                        coid = %req.client_order_id,
                                        "fxcm close: a close was sent and its outcome is UNKNOWN; further reduce-only \
                                         closes of {instrument} are refused for {} s",
                                        close::UNKNOWN_CLOSE_HOLD.as_secs()
                                    );
                                }
                                result
                                .map(|done| {
                                    if let Some(why) = &done.stopped {
                                        tracing::warn!(
                                            venue = "fxcm",
                                            coid = %req.client_order_id,
                                            reason = %why,
                                            "fxcm close: only part of the close was sent; the order ends cancelled \
                                             after the fills of the legs that were"
                                        );
                                    }
                                    close_reg =
                                        Some((done.more_ids, CloseTrack::new(done.units, done.stopped)));
                                    done.first
                                })
                                .map_err(|(unknown, why)| (unknown, why, None))
                            }
                            Route::Place => {
                                let placement = if is_market(&req.order_type) {
                                    session.place_market(&instrument, side, lots, amount)
                                } else if let Some(px) = req.price {
                                    session.place_limit_at(&instrument, side, px, lots, amount)
                                } else {
                                    session.place_limit_entry(
                                        &instrument,
                                        side,
                                        RESTING_PIPS,
                                        lots,
                                        amount,
                                    )
                                };
                                // The `bool` is "SENT, outcome UNKNOWN" (see below); the third is what a
                                // later `confirm` needs to find the order, when the shim handed it back.
                                placement.map_err(|e| {
                                    let unknown = e.outcome_unknown();
                                    let text = e.to_string();
                                    (unknown, text, e.into_lookup())
                                })
                            }
                        }
                    }
                };
                // The reason the instrument could not be subscribed rides on the refusal of an order the venue then
                // refuses (an unknown outcome or a shim too old to ask let the order go to the venue).
                let outcome = with_subscription_note(not_ensured.as_ref(), outcome);
                let evs = match &outcome {
                    Ok(p) => map_placement(&req.client_order_id, req.ts, Ok(p.order_id.as_str())),
                    // ⚠ SENT, outcome UNKNOWN (no answer, or no order id, and the shim's Orders
                    // lookup found nothing either): NO terminal event. The order may rest on the
                    // venue, and an `OrderRejected` would leave a live order the book does not
                    // know. It stays Submitted for the stuck-order watchdog; reconcile reports it
                    // if it rests.
                    Err((true, reason, lookup)) => {
                        // Only a placement the shim handed a RequestID for can be looked up again.
                        let next = if lookup.is_some() {
                            "the watchdog's confirm will look the order up again"
                        } else {
                            "the shim returned no RequestID, so confirm CANNOT look it up: it is \
                             left to reconcile and the watchdog's last resort"
                        };
                        tracing::warn!(
                            venue = "fxcm",
                            coid = %req.client_order_id,
                            reason = %reason,
                            "fxcm placement outcome UNKNOWN: no terminal event emitted; {next}"
                        );
                        Vec::new()
                    }
                    Err((false, reason, _)) => {
                        map_placement(&req.client_order_id, req.ts, Err(reason.as_str()))
                    }
                };
                let mut echo = None;
                match outcome {
                    Ok(p) => {
                        // Route a LATER async fill for this venue order back to its coid.
                        routes.insert(p.order_id.clone(), req.client_order_id.clone());
                        // A close that sent several legs: every leg's fill routes to the same order.
                        if let Some((more_ids, track)) = close_reg.take() {
                            register_close(
                                &mut routes,
                                &mut closes,
                                &req.client_order_id,
                                more_ids,
                                track,
                            );
                        }
                        if !is_market(&req.order_type) {
                            echo = Some((p.order_id.clone(), p.account_id.clone(), p.rate));
                        }
                        placed.insert(req.client_order_id.clone(), p);
                    }
                    // Remember what was sent, for `confirm`.
                    Err((true, _, Some(lookup))) => unknown.insert(
                        req.client_order_id.clone(),
                        UnknownPlacement { lookup, ts: req.ts },
                    ),
                    Err(_) => {}
                }
                for ev in evs {
                    let _ = events.blocking_send(ev);
                }
                // AFTER the accept is out, so the venue read (a refresh round trip) never delays it.
                if let Some((order_id, account_id, sent)) = echo {
                    echo_venue_rate(
                        &session,
                        "place",
                        &req.client_order_id,
                        &order_id,
                        &account_id,
                        req.price.or(req.trigger_price).unwrap_or(sent),
                        sent,
                    );
                }
            }
            Ok(ExecCommand::Cancel { client_order_id: coid, .. }) => {
                // A failed/unknown cancel must not vanish (audit A2): map every outcome to an event.
                let outcome = if let Some(p) = placed.get(&coid) {
                    match session.delete_order(&p.order_id, &p.account_id, &p.offer_id) {
                        Ok(()) => CancelOutcome::Canceled,
                        Err(e) => CancelOutcome::Rejected(e.to_string()),
                    }
                } else {
                    CancelOutcome::Rejected(format!("no resting order for {coid}"))
                };
                if matches!(outcome, CancelOutcome::Canceled) {
                    // Both maps: a canceled order has no resting order left to cancel AND can never
                    // produce another async envelope, so its routing row is dead too.
                    if let Some(p) = placed.remove(&coid) {
                        routes.remove(&p.order_id);
                    }
                }
                let _ = events.blocking_send(cancel_event(&coid, outcome));
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            Ok(ExecCommand::CancelBatch { client_order_ids, .. }) => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            // Price-only amend of a resting entry (`EditOrder`, `fc_edit_order`). EVERY outcome is
            // one explicit event: `OrderModified` on success, else the NON-terminal
            // `OrderModifyRejected` (the resting order keeps its terms) — a size change, an order
            // this process did not place, an off-grid rate, a shim that predates ABI 2, and the
            // venue's own refusal all included.
            Ok(ExecCommand::Modify { order, new_qty, new_price }) => {
                let coid = order.client_order_id.as_str();
                let ts = vike_model::now_ms();
                // The ids of an amend the venue ACCEPTED, for the echo that follows the event.
                let mut echo = None;
                let ev = match plan_amend(order.qty, new_qty, new_price) {
                    Err(reason) => map_amend(coid, None, ts, 0.0, Err(reason.as_str())),
                    Ok(rate) => match placed.get(coid) {
                        None => {
                            let why = format!("no resting order for {coid}");
                            map_amend(coid, None, ts, rate, Err(why.as_str()))
                        }
                        Some(p) => {
                            let instrument = to_fxcm_instrument(&order.symbol);
                            let res: Result<(), String> = match session.edit_order_rate(
                                &p.order_id,
                                &p.account_id,
                                &instrument,
                                rate,
                            ) {
                                Ok(()) => {
                                    echo = Some((p.order_id.clone(), p.account_id.clone(), rate));
                                    Ok(())
                                }
                                // SENT, reply lost: the amend may have applied. Read the order back.
                                Err(e) if e.outcome_unknown() => settle_lost_amend(
                                    rate,
                                    &e.to_string(),
                                    || {
                                        session
                                            .order_rate(&p.order_id, &p.account_id)
                                            .map(|o| o.map(|v| (v.rate, v.tick)))
                                            .map_err(|e| e.to_string())
                                    },
                                    || std::thread::sleep(AMEND_REREAD_PAUSE),
                                ),
                                Err(e) => Err(e.to_string()),
                            };
                            // The event carries the rate that was REQUESTED: the venue accepted the
                            // EditOrder, and a read-back (which can take its whole refresh timeout)
                            // must not delay it. The echo below only logs.
                            map_amend(
                                coid,
                                Some(p.order_id.as_str()),
                                ts,
                                rate,
                                res.as_ref().map(|_| ()).map_err(String::as_str),
                            )
                        }
                    },
                };
                let _ = events.blocking_send(ev);
                // AFTER the event is out, as for a placement: info, and WARN past half a tick.
                if let Some((order_id, account_id, rate)) = echo {
                    echo_venue_rate(&session, "amend", coid, &order_id, &account_id, rate, rate);
                }
            }
            Ok(ExecCommand::Shutdown) => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Answer the watchdog's `confirm`s, here because this thread owns the session. The loop wakes
        // at least every EVENT_POLL, so a confirm waits at most that long behind an idle thread.
        serve_confirms(
            &confirms,
            CONFIRM_DRAIN_BUDGET,
            &mut unknown,
            &mut placed,
            &mut routes,
            |l| session.find_order(l).map_err(|e| e.to_string()),
            |ev| {
                let _ = events.blocking_send(ev);
            },
        );
        // Drain any async order events the shim queued (fills/terminals). No-op on an empty queue.
        drain_events(&session, &mut routes, &mut placed, &mut closes, &mut opens, &events);
    }
    // `session` drops here → fc_logout
}

/// Remember a close in progress. Bounded: a close whose fills never arrive (a lost reply, a venue that never reports it)
/// is not removed by anything else, so past [`MAX_CLOSES`] the table starts over - the fills of those closes then decode
/// as plain `OrderFilled`s, which is what they were before this table existed, and reconcile is what settles the rest.
/// Register a finished close that sent more than its first leg: EVERY further leg's closing order id routes to the
/// same client order id (without it that leg's fill is an envelope for an order this process "did not place" and is
/// dropped), and the order's tracker is remembered so the fills of its legs read as ONE order ([`fold_envelope`]).
/// The first leg's id is routed by the caller, like any placement's.
fn register_close(
    routes: &mut RouteTable,
    closes: &mut HashMap<String, CloseTrack>,
    coid: &str,
    more_ids: Vec<String>,
    track: CloseTrack,
) {
    for id in more_ids {
        routes.insert(id, coid.to_string());
    }
    insert_close(closes, coid.to_string(), track);
}

fn insert_close(closes: &mut HashMap<String, CloseTrack>, coid: String, track: CloseTrack) {
    if closes.len() >= MAX_CLOSES {
        tracing::warn!(
            venue = "fxcm",
            "fxcm close: {MAX_CLOSES} closes in progress were never completed; forgetting them"
        );
        closes.clear();
    }
    closes.insert(coid, track);
}

/// Answer the queued `confirm`s on the exec thread, which owns the session, for at most `budget` in
/// total (the rest stay queued for the next pass). Every found order's events go out through `emit`
/// from HERE, then the worker is acked: a worker that gave up waiting (its ack send fails) has lost
/// nothing, because the events never travelled through it.
fn serve_confirms(
    confirms: &Receiver<ConfirmRequest>,
    budget: Duration,
    unknown: &mut UnknownTable,
    placed: &mut HashMap<String, PlacedOrder>,
    routes: &mut RouteTable,
    mut find: impl FnMut(&PendingLookup) -> Result<Option<String>, String>,
    mut emit: impl FnMut(Event),
) {
    let start = Instant::now();
    while let Ok(req) = confirms.try_recv() {
        for ev in confirm_unknown(unknown, placed, routes, &req.coid, &mut find) {
            emit(ev);
        }
        let _ = req.reply.send(());
        if start.elapsed() >= budget {
            break;
        }
    }
}

/// Answer one `confirm` for `coid`: look a placement whose reply was lost up again (`find`, the
/// shim's `fc_find_order` over the terms it sent). Found exactly one -> the SAME `OrderAccepted` a
/// normal accept emits, with the cancel and fill-routing rows a normal accept creates. Anything else
/// (not in the table, ambiguous, the read failed, not a coid this process left unknown) -> nothing,
/// and the row STAYS for the watchdog's next confirm: an absence is not a refusal, the order may be
/// booked late, and a terminal here would strand a live one. A found order id that this process
/// already holds under a DIFFERENT coid (`routes` or `placed`: a RequestID reused across sessions)
/// is not this coid's order either: nothing, and the row stays. Pure over its closure.
fn confirm_unknown(
    unknown: &mut UnknownTable,
    placed: &mut HashMap<String, PlacedOrder>,
    routes: &mut RouteTable,
    coid: &str,
    find: impl FnOnce(&PendingLookup) -> Result<Option<String>, String>,
) -> Vec<Event> {
    let Some(u) = unknown.map.get(coid) else {
        return Vec::new();
    };
    match find(&u.lookup) {
        Ok(Some(order_id)) => {
            let owner = routes
                .routes()
                .get(&order_id)
                .or_else(|| placed.iter().find(|(_, p)| p.order_id == order_id).map(|(c, _)| c))
                .filter(|c| c.as_str() != coid)
                .cloned();
            if let Some(owner) = owner {
                tracing::warn!(
                    venue = "fxcm",
                    coid,
                    order_id = %order_id,
                    owner = %owner,
                    "confirm: the lookup found an order id this session already holds for another \
                     client order id (a stale RequestID collision): not accepted for this one"
                );
                return Vec::new();
            }
            let Some(u) = unknown.remove(coid) else {
                return Vec::new();
            };
            routes.insert(order_id.clone(), coid.to_string());
            let evs = map_placement(coid, u.ts, Ok(order_id.as_str()));
            placed.insert(
                coid.to_string(),
                PlacedOrder {
                    order_id,
                    offer_id: u.lookup.offer_id,
                    account_id: u.lookup.account_id,
                    rate: u.lookup.rate,
                },
            );
            evs
        }
        Ok(None) => {
            tracing::warn!(
                venue = "fxcm",
                coid,
                "confirm: a fresh Orders table does not hold the placement whose reply was lost \
                 (it never rested, or it was a market order that filled and left the table): left \
                 to reconcile and the watchdog"
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(
                venue = "fxcm", coid, error = %e,
                "confirm: could not look the placement whose reply was lost up again"
            );
            Vec::new()
        }
    }
}

/// Settle an amend whose reply was lost by reading the order back, and READ ONCE MORE after a short
/// `pause` when the first read does not show the requested rate: one stale read proves nothing, the
/// broker may apply the edit later. `read` is the `(rate, tick)` read-back. Bounded: two reads and
/// one pause. The verdict is [`resolve_unknown_amend`]'s.
fn settle_lost_amend(
    rate: f64,
    why: &str,
    mut read: impl FnMut() -> Result<Option<(f64, f64)>, String>,
    pause: impl FnOnce(),
) -> Result<(), String> {
    let mut current = read();
    if amend_needs_reread(rate, &current) {
        pause();
        current = read();
    }
    resolve_unknown_amend(rate, why, current).map(|_| ())
}

/// Read the rate the VENUE holds for `order_id` and log it next to what was asked for and what the
/// shim sent: info always, WARN past half a tick. A failed or unavailable read (a shim without
/// `fc_order_rate`) is logged and never touches the order - the echo is evidence, not a gate. Bounded
/// in the shim (`ECHO_REFRESH_MS`), so a slow demo cannot stall a queued Cancel for long.
fn echo_venue_rate(
    session: &FxcmSession,
    what: &str,
    coid: &str,
    order_id: &str,
    account_id: &str,
    requested: f64,
    sent: f64,
) {
    match session.order_rate(order_id, account_id) {
        Ok(Some(v)) => {
            tracing::info!(
                venue = "fxcm",
                what,
                coid,
                order_id,
                requested,
                sent,
                venue_rate = v.rate,
                "fxcm order rate echo"
            );
            if venue_rate_differs(requested, v.rate, v.tick) {
                tracing::warn!(
                    venue = "fxcm",
                    what,
                    coid,
                    order_id,
                    requested,
                    sent,
                    venue_rate = v.rate,
                    tick = v.tick,
                    "fxcm order does NOT rest at the requested rate"
                );
            }
        }
        Ok(None) => {
            tracing::debug!(
                venue = "fxcm",
                what,
                coid,
                order_id,
                "the installed shim has no fc_order_rate: reinstall it to echo the venue rate"
            );
        }
        Err(e) => {
            tracing::warn!(
                venue = "fxcm", what, coid, order_id, requested, error = %e,
                "could not read the venue rate of the order"
            );
        }
    }
}

/// Drain the shim's async order-event queue until empty, mapping each event to canonical events
/// (dual-publish fill / cancel / reject), PRUNING the session's two bookkeeping maps, and pushing
/// the events to the core. Errors (incl. the stub's absence of a queue) stop this tick's drain. The
/// core dedups replayed trades by trade_id after a ForexConnect reconnect (A3).
///
/// The pruning is deliberately ASYMMETRIC — see `closes_routing` / `ends_cancelability`, which own
/// the argument. Before it existed neither map ever shrank on a fill, so a long-lived session grew
/// one permanent row per accepted order in each.
fn drain_events(
    session: &FxcmSession,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    closes: &mut HashMap<String, CloseTrack>,
    opens: &mut OpenCommissions,
    events: &EventSender,
) {
    loop {
        let json = match session.poll_event() {
            Ok(Some(j)) => j,
            Ok(None) => return, // queue empty
            Err(_) => return,   // native error — nothing to drain (no shim, no session: never here)
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
            continue;
        };
        for ev in fold_drained(v, routes, placed, closes, opens) {
            if events.blocking_send(ev).is_err() {
                return; // core gone
            }
        }
    }
}

/// [`fold_envelope`] after the commission is settled: an opening fill's figure is remembered and a closing fill's is
/// replaced by what the close still owes ([`OpenCommissions::settle`]). The one place `drain_events` and the tests share.
fn fold_drained(
    mut v: serde_json::Value,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    closes: &mut HashMap<String, CloseTrack>,
    opens: &mut OpenCommissions,
) -> Vec<Event> {
    opens.settle(&mut v);
    fold_envelope(&v, routes, placed, closes)
}

/// Everything [`drain_events`] does with ONE decoded shim envelope short of sending: route it to its client order id and
/// decode it, read the fills of a CLOSE as one order across its legs (a partial fill until the last,
/// [`CloseTrack::relabel`]), and prune the two bookkeeping maps. Pure over its arguments, so a scripted envelope drives
/// the whole wiring in a test (`exec_tests.rs`'s close tests); `drain_events` is this plus the session and the sender.
fn fold_envelope(
    v: &serde_json::Value,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    closes: &mut HashMap<String, CloseTrack>,
) -> Vec<Event> {
    // Routing + decode, both pure; an envelope for an order this process never placed publishes
    // nothing and says so at `warn!` (the declared restart hole).
    let evs = map_drained_event(v, routes.routes());
    // A fill of a CLOSE reads as one order across its legs: a partial fill until the last one.
    let evs = match coid_of(&evs).map(str::to_owned) {
        Some(coid) => match closes.get_mut(&coid) {
            Some(track) => {
                let (evs, done) = track.relabel(evs);
                if done {
                    closes.remove(&coid);
                }
                evs
            }
            None => evs,
        },
        None => evs,
    };
    if ends_cancelability(&evs)
        && let Some(coid) = coid_of(&evs)
    {
        placed.remove(coid);
    }
    if closes_routing(&evs) {
        let oid = v.get("order_id").and_then(|x| x.as_str()).unwrap_or_default();
        routes.remove(oid);
    }
    evs
}

/// The client order id a decoded envelope's events carry (they all carry the same one — the mapper
/// stamps the routed coid onto every event it emits).
fn coid_of(evs: &[Event]) -> Option<&str> {
    evs.iter().find_map(|e| match e {
        Event::OrderFilled(w) => Some(w.client_order_id.as_str()),
        Event::OrderCanceled(c) => Some(c.client_order_id.as_str()),
        Event::OrderRejected(r) => Some(r.client_order_id.as_str()),
        // every other event: not this tap's business (audited 2026-10, I-21)
        _ => None,
    })
}

#[path = "exec_tests.rs"]
#[cfg(test)]
mod exec_tests;

#[path = "exec_props.rs"]
#[cfg(test)]
mod exec_props;

#[path = "exec_commission_tests.rs"]
#[cfg(test)]
mod exec_commission_tests;
