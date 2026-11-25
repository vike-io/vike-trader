//! The dedicated-thread bridge: the session thread behind the shared `ExecutionClientActor`. See the module docs.

// Closing a trade by its id on a HEDGING account (a reduce-only market order must not open the opposite trade): the pure
// routing, planner and fill tracker. Crate-private: the exec thread is its only caller.
pub(crate) mod close;
// What a CLOSING fill owes in commission, from what the opening fill already booked. Crate-private: the exec thread's drain.
pub(crate) mod commission;
// PUBLIC because `crates/vike-bridge-core/tests/bridge_conformance.rs` drives it as the fxcm
// `ConformanceBridge`: `event_mapper` is the crate's whole PURE layer — the stateless
// shim-envelope decode plus the request preflight and the placement mapper lifted out of [`execution`].
pub mod event_mapper;
// A STOP-LOSS and a TAKE-PROFIT on a HEDGING account (a reduce-only stop or limit is bound to ONE open trade, because the
// plain resting order is an entry that OPENS a trade): the pure binder. Crate-private: the exec thread is its only caller.
pub(crate) mod exit;
// PUBLIC for one function, `funding::rollover_funding`: the grammar gate ablates the shim's `rollover` key through it.
// The rest is the exec drain's: the realized rollover of a closed trade, booked as funding (A26).
pub mod funding;
// Which of the venue's orders are ours, by venue order id (the reconcile listing names our order client id with it).
pub(crate) mod registry;
// Which trade a partially closed trade's remainder really is (the reconcile fill report and the exits follow the chain).
pub(crate) mod trade_roots;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use crate::execution::close::{CloseDesk, CloseTracker, Route};
use crate::execution::commission::OpenCommissions;
use crate::execution::event_mapper::{
    amend_needs_reread, closes_routing, ends_cancelability, is_market, kind_refusal, map_amend,
    map_drained_event, map_placement, plan_amend, preflight_request, resolve_unknown_amend,
    venue_rate_differs,
};
use crate::execution::exit::group::{ExitGroup, Want};
use crate::execution::exit::{Ask, ExitLeg, ExitVenue};
use crate::execution::funding::{RolloverBooked, stated_account_currency};
use crate::transport::sys::{FxcmError, FxcmSession, OrderSideWire, PendingLookup, PlacedOrder};
use vike_bridge_core::exec_actor::{
    ConfirmFn, ExecutionClientActor, ExecutionClientActorCancelOutcome,
    ExecutionClientActorCommand, ExecutionClientActorLateCloses, Sending, cancel_batch_undeclared,
    cancel_event, late_close_size, sending, too_old_to_amend, too_old_to_send,
};
use vike_bridge_core::execution_client::hooks::AdoptRestoredFn;
use vike_bridge_core::execution_client::order_client_id::FifoMap;
use vike_bridge_core::exit::ExitKind;
use vike_bridge_core::link_gap::GapWindow;
use vike_bridge_core::rest::{OrderCancelAck, amend_answer_time};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_bridge_core::venue_adapter::refuse_command;
use vike_execution::{EventSender, TradeLinkUpdate};
use vike_model::events::{Event, OrderCanceled, OrderModifyRejected, OrderSubmitted};
use vike_model::{OrderRefusalId, OrderRequest, OrderType};

use super::config::FxcmConfig;
use crate::account::venue_link::{FRESH, StatusChanges, from_status, login_failed, say};
use crate::reconcile::redial::{
    CREDENTIALS_REFUSED, one_retry_at_a_time, pause_to_a_login, refuses_credentials,
};
use vike_bridge_core::venue_link::VenueLink;

/// Cadence for polling the shim's async order-event queue between commands (audit A3 fill lane).
const EVENT_POLL: Duration = Duration::from_millis(100);

/// Resting distance (pips) for a limit that names NO price: the shim computes the rate from the live
/// quote (`forex_connect_place_entry`). A limit that names a price never uses it — it rests at that rate
/// (`forex_connect_place_entry_at`).
const RESTING_PIPS: i32 = 50;

/// How many venue-order-id → order-client-id routing rows the session keeps.
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
/// the first time ([`crate::instrument::subscription::EXECUTION_BUDGET_MS`]); an amend = 10 s edit + 5 s read-back +
/// [`AMEND_REREAD_PAUSE`] + 5 s read-back; both followed by the 5 s rate echo.
const COMMAND_WORST_MS: u128 =
    PLACEMENT_WORST_MS + crate::instrument::subscription::EXECUTION_BUDGET_MS as u128 + 5_000;
/// One placement alone, from `fcshim.cpp`'s bounds (`ORDER_WAIT_MS` + the two pre-send refreshes + `LOOKUP_REFRESH_MS`).
const PLACEMENT_WORST_MS: u128 = 20_000;
// The default of `guards.submit_ack_timeout_ms` (`vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT`, 30 s): the stuck-order
// watchdog's confirm falls this long after a submit, and a FIRST order on an instrument may spend the subscription budget
// before its placement. That sum must stay under it, and it does by 2 s: raise `EXECUTION_BUDGET_MS` and this fails the
// build, not the watchdog.
const _: () = assert!(
    PLACEMENT_WORST_MS + (crate::instrument::subscription::EXECUTION_BUDGET_MS as u128)
        < vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT.as_millis(),
    "a first order's subscription plus its placement must fit under the default ack timeout"
);
/// One closing leg alone, from `fcshim.cpp`'s bounds (`forex_connect_close_trade`: `PRESEND_REFRESH_MS` + `ORDER_WAIT_MS`).
const CLOSE_LEG_WORST_MS: u128 = 12_500;
/// A close sends one order per trade it closes and starts another only while the elapsed time is under this, so the
/// whole command (the open-trades read of 2.5 s counted in the elapsed time, the last leg at its worst) ends inside the
/// default ack timeout, and the legs left over are named in the order's terminal ([`close::Closed::stopped`]).
const CLOSE_LEG_START_MS: u128 = 15_000;
const _: () = assert!(
    CLOSE_LEG_START_MS + CLOSE_LEG_WORST_MS < vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT.as_millis(),
    "a close's last leg must end under the default ack timeout"
);
/// One trade-bound exit alone, from `fcshim.cpp`'s bounds: the open-trades read and the exits read (`PRESEND_REFRESH_MS`
/// each) in front of `forex_connect_place_exit` (its own trades refresh, `ORDER_WAIT_MS`, and the one `LOOKUP_REFRESH_MS` lookup of a
/// reply that was lost). Nothing is subscribed first (an exit is reduce-only), and the rate echo that follows is the 5 s
/// `ECHO_REFRESH_MS`, so the whole command fits [`COMMAND_WORST_MS`] and ends inside the default ack timeout.
const EXIT_WORST_MS: u128 = 2_500 + 2_500 + 2_500 + 10_000 + 5_000;
const _: () = assert!(
    EXIT_WORST_MS + 5_000 <= COMMAND_WORST_MS
        && EXIT_WORST_MS < vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT.as_millis(),
    "an exit's reads, placement and lookup must end under the default ack timeout and inside the command bound"
);
/// One venue order of an exit that spans trades, alone, from `fcshim.cpp`'s bounds (`forex_connect_place_exit`: `PRESEND_REFRESH_MS`,
/// `ORDER_WAIT_MS` and the one `LOOKUP_REFRESH_MS` lookup of a reply that was lost).
const EXIT_LEG_WORST_MS: u128 = 2_500 + 10_000 + 5_000;
/// An exit that spans trades places (or amends, or cancels) one venue order per trade and starts another only while the
/// command has run under this (the two reads that open it are counted), so the last placement ends inside the default ack
/// timeout and the legs left over are named in the order's refusal.
const EXIT_LEG_START_MS: u128 = 12_000;
const _: () = assert!(
    EXIT_LEG_START_MS + EXIT_LEG_WORST_MS < vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT.as_millis(),
    "the last leg of an exit that spans trades must end under the default ack timeout"
);
/// How many closes in progress the session remembers (see [`insert_close`]).
const MAX_CLOSES: usize = 1024;
const AMEND_WORST_MS: u128 = 10_000 + 5_000 + AMEND_REREAD_PAUSE.as_millis() + 5_000 + 5_000;
/// One `forex_connect_find_order` (the shim's `LOOKUP_REFRESH_MS`).
const LOOKUP_WORST_MS: u128 = 5_000;
const _: () = assert!(
    COMMAND_WORST_MS + LOOKUP_WORST_MS < CONFIRM_REPLY_WAIT.as_millis()
        && AMEND_WORST_MS + LOOKUP_WORST_MS < CONFIRM_REPLY_WAIT.as_millis(),
    "CONFIRM_REPLY_WAIT must outlast the slowest command plus one lookup"
);

/// The total time one pass over the confirm lane may spend, so a burst of confirms (each a bounded
/// `forex_connect_find_order`) cannot starve the commands and the event drain. Past it the rest stay queued for
/// the next pass of the loop, never dropped.
const CONFIRM_DRAIN_BUDGET: Duration = Duration::from_secs(10);
const _: () = assert!(
    COMMAND_WORST_MS + CONFIRM_DRAIN_BUDGET.as_millis() < FRESH.as_millis(),
    "a busy exec thread must not read as a stuck one: the session's freshness outlasts one command and a confirm pass"
);

/// A placement whose reply was lost: its terms for `forex_connect_find_order`, and the request's own timestamp
/// for the `OrderAccepted` a successful `confirm` emits.
struct UnknownPlacement {
    lookup: PendingLookup,
    timestamp: i64,
}

/// The placements left UNKNOWN, order client id -> terms, bounded FIFO (the shared [`FifoMap`], see [`MAX_UNKNOWN`]):
/// resolved rows are removed, and past the cap the OLDEST goes (age of the loss is the only signal; an old unknown
/// is the least likely to still be findable).
type UnknownTable = FifoMap<UnknownPlacement, MAX_UNKNOWN>;

/// One `confirm` for the exec thread to answer: the order client id, and where to ACK. The events are sent by
/// the exec thread itself (never through this reply): a worker that already gave up on the wait must
/// not take an `OrderAccepted` down with it.
struct ConfirmRequest {
    order_client_id: String,
    reply: Sender<()>,
}

/// The `confirm` worker's side: hand the order client id to the exec thread (the one owner of the session) and
/// wait, bounded, for its ack. A gone exec thread, or no ack, returns at once or at the bound; either
/// way the exec thread emits whatever it found, so there is never a `Vec` to carry back.
fn ask_execution_thread(tx: &Sender<ConfirmRequest>, order_client_id: &str) {
    let (reply, ack) = channel();
    if tx.send(ConfirmRequest { order_client_id: order_client_id.to_string(), reply }).is_ok() {
        let _ = ack.recv_timeout(CONFIRM_REPLY_WAIT);
    }
}

