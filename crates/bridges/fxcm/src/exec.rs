//! The dedicated-thread bridge + `ExecutionClient` impl. See the module docs.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use super::event_mapper::{
    closes_routing, ends_cancelability, is_market, map_drained_event, map_placement,
    preflight_request,
};
use super::sys::{FxcmSession, PlacedOrder, Side};
use vike_bridge_core::exec_actor::{
    CancelOutcome, ExecActor, ExecCommand, cancel_batch_undeclared, cancel_event,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderSubmitted};

use super::config::FxcmConfig;

/// Cadence for polling the shim's async order-event queue between commands (audit A3 fill lane).
const EVENT_POLL: Duration = Duration::from_millis(100);

/// Resting distance (pips) for the limit-entry the current shim places. The shim computes the
/// actual rate from the live quote; a request that NAMES a limit price is refused by
/// [`preflight_request`] rather than rested here at a price nobody asked for.
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
    /// Hand this client the HALT sentinel it watches: the venue's mount passes
    /// `MountInputs::process.halt_path`, the one file `vike-mount` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live mount calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path))
    }

    /// Spawn the session thread: it logs in with `config` and pumps venue events into `events`.
    ///
    /// ⚠ **When the login FAILS the thread stays alive and REFUSES every command with the reason**
    /// ([`refuse_every_command`]) — it does not exit. That is a behaviour change and it is the half
    /// of "a login failure says why" that an operator actually meets: this doc used to read "the
    /// thread exits immediately and later submits are no-ops", and those no-ops were a silent
    /// vanish the venue-adapter contract forbids. No session still means no orders reach FXCM; what
    /// changed is that the intent comes back REJECTED, naming the failure, instead of producing
    /// nothing at all.
    pub fn spawn(config: FxcmConfig, events: EventSender) -> Self {
        Self(ExecActor::spawn("fxcm-session", events.clone(), move |rx| run(config, events, rx)))
    }
}

impl ExecutionClient for FxcmExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.0.submit(request)
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.0.cancel(client_order_id)
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
fn to_fxcm_instrument(symbol: &str) -> String {
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

/// Answer every command with `reason`, for a mount whose session never opened.
///
/// ⚠ **This arm used to be `Err(_) => return`, and the two things it dropped were the error text
/// AND the orders.** Returning closed the command channel, so a submit that lost the race with the
/// thread's exit was simply discarded — no `OrderSubmitted`, no terminal, nothing — which is the
/// silent vanish the venue-adapter contract forbids, and the 45 seconds of nothing
/// `crates/bridges/fxcm/tests/fxcm_live_smoke.rs` reported as one mute timeout. (The other half of
/// the race reached `vike_bridge_core::exec_actor`'s dead-channel backstop, which synthesizes a
/// terminal reason of its own — "venue exec thread unavailable" — that says nothing about FXCM.)
///
/// The shape is the emitter split the contract requires: `OrderSubmitted` synchronously, then the
/// SAME [`map_placement`] refusal the preflight's own rejections take, so a dead session's reject
/// is byte-identical in shape to every other one this adapter emits.
fn refuse_every_command(events: &EventSender, rx: Receiver<ExecCommand>, reason: &str) {
    while let Ok(cmd) = rx.recv() {
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
            // no native amend on this venue — the same no-op the live loop takes
            ExecCommand::Modify { .. } => {}
            ExecCommand::Shutdown => break,
        }
    }
}