/// Venue order id → order client id, bounded FIFO (the shared [`FifoMap`], see [`MAX_ROUTES`]).
///
/// Insertion-ordered eviction, not LRU: the question a route answers is "did THIS process place
/// this order", and recency of USE says nothing about that — an order that has been resting
/// untouched for hours is exactly as live as one that just filled. Age of PLACEMENT is the only
/// signal available, so it is the one used.
#[derive(Default)]
struct RouteTable {
    map: FifoMap<String, MAX_ROUTES>,
    /// The routed ids a PREVIOUS process placed ([`Self::insert_taught`]): routed like any other, but not this process's
    /// own, so an exit among them is still one a new exit of its rate may take over (`crate::execution::exit`'s `takeable`).
    taught: HashSet<String>,
}

impl RouteTable {
    /// Route `venue_order_id` to `order_client_id`, and bind it where the reconcile listing reads it ([`crate::execution::registry`]), so
    /// the venue's row of this order names our order client id.
    fn insert(&mut self, venue_order_id: String, order_client_id: String) {
        crate::execution::registry::process().bind(&venue_order_id, &order_client_id);
        self.taught.remove(&venue_order_id);
        self.map.insert(venue_order_id, order_client_id);
    }

    /// Route an order a previous process placed, as the core re-taught it after a restart (`adopt_taught`).
    fn insert_taught(&mut self, venue_order_id: String, order_client_id: String) {
        self.insert(venue_order_id.clone(), order_client_id);
        self.taught.insert(venue_order_id);
    }

    /// Drop the row for `venue_order_id` (from the map and its queue: [`FifoMap::remove`] says why the scan), and its
    /// binding.
    fn remove(&mut self, venue_order_id: &str) {
        crate::execution::registry::process().forget(venue_order_id);
        self.taught.remove(venue_order_id);
        self.map.remove(venue_order_id);
    }

    fn routes(&self) -> &HashMap<String, String> {
        self.map.as_map()
    }

    /// Whether THIS process placed `venue_order_id` (a placement's, a leg's or a takeover's answer routed it), the
    /// question an exit's takeover asks. A re-taught route is not: it is still a previous process's order, unless
    /// [`adopt_taught`] rebuilt it as an exit of the order that taught it (a second order client id must not take that one over).
    fn placed_here(&self, venue_order_id: &str) -> bool {
        self.map.as_map().contains_key(venue_order_id) && !self.taught.contains(venue_order_id)
    }
}

/// **The live FXCM exec client**: the shared [`ExecutionClientActor`] over the session thread, with this venue's two hooks.
/// `submit`/`cancel` enqueue onto the session thread (non-blocking); every venue event returns through the core ingest.
/// Dropping it stops the thread (logout). The start hands it the HALT sentinel with `ExecutionClientActor::with_halt_path`.
///
/// ⚠ **When the login FAILS the thread stays alive and REFUSES every command with the reason**
/// ([`refuse_every_command`]) — it does not exit — **and logs in again** after a growing pause
/// (30 s, 1 min, 2 min, 5 min, then every 10 min: `crate::reconcile::redial::pause_after`, held to the FX open while
/// the FX market is closed by `crate::reconcile::redial::pause_to_a_login`), for the life of
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
///
/// `ExecutionClient::adopt_restored` is the actor's [`adopt_restored_hook`].
pub fn spawn_execution_client(config: FxcmConfig, events: EventSender) -> ExecutionClientActor {
    spawn(config, events, VenueLink::default(), false)
}

/// [`spawn_execution_client`] whose session states itself on `link` (the account's
/// `vike_bridge_core::venue_link::venue_link`, in [`crate::account::venue_link`]'s words) and announces to its HOST that it
/// has learned the venue once its session opens ([`announce_learned`]): what a venue start passes, so the gatewayhub's
/// status verb can say whether the ForexConnect session is up and its supervised rebuild is not held. The cell is CLAIMED
/// here (`VenueLink::claim`): what an earlier client of the account said is history, and what it says from now on (an exec
/// thread that outlived the gatewayhub's bounded join) lands nowhere.
pub(crate) fn spawn_execution_client_reporting(
    config: FxcmConfig,
    events: EventSender,
    link: VenueLink,
) -> ExecutionClientActor {
    spawn(config, events, link, true)
}

/// The two spawns' one body: `hosted` is a start's (the gatewayhub's) client, which announces `ExecLink{up:true}`; the front
/// door's callers (the smokes) read venue events only.
fn spawn(
    config: FxcmConfig,
    events: EventSender,
    link: VenueLink,
    hosted: bool,
) -> ExecutionClientActor {
    let link = link.claim();
    let (confirm_tx, confirm_rx) = channel::<ConfirmRequest>();
    let (taught_tx, taught_rx) = channel();
    // The exec thread sends the events itself; this closure only waits for its ack.
    let confirm: ConfirmFn = Arc::new(move |order_client_id: &str| {
        ask_execution_thread(&confirm_tx, order_client_id);
        Vec::new()
    });
    ExecutionClientActor::spawn("fxcm-session", events.clone(), move |rx| {
        run(config, events, rx, confirm_rx, taught_rx, &link, hosted)
    })
    .with_confirm(confirm)
    .with_adopt_restored(adopt_restored_hook(taught_tx))
}

/// **The orders a previous process placed** (a gatewayhub restart's re-teaching, `ExecutionClient::adopt_restored`):
/// ForexConnect echoes no order client id, so the venue order id the core holds is the only way back to our order client id. The
/// reconcile listing's binding is made HERE, at once (the re-teaching asks for a reconcile pass right after it, and the
/// session thread may be inside a command); the session thread's routes follow on its next pass over `taught`
/// ([`adopt_taught`]). An order taught with no venue id teaches nothing. Returns at once (a table write and a channel send).
fn adopt_restored_hook(taught: Sender<Vec<vike_execution::RestoredOrder>>) -> AdoptRestoredFn {
    Arc::new(move |orders: &[vike_execution::RestoredOrder]| {
        let restored = bind_restored(orders, &mut crate::execution::registry::process());
        if !restored.is_empty() {
            // A session thread that has ended routes nothing any more; the listing's binding still names the order.
            let _ = taught.send(restored);
        }
    })
}

/// **The orders a previous process placed, as the session thread takes them over** (`ExecutionClient::adopt_restored`): each
/// `venue order id -> order_client_id` the core taught joins the route table like a placement's answer, which also binds it where
/// the reconcile listing reads it ([`crate::execution::registry`]). So after a gatewayhub restart a pre-restart order lists under
/// our order client id again, and its fills and its removal by the venue route to it. What a cancel and an amend need is rebuilt
/// into `placed` (and `exits`) for an order this thread does not hold yet:
///
/// * a trade-bound EXIT, from the venue's listing (`read_exits`: the open trades and the resting exits,
///   [`ExitGroup::restored`]);
/// * a resting ENTRY (`LE`, an opening limit: an order the exits read does not list), from the login's account id and the
///   Offers row of the order's own symbol (`read_entry`, [`restored_entry`]), read only when there is one.
///
/// An order that cannot be rebuilt (a read failed, no row, no offer id, or the exits read failed so it is not known
/// which it is) is remembered in `unbuilt` with the reason, so its cancel and its amend are refused in those words
/// ([`nothing_to_act_on`]), never with a bare "no resting order".
///
/// Run by the session thread before each command, so a cancel the core sends after the teaching finds it; the reads
/// are paid only when a teaching came. What an order's own events later prune is pruned as for any placement.
fn adopt_taught(
    taught: &Receiver<Vec<vike_execution::RestoredOrder>>,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    exits: &mut HashMap<String, ExitGroup>,
    unbuilt: &mut HashMap<String, String>,
    read_exits: impl FnOnce() -> Result<(Vec<close::OpenTrade>, Vec<exit::OpenExit>), String>,
    read_entry: impl FnOnce() -> Result<(String, Vec<crate::account::quote_rate::Offer>), String>,
) {
    let orders: Vec<(String, vike_execution::RestoredOrder)> = taught
        .try_iter()
        .flatten()
        .filter_map(|o| Some((o.venue_order_id.clone().filter(|v| !v.is_empty())?, o)))
        .collect();
    for (venue_order_id, o) in &orders {
        routes.insert_taught(venue_order_id.clone(), o.order_client_id.clone());
    }
    // A teaching may come again: an order this thread already holds is left as it is.
    let fresh: Vec<(String, vike_execution::RestoredOrder)> = orders
        .into_iter()
        .filter(|(_, o)| {
            !placed.contains_key(&o.order_client_id) && !exits.contains_key(&o.order_client_id)
        })
        .collect();
    if fresh.is_empty() {
        return;
    }
    let (trades, open) = match read_exits() {
        Ok(book) => book,
        Err(why) => {
            let why = format!(
                "the venue's exits could not be read, so it is not known whether it is an exit or an entry: {why}"
            );
            for (venue_order_id, o) in fresh {
                not_rebuilt(unbuilt, &o.order_client_id, &venue_order_id, why.clone());
            }
            return;
        }
    };
    let claimed = |oid: &str| routes.routes().contains_key(oid);
    let (mut rebuilt, mut entries) = (Vec::new(), Vec::new());
    for (venue_order_id, o) in fresh {
        let Some(group) = ExitGroup::restored(&venue_order_id, &trades, &open, &claimed) else {
            entries.push((venue_order_id, o));
            continue;
        };
        if group.unknown_legs() {
            tracing::warn!(
                venue = "fxcm",
                order_client_id = %o.order_client_id,
                order_id = %venue_order_id,
                "fxcm restore: other {}s of this one's rate rest on {} that no order of this node names, so it may be one \
                 leg of several whose others cannot be found: its fill is told as a partial fill, an amend is refused, and \
                 those exits are left alone (a cancel removes this one only)",
                group.kind().word(),
                group.instrument()
            );
        }
        unbuilt.remove(&o.order_client_id);
        placed.insert(o.order_client_id.clone(), group.legs()[0].as_placed());
        exits.insert(o.order_client_id, group);
        rebuilt.push(venue_order_id);
    }
    // A live order of the core now holds each: a new exit of its rate must not take it over under a second order client id.
    for venue_order_id in rebuilt {
        routes.taught.remove(&venue_order_id);
    }
    if entries.is_empty() {
        return;
    }
    let ids = read_entry();
    for (venue_order_id, o) in entries {
        let entry = match &ids {
            Ok((account_id, offers)) => {
                restored_entry(&venue_order_id, &o.symbol, account_id, offers)
            }
            Err(why) => Err(why.clone()),
        };
        match entry {
            Ok(p) => {
                unbuilt.remove(&o.order_client_id);
                placed.insert(o.order_client_id, p);
            }
            Err(why) => not_rebuilt(unbuilt, &o.order_client_id, &venue_order_id, why),
        }
    }
}

/// Remember why the re-taught `order_client_id` could not be rebuilt, for the words its cancel and amend are refused in. Bounded by
/// the orders that rested before the restart (a teaching names no other); a later teaching that rebuilds it drops the row.
fn not_rebuilt(
    unbuilt: &mut HashMap<String, String>,
    order_client_id: &str,
    venue_order_id: &str,
    why: String,
) {
    tracing::warn!(
        venue = "fxcm",
        order_client_id,
        order_id = venue_order_id,
        reason = %why,
        "fxcm restore: what a cancel or an amend of this re-taught order needs could not be rebuilt; both are refused \
         with this reason"
    );
    unbuilt.insert(order_client_id.to_string(), why);
}

/// **What a cancel and an amend of a resting ENTRY a previous process placed need**: its venue order id, the login's
/// account id (`forex_connect_account`'s `firstAccount`, the account every placement of this login goes to) and the offer id of the
/// order's own symbol in the Offers table (`forex_connect_offers`; the row its user name names,
/// [`crate::account::quote_rate::row_for`]). The rate is not known (`0.0`); nothing reads it for a cancel or an amend.
fn restored_entry(
    venue_order_id: &str,
    symbol: &str,
    account_id: &str,
    offers: &[crate::account::quote_rate::Offer],
) -> Result<PlacedOrder, String> {
    if account_id.is_empty() {
        return Err("the session names no account".to_string());
    }
    let row = crate::account::quote_rate::row_for(offers, symbol)
        .map_err(|why| why.to_string())?
        .ok_or_else(|| format!("`{symbol}` is not in the venue's Offers table"))?;
    let offer_id = row.offer_id.clone().ok_or_else(|| {
        format!(
            "the Offers row of {} carries no offer id (a shim older than the instrument terms)",
            row.instrument
        )
    })?;
    Ok(PlacedOrder {
        order_id: venue_order_id.to_string(),
        offer_id,
        account_id: account_id.to_string(),
        rate: 0.0,
    })
}

/// The restored orders the core holds a venue id of, each bound (`venue order id -> order_client_id`) in `ours` (the reconcile
/// listing's table): what [`adopt_restored_hook`] makes of a teaching before the session thread routes it.
/// An order with no (or an empty) venue id is left out.
fn bind_restored(
    orders: &[vike_execution::RestoredOrder],
    ours: &mut crate::execution::registry::OrderClientIdLookup,
) -> Vec<vike_execution::RestoredOrder> {
    let taught: Vec<vike_execution::RestoredOrder> = orders
        .iter()
        .filter(|o| o.venue_order_id.as_deref().is_some_and(|v| !v.is_empty()))
        .cloned()
        .collect();
    for o in &taught {
        ours.bind(o.venue_order_id.as_deref().unwrap_or_default(), &o.order_client_id);
    }
    taught
}

/// **Whether and how an order makes its instrument tradable first**, with whether the shim was ASKED (so the caller
/// logs a failure once, not at every remembered refusal). A CLOSE (`reduce_only`) never subscribes and never pays a
/// round trip; a session that cannot subscribe at all (`cannot_subscribe`: an older shim, a connection that is not
/// `Demo`) answers a `transient` note without asking; otherwise `ask` runs once per instrument per session
/// ([`crate::instrument::subscription::ensure`]).
fn ensure_for_order(
    reduce_only: bool,
    cannot_subscribe: Option<&str>,
    instrument: &str,
    subscribed: &mut crate::instrument::subscription::Attempts,
    ask: impl FnOnce() -> Result<(), crate::instrument::subscription::Failure>,
) -> (Option<crate::instrument::subscription::Failure>, bool) {
    if reduce_only {
        return (None, false);
    }
    if let Some(gap) = cannot_subscribe {
        let why = crate::refusals::FxcmSubscribeRefusal::CannotItself {
            instrument: instrument.to_string(),
            gap: gap.to_string(),
        };
        return (Some(crate::instrument::subscription::Failure { why, transient: true }), false);
    }
    let mut asked = false;
    let result =
        crate::instrument::subscription::ensure(subscribed, instrument, Instant::now(), || {
            asked = true;
            ask()
        });
    (result.err(), asked)
}

/// What one placement came to: the order, or `(what the failure was, the reason, what a later lookup needs)`; a
/// failure sent with an UNKNOWN outcome is `OrderRefusalId::OutcomeUnknown`.
type Placement = Result<PlacedOrder, (OrderRefusalId, String, Option<PendingLookup>)>;

/// A close's or an exit's failure (`(sent with an UNKNOWN outcome, the reason)`, the shape `crate::execution::close`
/// and `crate::execution::exit` answer in) as a placement's: OUTCOME UNKNOWN, else the venue's refusal.
/// shortcut: a definite failure there also covers a read before the send (the open trades, the exits) and a shim
/// refusal before it, which are NOT SENT; carry the id through those modules if the difference ever matters.
fn leg_failure(unknown: bool) -> OrderRefusalId {
    if unknown { OrderRefusalId::OutcomeUnknown } else { OrderRefusalId::VenueRefused }
}

/// **The reason an instrument could not be subscribed rides on the refusal of an order the venue then refuses.** An
/// unknown outcome of the subscription (`note.transient`: no answer, an unreadable table, a shim too old to ask, a
/// connection that is not `Demo`) lets the order go to the venue, whose own lookup judges the row; when it is refused
/// (a DEFINITE refusal) the words say why the bridge did not subscribe it either. An accepted order, an order
/// whose own outcome is unknown (nothing was refused) and a definite subscription failure (it ended the order
/// before any placement, in its own words) are left exactly as they are.
fn with_subscription_note(
    note: Option<&crate::instrument::subscription::Failure>,
    outcome: Placement,
) -> Placement {
    match (note, outcome) {
        (Some(note), Err((refusal, reason, lookup)))
            if note.transient && refusal != OrderRefusalId::OutcomeUnknown =>
        {
            Err((refusal, format!("{reason}; {}", note.why), lookup))
        }
        (_, outcome) => outcome,
    }
}

/// **Why a cancel or an amend of `order_client_id` finds nothing to act on.** The exec thread keeps a row only for an order it
/// placed that has not ended, so the answer covers three cases it cannot tell apart: never placed here (another
/// client, an earlier session), already ended (filled, cancelled), and REMOVED BY THE VENUE. The last is the one a
/// bracket meets: a trade's stop and take-profit are one OCO pair at FXCM, so the venue deletes the other exit when
/// one fires, and the core's own OCO cancel of that sibling arrives after the row is gone. The sentence says so
/// instead of reading as a fault (the reply is a NON-terminal `OrderCancelRejected`, which the FSM drops on an
/// order that has ended).
fn no_resting_order(order_client_id: &str) -> String {
    format!(
        "no resting order for {order_client_id}: this bridge holds none under that id (never placed here, or already \
         ended: filled, cancelled, or removed by the venue, as the other exit of a trade is when one of its \
         exits fills)"
    )
}

/// Why a cancel or an amend of `order_client_id` finds nothing to act on: when a gatewayhub restart's re-teaching named the order
/// and what reaching it needs could not be rebuilt (`unbuilt`, [`adopt_taught`]), THAT reason, since the order may still
/// rest at the venue; otherwise [`no_resting_order`]'s three cases.
fn nothing_to_act_on(unbuilt: &HashMap<String, String>, order_client_id: &str) -> String {
    match unbuilt.get(order_client_id) {
        Some(why) => format!(
            "fxcm: {order_client_id} was placed before the gatewayhub restarted and may still rest at the venue, but what reaching it \
             needs could not be rebuilt ({why}); this bridge cannot cancel or amend it: do it by hand in the FXCM platform"
        ),
        None => no_resting_order(order_client_id),
    }
}

/// Cancel a resting order that is no exit (an entry): `delete` (the shim's `forex_connect_delete_order`) with the ids `placed` holds,
/// the outcome in the venue's words. A failed or unknown cancel must not vanish (audit A2): every outcome is one event,
/// and an order held nowhere is refused in [`nothing_to_act_on`]'s words. A confirmed cancel drops both bookkeeping rows:
/// a cancelled order has nothing left to cancel and can produce no further envelope. Any other answer keeps them
/// ([`cancel_not_done`], [`already_gone`]).
fn cancel_resting(
    placed: &mut HashMap<String, PlacedOrder>,
    routes: &mut RouteTable,
    unbuilt: &HashMap<String, String>,
    order_client_id: &str,
    delete: impl FnOnce(&PlacedOrder) -> Result<OrderCancelAck, FxcmError>,
) -> Event {
    let Some(p) = placed.get(order_client_id) else {
        return cancel_not_done(
            order_client_id,
            OrderRefusalId::NotSent,
            nothing_to_act_on(unbuilt, order_client_id),
        );
    };
    match delete(p) {
        Err(e) => return cancel_not_done(order_client_id, e.refusal(), e.to_string()),
        Ok(OrderCancelAck::AlreadyGone) => return already_gone(order_client_id),
        Ok(_) => {}
    }
    if let Some(p) = placed.remove(order_client_id) {
        routes.remove(&p.order_id);
    }
    cancel_event(order_client_id, ExecutionClientActorCancelOutcome::Canceled)
}

/// **The answer to a cancel of an order FXCM no longer holds** (`ORA-20186` / `ORA-20169`,
/// `crate::transport::sys::FxcmSession::delete_order`'s [`OrderCancelAck::AlreadyGone`]): the shared loop's
/// (`vike_bridge_core::exec_actor::ALREADY_GONE_REASON`), a NON-terminal `OrderCancelRejected` that is no refusal. The order
/// filled or was cancelled, and the caller keeps its rows, so its own event on the lane ends it (a fill, or the Orders
/// Delete's `canceled` envelope); reconcile reads it otherwise. Nothing ends it here: a cancel read as final would make a
/// fill still on its way illegal (the order table has no `Canceled -> Filled` edge), and no read here tells the two apart.
fn already_gone(order_client_id: &str) -> Event {
    let why = vike_bridge_core::exec_actor::ALREADY_GONE_REASON.to_string();
    // The venue's own "no such order": it refused the CANCEL, as the shared loop's answer says.
    cancel_event(
        order_client_id,
        ExecutionClientActorCancelOutcome::Rejected(why, OrderRefusalId::VenueRefused),
    )
}

/// **The NON-terminal answer to a cancel that did not end the order.** One the venue may have applied (`unknown`: the shim's
/// `FC_SENT_NO_ANSWER`, an older shim's `-8`, FXCM's internal error; [`FxcmError::outcome_unknown`]) is OUTCOME UNKNOWN through
/// the shared answer (`vike_bridge_core::rest::cancel_rejected`, decision 0303), never a refusal, and nothing sends it again. The
/// caller keeps the order's rows, so the venue's own word ends it: its Orders Delete on the event lane (status `C`, the
/// `canceled` envelope) or its fill; reconcile reads it otherwise. No read here can: the Orders table lists only working
/// orders, and a row gone from it was cancelled OR executed. Every other answer is the refusal in `why`, under
/// `refusal`, what it was (the venue's, or one never sent).
fn cancel_not_done(order_client_id: &str, refusal: OrderRefusalId, why: String) -> Event {
    if refusal == OrderRefusalId::OutcomeUnknown {
        let err = VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: why };
        return vike_bridge_core::rest::cancel_rejected(order_client_id, &err);
    }
    cancel_event(order_client_id, ExecutionClientActorCancelOutcome::Rejected(why, refusal))
}