fn run(config: FxcmConfig, events: EventSender, rx: Receiver<ExecCommand>) {
    let session =
        match FxcmSession::login(&config.user, &config.password, &config.url, &config.connection) {
            Ok(s) => s,
            Err(e) => {
                // The reason carries a MARKER prefix so a consumer can tell a dead SESSION from a
                // venue reject. `crates/bridges/fxcm/tests/fxcm_live_smoke.rs`'s `OrderRejected`
                // arm treats a venue reject as acceptable ("market closed") and passes — so
                // without the marker this very fix would deliver a dead demo account into that arm
                // and read GREEN, which is the defect it exists to close wearing a new costume.
                let reason = format!("{}: {e}", crate::SESSION_UNAVAILABLE);
                tracing::error!(
                    target: "vike_fxcm",
                    venue = "fxcm",
                    reason = %reason,
                    "fxcm: the ForexConnect session did not open — every command this mount \
                     receives will be REJECTED with this reason until the process restarts"
                );
                refuse_every_command(&events, rx, &reason);
                return;
            }
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
                // THE SIZING CONVERSION. `req.qty` is BASE UNITS (as it is at every other venue,
                // and as a fill's `last_qty` comes back); the shim places LOTS. So the instrument's
                // base unit size has to be read from the live session before the request can be
                // sized at all — it is per-instrument, per-account, and nothing static knows it.
                //
                // A session that cannot answer REFUSES the order. It must: the alternative is
                // assuming a multiplier, and this venue's whole sizing history is one silent
                // substitution after another. The refusal rides the same `Err` arm the preflight's
                // own refusals do, so it lands as a terminal `OrderRejected` like everything else.
                let base_unit = match base_units.get(&instrument).copied() {
                    Some(b) => Ok(b),
                    None => match session.base_unit_size(&instrument) {
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
                // limit carrying a price the shim cannot honor. `preflight_request` owns both
                // arguments; a refusal becomes the same terminal `OrderRejected` a dead venue path
                // would synthesize, so the intent is refused VISIBLY rather than filled at a size
                // or price nobody chose.
                let sized = base_unit
                    .and_then(|b| preflight_request(&req.order_type, req.qty, req.price, b));
                let outcome = match sized {
                    Err(reason) => Err(reason),
                    // The ONE place `order_type` is read for ROUTING. A market request goes to the
                    // shim's true-market placement (executes now); everything else keeps the fixed
                    // resting LIMIT entry. Before this split every submit rested at RESTING_PIPS
                    // regardless of the requested kind — the silent-substitution defect the `fxcm`
                    // caps row denied, and the same class as the two refusals above.
                    Ok(lots) => {
                        let placement = if is_market(&req.order_type) {
                            session.place_market(&instrument, side, lots)
                        } else {
                            session.place_limit_entry(&instrument, side, RESTING_PIPS, lots)
                        };
                        placement.map_err(|e| e.to_string())
                    }
                };
                let evs = match &outcome {
                    Ok(p) => map_placement(&req.client_order_id, req.ts, Ok(p.order_id.as_str())),
                    Err(reason) => {
                        map_placement(&req.client_order_id, req.ts, Err(reason.as_str()))
                    }
                };
                if let Ok(p) = outcome {
                    // Route a LATER async fill for this venue order back to its coid.
                    routes.insert(p.order_id.clone(), req.client_order_id.clone());
                    placed.insert(req.client_order_id.clone(), p);
                }
                for ev in evs {
                    let _ = events.blocking_send(ev);
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
            // no native amend on this venue: a modify leaves the resting order at its terms
            Ok(ExecCommand::Modify { .. }) => {}
            Ok(ExecCommand::Shutdown) => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Drain any async order events the shim queued (fills/terminals). No-op on an empty queue.
        drain_events(&session, &mut routes, &mut placed, &events);
    }
    // `session` drops here → fc_logout
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
        // Routing + decode, both pure; an envelope for an order this process never placed publishes
        // nothing and says so at `warn!` (the declared restart hole).
        let evs = map_drained_event(&v, routes.routes());
        if ends_cancelability(&evs)
            && let Some(coid) = coid_of(&evs)
        {
            placed.remove(coid);
        }
        if closes_routing(&evs) {
            let oid = v.get("order_id").and_then(|x| x.as_str()).unwrap_or_default();
            routes.remove(oid);
        }
        for ev in evs {
            if events.blocking_send(ev).is_err() {
                return; // core gone
            }
        }
    }
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