/// Move the rate of ONE resting order to `rate`, settling a reply that was lost by reading the order back (twice, with a
/// pause): an amend of a resting entry, and of every leg of an exit. A failure carries what it was
/// ([`FxcmError::refusal`]); a lost reply the read-back could not settle is OUTCOME UNKNOWN.
fn edit_resting(
    session: &FxcmSession,
    order_id: &str,
    account_id: &str,
    instrument: &str,
    rate: f64,
) -> Result<(), (String, OrderRefusalId)> {
    match session.edit_order_rate(order_id, account_id, instrument, rate) {
        Ok(()) => Ok(()),
        // SENT, reply lost: the amend may have applied. Read the order back.
        Err(e) if e.outcome_unknown() => settle_lost_amend(
            rate,
            &e.to_string(),
            || {
                session
                    .order_rate(order_id, account_id)
                    .map(|o| o.map(|v| (v.rate, v.tick)))
                    .map_err(|e| e.to_string())
            },
            || std::thread::sleep(AMEND_REREAD_PAUSE),
        )
        .map_err(|why| (why, OrderRefusalId::OutcomeUnknown)),
        Err(e) => Err((e.to_string(), e.refusal())),
    }
}

/// The venue's side of ONE exit order for the exec thread ([`ExitVenue`]): the shim's reads and writes bound to the
/// session, the order's instrument, side and kind. Its clock starts when it is built, which is when the command starts.
struct ExitVenueSession<'a> {
    session: &'a FxcmSession,
    instrument: String,
    /// The ORDER's side, which is the exit's: a SELL stop closes long trades.
    side: OrderSideWire,
    kind: ExitKind,
    started: Instant,
}

impl<'a> ExitVenueSession<'a> {
    fn new(
        session: &'a FxcmSession,
        instrument: &str,
        side: OrderSideWire,
        kind: ExitKind,
    ) -> Self {
        Self { session, instrument: instrument.to_string(), side, kind, started: Instant::now() }
    }

    /// The venue for a command on an exit that is already bound.
    fn of(session: &'a FxcmSession, group: &ExitGroup) -> Self {
        let side = if group.buy() { OrderSideWire::Buy } else { OrderSideWire::Sell };
        Self::new(session, group.instrument(), side, group.kind())
    }
}

impl ExitVenue for ExitVenueSession<'_> {
    fn open_trades(&mut self) -> Result<String, FxcmError> {
        self.session.open_trades_json()
    }

    fn open_exits(&mut self) -> Result<String, FxcmError> {
        self.session.open_exits_json()
    }

    fn place(&mut self, trade_id: &str, units: i32, rate: f64) -> Result<PlacedOrder, FxcmError> {
        self.session.place_exit(trade_id, self.side, exit::exit_order_type(self.kind), rate, units)
    }

    fn delete(&mut self, leg: &ExitLeg) -> Result<OrderCancelAck, FxcmError> {
        self.session.delete_order(&leg.order_id, &leg.account_id, &leg.offer_id)
    }

    fn edit(&mut self, leg: &ExitLeg, rate: f64) -> Result<(), String> {
        edit_resting(self.session, &leg.order_id, &leg.account_id, &self.instrument, rate)
            .map_err(|(why, _)| why)
    }

    fn may_start(&mut self) -> bool {
        self.started.elapsed().as_millis() < EXIT_LEG_START_MS
    }
}

/// Bind `ask` to its trades as their own `S` / `L` ([`exit::bind_exits`]; an exit the venue holds that this process did not
/// place is taken over when it is the very one asked for, `ours`) and keep what the exec thread needs: the first leg (the
/// venue order the core's accept names) and the group (`exits`). The submit path and a missed Close Limit's fallback
/// ([`rest_missed`]) share it.
fn bind_exit(
    ask: &Ask<'_>,
    order_client_id: &str,
    ours: &impl Fn(&str) -> bool,
    venue: &mut impl ExitVenue,
) -> Result<(PlacedOrder, ExitGroup), exit::ExitBindFailure> {
    let bound = exit::bind_exits(ask, ours, venue)?;
    let covers = bound.units();
    let taken = bound.legs.iter().filter(|l| l.taken_over).count();
    let Some(first) = bound.legs.first().map(ExitLeg::as_placed) else {
        return Err((false, "fxcm: the exit bound no trade".to_string(), None));
    };
    if covers != ask.units {
        tracing::warn!(
            venue = "fxcm",
            order_client_id,
            asked = ask.units,
            covers,
            "fxcm exit: the {} covers the one free trade there is, which holds less than the order asked; the engine \
             goes on counting the size it asked for",
            ask.kind.word()
        );
    }
    tracing::info!(
        venue = "fxcm",
        order_client_id,
        trades = bound.legs.len(),
        taken_over = taken,
        units = covers,
        rate = ask.rate,
        "fxcm exit: the {} is bound to {} trade(s) ({} taken over from the venue) and rests at the venue",
        ask.kind.word(),
        bound.legs.len(),
        taken
    );
    Ok((first, ExitGroup::new(ask.kind, ask.buy, ask.instrument, bound.legs)))
}

/// **A Close Limit the venue REFUSED outright** (`missed`: the price moved past the limit before it arrived, or any other
/// definite refusal of the first leg): its limit `ask` rests as the trade's own `L` instead, as a limit the market does not
/// reach does, and the order is accepted as that `L` (the review of 2026-11-16, finding 1(b)). If it cannot rest either,
/// the refusal names both reasons; nothing was accepted, so an `OrderRejected` is legal here.
fn rest_refused(
    missed: &str,
    ask: &Ask<'_>,
    order_client_id: &str,
    ours: &impl Fn(&str) -> bool,
    venue: &mut impl ExitVenue,
) -> Result<(PlacedOrder, ExitGroup), exit::ExitBindFailure> {
    match bind_exit(ask, order_client_id, ours, venue) {
        Ok(rested) => {
            tracing::warn!(
                venue = "fxcm",
                order_client_id,
                reason = %missed,
                "fxcm close: the close never worse than {} was refused; the limit rests as the trade's take-profit \
                 instead",
                ask.rate
            );
            Ok(rested)
        }
        Err((unknown, why, lookup)) => Err((
            unknown,
            format!(
                "fxcm: the close never worse than {} was refused ({missed}), and the limit could not rest as the \
                 trade's take-profit either: {why}",
                ask.rate
            ),
            lookup,
        )),
    }
}

/// **A Close Limit the venue ACCEPTED and then removed unfilled** (an Orders Delete with status `R` or `C`,
/// [`close::take_missed`]): its limit rests as the trade's own `L`, as a limit the market does not reach does, and the core's
/// order stays accepted, now resting, its fill to come from the `L` (the review of 2026-11-16, finding 1). If it cannot rest,
/// the order ends `OrderCanceled` with both reasons, the legal terminal from `Accepted` (an `OrderRejected` is not, and was
/// dropped: the order stayed `Accepted` for ever and the trade had neither close nor `L`).
fn rest_missed(
    order_client_id: &str,
    missed: close::Missed,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    exits: &mut HashMap<String, ExitGroup>,
    venue: &mut impl ExitVenue,
) -> Vec<Event> {
    let ask = Ask {
        instrument: &missed.rest.instrument,
        buy: missed.rest.buy,
        kind: ExitKind::Limit,
        units: missed.units,
        rate: missed.rest.rate,
    };
    let bound = {
        let ours = |oid: &str| routes.placed_here(oid);
        bind_exit(&ask, order_client_id, &ours, venue)
    };
    match bound {
        Ok((first, group)) => {
            tracing::warn!(
                venue = "fxcm",
                order_client_id,
                reason = %missed.why,
                "fxcm close: the close never worse than {} missed; the limit rests as the trade's take-profit instead",
                missed.rest.rate
            );
            for leg in group.legs() {
                routes.insert(leg.order_id.clone(), order_client_id.to_string());
            }
            placed.insert(order_client_id.to_string(), first);
            exits.insert(order_client_id.to_string(), group);
            Vec::new()
        }
        Err((unknown, why, _)) => {
            placed.remove(order_client_id);
            let may_rest = if unknown {
                " (an order sent for the take-profit has no answer: it MAY rest at the venue, and reconcile reports it)"
            } else {
                ""
            };
            vec![Event::OrderCanceled(OrderCanceled {
                order_client_id: order_client_id.to_string(),
                reason: format!(
                    "{}; and the limit could not rest as the trade's take-profit: {why}{may_rest}",
                    missed.why
                )
                .into(),
                timestamp: missed.at,
                post_only_would_cross: false,
                refusal_id: None,
            })]
        }
    }
}

/// Cancel an exit that is bound to one or several trades: every leg is removed, the routes of the ones that went are
/// dropped (the venue's own echo of a cancel the node made is then unroutable and says nothing), and the events say what
/// holds. `(the events, the order has no leg left)`. A cancel that removed SOME legs tells the core the cover it still
/// has before it refuses the rest.
fn cancel_exit(
    group: &mut ExitGroup,
    routes: &mut RouteTable,
    order_client_id: &str,
    venue: &mut impl ExitVenue,
) -> (Vec<Event>, bool) {
    let done = group.cancel(venue);
    for oid in &done.removed {
        routes.remove(oid);
    }
    tracing::info!(
        venue = "fxcm",
        order_client_id,
        "fxcm exit: the cancel removed {} exit(s) of the order; {} still rest",
        done.removed.len(),
        group.legs().len()
    );
    let mut evs = Vec::new();
    match done.refused {
        None => {
            evs.push(cancel_event(order_client_id, ExecutionClientActorCancelOutcome::Canceled))
        }
        Some(why) => {
            if !done.removed.is_empty() {
                evs.push(group.narrowed(order_client_id, vike_model::now_ms()));
            }
            // shortcut: a leg the command's time bound left untried was not sent at all; it reads as the venue's
            // refusal too. Carry the id in `exit::group::Cancelled` if the difference ever matters.
            let refusal = if done.outcome_unknown {
                OrderRefusalId::OutcomeUnknown
            } else {
                OrderRefusalId::VenueRefused
            };
            evs.push(if done.gone {
                already_gone(order_client_id)
            } else {
                cancel_not_done(order_client_id, refusal, why)
            });
        }
    }
    (evs, group.legs().is_empty())
}

/// Amend an exit that is bound to one or several trades: its rate and/or its size (`ExitGroup::amend`). The routes follow
/// the legs that were added or removed, and the events say what holds and what was refused.
fn amend_exit(
    group: &mut ExitGroup,
    routes: &mut RouteTable,
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    venue: &mut impl ExitVenue,
) -> Vec<Event> {
    let order_client_id = order.order_client_id.as_str();
    let want = match Want::of(order.qty, new_qty, new_price) {
        Ok(want) => want,
        Err(why) => {
            return vec![map_amend(
                order_client_id,
                None,
                amend_answer_time(None),
                0.0,
                Err((why.as_str(), OrderRefusalId::NotSent)),
            )];
        }
    };
    let owned = |oid: &str| routes.placed_here(oid);
    let done = group.amend(want, &owned, venue);
    tracing::info!(
        venue = "fxcm",
        order_client_id,
        "fxcm exit: the amend added {} exit(s) and removed {}; the order is now {:?} units at {:?}; refused: {:?}",
        done.added.len(),
        done.removed.len(),
        done.qty,
        done.rate,
        done.refused
    );
    for oid in &done.removed {
        routes.remove(oid);
    }
    for oid in &done.added {
        routes.insert(oid.clone(), order_client_id.to_string());
    }
    // stamped once the venue answered every leg (`vike_bridge_core::rest::amend_answer_time`)
    done.events(order_client_id, amend_answer_time(None))
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
/// one waiting — and still stops on `Shutdown`, so `ExecutionClientActor::detach` joins this thread.
///
/// Each answer is the one form of an arming refusal, `vike_bridge_core::venue_adapter::refuse_command`
/// (the owner's B-15): a submit is `OrderSubmitted` then a terminal `OrderRejected(reason)`; a cancel of an order
/// this thread refused is a terminal `OrderCanceled`, of any other (an earlier process's, which the venue may hold)
/// the non-terminal `OrderCancelRejected`; an amend the non-terminal `OrderModifyRejected(reason)` (owner, 2026-11-11).
/// This loop is not `run_loop` because it ticks.
///
/// ⚠ **The first version of a dead session's arm was `Err(_) => return`, and the two things it dropped were the
/// error text AND the orders.** Returning closed the command channel, so a submit that lost the race with the
/// thread's exit was simply discarded — no `OrderSubmitted`, no terminal, nothing — which is the silent vanish the
/// venue-adapter contract forbids, and the 45 seconds of nothing
/// `crates/bridges/fxcm/tests/fxcm_smokes/fxcm_live_smoke.rs` reported as one mute timeout.
fn refuse_every_command(
    events: &EventSender,
    rx: &Receiver<ExecutionClientActorCommand>,
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
                if !refuse_command(events, cmd, reason) {
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
/// `crates/bridges/fxcm/tests/fxcm_smokes/fxcm_live_smoke.rs`'s `OrderRejected` arm treats a venue reject as
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
    rx: &Receiver<ExecutionClientActorCommand>,
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
                    if !refuse_command(events, cmd, &late) {
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

/// **Tell the host this bridge has LEARNED the venue**: `ExecLink{up:true}` (`vike_execution::TradeLinkUpdate`), stamped
/// with the account's route key, once per start, when its ForexConnect session opens. This bridge learns nothing more by
/// waiting: the orders a previous process placed are routed when its host TEACHES them (`adopt_taught`, every pass), and
/// it rebinds no order of an earlier adapter by itself. The gatewayhub that supervises the session (decision 0164 verdict
/// 7(b), amended 2026-11-21) holds a rebuilt session until this word comes, and holds a session whose bridge never says it
/// (`watch_learning`); a session the SDK later gives up on is told by the cell, never by an `up: false` here.
///
/// ⚠ Only a HOSTED client says it (`spawn_execution_client_reporting`): the host must consume `Ingest::TradeLink` itself, and
/// the front door's callers read venue events only.
fn announce_learned(events: &EventSender) {
    let venue = vike_marketdata::Symbol::from(crate::reconcile::VENUE);
    let update = TradeLinkUpdate {
        route_key: events.route_key.unwrap_or(venue),
        venue,
        up: true,
        at_timestamp_ms: vike_model::now_ms(),
    };
    if events.execution_link(update).is_err() {
        tracing::debug!(
            venue = "fxcm",
            "fxcm: the session's learning was not announced: the event lane is closed"
        );
    }
}

/// `instrument`'s `(bid, ask)` NOW, the one row of one refresh bounded by the shim's pre-send bound
/// (`FxcmSession::quote`; it used to be the whole Offers table under its 8 s bound, the review of 2026-11-16, finding 6),
/// or `None`, said in a warning, when it cannot be read: a fired close then closes by trade id under its limit first,
/// any other order rests as the take-profit (`close::CloseDesk::route_order`).
fn quote_of(session: &FxcmSession, instrument: &str) -> Option<(f64, f64)> {
    let quote = match session.quote(instrument) {
        Ok(Some((bid, ask))) => close::usable_quote(bid, ask),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(venue = "fxcm", instrument, "fxcm: the quote could not be read: {e}");
            None
        }
    };
    if quote.is_none() {
        tracing::warn!(
            venue = "fxcm",
            instrument,
            "fxcm: no quote for {instrument}, so a reduce-only limit cannot be judged against the market: a fired close \
             closes its trades by id never worse than its price first and rests as their take-profit if that misses, and \
             any other rests as their take-profit"
        );
    }
    quote
}

fn run(
    config: FxcmConfig,
    events: EventSender,
    rx: &Receiver<ExecutionClientActorCommand>,
    confirms: Receiver<ConfirmRequest>,
    taught: Receiver<Vec<vike_execution::RestoredOrder>>,
    link: &VenueLink,
    hosted: bool,
) {
    let Some(session) = open_session(
        || {
            let login = FxcmSession::login(
                &config.user,
                config.password.expose(),
                &config.url,
                &config.connection,
            )
            .map_err(|e| e.to_string());
            // The reading only: `open_session` decides the retry and the refusals exactly as before.
            if let Err(e) = &login {
                let why = format!("{}: {e}", crate::SESSION_UNAVAILABLE);
                say(link, login_failed(&why, refuses_credentials(e)));
            }
            login
        },
        &events,
        rx,
        &confirms,
        |failures| pause_to_a_login(config.fx_clock.as_ref(), failures),
    ) else {
        return;
    };
    if hosted {
        announce_learned(&events);
    }
    // The cancel map: order client id -> the ids `delete_order` needs. Pruned on every terminal, since a
    // terminal order has no resting order to cancel (`ends_cancelability`).
    let mut placed: HashMap<String, PlacedOrder> = HashMap::new();
    // The re-taught orders `placed` could not be rebuilt for (order client id -> why), so their cancel says why ([`adopt_taught`]).
    let mut unbuilt: HashMap<String, String> = HashMap::new();
    // The fill-routing map, bounded — venue order_id -> order client id. Pruned only when nothing further can
    // arrive for that order (`closes_routing`); a FILL deliberately keeps its row.
    let mut routes = RouteTable::default();
    // FXCM instrument -> its base unit size on THIS session's account: the number `qty` is divided
    // by to get the lot count the shim places. Cached for the life of the session because that is
    // exactly the scope over which it cannot change — it is per-instrument AND per-ACCOUNT, and a
    // session has one account (`firstAccount`). A cache miss costs one FFI round trip on the first
    // order for an instrument and nothing after; without it every submit pays one.
    let mut base_units: HashMap<String, f64> = HashMap::new();
    // What THIS session already asked FXCM to subscribe for trading, and how it went (`crate::instrument::subscription`).
    let mut subscribed = crate::instrument::subscription::Attempts::default();
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
    // The placements whose reply was LOST (order client id -> what the shim sent), for `confirm`. In memory like
    // `routes`: after a restart the question falls to reconcile.
    let mut unknown = UnknownTable::default();
    // THE ACCOUNT'S MODE, read once (a login-time row): on a HEDGING account an opposite market order opens a second
    // trade, so a reduce-only market order closes the trades facing it BY ID instead (`crate::execution::close`). `Unknown` (a shim
    // without `forex_connect_account_traits`, a failed read) is not assumed to net.
    let traits = session.account_traits_json();
    let account_mode = close::position_mode(traits.as_ref().ok().and_then(|t| t.as_deref()));
    tracing::info!(
        target: "vike_fxcm",
        venue = "fxcm",
        mode = close::position_mode_words(account_mode),
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
    // The closes in progress (order client id -> what their fills still owe), so the several fills of one close read as ONE order.
    let mut closes: HashMap<String, CloseTracker> = HashMap::new();
    // The exits in force (order client id -> the venue orders it is made of, one per trade it covers), so a cancel, an amend, a fill and the
    // venue's removal of one leg read as the ONE order the core knows ([`crate::execution::exit::group`]).
    let mut exits: HashMap<String, ExitGroup> = HashMap::new();
    // What each opening fill carried in commission, so a closing fill books only what is still owed ([`crate::execution::commission`]).
    let mut opens = OpenCommissions::default();
    // The closing fills whose rollover interest was booked as funding, and the account's currency once read
    // ([`crate::execution::funding`]).
    let mut rollover = RolloverBooked::default();
    // The routing of a submit that may be a close: the account's mode, the shim's gap and the instruments with a close of
    // UNKNOWN outcome (further reduce-only closes of them are held for a minute).
    let mut close_desk = CloseDesk::new(account_mode, close_gap, exit_gap);
    let mut late = ExecutionClientActorLateCloses::default();
    // The idle-limit evidence (no behaviour): one line per change of the SDK's session status, and once a minute the
    // longest gap between the envelopes `drain_events` takes off the shim's queue (`vike_bridge_core::link_gap`).
    let mut statuses = StatusChanges::default();
    let mut gaps = GapWindow::new(Instant::now());

    // recv_timeout (not recv): between commands we poll the shim's async event queue so a delayed
    // fill / cancel / reject — and, after a ForexConnect reconnect, the re-surfaced trades (audit
    // A3) — reach the core. An empty queue makes poll_event return None, so this idles. (It said
    // "In the stub build poll_event returns None" until 2026-09-28; #1720 deleted the stub build —
    // a box with no shim never opens a session, and `refuse_every_command` answers there instead.)
    loop {
        // The SDK's word on the session, every pass (a local read): what the gatewayhub's status verb reports, and a
        // line when it changed. Then the minute's gap summary, when one is due.
        let now = Instant::now();
        let status = session.session_status();
        statuses.note(status, now);
        say(link, from_status(status));
        if let Some(summary) = gaps.take_due(now) {
            summary.say("fxcm");
        }
        let command = rx.recv_timeout(EVENT_POLL);
        // The orders a previous process placed: before this command (the core taught them before it sent it) and before
        // this pass drains their events.
        adopt_taught(
            &taught,
            &mut routes,
            &mut placed,
            &mut exits,
            &mut unbuilt,
            || {
                let trades = session
                    .open_trades_json()
                    .map_err(|e| e.to_string())
                    .and_then(|json| close::parse_open_trades(&json))?;
                let open = session
                    .open_exits_json()
                    .map_err(|e| e.to_string())
                    .and_then(|json| exit::parse_open_exits(&json))?;
                Ok((trades, open))
            },
            // The account id is the login-time row (no round trip); the Offers table is one bounded refresh.
            || {
                let account = session
                    .account_id()
                    .map_err(|e| format!("the account could not be read: {e}"))?;
                let offers = session
                    .offers_json()
                    .map_err(|e| format!("the Offers table could not be read: {e}"))?
                    .ok_or_else(|| {
                        "the installed shim exports no `forex_connect_offers`, so no offer id can be read"
                            .to_string()
                    })?;
                let offers = crate::account::quote_rate::parse_offers(&offers)
                    .map_err(|why| why.to_string())?;
                Ok((account, offers))
            },
        );
        match answer_stale_amend(&events, command) {
            Ok(ExecutionClientActorCommand::Submit(req, queued_at)) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: req.order_client_id.clone(),
                    timestamp: req.timestamp,
                }));
                // A kind this adapter places no order of, then an order too old to send: refused by name
                // before it costs a session read or a round trip (`preflight_request` below asks the kind
                // again, as the pure function it is). A cancel and a close have no age arm.
                if let Some((why, refusal)) = kind_refusal(&req.order_type)
                    .map(|why| (why, OrderRefusalId::VenueCapability))
                    .or_else(|| {
                        too_old_to_send(&req, queued_at).map(|why| (why, OrderRefusalId::NotSent))
                    })
                {
                    let refused = Err((why.as_str(), refusal));
                    for event in map_placement(&req.order_client_id, req.timestamp, refused) {
                        let _ = events.blocking_send(event);
                    }
                    continue;
                }
                // The venue's own name of the order's USER name, off its KEPT row (decision 0290: `EURUSD` sends
                // `EUR/USD`, `USOIL.CFD` sends `USOil`). No list, no row or two rows: refused in words, nothing sent.
                let instrument = match crate::instrument::names::wire_name(&req.symbol) {
                    Ok(wire) => wire,
                    Err(why) => {
                        let refused = Err((why.as_str(), OrderRefusalId::NotSent));
                        for event in map_placement(&req.order_client_id, req.timestamp, refused) {
                            let _ = events.blocking_send(event);
                        }
                        continue;
                    }
                };
                // A close that waited, on a netted account where it goes out as a plain opposite order, asks the
                // open trades first (`ExecutionClientActorLateCloses`); a hedged one closes by trade and cannot open.
                let late_quantity = if close_desk.nets() && late.must_read(&req, queued_at) {
                    let held = session
                        .open_trades_json()
                        .map_err(|e| e.to_string())
                        .and_then(|json| close::parse_open_trades(&json))
                        .map(|trades| close::net_units(&trades, &instrument));
                    match late_close_size(&req, held) {
                        Ok(cut) => cut,
                        Err(why) => {
                            let refused = Err((why.as_str(), OrderRefusalId::NotSent));
                            for event in map_placement(&req.order_client_id, req.timestamp, refused)
                            {
                                let _ = events.blocking_send(event);
                            }
                            continue;
                        }
                    }
                } else {
                    None
                };
                let side = if req.side >= 0 { OrderSideWire::Buy } else { OrderSideWire::Sell };
                // THE SUBSCRIPTION. FXCM holds most of its Offers table as view-only or disabled, and only a `T` row
                // trades; the bridge makes the instrument tradable ITSELF the first time this session is asked to
                // trade it (`crate::instrument::subscription`; the owner's "any ticker of the venue"), bounded and never
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
                    || {
                        session.subscribe_tradable(
                            &instrument,
                            crate::instrument::subscription::EXECUTION_BUDGET_MS,
                        )
                    },
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
                    not_ensured.as_ref().filter(|f| !f.transient).map(|f| f.why.to_string());
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
                // The subscription and the unit read above can spend longer than the order had left: asked again.
                let sized = base_unit
                    .clone()
                    .and_then(|b| {
                        preflight_request(
                            &req.order_type,
                            late_quantity.unwrap_or(req.qty),
                            req.price,
                            b,
                        )
                    })
                    .and_then(|lots| too_old_to_send(&req, queued_at).map_or(Ok(lots), Err));
                // The legs of a close, past the first: their order ids (routes) and what their fills owe (`closes`).
                let mut close_reg: Option<(Vec<String>, CloseTracker)> = None;
                // The legs of an exit (one or several trades): routed to this order and kept as its group (`exits`).
                let mut exit_reg: Option<ExitGroup> = None;
                let outcome = match sized {
                    // Every one a check before the send (the kind was asked above).
                    Err(reason) => Err((OrderRefusalId::NotSent, reason, None)),
                    // The ONE place `order_type` is read for ROUTING. A market request goes to the
                    // shim's true-market placement (executes now); a limit that NAMES a price rests
                    // AT that rate (`forex_connect_place_entry_at`); a limit with no price keeps the fixed
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
                        // opens a second trade (`crate::execution::close`, which owns the rules). A market one closes the trades
                        // facing it by id; a priced limit and a stop are bound to the trade they protect
                        // (`crate::execution::exit`); an unpriced limit, and a stop that is not reduce-only, are refused by name;
                        // a plain order and a netting account are exactly what they were. A reduce-only limit the market
                        // already reaches (a fired stop-limit) closes by trade id now, never worse than its price.
                        let route = close_desk.route_order(
                            &instrument,
                            &req,
                            || quote_of(&session, &instrument),
                            || session.close_limit_gap(),
                            Instant::now(),
                        );
                        match route {
                            // shortcut: a reduce-only hold after a close of unknown outcome is NOT SENT, not a
                            // capability; type `Route::Refuse` if the difference ever matters.
                            Route::Refuse(why) => Err((OrderRefusalId::VenueCapability, why, None)),
                            Route::BindExit(kind) => {
                                match exit::exit_shape(
                                    &req.order_type,
                                    req.price,
                                    req.trigger_price,
                                    req.trigger_by,
                                ) {
                                    // shortcut: a missing or non-positive rate is a terms check (NOT SENT),
                                    // the rest a kind fxcm binds no exit of.
                                    Err(why) => Err((OrderRefusalId::VenueCapability, why, None)),
                                    Ok((_, rate)) => {
                                        let ask = Ask {
                                            instrument: &instrument,
                                            buy: side == OrderSideWire::Buy,
                                            kind,
                                            units: i64::from(amount),
                                            rate,
                                        };
                                        let mut venue = ExitVenueSession::new(
                                            &session,
                                            &instrument,
                                            side,
                                            kind,
                                        );
                                        let ours = |oid: &str| routes.placed_here(oid);
                                        bind_exit(&ask, &req.order_client_id, &ours, &mut venue)
                                            .map(|(first, group)| {
                                                exit_reg = Some(group);
                                                first
                                            })
                                            .map_err(|(unknown, why, lookup)| {
                                                (leg_failure(unknown), why, lookup)
                                            })
                                    }
                                }
                            }
                            Route::CloseByTrade => {
                                let started = Instant::now();
                                let bound = close::close_bound(&req);
                                let result = close::close_by_trade(
                                    &instrument,
                                    side == OrderSideWire::Buy,
                                    i64::from(amount),
                                    || session.open_trades_json(),
                                    |trade_id, units| match bound {
                                        Some(rate) => {
                                            session.close_trade_limit(trade_id, units, rate)
                                        }
                                        None => session.close_trade(trade_id, units),
                                    },
                                    || started.elapsed().as_millis() < CLOSE_LEG_START_MS,
                                );
                                // A close whose reply was lost may have happened with nothing to route its fill: hold
                                // further reduce-only closes of this instrument (`close::UNKNOWN_CLOSE_HOLD`).
                                if close_desk.settle(&instrument, &result, Instant::now()) {
                                    tracing::warn!(
                                        target: "vike_fxcm",
                                        venue = "fxcm",
                                        instrument = %instrument,
                                        order_client_id = %req.order_client_id,
                                        "fxcm close: a close was sent and its outcome is UNKNOWN; further reduce-only \
                                         closes of {instrument} are refused for {} s",
                                        close::UNKNOWN_CLOSE_HOLD.as_secs()
                                    );
                                }
                                let buy = side == OrderSideWire::Buy;
                                match (result, bound) {
                                    // A Close Limit the venue refused outright (the price moved past the limit before it
                                    // arrived, or any other definite refusal): its limit rests as the trade's own `L`, as
                                    // one the market does not reach does (the review of 2026-11-16, finding 1).
                                    (Err((false, missed)), Some(rate)) => {
                                        // `rest_refused` answers in the exit's failure shape.
                                        let ask = Ask {
                                            instrument: &instrument,
                                            buy,
                                            kind: ExitKind::Limit,
                                            units: i64::from(amount),
                                            rate,
                                        };
                                        let mut venue = ExitVenueSession::new(
                                            &session,
                                            &instrument,
                                            side,
                                            ExitKind::Limit,
                                        );
                                        let ours = |oid: &str| routes.placed_here(oid);
                                        rest_refused(&missed, &ask, &req.order_client_id, &ours, &mut venue)
                                            .map(|(first, group)| {
                                                exit_reg = Some(group);
                                                first
                                            })
                                            .map_err(|(unknown, why, lookup)| {
                                                (leg_failure(unknown), why, lookup)
                                            })
                                    }
                                    (result, bound) => result
                                        .map(|done| {
                                            if let Some(why) = &done.stopped {
                                                tracing::warn!(
                                                    venue = "fxcm",
                                                    order_client_id = %req.order_client_id,
                                                    reason = %why,
                                                    "fxcm close: only part of the close was sent; the order ends \
                                                     cancelled after the fills of the legs that were"
                                                );
                                            }
                                            let track = CloseTracker::new(done.units, done.stopped)
                                                .with_legs(done.legs);
                                            let track = match bound {
                                                Some(rate) => track.resting_at(close::LimitRest {
                                                    instrument: instrument.clone(),
                                                    buy,
                                                    rate,
                                                }),
                                                None => track,
                                            };
                                            close_reg = Some((done.more_ids, track));
                                            done.first
                                        })
                                        .map_err(|(unknown, why)| (leg_failure(unknown), why, None)),
                                }
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
                                // The first is what the failure was (`OutcomeUnknown`: SENT, see below); the
                                // third is what a later `confirm` needs to find the order, when the shim handed
                                // it back.
                                placement.map_err(|e| {
                                    let refusal = e.refusal();
                                    let text = e.to_string();
                                    (refusal, text, e.into_lookup())
                                })
                            }
                        }
                    }
                };
                // The reason the instrument could not be subscribed rides on the refusal of an order the venue then
                // refuses (an unknown outcome or a shim too old to ask let the order go to the venue).
                let outcome = with_subscription_note(not_ensured.as_ref(), outcome);
                let evs = match &outcome {
                    Ok(p) => {
                        map_placement(&req.order_client_id, req.timestamp, Ok(p.order_id.as_str()))
                    }
                    // ⚠ SENT, outcome UNKNOWN (no answer, or no order id, and the shim's Orders
                    // lookup found nothing either): NO terminal event. The order may rest on the
                    // venue, and an `OrderRejected` would leave a live order the book does not
                    // know. It stays Submitted for the stuck-order watchdog; reconcile reports it
                    // if it rests.
                    Err((OrderRefusalId::OutcomeUnknown, reason, lookup)) => {
                        // Only a placement the shim handed a RequestID for can be looked up again.
                        let next = if lookup.is_some() {
                            "the watchdog's confirm will look the order up again"
                        } else {
                            "the shim returned no RequestID, so confirm CANNOT look it up: it is \
                             left to reconcile and the watchdog's last resort"
                        };
                        tracing::warn!(
                            venue = "fxcm",
                            order_client_id = %req.order_client_id,
                            reason = %reason,
                            "fxcm placement outcome UNKNOWN: no terminal event emitted; {next}"
                        );
                        Vec::new()
                    }
                    Err((refusal, reason, _)) => map_placement(
                        &req.order_client_id,
                        req.timestamp,
                        Err((reason.as_str(), *refusal)),
                    ),
                };
                let mut echo = None;
                match outcome {
                    Ok(p) => {
                        // Route a LATER async fill for this venue order back to its order client id.
                        routes.insert(p.order_id.clone(), req.order_client_id.clone());
                        // A close (a Close Limit included) rests nothing, so it has no resting rate to echo.
                        let closing = close_reg.is_some();
                        // A close that sent several legs: every leg's fill routes to the same order.
                        if let Some((more_ids, track)) = close_reg.take() {
                            register_close(
                                &mut routes,
                                &mut closes,
                                &req.order_client_id,
                                more_ids,
                                track,
                            );
                        }
                        // An exit that covers several trades: every leg's fill, removal and echo routes to the same order.
                        if let Some(group) = exit_reg.take() {
                            for leg in group.legs().iter().skip(1) {
                                routes.insert(leg.order_id.clone(), req.order_client_id.clone());
                            }
                            exits.insert(req.order_client_id.clone(), group);
                        }
                        if !is_market(&req.order_type) && !closing {
                            echo = Some((p.order_id.clone(), p.account_id.clone(), p.rate));
                        }
                        placed.insert(req.order_client_id.clone(), p);
                    }
                    // Remember what was sent, for `confirm`.
                    Err((OrderRefusalId::OutcomeUnknown, _, Some(lookup))) => {
                        unknown.insert(
                            req.order_client_id.clone(),
                            UnknownPlacement { lookup, timestamp: req.timestamp },
                        );
                    }
                    Err(_) => {}
                }
                for event in evs {
                    let _ = events.blocking_send(event);
                }
                // AFTER the accept is out, so the venue read (a refresh round trip) never delays it.
                if let Some((order_id, account_id, sent)) = echo {
                    echo_venue_rate(
                        &session,
                        "place",
                        &req.order_client_id,
                        &order_id,
                        &account_id,
                        req.price.or(req.trigger_price).unwrap_or(sent),
                        sent,
                    );
                }
            }
            Ok(ExecutionClientActorCommand::Cancel { order_client_id, .. })
                if exits.contains_key(&order_client_id) =>
            {
                // An exit (of one trade or several): every leg goes, and the events say what holds. A panic inside
                // leaves the cancel of unknown outcome (`sending`), answered by the dead exec thread.
                let done = sending(Sending::Cancels(vec![order_client_id.clone()]), || {
                    exits.get_mut(&order_client_id).map(|group| {
                        let mut venue = ExitVenueSession::of(&session, group);
                        cancel_exit(group, &mut routes, &order_client_id, &mut venue)
                    })
                });
                if let Some((evs, gone)) = done {
                    if gone {
                        exits.remove(&order_client_id);
                        placed.remove(&order_client_id);
                    }
                    for event in evs {
                        let _ = events.blocking_send(event);
                    }
                }
            }
            Ok(ExecutionClientActorCommand::Cancel { order_client_id, .. }) => {
                let event = sending(Sending::Cancels(vec![order_client_id.clone()]), || {
                    cancel_resting(&mut placed, &mut routes, &unbuilt, &order_client_id, |p| {
                        session.delete_order(&p.order_id, &p.account_id, &p.offer_id)
                    })
                });
                let _ = events.blocking_send(event);
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecutionClientActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            Ok(ExecutionClientActorCommand::CancelBatch { order_client_ids, .. }) => {
                cancel_batch_undeclared(&events, &order_client_ids)
            }
            // Unreachable the same way: this venue declares no bulk submit and no queued adopt.
            Ok(ExecutionClientActorCommand::SubmitBatch(requests, _)) => {
                vike_bridge_core::exec_actor::submit_batch_undeclared(&events, &requests)
            }
            Ok(ExecutionClientActorCommand::Adopt(order)) => {
                vike_bridge_core::exec_actor::adopt_undeclared(&order)
            }
            // An exit (of one trade or several): its rate and its SIZE (whole trades' exits added or removed), told as
            // what holds and what was refused ([`amend_exit`]).
            Ok(ExecutionClientActorCommand::Modify { order, new_qty, new_price, .. })
                if exits.contains_key(order.order_client_id.as_str()) =>
            {
                let evs = sending(Sending::Amend(order.order_client_id.clone()), || {
                    exits.get_mut(order.order_client_id.as_str()).map(|group| {
                        let mut venue = ExitVenueSession::of(&session, group);
                        amend_exit(group, &mut routes, &order, new_qty, new_price, &mut venue)
                    })
                });
                for event in evs.into_iter().flatten() {
                    let _ = events.blocking_send(event);
                }
            }
            // Price-only amend of a resting entry (`EditOrder`, `forex_connect_edit_order`). EVERY outcome is
            // one explicit event: `OrderModified` on success, else the NON-terminal
            // `OrderModifyRejected` (the resting order keeps its terms) — a size change, an order
            // this process did not place, an off-grid rate, a shim that predates ABI 2, and the
            // venue's own refusal all included.
            Ok(ExecutionClientActorCommand::Modify { order, new_qty, new_price, .. }) => {
                let order_client_id = order.order_client_id.as_str();
                // The ids of an amend the venue ACCEPTED, for the echo that follows the event.
                let mut echo = None;
                // Every answer is stamped when it is made, the venue's after its reply
                // (`vike_bridge_core::rest::amend_answer_time`: the session states no time).
                let event = match plan_amend(order.qty, new_qty, new_price) {
                    Err((reason, refusal)) => map_amend(
                        order_client_id,
                        None,
                        amend_answer_time(None),
                        0.0,
                        Err((reason.as_str(), refusal)),
                    ),
                    Ok(rate) => match placed.get(order_client_id) {
                        None => {
                            let why = nothing_to_act_on(&unbuilt, order_client_id);
                            map_amend(
                                order_client_id,
                                None,
                                amend_answer_time(None),
                                rate,
                                Err((why.as_str(), OrderRefusalId::NotSent)),
                            )
                        }
                        Some(p) => {
                            let res = sending(Sending::Amend(order_client_id.to_string()), || {
                                crate::instrument::names::wire_name(&order.symbol)
                                    .map_err(|why| (why, OrderRefusalId::NotSent))
                                    .and_then(|instrument| {
                                        edit_resting(
                                            &session,
                                            &p.order_id,
                                            &p.account_id,
                                            &instrument,
                                            rate,
                                        )
                                    })
                            });
                            if res.is_ok() {
                                echo = Some((p.order_id.clone(), p.account_id.clone(), rate));
                            }
                            // The event carries the rate that was REQUESTED: the venue accepted the
                            // EditOrder, and a read-back (which can take its whole refresh timeout)
                            // must not delay it. The echo below only logs.
                            map_amend(
                                order_client_id,
                                Some(p.order_id.as_str()),
                                amend_answer_time(None),
                                rate,
                                res.as_ref()
                                    .map(|_| ())
                                    .map_err(|(why, refusal)| (why.as_str(), *refusal)),
                            )
                        }
                    },
                };
                let _ = events.blocking_send(event);
                // AFTER the event is out, as for a placement: info, and WARN past half a tick.
                if let Some((order_id, account_id, rate)) = echo {
                    echo_venue_rate(
                        &session,
                        "amend",
                        order_client_id,
                        &order_id,
                        &account_id,
                        rate,
                        rate,
                    );
                }
            }
            Ok(ExecutionClientActorCommand::Shutdown) => break,
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
            |event| {
                let _ = events.blocking_send(event);
            },
        );
        // Drain any async order events the shim queued (fills/terminals). No-op on an empty queue.
        drain_events(
            &session,
            &mut routes,
            &mut placed,
            &mut closes,
            &mut exits,
            &mut opens,
            &mut rollover,
            &events,
            &mut gaps,
        );
    }
    // `session` drops here → forex_connect_logout
}

/// **An amend that waited past `vike_bridge_core::exec_actor::MAX_MODIFY_WAIT`** (by the stamp it joined the queue with) is
/// answered here, unsent, with the shared `run_loop`'s non-terminal `OrderModifyRejected` and words (this thread runs its
/// own loop), and the pass goes on as an idle one: the confirm lane and the drain still run. Every other command is handed
/// back. [`run`] asks it after the pass's session reads, the last point before an amend's own venue call.
fn answer_stale_amend(
    events: &EventSender,
    command: Result<ExecutionClientActorCommand, RecvTimeoutError>,
) -> Result<ExecutionClientActorCommand, RecvTimeoutError> {
    if let Ok(ExecutionClientActorCommand::Modify { order, queued_at, .. }) = &command
        && let Some(why) = too_old_to_amend(*queued_at)
    {
        let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: order.order_client_id.clone(),
            reason: why.into(),
            timestamp: amend_answer_time(None),
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::NotSent.word(),
            ),
        }));
        return Err(RecvTimeoutError::Timeout);
    }
    command
}

/// Remember a close in progress. Bounded: a close whose fills never arrive (a lost reply, a venue that never reports it)
/// is not removed by anything else, so past [`MAX_CLOSES`] the table starts over - the fills of those closes then decode
/// as plain `OrderFilled`s, which is what they were before this table existed, and reconcile is what settles the rest.
/// Register a finished close that sent more than its first leg: EVERY further leg's closing order id routes to the
/// same order client id (without it that leg's fill is an envelope for an order this process "did not place" and is
/// dropped), and the order's tracker is remembered so the fills of its legs read as ONE order ([`fold_envelope`]).
/// The first leg's id is routed by the caller, like any placement's.
fn register_close(
    routes: &mut RouteTable,
    closes: &mut HashMap<String, CloseTracker>,
    order_client_id: &str,
    more_ids: Vec<String>,
    track: CloseTracker,
) {
    for id in more_ids {
        routes.insert(id, order_client_id.to_string());
    }
    insert_close(closes, order_client_id.to_string(), track);
}

fn insert_close(
    closes: &mut HashMap<String, CloseTracker>,
    order_client_id: String,
    track: CloseTracker,
) {
    if closes.len() >= MAX_CLOSES {
        tracing::warn!(
            venue = "fxcm",
            "fxcm close: {MAX_CLOSES} closes in progress were never completed; forgetting them"
        );
        closes.clear();
    }
    closes.insert(order_client_id, track);
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
        for event in confirm_unknown(unknown, placed, routes, &req.order_client_id, &mut find) {
            emit(event);
        }
        let _ = req.reply.send(());
        if start.elapsed() >= budget {
            break;
        }
    }
}

/// Answer one `confirm` for `order_client_id`: look a placement whose reply was lost up again (`find`, the
/// shim's `forex_connect_find_order` over the terms it sent). Found exactly one -> the SAME `OrderAccepted` a
/// normal accept emits, with the cancel and fill-routing rows a normal accept creates. Anything else
/// (not in the table, ambiguous, the read failed, not an order client id this process left unknown) -> nothing,
/// and the row STAYS for the watchdog's next confirm: an absence is not a refusal, the order may be
/// booked late, and a terminal here would strand a live one. A found order id that this process
/// already holds under a DIFFERENT order client id (`routes` or `placed`: a RequestID reused across sessions)
/// is not this order client id's order either: nothing, and the row stays. Pure over its closure.
fn confirm_unknown(
    unknown: &mut UnknownTable,
    placed: &mut HashMap<String, PlacedOrder>,
    routes: &mut RouteTable,
    order_client_id: &str,
    find: impl FnOnce(&PendingLookup) -> Result<Option<String>, String>,
) -> Vec<Event> {
    let Some(u) = unknown.as_map().get(order_client_id) else {
        return Vec::new();
    };
    match find(&u.lookup) {
        Ok(Some(order_id)) => {
            let owner = routes
                .routes()
                .get(&order_id)
                .or_else(|| placed.iter().find(|(_, p)| p.order_id == order_id).map(|(c, _)| c))
                .filter(|c| c.as_str() != order_client_id)
                .cloned();
            if let Some(owner) = owner {
                tracing::warn!(
                    venue = "fxcm",
                    order_client_id,
                    order_id = %order_id,
                    owner = %owner,
                    "confirm: the lookup found an order id this session already holds for another \
                     order client id (a stale RequestID collision): not accepted for this one"
                );
                return Vec::new();
            }
            let Some(u) = unknown.remove(order_client_id) else {
                return Vec::new();
            };
            routes.insert(order_id.clone(), order_client_id.to_string());
            let evs = map_placement(order_client_id, u.timestamp, Ok(order_id.as_str()));
            placed.insert(
                order_client_id.to_string(),
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
                order_client_id,
                "confirm: a fresh Orders table does not hold the placement whose reply was lost \
                 (it never rested, or it was a market order that filled and left the table): left \
                 to reconcile and the watchdog"
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(
                venue = "fxcm", order_client_id, error = %e,
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
/// `forex_connect_order_rate`) is logged and never touches the order - the echo is evidence, not a gate. Bounded
/// in the shim (`ECHO_REFRESH_MS`), so a slow demo cannot stall a queued Cancel for long.
fn echo_venue_rate(
    session: &FxcmSession,
    what: &str,
    order_client_id: &str,
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
                order_client_id,
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
                    order_client_id,
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
                order_client_id,
                order_id,
                "the installed shim has no forex_connect_order_rate: reinstall it to echo the venue rate"
            );
        }
        Err(e) => {
            tracing::warn!(
                venue = "fxcm", what, order_client_id, order_id, requested, error = %e,
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
/// one permanent row per accepted order in each. Each envelope taken is one inbound event of `gaps`.
fn drain_events(
    session: &FxcmSession,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    closes: &mut HashMap<String, CloseTracker>,
    exits: &mut HashMap<String, ExitGroup>,
    opens: &mut OpenCommissions,
    rollover: &mut RolloverBooked,
    events: &EventSender,
    gaps: &mut GapWindow,
) {
    loop {
        let json = match session.poll_event() {
            Ok(Some(j)) => {
                gaps.note(Instant::now());
                j
            }
            Ok(None) => return, // queue empty
            Err(_) => return,   // native error — nothing to drain (no shim, no session: never here)
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
            continue;
        };
        // The account's currency is the session's own login-time property, read once and only when there is a
        // rollover to book.
        let read_currency = || {
            session
                .system_properties_json()
                .ok()
                .flatten()
                .and_then(|body| stated_account_currency(&body))
        };
        for event in
            fold_with_rollover(v, routes, placed, closes, exits, opens, rollover, read_currency)
        {
            if events.blocking_send(event).is_err() {
                return; // core gone
            }
        }
        // A Close Limit the venue removed unfilled rests as the trade's take-profit, or ends cancelled in words.
        for (order_client_id, missed) in close::take_missed(closes) {
            let side = if missed.rest.buy { OrderSideWire::Buy } else { OrderSideWire::Sell };
            let instrument = missed.rest.instrument.clone();
            let mut venue = ExitVenueSession::new(session, &instrument, side, ExitKind::Limit);
            for event in rest_missed(&order_client_id, missed, routes, placed, exits, &mut venue) {
                if events.blocking_send(event).is_err() {
                    return; // core gone
                }
            }
        }
    }
}

/// [`fold_drained`] plus the rollover interest of a CLOSING fill (`crate::execution::funding`): it rides with the fill and is
/// emitted only when THIS envelope emitted that fill (a close of an order this process never placed books neither),
/// after it. `read_currency` asks the session for the account's currency and runs only when there is a rollover to book and
/// the currency is still unknown. The one place `drain_events` and the tests share.
fn fold_with_rollover(
    v: serde_json::Value,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    closes: &mut HashMap<String, CloseTracker>,
    exits: &mut HashMap<String, ExitGroup>,
    opens: &mut OpenCommissions,
    rollover: &mut RolloverBooked,
    read_currency: impl FnOnce() -> Option<String>,
) -> Vec<Event> {
    let mut out = fold_drained(v.clone(), routes, placed, closes, exits, opens);
    if out.iter().any(|e| matches!(e, Event::Fill(_))) {
        if rollover.wants_currency(&v) {
            rollover.currency = read_currency();
        }
        out.extend(rollover.events(&v));
    }
    out
}

/// [`fold_envelope`] after the commission is settled: an opening fill's figure is remembered and a closing fill's is
/// replaced by what the close still owes ([`OpenCommissions::settle`]). The one place `drain_events` and the tests share.
fn fold_drained(
    mut v: serde_json::Value,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    closes: &mut HashMap<String, CloseTracker>,
    exits: &mut HashMap<String, ExitGroup>,
    opens: &mut OpenCommissions,
) -> Vec<Event> {
    opens.settle(&mut v);
    fold_envelope(&v, routes, placed, closes, exits)
}

/// A closing fill that left a REMAINDER: the shim's `remain_trade_id` is the closed row's `TradeIDRemain`, the NEW id the
/// rest of a PARTIALLY closed trade comes back under (MEASURED 2026-10-26). The split is followed wherever it is heard,
/// whoever placed the close: [`crate::execution::trade_roots`] learns that the remainder is the same trade (so the reconcile fill
/// report reads it as the trade the core already folded), and an exit that covers the closed trade moves to the
/// remainder's id and shrinks to its size ([`ExitGroup::follow_split`]). The closed id is the envelope's trade id less the
/// `-close-<closing order id>` the shim appends; an envelope that does not read that way is none of this.
///
/// Returns the core's news of it: ONE `OrderModified` per exit that was cut, with the size the venue now backs
/// ([`ExitGroup::narrowed`]), in order client id order. The venue cuts the exit's amount without a word under the exit's own
/// order id (MEASURED 2026-11-09: a stop and a take-profit of 2000, a close of 1000, both left at 1000 and no event for
/// either), so this is the only report of it. A replayed envelope cuts nothing (the leg already names the remainder).
fn note_split(v: &serde_json::Value, exits: &mut HashMap<String, ExitGroup>) -> Vec<Event> {
    let text = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default();
    let remainder = text("remain_trade_id");
    if remainder.is_empty() {
        return Vec::new();
    }
    let suffix = format!("-close-{}", text("order_id"));
    let Some(closed) = text("trade_id").strip_suffix(&suffix) else {
        return Vec::new();
    };
    if closed.is_empty() || closed == remainder {
        return Vec::new();
    }
    let root = {
        let mut roots = crate::execution::trade_roots::process();
        roots.link(closed, remainder);
        roots.root_of(remainder)
    };
    tracing::info!(
        venue = "fxcm",
        "fxcm trade split: trade {closed} was closed in part and the rest is now trade {remainder} (root {root})"
    );
    let units = v.get("amount").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let ts = v.get("timestamp").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let mut cut: Vec<(&String, &mut ExitGroup)> = exits
        .iter_mut()
        .filter_map(|(order_client_id, group)| {
            group.follow_split(closed, remainder, units).then_some((order_client_id, group))
        })
        .collect();
    cut.sort_by(|a, b| a.0.cmp(b.0));
    cut.into_iter().map(|(order_client_id, group)| group.narrowed(order_client_id, ts)).collect()
}

/// Everything [`drain_events`] does with ONE decoded shim envelope short of sending: route it to its order client id and
/// decode it, read the fills of a CLOSE as one order across its legs (a partial fill until the last,
/// [`CloseTracker::relabel`]) and those of an EXIT that covers several trades the same way ([`ExitGroup::fold`]), follow a
/// split trade ([`note_split`], whose `OrderModified` for each exit it cut comes after the envelope's own events), and prune
/// the bookkeeping maps. Pure over its arguments, so a scripted envelope drives
/// the whole wiring in a test (`execution/execution_tests.rs`'s close tests); `drain_events` is this plus the session and the sender.
fn fold_envelope(
    v: &serde_json::Value,
    routes: &mut RouteTable,
    placed: &mut HashMap<String, PlacedOrder>,
    closes: &mut HashMap<String, CloseTracker>,
    exits: &mut HashMap<String, ExitGroup>,
) -> Vec<Event> {
    let trimmed = note_split(v, exits);
    let oid = v.get("order_id").and_then(|x| x.as_str()).unwrap_or_default();
    // Routing + decode, both pure; an envelope for an order this process never placed publishes
    // nothing and says so at `warn!` (the declared restart hole).
    let evs = map_drained_event(v, routes.routes());
    // The route of a venue order that was cancelled or rejected is dead whatever its order makes of the news.
    let raw_leg_gone = closes_routing(&evs);
    let mut leg_gone = None;
    // A fill of a CLOSE reads as one order across its legs: a partial fill until the last one. So do the legs of an exit,
    // and the venue's removal of one of them is not the end of the order while others rest.
    let evs = match order_client_id_of(&evs).map(str::to_owned) {
        Some(order_client_id) => {
            if let Some(group) = exits.get_mut(&order_client_id) {
                leg_gone = Some(raw_leg_gone);
                let (evs, ended) = group.fold(oid, evs);
                if ended {
                    exits.remove(&order_client_id);
                }
                evs
            } else if let Some(track) = closes.get_mut(&order_client_id) {
                let (evs, done) = track.relabel(oid, evs);
                // A leg the venue removed is dead even when its miss said nothing yet.
                leg_gone = Some(raw_leg_gone || closes_routing(&evs));
                if done {
                    closes.remove(&order_client_id);
                }
                evs
            } else {
                evs
            }
        }
        None => evs,
    };
    if ends_cancelability(&evs)
        && let Some(order_client_id) = order_client_id_of(&evs)
    {
        placed.remove(order_client_id);
    }
    if leg_gone.unwrap_or_else(|| closes_routing(&evs)) {
        routes.remove(oid);
    }
    // after the close's own events, and outside the routing above: they are about OTHER orders (the exits it cut)
    let mut evs = evs;
    evs.extend(trimmed);
    evs
}

/// The order client id a decoded envelope's events carry (they all carry the same one — the mapper
/// stamps the routed order client id onto every event it emits): the first [`Event::order_client_id`].
fn order_client_id_of(evs: &[Event]) -> Option<&str> {
    evs.iter().find_map(Event::order_client_id)
}

#[path = "execution_tests.rs"]
#[cfg(test)]
pub(crate) mod execution_tests;

#[path = "execution_property_tests.rs"]
#[cfg(test)]
mod execution_property_tests;

#[path = "execution_commission_tests.rs"]
#[cfg(test)]
mod execution_commission_tests;

#[path = "execution_rollover_tests.rs"]
#[cfg(test)]
mod execution_rollover_tests;
